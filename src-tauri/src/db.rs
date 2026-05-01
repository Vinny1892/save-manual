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
    seed(&conn)?;
    Ok(conn)
}

fn seed(conn: &Connection) -> Result<(), String> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM emulators", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if count > 0 {
        return Ok(());
    }

    let defaults = [
        (
            "eden",
            "Eden (Switch)",
            "Selecione a pasta nand/ inteira (ex.: %APPDATA%\\Eden\\nand). \
             A NAND inteira precisa ser sincronizada — o UUID do perfil muda entre instalações.",
        ),
        (
            "pcsx2",
            "PCSX2 (PS2)",
            "Pasta dos memory cards (arquivos .ps2). \
             Padrão: %USERPROFILE%\\Documents\\PCSX2\\memcards. \
             Confirme em Settings > Memory Cards.",
        ),
        (
            "rpcs3",
            "RPCS3 (PS3)",
            "Selecione a pasta dev_hdd0 dentro da instalação do RPCS3. \
             Confirme em Settings > Advanced > Virtual File System.",
        ),
    ];

    for (id, name, hint) in defaults {
        conn.execute(
            "INSERT INTO emulators (id, name, hint) VALUES (?1, ?2, ?3)",
            params![id, name, hint],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn list_all(conn: &Connection) -> Result<Vec<Emulator>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, hint, source_path, dest_path, enabled, last_sync, last_error
             FROM emulators ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Emulator {
                id: r.get(0)?,
                name: r.get(1)?,
                hint: r.get(2)?,
                source_path: r.get(3)?,
                dest_path: r.get(4)?,
                enabled: r.get::<_, i64>(5)? != 0,
                last_sync: r.get(6)?,
                last_error: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

pub fn get(conn: &Connection, id: &str) -> Result<Emulator, String> {
    conn.query_row(
        "SELECT id, name, hint, source_path, dest_path, enabled, last_sync, last_error
         FROM emulators WHERE id = ?1",
        params![id],
        |r| {
            Ok(Emulator {
                id: r.get(0)?,
                name: r.get(1)?,
                hint: r.get(2)?,
                source_path: r.get(3)?,
                dest_path: r.get(4)?,
                enabled: r.get::<_, i64>(5)? != 0,
                last_sync: r.get(6)?,
                last_error: r.get(7)?,
            })
        },
    )
    .map_err(|e| format!("get({id}): {e}"))
}

pub fn set_paths(
    conn: &Connection,
    id: &str,
    source: &str,
    dest: &str,
) -> Result<(), String> {
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

pub fn set_last_sync(conn: &Connection, id: &str, ts: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE emulators SET last_sync = ?1, last_error = NULL WHERE id = ?2",
        params![ts, id],
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
