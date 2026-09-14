//! Bases de título centralizadas.
//!
//! `titledb.json` (~83 MB, blawar) e `ps2-gameindex.yaml` (~10 MB, PCSX2)
//! traduzem id cru em nome de jogo. Antes cada instalação baixava as duas e
//! mantinha atualizadas sozinha; aqui elas vivem num lugar só e os clients
//! consultam por API — o client Android já nasce sem esses 93 MB.
//!
//! Carregar é lento (parse de dezenas de MB), então nada disso acontece no
//! caminho de uma requisição: o boot dispara em background e a UI mostra id
//! cru enquanto não terminou.

use std::sync::Arc;

use save_sync_core::{ps2db, titledb};

use crate::api::AppState;

/// Carrega do cache em disco o que já existe. Não baixa nada — boot de
/// server não deve depender de rede.
pub fn load_cached(data_dir: &std::path::Path) -> (titledb::TitleDb, ps2db::Ps2Db) {
    let switch_path = titledb::cache_path(data_dir);
    let switch = match titledb::parse(&switch_path) {
        Ok(map) => titledb::TitleDb {
            map: Arc::new(map),
            last_update: titledb::cache_mtime(&switch_path),
        },
        Err(_) => titledb::TitleDb::default(),
    };

    let ps2_path = ps2db::cache_path(data_dir);
    let ps2 = match ps2db::parse(&ps2_path) {
        Ok(map) => ps2db::Ps2Db {
            map: Arc::new(map),
            last_update: ps2db::cache_mtime(&ps2_path),
        },
        Err(_) => ps2db::Ps2Db::default(),
    };

    (switch, ps2)
}

/// Baixa e recarrega uma das bases. Devolve quantas entradas ficaram.
pub async fn refresh(state: &AppState, which: &str) -> Result<usize, String> {
    match which {
        "switch" => {
            let path = titledb::cache_path(&state.data_dir);
            titledb::download(&path).await?;
            let map = titledb::parse(&path)?;
            let count = map.len();
            let mut guard = state.titles.write().map_err(|_| "lock".to_string())?;
            *guard = titledb::TitleDb {
                map: Arc::new(map),
                last_update: titledb::cache_mtime(&path),
            };
            Ok(count)
        }
        "ps2" => {
            let path = ps2db::cache_path(&state.data_dir);
            ps2db::download(&path).await?;
            let map = ps2db::parse(&path)?;
            let count = map.len();
            let mut guard = state.ps2.write().map_err(|_| "lock".to_string())?;
            *guard = ps2db::Ps2Db {
                map: Arc::new(map),
                last_update: ps2db::cache_mtime(&path),
            };
            Ok(count)
        }
        _ => Err("unknown_title_db".into()),
    }
}

/// Baixa em background o que ainda não existe em cache. Chamado no boot:
/// primeira subida do container fica utilizável em segundos e as bases
/// chegam depois, em vez de travar o start por minutos.
pub fn ensure_in_background(state: Arc<AppState>) {
    for which in ["switch", "ps2"] {
        let already = match which {
            "switch" => state.titles.read().map(|t| !t.map.is_empty()).unwrap_or(false),
            _ => state.ps2.read().map(|p| !p.map.is_empty()).unwrap_or(false),
        };
        if already {
            continue;
        }

        let state = Arc::clone(&state);
        tokio::spawn(async move {
            tracing::info!(db = which, "baixando title DB em background");
            state.emit(
                "title-db-status",
                serde_json::json!({ "db": which, "status": "refreshing" }),
            );
            match refresh(&state, which).await {
                Ok(count) => {
                    tracing::info!(db = which, count, "title DB pronta");
                    state.emit(
                        "title-db-status",
                        serde_json::json!({ "db": which, "status": "ready", "count": count }),
                    );
                }
                Err(e) => {
                    // Sem a DB o sistema funciona: a UI mostra id cru. Não
                    // vale derrubar o server por isso.
                    tracing::warn!(db = which, erro = %e, "title DB não carregou");
                    state.emit(
                        "title-db-status",
                        serde_json::json!({ "db": which, "status": "error", "detail": e }),
                    );
                }
            }
        });
    }
}
