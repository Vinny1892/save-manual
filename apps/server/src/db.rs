//! Persistência do server: índice de arquivos, devices e sessões.
//!
//! É um banco separado do `save-sync.db` do client — aqui mora o estado
//! autoritativo do protocolo (ver `docs/protocol.md`), não a configuração
//! de um device.

use std::collections::BTreeSet;

use rusqlite::{params, Connection, OptionalExtension};
use save_sync_core::protocol::{IndexEntry, Rev};

pub const SCHEMA_VERSION: i64 = 2;

pub fn open(path: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    // WAL: o server lê enquanto escreve (um device commitando, outro
    // planejando), e o modo padrão serializa demais pra isso.
    conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON").map_err(|e| e.to_string())?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;

    if version < 1 {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS devices (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL,
                platform     TEXT NOT NULL,
                token_hash   TEXT NOT NULL UNIQUE,
                created_at   INTEGER NOT NULL,
                last_seen    INTEGER
            );

            CREATE TABLE IF NOT EXISTS pairing_codes (
                code        TEXT PRIMARY KEY,
                expires_at  INTEGER NOT NULL,
                used_at     INTEGER
            );

            -- Índice autoritativo. Uma linha por path, viva ou tombstone.
            CREATE TABLE IF NOT EXISTS file_index (
                emulator_id TEXT NOT NULL,
                path        TEXT NOT NULL,
                rev         INTEGER NOT NULL,
                size        INTEGER NOT NULL,
                mtime       INTEGER NOT NULL,
                hash        TEXT NOT NULL,
                deleted     INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (emulator_id, path)
            );

            -- Consulta quente do plan: tudo que mudou desde o last_rev.
            CREATE INDEX IF NOT EXISTS idx_file_index_rev
                ON file_index (emulator_id, rev);

            CREATE TABLE IF NOT EXISTS emulator_state (
                emulator_id TEXT PRIMARY KEY,
                head_rev    INTEGER NOT NULL DEFAULT 0,
                -- Menor `last_rev` que ainda dá pra atender. Sobe quando
                -- tombstones são podados: sem o tombstone, o server não tem
                -- como dizer que aquele arquivo foi apagado, e um device
                -- parado desde antes disso precisa de resync.
                min_valid_rev INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS device_state (
                device_id   TEXT NOT NULL,
                emulator_id TEXT NOT NULL,
                last_rev    INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (device_id, emulator_id),
                FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                device_id   TEXT NOT NULL,
                emulator_id TEXT NOT NULL,
                plan        TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER NOT NULL,
                FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
            );

            -- Um blob já recebido no staging desta sessão. É o que torna o
            -- PUT idempotente e o resume barato.
            CREATE TABLE IF NOT EXISTS session_blobs (
                session_id TEXT NOT NULL,
                path       TEXT NOT NULL,
                hash       TEXT NOT NULL,
                size       INTEGER NOT NULL,
                mtime      INTEGER NOT NULL,
                PRIMARY KEY (session_id, path),
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            ",
        )
        .map_err(|e| e.to_string())?;
    }

    if version < 2 {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS users (
                id            TEXT PRIMARY KEY,
                username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                created_at    INTEGER NOT NULL
            );

            -- Sessão do browser. O token vai hasheado pelo mesmo motivo que
            -- o de device: quem ler o banco não ganha sessão de ninguém.
            CREATE TABLE IF NOT EXISTS web_sessions (
                token_hash TEXT PRIMARY KEY,
                user_id    TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
            );

            -- Trava de força bruta. O argon2 já limita a ~10 tentativas por
            -- segundo por si só, mas isso é pouco pra uma senha fraca se o
            -- server estiver exposto.
            CREATE TABLE IF NOT EXISTS login_attempts (
                username     TEXT PRIMARY KEY COLLATE NOCASE,
                failed_count INTEGER NOT NULL DEFAULT 0,
                locked_until INTEGER NOT NULL DEFAULT 0
            );
            ",
        )
        .map_err(|e| e.to_string())?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ─── estado do emulador ─────────────────────────────────────────────────

pub fn head_rev(conn: &Connection, emu: &str) -> Result<Rev, String> {
    let rev: Option<i64> = conn
        .query_row(
            "SELECT head_rev FROM emulator_state WHERE emulator_id = ?1",
            params![emu],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(rev.unwrap_or(0) as Rev)
}

pub fn bump_head_rev(conn: &Connection, emu: &str) -> Result<Rev, String> {
    let next = head_rev(conn, emu)? + 1;
    conn.execute(
        "INSERT INTO emulator_state (emulator_id, head_rev) VALUES (?1, ?2)
         ON CONFLICT(emulator_id) DO UPDATE SET head_rev = ?2",
        params![emu, next as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(next)
}

// ─── índice ─────────────────────────────────────────────────────────────

/// Entradas alteradas depois de `since` — é o "o que mudou do lado do
/// server" que entra no `compute_plan`.
pub fn changes_since(conn: &Connection, emu: &str, since: Rev) -> Result<Vec<IndexEntry>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT path, rev, size, mtime, hash, deleted
             FROM file_index WHERE emulator_id = ?1 AND rev > ?2
             ORDER BY path",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![emu, since as i64], |r| {
            Ok(IndexEntry {
                path: r.get(0)?,
                rev: r.get::<_, i64>(1)? as Rev,
                size: r.get::<_, i64>(2)? as u64,
                mtime: r.get(3)?,
                hash: r.get(4)?,
                deleted: r.get::<_, i64>(5)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Paths vivos hoje. Usado pra escolher um `.conflictN` que não colida.
pub fn live_paths(conn: &Connection, emu: &str) -> Result<BTreeSet<String>, String> {
    let mut stmt = conn
        .prepare("SELECT path FROM file_index WHERE emulator_id = ?1 AND deleted = 0")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![emu], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<BTreeSet<_>, _>>().map_err(|e| e.to_string())
}

pub fn get_entry(conn: &Connection, emu: &str, path: &str) -> Result<Option<IndexEntry>, String> {
    conn.query_row(
        "SELECT path, rev, size, mtime, hash, deleted
         FROM file_index WHERE emulator_id = ?1 AND path = ?2",
        params![emu, path],
        |r| {
            Ok(IndexEntry {
                path: r.get(0)?,
                rev: r.get::<_, i64>(1)? as Rev,
                size: r.get::<_, i64>(2)? as u64,
                mtime: r.get(3)?,
                hash: r.get(4)?,
                deleted: r.get::<_, i64>(5)? != 0,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

pub fn put_entry(conn: &Connection, emu: &str, entry: &IndexEntry) -> Result<(), String> {
    conn.execute(
        "INSERT INTO file_index (emulator_id, path, rev, size, mtime, hash, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(emulator_id, path) DO UPDATE SET
           rev = ?3, size = ?4, mtime = ?5, hash = ?6, deleted = ?7",
        params![
            emu,
            entry.path,
            entry.rev as i64,
            entry.size as i64,
            entry.mtime,
            entry.hash,
            entry.deleted as i64
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Marca como tombstone em vez de apagar a linha — é o tombstone que
/// propaga a deleção pra quem estava offline.
pub fn tombstone(conn: &Connection, emu: &str, path: &str, rev: Rev) -> Result<(), String> {
    put_entry(conn, emu, &IndexEntry::tombstone(path, rev))
}

/// Menor `last_rev` que o server ainda consegue atender sem resync.
pub fn min_valid_rev(conn: &Connection, emu: &str) -> Result<Rev, String> {
    let rev: Option<i64> = conn
        .query_row(
            "SELECT min_valid_rev FROM emulator_state WHERE emulator_id = ?1",
            params![emu],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(rev.unwrap_or(0) as Rev)
}

/// Remove tombstones cujo commit é anterior a `cutoff_rev` e move o
/// `min_valid_rev` junto.
///
/// Este é o passo que torna o `410 resync_required` necessário: depois de
/// apagar o tombstone, um device parado desde antes dele veria o arquivo
/// como "existe aqui e o server não tem" e o ressuscitaria. Subir o
/// `min_valid_rev` força esse device a refazer o baseline em vez disso.
#[allow(dead_code)] // chamador entra com a retenção server-side (#5)
pub fn prune_tombstones(conn: &Connection, emu: &str, cutoff_rev: Rev) -> Result<usize, String> {
    let removed = conn
        .execute(
            "DELETE FROM file_index
             WHERE emulator_id = ?1 AND deleted = 1 AND rev < ?2",
            params![emu, cutoff_rev as i64],
        )
        .map_err(|e| e.to_string())?;

    if removed > 0 {
        conn.execute(
            "INSERT INTO emulator_state (emulator_id, head_rev, min_valid_rev)
             VALUES (?1, 0, ?2)
             ON CONFLICT(emulator_id) DO UPDATE SET
               min_valid_rev = MAX(min_valid_rev, ?2)",
            params![emu, cutoff_rev as i64],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

// ─── estado por device ──────────────────────────────────────────────────

pub fn device_last_rev(conn: &Connection, device: &str, emu: &str) -> Result<Rev, String> {
    let rev: Option<i64> = conn
        .query_row(
            "SELECT last_rev FROM device_state WHERE device_id = ?1 AND emulator_id = ?2",
            params![device, emu],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(rev.unwrap_or(0) as Rev)
}

pub fn set_device_last_rev(
    conn: &Connection,
    device: &str,
    emu: &str,
    rev: Rev,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO device_state (device_id, emulator_id, last_rev) VALUES (?1, ?2, ?3)
         ON CONFLICT(device_id, emulator_id) DO UPDATE SET last_rev = ?3",
        params![device, emu, rev as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn migration_sets_user_version() {
        let conn = mem();
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = mem();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(head_rev(&conn, "eden").unwrap(), 0);
    }

    #[test]
    fn head_rev_starts_at_zero_and_increments() {
        let conn = mem();
        assert_eq!(head_rev(&conn, "eden").unwrap(), 0);
        assert_eq!(bump_head_rev(&conn, "eden").unwrap(), 1);
        assert_eq!(bump_head_rev(&conn, "eden").unwrap(), 2);
        assert_eq!(head_rev(&conn, "eden").unwrap(), 2);
    }

    #[test]
    fn head_rev_is_per_emulator() {
        let conn = mem();
        bump_head_rev(&conn, "eden").unwrap();
        bump_head_rev(&conn, "eden").unwrap();
        assert_eq!(head_rev(&conn, "eden").unwrap(), 2);
        assert_eq!(head_rev(&conn, "pcsx2").unwrap(), 0);
    }

    #[test]
    fn changes_since_filters_by_rev() {
        let conn = mem();
        put_entry(&conn, "eden", &IndexEntry::live("a", 1, 10, 100, "h1")).unwrap();
        put_entry(&conn, "eden", &IndexEntry::live("b", 5, 10, 100, "h2")).unwrap();

        let since_0 = changes_since(&conn, "eden", 0).unwrap();
        assert_eq!(since_0.len(), 2);

        let since_1 = changes_since(&conn, "eden", 1).unwrap();
        assert_eq!(since_1.len(), 1);
        assert_eq!(since_1[0].path, "b");

        assert!(changes_since(&conn, "eden", 5).unwrap().is_empty());
    }

    #[test]
    fn changes_since_includes_tombstones() {
        let conn = mem();
        tombstone(&conn, "eden", "apagado", 3).unwrap();
        let changes = changes_since(&conn, "eden", 0).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].deleted);
    }

    #[test]
    fn put_entry_overwrites_same_path() {
        let conn = mem();
        put_entry(&conn, "eden", &IndexEntry::live("a", 1, 10, 100, "h1")).unwrap();
        put_entry(&conn, "eden", &IndexEntry::live("a", 2, 20, 200, "h2")).unwrap();
        let e = get_entry(&conn, "eden", "a").unwrap().unwrap();
        assert_eq!(e.rev, 2);
        assert_eq!(e.hash, "h2");
        assert_eq!(changes_since(&conn, "eden", 0).unwrap().len(), 1);
    }

    #[test]
    fn tombstone_keeps_the_row_so_deletion_propagates() {
        let conn = mem();
        put_entry(&conn, "eden", &IndexEntry::live("a", 1, 10, 100, "h1")).unwrap();
        tombstone(&conn, "eden", "a", 2).unwrap();

        let e = get_entry(&conn, "eden", "a").unwrap().unwrap();
        assert!(e.deleted, "a linha tem que sobreviver como tombstone");
        assert_eq!(e.rev, 2);
    }

    #[test]
    fn live_paths_excludes_tombstones() {
        let conn = mem();
        put_entry(&conn, "eden", &IndexEntry::live("vivo", 1, 10, 100, "h1")).unwrap();
        tombstone(&conn, "eden", "morto", 2).unwrap();
        let live = live_paths(&conn, "eden").unwrap();
        assert!(live.contains("vivo"));
        assert!(!live.contains("morto"));
    }

    #[test]
    fn prune_tombstones_only_removes_old_dead_rows() {
        let conn = mem();
        put_entry(&conn, "eden", &IndexEntry::live("vivo", 1, 10, 100, "h1")).unwrap();
        tombstone(&conn, "eden", "antigo", 2).unwrap();
        tombstone(&conn, "eden", "recente", 9).unwrap();

        let removed = prune_tombstones(&conn, "eden", 5).unwrap();
        assert_eq!(removed, 1);
        assert!(get_entry(&conn, "eden", "antigo").unwrap().is_none());
        assert!(get_entry(&conn, "eden", "recente").unwrap().is_some());
        assert!(
            get_entry(&conn, "eden", "vivo").unwrap().is_some(),
            "prune não pode tocar em arquivo vivo"
        );
    }

    #[test]
    fn prune_raises_min_valid_rev_so_old_devices_get_resync() {
        let conn = mem();
        assert_eq!(min_valid_rev(&conn, "eden").unwrap(), 0);
        tombstone(&conn, "eden", "antigo", 2).unwrap();
        prune_tombstones(&conn, "eden", 5).unwrap();
        assert_eq!(min_valid_rev(&conn, "eden").unwrap(), 5);
    }

    #[test]
    fn prune_without_matches_leaves_min_valid_rev_alone() {
        // Sem tombstone removido, nenhum device perdeu informação — subir o
        // corte forçaria resync à toa.
        let conn = mem();
        prune_tombstones(&conn, "eden", 99).unwrap();
        assert_eq!(min_valid_rev(&conn, "eden").unwrap(), 0);
    }

    #[test]
    fn prune_never_lowers_min_valid_rev() {
        let conn = mem();
        tombstone(&conn, "eden", "a", 1).unwrap();
        prune_tombstones(&conn, "eden", 10).unwrap();
        tombstone(&conn, "eden", "b", 2).unwrap();
        prune_tombstones(&conn, "eden", 3).unwrap();
        assert_eq!(min_valid_rev(&conn, "eden").unwrap(), 10);
    }

    #[test]
    fn prune_does_not_disturb_head_rev() {
        let conn = mem();
        bump_head_rev(&conn, "eden").unwrap();
        bump_head_rev(&conn, "eden").unwrap();
        tombstone(&conn, "eden", "a", 1).unwrap();
        prune_tombstones(&conn, "eden", 2).unwrap();
        assert_eq!(head_rev(&conn, "eden").unwrap(), 2);
    }

    #[test]
    fn device_last_rev_defaults_to_zero_meaning_resync() {
        let conn = mem();
        assert_eq!(device_last_rev(&conn, "dev1", "eden").unwrap(), 0);
    }

    #[test]
    fn device_last_rev_roundtrips_per_emulator() {
        let conn = mem();
        conn.execute(
            "INSERT INTO devices (id, name, platform, token_hash, created_at)
             VALUES ('dev1', 'pc', 'windows', 'hash', 0)",
            [],
        )
        .unwrap();
        set_device_last_rev(&conn, "dev1", "eden", 7).unwrap();
        assert_eq!(device_last_rev(&conn, "dev1", "eden").unwrap(), 7);
        assert_eq!(device_last_rev(&conn, "dev1", "pcsx2").unwrap(), 0);
    }

}
