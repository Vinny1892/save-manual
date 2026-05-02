mod db;
mod detect;
mod saves;
mod sync;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::time::Duration;

use chrono::Local;
use db::Emulator;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use serde::Serialize;
use sysinfo::{ProcessesToUpdate, System};
use sync::{copy_eden_saves, copy_saves, make_watcher};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Mutex};

struct WatcherEntry {
    _watcher: RecommendedWatcher,
    _stop_tx: mpsc::Sender<()>,
}

struct ProcWatcherEntry {
    _stop_tx: mpsc::Sender<()>,
}

pub struct AppData {
    conn: Connection,
    watchers: HashMap<String, WatcherEntry>,
    proc_watchers: HashMap<String, ProcWatcherEntry>,
}

type AppState = Mutex<AppData>;

#[derive(Debug, Clone, Serialize)]
pub struct EmulatorView {
    #[serde(flatten)]
    emulator: Emulator,
    watching: bool,
    proc_watching: bool,
}

fn view(emulator: Emulator, data: &AppData) -> EmulatorView {
    let watching = data.watchers.contains_key(&emulator.id);
    let proc_watching = data.proc_watchers.contains_key(&emulator.id);
    EmulatorView { emulator, watching, proc_watching }
}

/// Compara nome do processo ignorando .exe e case.
/// Também aceita match parcial (contains) para package names Android.
fn proc_matches(proc_name: &OsStr, target: &str) -> bool {
    let proc = proc_name.to_string_lossy();
    let p = proc.trim_end_matches(".exe");
    let t = target.trim_end_matches(".exe");
    p.eq_ignore_ascii_case(t) || proc.contains(target)
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
async fn set_process_name(
    id: String,
    process_name: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let s = state.lock().await;
    db::set_process_name(&s.conn, &id, &process_name)
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
        stop_watch_inner(&id, &state).await?;
        stop_proc_watch_inner(&id, &state).await?;
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

    let result = do_sync(&id, &source, &dest);
    record_result(&id, &result, &state).await;
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
                    let result = do_sync(&id_clone, &source, &dest);
                    let app_state = app_clone.state::<AppState>();
                    record_result(&id_clone, &result, &app_state).await;
                    emit_changed(&app_clone, &app_state, &id_clone).await;
                }
                _ = stop_rx.recv() => break,
            }
        }
    });

    {
        let mut s = state.lock().await;
        s.watchers.insert(id.clone(), WatcherEntry { _watcher: watcher, _stop_tx: stop_tx });
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

#[tauri::command]
async fn start_proc_watch(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (source, dest, proc_name) = {
        let s = state.lock().await;
        if s.proc_watchers.contains_key(&id) {
            return Ok(());
        }
        let emu = db::get(&s.conn, &id)?;
        if !emu.enabled {
            return Err("Emulador desativado".into());
        }
        if emu.process_name.is_empty() {
            return Err("Nome do processo não configurado".into());
        }
        if emu.source_path.is_empty() || emu.dest_path.is_empty() {
            return Err("Configuração incompleta".into());
        }
        (
            std::path::PathBuf::from(&emu.source_path),
            std::path::PathBuf::from(&emu.dest_path),
            emu.process_name.clone(),
        )
    };

    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

    let app_clone = app.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        let mut sys = System::new();
        let mut was_running = false;

        loop {
            tokio::select! {
                _ = stop_rx.recv() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    sys.refresh_processes(ProcessesToUpdate::All, true);
                    let is_running = sys.processes().values()
                        .any(|p| proc_matches(p.name(), &proc_name));

                    if was_running && !is_running {
                        let result = do_sync(&id_clone, &source, &dest);
                        let app_state = app_clone.state::<AppState>();
                        record_result(&id_clone, &result, &app_state).await;
                        emit_changed(&app_clone, &app_state, &id_clone).await;
                    }

                    was_running = is_running;
                }
            }
        }
    });

    {
        let mut s = state.lock().await;
        s.proc_watchers.insert(id.clone(), ProcWatcherEntry { _stop_tx: stop_tx });
    }
    emit_changed(&app, &state, &id).await;
    Ok(())
}

#[tauri::command]
async fn stop_proc_watch(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    stop_proc_watch_inner(&id, &state).await?;
    emit_changed(&app, &state, &id).await;
    Ok(())
}

#[tauri::command]
async fn list_saves(id: String, state: State<'_, AppState>) -> Result<Vec<saves::SaveEntry>, String> {
    let source = {
        let s = state.lock().await;
        db::get(&s.conn, &id)?.source_path
    };
    if source.is_empty() {
        return Err("Configuração incompleta".into());
    }
    Ok(saves::list_saves(&id, &source))
}

#[tauri::command]
async fn get_setting(key: String, state: State<'_, AppState>) -> Result<Option<String>, String> {
    let s = state.lock().await;
    db::get_setting(&s.conn, &key)
}

#[tauri::command]
async fn set_setting(key: String, value: String, state: State<'_, AppState>) -> Result<(), String> {
    let s = state.lock().await;
    db::set_setting(&s.conn, &key, &value)
}

#[tauri::command]
async fn get_save_entry(
    id: String,
    raw_id: String,
    state: State<'_, AppState>,
) -> Result<Option<saves::SaveEntry>, String> {
    let source = {
        let s = state.lock().await;
        db::get(&s.conn, &id)?.source_path
    };
    if source.is_empty() {
        return Err("Configuração incompleta".into());
    }
    Ok(saves::get_save(&id, &source, &raw_id))
}

#[tauri::command]
async fn delete_save_entry(
    id: String,
    raw_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let source = {
        let s = state.lock().await;
        db::get(&s.conn, &id)?.source_path
    };
    saves::delete_save(&id, &source, &raw_id)
}

#[tauri::command]
async fn sync_one_save(
    id: String,
    raw_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (source, dest) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        if emu.source_path.is_empty() || emu.dest_path.is_empty() {
            return Err("Configuração incompleta".into());
        }
        (emu.source_path, emu.dest_path)
    };
    let result = saves::sync_one(&id, &source, &dest, &raw_id);
    record_result(&id, &result, &state).await;
    emit_changed(&app, &state, &id).await;
    result
}

#[tauri::command]
async fn open_save_folder(
    id: String,
    raw_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let source = {
        let s = state.lock().await;
        db::get(&s.conn, &id)?.source_path
    };
    let path = saves::save_fs_path(&id, &source, &raw_id)
        .ok_or_else(|| format!("save not found: {raw_id}"))?;
    app.opener()
        .open_path(path.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| e.to_string())
}

const SGDB_KEY: &str = "f80f92019254471cca9d62ff91c21eee";

#[tauri::command]
async fn fetch_cover_url(title: String) -> Result<Option<String>, String> {
    let client = reqwest::Client::new();

    let search: serde_json::Value = client
        .get(format!(
            "https://www.steamgriddb.com/api/v2/search/autocomplete/{}",
            urlencoding::encode(&title)
        ))
        .header("Authorization", format!("Bearer {}", SGDB_KEY))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let game_id = match search["data"][0]["id"].as_u64() {
        Some(id) => id,
        None => return Ok(None),
    };

    let grids: serde_json::Value = client
        .get(format!(
            "https://www.steamgriddb.com/api/v2/grids/game/{}?dimensions=600x900&limit=1",
            game_id
        ))
        .header("Authorization", format!("Bearer {}", SGDB_KEY))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    Ok(grids["data"][0]["url"].as_str().map(|s| s.to_string()))
}

#[tauri::command]
async fn detect_save_paths(id: String) -> Result<Vec<detect::DetectCandidate>, String> {
    Ok(detect::detect_paths(&id))
}

#[tauri::command]
async fn get_eden_uuid(nand_path: String) -> Result<Option<String>, String> {
    Ok(sync::read_eden_uuid(std::path::Path::new(&nand_path)))
}

async fn stop_watch_inner(id: &str, state: &State<'_, AppState>) -> Result<(), String> {
    let mut s = state.lock().await;
    s.watchers.remove(id);
    Ok(())
}

async fn stop_proc_watch_inner(id: &str, state: &State<'_, AppState>) -> Result<(), String> {
    let mut s = state.lock().await;
    s.proc_watchers.remove(id);
    Ok(())
}

fn do_sync(id: &str, source: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    if id == "eden" {
        copy_eden_saves(source, dest)
    } else {
        copy_saves(source, dest)
    }
}

async fn record_result(id: &str, result: &Result<(), String>, state: &State<'_, AppState>) {
    let s = state.lock().await;
    let ts = Local::now().format("%d/%m/%Y %H:%M:%S").to_string();
    let _ = match result {
        Ok(_) => db::set_last_sync(&s.conn, id, &ts),
        Err(e) => db::set_last_error(&s.conn, id, e),
    };
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
                proc_watchers: HashMap::new(),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_emulators,
            get_emulator,
            set_emulator_paths,
            set_process_name,
            set_enabled,
            sync_now,
            start_watch,
            stop_watch,
            start_proc_watch,
            stop_proc_watch,
            detect_save_paths,
            get_eden_uuid,
            list_saves,
            fetch_cover_url,
            get_save_entry,
            delete_save_entry,
            sync_one_save,
            open_save_folder,
            get_setting,
            set_setting,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
