//! Lado client do protocolo de sync (`docs/protocol.md`).
//!
//! Três responsabilidades, nesta ordem:
//!
//! 1. **varrer** a árvore local e comparar com o baseline — o retrato de
//!    como as coisas estavam no fim do último sync
//! 2. **conversar** com o server: plan, transferir, commit
//! 3. **aplicar** o que veio e gravar o baseline novo
//!
//! O baseline é o que o `rclone bisync` guardava nos listing files, e é o
//! que permite distinguir "apaguei" de "nunca tive" — sem ele o protocolo
//! fica incorreto de um jeito que só aparece em produção (§2 da spec).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::protocol::{Change, Rev};

/// Metadados de um arquivo local. O `hash` é sempre preenchido, mas nem
/// sempre recalculado: quando `size` e `mtime` batem com o baseline, o
/// valor anterior é reusado em vez de reler o arquivo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    pub size: u64,
    pub mtime: i64,
    pub hash: String,
}

/// `path` relativo à raiz do emulador → metadados.
pub type Tree = BTreeMap<String, FileMeta>;

/// Estado do device pra um emulador: até onde ele acompanhou o server, e
/// como a árvore estava naquele momento.
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    pub last_rev: Rev,
    pub files: Tree,
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Ok(sha256_bytes(&bytes))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Varre as subárvores sincronizadas e devolve a árvore atual.
///
/// `known` é o baseline: quando `size` e `mtime` batem, o hash é reusado em
/// vez de recalculado. Numa árvore parada isso significa **zero leitura de
/// conteúdo** — só `stat`.
pub fn scan_tree(root: &Path, subtrees: &[&str], known: &Tree) -> Result<Tree, String> {
    let mut out = Tree::new();
    for sub in subtrees {
        let start = if sub.is_empty() {
            root.to_path_buf()
        } else {
            root.join(sub)
        };
        if !start.is_dir() {
            continue; // subárvore ainda não existe: nada a varrer
        }
        walk(&start, root, known, &mut out)?;
    }
    Ok(out)
}

fn walk(dir: &Path, root: &Path, known: &Tree, out: &mut Tree) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };

        if meta.is_dir() {
            walk(&path, root, known, out)?;
            continue;
        }
        if !meta.is_file() {
            // Symlink e afins ficam de fora: o protocolo transfere
            // conteúdo, e seguir link levaria pra fora da árvore.
            continue;
        }

        let Some(rel) = relative_posix(root, &path) else { continue };
        let size = meta.len();
        let mtime = mtime_ms(&meta);

        let hash = match known.get(&rel) {
            Some(prev) if prev.size == size && prev.mtime == mtime => prev.hash.clone(),
            _ => sha256_file(&path)?,
        };
        out.insert(rel, FileMeta { size, mtime, hash });
    }
    Ok(())
}

/// Caminho relativo com separador POSIX, que é o que o protocolo usa.
fn relative_posix(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// O que mudou localmente desde o baseline.
///
/// Um arquivo cujo conteúdo voltou a ser o do baseline (mesmo hash) **não**
/// é mudança, mesmo que o mtime tenha mexido — senão todo save-load do
/// emulador geraria tráfego à toa.
pub fn local_changes(baseline: &Tree, current: &Tree) -> Vec<Change> {
    let mut changes = Vec::new();

    for (path, meta) in current {
        match baseline.get(path) {
            Some(prev) if prev.hash == meta.hash => {}
            _ => changes.push(Change::put(path, meta.size, meta.mtime, &meta.hash)),
        }
    }

    for path in baseline.keys() {
        if !current.contains_key(path) {
            changes.push(Change::delete(path));
        }
    }

    changes
}

/// Grava um arquivo baixado, criando o que falta do caminho. Devolve os
/// metadados pro baseline novo.
pub fn write_local(root: &Path, rel: &str, bytes: &[u8], mtime: i64) -> Result<FileMeta, String> {
    let dest = root.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
    Ok(FileMeta {
        size: bytes.len() as u64,
        mtime,
        hash: sha256_bytes(bytes),
    })
}

/// Apaga um arquivo e os diretórios que ficaram vazios, parando na raiz.
/// Sem a limpeza, save deletado deixa esqueleto de pasta — que a UI mostra
/// como jogo fantasma.
pub fn delete_local(root: &Path, rel: &str) -> Result<(), String> {
    let target = root.join(rel);
    if !target.exists() {
        return Ok(());
    }
    std::fs::remove_file(&target).map_err(|e| e.to_string())?;

    let mut dir = target.parent().map(PathBuf::from);
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) {
            break;
        }
        let is_empty = match std::fs::read_dir(&current) {
            Ok(mut entries) => entries.next().is_none(),
            Err(_) => break,
        };
        if !is_empty || std::fs::remove_dir(&current).is_err() {
            break;
        }
        dir = current.parent().map(PathBuf::from);
    }
    Ok(())
}

/// Renomeia o perdedor de um conflito antes de subir. O client segura o
/// perdedor quando o server venceu (§6 da spec).
pub fn rename_local(root: &Path, from: &str, to: &str) -> Result<(), String> {
    let src = root.join(from);
    if !src.exists() {
        return Ok(());
    }
    let dst = root.join(to);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&src, &dst).map_err(|e| e.to_string())
}

// ─── conversa com o server ──────────────────────────────────────────────

/// Cliente HTTP do protocolo. Carrega a base e o token do device; o resto
/// é só tradução de chamada.
#[derive(Debug, Clone)]
pub struct ServerClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct PlanResponse {
    pub session: String,
    pub head_rev: Rev,
    #[serde(flatten)]
    pub plan: crate::protocol::ClientPlan,
}

#[derive(Debug, Deserialize)]
pub struct CommitResponse {
    pub new_rev: Rev,
}

#[derive(Debug, Deserialize)]
pub struct PairResponse {
    pub device_id: String,
    pub device_token: String,
}

/// Extrai o código de erro estável do corpo, pra que o front consiga
/// traduzir com o mesmo `tErr()` de sempre.
async fn api_error(res: reqwest::Response) -> String {
    let status = res.status();
    match res.json::<serde_json::Value>().await {
        Ok(body) => body
            .get("error")
            .and_then(|e| e.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("http_{status}")),
        Err(_) => format!("http_{status}"),
    }
}

impl ServerClient {
    pub fn new(base: &str, token: &str) -> ServerClient {
        ServerClient {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base, path)
    }

    /// Troca um código de pareamento por um token. Não usa `self.token` —
    /// é justamente a chamada que o obtém.
    pub async fn pair(
        base: &str,
        code: &str,
        device_name: &str,
        platform: &str,
    ) -> Result<PairResponse, String> {
        let res = reqwest::Client::new()
            .post(format!("{}/api/v1/pair", base.trim_end_matches('/')))
            .json(&serde_json::json!({
                "code": code,
                "device_name": device_name,
                "platform": platform,
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(api_error(res).await);
        }
        res.json().await.map_err(|e| e.to_string())
    }

    pub async fn plan(
        &self,
        emu: &str,
        last_rev: Rev,
        changes: &[Change],
    ) -> Result<PlanResponse, String> {
        let res = self
            .http
            .post(self.url(&format!("/sync/{emu}/plan")))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "last_rev": last_rev, "changes": changes }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(api_error(res).await);
        }
        res.json().await.map_err(|e| e.to_string())
    }

    pub async fn put_blob(
        &self,
        emu: &str,
        session: &str,
        rel: &str,
        bytes: Vec<u8>,
        hash: &str,
        mtime: i64,
    ) -> Result<(), String> {
        let res = self
            .http
            .put(self.url(&format!("/sync/{emu}/blob")))
            .bearer_auth(&self.token)
            .query(&[("session", session), ("path", rel)])
            .header("x-save-sync-hash", hash)
            .header("x-save-sync-mtime", mtime.to_string())
            .body(bytes)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(api_error(res).await);
        }
        Ok(())
    }

    /// Baixa validando por **hash**, não por `rev`. O `rev` de um arquivo
    /// pode mudar entre o plano e o commit sem que o conteúdo mude — é o
    /// que acontece quando o server preserva um perdedor de conflito por
    /// rename. Pedir por conteúdo evita esse falso conflito.
    pub async fn get_blob(&self, emu: &str, rel: &str, hash: &str) -> Result<Vec<u8>, String> {
        let res = self
            .http
            .get(self.url(&format!("/sync/{emu}/blob")))
            .bearer_auth(&self.token)
            .query(&[("path", rel), ("hash", hash)])
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(api_error(res).await);
        }
        Ok(res.bytes().await.map_err(|e| e.to_string())?.to_vec())
    }

    pub async fn commit(&self, emu: &str, session: &str) -> Result<CommitResponse, String> {
        let res = self
            .http
            .post(self.url(&format!("/sync/{emu}/commit")))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "session": session }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(api_error(res).await);
        }
        res.json().await.map_err(|e| e.to_string())
    }
}

/// Resultado de um ciclo de sync.
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub uploaded: usize,
    pub downloaded: usize,
    pub deleted: usize,
    pub conflicts: usize,
    pub new_rev: Rev,
    /// Baseline a gravar. Só é válido porque o commit deu certo — gravar
    /// antes deixaria o device achando que está em dia sem estar.
    pub baseline: Baseline,
}

/// Um ciclo completo: varre, planeja, transfere, commita e aplica.
///
/// A ordem do fim importa: o `last_rev` novo só vale depois que tudo foi
/// aplicado em disco. Se o processo morrer no meio, o baseline antigo
/// continua valendo e o próximo plano recalcula — a aplicação é idempotente
/// de propósito.
pub async fn sync_emulator(
    server: &ServerClient,
    emu: &str,
    root: &Path,
    subtrees: &[&str],
    baseline: &Baseline,
) -> Result<SyncReport, String> {
    let current = scan_tree(root, subtrees, &baseline.files)?;
    let changes = local_changes(&baseline.files, &current);

    let planned = server.plan(emu, baseline.last_rev, &changes).await?;
    let plan = planned.plan;

    // O perdedor de conflito que o client segura tem que ser renomeado
    // antes do upload: é com o nome novo que ele sobe.
    let mut staged = current.clone();
    for conflict in &plan.conflicts {
        if conflict.winner == crate::protocol::Side::Server {
            rename_local(root, &conflict.path, &conflict.loser_path)?;
            if let Some(meta) = staged.remove(&conflict.path) {
                staged.insert(conflict.loser_path.clone(), meta);
            }
        }
    }

    for item in &plan.upload {
        let meta = staged
            .get(&item.path)
            .ok_or_else(|| format!("upload pedido de arquivo ausente: {}", item.path))?;
        let bytes = std::fs::read(root.join(&item.path)).map_err(|e| e.to_string())?;
        server
            .put_blob(emu, &planned.session, &item.path, bytes, &meta.hash, meta.mtime)
            .await?;
    }

    let committed = server.commit(emu, &planned.session).await?;

    // Só agora o local muda. Antes do commit, um erro deixaria o disco
    // adiantado em relação ao server.
    let mut next = staged;
    for item in &plan.download {
        let bytes = server.get_blob(emu, &item.path, &item.hash).await?;
        let meta = write_local(root, &item.path, &bytes, item.mtime)?;
        next.insert(item.path.clone(), meta);
    }
    for item in &plan.delete_local {
        delete_local(root, &item.path)?;
        next.remove(&item.path);
    }

    Ok(SyncReport {
        uploaded: plan.upload.len(),
        downloaded: plan.download.len(),
        deleted: plan.delete_local.len(),
        conflicts: plan.conflicts.len(),
        new_rev: committed.new_rev,
        baseline: Baseline { last_rev: committed.new_rev, files: next },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ChangeOp;

    fn write(root: &Path, rel: &str, content: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn tree_of(root: &Path) -> Tree {
        scan_tree(root, &["user/save"], &Tree::new()).unwrap()
    }

    // ─── varredura ──────────────────────────────────────────────────────

    #[test]
    fn scan_finds_files_with_posix_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/jogo/a.sav", b"conteudo");

        let tree = tree_of(tmp.path());
        assert_eq!(tree.len(), 1);
        let meta = tree.get("user/save/jogo/a.sav").expect("path POSIX esperado");
        assert_eq!(meta.size, 8);
        assert_eq!(meta.hash, sha256_bytes(b"conteudo"));
    }

    #[test]
    fn scan_ignores_paths_outside_the_subtrees() {
        // O NAND do eden tem gigabytes de conteúdo de sistema; varrer tudo
        // seria lento e mandaria pro server o que não é save.
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"dentro");
        write(tmp.path(), "system/Contents/x.nca", b"fora");

        let tree = tree_of(tmp.path());
        assert_eq!(tree.len(), 1);
        assert!(tree.contains_key("user/save/a.sav"));
    }

    #[test]
    fn scan_on_missing_subtree_is_empty_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(tree_of(tmp.path()).is_empty());
    }

    #[test]
    fn scan_reuses_hash_when_size_and_mtime_match() {
        // É o que torna um sync de árvore parada barato: sem reuso, cada
        // ciclo leria todo o conteúdo do disco.
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"conteudo");

        let first = tree_of(tmp.path());
        let mut known = first.clone();
        // Marca o hash guardado pra detectar se foi recalculado.
        known.get_mut("user/save/a.sav").unwrap().hash = "reusado".into();

        let second = scan_tree(tmp.path(), &["user/save"], &known).unwrap();
        assert_eq!(second["user/save/a.sav"].hash, "reusado");
    }

    #[test]
    fn scan_rehashes_when_size_changed() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"curto");
        let mut known = tree_of(tmp.path());
        known.get_mut("user/save/a.sav").unwrap().hash = "obsoleto".into();

        write(tmp.path(), "user/save/a.sav", b"conteudo bem mais longo");
        let tree = scan_tree(tmp.path(), &["user/save"], &known).unwrap();
        assert_ne!(tree["user/save/a.sav"].hash, "obsoleto");
    }

    // ─── diff ───────────────────────────────────────────────────────────

    #[test]
    fn new_file_is_a_put() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"novo");
        let changes = local_changes(&Tree::new(), &tree_of(tmp.path()));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "user/save/a.sav");
        assert!(matches!(changes[0].op, ChangeOp::Put { .. }));
    }

    #[test]
    fn missing_file_is_a_delete() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"existe");
        let baseline = tree_of(tmp.path());

        std::fs::remove_file(tmp.path().join("user/save/a.sav")).unwrap();
        let changes = local_changes(&baseline, &tree_of(tmp.path()));
        assert_eq!(changes, vec![Change::delete("user/save/a.sav")]);
    }

    #[test]
    fn untouched_tree_produces_no_changes() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"parado");
        let baseline = tree_of(tmp.path());
        assert!(local_changes(&baseline, &tree_of(tmp.path())).is_empty());
    }

    #[test]
    fn same_content_with_new_mtime_is_not_a_change() {
        // Emulador reescreve save com o mesmo conteúdo o tempo todo; se
        // mtime bastasse, cada partida geraria transferência à toa.
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"igual");
        let mut baseline = tree_of(tmp.path());
        baseline.get_mut("user/save/a.sav").unwrap().mtime -= 10_000;

        let current = scan_tree(tmp.path(), &["user/save"], &Tree::new()).unwrap();
        assert!(local_changes(&baseline, &current).is_empty());
    }

    #[test]
    fn edited_file_is_a_put_with_the_new_hash() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"antes");
        let baseline = tree_of(tmp.path());

        write(tmp.path(), "user/save/a.sav", b"depois");
        let current = scan_tree(tmp.path(), &["user/save"], &Tree::new()).unwrap();
        let changes = local_changes(&baseline, &current);

        assert_eq!(changes.len(), 1);
        match &changes[0].op {
            ChangeOp::Put { hash, .. } => assert_eq!(*hash, sha256_bytes(b"depois")),
            other => panic!("esperado put, veio {other:?}"),
        }
    }

    // ─── aplicação local ────────────────────────────────────────────────

    #[test]
    fn write_local_creates_missing_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = write_local(tmp.path(), "user/save/novo/a.sav", b"baixado", 123).unwrap();
        assert_eq!(
            std::fs::read(tmp.path().join("user/save/novo/a.sav")).unwrap(),
            b"baixado"
        );
        assert_eq!(meta.hash, sha256_bytes(b"baixado"));
        assert_eq!(meta.mtime, 123);
    }

    #[test]
    fn delete_local_cleans_empty_directories_up_to_root() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/jogo/a.sav", b"x");
        delete_local(tmp.path(), "user/save/jogo/a.sav").unwrap();

        assert!(!tmp.path().join("user/save/jogo").exists());
        assert!(tmp.path().exists(), "a raiz não pode ser removida");
    }

    #[test]
    fn delete_local_keeps_directories_with_other_files() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/jogo/a.sav", b"x");
        write(tmp.path(), "user/save/jogo/b.sav", b"y");
        delete_local(tmp.path(), "user/save/jogo/a.sav").unwrap();
        assert!(tmp.path().join("user/save/jogo/b.sav").exists());
    }

    #[test]
    fn delete_local_on_missing_file_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(delete_local(tmp.path(), "user/save/nao-existe.sav").is_ok());
    }

    #[test]
    fn rename_local_preserves_the_loser_content() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "user/save/a.sav", b"perdedor");
        rename_local(tmp.path(), "user/save/a.sav", "user/save/a.sav.conflict1").unwrap();

        assert!(!tmp.path().join("user/save/a.sav").exists());
        assert_eq!(
            std::fs::read(tmp.path().join("user/save/a.sav.conflict1")).unwrap(),
            b"perdedor"
        );
    }

    #[test]
    fn rename_local_on_missing_source_is_ok() {
        // O arquivo pode ter sumido entre o plano e a aplicação.
        let tmp = tempfile::tempdir().unwrap();
        assert!(rename_local(tmp.path(), "sumiu.sav", "sumiu.sav.conflict1").is_ok());
    }
}
