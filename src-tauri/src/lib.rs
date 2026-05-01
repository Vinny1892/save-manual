mod db;
mod sync;

use std::collections::HashMap;
use std::time::Duration;

use chrono::Local;
use db::Emulator;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use serde::Serialize;
use sync::{copy_saves, make_watcher};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Mutex};

struct WatcherEntry {
    _watcher: RecommendedWatcher,
    _stop_tx: mpsc::Sender<()>,
}

pub struct AppData {
    conn: Connection,
    watchers: HashMap<String, WatcherEntry>,
}

type AppState = Mutex<AppData>;

#[derive(Debug, Clone, Serialize)]
pub struct EmulatorView {
    #[serde(flatten)]
    emulator: Emulator,
    watching: bool,
}

fn view(emulator: Emulator, data: &AppData) -> EmulatorView {
    let watching = data.watchers.contains_key(&emulator.id);
    EmulatorView {
        emulator,
        watching,
    }
}

#[tauri::command]
async fn list_emulators(state: State<'_, AppState>) -> Result<Vec<EmulatorView>, String> {
    let s = state.lock().await;
    let emus = db::list_all(&s.conn)?;
    Ok(emus.into_iter().map(|e| view(e, &s)).collect())
}

#[tauri::command]
async fn get_emulator(id: String, state: State<'_, AppState>) -> Result<EmulatorView, String> {
    let s = state.lock().await;
    let emu = db::get(&s.conn, &id)?;
    Ok(view(emu, &s))
}

#[tauri::command]
async fn set_emulator_paths(
    id: String,
    source_path: String,
    dest_path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let s = state.lock().await;
    db::set_paths(&s.conn, &id, &source_path, &dest_path)
}

#[tauri::command]
async fn set_enabled(
    id: String,
    enabled: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let s = state.lock().await;
        db::set_enabled(&s.conn, &id, enabled)?;
    }
    if !enabled {
        let _ = stop_watch_inner(&id, &state).await;
    }
    emit_changed(&app, &state, &id).await;
    Ok(())
}

#[tauri::command]
async fn sync_now(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (source, dest) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        if !emu.enabled {
            return Err("Emulador desativado".into());
        }
        if emu.source_path.is_empty() || emu.dest_path.is_empty() {
            return Err("Configuração incompleta".into());
        }
        (
            std::path::PathBuf::from(&emu.source_path),
            std::path::PathBuf::from(&emu.dest_path),
        )
    };

    let result = copy_saves(&source, &dest);

    {
        let s = state.lock().await;
        match &result {
            Ok(_) => db::set_last_sync(
                &s.conn,
                &id,
                &Local::now().format("%d/%m/%Y %H:%M:%S").to_string(),
            )?,
            Err(e) => db::set_last_error(&s.conn, &id, e)?,
        }
    }
    emit_changed(&app, &state, &id).await;
    result
}

#[tauri::command]
async fn start_watch(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (source, dest) = {
        let s = state.lock().await;
        if s.watchers.contains_key(&id) {
            return Ok(());
        }
        let emu = db::get(&s.conn, &id)?;
        if !emu.enabled {
            return Err("Emulador desativado".into());
        }
        if emu.source_path.is_empty() || emu.dest_path.is_empty() {
            return Err("Configuração incompleta".into());
        }
        (
            std::path::PathBuf::from(&emu.source_path),
            std::path::PathBuf::from(&emu.dest_path),
        )
    };

    let (event_tx, mut event_rx) = mpsc::channel::<()>(16);
    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

    let mut watcher = make_watcher(event_tx)?;
    watcher
        .watch(&source, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;

    let app_clone = app.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = event_rx.recv() => {
                    if msg.is_none() { break; }
                    loop {
                        match tokio::time::timeout(Duration::from_secs(2), event_rx.recv()).await {
                            Ok(Some(())) => continue,
                            _ => break,
                        }
                    }
                    let result = copy_saves(&source, &dest);
                    let app_state = app_clone.state::<AppState>();
                    {
                        let s = app_state.lock().await;
                        let _ = match &result {
                            Ok(_) => db::set_last_sync(
                                &s.conn,
                                &id_clone,
                                &Local::now().format("%d/%m/%Y %H:%M:%S").to_string(),
                            ),
                            Err(e) => db::set_last_error(&s.conn, &id_clone, e),
                        };
                    }
                    emit_changed(&app_clone, &app_state, &id_clone).await;
                }
                _ = stop_rx.recv() => { break; }
            }
        }
    });

    {
        let mut s = state.lock().await;
        s.watchers.insert(
            id.clone(),
            WatcherEntry {
                _watcher: watcher,
                _stop_tx: stop_tx,
            },
        );
    }
    emit_changed(&app, &state, &id).await;
    Ok(())
}

#[tauri::command]
async fn stop_watch(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    stop_watch_inner(&id, &state).await?;
    emit_changed(&app, &state, &id).await;
    Ok(())
}

async fn stop_watch_inner(id: &str, state: &State<'_, AppState>) -> Result<(), String> {
    let mut s = state.lock().await;
    s.watchers.remove(id);
    Ok(())
}

async fn emit_changed(app: &AppHandle, state: &State<'_, AppState>, id: &str) {
    let s = state.lock().await;
    if let Ok(emu) = db::get(&s.conn, id) {
        let v = view(emu, &s);
        let _ = app.emit("emulator-changed", v);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("falha ao resolver app_data_dir");
            std::fs::create_dir_all(&app_data_dir).ok();
            let db_path = app_data_dir.join("save-sync.db");
            let conn = db::open(&db_path).expect("falha ao abrir DB");
            app.manage(Mutex::new(AppData {
                conn,
                watchers: HashMap::new(),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_emulators,
            get_emulator,
            set_emulator_paths,
            set_enabled,
            sync_now,
            start_watch,
            stop_watch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
