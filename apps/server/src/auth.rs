//! Tokens de device e códigos de pareamento.
//!
//! O modelo está em `docs/protocol.md` §10: o usuário loga na web UI e gera
//! um código de uso único; o client troca o código por um `device_token`
//! que não expira e é revogável.
//!
//! O token vai pro banco **hasheado**. Se o `save-sync-server.db` vazar (e
//! ele fica num NAS, junto dos saves), os tokens não saem junto.

use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

/// Alfabeto do código de pareamento: sem 0/O/1/I/L, que o usuário vai
/// digitar olhando pra tela do celular.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;
const CODE_TTL_MS: i64 = 10 * 60 * 1000;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill(&mut buf[..]);
    hex::encode(buf)
}

pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

// ─── pareamento ─────────────────────────────────────────────────────────

pub fn create_pairing_code(conn: &Connection) -> Result<String, String> {
    let mut rng = rand::thread_rng();
    let code: String = (0..CODE_LEN)
        .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
        .collect();
    conn.execute(
        "INSERT INTO pairing_codes (code, expires_at) VALUES (?1, ?2)",
        params![code, now_ms() + CODE_TTL_MS],
    )
    .map_err(|e| e.to_string())?;
    Ok(code)
}

pub struct PairedDevice {
    pub device_id: String,
    pub device_token: String,
}

/// `Debug` manual, não derivado: o token é credencial de acesso total ao
/// sync, e derivar colocaria ele em qualquer `{:?}` que caia num log.
impl std::fmt::Debug for PairedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairedDevice")
            .field("device_id", &self.device_id)
            .field("device_token", &"<redigido>")
            .finish()
    }
}

/// Consome o código e cria o device. O código é de uso único: o `used_at`
/// é gravado na mesma transação que cria o device, então duas tentativas
/// simultâneas não podem parear dois devices com o mesmo código.
pub fn redeem_pairing_code(
    conn: &mut Connection,
    code: &str,
    name: &str,
    platform: &str,
) -> Result<PairedDevice, &'static str> {
    let tx = conn.transaction().map_err(|_| "internal")?;

    let expires_at: Option<i64> = tx
        .query_row(
            "SELECT expires_at FROM pairing_codes WHERE code = ?1 AND used_at IS NULL",
            params![code],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "internal")?;

    let Some(expires_at) = expires_at else {
        return Err("invalid_code");
    };
    if expires_at < now_ms() {
        return Err("code_expired");
    }

    let device_id = random_hex(16);
    let device_token = random_hex(32);

    tx.execute(
        "UPDATE pairing_codes SET used_at = ?1 WHERE code = ?2",
        params![now_ms(), code],
    )
    .map_err(|_| "internal")?;
    tx.execute(
        "INSERT INTO devices (id, name, platform, token_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![device_id, name, platform, hash_token(&device_token), now_ms()],
    )
    .map_err(|_| "internal")?;

    tx.commit().map_err(|_| "internal")?;
    Ok(PairedDevice { device_id, device_token })
}

// ─── autenticação das chamadas de sync ──────────────────────────────────

/// Resolve o `Bearer` num `device_id`. Token revogado (linha apagada) deixa
/// de resolver na hora — é o que faz a revogação pela web UI ter efeito
/// imediato, sem lista de bloqueio.
pub fn device_for_token(conn: &Connection, token: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT id FROM devices WHERE token_hash = ?1",
        params![hash_token(token)],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

pub fn touch_device(conn: &Connection, device_id: &str) {
    let _ = conn.execute(
        "UPDATE devices SET last_seen = ?1 WHERE id = ?2",
        params![now_ms(), device_id],
    );
}

/// Usado pela web UI quando o usuário revoga um device (#4). Sem chamador
/// ainda — a tela de devices é parte daquela issue.
#[allow(dead_code)]
pub fn revoke_device(conn: &Connection, device_id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM devices WHERE id = ?1", params![device_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Extrai o token de um header `Authorization: Bearer <token>`.
pub fn bearer_token(header: Option<&str>) -> Option<&str> {
    let value = header?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn pairing_code_has_expected_shape() {
        let conn = mem();
        let code = create_pairing_code(&conn).unwrap();
        assert_eq!(code.len(), CODE_LEN);
        assert!(
            code.chars().all(|c| CODE_ALPHABET.contains(&(c as u8))),
            "código {code} tem caractere ambíguo"
        );
    }

    #[test]
    fn redeem_returns_device_and_token() {
        let mut conn = mem();
        let code = create_pairing_code(&conn).unwrap();
        let paired = redeem_pairing_code(&mut conn, &code, "pc", "windows").unwrap();
        assert_eq!(paired.device_id.len(), 32);
        assert_eq!(paired.device_token.len(), 64);
    }

    #[test]
    fn code_is_single_use() {
        let mut conn = mem();
        let code = create_pairing_code(&conn).unwrap();
        redeem_pairing_code(&mut conn, &code, "pc", "windows").unwrap();
        assert_eq!(
            redeem_pairing_code(&mut conn, &code, "outro", "android").unwrap_err(),
            "invalid_code"
        );
    }

    #[test]
    fn unknown_code_is_rejected() {
        let mut conn = mem();
        assert_eq!(
            redeem_pairing_code(&mut conn, "NAOEXISTE", "pc", "windows").unwrap_err(),
            "invalid_code"
        );
    }

    #[test]
    fn expired_code_is_rejected() {
        let mut conn = mem();
        conn.execute(
            "INSERT INTO pairing_codes (code, expires_at) VALUES ('VELHO123', ?1)",
            params![now_ms() - 1],
        )
        .unwrap();
        assert_eq!(
            redeem_pairing_code(&mut conn, "VELHO123", "pc", "windows").unwrap_err(),
            "code_expired"
        );
    }

    #[test]
    fn token_is_not_stored_in_plaintext() {
        let mut conn = mem();
        let code = create_pairing_code(&conn).unwrap();
        let paired = redeem_pairing_code(&mut conn, &code, "pc", "windows").unwrap();

        let stored: String = conn
            .query_row("SELECT token_hash FROM devices", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, paired.device_token);
        assert_eq!(stored, hash_token(&paired.device_token));
    }

    #[test]
    fn token_resolves_to_its_device() {
        let mut conn = mem();
        let code = create_pairing_code(&conn).unwrap();
        let paired = redeem_pairing_code(&mut conn, &code, "pc", "windows").unwrap();

        let found = device_for_token(&conn, &paired.device_token).unwrap();
        assert_eq!(found.as_deref(), Some(paired.device_id.as_str()));
    }

    #[test]
    fn unknown_token_resolves_to_nothing() {
        let conn = mem();
        assert!(device_for_token(&conn, "nao-existe").unwrap().is_none());
    }

    #[test]
    fn revoked_device_stops_resolving_immediately() {
        let mut conn = mem();
        let code = create_pairing_code(&conn).unwrap();
        let paired = redeem_pairing_code(&mut conn, &code, "pc", "windows").unwrap();

        revoke_device(&conn, &paired.device_id).unwrap();
        assert!(device_for_token(&conn, &paired.device_token).unwrap().is_none());
    }

    #[test]
    fn two_devices_get_distinct_tokens() {
        let mut conn = mem();
        let c1 = create_pairing_code(&conn).unwrap();
        let c2 = create_pairing_code(&conn).unwrap();
        let d1 = redeem_pairing_code(&mut conn, &c1, "pc", "windows").unwrap();
        let d2 = redeem_pairing_code(&mut conn, &c2, "celular", "android").unwrap();
        assert_ne!(d1.device_token, d2.device_token);
        assert_ne!(d1.device_id, d2.device_id);
    }

    // ─── parsing do header ──────────────────────────────────────────────

    #[test]
    fn bearer_token_parses_valid_header() {
        assert_eq!(bearer_token(Some("Bearer abc123")), Some("abc123"));
        assert_eq!(bearer_token(Some("bearer abc123")), Some("abc123"));
    }

    #[test]
    fn bearer_token_rejects_other_schemes_and_junk() {
        assert_eq!(bearer_token(None), None);
        assert_eq!(bearer_token(Some("Basic abc123")), None);
        assert_eq!(bearer_token(Some("abc123")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None);
        assert_eq!(bearer_token(Some("Bearer   ")), None);
    }
}
