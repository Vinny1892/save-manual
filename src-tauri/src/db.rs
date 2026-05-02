use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Emulator {
    pub id: String,
    pub name: String,
    pub hint: String,
    pub source_path: String,
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

    Ok(())
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
    })
}

pub fn list_all(conn: &Connection) -> Result<Vec<Emulator>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, hint, source_path, dest_path, enabled, last_sync, last_error, process_name
             FROM emulators ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], map_row)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

pub fn get(conn: &Connection, id: &str) -> Result<Emulator, String> {
    conn.query_row(
        "SELECT id, name, hint, source_path, dest_path, enabled, last_sync, last_error, process_name
         FROM emulators WHERE id = ?1",
        params![id],
        map_row,
    )
    .map_err(|e| format!("get({id}): {e}"))
}

pub fn set_paths(conn: &Connection, id: &str, source: &str, dest: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE emulators SET source_path = ?1, dest_path = ?2 WHERE id = ?3",
        params![source, dest, id],
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
