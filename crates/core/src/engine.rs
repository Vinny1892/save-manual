//! Motor de sincronização.
//!
//! `do_sync` é o coração: decide entre baseline (`--resync`) e bisync
//! incremental, tira o snapshot full antes de mexer, passa `--backupdir2`
//! pro delta, e fecha aplicando retenção. `do_sync_async` embrulha isso
//! em `spawn_blocking` (librclone é blocking) com um reporter de progresso
//! em paralelo.
//!
//! O progresso sai por `ProgressSink` em vez de ir direto pro `AppHandle`
//! do Tauri: o client emite evento pra UI, o server vai empurrar por SSE,
//! e nenhum dos dois precisa aparecer aqui.

use std::sync::Arc;

use chrono::Utc;

use crate::backend::Backend;
use crate::db::{Emulator, HistorySettings};
use crate::history::{auto_duplicate_file_conflicts, prune_history};
use crate::rclone;

/// Destino dos eventos de progresso de um sync em andamento.
///
/// `active: false` é o último evento de um run — é o sinal de que a UI
/// pode limpar o banner. `stats` vem do `core/stats` do rclone, que é
/// global ao processo: com dois syncs simultâneos os números vêm somados.
pub trait ProgressSink: Send + Sync + 'static {
    fn progress(&self, emulator_id: &str, active: bool, stats: Option<serde_json::Value>);
}

/// Sink que descarta tudo. Útil pra chamadas onde ninguém está olhando
/// (testes, sync disparado por script) sem precisar de um `Option` no
/// caminho todo.
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn progress(&self, _emulator_id: &str, _active: bool, _stats: Option<serde_json::Value>) {}
}

/// Per-emulator list of subtrees that participate in sync. Anything outside
/// these is ignored — Eden's NAND has gigabytes of system content we don't
/// want to mirror, so we whitelist only the save-bearing folders.
pub fn sync_subtrees(emu_id: &str) -> &'static [&'static str] {
    match emu_id {
        "eden" => &["system/save/8000000000000010", "user/save"],
        // pcsx2/rpcs3: bisync the entire source folder (memcards dir / dev_hdd0).
        _ => &[""],
    }
}

/// Outcome of a do_sync run.
pub struct SyncOutcome {
    /// True if this run was the first bisync for the pair (used `--resync`).
    pub initial: bool,
}

pub fn do_sync(
    emu: &Emulator,
    source: &std::path::Path,
    history: &HistorySettings,
) -> Result<SyncOutcome, String> {
    let backend = Backend::for_emulator(emu)?;
    backend.ensure_dir()?;

    if !history.bisync_initialized {
        return do_initial_bisync(emu, source, &backend).map(|()| SyncOutcome { initial: true });
    }

    let ts = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();

    // Full-mode snapshot, when enabled, is taken BEFORE bisync — captures
    // the entire live state about to be overwritten/merged. Independent of
    // incremental_enabled: both can be on, in which case `.history/<ts>/`
    // ends up with both `full/` and `delta/...` subdirs.
    let take_full = history.enabled && history.full_enabled;
    let track_delta = history.enabled && history.incremental_enabled;

    if take_full {
        backend.snapshot_full(&ts)?;
    }

    for sub in sync_subtrees(&emu.id) {
        let local = if sub.is_empty() {
            source.to_path_buf()
        } else {
            source.join(sub)
        };
        // Tolerate missing source subtrees (e.g. fresh install of an emulator
        // that hasn't created its save folder yet). Bisync against an empty
        // local dir works, but the dir must exist.
        std::fs::create_dir_all(&local).ok();

        let path1 = local.to_string_lossy().into_owned();
        let path2 = backend.live_fs_at(sub);
        let backupdir2 = if track_delta {
            Some(backend.snapshot_delta_fs_at(&ts, sub))
        } else {
            None
        };

        rclone::bisync(&rclone::BisyncOpts {
            path1: &path1,
            path2: &path2,
            backup_dir2: backupdir2.as_deref(),
            conflict_resolve: "newer",
            // Preserve the loser as `<path>.conflict1` instead of deleting
            // — conflict UI later surfaces these for the user to resolve.
            conflict_loser: "num",
            resync: false,
            resync_mode: "newer",
        })?;
    }

    // For file-based emulators (pcsx2), rename any `.conflictN` siblings
    // produced by this run so the emulator sees them as standalone
    // memcards. No-op for directory-based emulators.
    let _ = auto_duplicate_file_conflicts(emu, source, &backend);

    // Best-effort retention enforcement. Errors here don't fail the sync
    // — old snapshots lingering is annoying, but a failed sync is worse.
    let _ = prune_history(&backend, history);

    Ok(SyncOutcome { initial: false })
}

/// First-ever bisync for this pair. We pick `--resync-mode` automatically
/// based on which side already has data:
///   - only local has data    → "path1" (push to empty cloud)
///   - only remote has data   → "path2" (pull to empty PC — preserves cloud)
///   - both have data         → "newer" (per-file merge; conflicts surface
///                              on subsequent runs through conflict_resolve)
///   - neither has data       → error
pub fn do_initial_bisync(
    emu: &Emulator,
    source: &std::path::Path,
    backend: &Backend,
) -> Result<(), String> {
    let local_has = source
        .read_dir()
        .ok()
        .and_then(|mut d| d.next())
        .is_some();
    let remote_has = backend.live_has_data().unwrap_or(false);

    let resync_mode = match (local_has, remote_has) {
        (false, false) => return Err("initial_sync_both_empty".into()),
        (true, false) => "path1",
        (false, true) => "path2",
        (true, true) => "newer",
    };

    for sub in sync_subtrees(&emu.id) {
        let local = if sub.is_empty() {
            source.to_path_buf()
        } else {
            source.join(sub)
        };
        std::fs::create_dir_all(&local).ok();

        let path1 = local.to_string_lossy().into_owned();
        let path2 = backend.live_fs_at(sub);
        rclone::bisync(&rclone::BisyncOpts {
            path1: &path1,
            path2: &path2,
            backup_dir2: None, // first run never writes history
            conflict_resolve: "newer",
            conflict_loser: "num",
            resync: true,
            resync_mode,
        })?;
    }
    Ok(())
}

/// Async wrapper around `do_sync` that:
///   1. spawns a progress reporter task polling `core/stats` every 500ms
///      and pushing `{active: true, stats}` into the sink,
///   2. runs the blocking `do_sync` in a `spawn_blocking` thread so the
///      caller's async runtime stays responsive during long syncs,
///   3. pushes a final `active: false` so the UI banner clears.
///
/// Callers should prefer this over `do_sync` for any user-triggered
/// (sync_now) or background-task (watcher/proc-watch) sync.
pub async fn do_sync_async(
    emu: Emulator,
    source: std::path::PathBuf,
    history: HistorySettings,
    sink: Arc<dyn ProgressSink>,
) -> Result<SyncOutcome, String> {
    use tokio::sync::mpsc;
    use tokio::time::{interval, Duration};

    let id = emu.id.clone();

    // Progress reporter — runs until we send on stop_tx.
    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
    let progress_sink = Arc::clone(&sink);
    let progress_id = id.clone();
    let progress_task = tokio::spawn(async move {
        let mut tick = interval(Duration::from_millis(500));
        // First tick fires immediately — skip so we don't report before
        // rclone has anything to say.
        tick.tick().await;
        loop {
            tokio::select! {
                _ = stop_rx.recv() => break,
                _ = tick.tick() => {
                    let stats = rclone::core_stats().ok();
                    progress_sink.progress(&progress_id, true, stats);
                }
            }
        }
    });

    // Blocking bisync moved to a worker thread.
    let outcome = tokio::task::spawn_blocking(move || do_sync(&emu, &source, &history))
        .await
        .map_err(|e| e.to_string())?;

    // Stop reporter, then the final inactive event so the UI clears the banner.
    let _ = stop_tx.send(()).await;
    let _ = progress_task.await;
    sink.progress(&id, false, None);

    outcome
}

pub fn validate_config(emu: &Emulator) -> Result<(), String> {
    if emu.source_path.is_empty() {
        return Err("config_incomplete_source".into());
    }
    if emu.dest_path.is_empty() {
        return Err("config_incomplete_dest".into());
    }
    if emu.dest_kind == "rclone" && emu.dest_remote.is_empty() {
        return Err("config_incomplete_remote".into());
    }
    Ok(())
}

/// Compara nome do processo ignorando .exe e case.
/// Também aceita match parcial (contains) para package names Android.
pub fn proc_matches(proc_name: &std::ffi::OsStr, target: &str) -> bool {
    let proc = proc_name.to_string_lossy();
    let p = proc.trim_end_matches(".exe");
    let t = target.trim_end_matches(".exe");
    p.eq_ignore_ascii_case(t) || proc.contains(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emu(id: &str, source: &str, kind: &str, remote: &str, path: &str) -> Emulator {
        Emulator {
            id: id.into(),
            name: String::new(),
            hint: String::new(),
            source_path: source.into(),
            dest_kind: kind.into(),
            dest_remote: remote.into(),
            dest_path: path.into(),
            enabled: true,
            last_sync: None,
            last_error: None,
            process_name: String::new(),
        }
    }

    // ─── sync_subtrees ────────────────────────────────────────────────────

    #[test]
    fn sync_subtrees_eden_has_profile_and_user_save() {
        let subs = sync_subtrees("eden");
        assert_eq!(subs.len(), 2);
        assert!(subs.contains(&"system/save/8000000000000010"));
        assert!(subs.contains(&"user/save"));
    }

    #[test]
    fn sync_subtrees_pcsx2_is_single_empty_string() {
        // Empty string means "sync the source root directly" — pcsx2's
        // memcards folder IS the unit.
        assert_eq!(sync_subtrees("pcsx2"), &[""]);
    }

    #[test]
    fn sync_subtrees_rpcs3_is_single_root() {
        assert_eq!(sync_subtrees("rpcs3"), &[""]);
    }

    #[test]
    fn sync_subtrees_unknown_emu_defaults_to_root() {
        // Future emulators get the root-sync default rather than panicking.
        assert_eq!(sync_subtrees("duckstation"), &[""]);
    }

    // ─── validate_config ──────────────────────────────────────────────────

    #[test]
    fn validate_rejects_empty_source() {
        let e = emu("eden", "", "local", "", "/dest");
        assert!(validate_config(&e).is_err());
    }

    #[test]
    fn validate_rejects_empty_dest_path() {
        let e = emu("eden", "/src", "local", "", "");
        assert!(validate_config(&e).is_err());
    }

    #[test]
    fn validate_rejects_rclone_without_remote_name() {
        // dest_kind="rclone" + empty dest_remote is incoherent — must error
        // even when dest_path is provided.
        let e = emu("eden", "/src", "rclone", "", "bucket/path");
        assert!(validate_config(&e).is_err());
    }

    #[test]
    fn validate_accepts_local_complete() {
        let e = emu("eden", "/src", "local", "", "/dest");
        assert!(validate_config(&e).is_ok());
    }

    #[test]
    fn validate_accepts_rclone_complete() {
        let e = emu("eden", "/src", "rclone", "s3", "bucket/path");
        assert!(validate_config(&e).is_ok());
    }

    #[test]
    fn validate_local_ignores_empty_dest_remote() {
        // dest_remote is irrelevant when dest_kind == "local" — having a stale
        // value from a previous rclone config must not fail validation.
        let e = emu("eden", "/src", "local", "s3", "/dest");
        assert!(validate_config(&e).is_ok());
    }
}
