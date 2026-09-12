//! Histórico de backup e conflitos.
//!
//! Duas coisas que o bisync produz e alguém precisa administrar depois:
//! os snapshots em `.history/<ts>/{full,delta}/` e os arquivos
//! `.conflictN` que sobram quando os dois lados mudaram o mesmo arquivo.
//!
//! Vive no core porque tanto o client quanto o server precisam: o client
//! pra aplicar retenção depois de sincronizar, o server pra ser a
//! autoridade sobre o histórico que a UI lista e reverte.

use serde::Serialize;

use crate::backend::Backend;
use crate::db::{self, Emulator, HistorySettings};
use crate::rclone;

#[derive(Debug, Clone, Serialize)]
pub struct SaveHistoryEntry {
    pub timestamp: String,
    /// Whether this run produced a `full/` snapshot containing the save.
    pub has_full: bool,
    /// Whether this run produced a `delta/` entry for the save (i.e. the
    /// save was overwritten/deleted on that sync and rclone moved the
    /// previous version into --backupdir2).
    pub has_delta: bool,
    /// Sum of sizes of all files belonging to this save at this timestamp.
    /// Useful for the UI to show storage cost per version.
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictEntry {
    /// Path of the "current" winning version, relative to live root.
    pub path: String,
    /// Path of the preserved loser (always `<path>.conflict<n>`).
    pub conflict_path: String,
    /// 1-based numeric suffix — useful when a file accumulates several
    /// rounds of unresolved conflicts (`.conflict1`, `.conflict2`, ...).
    pub conflict_num: u32,
    pub current_size: u64,
    pub conflict_size: u64,
    /// rclone-style ISO mtime. Empty when the backend didn't supply one.
    pub current_modified: String,
    pub conflict_modified: String,
}

/// Strip a `.conflict<N>` suffix from a path. Returns `(original, n)`
/// or None when the path isn't a conflict marker.
pub fn strip_conflict_marker(path: &str) -> Option<(String, u32)> {
    let idx = path.rfind(".conflict")?;
    let n_part = &path[idx + ".conflict".len()..];
    if n_part.is_empty() {
        return None;
    }
    let n: u32 = n_part.parse().ok()?;
    let original = path[..idx].to_string();
    if original.is_empty() {
        return None;
    }
    Some((original, n))
}

/// Move the `.conflict<N>` suffix from "after extension" to "before
/// extension" so the resulting file no longer looks like an rclone
/// auto-generated marker. Used by the "keep both" resolution path.
///
/// `Mcd001.ps2.conflict1` → `Mcd001-conflict1.ps2`
/// `save.dat.conflict2`   → `save-conflict2.dat`
/// `weirdfile.conflict1`  → `weirdfile-conflict1` (no extension)
pub fn rename_keep_both_path(conflict_path: &str) -> String {
    let Some((original, n)) = strip_conflict_marker(conflict_path) else {
        return conflict_path.to_string();
    };
    let segment_start = original.rfind('/').map(|i| i + 1).unwrap_or(0);
    let basename = &original[segment_start..];
    if let Some(dot_rel) = basename.rfind('.') {
        let abs_dot = segment_start + dot_rel;
        format!(
            "{}-conflict{}.{}",
            &original[..abs_dot],
            n,
            &original[abs_dot + 1..]
        )
    } else {
        format!("{}-conflict{}", original, n)
    }
}

// ─── prune (retention enforcement) ──────────────────────────────────────
//
// After every successful sync we evaluate two rules against the per-emu
// history dir. Both are user-configurable via `history_settings`:
//
//   retention_days   — anything older than N days gets purged
//   retention_max_mb — if total still exceeds, oldest snapshots purged
//                      until size fits under the cap
//
// `<= 0` disables either rule. Pruning is best-effort: failures don't
// fail the sync (we already wrote new data, deleting old is gravy).

#[derive(Debug, Clone, Serialize)]
pub struct PruneSummary {
    pub deleted_count: usize,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    timestamp: String,
    size_bytes: u64,
}

/// Parses our snapshot timestamp format (`YYYY-MM-DDTHH-MM-SSZ` — hyphens
/// instead of colons in time so it's filesystem-safe on Windows). Returns
/// None on any malformation.
pub fn parse_snapshot_ts(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    let bytes = ts.as_bytes();
    if bytes.len() != 20 || !ts.ends_with('Z') || bytes[10] != b'T' {
        return None;
    }
    // Reassemble standard ISO8601 by swapping the hyphens at positions 13
    // and 16 back to colons.
    let iso = format!(
        "{}T{}:{}:{}",
        &ts[..10],
        &ts[11..13],
        &ts[14..16],
        &ts[17..19],
    );
    chrono::NaiveDateTime::parse_from_str(&iso, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|nd| chrono::Utc.from_utc_datetime(&nd))
}

/// Pure decision: which snapshot timestamps to delete given retention rules.
/// Returns empty Vec when nothing should go (or both rules disabled).
pub fn pick_snapshots_to_prune(
    snapshots: &[SnapshotInfo],
    retention_days: i64,
    retention_max_mb: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut to_delete: BTreeSet<String> = BTreeSet::new();

    // Rule 1: age cap. retention_days <= 0 means "no age limit".
    if retention_days > 0 {
        let cutoff = now - chrono::Duration::days(retention_days);
        for s in snapshots {
            if let Some(t) = parse_snapshot_ts(&s.timestamp) {
                if t < cutoff {
                    to_delete.insert(s.timestamp.clone());
                }
            }
        }
    }

    // Rule 2: size cap. retention_max_mb <= 0 means "no size limit".
    if retention_max_mb > 0 {
        let max_bytes = (retention_max_mb as u64).saturating_mul(1024 * 1024);
        // Survivors of rule 1, oldest first.
        let mut remaining: Vec<&SnapshotInfo> = snapshots
            .iter()
            .filter(|s| !to_delete.contains(&s.timestamp))
            .collect();
        remaining.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        let mut total: u64 = remaining.iter().map(|s| s.size_bytes).sum();
        let mut idx = 0;
        while total > max_bytes && idx < remaining.len() {
            let victim = remaining[idx];
            to_delete.insert(victim.timestamp.clone());
            total = total.saturating_sub(victim.size_bytes);
            idx += 1;
        }
    }

    to_delete.into_iter().collect()
}

/// One recursive listing, bucketed by top-level segment (= timestamp).
/// Used by both prune and the future "history size" surface.
pub fn list_snapshots(backend: &Backend) -> Result<Vec<SnapshotInfo>, String> {
    let history_root = backend.history_root_fs();
    let (fs, remote) = rclone::split_root(&history_root);
    let entries = rclone::list_recursive(&fs, &remote)?;

    use std::collections::HashMap;
    let mut by_ts: HashMap<String, u64> = HashMap::new();
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        let Some(slash) = entry.path.find('/') else { continue };
        let ts = entry.path[..slash].to_string();
        *by_ts.entry(ts).or_insert(0) += entry.size.max(0) as u64;
    }

    Ok(by_ts
        .into_iter()
        .map(|(timestamp, size_bytes)| SnapshotInfo { timestamp, size_bytes })
        .collect())
}

/// Apply retention to a backend's history dir. Returns a summary. Failure
/// to purge an individual snapshot doesn't abort the rest — best-effort.
pub fn prune_history(
    backend: &Backend,
    history: &HistorySettings,
) -> Result<PruneSummary, String> {
    if !history.enabled || (history.retention_days <= 0 && history.retention_max_mb <= 0) {
        return Ok(PruneSummary {
            deleted_count: 0,
            freed_bytes: 0,
        });
    }
    let snapshots = list_snapshots(backend)?;
    let now = chrono::Utc::now();
    let targets = pick_snapshots_to_prune(
        &snapshots,
        history.retention_days,
        history.retention_max_mb,
        now,
    );

    let mut deleted = 0usize;
    let mut freed = 0u64;
    for ts in &targets {
        let path = backend.snapshot_run_fs(ts);
        match rclone::purge_at(&path) {
            Ok(()) => {
                deleted += 1;
                if let Some(s) = snapshots.iter().find(|s| &s.timestamp == ts) {
                    freed = freed.saturating_add(s.size_bytes);
                }
            }
            Err(_) => {
                // Best-effort — skip and move on. Common cause: another
                // device already pruned this snapshot.
            }
        }
    }

    Ok(PruneSummary {
        deleted_count: deleted,
        freed_bytes: freed,
    })
}

/// For file-based emulators (pcsx2 today), conflicting memcards stay as
/// `<file>.conflict1` siblings of the winning version. PCSX2 wouldn't
/// pick those up as separate memcards (it only scans `.ps2`), so we
/// rename them to `<base>-conflict<N>.<ext>` automatically — the user
/// then sees both as selectable memcards in the emulator UI.
///
/// Runs on both live (cloud/local backup) and source (local PC). No-op
/// when emulator is directory-based (eden/rpcs3) — those use the
/// per-file conflict resolution UI instead.
pub fn auto_duplicate_file_conflicts(
    emu: &Emulator,
    source: &std::path::Path,
    backend: &Backend,
) -> Result<(), String> {
    if db::supports_incremental_history(&emu.id) {
        return Ok(());
    }

    let live = backend.live_fs();
    let (fs, remote) = rclone::split_root(&live);
    let entries = rclone::list_recursive(&fs, &remote)?;
    let source_root = source
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_string();

    for entry in entries {
        if entry.is_dir {
            continue;
        }
        let renamed = rename_keep_both_path(&entry.path);
        if renamed == entry.path {
            continue; // not a `.conflictN` marker
        }

        let live_src = format!("{live}/{}", entry.path);
        let live_dst = format!("{live}/{renamed}");
        let source_src = format!("{source_root}/{}", entry.path);
        let source_dst = format!("{source_root}/{renamed}");

        // Best-effort — failure on one side shouldn't block the rest.
        // Most common cause: the source-side rename already happened on
        // a previous tick (another device beat us to it).
        let _ = rclone::move_file_at(&live_src, &live_dst);
        let _ = rclone::move_file_at(&source_src, &source_dst);
    }

    Ok(())
}

/// Pairs each `<path>.conflict<n>` entry with its matching "current"
/// entry from the same listing. Orphan conflict files (no matching
/// current) are skipped — they shouldn't happen under normal bisync
/// flow but a previous failed resolve could leave one behind.
pub fn find_conflicts(entries: &[rclone::ListEntry]) -> Vec<ConflictEntry> {
    use std::collections::HashMap;
    let by_path: HashMap<&str, &rclone::ListEntry> =
        entries.iter().map(|e| (e.path.as_str(), e)).collect();

    let mut out: Vec<ConflictEntry> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter_map(|entry| {
            let (original, n) = strip_conflict_marker(&entry.path)?;
            let orig = by_path.get(original.as_str())?;
            // A directory can't be the "current" of a conflicted file —
            // ignore the pairing (the conflict file becomes orphaned and
            // gets skipped, matching the semantics of "no current to
            // resolve against").
            if orig.is_dir {
                return None;
            }
            Some(ConflictEntry {
                path: original,
                conflict_path: entry.path.clone(),
                conflict_num: n,
                current_size: orig.size.max(0) as u64,
                conflict_size: entry.size.max(0) as u64,
                current_modified: orig.mod_time.clone(),
                conflict_modified: entry.mod_time.clone(),
            })
        })
        .collect();
    // Path first (groups related conflicts together), then conflict_num
    // (so `.conflict1` shows before `.conflict2` for the same file).
    out.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.conflict_num.cmp(&b.conflict_num))
    });
    out
}

/// Pure aggregation pass — splits each history-relative entry path into
/// `<ts>/<mode>/<path_in_mode>`, filters for entries matching `sub_path`
/// (exact, or a child below it — trailing-slash check guards against
/// `Mcd001.ps2` vs `Mcd001b.ps2` style false prefixes), and accumulates
/// per-timestamp size + mode flags. Extracted from `list_save_history` so
/// tests can exercise it without librclone.
pub fn group_history_entries(
    entries: &[rclone::ListEntry],
    sub_path: &str,
) -> Vec<SaveHistoryEntry> {
    use std::collections::BTreeMap;
    let mut by_ts: BTreeMap<String, SaveHistoryEntry> = BTreeMap::new();

    for entry in entries {
        let parts: Vec<&str> = entry.path.splitn(3, '/').collect();
        if parts.len() < 3 {
            continue;
        }
        let ts = parts[0];
        let mode = parts[1];
        let path_in_mode = parts[2];

        if mode != "full" && mode != "delta" {
            continue;
        }

        if path_in_mode != sub_path {
            if !path_in_mode.starts_with(sub_path) {
                continue;
            }
            let after = &path_in_mode[sub_path.len()..];
            if !after.starts_with('/') {
                continue;
            }
        }

        let bucket = by_ts
            .entry(ts.to_string())
            .or_insert_with(|| SaveHistoryEntry {
                timestamp: ts.to_string(),
                has_full: false,
                has_delta: false,
                size_bytes: 0,
            });
        if mode == "full" {
            bucket.has_full = true;
        }
        if mode == "delta" {
            bucket.has_delta = true;
        }
        if !entry.is_dir {
            bucket.size_bytes += entry.size.max(0) as u64;
        }
    }

    let mut out: Vec<SaveHistoryEntry> = by_ts.into_values().collect();
    // Reverse chronological — newest first matches the "1, 2, 3 days ago"
    // intuition of revert-to-N-days.
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rclone;

    // ─── group_history_entries ────────────────────────────────────────────

    fn entry(path: &str, size: i64, is_dir: bool) -> rclone::ListEntry {
        rclone::ListEntry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or("").into(),
            size,
            mod_time: String::new(),
            is_dir,
        }
    }

    #[test]
    fn group_history_buckets_by_timestamp() {
        let entries = vec![
            entry("2026-05-08T19-45-12Z/full/Mcd001.ps2", 8_388_608, false),
            entry("2026-05-09T14-30-00Z/full/Mcd001.ps2", 8_388_608, false),
        ];
        let out = group_history_entries(&entries, "Mcd001.ps2");
        assert_eq!(out.len(), 2);
        // Reverse chronological — newest first.
        assert_eq!(out[0].timestamp, "2026-05-09T14-30-00Z");
        assert_eq!(out[1].timestamp, "2026-05-08T19-45-12Z");
        assert!(out[0].has_full);
        assert!(!out[0].has_delta);
    }

    #[test]
    fn group_history_combines_full_and_delta_in_same_run() {
        // When both modes are on for a single sync, the timestamp dir has
        // both `full/` and `delta/` subtrees — should fold into one entry.
        let entries = vec![
            entry("2026-05-09T14-30-00Z/full/user/save/uuid/titleA/file", 100, false),
            entry("2026-05-09T14-30-00Z/delta/user/save/uuid/titleA/file", 50, false),
        ];
        let out = group_history_entries(&entries, "user/save/uuid/titleA");
        assert_eq!(out.len(), 1);
        assert!(out[0].has_full);
        assert!(out[0].has_delta);
        assert_eq!(out[0].size_bytes, 150);
    }

    #[test]
    fn group_history_filters_by_sub_path_prefix() {
        // Listings include sibling saves' entries — must not leak into our
        // result. titleA's listing should ignore titleB and titleAA.
        let entries = vec![
            entry("2026-05-09T14-30-00Z/full/user/save/uuid/titleA/file1", 100, false),
            entry("2026-05-09T14-30-00Z/full/user/save/uuid/titleB/file1", 200, false),
            entry("2026-05-09T14-30-00Z/full/user/save/uuid/titleAA/file1", 400, false),
        ];
        let out = group_history_entries(&entries, "user/save/uuid/titleA");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size_bytes, 100); // titleB + titleAA excluded
    }

    #[test]
    fn group_history_handles_exact_file_match_pcsx2_style() {
        // pcsx2's sub_path is just "Mcd001.ps2" — the entry path equals it
        // (no trailing slash). Earlier prefix check would mis-match
        // "Mcd001.ps2.bak" if not careful. Trailing-slash check guards.
        let entries = vec![
            entry("2026-05-09T14-30-00Z/full/Mcd001.ps2", 8_388_608, false),
            entry("2026-05-09T14-30-00Z/full/Mcd001.ps2.bak", 8_388_608, false),
            entry("2026-05-09T14-30-00Z/full/Mcd0011.ps2", 8_388_608, false),
        ];
        let out = group_history_entries(&entries, "Mcd001.ps2");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size_bytes, 8_388_608);
    }

    #[test]
    fn group_history_sums_sizes_of_files_only() {
        // Directory entries (`IsDir: true`) shouldn't contribute to size.
        let entries = vec![
            entry("2026-05-09T14-30-00Z/full/user/save/uuid/titleA", 0, true),
            entry("2026-05-09T14-30-00Z/full/user/save/uuid/titleA/file1", 100, false),
            entry("2026-05-09T14-30-00Z/full/user/save/uuid/titleA/file2", 50, false),
        ];
        let out = group_history_entries(&entries, "user/save/uuid/titleA");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size_bytes, 150);
    }

    #[test]
    fn group_history_empty_input_returns_empty() {
        assert!(group_history_entries(&[], "anything").is_empty());
    }

    #[test]
    fn group_history_ignores_unknown_mode_subdirs() {
        // Future variant or stray data under .history/<ts>/<x>/ shouldn't
        // accidentally count.
        let entries = vec![
            entry("2026-05-09T14-30-00Z/snapshot/Mcd001.ps2", 100, false),
            entry("2026-05-09T14-30-00Z/full/Mcd001.ps2", 50, false),
        ];
        let out = group_history_entries(&entries, "Mcd001.ps2");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size_bytes, 50);
    }

    // ─── strip_conflict_marker / rename_keep_both_path ────────────────────

    #[test]
    fn strip_conflict_marker_basic() {
        assert_eq!(
            strip_conflict_marker("Mcd001.ps2.conflict1"),
            Some(("Mcd001.ps2".to_string(), 1))
        );
        assert_eq!(
            strip_conflict_marker("path/to/save.dat.conflict42"),
            Some(("path/to/save.dat".to_string(), 42))
        );
    }

    #[test]
    fn strip_conflict_marker_rejects_non_marker() {
        // No suffix at all
        assert_eq!(strip_conflict_marker("Mcd001.ps2"), None);
        // Suffix without numeric tail
        assert_eq!(strip_conflict_marker("foo.conflict"), None);
        // Numeric tail but no .conflict prefix
        assert_eq!(strip_conflict_marker("foo.bak1"), None);
        // Non-numeric tail
        assert_eq!(strip_conflict_marker("foo.conflictX"), None);
    }

    #[test]
    fn strip_conflict_marker_rejects_orphan_with_empty_original() {
        // `.conflict1` at root with nothing before — bogus, would
        // try to look up "" as the original. Skip.
        assert_eq!(strip_conflict_marker(".conflict1"), None);
    }

    #[test]
    fn rename_keep_both_moves_marker_before_extension() {
        assert_eq!(
            rename_keep_both_path("Mcd001.ps2.conflict1"),
            "Mcd001-conflict1.ps2"
        );
        assert_eq!(
            rename_keep_both_path("user/save/uuid/title-id/save.dat.conflict2"),
            "user/save/uuid/title-id/save-conflict2.dat"
        );
    }

    #[test]
    fn rename_keep_both_no_extension_appends() {
        assert_eq!(
            rename_keep_both_path("README.conflict1"),
            "README-conflict1"
        );
        assert_eq!(
            rename_keep_both_path("path/to/README.conflict3"),
            "path/to/README-conflict3"
        );
    }

    #[test]
    fn rename_keep_both_passthrough_when_not_a_marker() {
        // If the path doesn't look like a conflict marker, we return it
        // unchanged. Defensive — should never be called this way in
        // practice but the helper shouldn't corrupt input.
        assert_eq!(rename_keep_both_path("Mcd001.ps2"), "Mcd001.ps2");
    }

    #[test]
    fn rename_keep_both_extension_in_dirname_only() {
        // A dot in a parent directory shouldn't be treated as extension.
        // e.g. "1.0.0/save.conflict1" → "1.0.0/save-conflict1" (no ext)
        assert_eq!(
            rename_keep_both_path("1.0.0/save.conflict1"),
            "1.0.0/save-conflict1"
        );
    }

    // ─── find_conflicts ────────────────────────────────────────────────────

    fn list_entry(path: &str, size: i64, mod_time: &str) -> rclone::ListEntry {
        rclone::ListEntry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or("").into(),
            size,
            mod_time: mod_time.into(),
            is_dir: false,
        }
    }

    #[test]
    fn find_conflicts_pairs_current_with_loser() {
        let entries = vec![
            list_entry("Mcd001.ps2", 8_388_608, "2026-05-09T14:30:00Z"),
            list_entry("Mcd001.ps2.conflict1", 8_388_608, "2026-05-08T20:00:00Z"),
        ];
        let out = find_conflicts(&entries);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "Mcd001.ps2");
        assert_eq!(out[0].conflict_path, "Mcd001.ps2.conflict1");
        assert_eq!(out[0].conflict_num, 1);
        assert_eq!(out[0].current_size, 8_388_608);
        assert_eq!(out[0].conflict_size, 8_388_608);
        assert_eq!(out[0].current_modified, "2026-05-09T14:30:00Z");
    }

    #[test]
    fn find_conflicts_handles_multi_conflict_chain() {
        // When a file has had multiple rounds of unresolved conflict,
        // .conflict1 AND .conflict2 coexist alongside the current.
        let entries = vec![
            list_entry("save.dat", 100, ""),
            list_entry("save.dat.conflict1", 90, ""),
            list_entry("save.dat.conflict2", 110, ""),
        ];
        let out = find_conflicts(&entries);
        assert_eq!(out.len(), 2);
        // Sorted by (path, conflict_num)
        assert_eq!(out[0].conflict_num, 1);
        assert_eq!(out[1].conflict_num, 2);
    }

    #[test]
    fn find_conflicts_skips_orphan_conflict_files() {
        // .conflict1 exists but the original was deleted — orphan. Should
        // be ignored (will require manual cleanup or a future "orphans"
        // surface in the UI).
        let entries = vec![list_entry("save.dat.conflict1", 90, "")];
        assert!(find_conflicts(&entries).is_empty());
    }

    #[test]
    fn find_conflicts_ignores_dir_entries() {
        let dir = rclone::ListEntry {
            path: "save.dat".into(),
            name: "save.dat".into(),
            size: 0,
            mod_time: String::new(),
            is_dir: true,
        };
        let entries = vec![dir, list_entry("save.dat.conflict1", 90, "")];
        // current is a dir entry — treat as no current, conflict is orphaned
        // and skipped.
        assert!(find_conflicts(&entries).is_empty());
    }

    #[test]
    fn find_conflicts_empty_input_returns_empty() {
        assert!(find_conflicts(&[]).is_empty());
    }

    // ─── parse_snapshot_ts ────────────────────────────────────────────────

    #[test]
    fn parse_snapshot_ts_valid() {
        let parsed = parse_snapshot_ts("2026-05-09T14-30-00Z").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-05-09T14:30:00+00:00");
    }

    #[test]
    fn parse_snapshot_ts_rejects_wrong_length() {
        assert!(parse_snapshot_ts("2026-05-09").is_none());
        assert!(parse_snapshot_ts("2026-05-09T14-30-00").is_none()); // missing Z
        assert!(parse_snapshot_ts("2026-05-09T14-30-00Z-extra").is_none());
    }

    #[test]
    fn parse_snapshot_ts_rejects_missing_t_separator() {
        // T at position 10 — anything else is invalid
        assert!(parse_snapshot_ts("2026-05-09-14-30-00Z").is_none());
    }

    #[test]
    fn parse_snapshot_ts_rejects_garbage() {
        assert!(parse_snapshot_ts("hello-world-foo-baz-Z").is_none());
        assert!(parse_snapshot_ts("").is_none());
    }

    // ─── pick_snapshots_to_prune ──────────────────────────────────────────

    fn snap(ts: &str, size_mb: u64) -> SnapshotInfo {
        SnapshotInfo {
            timestamp: ts.into(),
            size_bytes: size_mb * 1024 * 1024,
        }
    }

    fn at(ts: &str) -> chrono::DateTime<chrono::Utc> {
        parse_snapshot_ts(ts).unwrap()
    }

    #[test]
    fn pick_prune_age_rule_alone() {
        let snaps = vec![
            snap("2026-05-01T00-00-00Z", 100), // 8 days old @ now=05-09
            snap("2026-05-07T00-00-00Z", 100), // 2 days old
            snap("2026-05-09T00-00-00Z", 100), // 0 days old
        ];
        let now = at("2026-05-09T00-00-00Z");
        let to_del = pick_snapshots_to_prune(&snaps, 7, 0, now); // 7-day cap, no size cap
        assert_eq!(to_del, vec!["2026-05-01T00-00-00Z".to_string()]);
    }

    #[test]
    fn pick_prune_size_rule_alone_oldest_first() {
        // 3 snaps × 100 MB each = 300 MB. Cap = 250 MB → drop oldest.
        let snaps = vec![
            snap("2026-05-07T00-00-00Z", 100),
            snap("2026-05-08T00-00-00Z", 100),
            snap("2026-05-09T00-00-00Z", 100),
        ];
        let now = at("2026-05-09T00-00-00Z");
        let to_del = pick_snapshots_to_prune(&snaps, 0, 250, now);
        assert_eq!(to_del, vec!["2026-05-07T00-00-00Z".to_string()]);
    }

    #[test]
    fn pick_prune_size_rule_drops_multiple_oldest() {
        let snaps = vec![
            snap("2026-05-05T00-00-00Z", 100),
            snap("2026-05-06T00-00-00Z", 100),
            snap("2026-05-07T00-00-00Z", 100),
            snap("2026-05-08T00-00-00Z", 100),
            snap("2026-05-09T00-00-00Z", 100),
        ];
        let now = at("2026-05-09T00-00-00Z");
        // 500 MB total, cap 250 → drop 3 oldest to leave 200.
        let to_del = pick_snapshots_to_prune(&snaps, 0, 250, now);
        assert_eq!(to_del.len(), 3);
        assert!(to_del.contains(&"2026-05-05T00-00-00Z".to_string()));
        assert!(to_del.contains(&"2026-05-06T00-00-00Z".to_string()));
        assert!(to_del.contains(&"2026-05-07T00-00-00Z".to_string()));
    }

    #[test]
    fn pick_prune_both_rules_age_first_then_size() {
        let snaps = vec![
            snap("2026-04-01T00-00-00Z", 100), // 38 days — caught by age (30d)
            snap("2026-05-01T00-00-00Z", 100), // 8 days — survives age
            snap("2026-05-08T00-00-00Z", 100), // 1 day — survives age
            snap("2026-05-09T00-00-00Z", 100), // 0 days — survives age
        ];
        let now = at("2026-05-09T00-00-00Z");
        // Cap 250 MB. After age rule: 300 MB left → drop oldest of survivors.
        let to_del = pick_snapshots_to_prune(&snaps, 30, 250, now);
        assert_eq!(to_del.len(), 2);
        assert!(to_del.contains(&"2026-04-01T00-00-00Z".to_string())); // age
        assert!(to_del.contains(&"2026-05-01T00-00-00Z".to_string())); // size (oldest survivor)
    }

    #[test]
    fn pick_prune_disabled_when_both_rules_nonpositive() {
        let snaps = vec![
            snap("2025-01-01T00-00-00Z", 99999), // ancient AND huge
        ];
        let now = at("2026-05-09T00-00-00Z");
        assert!(pick_snapshots_to_prune(&snaps, 0, 0, now).is_empty());
        assert!(pick_snapshots_to_prune(&snaps, -5, -5, now).is_empty());
    }

    #[test]
    fn pick_prune_under_size_cap_keeps_all() {
        let snaps = vec![snap("2026-05-09T00-00-00Z", 50)];
        let now = at("2026-05-09T00-00-00Z");
        assert!(pick_snapshots_to_prune(&snaps, 30, 500, now).is_empty());
    }

    #[test]
    fn pick_prune_under_age_cap_keeps_all() {
        let snaps = vec![
            snap("2026-05-08T00-00-00Z", 10),
            snap("2026-05-09T00-00-00Z", 10),
        ];
        let now = at("2026-05-09T00-00-00Z");
        assert!(pick_snapshots_to_prune(&snaps, 30, 0, now).is_empty());
    }

    #[test]
    fn pick_prune_skips_unparseable_timestamps() {
        // Stray non-conformant entry shouldn't get pruned (defensive — could
        // be user data accidentally under .history). Size rule may still
        // count it; that's debatable but matches the "safer to keep" intent.
        let snaps = vec![
            snap("garbage-folder", 10),
            snap("2026-05-01T00-00-00Z", 10), // 8 days old, age cap 7
        ];
        let now = at("2026-05-09T00-00-00Z");
        let to_del = pick_snapshots_to_prune(&snaps, 7, 0, now);
        assert!(!to_del.contains(&"garbage-folder".to_string()));
        assert!(to_del.contains(&"2026-05-01T00-00-00Z".to_string()));
    }
}
