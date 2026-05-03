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
