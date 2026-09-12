use std::path::Path;

use notify::{Event, EventKind, RecommendedWatcher};
use tokio::sync::mpsc;

/// Lê o UUID do perfil ativo escaneando nand/user/save/<title-id>/<user-id>/.
/// O user-id é uma string hex de 32 chars (128 bits).
pub fn read_eden_uuid(nand_root: &Path) -> Option<String> {
    let saves = nand_root.join("user/save");
    if !saves.is_dir() {
        return None;
    }
    for title_entry in std::fs::read_dir(&saves).ok()?.flatten() {
        if !title_entry.path().is_dir() {
            continue;
        }
        for uuid_entry in std::fs::read_dir(title_entry.path()).ok()?.flatten() {
            let name = uuid_entry.file_name();
            let s = name.to_string_lossy();
            if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(s.into_owned());
            }
        }
    }
    None
}

pub fn make_watcher(event_tx: mpsc::Sender<()>) -> Result<RecommendedWatcher, String> {
    notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                let _ = event_tx.try_send(());
            }
            _ => {}
        }
    })
    .map_err(|e| e.to_string())
}
