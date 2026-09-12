use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Local};
use serde::Deserialize;

const BLAWAR_URL: &str =
    "https://raw.githubusercontent.com/blawar/titledb/master/US.en.json";

pub type TitleMap = HashMap<String, String>;

#[derive(Default)]
pub struct TitleDb {
    pub map: Arc<TitleMap>,
    pub last_update: Option<DateTime<Local>>,
}

pub fn cache_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("titledb.json")
}

pub async fn download(target: &Path) -> Result<(), String> {
    let resp = reqwest::Client::new()
        .get(BLAWAR_URL)
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

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

pub fn parse(target: &Path) -> Result<TitleMap, String> {
    let data = std::fs::read(target).map_err(|e| e.to_string())?;
    let raw: HashMap<String, Entry> =
        serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    let mut map = TitleMap::new();
    for (_, entry) in raw {
        if let (Some(id), Some(name)) = (entry.id, entry.name) {
            if !id.is_empty() && !name.is_empty() {
                map.insert(id.to_uppercase(), name);
            }
        }
    }
    Ok(map)
}

/// Per-title fallback when an id isn't in the cached blawar dump.
pub async fn fetch_nlib_name(id: &str) -> Option<String> {
    let url = format!("https://api.nlib.cc/nx/{}?fields=name", id);
    let resp = reqwest::Client::new().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    v["name"].as_str().map(|s| s.to_string())
}

pub fn cache_mtime(path: &Path) -> Option<DateTime<Local>> {
    let meta = std::fs::metadata(path).ok()?;
    let t = meta.modified().ok()?;
    Some(t.into())
}
