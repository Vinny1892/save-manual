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
// nand/user/save/<title-id-16hex>/

fn list_eden(nand_root: &Path) -> Vec<SaveEntry> {
    let saves_dir = nand_root.join("user/save");
    let Ok(dirs) = std::fs::read_dir(&saves_dir) else {
        return vec![];
    };
    let mut entries: Vec<SaveEntry> = dirs
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let raw_id = e.file_name().to_string_lossy().into_owned();
            if raw_id.len() != 16 || !raw_id.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            Some(SaveEntry {
                title: raw_id.clone(),
                raw_id,
                modified: dir_modified(&e.path()),
                size_bytes: dir_size(&e.path()),
            })
        })
        .collect();
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
        "eden" => root.join("user/save").join(raw_id),
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
    let opts = fs_extra::dir::CopyOptions { overwrite: true, copy_inside: false, ..Default::default() };
    match emulator_id {
        "eden" => {
            let from = Path::new(source).join("user/save").join(raw_id);
            let to   = Path::new(dest).join("user/save");
            std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
            fs_extra::dir::copy(&from, &to, &opts).map(|_| ()).map_err(|e| e.to_string())
        }
        "rpcs3" => {
            let home_src = Path::new(source).join("home");
            let home_dst = Path::new(dest).join("home");
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
            let to   = Path::new(dest).join(raw_id);
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
