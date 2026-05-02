//! PS2 serial → game name resolution backed by PCSX2's GameIndex.yaml.
//!
//! The file is the canonical PS2 game database the PCSX2 project itself
//! uses (~10 MB, MIT-licensed). We download to `app_data_dir/ps2-gameindex.yaml`
//! and parse a minimal `serial → english name` map without pulling a YAML
//! parser crate (the format is regular enough for a 30-line line scanner).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Local};

const URL: &str =
    "https://raw.githubusercontent.com/PCSX2/pcsx2/master/bin/resources/GameIndex.yaml";

pub type Ps2Map = HashMap<String, String>;

#[derive(Default)]
pub struct Ps2Db {
    pub map: Arc<Ps2Map>,
    pub last_update: Option<DateTime<Local>>,
}

pub fn cache_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("ps2-gameindex.yaml")
}

pub async fn download(target: &Path) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .get(URL)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if let Some(p) = target.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(target, &bytes).map_err(|e| e.to_string())?;
    Ok(())
}

/// Parse the GameIndex.yaml structure:
///
/// ```yaml
/// SLUS-12345:
///   name: "..."        # native title (could be JP)
///   name-en: "..."     # English title (only on Japanese entries)
///   region: "NTSC-U"
/// ```
///
/// We pick `name-en` when present, falling back to `name` otherwise — this
/// gives us a consistently-English UI for both western and JP releases.
pub fn parse(target: &Path) -> Result<Ps2Map, String> {
    let data = std::fs::read_to_string(target).map_err(|e| e.to_string())?;
    let mut map = Ps2Map::new();

    let mut current_serial: Option<String> = None;
    let mut name_default: Option<String> = None;
    let mut name_en: Option<String> = None;

    fn commit(
        serial: &mut Option<String>,
        default: &mut Option<String>,
        en: &mut Option<String>,
        out: &mut Ps2Map,
    ) {
        if let Some(s) = serial.take() {
            let name = en.take().or_else(|| default.take());
            if let Some(n) = name {
                out.insert(s.to_uppercase(), n);
            }
        }
        *default = None;
        *en = None;
    }

    fn unquote(v: &str) -> String {
        let v = v.trim();
        let v = v.strip_prefix('"').unwrap_or(v);
        let v = v.strip_suffix('"').unwrap_or(v);
        let v = v.strip_prefix('\'').unwrap_or(v);
        let v = v.strip_suffix('\'').unwrap_or(v);
        v.to_string()
    }

    for line in data.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !trimmed.starts_with(' ') {
            // top-level entry: `SERIAL:`
            if let Some(rest) = trimmed.strip_suffix(':') {
                commit(&mut current_serial, &mut name_default, &mut name_en, &mut map);
                current_serial = Some(rest.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("  name-en:") {
            let v = unquote(rest);
            if !v.is_empty() {
                name_en = Some(v);
            }
        } else if let Some(rest) = trimmed.strip_prefix("  name:") {
            let v = unquote(rest);
            if !v.is_empty() {
                name_default = Some(v);
            }
        }
    }
    commit(&mut current_serial, &mut name_default, &mut name_en, &mut map);

    Ok(map)
}

pub fn cache_mtime(path: &Path) -> Option<DateTime<Local>> {
    let meta = std::fs::metadata(path).ok()?;
    let t = meta.modified().ok()?;
    Some(t.into())
}
