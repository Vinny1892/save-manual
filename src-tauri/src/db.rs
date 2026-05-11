use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Emulator {
    pub id: String,
    pub name: String,
    pub hint: String,
    pub source_path: String,
    /// Where the backup goes. `local` = filesystem path; `rclone` = an rclone
    /// remote configured via `config/create`. When `rclone`, `dest_remote`
    /// names the remote and `dest_path` is the path inside it (e.g. bucket/prefix).
    pub dest_kind: String,
    pub dest_remote: String,
    pub dest_path: String,
    pub enabled: bool,
    pub last_sync: Option<String>,
    pub last_error: Option<String>,
    pub process_name: String,
}

pub fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS emulators (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            hint TEXT NOT NULL,
            source_path TEXT NOT NULL DEFAULT '',
            dest_path TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            last_sync TEXT,
            last_error TEXT
        );",
    )
    .map_err(|e| e.to_string())?;
    migrate(&conn)?;
    seed(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;

    if version < 1 {
        conn.execute_batch(
            "ALTER TABLE emulators ADD COLUMN process_name TEXT NOT NULL DEFAULT '';
             PRAGMA user_version = 1;",
        )
        .map_err(|e| e.to_string())?;
        seed_process_names(conn)?;
    }

    if version < 2 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL DEFAULT ''
             );
             PRAGMA user_version = 2;",
        )
        .map_err(|e| e.to_string())?;
    }

    if version < 3 {
        // dest_kind: 'local' (default) or 'rclone'.
        // dest_remote: rclone remote name when dest_kind = 'rclone'.
        // Existing rows keep dest_kind='local' so behavior is unchanged.
        conn.execute_batch(
            "ALTER TABLE emulators ADD COLUMN dest_kind TEXT NOT NULL DEFAULT 'local';
             ALTER TABLE emulators ADD COLUMN dest_remote TEXT NOT NULL DEFAULT '';
             PRAGMA user_version = 3;",
        )
        .map_err(|e| e.to_string())?;
    }

    if version < 4 {
        // Per-emulator history settings. Rows are created lazily by
        // `get_history_settings` (which falls back to defaults), so we don't
        // pre-populate here — keeps fresh-install and upgrade paths uniform.
        // bisync_initialized tracks whether rclone has run --resync for this
        // pair yet; reset to 0 by `set_paths` whenever source/dest changes.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history_settings (
                emulator_id        TEXT PRIMARY KEY,
                enabled            INTEGER NOT NULL DEFAULT 1,
                mode               TEXT NOT NULL DEFAULT 'incremental',
                retention_days     INTEGER NOT NULL DEFAULT 30,
                retention_max_mb   INTEGER NOT NULL DEFAULT 500,
                bisync_initialized INTEGER NOT NULL DEFAULT 0
             );
             PRAGMA user_version = 4;",
        )
        .map_err(|e| e.to_string())?;
    }

    if version < 5 {
        // mode (single string) → two independent booleans. Either or both
        // can be on; "at least one when enabled" is enforced in set_history.
        // The dormant `mode` column stays for v4 rollbacks; a future v6 may
        // DROP it.
        conn.execute_batch(
            "ALTER TABLE history_settings ADD COLUMN incremental_enabled INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE history_settings ADD COLUMN full_enabled        INTEGER NOT NULL DEFAULT 0;
             UPDATE history_settings SET
                incremental_enabled = CASE WHEN mode = 'incremental' THEN 1 ELSE 0 END,
                full_enabled        = CASE WHEN mode = 'full'        THEN 1 ELSE 0 END;
             PRAGMA user_version = 5;",
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Whether an emulator's save data is structured as independent directories
/// per save (true → incremental backup makes sense) or as a single binary
/// file like a memory card (false → only full snapshots make sense).
pub fn supports_incremental_history(emu_id: &str) -> bool {
    !matches!(emu_id, "pcsx2")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySettings {
    pub emulator_id: String,
    pub enabled: bool,
    /// Route bisync overwrites/deletes through --backupdir2. Storage-cheap,
    /// captures only deltas. Forced to false for file-based emulators.
    pub incremental_enabled: bool,
    /// Pre-sync full snapshot (live → .history/<ts>/full/). Storage-heavy
    /// but easier to reason about — every snapshot is a complete state.
    pub full_enabled: bool,
    pub retention_days: i64,
    pub retention_max_mb: i64,
    pub bisync_initialized: bool,
}

impl HistorySettings {
    pub fn defaults_for(emu_id: &str) -> Self {
        let (incremental, full) = if supports_incremental_history(emu_id) {
            (true, false)
        } else {
            // file-based emus can only do full
            (false, true)
        };
        HistorySettings {
            emulator_id: emu_id.to_string(),
            enabled: true,
            incremental_enabled: incremental,
            full_enabled: full,
            retention_days: 30,
            retention_max_mb: 500,
            bisync_initialized: false,
        }
    }
}

fn seed_process_names(conn: &Connection) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let (eden, pcsx2, rpcs3) = ("eden.exe", "pcsx2-qt.exe", "rpcs3.exe");
    #[cfg(not(target_os = "windows"))]
    let (eden, pcsx2, rpcs3) = ("eden", "pcsx2-qt", "rpcs3");

    for (id, proc) in [("eden", eden), ("pcsx2", pcsx2), ("rpcs3", rpcs3)] {
        conn.execute(
            "UPDATE emulators SET process_name = ?1 WHERE id = ?2 AND process_name = ''",
            params![proc, id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn seed(conn: &Connection) -> Result<(), String> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM emulators", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if count > 0 {
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    let (eden_proc, pcsx2_proc, rpcs3_proc) = ("eden.exe", "pcsx2-qt.exe", "rpcs3.exe");
    #[cfg(not(target_os = "windows"))]
    let (eden_proc, pcsx2_proc, rpcs3_proc) = ("eden", "pcsx2-qt", "rpcs3");

    let defaults = [
        (
            "eden",
            "Eden (Switch)",
            "Aponte para a pasta nand/ da instalação de origem (ex.: %APPDATA%\\Eden\\nand). \
             O app detecta o UUID do perfil e sincroniza apenas o necessário — \
             sem precisar configurar UUIDs manualmente nem copiar a NAND inteira.",
            eden_proc,
        ),
        (
            "pcsx2",
            "PCSX2 (PS2)",
            "Pasta dos memory cards (arquivos .ps2). \
             Padrão: %USERPROFILE%\\Documents\\PCSX2\\memcards. \
             Confirme em Settings > Memory Cards.",
            pcsx2_proc,
        ),
        (
            "rpcs3",
            "RPCS3 (PS3)",
            "Selecione a pasta dev_hdd0 dentro da instalação do RPCS3. \
             Confirme em Settings > Advanced > Virtual File System.",
            rpcs3_proc,
        ),
    ];

    for (id, name, hint, proc) in defaults {
        conn.execute(
            "INSERT INTO emulators (id, name, hint, process_name) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, hint, proc],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

const SELECT_COLS: &str =
    "id, name, hint, source_path, dest_path, enabled, last_sync, last_error, process_name, dest_kind, dest_remote";

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Emulator> {
    Ok(Emulator {
        id: r.get(0)?,
        name: r.get(1)?,
        hint: r.get(2)?,
        source_path: r.get(3)?,
        dest_path: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        last_sync: r.get(6)?,
        last_error: r.get(7)?,
        process_name: r.get(8)?,
        dest_kind: r.get(9)?,
        dest_remote: r.get(10)?,
    })
}

pub fn list_all(conn: &Connection) -> Result<Vec<Emulator>, String> {
    let sql = format!("SELECT {SELECT_COLS} FROM emulators ORDER BY id");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], map_row)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

pub fn get(conn: &Connection, id: &str) -> Result<Emulator, String> {
    let sql = format!("SELECT {SELECT_COLS} FROM emulators WHERE id = ?1");
    conn.query_row(&sql, params![id], map_row)
        .map_err(|e| format!("get({id}): {e}"))
}

pub fn set_paths(
    conn: &Connection,
    id: &str,
    source: &str,
    dest_kind: &str,
    dest_remote: &str,
    dest_path: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE emulators
         SET source_path = ?1, dest_kind = ?2, dest_remote = ?3, dest_path = ?4
         WHERE id = ?5",
        params![source, dest_kind, dest_remote, dest_path, id],
    )
    .map_err(|e| e.to_string())?;
    // Mudar source/dest invalida o pareamento bisync — força --resync no
    // próximo sync. Idempotente: se a row ainda não existe não faz nada.
    conn.execute(
        "UPDATE history_settings SET bisync_initialized = 0 WHERE emulator_id = ?1",
        params![id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_history_settings(conn: &Connection, emu_id: &str) -> Result<HistorySettings, String> {
    let row = conn.query_row(
        "SELECT enabled, incremental_enabled, full_enabled,
                retention_days, retention_max_mb, bisync_initialized
         FROM history_settings WHERE emulator_id = ?1",
        params![emu_id],
        |r| {
            Ok(HistorySettings {
                emulator_id: emu_id.to_string(),
                enabled: r.get::<_, i64>(0)? != 0,
                incremental_enabled: r.get::<_, i64>(1)? != 0,
                full_enabled: r.get::<_, i64>(2)? != 0,
                retention_days: r.get(3)?,
                retention_max_mb: r.get(4)?,
                bisync_initialized: r.get::<_, i64>(5)? != 0,
            })
        },
    );
    match row {
        Ok(s) => Ok(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(HistorySettings::defaults_for(emu_id)),
        Err(e) => Err(e.to_string()),
    }
}

pub fn set_history_settings(conn: &Connection, s: &HistorySettings) -> Result<(), String> {
    // File-based emulators (pcsx2 today, duckstation tomorrow) can only do
    // full snapshots. Coerce silently — UI hides the incremental checkbox
    // for these, so a client sending true is either stale or buggy; either
    // way the backend is the source of truth.
    let incremental = supports_incremental_history(&s.emulator_id) && s.incremental_enabled;
    let full = s.full_enabled;

    // "At least one when enabled" — backstop for the UI which should never
    // let the user save both off. If history is fully disabled, modes are
    // free to be whatever (they're inert anyway).
    if s.enabled && !incremental && !full {
        return Err(
            "selecione pelo menos um modo de backup (incremental ou full) quando history está ativo"
                .into(),
        );
    }

    conn.execute(
        "INSERT INTO history_settings
            (emulator_id, enabled, incremental_enabled, full_enabled,
             retention_days, retention_max_mb, bisync_initialized)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(emulator_id) DO UPDATE SET
            enabled             = excluded.enabled,
            incremental_enabled = excluded.incremental_enabled,
            full_enabled        = excluded.full_enabled,
            retention_days      = excluded.retention_days,
            retention_max_mb    = excluded.retention_max_mb",
        params![
            s.emulator_id,
            s.enabled as i64,
            incremental as i64,
            full as i64,
            s.retention_days,
            s.retention_max_mb,
            s.bisync_initialized as i64,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn mark_bisync_initialized(conn: &Connection, emu_id: &str) -> Result<(), String> {
    // Use INSERT OR IGNORE + UPDATE to handle the case where the row doesn't
    // exist yet (first sync after install — settings still come from defaults).
    conn.execute(
        "INSERT OR IGNORE INTO history_settings (emulator_id) VALUES (?1)",
        params![emu_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE history_settings SET bisync_initialized = 1 WHERE emulator_id = ?1",
        params![emu_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Inverse of mark_bisync_initialized — forces a `--resync` on the next sync.
/// Called after revert because the rclone bisync state files (in workdir)
/// reflect pre-revert listings; running a normal bisync would see both
/// sides "regressed" and may flag conflicts. `--resync` rebuilds baseline.
pub fn mark_bisync_needs_resync(conn: &Connection, emu_id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE history_settings SET bisync_initialized = 0 WHERE emulator_id = ?1",
        params![emu_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_enabled(conn: &Connection, id: &str, enabled: bool) -> Result<(), String> {
    conn.execute(
        "UPDATE emulators SET enabled = ?1 WHERE id = ?2",
        params![enabled as i64, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_process_name(conn: &Connection, id: &str, name: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE emulators SET process_name = ?1 WHERE id = ?2",
        params![name, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_last_sync(conn: &Connection, id: &str, ts: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE emulators SET last_sync = ?1, last_error = NULL WHERE id = ?2",
        params![ts, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    let mut stmt = conn
        .prepare("SELECT value FROM settings WHERE key = ?1")
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query(params![key]).map_err(|e| e.to_string())?;
    Ok(rows.next().map_err(|e| e.to_string())?.map(|r| r.get(0).unwrap_or_default()))
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_last_error(conn: &Connection, id: &str, err: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE emulators SET last_error = ?1 WHERE id = ?2",
        params![err, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an in-memory DB at the same shape `open()` produces, so
    /// migrations and helpers can be exercised without touching disk.
    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS emulators (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                hint TEXT NOT NULL,
                source_path TEXT NOT NULL DEFAULT '',
                dest_path TEXT NOT NULL DEFAULT '',
                enabled INTEGER NOT NULL DEFAULT 1,
                last_sync TEXT,
                last_error TEXT
            );",
        )
        .unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn insert_emu(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO emulators (id, name, hint) VALUES (?1, ?2, '')",
            params![id, id],
        )
        .unwrap();
    }

    // ─── classification ──────────────────────────────────────────────────

    #[test]
    fn supports_incremental_matches_classification() {
        assert!(supports_incremental_history("eden"));
        assert!(supports_incremental_history("rpcs3"));
        assert!(!supports_incremental_history("pcsx2"));
    }

    #[test]
    fn defaults_for_pcsx2_is_full_only() {
        let s = HistorySettings::defaults_for("pcsx2");
        assert!(!s.incremental_enabled);
        assert!(s.full_enabled);
        assert!(s.enabled);
        assert!(!s.bisync_initialized);
    }

    #[test]
    fn defaults_for_eden_is_incremental_only() {
        let s = HistorySettings::defaults_for("eden");
        assert!(s.incremental_enabled);
        assert!(!s.full_enabled);
    }

    // ─── migrations ──────────────────────────────────────────────────────

    #[test]
    fn migration_v4_creates_history_settings_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='history_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn migration_advances_user_version_to_5() {
        let conn = fresh_db();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 5);
    }

    #[test]
    fn migration_v5_translates_legacy_mode_to_booleans() {
        // Simulate the v4 schema with a populated row, then run migrate
        // to reach v5 — the booleans should mirror the old mode value.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE emulators (
                id TEXT PRIMARY KEY, name TEXT, hint TEXT,
                source_path TEXT DEFAULT '', dest_path TEXT DEFAULT '',
                enabled INTEGER DEFAULT 1, last_sync TEXT, last_error TEXT
            );
            CREATE TABLE history_settings (
                emulator_id TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 1,
                mode TEXT NOT NULL DEFAULT 'incremental',
                retention_days INTEGER NOT NULL DEFAULT 30,
                retention_max_mb INTEGER NOT NULL DEFAULT 500,
                bisync_initialized INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO history_settings (emulator_id, mode) VALUES ('eden', 'incremental');
            INSERT INTO history_settings (emulator_id, mode) VALUES ('pcsx2', 'full');
            PRAGMA user_version = 4;",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let eden = get_history_settings(&conn, "eden").unwrap();
        assert!(eden.incremental_enabled);
        assert!(!eden.full_enabled);

        let pcsx2 = get_history_settings(&conn, "pcsx2").unwrap();
        assert!(!pcsx2.incremental_enabled);
        assert!(pcsx2.full_enabled);
    }

    // ─── get/set history_settings ────────────────────────────────────────

    #[test]
    fn get_history_returns_defaults_when_row_missing() {
        let conn = fresh_db();
        let s = get_history_settings(&conn, "eden").unwrap();
        assert!(s.enabled);
        assert!(s.incremental_enabled);
        assert!(!s.full_enabled);
        assert_eq!(s.retention_days, 30);
        assert_eq!(s.retention_max_mb, 500);
        assert!(!s.bisync_initialized);
    }

    #[test]
    fn set_history_round_trips_for_eden() {
        let conn = fresh_db();
        let s = HistorySettings {
            emulator_id: "eden".into(),
            enabled: true,
            incremental_enabled: true,
            full_enabled: true, // both on
            retention_days: 7,
            retention_max_mb: 100,
            bisync_initialized: false,
        };
        set_history_settings(&conn, &s).unwrap();
        let loaded = get_history_settings(&conn, "eden").unwrap();
        assert!(loaded.enabled);
        assert!(loaded.incremental_enabled);
        assert!(loaded.full_enabled);
        assert_eq!(loaded.retention_days, 7);
        assert_eq!(loaded.retention_max_mb, 100);
    }

    #[test]
    fn set_history_pcsx2_coerces_incremental_to_false() {
        // UI shouldn't send this, but if it does (stale client, manual API
        // call), backend forces incremental off and keeps full as user set.
        let conn = fresh_db();
        let s = HistorySettings {
            emulator_id: "pcsx2".into(),
            enabled: true,
            incremental_enabled: true,
            full_enabled: true,
            retention_days: 30,
            retention_max_mb: 500,
            bisync_initialized: false,
        };
        set_history_settings(&conn, &s).unwrap();
        let loaded = get_history_settings(&conn, "pcsx2").unwrap();
        assert!(!loaded.incremental_enabled);
        assert!(loaded.full_enabled);
    }

    #[test]
    fn set_history_rejects_both_off_when_enabled() {
        let conn = fresh_db();
        let s = HistorySettings {
            emulator_id: "eden".into(),
            enabled: true,
            incremental_enabled: false,
            full_enabled: false,
            retention_days: 30,
            retention_max_mb: 500,
            bisync_initialized: false,
        };
        assert!(set_history_settings(&conn, &s).is_err());
    }

    #[test]
    fn set_history_allows_both_off_when_disabled() {
        // history fully off → modes don't matter, no validation.
        let conn = fresh_db();
        let s = HistorySettings {
            emulator_id: "eden".into(),
            enabled: false,
            incremental_enabled: false,
            full_enabled: false,
            retention_days: 30,
            retention_max_mb: 500,
            bisync_initialized: false,
        };
        assert!(set_history_settings(&conn, &s).is_ok());
    }

    #[test]
    fn set_history_pcsx2_with_only_incremental_errors_after_coercion() {
        // pcsx2 + enabled=true + only_incremental=true → coercion turns
        // incremental off → both flags off → error.
        let conn = fresh_db();
        let s = HistorySettings {
            emulator_id: "pcsx2".into(),
            enabled: true,
            incremental_enabled: true,
            full_enabled: false,
            retention_days: 30,
            retention_max_mb: 500,
            bisync_initialized: false,
        };
        assert!(set_history_settings(&conn, &s).is_err());
    }

    // ─── bisync_initialized lifecycle ────────────────────────────────────

    #[test]
    fn mark_bisync_initialized_works_with_no_prior_row() {
        let conn = fresh_db();
        mark_bisync_initialized(&conn, "eden").unwrap();
        assert!(get_history_settings(&conn, "eden").unwrap().bisync_initialized);
    }

    #[test]
    fn mark_bisync_initialized_is_idempotent() {
        let conn = fresh_db();
        mark_bisync_initialized(&conn, "eden").unwrap();
        mark_bisync_initialized(&conn, "eden").unwrap();
        assert!(get_history_settings(&conn, "eden").unwrap().bisync_initialized);
    }

    #[test]
    fn set_paths_resets_bisync_initialized() {
        let conn = fresh_db();
        insert_emu(&conn, "eden");
        mark_bisync_initialized(&conn, "eden").unwrap();
        assert!(get_history_settings(&conn, "eden").unwrap().bisync_initialized);

        set_paths(&conn, "eden", "/new/src", "local", "", "/new/dest").unwrap();

        assert!(!get_history_settings(&conn, "eden").unwrap().bisync_initialized);
    }

    #[test]
    fn set_history_does_not_reset_bisync_initialized() {
        // Only set_paths should reset the flag — set_history must not
        // accidentally clobber it (otherwise users editing retention would
        // unwittingly trigger a resync next sync).
        let conn = fresh_db();
        mark_bisync_initialized(&conn, "eden").unwrap();
        let mut s = HistorySettings::defaults_for("eden");
        s.retention_days = 7;
        set_history_settings(&conn, &s).unwrap();
        assert!(get_history_settings(&conn, "eden").unwrap().bisync_initialized);
    }

    #[test]
    fn defaults_are_valid_per_set_history_validation() {
        // defaults_for must always produce a state that round-trips through
        // set_history_settings — protects against future drift between
        // defaults and validation.
        let conn = fresh_db();
        for id in ["eden", "rpcs3", "pcsx2"] {
            let s = HistorySettings::defaults_for(id);
            set_history_settings(&conn, &s)
                .unwrap_or_else(|e| panic!("defaults_for({id}) rejected by set_history: {e}"));
        }
    }
}
