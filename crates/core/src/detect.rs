use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DetectCandidate {
    pub path: String,
    pub label: String,
}

pub fn detect_paths(emulator_id: &str) -> Vec<DetectCandidate> {
    let roots = mount_roots();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut results = Vec::new();

    for (root, label) in &roots {
        let candidates = match emulator_id {
            "eden" => check_eden(root),
            "pcsx2" => check_pcsx2(root),
            "rpcs3" => check_rpcs3(root),
            _ => vec![],
        };

        for path in candidates {
            let key = path.canonicalize().unwrap_or_else(|_| path.clone());
            if seen.insert(key) {
                results.push(DetectCandidate {
                    path: path.to_string_lossy().into_owned(),
                    label: label.clone(),
                });
            }
        }
    }

    results
}

fn check_eden(root: &Path) -> Vec<PathBuf> {
    [
        root.join(".local/share/eden/nand"),
        root.join("AppData/Roaming/Eden/nand"),
        // O pacote do eden no Android. Vale lembrar que desde o Android 11
        // este caminho não é alcançável por outro processo — está aqui pro
        // caso de root ou ROM custom, onde o acesso vem por
        // `/data/media/0/Android/data/...` em vez de `/sdcard/`.
        root.join("Android/data/dev.eden.eden_emulator/files/nand"),
        root.join("eden/nand"),
        root.join("Eden/nand"),
    ]
    .into_iter()
    .filter(|p| {
        p.is_dir()
            && (p.join("user").is_dir()
                || p.join("Contents").is_dir()
                || p.join("system").is_dir())
    })
    .collect()
}

fn check_pcsx2(root: &Path) -> Vec<PathBuf> {
    [
        root.join(".config/PCSX2/memcards"),
        root.join("Documents/PCSX2/memcards"),
        // AetherSX2 e NetherSX2 são os equivalentes no Android
        root.join("Android/data/xyz.aethersx2.android/files/memcards"),
        root.join("Android/data/xyz.nethersx2.android/files/memcards"),
        root.join("PCSX2/memcards"),
        root.join("pcsx2/memcards"),
    ]
    .into_iter()
    .filter(|p| p.is_dir())
    .collect()
}

fn check_rpcs3(root: &Path) -> Vec<PathBuf> {
    // RPCS3 não tem versão Android
    [
        root.join(".config/rpcs3/dev_hdd0"),
        root.join("rpcs3/dev_hdd0"),
        root.join("RPCS3/dev_hdd0"),
    ]
    .into_iter()
    .filter(|p| {
        p.is_dir()
            && (p.join("game").is_dir()
                || p.join("home").is_dir()
                || p.join("dev_flash").is_dir())
    })
    .collect()
}

#[cfg(target_os = "linux")]
fn mount_roots() -> Vec<(PathBuf, String)> {
    let mut roots = Vec::new();

    if let Some(home) = dirs::home_dir() {
        roots.push((home, "~".to_string()));
    }

    for base in ["/mnt", "/media", "/run/media"] {
        collect_mount_children(Path::new(base), 2, &mut roots);
    }

    roots
}

#[cfg(target_os = "linux")]
fn collect_mount_children(base: &Path, depth: u8, out: &mut Vec<(PathBuf, String)>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.push((path.clone(), path.to_string_lossy().into_owned()));
            collect_mount_children(&path, depth - 1, out);
        }
    }
}

#[cfg(target_os = "android")]
fn mount_roots() -> Vec<(PathBuf, String)> {
    let mut roots = Vec::new();

    let internal = PathBuf::from("/storage/emulated/0");
    if internal.is_dir() {
        roots.push((internal, "internal".to_string()));
    }

    // Cartões SD externos: /storage/<UUID>
    if let Ok(entries) = std::fs::read_dir("/storage") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str != "emulated" && name_str != "self" {
                let path = entry.path();
                if path.is_dir() {
                    roots.push((path, format!("sdcard:{name_str}")));
                }
            }
        }
    }

    roots
}

#[cfg(target_os = "windows")]
fn mount_roots() -> Vec<(PathBuf, String)> {
    let mut roots = Vec::new();

    if let Some(home) = dirs::home_dir() {
        roots.push((home, "~".to_string()));
    }

    for c in b'A'..=b'Z' {
        let drive = format!("{}:\\", c as char);
        let path = PathBuf::from(&drive);
        if path.exists() {
            roots.push((path, drive));
        }
    }

    roots
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
)))]
fn mount_roots() -> Vec<(PathBuf, String)> {
    dirs::home_dir()
        .map(|h| vec![(h, "~".to_string())])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(root: &Path, rel: &str) {
        std::fs::create_dir_all(root.join(rel)).unwrap();
    }

    #[test]
    fn eden_android_uses_the_real_package_name() {
        // Regressão: o candidato apontava pra `org.eden.android`, que não
        // existe. O pacote é `dev.eden.eden_emulator`, e o caminho errado
        // nunca casou desde que foi escrito.
        let tmp = tempfile::tempdir().unwrap();
        make(tmp.path(), "Android/data/dev.eden.eden_emulator/files/nand/user");

        let found = check_eden(tmp.path());
        assert_eq!(found.len(), 1, "esperado detectar o pacote real");
        assert!(found[0].to_string_lossy().contains("dev.eden.eden_emulator"));
    }

    #[test]
    fn eden_requires_a_nand_that_looks_like_one() {
        // Pasta `nand` vazia não é NAND: sem `user`, `system` ou `Contents`
        // dentro, apontar o sync pra lá sincronizaria o nada.
        //
        // Usa o layout do Linux e não `eden/nand` porque a lista tem
        // `eden/` e `Eden/`, que em filesystem case-insensitive (Windows,
        // macOS) são o mesmo diretório e casariam duas vezes. A dedup
        // existe, mas mora no `detect_paths` — ver o teste abaixo.
        let tmp = tempfile::tempdir().unwrap();
        make(tmp.path(), ".local/share/eden/nand");
        assert!(check_eden(tmp.path()).is_empty());

        make(tmp.path(), ".local/share/eden/nand/user");
        assert_eq!(check_eden(tmp.path()).len(), 1);
    }

    #[test]
    fn case_insensitive_filesystems_yield_duplicate_candidates() {
        // Documenta o porquê da dedup por `canonicalize` no `detect_paths`:
        // aqui o mesmo diretório casa com dois candidatos da lista.
        let tmp = tempfile::tempdir().unwrap();
        make(tmp.path(), "eden/nand/user");

        let found = check_eden(tmp.path());
        if found.len() > 1 {
            let canonical: HashSet<PathBuf> = found
                .iter()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                .collect();
            assert_eq!(
                canonical.len(),
                1,
                "candidatos duplicados têm que colapsar no mesmo diretório real"
            );
        }
    }

    #[test]
    fn eden_finds_the_desktop_layouts() {
        for layout in [
            ".local/share/eden/nand/user",
            "AppData/Roaming/Eden/nand/user",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            make(tmp.path(), layout);
            assert_eq!(check_eden(tmp.path()).len(), 1, "falhou em {layout}");
        }
    }

    #[test]
    fn pcsx2_finds_memcards_including_android_forks() {
        for layout in [
            ".config/PCSX2/memcards",
            "Documents/PCSX2/memcards",
            "Android/data/xyz.aethersx2.android/files/memcards",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            make(tmp.path(), layout);
            assert_eq!(check_pcsx2(tmp.path()).len(), 1, "falhou em {layout}");
        }
    }

    #[test]
    fn rpcs3_requires_a_dev_hdd0_with_content() {
        let tmp = tempfile::tempdir().unwrap();
        make(tmp.path(), ".config/rpcs3/dev_hdd0");
        assert!(check_rpcs3(tmp.path()).is_empty());

        make(tmp.path(), ".config/rpcs3/dev_hdd0/home");
        assert_eq!(check_rpcs3(tmp.path()).len(), 1);
    }

    #[test]
    fn nothing_is_detected_in_an_empty_tree() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(check_eden(tmp.path()).is_empty());
        assert!(check_pcsx2(tmp.path()).is_empty());
        assert!(check_rpcs3(tmp.path()).is_empty());
    }
}
