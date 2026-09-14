//! Server do save-sync.
//!
//! Sobe o HTTP, serve a SPA de `apps/web`, atende o protocolo de sync
//! (`docs/protocol.md`) e centraliza as title DBs.
//!
//! Duas variáveis de ambiente controlam o processo (defaults pensados pro
//! container, onde os dois caminhos são volumes):
//!   SAVE_SYNC_ADDR    endereço de escuta          (default 0.0.0.0:8787)
//!   SAVE_SYNC_WEB     diretório da SPA buildada   (default /srv/web)
//!   SAVE_SYNC_DATA    raiz dos dados persistentes (default /data)

mod api;
mod auth;
mod db;
mod storage;
mod title_dbs;
mod users;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use axum::{routing::get, Json, Router};
use tower_http::services::{ServeDir, ServeFile};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Valor que segue uma flag: `--create-user vinicius` devolve `vinicius`.
fn flag_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let pos = args.iter().position(|a| a == flag)?;
    args.get(pos + 1).filter(|v| !v.starts_with("--")).cloned()
}

/// Lê a senha da stdin quando ela foi redirecionada. Com terminal
/// interativo não bloqueia esperando digitação — nesse caso o chamador
/// gera uma senha.
fn read_stdin_password() -> Option<String> {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    Some(buf.trim().to_string())
}

/// `--health-check` é o que o HEALTHCHECK do Dockerfile roda. Um TCP connect
/// no endereço de escuta basta e não custa dependência nenhuma — a imagem é
/// debian-slim, sem curl nem wget.
fn health_check() -> ! {
    let addr = env_or("SAVE_SYNC_ADDR", "0.0.0.0:8787");
    // 0.0.0.0 é endereço de bind, não de destino: pra conectar, vira loopback.
    let target = addr.replace("0.0.0.0:", "127.0.0.1:");
    match target
        .parse::<SocketAddr>()
        .map_err(|e| e.to_string())
        .and_then(|a| {
            std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(3))
                .map_err(|e| e.to_string())
        }) {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            eprintln!("health check falhou em {target}: {e}");
            std::process::exit(1)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--health-check") {
        health_check();
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let addr: SocketAddr = env_or("SAVE_SYNC_ADDR", "0.0.0.0:8787").parse()?;
    let web_dir = PathBuf::from(env_or("SAVE_SYNC_WEB", "/srv/web"));
    let data_dir = PathBuf::from(env_or("SAVE_SYNC_DATA", "/data"));

    std::fs::create_dir_all(&data_dir)?;

    let conn = db::open(&data_dir.join("save-sync-server.db"))?;

    // `--create-user <nome>` cria uma conta e sai. É o bootstrap: o primeiro
    // usuário não tem como ser criado pela web UI, porque a web UI exige
    // estar logado. Exigir `docker exec` no NAS pra isso é a garantia de
    // que ninguém na rede cria a primeira conta antes do dono.
    //
    // A senha vem da stdin quando há algo lá (`echo senha | ... --create-user
    // vini`); sem stdin, o server gera uma e imprime uma vez só.
    if let Some(username) = flag_value("--create-user") {
        let piped = read_stdin_password();
        let (password, generated) = match piped {
            Some(p) if !p.is_empty() => (p, false),
            _ => (users::generate_password(), true),
        };
        users::create_user(&conn, &username, &password)?;
        println!("usuário '{username}' criado");
        if generated {
            println!("senha: {password}");
            println!("guarde agora — ela não é exibida de novo");
        }
        return Ok(());
    }

    // `--pair` emite um código de pareamento pela CLI. A web UI logada tem
    // o mesmo em `POST /api/v1/admin/pairing-code`; a via de CLI continua
    // existindo pra quando não há browser à mão.
    if std::env::args().any(|a| a == "--pair") {
        let code = auth::create_pairing_code(&conn)?;
        println!("código de pareamento: {code}");
        println!("válido por 10 minutos, uso único");
        return Ok(());
    }

    if users::user_count(&conn)? == 0 {
        tracing::warn!(
            "nenhum usuário cadastrado — a web UI não tem como ser acessada. \
             Crie o primeiro com: save-sync-server --create-user <nome>"
        );
    }

    // Carrega do cache em disco o que já existe; o que faltar é baixado em
    // background depois que o HTTP subir.
    let (titles, ps2) = title_dbs::load_cached(&data_dir);
    let (events, _) = tokio::sync::broadcast::channel(256);

    let state = Arc::new(api::AppState {
        conn: Mutex::new(conn),
        store: storage::Store::new(&data_dir),
        titles: std::sync::RwLock::new(titles),
        ps2: std::sync::RwLock::new(ps2),
        data_dir: data_dir.clone(),
        events,
    });

    title_dbs::ensure_in_background(Arc::clone(&state));

    // A SPA é client-side routed: qualquer path desconhecido cai no
    // index.html e o SvelteKit resolve a rota no browser.
    //
    // `fallback` e não `not_found_service`: o segundo força status 404 na
    // resposta do fallback, o que faria toda rota da SPA (/emulator/eden e
    // afins) chegar no browser como 404 — renderiza, mas quebra cache e
    // qualquer coisa que leia o status.
    let spa = ServeDir::new(&web_dir).fallback(ServeFile::new(web_dir.join("index.html")));

    let app = Router::new()
        .route("/health", get(health))
        .merge(api::routes(Arc::clone(&state)))
        .fallback_service(spa);

    tracing::info!(%addr, web = %web_dir.display(), data = %data_dir.display(), "save-sync-server");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
