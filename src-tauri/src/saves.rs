use std::path::Path;

use chrono::{DateTime, Local};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct SaveEntry {
    pub raw_id: String,
    pub title: String,
    pub modified: Option<String>,
    pub size_bytes: u64,
}

pub fn list_saves(emulator_id: &str, source_path: &str) -> Vec<SaveEntry> {
    let path = Path::new(source_path);
    if !path.exists() {
        return vec![];
    }
    match emulator_id {
        "eden" => list_eden(path),
        "rpcs3" => list_rpcs3(path),
        "pcsx2" => list_pcsx2(path),
        _ => vec![],
    }
}

// ─── Eden (Switch) ──────────────────────────────────────────────────────────
// nand/user/save/0000000000000000/<user-uuid 32-hex>/<title-id 16-hex>/

fn eden_user_saves_base(nand_root: &Path) -> std::path::PathBuf {
    nand_root.join("user").join("save").join("0000000000000000")
}

fn list_eden(nand_root: &Path) -> Vec<SaveEntry> {
    let base = eden_user_saves_base(nand_root);
    let Ok(uuid_dirs) = std::fs::read_dir(&base) else {
        return vec![];
    };

    // dedup by title-id; keep the entry with the latest mtime if multiple
    // user profiles have a save for the same game
    let mut by_id: std::collections::HashMap<String, SaveEntry> = std::collections::HashMap::new();

    for uuid_entry in uuid_dirs.flatten() {
        let uuid_path = uuid_entry.path();
        if !uuid_path.is_dir() {
            continue;
        }
        let uuid_name = uuid_entry.file_name().to_string_lossy().into_owned();
        if uuid_name.len() != 32 || !uuid_name.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }

        let Ok(title_dirs) = std::fs::read_dir(&uuid_path) else { continue };
        for title_entry in title_dirs.flatten() {
            let title_path = title_entry.path();
            if !title_path.is_dir() {
                continue;
            }
            let raw_id = title_entry.file_name().to_string_lossy().into_owned();
            if raw_id.len() != 16 || !raw_id.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let entry = SaveEntry {
                title: raw_id.clone(),
                raw_id: raw_id.clone(),
                modified: dir_modified(&title_path),
                size_bytes: dir_size(&title_path),
            };
            by_id
                .entry(raw_id)
                .and_modify(|cur| {
                    if entry.modified > cur.modified {
                        *cur = entry.clone();
                    }
                })
                .or_insert(entry);
        }
    }

    let mut entries: Vec<SaveEntry> = by_id.into_values().collect();
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    entries
}

// ─── RPCS3 (PS3) ────────────────────────────────────────────────────────────
// dev_hdd0/home/<user-id>/savedata/<save-id>/PARAM.SFO

fn list_rpcs3(dev_hdd0: &Path) -> Vec<SaveEntry> {
    let home_dir = dev_hdd0.join("home");
    let Ok(users) = std::fs::read_dir(&home_dir) else {
        return vec![];
    };
    let mut entries = Vec::new();
    for user in users.flatten() {
        let savedata = user.path().join("savedata");
        let Ok(saves) = std::fs::read_dir(&savedata) else {
            continue;
        };
        for save in saves.flatten() {
            if !save.path().is_dir() {
                continue;
            }
            let raw_id = save.file_name().to_string_lossy().into_owned();
            let title = read_sfo_title(&save.path().join("PARAM.SFO"))
                .unwrap_or_else(|| raw_id.clone());
            entries.push(SaveEntry {
                raw_id,
                title,
                modified: dir_modified(&save.path()),
                size_bytes: dir_size(&save.path()),
            });
        }
    }
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    entries
}

fn read_sfo_title(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if data.get(0..4)? != b"\x00PSF" {
        return None;
    }
    let key_table_off = u32::from_le_bytes(data[8..12].try_into().ok()?) as usize;
    let data_table_off = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;
    let num_entries = u32::from_le_bytes(data[16..20].try_into().ok()?) as usize;

    for i in 0..num_entries {
        let e = data.get(20 + i * 16..20 + i * 16 + 16)?;
        let key_off = u16::from_le_bytes(e[0..2].try_into().ok()?) as usize;
        let fmt = e[3];
        let data_len = u32::from_le_bytes(e[4..8].try_into().ok()?) as usize;
        let data_off = u32::from_le_bytes(e[12..16].try_into().ok()?) as usize;

        let kp = key_table_off + key_off;
        let ke = data.get(kp..)?.iter().position(|&b| b == 0)? + kp;
        let key = std::str::from_utf8(data.get(kp..ke)?).ok()?;

        if key == "TITLE" && fmt == 2 {
            let dp = data_table_off + data_off;
            let val = data.get(dp..dp + data_len)?;
            let val = val.split(|&b| b == 0).next()?;
            return std::str::from_utf8(val).ok().map(|s| s.trim().to_string());
        }
    }
    None
}

// ─── PCSX2 (PS2) ────────────────────────────────────────────────────────────
// <memcards-dir>/*.ps2

const PS2MC_MAGIC: &[u8] = b"Sony PS2 Memory Card Format ";

/// PCSX2 creates 8-MB memcard placeholders before any game writes to them.
/// Those files have the right size and `.ps2` extension but their first
/// bytes are all zero (no SuperBlock yet). Skip them so they don't show up
/// as listable entries.
fn ps2_card_is_formatted(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; PS2MC_MAGIC.len()];
    if f.read_exact(&mut buf).is_err() {
        return false;
    }
    buf == PS2MC_MAGIC
}

fn list_pcsx2(memcards_dir: &Path) -> Vec<SaveEntry> {
    let Ok(files) = std::fs::read_dir(memcards_dir) else {
        return vec![];
    };
    let mut entries: Vec<SaveEntry> = files
        .flatten()
        .filter(|e| {
            e.path().is_file()
                && e.file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .ends_with(".ps2")
                && ps2_card_is_formatted(&e.path())
        })
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = meta
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let dt: DateTime<Local> = t.into();
                    dt.format("%d/%m/%Y %H:%M").to_string()
                });
            SaveEntry {
                title: name.trim_end_matches(".ps2").to_string(),
                raw_id: name,
                modified,
                size_bytes: size,
            }
        })
        .collect();
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    entries
}

// ─── single-save operations ─────────────────────────────────────────────────

pub fn get_save(emulator_id: &str, source_path: &str, raw_id: &str) -> Option<SaveEntry> {
    list_saves(emulator_id, source_path).into_iter().find(|e| e.raw_id == raw_id)
}

pub fn save_fs_path(emulator_id: &str, source_path: &str, raw_id: &str) -> Option<std::path::PathBuf> {
    let root = Path::new(source_path);
    let p = match emulator_id {
        "eden" => {
            let base = eden_user_saves_base(root);
            std::fs::read_dir(&base).ok()?.flatten()
                .map(|u| u.path().join(raw_id))
                .find(|p| p.exists())?
        }
        "rpcs3" => {
            let home = root.join("home");
            std::fs::read_dir(&home).ok()?.flatten()
                .map(|u| u.path().join("savedata").join(raw_id))
                .find(|p| p.exists())?
        }
        "pcsx2" => root.join(raw_id),
        _ => return None,
    };
    if p.exists() { Some(p) } else { None }
}

pub fn delete_save(emulator_id: &str, source_path: &str, raw_id: &str) -> Result<(), String> {
    let p = save_fs_path(emulator_id, source_path, raw_id)
        .ok_or_else(|| format!("save not found: {raw_id}"))?;
    if p.is_dir() {
        std::fs::remove_dir_all(&p).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(&p).map_err(|e| e.to_string())
    }
}

pub fn sync_one(emulator_id: &str, source: &str, dest: &str, raw_id: &str) -> Result<(), String> {
    let dest_root = Path::new(dest).join(emulator_id);
    std::fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;
    let opts = fs_extra::dir::CopyOptions { overwrite: true, copy_inside: false, ..Default::default() };
    match emulator_id {
        "eden" => {
            let base_src = eden_user_saves_base(Path::new(source));
            let base_dst = eden_user_saves_base(&dest_root);
            for user in std::fs::read_dir(&base_src).map_err(|e| e.to_string())?.flatten() {
                let from = user.path().join(raw_id);
                if from.exists() {
                    let to = base_dst.join(user.file_name());
                    std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
                    return fs_extra::dir::copy(&from, &to, &opts)
                        .map(|_| ()).map_err(|e| e.to_string());
                }
            }
            Err(format!("save not found: {raw_id}"))
        }
        "rpcs3" => {
            let home_src = Path::new(source).join("home");
            let home_dst = dest_root.join("home");
            for user in std::fs::read_dir(&home_src).map_err(|e| e.to_string())?.flatten() {
                let from = user.path().join("savedata").join(raw_id);
                if from.exists() {
                    let to = home_dst.join(user.file_name()).join("savedata");
                    std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
                    return fs_extra::dir::copy(&from, &to, &opts)
                        .map(|_| ()).map_err(|e| e.to_string());
                }
            }
            Err(format!("save not found: {raw_id}"))
        }
        "pcsx2" => {
            let from = Path::new(source).join(raw_id);
            let to   = dest_root.join(raw_id);
            std::fs::copy(&from, &to).map(|_| ()).map_err(|e| e.to_string())
        }
        _ => Err("emulator not supported".into()),
    }
}

// ─── helpers ────────────────────────────────────────────────────────────────

fn dir_size(path: &Path) -> u64 {
    walkdir_size(path)
}

fn walkdir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if p.is_dir() {
                total += walkdir_size(&p);
            }
        }
    }
    total
}

fn dir_modified(path: &Path) -> Option<String> {
    // Latest mtime among direct children
    let mut latest: Option<std::time::SystemTime> = None;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(m) = entry.metadata() {
                if let Ok(t) = m.modified() {
                    latest = Some(match latest {
                        Some(l) if t > l => t,
                        Some(l) => l,
                        None => t,
                    });
                }
            }
        }
    }
    latest.map(|t| {
        let dt: DateTime<Local> = t.into();
        dt.format("%d/%m/%Y %H:%M").to_string()
    })
}
