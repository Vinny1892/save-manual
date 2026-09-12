//! Server do save-sync.
//!
//! Estado atual: esqueleto. Sobe o HTTP, serve a SPA buildada de `apps/web`
//! e responde `/health`. A API de sync, o login e o histórico entram nas
//! fases seguintes — ver o roadmap no README.
//!
//! Duas variáveis de ambiente controlam o processo (defaults pensados pro
//! container, onde os dois caminhos são volumes):
//!   SAVE_SYNC_ADDR    endereço de escuta          (default 0.0.0.0:8787)
//!   SAVE_SYNC_WEB     diretório da SPA buildada   (default /srv/web)
//!   SAVE_SYNC_DATA    raiz dos dados persistentes (default /data)

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{routing::get, Json, Router};
use tower_http::services::{ServeDir, ServeFile};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
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
