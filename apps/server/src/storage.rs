//! Árvore de arquivos do server: validação de path, staging e commit.
//!
//! Layout sob `SAVE_SYNC_DATA`:
//!
//! ```text
//! /data
//! ├── save-sync-server.db
//! ├── live/<emu>/...            árvore viva — o que os devices veem
//! ├── .history/<emu>/<ts>/...   snapshots (sibling de live, nunca dentro)
//! └── staging/<session>/...     uploads em voo, ainda não commitados
//! ```
//!
//! A regra que organiza tudo: **nada entra em `live/` fora do commit.** O
//! upload vai pro staging, e o commit move. É o que dá atomicidade e o que
//! permite tirar o snapshot de history no instante certo — antes de
//! sobrescrever.

use std::path::{Path, PathBuf};

use save_sync_core::backend::Backend;
use save_sync_core::engine::sync_subtrees;
use save_sync_core::protocol::{RenamePair, SyncPlan};
use sha2::{Digest, Sha256};

pub const EMULATORS: [&str; 3] = ["eden", "rpcs3", "pcsx2"];

pub fn is_known_emulator(emu: &str) -> bool {
    EMULATORS.contains(&emu)
}

#[derive(Debug, Clone)]
pub struct Store {
    data_dir: PathBuf,
}

impl Store {
    pub fn new(data_dir: impl Into<PathBuf>) -> Store {
        Store { data_dir: data_dir.into() }
    }

    pub fn live_root(&self, emu: &str) -> PathBuf {
        self.data_dir.join("live").join(emu)
    }

    pub fn staging_root(&self, session: &str) -> PathBuf {
        self.data_dir.join("staging").join(session)
    }

    /// Backend do core apontando pra árvore viva deste emulador. É o que dá
    /// acesso a `snapshot_full` e ao `.history` com a invariante de sibling
    /// já garantida (`live/` e `.history/` são irmãos sob `/data`).
    pub fn backend(&self, emu: &str) -> Backend {
        Backend::Local { root: self.live_root(emu) }
    }

    pub fn live_path(&self, emu: &str, rel: &str) -> PathBuf {
        self.live_root(emu).join(rel)
    }

    pub fn staged_path(&self, session: &str, rel: &str) -> PathBuf {
        self.staging_root(session).join(rel)
    }
}

// ─── validação de path ──────────────────────────────────────────────────

/// Um path do protocolo é relativo, POSIX, e confinado às subárvores que
/// participam do sync. A checagem é puramente textual de propósito: ela
/// roda antes de qualquer toque em disco, então não há janela entre validar
/// e usar.
pub fn validate_path(emu: &str, rel: &str) -> Result<(), &'static str> {
    if rel.is_empty() {
        return Err("invalid_path");
    }
    // Barra invertida entraria como nome de arquivo no Linux e como
    // separador no Windows — recusar remove a ambiguidade.
    if rel.contains('\\') {
        return Err("invalid_path");
    }
    if rel.starts_with('/') || rel.ends_with('/') {
        return Err("invalid_path");
    }
    if rel.contains("//") {
        return Err("invalid_path");
    }
    // Windows aceita "C:" e "\\?\"; barrar já na forma textual evita
    // depender do comportamento do Path por plataforma.
    if rel.chars().nth(1) == Some(':') {
        return Err("invalid_path");
    }
    for segment in rel.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("invalid_path");
        }
    }
    if !within_synced_subtree(emu, rel) {
        return Err("invalid_path");
    }
    Ok(())
}

/// O eden só sincroniza duas subárvores do NAND; os outros sincronizam a
/// raiz inteira. Mesma whitelist que o `engine::sync_subtrees` usa no
/// client — se divergisse, o server aceitaria o que o client nunca manda.
fn within_synced_subtree(emu: &str, rel: &str) -> bool {
    sync_subtrees(emu).iter().any(|sub| {
        sub.is_empty() || rel == *sub || rel.starts_with(&format!("{sub}/"))
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

// ─── staging ────────────────────────────────────────────────────────────

/// Grava um blob no staging da sessão, verificando o hash **na ingestão**.
/// Falhar aqui e não no commit é deliberado: o client descobre o problema
/// no arquivo que acabou de mandar, em vez de num commit que reprova tudo.
pub fn stage_blob(
    store: &Store,
    session: &str,
    rel: &str,
    bytes: &[u8],
    declared_hash: &str,
) -> Result<(), &'static str> {
    let actual = sha256_hex(bytes);
    if !actual.eq_ignore_ascii_case(declared_hash) {
        return Err("hash_mismatch");
    }
    let dest = store.staged_path(session, rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "internal")?;
    }
    std::fs::write(&dest, bytes).map_err(|_| "internal")?;
    Ok(())
}

// ─── commit ─────────────────────────────────────────────────────────────

/// Move o staging pra árvore viva e aplica renames e deleções.
///
/// A ordem importa e é esta:
///   1. cópia pro `delta_dir` da versão anterior de tudo que vai ser
///      sobrescrito ou apagado — tem que ser antes de qualquer escrita
///   2. renames dos perdedores de conflito, pra que a versão preservada
///      ainda seja a antiga
///   3. uploads staged → live
///   4. deleções
///
/// `delta_dir` é o equivalente do `--backupdir2` do rclone: com o bisync
/// fora do caminho, é o server que passa a guardar a versão anterior. Passa
/// `None` quando o modo incremental está desligado pro emulador.
///
/// O snapshot **full** é tirado pelo chamador antes desta função, porque
/// retrata a árvore inteira e não só o que este commit toca.
pub fn apply_to_live(
    store: &Store,
    emu: &str,
    session: &str,
    plan: &SyncPlan,
    delta_dir: Option<&Path>,
) -> Result<(), String> {
    // Guarda a versão que está prestes a sumir, antes de qualquer escrita.
    if let Some(delta) = delta_dir {
        let overwritten = plan
            .upload
            .iter()
            .map(|u| u.path.as_str())
            .chain(plan.server_delete.iter().map(|p| p.as_str()));
        for rel in overwritten {
            let current = store.live_path(emu, rel);
            if !current.is_file() {
                continue; // arquivo novo: não há versão anterior a guardar
            }
            let dest = delta.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::copy(&current, &dest).map_err(|e| e.to_string())?;
        }
    }

    for RenamePair { from, to } in &plan.server_rename {
        let src = store.live_path(emu, from);
        if !src.exists() {
            // O arquivo sumiu entre o plano e o commit. Não é fatal: o
            // conteúdo do vencedor chega logo abaixo, e o perdedor que não
            // existe mais não tem o que preservar.
            continue;
        }
        let dst = store.live_path(emu, to);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    }

    for item in &plan.upload {
        let staged = store.staged_path(session, &item.path);
        let dest = store.live_path(emu, &item.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // rename primeiro (mesmo volume, é atômico); copy como plano B pro
        // caso do staging estar em outro filesystem.
        if std::fs::rename(&staged, &dest).is_err() {
            std::fs::copy(&staged, &dest).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&staged);
        }
    }

    for path in &plan.server_delete {
        let target = store.live_path(emu, path);
        if target.exists() {
            std::fs::remove_file(&target).map_err(|e| e.to_string())?;
            prune_empty_dirs(&store.live_root(emu), target.parent());
        }
    }

    Ok(())
}

/// Sobe apagando diretórios que ficaram vazios, parando na raiz viva. Sem
/// isso a árvore acumula esqueleto de save deletado — o que aparece na UI
/// como jogo fantasma sem arquivo.
fn prune_empty_dirs(root: &Path, mut dir: Option<&Path>) {
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) {
            return;
        }
        let is_empty = match std::fs::read_dir(current) {
            Ok(mut entries) => entries.next().is_none(),
            Err(_) => return,
        };
        if !is_empty || std::fs::remove_dir(current).is_err() {
            return;
        }
        dir = current.parent();
    }
}

pub fn clear_staging(store: &Store, session: &str) {
    let _ = std::fs::remove_dir_all(store.staging_root(session));
}

#[cfg(test)]
mod tests {
    use super::*;
    use save_sync_core::protocol::UploadItem;

    fn store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        (tmp, store)
    }

    // ─── validate_path ──────────────────────────────────────────────────

    #[test]
    fn accepts_paths_inside_synced_subtrees() {
        assert!(validate_path("eden", "user/save/0000/game.sav").is_ok());
        assert!(validate_path("eden", "system/save/8000000000000010/x").is_ok());
        // pcsx2/rpcs3 sincronizam a raiz inteira.
        assert!(validate_path("pcsx2", "Mcd001.ps2").is_ok());
        assert!(validate_path("rpcs3", "home/0001/savedata/X/PARAM.SFO").is_ok());
    }

    #[test]
    fn rejects_eden_paths_outside_the_whitelist() {
        // O NAND do eden tem gigabytes de conteúdo de sistema que nunca
        // deve entrar no sync.
        assert!(validate_path("eden", "system/Contents/registered/xyz.nca").is_err());
        assert!(validate_path("eden", "user/Contents/algo").is_err());
    }

    #[test]
    fn rejects_traversal() {
        for evil in [
            "../etc/passwd",
            "user/save/../../../etc/passwd",
            "user/save/..",
            "..",
        ] {
            assert!(validate_path("eden", evil).is_err(), "aceitou {evil}");
        }
    }

    #[test]
    fn rejects_absolute_and_windows_style_paths() {
        for evil in ["/etc/passwd", "C:/Windows/x", "user\\save\\x", "\\\\server\\share"] {
            assert!(validate_path("eden", evil).is_err(), "aceitou {evil}");
        }
    }

    #[test]
    fn rejects_malformed_shapes() {
        for evil in ["", "user/save/", "user//save/x", "./user/save/x"] {
            assert!(validate_path("eden", evil).is_err(), "aceitou {evil}");
        }
    }

    // ─── hash ───────────────────────────────────────────────────────────

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256 de "abc", o vetor canônico do FIPS 180-4.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn stage_blob_rejects_wrong_hash() {
        let (_tmp, store) = store();
        let err = stage_blob(&store, "s1", "a.sav", b"conteudo", "deadbeef").unwrap_err();
        assert_eq!(err, "hash_mismatch");
        assert!(!store.staged_path("s1", "a.sav").exists(), "não pode gravar lixo");
    }

    #[test]
    fn stage_blob_writes_when_hash_matches() {
        let (_tmp, store) = store();
        let hash = sha256_hex(b"conteudo");
        stage_blob(&store, "s1", "user/save/a.sav", b"conteudo", &hash).unwrap();
        assert_eq!(
            std::fs::read(store.staged_path("s1", "user/save/a.sav")).unwrap(),
            b"conteudo"
        );
    }

    #[test]
    fn stage_blob_accepts_uppercase_hash() {
        let (_tmp, store) = store();
        let hash = sha256_hex(b"conteudo").to_uppercase();
        assert!(stage_blob(&store, "s1", "a.sav", b"conteudo", &hash).is_ok());
    }

    // ─── apply_to_live ──────────────────────────────────────────────────

    #[test]
    fn apply_moves_staged_files_into_live() {
        let (_tmp, store) = store();
        let hash = sha256_hex(b"novo");
        stage_blob(&store, "s1", "user/save/a.sav", b"novo", &hash).unwrap();

        let plan = SyncPlan {
            upload: vec![UploadItem { path: "user/save/a.sav".into() }],
            ..Default::default()
        };
        apply_to_live(&store, "eden", "s1", &plan, None).unwrap();

        assert_eq!(
            std::fs::read(store.live_path("eden", "user/save/a.sav")).unwrap(),
            b"novo"
        );
    }

    #[test]
    fn apply_renames_loser_before_writing_winner() {
        // A ordem é o que preserva o dado: se o upload entrasse primeiro, o
        // rename pegaria a versão nova e o perdedor sumiria.
        let (_tmp, store) = store();
        let live = store.live_path("eden", "user/save/a.sav");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        std::fs::write(&live, b"antigo").unwrap();

        let hash = sha256_hex(b"vencedor");
        stage_blob(&store, "s1", "user/save/a.sav", b"vencedor", &hash).unwrap();

        let plan = SyncPlan {
            upload: vec![UploadItem { path: "user/save/a.sav".into() }],
            server_rename: vec![RenamePair {
                from: "user/save/a.sav".into(),
                to: "user/save/a.sav.conflict1".into(),
            }],
            ..Default::default()
        };
        apply_to_live(&store, "eden", "s1", &plan, None).unwrap();

        assert_eq!(std::fs::read(&live).unwrap(), b"vencedor");
        assert_eq!(
            std::fs::read(store.live_path("eden", "user/save/a.sav.conflict1")).unwrap(),
            b"antigo",
            "o perdedor tem que sobreviver com o conteúdo antigo"
        );
    }

    #[test]
    fn apply_deletes_and_cleans_empty_dirs() {
        let (_tmp, store) = store();
        let live = store.live_path("eden", "user/save/jogo/a.sav");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        std::fs::write(&live, b"x").unwrap();

        let plan = SyncPlan {
            server_delete: vec!["user/save/jogo/a.sav".into()],
            ..Default::default()
        };
        apply_to_live(&store, "eden", "s1", &plan, None).unwrap();

        assert!(!live.exists());
        assert!(
            !store.live_path("eden", "user/save/jogo").exists(),
            "diretório vazio vira jogo fantasma na UI"
        );
        assert!(store.live_root("eden").exists(), "a raiz viva não pode ser removida");
    }

    #[test]
    fn apply_delete_keeps_dirs_that_still_have_content() {
        let (_tmp, store) = store();
        let dir = store.live_path("eden", "user/save/jogo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.sav"), b"x").unwrap();
        std::fs::write(dir.join("b.sav"), b"y").unwrap();

        let plan = SyncPlan {
            server_delete: vec!["user/save/jogo/a.sav".into()],
            ..Default::default()
        };
        apply_to_live(&store, "eden", "s1", &plan, None).unwrap();
        assert!(dir.join("b.sav").exists());
        assert!(dir.exists());
    }

    #[test]
    fn apply_tolerates_rename_source_that_vanished() {
        let (_tmp, store) = store();
        let plan = SyncPlan {
            server_rename: vec![RenamePair {
                from: "user/save/sumiu.sav".into(),
                to: "user/save/sumiu.sav.conflict1".into(),
            }],
            ..Default::default()
        };
        assert!(apply_to_live(&store, "eden", "s1", &plan, None).is_ok());
    }

    #[test]
    fn delta_keeps_the_version_about_to_be_overwritten() {
        let (tmp, store) = store();
        let live = store.live_path("eden", "user/save/a.sav");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        std::fs::write(&live, b"versao-antiga").unwrap();

        let hash = sha256_hex(b"versao-nova");
        stage_blob(&store, "s1", "user/save/a.sav", b"versao-nova", &hash).unwrap();

        let delta = tmp.path().join("delta");
        let plan = SyncPlan {
            upload: vec![UploadItem { path: "user/save/a.sav".into() }],
            ..Default::default()
        };
        apply_to_live(&store, "eden", "s1", &plan, Some(&delta)).unwrap();

        assert_eq!(std::fs::read(&live).unwrap(), b"versao-nova");
        assert_eq!(
            std::fs::read(delta.join("user/save/a.sav")).unwrap(),
            b"versao-antiga",
            "sem isso o revert não tem pra onde voltar"
        );
    }

    #[test]
    fn delta_keeps_the_version_about_to_be_deleted() {
        let (tmp, store) = store();
        let live = store.live_path("eden", "user/save/a.sav");
        std::fs::create_dir_all(live.parent().unwrap()).unwrap();
        std::fs::write(&live, b"vai-sumir").unwrap();

        let delta = tmp.path().join("delta");
        let plan = SyncPlan {
            server_delete: vec!["user/save/a.sav".into()],
            ..Default::default()
        };
        apply_to_live(&store, "eden", "s1", &plan, Some(&delta)).unwrap();

        assert!(!live.exists());
        assert_eq!(std::fs::read(delta.join("user/save/a.sav")).unwrap(), b"vai-sumir");
    }

    #[test]
    fn delta_skips_files_that_are_new() {
        // Arquivo que ainda não existe na árvore viva não tem versão
        // anterior — o delta não deve criar entrada vazia pra ele.
        let (tmp, store) = store();
        let hash = sha256_hex(b"novo");
        stage_blob(&store, "s1", "user/save/novo.sav", b"novo", &hash).unwrap();

        let delta = tmp.path().join("delta");
        let plan = SyncPlan {
            upload: vec![UploadItem { path: "user/save/novo.sav".into() }],
            ..Default::default()
        };
        apply_to_live(&store, "eden", "s1", &plan, Some(&delta)).unwrap();

        assert!(!delta.join("user/save/novo.sav").exists());
    }

    #[test]
    fn history_lands_outside_the_live_tree() {
        let (_tmp, store) = store();
        let backend = store.backend("eden");
        let history = backend.history_root_fs();
        let live = store.live_root("eden");
        assert!(
            !PathBuf::from(&history).starts_with(&live),
            "history em {history} está dentro da árvore viva {live:?}"
        );
    }

    #[test]
    fn known_emulators_match_the_supported_set() {
        assert!(is_known_emulator("eden"));
        assert!(is_known_emulator("pcsx2"));
        assert!(is_known_emulator("rpcs3"));
        assert!(!is_known_emulator("duckstation"));
        assert!(!is_known_emulator("../etc"));
    }
}
