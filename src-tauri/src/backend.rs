//! Sync destination abstraction. Two concrete kinds: `Local` (filesystem
//! via `fs_extra`) and `Rclone` (any rclone-supported remote, called via
//! the librclone in-process RPC). The frontend chooses one per emulator
//! through `dest_kind` in the `emulators` table.
//!
//! Modeled as an enum (not a `dyn Trait`) because the set of backends is
//! closed and the call sites are tiny — pattern-matching keeps the dispatch
//! visible and avoids a Box allocation per sync.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::db::Emulator;
use crate::rclone;

#[derive(Debug, Clone)]
pub enum Backend {
    Local { root: PathBuf },
    Rclone { remote: String, path: String },
}

impl Backend {
    /// Resolve the backend for an emulator's configured destination. The
    /// returned backend is rooted at `<dest>/<emu_id>` so each emulator gets
    /// its own subtree (mirrors the legacy local-only behavior).
    pub fn for_emulator(emu: &Emulator) -> Result<Backend, String> {
        if emu.dest_path.is_empty() {
            return Err("destino não configurado".into());
        }
        match emu.dest_kind.as_str() {
            "" | "local" => Ok(Backend::Local {
                root: PathBuf::from(&emu.dest_path).join(&emu.id),
            }),
            "rclone" => {
                if emu.dest_remote.is_empty() {
                    return Err("rclone remote não configurado".into());
                }
                Ok(Backend::Rclone {
                    remote: emu.dest_remote.clone(),
                    path: join_rclone_path(&emu.dest_path, &emu.id),
                })
            }
            other => Err(format!("dest_kind desconhecido: {other}")),
        }
    }

    /// Returns a backend rooted at `<self_root>/<segment>`. Used to descend
    /// into per-save subpaths (e.g. `user/save/<uuid>`).
    pub fn child(&self, segment: &str) -> Backend {
        match self {
            Backend::Local { root } => Backend::Local {
                root: root.join(segment),
            },
            Backend::Rclone { remote, path } => Backend::Rclone {
                remote: remote.clone(),
                path: join_rclone_path(path, segment),
            },
        }
    }

    /// Ensure the destination directory exists. No-op for rclone — `sync/copy`
    /// creates intermediate directories on demand and remote object stores
    /// (S3 et al) don't have real directories anyway.
    pub fn ensure_dir(&self) -> Result<(), String> {
        match self {
            Backend::Local { root } => {
                std::fs::create_dir_all(root).map_err(|e| e.to_string())
            }
            Backend::Rclone { .. } => Ok(()),
        }
    }

    /// Mirror the contents of `src` (a local directory) into the backend root.
    /// Existing files in dst are overwritten; extra files are NOT removed
    /// (fs_extra::dir::copy with overwrite=true; rclone `sync/copy` is the
    /// non-deleting variant of `sync/sync`). A delete-extra mode can be added
    /// later if the user opts in.
    pub fn copy_dir_contents(&self, src: &Path) -> Result<(), String> {
        if !src.exists() {
            return Err(format!("origem não encontrada: {}", src.display()));
        }
        match self {
            Backend::Local { root } => {
                std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
                let opts = fs_extra::dir::CopyOptions {
                    overwrite: true,
                    copy_inside: true,
                    ..Default::default()
                };
                fs_extra::dir::copy(src, root, &opts)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            Backend::Rclone { remote, path } => {
                rclone::rpc_json(
                    "sync/copy",
                    json!({
                        "srcFs": src.to_string_lossy(),
                        "dstFs": format!("{remote}:{path}"),
                        "createEmptySrcDirs": true,
                    }),
                )
                .map(|_| ())
            }
        }
    }

    /// Copy a single file into the backend, preserving its basename.
    pub fn copy_file(&self, src: &Path) -> Result<(), String> {
        if !src.is_file() {
            return Err(format!("arquivo não encontrado: {}", src.display()));
        }
        let basename = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "nome de arquivo inválido".to_string())?;
        match self {
            Backend::Local { root } => {
                std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
                std::fs::copy(src, root.join(basename))
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            Backend::Rclone { remote, path } => {
                let src_dir = src
                    .parent()
                    .ok_or_else(|| "arquivo sem diretório pai".to_string())?;
                rclone::rpc_json(
                    "operations/copyfile",
                    json!({
                        "srcFs": src_dir.to_string_lossy(),
                        "srcRemote": basename,
                        "dstFs": format!("{remote}:{path}"),
                        "dstRemote": basename,
                    }),
                )
                .map(|_| ())
            }
        }
    }
}

/// rclone path component joiner — always uses POSIX `/` (rclone normalizes
/// these on every backend, including local paths on Windows).
fn join_rclone_path(base: &str, segment: &str) -> String {
    let base = base.trim_end_matches('/');
    let segment = segment.trim_start_matches('/');
    if base.is_empty() {
        segment.to_string()
    } else {
        format!("{base}/{segment}")
    }
}

// ─── history / bisync path computation ────────────────────────────────────
//
// History sits one level ABOVE the live subtree (sibling of `<emu_id>/`),
// not nested inside it — otherwise bisync would try to mirror history
// itself back to the source. Concretely:
//
//   live_root      = <base>/<emu_id>
//   history_root   = <base>/.history/<emu_id>
//   snapshot(ts)   = <base>/.history/<emu_id>/<ts>
//
// For Local backends `<base>` is the dest folder; for Rclone it's the
// dest path inside the remote (typically `bucket/prefix`).

impl Backend {
    /// rclone-style fs string ("remote:path" or absolute local path) of the
    /// live root. Used as `path2` in bisync and `dstFs` in copies.
    pub fn live_fs(&self) -> String {
        match self {
            Backend::Local { root } => root.to_string_lossy().into_owned(),
            Backend::Rclone { remote, path } => format!("{remote}:{path}"),
        }
    }

    /// `live_fs()` + `/sub`. Empty `sub` is identity. Used to bisync a
    /// specific subtree of an emulator that doesn't sync its full source
    /// (eden's NAND has gigs of stuff we don't want to mirror).
    pub fn live_fs_at(&self, sub: &str) -> String {
        let base = self.live_fs();
        if sub.is_empty() {
            return base;
        }
        match self {
            Backend::Local { .. } => std::path::Path::new(&base)
                .join(sub)
                .to_string_lossy()
                .into_owned(),
            Backend::Rclone { .. } => format!("{base}/{sub}"),
        }
    }


    /// `<base>/.history/<emu_id>` — parent of all snapshot timestamps.
    pub fn history_root_fs(&self) -> String {
        let (parent, emu_id) = self.split_root();
        match self {
            Backend::Local { .. } => {
                let p = std::path::Path::new(&parent)
                    .join(".history")
                    .join(&emu_id);
                p.to_string_lossy().into_owned()
            }
            Backend::Rclone { remote, .. } => {
                let stem = if parent.is_empty() {
                    format!(".history/{emu_id}")
                } else {
                    format!("{parent}/.history/{emu_id}")
                };
                format!("{remote}:{stem}")
            }
        }
    }

    /// `<base>/.history/<emu_id>/<ts>` — the timestamped run dir.
    /// Inside it, `full/` and `delta/<sub>/` are populated according to
    /// which history modes are active for this run. Subdirs keep the two
    /// from colliding when both modes are on simultaneously.
    pub fn snapshot_run_fs(&self, ts: &str) -> String {
        let root = self.history_root_fs();
        match self {
            Backend::Local { .. } => std::path::Path::new(&root)
                .join(ts)
                .to_string_lossy()
                .into_owned(),
            Backend::Rclone { .. } => format!("{root}/{ts}"),
        }
    }

    /// `<run>/full` — destination for the full-mode pre-sync snapshot of
    /// the entire live root.
    pub fn snapshot_full_fs(&self, ts: &str) -> String {
        let run = self.snapshot_run_fs(ts);
        match self {
            Backend::Local { .. } => std::path::Path::new(&run)
                .join("full")
                .to_string_lossy()
                .into_owned(),
            Backend::Rclone { .. } => format!("{run}/full"),
        }
    }

    /// `<run>/delta[/<sub>]` — destination for rclone's `--backupdir2`.
    /// `sub` matches the bisync subtree being run so the structure under
    /// `delta/` mirrors the live layout (eden gets `delta/user/save/...`
    /// vs `delta/system/save/...` instead of colliding at the root).
    pub fn snapshot_delta_fs_at(&self, ts: &str, sub: &str) -> String {
        let run = self.snapshot_run_fs(ts);
        let delta = match self {
            Backend::Local { .. } => std::path::Path::new(&run)
                .join("delta")
                .to_string_lossy()
                .into_owned(),
            Backend::Rclone { .. } => format!("{run}/delta"),
        };
        if sub.is_empty() {
            return delta;
        }
        match self {
            Backend::Local { .. } => std::path::Path::new(&delta)
                .join(sub)
                .to_string_lossy()
                .into_owned(),
            Backend::Rclone { .. } => format!("{delta}/{sub}"),
        }
    }

    /// Splits the live root into (parent, last_segment). For Local it's
    /// PathBuf parent + file_name; for Rclone it's a POSIX rsplit of `path`.
    fn split_root(&self) -> (String, String) {
        match self {
            Backend::Local { root } => {
                let parent = root
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name = root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (parent, name)
            }
            Backend::Rclone { path, .. } => {
                if let Some((p, n)) = path.rsplit_once('/') {
                    (p.to_string(), n.to_string())
                } else {
                    (String::new(), path.clone())
                }
            }
        }
    }

    /// Whether the live root currently has any data — used to pick the
    /// `resync_mode` on the very first bisync of a pair.
    pub fn live_has_data(&self) -> Result<bool, String> {
        match self {
            Backend::Local { root } => match std::fs::read_dir(root) {
                Ok(mut it) => Ok(it.next().is_some()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e.to_string()),
            },
            Backend::Rclone { remote, path } => {
                rclone::has_entries(&format!("{remote}:"), path)
            }
        }
    }

    /// Take a full snapshot of the live root into `<.history>/<ts>`. Server-
    /// side copy on cloud backends, plain file copy locally — both go
    /// through rclone so the call site doesn't branch.
    pub fn snapshot_full(&self, ts: &str) -> Result<(), String> {
        let src = self.live_fs();
        let dst = self.snapshot_full_fs(ts);
        rclone::copy_fs(&src, &dst)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn local_emu(id: &str, dest: &str) -> Emulator {
        Emulator {
            id: id.into(),
            name: String::new(),
            hint: String::new(),
            source_path: "/src".into(),
            dest_kind: "local".into(),
            dest_remote: String::new(),
            dest_path: dest.into(),
            enabled: true,
            last_sync: None,
            last_error: None,
            process_name: String::new(),
        }
    }

    fn rclone_emu(id: &str, remote: &str, path: &str) -> Emulator {
        Emulator {
            id: id.into(),
            name: String::new(),
            hint: String::new(),
            source_path: "/src".into(),
            dest_kind: "rclone".into(),
            dest_remote: remote.into(),
            dest_path: path.into(),
            enabled: true,
            last_sync: None,
            last_error: None,
            process_name: String::new(),
        }
    }

    // ─── for_emulator ────────────────────────────────────────────────────

    #[test]
    fn for_emulator_local_appends_id_to_root() {
        let emu = local_emu("eden", "/backup");
        match Backend::for_emulator(&emu).unwrap() {
            Backend::Local { root } => {
                assert_eq!(root, PathBuf::from("/backup").join("eden"));
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn for_emulator_empty_dest_kind_defaults_to_local() {
        let mut emu = local_emu("eden", "/backup");
        emu.dest_kind = String::new();
        // Empty dest_kind is normalized to "local" — should succeed.
        assert!(matches!(
            Backend::for_emulator(&emu).unwrap(),
            Backend::Local { .. }
        ));
    }

    #[test]
    fn for_emulator_rclone_joins_path_posix() {
        let emu = rclone_emu("rpcs3", "s3", "mybucket/saves");
        match Backend::for_emulator(&emu).unwrap() {
            Backend::Rclone { remote, path } => {
                assert_eq!(remote, "s3");
                assert_eq!(path, "mybucket/saves/rpcs3");
            }
            _ => panic!("expected Rclone"),
        }
    }

    #[test]
    fn for_emulator_rclone_empty_dest_path_with_bucket_root() {
        // Some configs use bucket-as-root (dest_path = ""): then path = emu_id.
        let mut emu = rclone_emu("eden", "s3", "");
        // dest_path == "" is rejected (no destination configured at all)
        assert!(Backend::for_emulator(&emu).is_err());
        // But "/" or actual path is fine:
        emu.dest_path = "bucket".into();
        match Backend::for_emulator(&emu).unwrap() {
            Backend::Rclone { path, .. } => assert_eq!(path, "bucket/eden"),
            _ => panic!(),
        }
    }

    #[test]
    fn for_emulator_rclone_without_remote_errors() {
        let emu = rclone_emu("eden", "", "bucket");
        assert!(Backend::for_emulator(&emu).is_err());
    }

    #[test]
    fn for_emulator_unknown_dest_kind_errors() {
        let mut emu = local_emu("eden", "/backup");
        emu.dest_kind = "ftp".into();
        assert!(Backend::for_emulator(&emu).is_err());
    }

    // ─── live_fs / live_fs_at ────────────────────────────────────────────

    #[test]
    fn rclone_live_fs_format() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(b.live_fs(), "s3:bucket/saves/eden");
    }

    #[test]
    fn rclone_live_fs_at_empty_sub_is_identity() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(b.live_fs_at(""), "s3:bucket/saves/eden");
    }

    #[test]
    fn rclone_live_fs_at_appends_subpath() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(
            b.live_fs_at("user/save"),
            "s3:bucket/saves/eden/user/save"
        );
    }

    // ─── history_root_fs (the "sibling, not nested" invariant) ───────────

    #[test]
    fn rclone_history_root_is_sibling_of_live() {
        // base = "bucket/saves", emu_id = "eden"
        // live_root    = bucket/saves/eden
        // history_root MUST be bucket/saves/.history/eden, NOT bucket/saves/eden/.history.
        // Otherwise bisync would mirror history back to source and corrupt everything.
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(b.history_root_fs(), "s3:bucket/saves/.history/eden");
    }

    #[test]
    fn rclone_history_root_with_emu_at_top_level() {
        // dest_path = "" → path = "eden" → no parent. history goes to ".history/eden".
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "eden".into(),
        };
        assert_eq!(b.history_root_fs(), "s3:.history/eden");
    }

    // ─── snapshot paths: run / full / delta ──────────────────────────────

    #[test]
    fn rclone_snapshot_run_fs_is_timestamp_dir() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(
            b.snapshot_run_fs("20260509T143000Z"),
            "s3:bucket/saves/.history/eden/20260509T143000Z"
        );
    }

    #[test]
    fn rclone_snapshot_full_fs_appends_full_subdir() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(
            b.snapshot_full_fs("20260509T143000Z"),
            "s3:bucket/saves/.history/eden/20260509T143000Z/full"
        );
    }

    #[test]
    fn rclone_snapshot_delta_fs_at_appends_delta_then_sub() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(
            b.snapshot_delta_fs_at("20260509T143000Z", "user/save"),
            "s3:bucket/saves/.history/eden/20260509T143000Z/delta/user/save"
        );
    }

    #[test]
    fn rclone_snapshot_delta_fs_empty_sub_omits_trailing_slash() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/saves/eden".into(),
        };
        assert_eq!(
            b.snapshot_delta_fs_at("20260509T143000Z", ""),
            "s3:bucket/saves/.history/eden/20260509T143000Z/delta"
        );
    }

    #[test]
    fn snapshot_full_and_delta_never_collide() {
        // The subdir split is the whole point — if these collided, running
        // both modes simultaneously would overwrite each other's history.
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/eden".into(),
        };
        let full = b.snapshot_full_fs("T1");
        let delta = b.snapshot_delta_fs_at("T1", "user/save");
        assert!(!full.starts_with(&delta));
        assert!(!delta.starts_with(&full));
    }

    // ─── join_rclone_path ────────────────────────────────────────────────

    #[test]
    fn join_rclone_path_handles_all_slash_combinations() {
        assert_eq!(join_rclone_path("", "eden"), "eden");
        assert_eq!(join_rclone_path("base", "eden"), "base/eden");
        assert_eq!(join_rclone_path("base/", "eden"), "base/eden");
        assert_eq!(join_rclone_path("base", "/eden"), "base/eden");
        assert_eq!(join_rclone_path("base/", "/eden"), "base/eden");
        assert_eq!(join_rclone_path("", "/eden"), "eden");
    }

    // ─── child() invariants ──────────────────────────────────────────────

    #[test]
    fn child_descends_into_live_root() {
        let b = Backend::Rclone {
            remote: "s3".into(),
            path: "bucket/eden".into(),
        };
        match b.child("user/save") {
            Backend::Rclone { remote, path } => {
                assert_eq!(remote, "s3");
                assert_eq!(path, "bucket/eden/user/save");
            }
            _ => panic!(),
        }
    }
}
