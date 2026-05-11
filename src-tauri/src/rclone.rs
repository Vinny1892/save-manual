//! Bindings to librclone (rclone built as a c-shared library) loaded
//! dynamically via libloading. We avoid build-time linking so the same
//! librclone artifact works regardless of the Rust toolchain (MinGW-built
//! .dll plays nice with MSVC-target Rust because we only call extern "C").
//!
//! Native API (from librclone.h):
//! ```c
//! void RcloneInitialize(void);
//! void RcloneFinalize(void);
//! struct RcloneRPCResult { char* Output; int Status; };
//! struct RcloneRPCResult RcloneRPC(char* method, char* input);
//! void RcloneFreeString(char* str);
//! ```

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::{Mutex, OnceLock};

use libloading::{Library, Symbol};

#[repr(C)]
struct RcloneRPCResult {
    output: *mut c_char,
    status: c_int,
}

type FnInitialize = unsafe extern "C" fn();
type FnFinalize = unsafe extern "C" fn();
type FnRPC = unsafe extern "C" fn(*mut c_char, *mut c_char) -> RcloneRPCResult;
type FnFreeString = unsafe extern "C" fn(*mut c_char);

struct Bindings {
    lib: Library,
    initialized: Mutex<bool>,
}

unsafe impl Send for Bindings {}
unsafe impl Sync for Bindings {}

static BINDINGS: OnceLock<Result<Bindings, String>> = OnceLock::new();

fn lib_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "librclone.dll"
    } else if cfg!(target_os = "macos") {
        "librclone.dylib"
    } else {
        "librclone.so"
    }
}

fn load() -> Result<Bindings, String> {
    // libloading respects OS rules: bare name is looked up next to the exe,
    // in OS lib paths, and (on Windows) in the current dir. build.rs copies
    // the lib next to the exe at build time.
    let name = lib_filename();
    let lib = unsafe { Library::new(name) }
        .map_err(|e| format!("dlopen {}: {}", name, e))?;
    Ok(Bindings {
        lib,
        initialized: Mutex::new(false),
    })
}

fn bindings() -> Result<&'static Bindings, String> {
    let entry = BINDINGS.get_or_init(load);
    entry.as_ref().map_err(|e| e.clone())
}

fn ensure_initialized(b: &Bindings) -> Result<(), String> {
    let mut flag = b.initialized.lock().unwrap();
    if *flag {
        return Ok(());
    }
    let init: Symbol<FnInitialize> = unsafe { b.lib.get(b"RcloneInitialize") }
        .map_err(|e| format!("dlsym RcloneInitialize: {}", e))?;
    unsafe { init() };
    *flag = true;
    Ok(())
}

/// Call an rclone RC method. `method` is e.g. `"sync/copy"`, `input` is the
/// argument JSON. Returns the response JSON on success (HTTP status 2xx),
/// or the error JSON otherwise.
pub fn rpc(method: &str, input_json: &str) -> Result<String, String> {
    let b = bindings()?;
    ensure_initialized(b)?;

    let rpc_fn: Symbol<FnRPC> = unsafe { b.lib.get(b"RcloneRPC") }
        .map_err(|e| format!("dlsym RcloneRPC: {}", e))?;
    let free_fn: Symbol<FnFreeString> = unsafe { b.lib.get(b"RcloneFreeString") }
        .map_err(|e| format!("dlsym RcloneFreeString: {}", e))?;

    let m = CString::new(method).map_err(|e| e.to_string())?;
    let i = CString::new(input_json).map_err(|e| e.to_string())?;

    let result = unsafe { rpc_fn(m.into_raw(), i.into_raw()) };

    let output = if result.output.is_null() {
        String::new()
    } else {
        let s = unsafe { CStr::from_ptr(result.output) }
            .to_string_lossy()
            .into_owned();
        unsafe { free_fn(result.output) };
        s
    };

    if (200..300).contains(&result.status) {
        Ok(output)
    } else {
        Err(output)
    }
}

/// Convenience for callers preferring serde_json::Value.
pub fn rpc_json(
    method: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let response = rpc(method, &input.to_string())?;
    if response.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&response).map_err(|e| format!("parse rclone response: {e}"))
}

// ─── high-level helpers ────────────────────────────────────────────────────
//
// These wrap the librclone RPC for the operations the app actually uses.
// Kept thin: each one is a single rpc_json call with the right shape.

/// Parameters for an S3-compatible remote. `provider` selects rclone's
/// per-vendor quirks (AWS, Cloudflare R2, Minio, etc). `endpoint` is
/// ignored for `provider = "AWS"` (rclone derives it from the region) and
/// required for everyone else. `region` defaults to "auto" for providers
/// that don't care (R2 in particular).
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3RemoteConfig {
    pub name: String,
    pub provider: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
}

/// Create (or replace, via `config/update`) an S3-type rclone remote.
/// `obscure: true` makes rclone encode the secret_access_key with its
/// internal scrambler — we never store the plaintext on disk.
pub fn create_s3_remote(cfg: &S3RemoteConfig) -> Result<(), String> {
    let mut params = serde_json::Map::new();
    params.insert("provider".into(), cfg.provider.clone().into());
    params.insert("access_key_id".into(), cfg.access_key_id.clone().into());
    params.insert(
        "secret_access_key".into(),
        cfg.secret_access_key.clone().into(),
    );
    if let Some(endpoint) = cfg.endpoint.as_deref().filter(|s| !s.is_empty()) {
        params.insert("endpoint".into(), endpoint.into());
    }
    if let Some(region) = cfg.region.as_deref().filter(|s| !s.is_empty()) {
        params.insert("region".into(), region.into());
    }

    let exists = list_remotes()?
        .into_iter()
        .any(|r| r == cfg.name || r == format!("{}:", cfg.name));

    let method = if exists { "config/update" } else { "config/create" };
    let mut input = serde_json::json!({
        "name": cfg.name,
        "parameters": serde_json::Value::Object(params),
        "opt": { "obscure": true, "nonInteractive": true },
    });
    if !exists {
        input["type"] = "s3".into();
    }
    rpc_json(method, input).map(|_| ())
}

/// Delete a remote. Idempotent — rclone returns success even if missing,
/// and we treat "not found" as ok.
pub fn delete_remote(name: &str) -> Result<(), String> {
    rpc_json("config/delete", serde_json::json!({ "name": name })).map(|_| ())
}

pub fn list_remotes() -> Result<Vec<String>, String> {
    let v = rpc_json("config/listremotes", serde_json::json!({}))?;
    Ok(v.get("remotes")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| x.as_str().map(|s| s.to_string()))
        .collect())
}

/// Returns the remote's stored configuration. Secrets come back obscured —
/// safe to display, but won't roundtrip if you re-submit them as plaintext.
pub fn get_remote(name: &str) -> Result<serde_json::Value, String> {
    rpc_json("config/get", serde_json::json!({ "name": name }))
}

/// Quick connectivity check. `operations/list` with `maxDepth: 1` does a
/// shallow listing — for S3 that's a HEAD/LIST against the bucket prefix,
/// which surfaces auth and reachability errors without transferring data.
pub fn test_remote(name: &str, path: &str) -> Result<(), String> {
    rpc_json(
        "operations/list",
        serde_json::json!({
            "fs": format!("{name}:"),
            "remote": path,
            "opt": { "maxDepth": 1 },
        }),
    )
    .map(|_| ())
}

/// One entry returned by `operations/list`. Field names follow rclone's
/// JSON casing so serde can map straight from the RPC response.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ListEntry {
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Size", default)]
    pub size: i64,
    #[serde(rename = "ModTime", default)]
    pub mod_time: String,
    #[serde(rename = "IsDir", default)]
    pub is_dir: bool,
}

/// Recursive listing of `<fs>:<path>`. Returns an empty Vec if the path
/// doesn't exist (cloud prefixes spring into being on first write).
///
/// File `Path` fields are relative to the listed root, e.g. listing
/// `.history/eden/` returns entries like `2026-05-09T.../full/Mcd001.ps2`.
pub fn list_recursive(fs: &str, path: &str) -> Result<Vec<ListEntry>, String> {
    let res = rpc_json(
        "operations/list",
        serde_json::json!({
            "fs": fs,
            "remote": path,
            "opt": { "recurse": true },
        }),
    );
    let v = match res {
        Ok(v) => v,
        Err(e) => {
            let msg = e.to_lowercase();
            if msg.contains("not found") || msg.contains("doesn't exist") {
                return Ok(Vec::new());
            }
            return Err(e);
        }
    };
    let arr = v
        .get("list")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        match serde_json::from_value::<ListEntry>(item) {
            Ok(e) => out.push(e),
            Err(_) => {} // skip malformed entries — robustness over strictness
        }
    }
    Ok(out)
}

/// Splits a full rclone fs string into the (backend_prefix, path) pair
/// that `operations/copyfile` and similar callers need. Handles three cases:
///   - "remote:path/..."  → ("remote:", "path/...")
///   - "C:\..." / "C:/.." → ("C:\...", "") (Windows local path, never split)
///   - "/abs/path"        → ("/abs/path", "") (POSIX local path)
pub fn split_root(full: &str) -> (String, String) {
    let bytes = full.as_bytes();
    let is_win_local = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\');

    if !is_win_local {
        if let Some(colon_pos) = full.find(':') {
            let (remote, rest) = full.split_at(colon_pos + 1);
            return (remote.to_string(), rest.to_string());
        }
    }
    (full.to_string(), String::new())
}

/// Stat a path on any backend. Returns None if it doesn't exist. Used to
/// pick between full/ vs delta/ when reverting a save.
pub fn stat(fs: &str, path: &str) -> Result<Option<ListEntry>, String> {
    let res = rpc_json(
        "operations/stat",
        serde_json::json!({ "fs": fs, "remote": path }),
    );
    match res {
        Ok(v) => match v.get("item") {
            Some(item) if !item.is_null() => serde_json::from_value::<ListEntry>(item.clone())
                .map(Some)
                .map_err(|e| e.to_string()),
            _ => Ok(None),
        },
        Err(e) => {
            let msg = e.to_lowercase();
            if msg.contains("not found") || msg.contains("doesn't exist") {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

/// Stat the path at the given full fs string by splitting it internally.
/// Returns Some if the path exists (file OR directory), None otherwise.
pub fn stat_path(full: &str) -> Result<Option<ListEntry>, String> {
    let (fs, remote) = split_root(full);
    stat(&fs, &remote)
}

/// Copy from one full fs string to another. Picks `operations/copyfile`
/// when src is a file, `sync/copy` when it's a directory. Caller must
/// know which (or accept the round-trip cost of a stat first).
pub fn copy_path(src: &str, dst: &str, is_file: bool) -> Result<(), String> {
    if is_file {
        let (src_fs, src_remote) = split_root(src);
        let (dst_fs, dst_remote) = split_root(dst);
        copyfile(&src_fs, &src_remote, &dst_fs, &dst_remote)
    } else {
        copy_fs(src, dst)
    }
}

/// Returns true if there is at least one entry at the given fs/path.
/// Treats "directory not found" as empty (cloud-side prefixes don't exist
/// until something is written into them).
pub fn has_entries(fs: &str, path: &str) -> Result<bool, String> {
    let res = rpc_json(
        "operations/list",
        serde_json::json!({ "fs": fs, "remote": path, "opt": { "maxDepth": 1 } }),
    );
    match res {
        Ok(v) => Ok(v
            .get("list")
            .and_then(|l| l.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)),
        Err(e) => {
            let msg = e.to_lowercase();
            if msg.contains("not found") || msg.contains("doesn't exist") {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Server-side (or in-process for local) directory copy. Used to take a
/// full snapshot from `<live>` → `<.history/<ts>>`. For S3 family this is
/// CopyObject — no bytes leave the cloud.
pub fn copy_fs(src_fs: &str, dst_fs: &str) -> Result<(), String> {
    rpc_json(
        "sync/copy",
        serde_json::json!({
            "srcFs": src_fs,
            "dstFs": dst_fs,
            "createEmptySrcDirs": true,
        }),
    )
    .map(|_| ())
}

/// Single-file copy across rclone backends. Like `copy_fs` but for one
/// file by name within a parent fs.
pub fn copyfile(src_fs: &str, src_remote: &str, dst_fs: &str, dst_remote: &str) -> Result<(), String> {
    rpc_json(
        "operations/copyfile",
        serde_json::json!({
            "srcFs": src_fs,
            "srcRemote": src_remote,
            "dstFs": dst_fs,
            "dstRemote": dst_remote,
        }),
    )
    .map(|_| ())
}

/// Recursively delete an entire path (used for prune and revert cleanup).
pub fn purge(fs: &str, path: &str) -> Result<(), String> {
    rpc_json("operations/purge", serde_json::json!({ "fs": fs, "remote": path }))
        .map(|_| ())
}

#[derive(Debug)]
pub struct BisyncOpts<'a> {
    pub path1: &'a str,
    pub path2: &'a str,
    /// rclone-fs string where path2's overwrites/deletes go. None = no history.
    pub backup_dir2: Option<&'a str>,
    /// "newer" (default), "older", "larger", "smaller", "path1", "path2", "none".
    /// "none" surfaces conflicts in the response without resolving.
    pub conflict_resolve: &'a str,
    /// First-run flag — must be true the first time this pair is bisynced.
    pub resync: bool,
    /// When `resync = true`, picks the seed: "path1" (push), "path2" (pull),
    /// "newer" (per-file mtime). Ignored otherwise.
    pub resync_mode: &'a str,
}

/// Bisync wrapper. Returns the raw rclone response — caller inspects it for
/// stats and (when conflict_resolve == "none") the conflict list.
pub fn bisync(opts: &BisyncOpts) -> Result<serde_json::Value, String> {
    let mut input = serde_json::json!({
        "path1": opts.path1,
        "path2": opts.path2,
        "createEmptySrcDirs": true,
        "conflictResolve": opts.conflict_resolve,
        "resync": opts.resync,
    });
    if opts.resync {
        input["resyncMode"] = opts.resync_mode.into();
    }
    if let Some(bd) = opts.backup_dir2 {
        input["backupdir2"] = bd.into();
    }
    rpc_json("sync/bisync", input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_root_rclone_with_path() {
        let (fs, path) = split_root("s3:bucket/saves/.history/eden");
        assert_eq!(fs, "s3:");
        assert_eq!(path, "bucket/saves/.history/eden");
    }

    #[test]
    fn split_root_rclone_bare() {
        // "remote:" with no path — fs is the whole thing, path is empty.
        let (fs, path) = split_root("s3:");
        assert_eq!(fs, "s3:");
        assert_eq!(path, "");
    }

    #[test]
    fn split_root_windows_drive_letter_stays_intact() {
        // "C:\foo" must NOT split at the drive-letter colon — the path is
        // entirely local. Same for forward-slash flavor "C:/foo".
        let (fs, path) = split_root("C:\\Users\\vini\\backup");
        assert_eq!(fs, "C:\\Users\\vini\\backup");
        assert_eq!(path, "");

        let (fs, path) = split_root("D:/backup/saves");
        assert_eq!(fs, "D:/backup/saves");
        assert_eq!(path, "");
    }

    #[test]
    fn split_root_posix_absolute_path() {
        let (fs, path) = split_root("/home/vini/backup");
        assert_eq!(fs, "/home/vini/backup");
        assert_eq!(path, "");
    }

    #[test]
    fn split_root_lowercase_drive_letter() {
        // Lowercase drive letters are valid on Windows.
        let (fs, _) = split_root("c:/backup");
        assert_eq!(fs, "c:/backup");
    }
}
