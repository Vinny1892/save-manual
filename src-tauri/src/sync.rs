use std::path::Path;

use notify::{Event, EventKind, RecommendedWatcher};
use tokio::sync::mpsc;

pub fn copy_saves(source: &Path, dest: &Path) -> Result<(), String> {
    if !source.exists() {
        return Err(format!("Origem não encontrada: {}", source.display()));
    }
    fs_extra::dir::copy(
        source,
        dest,
        &fs_extra::dir::CopyOptions {
            overwrite: true,
            copy_inside: true,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
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
