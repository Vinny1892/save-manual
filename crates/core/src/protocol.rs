//! Tipos do wire e o algoritmo de plano do protocolo de sync.
//!
//! Especificação completa em `docs/protocol.md`. Este módulo é o contrato
//! compartilhado: o server monta o plano com `compute_plan`, o client
//! consome os mesmos tipos, e o Android vai serializar o mesmo JSON.
//!
//! O algoritmo é puro de propósito — entra estado, sai plano, sem tocar em
//! disco nem em rede. É o que permite cobrir a matriz de casos com teste
//! unitário, que é onde bug de sync bidirecional se esconde.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Contador monotônico por emulador. Toda entrada alterada num commit leva
/// o `head_rev` daquele commit.
pub type Rev = u64;

/// Versão do protocolo que este build fala. O server recusa client que
/// mande versão maior.
pub const PROTOCOL_VERSION: u32 = 1;

// ─── mudanças reportadas pelo client ────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ChangeOp {
    Put {
        size: u64,
        /// epoch em milissegundos, UTC
        mtime: i64,
        /// SHA-256 hex minúsculo
        hash: String,
    },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    #[serde(flatten)]
    pub op: ChangeOp,
}

impl Change {
    pub fn put(path: &str, size: u64, mtime: i64, hash: &str) -> Change {
        Change {
            path: path.to_string(),
            op: ChangeOp::Put { size, mtime, hash: hash.to_string() },
        }
    }

    pub fn delete(path: &str) -> Change {
        Change { path: path.to_string(), op: ChangeOp::Delete }
    }
}

// ─── estado do server ───────────────────────────────────────────────────

/// Uma entrada do índice do server. `deleted` é tombstone: a entrada existe
/// justamente pra propagar a deleção pra quem estava offline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub path: String,
    pub rev: Rev,
    pub size: u64,
    pub mtime: i64,
    pub hash: String,
    pub deleted: bool,
}

impl IndexEntry {
    pub fn live(path: &str, rev: Rev, size: u64, mtime: i64, hash: &str) -> IndexEntry {
        IndexEntry {
            path: path.to_string(),
            rev,
            size,
            mtime,
            hash: hash.to_string(),
            deleted: false,
        }
    }

    pub fn tombstone(path: &str, rev: Rev) -> IndexEntry {
        IndexEntry {
            path: path.to_string(),
            rev,
            size: 0,
            mtime: 0,
            hash: String::new(),
            deleted: true,
        }
    }
}

// ─── requisição e resposta ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRequest {
    pub last_rev: Rev,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Client,
    Server,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadItem {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadItem {
    pub path: String,
    pub rev: Rev,
    pub size: u64,
    pub mtime: i64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteItem {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictItem {
    pub path: String,
    pub winner: Side,
    /// Onde o perdedor foi preservado. Este path aparece em `upload` ou em
    /// `download` conforme o lado que segura o conteúdo — o client nunca
    /// precisa inferir.
    pub loser_path: String,
}

/// Renomeação que o server aplica na árvore viva no commit, pra preservar
/// o perdedor de um conflito sem transferir bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenamePair {
    pub from: String,
    pub to: String,
}

/// O plano completo, incluindo as ações que só o server executa. É esta
/// forma que fica guardada na sessão entre o `plan` e o `commit` — por isso
/// serializa inteira, `server_*` incluído.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncPlan {
    pub upload: Vec<UploadItem>,
    pub download: Vec<DownloadItem>,
    pub delete_local: Vec<DeleteItem>,
    pub conflicts: Vec<ConflictItem>,
    /// Paths que o server vira tombstone no commit. O client não recebe:
    /// ele já sabe que apagou, foi ele quem reportou.
    #[serde(default)]
    pub server_delete: Vec<String>,
    /// Renames que o server aplica na árvore viva pra preservar perdedor de
    /// conflito sem transferir bytes.
    #[serde(default)]
    pub server_rename: Vec<RenamePair>,
}

/// A fatia do plano que vai pro client. Existe separada de `SyncPlan` pra
/// que as ações internas do server não vazem no wire por descuido — a
/// diferença fica explícita num tipo, não num atributo fácil de perder numa
/// refatoração.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientPlan {
    pub upload: Vec<UploadItem>,
    pub download: Vec<DownloadItem>,
    pub delete_local: Vec<DeleteItem>,
    pub conflicts: Vec<ConflictItem>,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.upload.is_empty()
            && self.download.is_empty()
            && self.delete_local.is_empty()
            && self.server_delete.is_empty()
            && self.server_rename.is_empty()
    }

    pub fn client_view(&self) -> ClientPlan {
        ClientPlan {
            upload: self.upload.clone(),
            download: self.download.clone(),
            delete_local: self.delete_local.clone(),
            conflicts: self.conflicts.clone(),
        }
    }
}

// ─── algoritmo ──────────────────────────────────────────────────────────

/// Monta o plano cruzando o que o client mudou desde o baseline com o que o
/// server mudou desde o `last_rev` daquele device.
///
/// `remote` deve conter só entradas com `rev > last_rev`; `taken` é o
/// conjunto de paths que já existem na árvore viva, usado pra escolher um
/// sufixo `.conflictN` livre.
///
/// A matriz de decisão, por path:
///
/// | client        | server            | resultado |
/// |---------------|-------------------|-----------|
/// | put           | —                 | upload |
/// | delete        | —                 | server_delete |
/// | —             | put               | download |
/// | —             | tombstone         | delete_local |
/// | put           | put, mesmo hash   | nada (só reconcilia) |
/// | put           | put, hash difere  | conflito por mtime |
/// | put           | tombstone         | upload (edição vence deleção) |
/// | delete        | put               | download (edição vence deleção) |
/// | delete        | tombstone         | nada |
pub fn compute_plan(local: &[Change], remote: &[IndexEntry], taken: &BTreeSet<String>) -> SyncPlan {
    let local_by_path: BTreeMap<&str, &Change> =
        local.iter().map(|c| (c.path.as_str(), c)).collect();
    let remote_by_path: BTreeMap<&str, &IndexEntry> =
        remote.iter().map(|e| (e.path.as_str(), e)).collect();

    let mut plan = SyncPlan::default();
    // Paths que passam a existir neste ciclo contam como ocupados na hora
    // de escolher o próximo `.conflictN` — senão dois conflitos no mesmo
    // sync escolheriam o mesmo sufixo.
    let mut occupied: BTreeSet<String> = taken.clone();

    let all: BTreeSet<&str> = local_by_path
        .keys()
        .chain(remote_by_path.keys())
        .copied()
        .collect();

    for path in all {
        match (local_by_path.get(path), remote_by_path.get(path)) {
            // Só o client mexeu.
            (Some(change), None) => match &change.op {
                ChangeOp::Put { .. } => plan.upload.push(UploadItem { path: path.to_string() }),
                ChangeOp::Delete => plan.server_delete.push(path.to_string()),
            },

            // Só o server mexeu.
            (None, Some(entry)) => {
                if entry.deleted {
                    plan.delete_local.push(DeleteItem { path: path.to_string() });
                } else {
                    plan.download.push(download_of(entry));
                }
            }

            // Os dois mexeram.
            (Some(change), Some(entry)) => match (&change.op, entry.deleted) {
                // Edição vence deleção, nas duas direções. Apagar um save
                // que alguém acabou de modificar é perda irreversível;
                // ressuscitar um que alguém queria apagado custa um delete.
                (ChangeOp::Put { .. }, true) => {
                    plan.upload.push(UploadItem { path: path.to_string() })
                }
                (ChangeOp::Delete, false) => plan.download.push(download_of(entry)),

                // Os dois apagaram — nada a fazer, o tombstone já existe.
                (ChangeOp::Delete, true) => {}

                (ChangeOp::Put { mtime, hash, .. }, false) => {
                    if *hash == entry.hash {
                        // Mesmo conteúdo dos dois lados: não é conflito,
                        // só convergência. Nada transfere.
                        continue;
                    }
                    let client_wins = match mtime.cmp(&entry.mtime) {
                        std::cmp::Ordering::Greater => true,
                        std::cmp::Ordering::Less => false,
                        // Empate de mtime com conteúdo diferente: ninguém
                        // sabe a ordem real, então desempata sempre pro
                        // server pra que o resultado seja determinístico.
                        std::cmp::Ordering::Equal => false,
                    };
                    let loser_path = next_conflict_path(path, &occupied);
                    occupied.insert(loser_path.clone());

                    if client_wins {
                        // A versão do server vira o perdedor: ela já está
                        // lá, então é rename local dele. O client sobe a
                        // sua e baixa o perdedor renomeado.
                        plan.upload.push(UploadItem { path: path.to_string() });
                        plan.server_rename.push(RenamePair {
                            from: path.to_string(),
                            to: loser_path.clone(),
                        });
                        plan.download.push(DownloadItem {
                            path: loser_path.clone(),
                            rev: entry.rev,
                            size: entry.size,
                            mtime: entry.mtime,
                            hash: entry.hash.clone(),
                        });
                        plan.conflicts.push(ConflictItem {
                            path: path.to_string(),
                            winner: Side::Client,
                            loser_path,
                        });
                    } else {
                        // O client é quem segura o perdedor: ele renomeia
                        // local e sobe com o nome novo, e baixa a versão
                        // vencedora pro path original.
                        plan.download.push(download_of(entry));
                        plan.upload.push(UploadItem { path: loser_path.clone() });
                        plan.conflicts.push(ConflictItem {
                            path: path.to_string(),
                            winner: Side::Server,
                            loser_path,
                        });
                    }
                }
            },

            (None, None) => unreachable!("path veio de uma das duas coleções"),
        }
    }

    plan
}

fn download_of(entry: &IndexEntry) -> DownloadItem {
    DownloadItem {
        path: entry.path.clone(),
        rev: entry.rev,
        size: entry.size,
        mtime: entry.mtime,
        hash: entry.hash.clone(),
    }
}

/// Menor `<path>.conflict<n>` livre, começando em 1. Mesma convenção do
/// `--conflict-loser num` do rclone, que é o que a UI de conflitos já
/// entende hoje.
pub fn next_conflict_path(path: &str, taken: &BTreeSet<String>) -> String {
    (1u32..)
        .map(|n| format!("{path}.conflict{n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("a sequência é infinita")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taken(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    fn no_taken() -> BTreeSet<String> {
        BTreeSet::new()
    }

    // ─── casos de um lado só ────────────────────────────────────────────

    #[test]
    fn local_put_alone_uploads() {
        let plan = compute_plan(&[Change::put("a.sav", 10, 100, "h1")], &[], &no_taken());
        assert_eq!(plan.upload, vec![UploadItem { path: "a.sav".into() }]);
        assert!(plan.download.is_empty());
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn local_delete_alone_deletes_on_server() {
        let plan = compute_plan(&[Change::delete("a.sav")], &[], &no_taken());
        assert_eq!(plan.server_delete, vec!["a.sav".to_string()]);
        assert!(plan.upload.is_empty());
        assert!(plan.delete_local.is_empty());
    }

    #[test]
    fn remote_put_alone_downloads() {
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 100, "h1")];
        let plan = compute_plan(&[], &remote, &no_taken());
        assert_eq!(plan.download.len(), 1);
        assert_eq!(plan.download[0].path, "a.sav");
        assert_eq!(plan.download[0].rev, 5);
        assert!(plan.upload.is_empty());
    }

    #[test]
    fn remote_tombstone_alone_deletes_locally() {
        let remote = vec![IndexEntry::tombstone("a.sav", 5)];
        let plan = compute_plan(&[], &remote, &no_taken());
        assert_eq!(plan.delete_local, vec![DeleteItem { path: "a.sav".into() }]);
        assert!(plan.download.is_empty());
    }

    // ─── o caso que o "diff de manifesto" erra ──────────────────────────

    #[test]
    fn absent_locally_without_local_change_is_download_not_delete() {
        // O client não reportou mudança nenhuma: o arquivo é novidade do
        // server, não algo que o client apagou. Tem que baixar.
        let remote = vec![IndexEntry::live("novo.sav", 7, 10, 100, "h1")];
        let plan = compute_plan(&[], &remote, &no_taken());
        assert_eq!(plan.download.len(), 1);
        assert!(plan.server_delete.is_empty());
    }

    #[test]
    fn absent_locally_with_local_delete_is_server_delete_not_download() {
        // Mesmo estado observável do teste acima — o arquivo não está no
        // client — mas aqui ele reportou a deleção. O desfecho é oposto.
        let plan = compute_plan(&[Change::delete("novo.sav")], &[], &no_taken());
        assert_eq!(plan.server_delete, vec!["novo.sav".to_string()]);
        assert!(plan.download.is_empty());
    }

    // ─── convergência ───────────────────────────────────────────────────

    #[test]
    fn same_hash_both_sides_transfers_nothing() {
        let local = vec![Change::put("a.sav", 10, 100, "igual")];
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 999, "igual")];
        let plan = compute_plan(&local, &remote, &no_taken());
        assert!(plan.is_empty(), "mesmo conteúdo não deve transferir nada");
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn both_deleted_is_noop() {
        let local = vec![Change::delete("a.sav")];
        let remote = vec![IndexEntry::tombstone("a.sav", 5)];
        let plan = compute_plan(&local, &remote, &no_taken());
        assert!(plan.is_empty());
    }

    // ─── edição contra deleção ──────────────────────────────────────────

    #[test]
    fn local_edit_beats_remote_delete() {
        let local = vec![Change::put("a.sav", 10, 100, "h1")];
        let remote = vec![IndexEntry::tombstone("a.sav", 5)];
        let plan = compute_plan(&local, &remote, &no_taken());
        assert_eq!(plan.upload, vec![UploadItem { path: "a.sav".into() }]);
        assert!(plan.delete_local.is_empty());
    }

    #[test]
    fn remote_edit_beats_local_delete() {
        let local = vec![Change::delete("a.sav")];
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 100, "h1")];
        let plan = compute_plan(&local, &remote, &no_taken());
        assert_eq!(plan.download.len(), 1);
        assert!(plan.server_delete.is_empty(), "deleção não pode vencer edição");
    }

    // ─── conflito de verdade ────────────────────────────────────────────

    #[test]
    fn client_newer_wins_and_server_version_is_preserved() {
        let local = vec![Change::put("a.sav", 10, 200, "cliente")];
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 100, "servidor")];
        let plan = compute_plan(&local, &remote, &no_taken());

        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].winner, Side::Client);
        assert_eq!(plan.conflicts[0].loser_path, "a.sav.conflict1");

        // Sobe a versão vencedora do client.
        assert!(plan.upload.contains(&UploadItem { path: "a.sav".into() }));
        // O server preserva a dele por rename, sem transferir.
        assert_eq!(
            plan.server_rename,
            vec![RenamePair { from: "a.sav".into(), to: "a.sav.conflict1".into() }]
        );
        // E o perdedor volta pro client como download.
        assert!(plan.download.iter().any(|d| d.path == "a.sav.conflict1"));
    }

    #[test]
    fn server_newer_wins_and_client_version_is_preserved() {
        let local = vec![Change::put("a.sav", 10, 100, "cliente")];
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 200, "servidor")];
        let plan = compute_plan(&local, &remote, &no_taken());

        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].winner, Side::Server);
        assert_eq!(plan.conflicts[0].loser_path, "a.sav.conflict1");

        // Client baixa a vencedora e sobe a sua já renomeada.
        assert!(plan.download.iter().any(|d| d.path == "a.sav"));
        assert!(plan.upload.contains(&UploadItem { path: "a.sav.conflict1".into() }));
        // Nada pra renomear no server: quem segura o perdedor é o client.
        assert!(plan.server_rename.is_empty());
    }

    #[test]
    fn mtime_tie_with_different_content_goes_to_server() {
        let local = vec![Change::put("a.sav", 10, 100, "cliente")];
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 100, "servidor")];
        let plan = compute_plan(&local, &remote, &no_taken());
        assert_eq!(plan.conflicts[0].winner, Side::Server);
    }

    #[test]
    fn conflict_never_discards_either_side() {
        // A invariante que mais importa: depois do plano, os dois conteúdos
        // continuam existindo em algum path.
        for (client_mtime, server_mtime) in [(200, 100), (100, 200), (100, 100)] {
            let local = vec![Change::put("a.sav", 10, client_mtime, "cliente")];
            let remote = vec![IndexEntry::live("a.sav", 5, 10, server_mtime, "servidor")];
            let plan = compute_plan(&local, &remote, &no_taken());

            let survives_original = plan.upload.iter().any(|u| u.path == "a.sav")
                || plan.download.iter().any(|d| d.path == "a.sav");
            let survives_loser = plan.upload.iter().any(|u| u.path.contains(".conflict"))
                || plan.download.iter().any(|d| d.path.contains(".conflict"))
                || !plan.server_rename.is_empty();

            assert!(survives_original && survives_loser, "perdeu dado com mtimes {client_mtime}/{server_mtime}");
            assert!(plan.delete_local.is_empty());
            assert!(plan.server_delete.is_empty());
        }
    }

    // ─── sufixo do conflito ─────────────────────────────────────────────

    #[test]
    fn next_conflict_path_starts_at_one() {
        assert_eq!(next_conflict_path("a.sav", &no_taken()), "a.sav.conflict1");
    }

    #[test]
    fn next_conflict_path_skips_occupied_suffixes() {
        let t = taken(&["a.sav.conflict1", "a.sav.conflict2"]);
        assert_eq!(next_conflict_path("a.sav", &t), "a.sav.conflict3");
    }

    #[test]
    fn two_conflicts_in_one_plan_get_distinct_suffixes() {
        let local = vec![
            Change::put("a.sav", 10, 200, "c1"),
            Change::put("b.sav", 10, 200, "c2"),
        ];
        let remote = vec![
            IndexEntry::live("a.sav", 5, 10, 100, "s1"),
            IndexEntry::live("b.sav", 5, 10, 100, "s2"),
        ];
        let plan = compute_plan(&local, &remote, &no_taken());
        assert_eq!(plan.conflicts.len(), 2);
        assert_eq!(plan.conflicts[0].loser_path, "a.sav.conflict1");
        assert_eq!(plan.conflicts[1].loser_path, "b.sav.conflict1");
    }

    #[test]
    fn repeated_conflict_on_same_path_does_not_reuse_suffix() {
        // Já existe um .conflict1 não resolvido na árvore viva.
        let t = taken(&["a.sav.conflict1"]);
        let local = vec![Change::put("a.sav", 10, 200, "cliente")];
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 100, "servidor")];
        let plan = compute_plan(&local, &remote, &t);
        assert_eq!(plan.conflicts[0].loser_path, "a.sav.conflict2");
    }

    // ─── forma do JSON ──────────────────────────────────────────────────

    #[test]
    fn change_put_serializes_flat_with_op_tag() {
        let json = serde_json::to_value(Change::put("a.sav", 10, 100, "h1")).unwrap();
        assert_eq!(json["op"], "put");
        assert_eq!(json["path"], "a.sav");
        assert_eq!(json["size"], 10);
        assert_eq!(json["mtime"], 100);
        assert_eq!(json["hash"], "h1");
    }

    #[test]
    fn change_delete_serializes_without_put_fields() {
        let json = serde_json::to_value(Change::delete("a.sav")).unwrap();
        assert_eq!(json["op"], "delete");
        assert!(json.get("hash").is_none());
    }

    #[test]
    fn change_roundtrips_through_json() {
        for original in [Change::put("a.sav", 10, 100, "h1"), Change::delete("b.sav")] {
            let json = serde_json::to_string(&original).unwrap();
            let back: Change = serde_json::from_str(&json).unwrap();
            assert_eq!(back, original);
        }
    }

    #[test]
    fn client_view_drops_server_only_actions() {
        let plan = compute_plan(&[Change::delete("a.sav")], &[], &no_taken());
        assert_eq!(plan.server_delete, vec!["a.sav".to_string()]);

        let json = serde_json::to_value(plan.client_view()).unwrap();
        assert!(json.get("server_delete").is_none());
        assert!(json.get("server_rename").is_none());
        assert!(json.get("upload").is_some());
    }

    #[test]
    fn full_plan_roundtrips_with_server_actions_intact() {
        // A sessão guarda o plano entre o `plan` e o `commit`: se
        // `server_rename` não sobrevivesse à serialização, o perdedor de um
        // conflito seria sobrescrito no commit.
        let local = vec![Change::put("a.sav", 10, 200, "cliente")];
        let remote = vec![IndexEntry::live("a.sav", 5, 10, 100, "servidor")];
        let plan = compute_plan(&local, &remote, &no_taken());

        let json = serde_json::to_string(&plan).unwrap();
        let back: SyncPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, plan);
        assert_eq!(back.server_rename.len(), 1);
    }

    #[test]
    fn side_serializes_lowercase() {
        assert_eq!(serde_json::to_value(Side::Client).unwrap(), "client");
        assert_eq!(serde_json::to_value(Side::Server).unwrap(), "server");
    }
}
