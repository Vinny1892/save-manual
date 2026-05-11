use std::path::PathBuf;

fn main() {
    // Stage librclone FIRST so the `_bundle_lib/` glob in tauri.conf.json
    // resources has a file to match by the time tauri_build validates the
    // config. Otherwise tauri_build errors with "path not found or didn't
    // match any files".
    stage_librclone();
    tauri_build::build();
}

/// Copy the librclone artifact (built by scripts/build-librclone.{ps1,sh})
/// next to the produced binary so libloading can dlopen it at runtime.
/// We don't link at build time — the lib is loaded dynamically, which lets
/// the same MinGW-built .dll work with MSVC-target Rust.
fn stage_librclone() {
    let lib_filename = if cfg!(target_os = "windows") {
        "librclone.dll"
    } else if cfg!(target_os = "macos") {
        "librclone.dylib"
    } else {
        "librclone.so"
    };

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib_root = manifest_dir.join("lib");

    // Search any subdir of lib/ for the library — accepts both
    // x86_64-pc-windows-gnu (built by our script) and x86_64-pc-windows-msvc
    // (if the user supplies a pre-built variant).
    let lib_path = std::fs::read_dir(&lib_root)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path().join(lib_filename))
                .find(|p| p.exists())
        });

    let Some(lib_path) = lib_path else {
        panic!(
            "librclone not found in {} - run scripts/build-librclone.ps1 (Windows) or .sh (Unix) first",
            lib_root.display()
        );
    };

    println!("cargo:rerun-if-changed={}", lib_path.display());

    // OUT_DIR is .../target/<profile>/build/<crate>-<hash>/out
    // We want to copy to .../target/<profile>/<lib_filename>.
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        if let Some(target_dir) = PathBuf::from(&out_dir)
            .ancestors()
            .nth(3)
            .map(|p| p.to_path_buf())
        {
            let dest = target_dir.join(lib_filename);
            let _ = std::fs::copy(&lib_path, &dest);
        }
    }

    // Also stage at a fixed, target-agnostic location that
    // `tauri.conf.json::bundle.resources` can reference verbatim. Without
    // this, Tauri's bundler doesn't include `librclone.{so,dll}` in
    // .deb/AppImage/MSI/NSIS — the dev/portable layout works only because
    // the file ends up next to the binary in target/<profile>/.
    let bundle_lib = manifest_dir.join("_bundle_lib");
    std::fs::create_dir_all(&bundle_lib).ok();
    let bundle_dest = bundle_lib.join(lib_filename);
    let _ = std::fs::copy(&lib_path, &bundle_dest);
}
