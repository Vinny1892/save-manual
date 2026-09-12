//! Camada Tauri do client de PC. O domínio mora em `save-sync-core`; o que
//! fica aqui é o que só existe no client: os comandos expostos à UI, o
//! estado da aplicação e os watchers locais (filesystem e processo), que
//! precisam rodar na máquina onde o emulador está.

use save_sync_core::{backend, db, detect, engine, history, ps2db, ps2mc, rclone, saves, sync, titledb};

use engine::{do_sync_async, proc_matches, validate_config, ProgressSink, SyncOutcome};
use history::{
    find_conflicts, group_history_entries, prune_history, rename_keep_both_path,
    strip_conflict_marker, ConflictEntry, PruneSummary, SaveHistoryEntry,
};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use backend::Backend;
use chrono::Local;
use db::{Emulator, HistorySettings};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use serde::Serialize;
use sysinfo::{ProcessesToUpdate, System};
use sync::make_watcher;
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
    titles: titledb::TitleDb,
    title_db_path: std::path::PathBuf,
    title_db_refreshing: bool,
    ps2: ps2db::Ps2Db,
    ps2_db_path: std::path::PathBuf,
    ps2_db_refreshing: bool,
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

/// Liga o progresso do motor de sync ao evento `sync-progress` do Tauri.
/// O core não conhece `AppHandle`; esta é a única tradução necessária.
struct TauriProgress(AppHandle);

impl ProgressSink for TauriProgress {
    fn progress(&self, emulator_id: &str, active: bool, stats: Option<serde_json::Value>) {
        let _ = self.0.emit(
            "sync-progress",
            serde_json::json!({
                "id": emulator_id,
                "active": active,
                "stats": stats.unwrap_or(serde_json::Value::Null),
            }),
        );
    }
}

fn progress_sink(app: &AppHandle) -> Arc<dyn ProgressSink> {
    Arc::new(TauriProgress(app.clone()))
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
    dest_kind: String,
    dest_remote: String,
    dest_path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let kind = match dest_kind.as_str() {
        "local" | "rclone" => dest_kind.as_str(),
        "" => "local",
        _ => return Err("dest_kind_invalid".into()),
    };
    {
        let s = state.lock().await;
        db::set_paths(&s.conn, &id, &source_path, kind, &dest_remote, &dest_path)?;
    }
    emit_changed(&app, &state, &id).await;
    Ok(())
}

#[tauri::command]
async fn set_process_name(
    id: String,
    process_name: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let s = state.lock().await;
        db::set_process_name(&s.conn, &id, &process_name)?;
    }
    emit_changed(&app, &state, &id).await;
    Ok(())
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
    let (source, emu, history) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        if !emu.enabled {
            return Err("emulator_disabled".into());
        }
        validate_config(&emu)?;
        let history = db::get_history_settings(&s.conn, &id)?;
        (std::path::PathBuf::from(&emu.source_path), emu, history)
    };

    let outcome = do_sync_async(emu, source, history, progress_sink(&app)).await;
    if matches!(&outcome, Ok(SyncOutcome { initial: true })) {
        let s = state.lock().await;
        let _ = db::mark_bisync_initialized(&s.conn, &id);
    }
    let result = outcome.map(|_| ());
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
    let (source, emu) = {
        let s = state.lock().await;
        if s.watchers.contains_key(&id) {
            return Ok(());
        }
        let emu = db::get(&s.conn, &id)?;
        if !emu.enabled {
            return Err("emulator_disabled".into());
        }
        validate_config(&emu)?;
        (std::path::PathBuf::from(&emu.source_path), emu)
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
                    let app_state = app_clone.state::<AppState>();
                    let history = {
                        let s = app_state.lock().await;
                        db::get_history_settings(&s.conn, &id_clone).unwrap_or_else(|_| HistorySettings::defaults_for(&id_clone))
                    };
                    let outcome = do_sync_async(emu.clone(), source.clone(), history, progress_sink(&app_clone)).await;
                    if matches!(&outcome, Ok(SyncOutcome { initial: true })) {
                        let s = app_state.lock().await;
                        let _ = db::mark_bisync_initialized(&s.conn, &id_clone);
                    }
                    let result = outcome.map(|_| ());
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
    let (source, emu, proc_name) = {
        let s = state.lock().await;
        if s.proc_watchers.contains_key(&id) {
            return Ok(());
        }
        let emu = db::get(&s.conn, &id)?;
        if !emu.enabled {
            return Err("emulator_disabled".into());
        }
        if emu.process_name.is_empty() {
            return Err("process_name_missing".into());
        }
        validate_config(&emu)?;
        let proc_name = emu.process_name.clone();
        (std::path::PathBuf::from(&emu.source_path), emu, proc_name)
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
                        let app_state = app_clone.state::<AppState>();
                        let history = {
                            let s = app_state.lock().await;
                            db::get_history_settings(&s.conn, &id_clone)
                                .unwrap_or_else(|_| HistorySettings::defaults_for(&id_clone))
                        };
                        let outcome = do_sync_async(emu.clone(), source.clone(), history, progress_sink(&app_clone)).await;
                        if matches!(&outcome, Ok(SyncOutcome { initial: true })) {
                            let s = app_state.lock().await;
                            let _ = db::mark_bisync_initialized(&s.conn, &id_clone);
                        }
                        let result = outcome.map(|_| ());
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
    let (source, titles) = {
        let s = state.lock().await;
        (db::get(&s.conn, &id)?.source_path, s.titles.map.clone())
    };
    if source.is_empty() {
        return Err("config_incomplete_source".into());
    }
    let mut entries = saves::list_saves(&id, &source);
    if id == "eden" {
        for e in &mut entries {
            if let Some(name) = titles.get(&e.raw_id.to_uppercase()) {
                e.title = name.clone();
            }
        }
    }
    Ok(entries)
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
    let (source, titles) = {
        let s = state.lock().await;
        (db::get(&s.conn, &id)?.source_path, s.titles.map.clone())
    };
    if source.is_empty() {
        return Err("config_incomplete_source".into());
    }
    let mut entry = saves::get_save(&id, &source, &raw_id);
    if id == "eden" {
        if let Some(e) = entry.as_mut() {
            if let Some(name) = titles.get(&e.raw_id.to_uppercase()) {
                e.title = name.clone();
            } else if let Some(name) = titledb::fetch_nlib_name(&e.raw_id).await {
                e.title = name;
            }
        }
    }
    Ok(entry)
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
    let (source, emu) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        validate_config(&emu)?;
        (emu.source_path.clone(), emu)
    };
    let backend = Backend::for_emulator(&emu)?;
    let result = saves::sync_one(&id, &source, &backend, &raw_id);
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
        .ok_or_else(|| "save_not_found".to_string())?;
    app.opener()
        .open_path(path.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| e.to_string())
}

const SGDB_KEY: &str = "f80f92019254471cca9d62ff91c21eee";

#[tauri::command]
async fn fetch_cover_url(
    title: String,
    kind: Option<String>,
) -> Result<Option<String>, String> {
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

    // "icon" → square icon (best for list/thumb views).
    // anything else (incl. None) → 600x900 portrait grid (default for cards).
    let endpoint = match kind.as_deref() {
        Some("icon") => format!(
            "https://www.steamgriddb.com/api/v2/icons/game/{}?limit=1",
            game_id
        ),
        _ => format!(
            "https://www.steamgriddb.com/api/v2/grids/game/{}?dimensions=600x900&limit=1",
            game_id
        ),
    };

    let assets: serde_json::Value = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", SGDB_KEY))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    Ok(assets["data"][0]["url"].as_str().map(|s| s.to_string()))
}

/// Downloads a cover image and computes a saturation-weighted dominant color.
/// Done in Rust to avoid the Tauri webview CORS issue with the SGDB CDN
/// (which would taint the canvas in the frontend).
#[tauri::command]
async fn fetch_cover_tint(url: String) -> Result<Option<String>, String> {
    let client = reqwest::Client::builder()
        .user_agent("save-sync/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| format!("body: {e}"))?;
    if !status.is_success() {
        return Err(format!("http {} from {} ({} B)", status, url, bytes.len()));
    }

    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("decode ({} B): {e}", bytes.len()))?;
    // Downscale aggressively — pixel sampling is approximate by nature, so
    // 32x48 is plenty and keeps the loop fast.
    let small = img.thumbnail(32, 48).to_rgb8();

    let mut acc_r: f64 = 0.0;
    let mut acc_g: f64 = 0.0;
    let mut acc_b: f64 = 0.0;
    let mut total: f64 = 0.0;

    for px in small.pixels() {
        let r = px.0[0] as f64;
        let g = px.0[1] as f64;
        let b = px.0[2] as f64;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max < 35.0 || min > 225.0 {
            continue; // skip near-black / near-white
        }
        let sat = if max == 0.0 { 0.0 } else { (max - min) / max };
        let w = sat * 4.0 + 1.0;
        acc_r += r * w;
        acc_g += g * w;
        acc_b += b * w;
        total += w;
    }

    if total == 0.0 {
        return Ok(None);
    }
    let mut r = acc_r / total;
    let mut g = acc_g / total;
    let mut b = acc_b / total;

    // Floor brightness so tinted text/borders are always legible against
    // the dark/light backgrounds. Perceived luminance via Rec. 709.
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if lum < 140.0 && lum > 0.0 {
        let scale = 140.0 / lum;
        r = (r * scale).min(255.0);
        g = (g * scale).min(255.0);
        b = (b * scale).min(255.0);
    }

    Ok(Some(format!(
        "{}, {}, {}",
        r.round() as u32,
        g.round() as u32,
        b.round() as u32
    )))
}

#[tauri::command]
async fn detect_save_paths(id: String) -> Result<Vec<detect::DetectCandidate>, String> {
    Ok(detect::detect_paths(&id))
}

#[tauri::command]
async fn rclone_version() -> Result<serde_json::Value, String> {
    rclone::rpc_json("core/version", serde_json::json!({}))
}

#[tauri::command]
async fn rclone_list_remotes() -> Result<Vec<String>, String> {
    rclone::list_remotes()
}

#[tauri::command]
async fn rclone_create_s3_remote(config: rclone::S3RemoteConfig) -> Result<(), String> {
    rclone::create_s3_remote(&config)
}

#[tauri::command]
async fn rclone_delete_remote(name: String) -> Result<(), String> {
    rclone::delete_remote(&name)
}

#[tauri::command]
async fn rclone_get_remote(name: String) -> Result<serde_json::Value, String> {
    rclone::get_remote(&name)
}

#[tauri::command]
async fn rclone_test_remote(name: String, path: Option<String>) -> Result<(), String> {
    rclone::test_remote(&name, path.as_deref().unwrap_or(""))
}

#[tauri::command]
async fn get_history_settings(
    id: String,
    state: State<'_, AppState>,
) -> Result<HistorySettings, String> {
    let s = state.lock().await;
    db::get_history_settings(&s.conn, &id)
}

#[tauri::command]
async fn set_history_settings(
    settings: HistorySettings,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let s = state.lock().await;
        db::set_history_settings(&s.conn, &settings)?;
    }
    // Emit a small ping so the UI knows to refresh (paths card already
    // listens on emulator-changed, we piggyback for consistency).
    let _ = app.emit("history-settings-changed", &settings.emulator_id);
    Ok(())
}

#[tauri::command]
async fn supports_incremental_history(id: String) -> Result<bool, String> {
    Ok(db::supports_incremental_history(&id))
}

#[tauri::command]
async fn prune_history_now(
    id: String,
    state: State<'_, AppState>,
) -> Result<PruneSummary, String> {
    let (emu, history) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        validate_config(&emu)?;
        let history = db::get_history_settings(&s.conn, &id)?;
        (emu, history)
    };
    // Manual prune triggered before any sync would hit a non-existent
    // `.history/` and spam rclone logs. Short-circuit with empty summary.
    if !history.bisync_initialized {
        return Ok(PruneSummary { deleted_count: 0, freed_bytes: 0 });
    }
    let backend = Backend::for_emulator(&emu)?;
    prune_history(&backend, &history)
}


#[tauri::command]
async fn list_save_history(
    id: String,
    raw_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<SaveHistoryEntry>, String> {
    let (source, emu, history) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        validate_config(&emu)?;
        let history = db::get_history_settings(&s.conn, &id)?;
        (emu.source_path.clone(), emu, history)
    };

    // Pre-sync there's no .history/ to walk. rclone logs noisy
    // "directory not found" if we ask anyway; short-circuit avoids it
    // and matches the "nothing to revert yet" UX.
    if !history.bisync_initialized {
        return Ok(Vec::new());
    }

    let sub_path = saves::save_sub_path(&id, &source, &raw_id)
        .ok_or_else(|| "save_not_found".to_string())?;
    let backend = Backend::for_emulator(&emu)?;

    // Single recursive listing of the whole .history/<emu_id>/ tree.
    // For a 30-day retention with daily syncs and ~20 saves, this is a few
    // thousand entries — cheap to walk client-side and avoids N round-trips.
    let history_root = backend.history_root_fs();
    let (history_fs, history_remote) = rclone::split_root(&history_root);
    let entries = rclone::list_recursive(&history_fs, &history_remote)?;

    Ok(group_history_entries(&entries, &sub_path))
}

#[tauri::command]
async fn list_conflicts(
    id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ConflictEntry>, String> {
    let (emu, history) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        validate_config(&emu)?;
        let history = db::get_history_settings(&s.conn, &id)?;
        (emu, history)
    };

    // Live root only exists after the first bisync — listing it earlier
    // generates the rclone "directory not found" error in logs. Skip and
    // return empty until then.
    if !history.bisync_initialized {
        return Ok(Vec::new());
    }

    let backend = Backend::for_emulator(&emu)?;
    let live = backend.live_fs();
    let (fs, remote) = rclone::split_root(&live);
    let entries = rclone::list_recursive(&fs, &remote)?;
    Ok(find_conflicts(&entries))
}

#[tauri::command]
async fn resolve_conflict(
    id: String,
    conflict_path: String,
    action: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (source, emu) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        validate_config(&emu)?;
        (emu.source_path.clone(), emu)
    };

    let (original, _) = strip_conflict_marker(&conflict_path)
        .ok_or_else(|| "conflict_marker_invalid".to_string())?;

    let backend = Backend::for_emulator(&emu)?;
    let live_root = backend.live_fs();
    let source_root = source.trim_end_matches(['/', '\\']).to_string();

    let live_current = format!("{live_root}/{original}");
    let live_conflict = format!("{live_root}/{conflict_path}");
    let source_current = format!("{source_root}/{original}");
    let source_conflict = format!("{source_root}/{conflict_path}");

    // `--conflict-loser num` always renames a single file (never a dir), so
    // every resolve operates on individual files regardless of the
    // emulator's save granularity.
    match action.as_str() {
        "keep_current" => {
            rclone::delete_file_at(&live_conflict)?;
            // Source might already be missing (user-deleted, or earlier
            // resync raced). Don't fail the whole operation in that case.
            let _ = rclone::delete_file_at(&source_conflict);
        }
        "use_conflict" => {
            rclone::copy_path(&live_conflict, &live_current, true)?;
            rclone::copy_path(&source_conflict, &source_current, true)?;
            rclone::delete_file_at(&live_conflict)?;
            let _ = rclone::delete_file_at(&source_conflict);
        }
        "keep_both" => {
            // Promote the conflict file to a permanent name (no `.conflictN`
            // suffix) so it survives future syncs as a sibling of the current.
            let renamed = rename_keep_both_path(&conflict_path);
            let live_renamed = format!("{live_root}/{renamed}");
            let source_renamed = format!("{source_root}/{renamed}");
            rclone::move_file_at(&live_conflict, &live_renamed)?;
            let _ = rclone::move_file_at(&source_conflict, &source_renamed);
        }
        _ => return Err("resolve_action_invalid".into()),
    }

    // Live + source mutated outside the bisync flow — invalidate state so
    // the next sync re-baselines instead of flagging artificial conflicts.
    {
        let s = state.lock().await;
        let _ = db::mark_bisync_needs_resync(&s.conn, &emu.id);
    }

    emit_changed(&app, &state, &id).await;
    Ok(())
}

#[tauri::command]
async fn revert_save(
    id: String,
    raw_id: String,
    timestamp: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let (source, emu) = {
        let s = state.lock().await;
        let emu = db::get(&s.conn, &id)?;
        validate_config(&emu)?;
        (emu.source_path.clone(), emu)
    };

    let sub_path = saves::save_sub_path(&id, &source, &raw_id)
        .ok_or_else(|| "save_not_found".to_string())?;
    let backend = Backend::for_emulator(&emu)?;

    // File-based emus (pcsx2) treat the save as a single file — picks
    // operations/copyfile downstream. Dir-based use sync/copy.
    let is_file = !db::supports_incremental_history(&emu.id);

    // Locate the version in history. Prefer full/ (always complete state)
    // over delta/ (only the files that were overwritten that run).
    let full_src = format!("{}/{}", backend.snapshot_full_fs(&timestamp), sub_path);
    let delta_src = backend.snapshot_delta_fs_at(&timestamp, &sub_path);

    let history_src = if rclone::stat_path(&full_src)?.is_some() {
        full_src
    } else if rclone::stat_path(&delta_src)?.is_some() {
        delta_src
    } else {
        return Err("save_not_found_in_history".into());
    };

    // Two destinations to keep consistent: the cloud/local live copy and
    // the local source path where the emulator actually reads from.
    // rclone accepts forward slashes on Windows, so we just concat — no
    // OS-separator dance needed.
    let live_target = format!("{}/{}", backend.live_fs(), sub_path);
    let source_target = format!("{}/{}", source.trim_end_matches(['/', '\\']), sub_path);

    rclone::copy_path(&history_src, &live_target, is_file)?;
    rclone::copy_path(&history_src, &source_target, is_file)?;

    // Invalidate bisync state — the next sync will --resync from this
    // post-revert baseline. Without this, bisync's cached listings would
    // see both sides "regressed" and surface false conflicts.
    {
        let s = state.lock().await;
        let _ = db::mark_bisync_needs_resync(&s.conn, &emu.id);
    }

    emit_changed(&app, &state, &id).await;
    Ok(())
}

#[tauri::command]
async fn list_memcard_saves(
    id: String,
    raw_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ps2mc::McSave>, String> {
    if id != "pcsx2" {
        return Err("memcard_not_supported".into());
    }
    let (source, ps2_titles) = {
        let s = state.lock().await;
        (db::get(&s.conn, &id)?.source_path, s.ps2.map.clone())
    };
    if source.is_empty() {
        return Err("config_incomplete_source".into());
    }
    let path = std::path::Path::new(&source).join(&raw_id);
    if !path.exists() {
        return Err("memcard_not_found".into());
    }
    let mut saves = ps2mc::list_saves(&path)?;
    for save in &mut saves {
        if let Some(serial) = &save.serial {
            if let Some(name) = ps2_titles.get(&serial.to_uppercase()) {
                save.title = Some(name.clone());
            }
        }
    }
    Ok(saves)
}

#[tauri::command]
async fn title_db_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.lock().await;
    let last_update = s
        .titles
        .last_update
        .map(|t| t.format("%d/%m/%Y %H:%M").to_string());
    Ok(serde_json::json!({
        "count": s.titles.map.len(),
        "last_update": last_update,
        "refreshing": s.title_db_refreshing,
        "cache_path": s.title_db_path.to_string_lossy(),
    }))
}

#[tauri::command]
async fn ps2_db_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.lock().await;
    let last_update = s
        .ps2
        .last_update
        .map(|t| t.format("%d/%m/%Y %H:%M").to_string());
    Ok(serde_json::json!({
        "count": s.ps2.map.len(),
        "last_update": last_update,
        "refreshing": s.ps2_db_refreshing,
        "cache_path": s.ps2_db_path.to_string_lossy(),
    }))
}

#[tauri::command]
async fn refresh_ps2_db(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let target = {
        let mut s = state.lock().await;
        if s.ps2_db_refreshing {
            return Err("db_already_refreshing".into());
        }
        s.ps2_db_refreshing = true;
        s.ps2_db_path.clone()
    };
    let _ = app.emit("ps2-db-status", "refreshing");

    let outcome = async {
        ps2db::download(&target).await?;
        let map = ps2db::parse(&target)?;
        Ok::<_, String>(map)
    }
    .await;

    let mut s = state.lock().await;
    s.ps2_db_refreshing = false;
    match outcome {
        Ok(map) => {
            s.ps2.map = std::sync::Arc::new(map);
            s.ps2.last_update = ps2db::cache_mtime(&s.ps2_db_path);
            let _ = app.emit("ps2-db-status", "ready");
            Ok(())
        }
        Err(e) => {
            let _ = app.emit("ps2-db-status", format!("error: {}", e));
            Err(e)
        }
    }
}

#[tauri::command]
async fn refresh_title_db(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let target = {
        let mut s = state.lock().await;
        if s.title_db_refreshing {
            return Err("db_already_refreshing".into());
        }
        s.title_db_refreshing = true;
        s.title_db_path.clone()
    };
    let _ = app.emit("title-db-status", "refreshing");

    let outcome = async {
        titledb::download(&target).await?;
        let map = titledb::parse(&target)?;
        Ok::<_, String>(map)
    }
    .await;

    let mut s = state.lock().await;
    s.title_db_refreshing = false;
    match outcome {
        Ok(map) => {
            s.titles.map = std::sync::Arc::new(map);
            s.titles.last_update = titledb::cache_mtime(&s.title_db_path);
            let _ = app.emit("title-db-status", "ready");
            Ok(())
        }
        Err(e) => {
            let _ = app.emit("title-db-status", format!("error: {}", e));
            Err(e)
        }
    }
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
            let title_db_path = titledb::cache_path(&app_data_dir);
            let ps2_db_path = ps2db::cache_path(&app_data_dir);

            app.manage(Mutex::new(AppData {
                conn,
                watchers: HashMap::new(),
                proc_watchers: HashMap::new(),
                titles: titledb::TitleDb::default(),
                title_db_path: title_db_path.clone(),
                title_db_refreshing: false,
                ps2: ps2db::Ps2Db::default(),
                ps2_db_path: ps2_db_path.clone(),
                ps2_db_refreshing: false,
            }));

            // Background: load existing cache (or download on first run), then
            // populate the in-memory map so eden saves get human names.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let needs_download = !title_db_path.exists();
                if needs_download {
                    let state = app_handle.state::<AppState>();
                    {
                        let mut s = state.lock().await;
                        s.title_db_refreshing = true;
                    }
                    let _ = app_handle.emit("title-db-status", "refreshing");
                    if let Err(e) = titledb::download(&title_db_path).await {
                        let _ = app_handle.emit(
                            "title-db-status",
                            format!("error: {}", e),
                        );
                        let mut s = state.lock().await;
                        s.title_db_refreshing = false;
                        return;
                    }
                }
                match titledb::parse(&title_db_path) {
                    Ok(map) => {
                        let state = app_handle.state::<AppState>();
                        let mut s = state.lock().await;
                        s.titles.map = std::sync::Arc::new(map);
                        s.titles.last_update = titledb::cache_mtime(&title_db_path);
                        s.title_db_refreshing = false;
                        let _ = app_handle.emit("title-db-status", "ready");
                    }
                    Err(e) => {
                        let _ = app_handle.emit(
                            "title-db-status",
                            format!("error: {}", e),
                        );
                        let state = app_handle.state::<AppState>();
                        let mut s = state.lock().await;
                        s.title_db_refreshing = false;
                        let _ = e;
                    }
                }
            });

            // Same lazy-load pattern for the PS2 database.
            let app_handle_ps2 = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let needs_download = !ps2_db_path.exists();
                if needs_download {
                    let state = app_handle_ps2.state::<AppState>();
                    {
                        let mut s = state.lock().await;
                        s.ps2_db_refreshing = true;
                    }
                    let _ = app_handle_ps2.emit("ps2-db-status", "refreshing");
                    if let Err(e) = ps2db::download(&ps2_db_path).await {
                        let _ = app_handle_ps2
                            .emit("ps2-db-status", format!("error: {}", e));
                        let mut s = state.lock().await;
                        s.ps2_db_refreshing = false;
                        return;
                    }
                }
                match ps2db::parse(&ps2_db_path) {
                    Ok(map) => {
                        let state = app_handle_ps2.state::<AppState>();
                        let mut s = state.lock().await;
                        s.ps2.map = std::sync::Arc::new(map);
                        s.ps2.last_update = ps2db::cache_mtime(&ps2_db_path);
                        s.ps2_db_refreshing = false;
                        let _ = app_handle_ps2.emit("ps2-db-status", "ready");
                    }
                    Err(e) => {
                        let _ = app_handle_ps2
                            .emit("ps2-db-status", format!("error: {}", e));
                        let state = app_handle_ps2.state::<AppState>();
                        let mut s = state.lock().await;
                        s.ps2_db_refreshing = false;
                    }
                }
            });

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
            fetch_cover_tint,
            get_save_entry,
            delete_save_entry,
            sync_one_save,
            open_save_folder,
            get_setting,
            set_setting,
            title_db_status,
            refresh_title_db,
            list_memcard_saves,
            ps2_db_status,
            refresh_ps2_db,
            rclone_version,
            rclone_list_remotes,
            rclone_create_s3_remote,
            rclone_delete_remote,
            rclone_get_remote,
            rclone_test_remote,
            get_history_settings,
            set_history_settings,
            supports_incremental_history,
            list_save_history,
            revert_save,
            list_conflicts,
            resolve_conflict,
            prune_history_now,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

