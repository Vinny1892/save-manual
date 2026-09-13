//! Handlers HTTP do protocolo de sync. Contrato em `docs/protocol.md`.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, request::Parts, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
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

/// Sessão sem atividade por mais que isso é considerada abandonada, e o
/// emulador volta a aceitar `plan` de outro device.
const SESSION_TTL_MS: i64 = 60 * 60 * 1000;

pub struct AppState {
    pub conn: Mutex<Connection>,
    pub store: Store,
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

fn check_emulator(emu: &str) -> Result<(), ApiError> {
    if storage::is_known_emulator(emu) {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_emulator"))
    }
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
    // é mais barato do que entregar bytes que ele vai descartar.
    if let Some(requested) = q.rev {
        if requested != entry.rev {
            return Err(ApiError::new(StatusCode::CONFLICT, "stale_rev"));
        }
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

    Ok(Json(CommitResponse {
        new_rev,
        applied: json!({
            "uploaded": plan.upload.len(),
            "deleted": plan.server_delete.len(),
            "conflicts": plan.conflicts.len(),
        }),
    }))
}

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
        .route("/api/v1/pair", post(pair))
        .route("/api/v1/sync/{emu}/plan", post(plan))
        .route("/api/v1/sync/{emu}/blob", put(put_blob).get(get_blob))
        .route("/api/v1/sync/{emu}/commit", post(commit))
        .with_state(state)
}
