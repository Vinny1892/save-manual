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

/// Sincroniza apenas o que importa da NAND do Eden:
///   - system/save/8000000000000010  (definição do perfil / UUID)
///   - user/save/                    (saves dos jogos)
///
/// Isso elimina a necessidade de sincronizar a NAND inteira e de configurar
/// o UUID manualmente: o perfil da origem é propagado para o destino.
pub fn copy_eden_saves(nand_src: &Path, nand_dst: &Path) -> Result<(), String> {
    if !nand_src.exists() {
        return Err(format!("NAND origem não encontrada: {}", nand_src.display()));
    }

    let opts = fs_extra::dir::CopyOptions {
        overwrite: true,
        copy_inside: false,
        ..Default::default()
    };

    // Perfil / UUID
    let account_src = nand_src.join("system/save/8000000000000010");
    if account_src.is_dir() {
        let parent = nand_dst.join("system/save");
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        fs_extra::dir::copy(&account_src, &parent, &opts).map_err(|e| e.to_string())?;
    }

    // Saves dos jogos
    let saves_src = nand_src.join("user/save");
    if saves_src.is_dir() {
        let parent = nand_dst.join("user");
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        fs_extra::dir::copy(&saves_src, &parent, &opts).map_err(|e| e.to_string())?;
    }

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
