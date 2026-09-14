//! Handlers HTTP do protocolo de sync. Contrato em `docs/protocol.md`.

use std::sync::{Arc, Mutex, RwLock};

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, request::Parts, HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use save_sync_core::db::HistorySettings;
use save_sync_core::protocol::{
    compute_plan, ClientPlan, IndexEntry, PlanRequest, Rev, SyncPlan,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth;
use crate::db;
use crate::storage::{self, Store};
use crate::users;

/// Sessão sem atividade por mais que isso é considerada abandonada, e o
/// emulador volta a aceitar `plan` de outro device.
const SESSION_TTL_MS: i64 = 60 * 60 * 1000;

pub struct AppState {
    pub conn: Mutex<Connection>,
    pub store: Store,
    /// Title DBs centralizadas. Antes cada instalação baixava 93 MB e
    /// mantinha atualizado sozinha; agora vivem num lugar só e os clients
    /// consultam por API — o client Android já nasce sem esse peso.
    pub titles: RwLock<save_sync_core::titledb::TitleDb>,
    pub ps2: RwLock<save_sync_core::ps2db::Ps2Db>,
    pub data_dir: std::path::PathBuf,
    /// Canal de eventos pro SSE. `broadcast` porque pode haver várias abas
    /// e o client de PC abertos ao mesmo tempo, e um evento perdido por
    /// receptor lento é melhor que um receptor segurando o commit.
    pub events: tokio::sync::broadcast::Sender<String>,
}

impl AppState {
    /// Publica um evento pra quem estiver ouvindo o SSE. Falha (ninguém
    /// ouvindo) é silenciosa de propósito: o evento é notificação, não
    /// parte da transação.
    pub fn emit(&self, kind: &str, payload: serde_json::Value) {
        let _ = self
            .events
            .send(json!({ "type": kind, "payload": payload }).to_string());
    }
}

// ─── erros ──────────────────────────────────────────────────────────────

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: Option<String>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str) -> ApiError {
        ApiError { status, code, detail: None }
    }

    fn with_detail(status: StatusCode, code: &'static str, detail: impl Into<String>) -> ApiError {
        ApiError { status, code, detail: Some(detail.into()) }
    }

    fn internal(detail: impl Into<String>) -> ApiError {
        ApiError::with_detail(StatusCode::INTERNAL_SERVER_ERROR, "internal", detail)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.code });
        if let Some(detail) = self.detail {
            body["detail"] = json!(detail);
        }
        (self.status, Json(body)).into_response()
    }
}

/// Erros de validação de path e de hash vêm das funções de storage como
/// `&'static str` com o código já no vocabulário do protocolo.
fn map_storage_err(code: &'static str) -> ApiError {
    let status = match code {
        "invalid_path" => StatusCode::BAD_REQUEST,
        "hash_mismatch" => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError::new(status, code)
}

// ─── autenticação ───────────────────────────────────────────────────────

/// Device autenticado pelo `Authorization: Bearer`. Como extractor, um
/// handler que pede `Device` não tem como esquecer de autenticar.
pub struct Device(pub String);

impl FromRequestParts<Arc<AppState>> for Device {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        let token =
            auth::bearer_token(header).ok_or(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))?;

        let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
        let device = auth::device_for_token(&conn, token)
            .map_err(ApiError::internal)?
            .ok_or(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))?;
        auth::touch_device(&conn, &device);
        Ok(Device(device))
    }
}

/// Usuário logado, resolvido pelo cookie de sessão. Um handler que pede
/// `User` não tem como esquecer de checar o login.
pub struct User(pub String);

pub const SESSION_COOKIE: &str = "save_sync_session";

/// Lê um cookie do header `Cookie`. Não vale trazer uma dependência de
/// cookie jar pra isso: o header é uma lista `nome=valor` separada por
/// `; `, e só precisamos ler um nome.
fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

impl FromRequestParts<Arc<AppState>> for User {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let token = cookie_value(&parts.headers, SESSION_COOKIE)
            .ok_or(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))?
            .to_string();
        let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
        let user = users::user_for_session(&conn, &token)
            .map_err(ApiError::internal)?
            .ok_or(ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))?;
        Ok(User(user))
    }
}

/// Quem pode **ler** o estado sincronizado: um usuário logado ou um device
/// pareado.
///
/// Aceitar as duas provas não é conveniência, é o que evita um problema
/// real: dentro da janela do Tauri a origem é `tauri://localhost` e o
/// server é `http://nas:8787`, então o cookie de sessão só viajaria com
/// `SameSite=None; Secure` — que exige TLS e quebraria o uso em HTTP na
/// LAN. O client já tem `device_token`; usar ele pra ler é estritamente
/// menos privilégio do que já tem pra escrever via sync.
///
/// Os endpoints de `/admin` continuam exigindo sessão: parear outro device
/// ou revogar não é coisa que um device deva poder fazer sozinho.
pub struct Viewer;

impl FromRequestParts<Arc<AppState>> for Viewer {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if User::from_request_parts(parts, state).await.is_ok() {
            return Ok(Viewer);
        }
        Device::from_request_parts(parts, state).await.map(|_| Viewer)
    }
}

/// `Secure` só entra quando o server está atrás de TLS. Ligar por padrão
/// quebraria o acesso por HTTP na LAN, que é o caso comum num NAS; deixar
/// desligado atrás de HTTPS deixaria o cookie viajar em claro. Por isso é
/// escolha explícita, via `SAVE_SYNC_SECURE_COOKIE=1`.
fn session_cookie(token: &str, max_age_secs: i64) -> String {
    let secure = std::env::var("SAVE_SYNC_SECURE_COOKIE").is_ok_and(|v| v == "1");
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{}",
        if secure { "; Secure" } else { "" }
    )
}

fn check_emulator(emu: &str) -> Result<(), ApiError> {
    if storage::is_known_emulator(emu) {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_emulator"))
    }
}

// ─── login ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let token = {
        let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
        users::login(&conn, &req.username, &req.password).map_err(|e| match e {
            users::LoginError::Invalid => {
                ApiError::new(StatusCode::UNAUTHORIZED, "invalid_credentials")
            }
            users::LoginError::Locked { until_ms } => ApiError::with_detail(
                StatusCode::TOO_MANY_REQUESTS,
                "account_locked",
                format!("destrava em {} segundos", (until_ms - auth::now_ms()) / 1000),
            ),
            users::LoginError::Internal(detail) => ApiError::internal(detail),
        })?
    };

    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, session_cookie(&token, 30 * 24 * 3600))],
        Json(json!({"status": "ok"})),
    )
        .into_response())
}

async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE) {
        let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
        users::logout(&conn, token).map_err(ApiError::internal)?;
    }
    // Max-Age=0 apaga o cookie no browser mesmo se a sessão já não existia.
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, session_cookie("", 0))],
        Json(json!({"status": "ok"})),
    )
        .into_response())
}

async fn me(
    State(state): State<Arc<AppState>>,
    User(user_id): User,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    let username = users::username_of(&conn, &user_id)
        .map_err(ApiError::internal)?
        .unwrap_or_default();
    Ok(Json(json!({"user_id": user_id, "username": username})))
}

// ─── administração (exige login) ────────────────────────────────────────

async fn create_pairing_code(
    State(state): State<Arc<AppState>>,
    User(_): User,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    let code = auth::create_pairing_code(&conn).map_err(ApiError::internal)?;
    Ok(Json(json!({"code": code, "expires_in_seconds": 600})))
}

async fn list_devices(
    State(state): State<Arc<AppState>>,
    User(_): User,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    let mut stmt = conn
        .prepare("SELECT id, name, platform, created_at, last_seen FROM devices ORDER BY created_at")
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "platform": r.get::<_, String>(2)?,
                "created_at": r.get::<_, i64>(3)?,
                "last_seen": r.get::<_, Option<i64>>(4)?,
            }))
        })
        .map_err(|e| ApiError::internal(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(json!({"devices": rows})))
}

async fn revoke_device(
    State(state): State<Arc<AppState>>,
    User(_): User,
    Path(device_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    auth::revoke_device(&conn, &device_id).map_err(ApiError::internal)?;
    Ok(Json(json!({"status": "revogado"})))
}

// ─── leitura da árvore viva (exige login) ───────────────────────────────

fn display_name(emu: &str) -> &'static str {
    match emu {
        "eden" => "eden",
        "rpcs3" => "rpcs3",
        "pcsx2" => "pcsx2",
        _ => "?",
    }
}

fn platform_hint(emu: &str) -> &'static str {
    match emu {
        "eden" => "switch",
        "rpcs3" => "ps3",
        "pcsx2" => "ps2",
        _ => "",
    }
}

/// Resumo por emulador. O que o server sabe é o que está sincronizado —
/// paths locais e watchers são do client e não aparecem aqui.
async fn list_emulators(
    State(state): State<Arc<AppState>>,
    _: Viewer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    let mut out = Vec::new();
    for emu in storage::EMULATORS {
        let head = db::head_rev(&conn, emu).map_err(ApiError::internal)?;
        let saves = save_sync_core::saves::list_saves(
            emu,
            &state.store.live_root(emu).to_string_lossy(),
        );
        let last_sync = db::last_commit_at(&conn, emu)
            .map_err(ApiError::internal)?
            .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
            .map(|dt| dt.format("%d/%m/%Y %H:%M:%S").to_string());

        // A forma é a mesma do `EmulatorView` do client, de propósito: a UI
        // é uma só, e os campos que só existem na máquina do emulador
        // (paths, watchers, processo) vêm vazios em vez de ausentes. Quem
        // decide se mostra o controle é o `isTauri()` no front.
        out.push(json!({
            "id": emu,
            "name": display_name(emu),
            "hint": platform_hint(emu),
            "source_path": "",
            "dest_kind": "local",
            "dest_remote": "",
            "dest_path": "",
            "enabled": true,
            "watching": false,
            "proc_watching": false,
            "process_name": "",
            "last_sync": last_sync,
            "last_error": null,
            // Acréscimos que só o server sabe.
            "head_rev": head,
            "save_count": saves.len(),
            "synced": head > 0,
        }));
    }
    Ok(Json(json!({ "emulators": out })))
}

/// Troca o id cru pelo nome do jogo quando a DB do emulador conhece.
/// Enquanto a DB não carregou, o id cru fica — a UI já sabe exibir assim.
fn resolve_titles(state: &AppState, emu: &str, entries: &mut [save_sync_core::saves::SaveEntry]) {
    if emu != "eden" {
        return;
    }
    let Ok(titles) = state.titles.read() else { return };
    for e in entries {
        if let Some(name) = titles.map.get(&e.raw_id.to_uppercase()) {
            e.title = name.clone();
        }
    }
}

async fn list_saves(
    State(state): State<Arc<AppState>>,
    _: Viewer,
    Path(emu): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_emulator(&emu)?;
    // Reusa o mesmo parser do client — a árvore viva tem a estrutura que
    // `core::saves` já sabe ler, porque foi ela que o sync replicou.
    let mut saves = save_sync_core::saves::list_saves(
        &emu,
        &state.store.live_root(&emu).to_string_lossy(),
    );
    resolve_titles(&state, &emu, &mut saves);
    Ok(Json(json!({ "saves": saves })))
}

// ─── title DBs ──────────────────────────────────────────────────────────

async fn title_db_status(
    State(state): State<Arc<AppState>>,
    _: Viewer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (switch_count, switch_at) = {
        let t = state.titles.read().map_err(|_| ApiError::internal("lock"))?;
        (t.map.len(), t.last_update.map(|d| d.to_rfc3339()))
    };
    let (ps2_count, ps2_at) = {
        let p = state.ps2.read().map_err(|_| ApiError::internal("lock"))?;
        (p.map.len(), p.last_update.map(|d| d.to_rfc3339()))
    };
    Ok(Json(json!({
        "switch": { "count": switch_count, "updated_at": switch_at },
        "ps2": { "count": ps2_count, "updated_at": ps2_at },
    })))
}

async fn refresh_title_db(
    State(state): State<Arc<AppState>>,
    User(_): User,
    Path(which): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match which.as_str() {
        "switch" | "ps2" => {}
        _ => return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_title_db")),
    }

    let state2 = Arc::clone(&state);
    let which2 = which.clone();
    // Download de 83 MB não pode segurar a resposta HTTP. Quem quiser
    // acompanhar escuta o SSE.
    tokio::spawn(async move {
        state2.emit("title-db-status", json!({ "db": which2, "status": "refreshing" }));
        let result = crate::title_dbs::refresh(&state2, &which2).await;
        match result {
            Ok(count) => state2.emit(
                "title-db-status",
                json!({ "db": which2, "status": "ready", "count": count }),
            ),
            Err(e) => state2.emit(
                "title-db-status",
                json!({ "db": which2, "status": "error", "detail": e }),
            ),
        }
    });

    Ok(Json(json!({ "status": "refreshing" })))
}

// ─── eventos (SSE) ──────────────────────────────────────────────────────

/// Stream de eventos do server. Substitui os `emit` do Tauri pro que é
/// estado compartilhado: commit de outro device, progresso de refresh das
/// DBs. O client de PC continua usando IPC pro que é local dele.
async fn events(
    State(state): State<Arc<AppState>>,
    _: Viewer,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    use futures::StreamExt;

    let rx = state.events.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|msg| async move {
        // Receptor lento perde evento em vez de segurar o canal; o próximo
        // evento traz o estado de novo.
        msg.ok().map(|data| Ok(Event::default().data(data)))
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    )
}

async fn get_save(
    State(state): State<Arc<AppState>>,
    _: Viewer,
    Path((emu, raw_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_emulator(&emu)?;
    let root = state.store.live_root(&emu).to_string_lossy().into_owned();
    save_sync_core::saves::get_save(&emu, &root, &raw_id)
        .map(|s| Json(json!(s)))
        .ok_or(ApiError::new(StatusCode::NOT_FOUND, "save_not_found"))
}

async fn get_settings(
    State(state): State<Arc<AppState>>,
    _: Viewer,
    Path(emu): Path<String>,
) -> Result<Json<db::HistoryPolicy>, ApiError> {
    check_emulator(&emu)?;
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    db::history_policy(&conn, &emu)
        .map(Json)
        .map_err(ApiError::internal)
}

async fn set_settings(
    State(state): State<Arc<AppState>>,
    _: Viewer,
    Path(emu): Path<String>,
    Json(policy): Json<db::HistoryPolicy>,
) -> Result<Json<db::HistoryPolicy>, ApiError> {
    check_emulator(&emu)?;
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    db::set_history_policy(&conn, &emu, &policy)
        .map(Json)
        .map_err(|e| match e.as_str() {
            "history_mode_required" => {
                ApiError::new(StatusCode::BAD_REQUEST, "history_mode_required")
            }
            _ => ApiError::internal(e),
        })
}

/// Conflitos pendentes: todo `.conflictN` que ainda está na árvore viva.
async fn list_conflicts(
    State(state): State<Arc<AppState>>,
    _: Viewer,
    Path(emu): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_emulator(&emu)?;
    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    let live = db::live_paths(&conn, &emu).map_err(ApiError::internal)?;

    let mut out = Vec::new();
    for path in &live {
        let Some((original, num)) = save_sync_core::history::strip_conflict_marker(path) else {
            continue;
        };
        // Perdedor órfão (o vencedor sumiu) não é conflito acionável — não
        // há "keep current" possível.
        if !live.contains(&original) {
            continue;
        }
        out.push(json!({
            "path": original,
            "conflict_path": path,
            "conflict_num": num,
        }));
    }
    Ok(Json(json!({ "conflicts": out })))
}

// ─── pareamento ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PairRequest {
    pub code: String,
    #[serde(default = "default_device_name")]
    pub device_name: String,
    #[serde(default)]
    pub platform: String,
}

fn default_device_name() -> String {
    "device".to_string()
}

#[derive(Serialize)]
pub struct PairResponse {
    pub device_id: String,
    pub device_token: String,
}

async fn pair(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PairRequest>,
) -> Result<Json<PairResponse>, ApiError> {
    let mut conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    let paired = auth::redeem_pairing_code(&mut conn, &req.code, &req.device_name, &req.platform)
        .map_err(|code| ApiError::new(StatusCode::FORBIDDEN, code))?;
    Ok(Json(PairResponse {
        device_id: paired.device_id,
        device_token: paired.device_token,
    }))
}

// ─── plan ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct PlanResponse {
    pub session: String,
    pub head_rev: Rev,
    #[serde(flatten)]
    pub plan: ClientPlan,
}

async fn plan(
    State(state): State<Arc<AppState>>,
    Device(device): Device,
    Path(emu): Path<String>,
    Json(req): Json<PlanRequest>,
) -> Result<Json<PlanResponse>, ApiError> {
    check_emulator(&emu)?;
    for change in &req.changes {
        storage::validate_path(&emu, &change.path).map_err(map_storage_err)?;
    }

    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;

    // Lock por emulador: duas sessões vivas no mesmo emulador poderiam
    // commitar planos calculados sobre o mesmo head_rev e uma sobrescrever
    // a outra.
    expire_stale_sessions(&conn)?;
    if let Some(owner) = active_session_device(&conn, &emu)? {
        if owner != device {
            return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "busy"));
        }
        // Mesmo device replanejando: descarta a sessão anterior dele.
        drop_sessions_for(&conn, &emu, &device)?;
    }

    // Device parado desde antes do último prune de tombstones: o server já
    // não sabe o que foi apagado nesse intervalo, então um plano normal o
    // faria ressuscitar arquivos. Resync é a única resposta correta.
    let floor = db::min_valid_rev(&conn, &emu).map_err(ApiError::internal)?;
    if req.last_rev > 0 && req.last_rev < floor {
        return Err(ApiError::with_detail(
            StatusCode::GONE,
            "resync_required",
            format!("last_rev {} é anterior ao corte {}", req.last_rev, floor),
        ));
    }

    // O client é a fonte do `last_rev`, mas não a autoridade: se ele alegar
    // mais do que o server registrou pra ele (backup restaurado, banco
    // local copiado de outro device), confiar pularia mudanças que ele
    // nunca viu. Descer pro registrado só custa mandar entrada a mais.
    let recorded = db::device_last_rev(&conn, &device, &emu).map_err(ApiError::internal)?;
    let last_rev = req.last_rev.min(recorded);

    let head = db::head_rev(&conn, &emu).map_err(ApiError::internal)?;
    let remote: Vec<IndexEntry> =
        db::changes_since(&conn, &emu, last_rev).map_err(ApiError::internal)?;
    let taken = db::live_paths(&conn, &emu).map_err(ApiError::internal)?;

    let sync_plan = compute_plan(&req.changes, &remote, &taken);

    let session = new_session_id();
    let now = auth::now_ms();
    conn.execute(
        "INSERT INTO sessions (id, device_id, emulator_id, plan, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session,
            device,
            emu,
            serde_json::to_string(&sync_plan).map_err(|e| ApiError::internal(e.to_string()))?,
            now,
            now + SESSION_TTL_MS
        ],
    )
    .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(PlanResponse {
        session,
        head_rev: head,
        plan: sync_plan.client_view(),
    }))
}

// ─── blobs ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BlobUploadQuery {
    pub session: String,
    pub path: String,
}

async fn put_blob(
    State(state): State<Arc<AppState>>,
    Device(device): Device,
    Path(emu): Path<String>,
    Query(q): Query<BlobUploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_emulator(&emu)?;
    storage::validate_path(&emu, &q.path).map_err(map_storage_err)?;

    let hash = headers
        .get("x-save-sync-hash")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::with_detail(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "header X-Save-Sync-Hash ausente",
        ))?
        .to_string();
    let mtime: i64 = headers
        .get("x-save-sync-mtime")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(auth::now_ms);

    {
        let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
        session_owned_by(&conn, &q.session, &device, &emu)?;

        // Idempotência: mesmo (sessão, path, hash) já recebido não regrava.
        let known: Option<String> = conn
            .query_row(
                "SELECT hash FROM session_blobs WHERE session_id = ?1 AND path = ?2",
                params![q.session, q.path],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if known.as_deref() == Some(hash.as_str()) {
            return Ok(Json(json!({"status": "ja_recebido"})));
        }
    }

    storage::stage_blob(&state.store, &q.session, &q.path, &body, &hash)
        .map_err(map_storage_err)?;

    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    conn.execute(
        "INSERT INTO session_blobs (session_id, path, hash, size, mtime)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(session_id, path) DO UPDATE SET hash = ?3, size = ?4, mtime = ?5",
        params![q.session, q.path, hash, body.len() as i64, mtime],
    )
    .map_err(|e| ApiError::internal(e.to_string()))?;
    touch_session(&conn, &q.session)?;

    Ok(Json(json!({"status": "recebido", "size": body.len()})))
}

#[derive(Deserialize)]
pub struct BlobDownloadQuery {
    pub path: String,
    pub rev: Option<Rev>,
    /// Hash esperado. É a validação preferida, e não o `rev`, porque o
    /// `rev` de um arquivo muda por motivos que não alteram o conteúdo —
    /// preservar um perdedor de conflito renomeia a entrada e carimba um
    /// `rev` novo, e o client, que planejou antes do commit, ainda carrega
    /// o antigo. Validar por conteúdo não tem esse falso negativo.
    pub hash: Option<String>,
}

async fn get_blob(
    State(state): State<Arc<AppState>>,
    Device(_device): Device,
    Path(emu): Path<String>,
    Query(q): Query<BlobDownloadQuery>,
) -> Result<Response, ApiError> {
    check_emulator(&emu)?;
    storage::validate_path(&emu, &q.path).map_err(map_storage_err)?;

    let entry = {
        let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
        db::get_entry(&conn, &emu, &q.path).map_err(ApiError::internal)?
    };
    let Some(entry) = entry.filter(|e| !e.deleted) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "save_not_found"));
    };
    // O client pede uma versão específica: se ela já mudou, refazer o plano
    // é mais barato do que entregar bytes que ele vai descartar. Hash tem
    // precedência sobre rev quando os dois vêm.
    match (&q.hash, q.rev) {
        (Some(hash), _) if !hash.eq_ignore_ascii_case(&entry.hash) => {
            return Err(ApiError::new(StatusCode::CONFLICT, "stale_rev"));
        }
        (None, Some(rev)) if rev != entry.rev => {
            return Err(ApiError::new(StatusCode::CONFLICT, "stale_rev"));
        }
        _ => {}
    }

    let bytes = std::fs::read(state.store.live_path(&emu, &q.path))
        .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            ("x-save-sync-hash".parse().unwrap(), entry.hash.clone()),
            ("x-save-sync-mtime".parse().unwrap(), entry.mtime.to_string()),
        ],
        bytes,
    )
        .into_response())
}

// ─── commit ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CommitRequest {
    pub session: String,
}

#[derive(Serialize)]
pub struct CommitResponse {
    pub new_rev: Rev,
    pub applied: serde_json::Value,
}

async fn commit(
    State(state): State<Arc<AppState>>,
    Device(device): Device,
    Path(emu): Path<String>,
    Json(req): Json<CommitRequest>,
) -> Result<Json<CommitResponse>, ApiError> {
    check_emulator(&emu)?;

    let conn = state.conn.lock().map_err(|_| ApiError::internal("lock"))?;
    session_owned_by(&conn, &req.session, &device, &emu)?;

    let plan_json: String = conn
        .query_row(
            "SELECT plan FROM sessions WHERE id = ?1",
            params![req.session],
            |r| r.get(0),
        )
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "unknown_session"))?;
    let plan: SyncPlan =
        serde_json::from_str(&plan_json).map_err(|e| ApiError::internal(e.to_string()))?;

    // Todo upload prometido tem que ter chegado — senão o commit deixaria o
    // índice apontando pra arquivo que não existe.
    let missing = missing_uploads(&conn, &req.session, &plan)?;
    if !missing.is_empty() {
        return Err(ApiError::with_detail(
            StatusCode::CONFLICT,
            "incomplete_session",
            missing.join(", "),
        ));
    }

    let history = HistorySettings::defaults_for(&emu);
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let backend = state.store.backend(&emu);

    // Snapshot full antes de tudo: retrata a árvore que está prestes a
    // mudar. Falha aqui aborta o commit — history é a rede de segurança do
    // revert, e commitar sem ela seria perder a rede sem avisar.
    if history.enabled && history.full_enabled {
        backend.snapshot_full(&ts).map_err(ApiError::internal)?;
    }
    let delta_dir = (history.enabled && history.incremental_enabled).then(|| {
        std::path::PathBuf::from(backend.snapshot_delta_fs_at(&ts, ""))
    });

    storage::apply_to_live(&state.store, &emu, &req.session, &plan, delta_dir.as_deref())
        .map_err(ApiError::internal)?;

    let new_rev = db::bump_head_rev(&conn, &emu).map_err(ApiError::internal)?;

    // Perdedor de conflito renomeado vira arquivo vivo por direito próprio,
    // pra que o outro device o receba como download comum.
    for rename in &plan.server_rename {
        if let Some(old) = db::get_entry(&conn, &emu, &rename.from).map_err(ApiError::internal)? {
            db::put_entry(
                &conn,
                &emu,
                &IndexEntry::live(&rename.to, new_rev, old.size, old.mtime, &old.hash),
            )
            .map_err(ApiError::internal)?;
        }
    }

    for item in &plan.upload {
        let staged: Option<(String, i64, i64)> = conn
            .query_row(
                "SELECT hash, size, mtime FROM session_blobs WHERE session_id = ?1 AND path = ?2",
                params![req.session, item.path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let Some((hash, size, mtime)) = staged else { continue };
        db::put_entry(
            &conn,
            &emu,
            &IndexEntry::live(&item.path, new_rev, size as u64, mtime, &hash),
        )
        .map_err(ApiError::internal)?;
    }

    for path in &plan.server_delete {
        db::tombstone(&conn, &emu, path, new_rev).map_err(ApiError::internal)?;
    }

    db::set_device_last_rev(&conn, &device, &emu, new_rev).map_err(ApiError::internal)?;

    storage::clear_staging(&state.store, &req.session);
    conn.execute("DELETE FROM sessions WHERE id = ?1", params![req.session])
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Retenção é best-effort e roda depois do commit: o que importa é o
    // dado ter entrado; apagar snapshot velho é ganho, e falhar nisso não
    // pode desfazer um sync que deu certo.
    let _ = prune_history(&state, &conn, &emu, &history);

    drop(conn);
    state.emit(
        "emulator-changed",
        json!({ "id": emu, "rev": new_rev, "device": device }),
    );

    Ok(Json(CommitResponse {
        new_rev,
        applied: json!({
            "uploaded": plan.upload.len(),
            "deleted": plan.server_delete.len(),
            "conflicts": plan.conflicts.len(),
        }),
    }))
}

/// Aplica a política de retenção: apaga snapshots velhos demais ou acima do
/// teto de tamanho, e depois poda tombstones que já passaram da janela.
fn prune_history(
    state: &AppState,
    conn: &Connection,
    emu: &str,
    history: &HistorySettings,
) -> Result<(), String> {
    if history.enabled {
        let backend = state.store.backend(emu);
        save_sync_core::history::prune_history(&backend, history)?;
    }

    // Tombstone que passou da janela some, e o `min_valid_rev` sobe junto —
    // devices parados desde antes disso passam a receber `410 resync_required`
    // em vez de ressuscitar arquivo apagado.
    let cutoff_ms = auth::now_ms() - TOMBSTONE_TTL_MS;
    let cutoff_rev: Option<i64> = conn
        .query_row(
            "SELECT MAX(rev) FROM file_index
             WHERE emulator_id = ?1 AND deleted = 1 AND mtime < ?2",
            params![emu, cutoff_ms],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();

    if let Some(rev) = cutoff_rev {
        db::prune_tombstones(conn, emu, rev as Rev + 1)?;
    }
    Ok(())
}

/// Janela de vida dos tombstones (§7 da spec). Device offline por mais que
/// isso precisa de resync.
const TOMBSTONE_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1000;

// ─── sessões ────────────────────────────────────────────────────────────

fn new_session_id() -> String {
    format!("{:x}{:x}", auth::now_ms(), rand::random::<u64>())
}

fn expire_stale_sessions(conn: &Connection) -> Result<(), ApiError> {
    conn.execute(
        "DELETE FROM sessions WHERE expires_at < ?1",
        params![auth::now_ms()],
    )
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

fn active_session_device(conn: &Connection, emu: &str) -> Result<Option<String>, ApiError> {
    conn.query_row(
        "SELECT device_id FROM sessions WHERE emulator_id = ?1 LIMIT 1",
        params![emu],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| ApiError::internal(e.to_string()))
}

fn drop_sessions_for(conn: &Connection, emu: &str, device: &str) -> Result<(), ApiError> {
    conn.execute(
        "DELETE FROM sessions WHERE emulator_id = ?1 AND device_id = ?2",
        params![emu, device],
    )
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

fn touch_session(conn: &Connection, session: &str) -> Result<(), ApiError> {
    conn.execute(
        "UPDATE sessions SET expires_at = ?1 WHERE id = ?2",
        params![auth::now_ms() + SESSION_TTL_MS, session],
    )
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

/// Confere que a sessão existe e pertence a este device e emulador. Sem
/// isso um device poderia escrever no staging da sessão de outro.
fn session_owned_by(
    conn: &Connection,
    session: &str,
    device: &str,
    emu: &str,
) -> Result<(), ApiError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT device_id, emulator_id FROM sessions WHERE id = ?1",
            params![session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| ApiError::internal(e.to_string()))?;

    match row {
        Some((owner, owner_emu)) if owner == device && owner_emu == emu => Ok(()),
        Some(_) => Err(ApiError::new(StatusCode::FORBIDDEN, "unauthorized")),
        None => Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_session")),
    }
}

fn missing_uploads(
    conn: &Connection,
    session: &str,
    plan: &SyncPlan,
) -> Result<Vec<String>, ApiError> {
    let mut missing = Vec::new();
    for item in &plan.upload {
        let present: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM session_blobs WHERE session_id = ?1 AND path = ?2",
                params![session, item.path],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if present.is_none() {
            missing.push(item.path.clone());
        }
    }
    Ok(missing)
}

// ─── router ─────────────────────────────────────────────────────────────

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/login", post(login))
        .route("/api/v1/logout", post(logout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/admin/pairing-code", post(create_pairing_code))
        .route("/api/v1/admin/devices", get(list_devices))
        .route("/api/v1/admin/devices/{id}", delete(revoke_device))
        .route("/api/v1/emulators", get(list_emulators))
        .route("/api/v1/emulators/{emu}/saves", get(list_saves))
        .route("/api/v1/emulators/{emu}/saves/{raw_id}", get(get_save))
        .route("/api/v1/emulators/{emu}/settings", get(get_settings).put(set_settings))
        .route("/api/v1/emulators/{emu}/conflicts", get(list_conflicts))
        .route("/api/v1/title-dbs", get(title_db_status))
        .route("/api/v1/title-dbs/{which}/refresh", post(refresh_title_db))
        .route("/api/v1/events", get(events))
        .route("/api/v1/pair", post(pair))
        .route("/api/v1/sync/{emu}/plan", post(plan))
        .route("/api/v1/sync/{emu}/blob", put(put_blob).get(get_blob))
        .route("/api/v1/sync/{emu}/commit", post(commit))
        .with_state(state)
}
