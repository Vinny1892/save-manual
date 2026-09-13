//! Usuários da web UI, sessões de browser e defesa contra força bruta.
//!
//! Separado do `auth.rs`, que cuida de **devices**. São dois mundos com
//! regras diferentes: device tem token que não expira e serve pra sync;
//! usuário tem senha e sessão que expira, e serve pra administrar.

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};

use crate::auth::{hash_token, now_ms};

/// Sessão de browser dura 30 dias. Não é "para sempre" porque cookie
/// vazado é acesso total; não é curto porque ninguém quer relogar no
/// celular toda semana pra ver um save.
const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Depois de 5 erros a conta trava por 5 minutos. Trava por usuário e não
/// por IP: atrás de um reverse proxy todo mundo tem o mesmo IP, e o IP do
/// proxy é fácil demais de forjar via header.
const MAX_FAILED: i64 = 5;
const LOCKOUT_MS: i64 = 5 * 60 * 1000;

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill(&mut buf[..]);
    hex::encode(buf)
}

// ─── senha ──────────────────────────────────────────────────────────────

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Gera uma senha legível pra quando o admin não fornece uma. Alfabeto sem
/// caracteres ambíguos, mesma escolha do código de pareamento.
pub fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..20)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

// ─── usuários ───────────────────────────────────────────────────────────

pub fn user_count(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

pub fn create_user(conn: &Connection, username: &str, password: &str) -> Result<String, String> {
    let username = username.trim();
    if username.is_empty() {
        return Err("username_vazio".into());
    }
    if password.len() < 8 {
        return Err("senha precisa de pelo menos 8 caracteres".into());
    }
    let id = random_hex(16);
    conn.execute(
        "INSERT INTO users (id, username, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, username, hash_password(password)?, now_ms()],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            "usuário já existe".to_string()
        } else {
            e.to_string()
        }
    })?;
    Ok(id)
}

// ─── login ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LoginError {
    /// Credencial errada. Mesma resposta pra usuário inexistente e senha
    /// errada — distinguir entregaria quais usuários existem.
    Invalid,
    /// Bloqueado por tentativas demais; o valor é quando destrava.
    Locked { until_ms: i64 },
    Internal(String),
}

/// Verifica credenciais e abre uma sessão. Devolve o token do cookie.
pub fn login(conn: &Connection, username: &str, password: &str) -> Result<String, LoginError> {
    if let Some(until) = locked_until(conn, username).map_err(LoginError::Internal)? {
        return Err(LoginError::Locked { until_ms: until });
    }

    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT id, password_hash FROM users WHERE username = ?1",
            params![username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| LoginError::Internal(e.to_string()))?;

    // Usuário inexistente ainda paga o custo de um hash, pra que o tempo de
    // resposta não diga se o usuário existe.
    let Some((user_id, stored)) = row else {
        let _ = verify_password(password, "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$e8Q4mF0sJ1Z0mM6Qz0kK7QzZ0mM6Qz0kK7QzZ0mM6Qw");
        record_failure(conn, username).map_err(LoginError::Internal)?;
        return Err(LoginError::Invalid);
    };

    if !verify_password(password, &stored) {
        record_failure(conn, username).map_err(LoginError::Internal)?;
        return Err(LoginError::Invalid);
    }

    clear_failures(conn, username).map_err(LoginError::Internal)?;

    let token = random_hex(32);
    conn.execute(
        "INSERT INTO web_sessions (token_hash, user_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![hash_token(&token), user_id, now_ms(), now_ms() + SESSION_TTL_MS],
    )
    .map_err(|e| LoginError::Internal(e.to_string()))?;

    Ok(token)
}

/// Resolve um cookie de sessão no usuário. Sessão expirada é apagada em vez
/// de só ignorada, senão a tabela cresce pra sempre.
pub fn user_for_session(conn: &Connection, token: &str) -> Result<Option<String>, String> {
    let hash = hash_token(token);
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT user_id, expires_at FROM web_sessions WHERE token_hash = ?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    match row {
        Some((user, expires)) if expires > now_ms() => Ok(Some(user)),
        Some(_) => {
            let _ = conn.execute(
                "DELETE FROM web_sessions WHERE token_hash = ?1",
                params![hash],
            );
            Ok(None)
        }
        None => Ok(None),
    }
}

pub fn logout(conn: &Connection, token: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM web_sessions WHERE token_hash = ?1",
        params![hash_token(token)],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn username_of(conn: &Connection, user_id: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT username FROM users WHERE id = ?1",
        params![user_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

// ─── trava de força bruta ───────────────────────────────────────────────

fn locked_until(conn: &Connection, username: &str) -> Result<Option<i64>, String> {
    let until: Option<i64> = conn
        .query_row(
            "SELECT locked_until FROM login_attempts WHERE username = ?1",
            params![username],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(until.filter(|u| *u > now_ms()))
}

fn record_failure(conn: &Connection, username: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO login_attempts (username, failed_count, locked_until)
         VALUES (?1, 1, 0)
         ON CONFLICT(username) DO UPDATE SET
           failed_count = failed_count + 1,
           locked_until = CASE
             WHEN failed_count + 1 >= ?2 THEN ?3
             ELSE locked_until
           END",
        params![username, MAX_FAILED, now_ms() + LOCKOUT_MS],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn clear_failures(conn: &Connection, username: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM login_attempts WHERE username = ?1",
        params![username],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
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

    fn with_user() -> Connection {
        let conn = mem();
        create_user(&conn, "vinicius", "senhaforte123").unwrap();
        conn
    }

    // ─── hashing ────────────────────────────────────────────────────────

    #[test]
    fn password_hash_is_not_the_password() {
        let hash = hash_password("senhaforte123").unwrap();
        assert!(!hash.contains("senhaforte123"));
        assert!(hash.starts_with("$argon2"));
    }

    #[test]
    fn same_password_hashes_differently_each_time() {
        // Salt aleatório: dois usuários com a mesma senha não podem ter o
        // mesmo hash, senão o banco entrega quem repetiu senha.
        let a = hash_password("mesmasenha").unwrap();
        let b = hash_password("mesmasenha").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("mesmasenha", &a));
        assert!(verify_password("mesmasenha", &b));
    }

    #[test]
    fn verify_rejects_wrong_password_and_garbage_hash() {
        let hash = hash_password("certa").unwrap();
        assert!(!verify_password("errada", &hash));
        assert!(!verify_password("certa", "nao-e-um-hash"));
        assert!(!verify_password("certa", ""));
    }

    // ─── criação ────────────────────────────────────────────────────────

    #[test]
    fn create_user_rejects_short_password() {
        let conn = mem();
        assert!(create_user(&conn, "alguem", "curta").is_err());
        assert_eq!(user_count(&conn).unwrap(), 0);
    }

    #[test]
    fn create_user_rejects_empty_username() {
        let conn = mem();
        assert!(create_user(&conn, "   ", "senhaforte123").is_err());
    }

    #[test]
    fn create_user_rejects_duplicate_case_insensitively() {
        let conn = with_user();
        // Sem COLLATE NOCASE, "Vinicius" e "vinicius" seriam contas
        // distintas e o login ficaria ambíguo.
        let err = create_user(&conn, "VINICIUS", "outrasenha123").unwrap_err();
        assert!(err.contains("já existe"), "erro inesperado: {err}");
    }

    #[test]
    fn user_count_tracks_creation() {
        let conn = mem();
        assert_eq!(user_count(&conn).unwrap(), 0);
        create_user(&conn, "a", "senhaforte123").unwrap();
        assert_eq!(user_count(&conn).unwrap(), 1);
    }

    // ─── login ──────────────────────────────────────────────────────────

    #[test]
    fn login_with_correct_password_opens_session() {
        let conn = with_user();
        let token = login(&conn, "vinicius", "senhaforte123").unwrap();
        assert_eq!(token.len(), 64);
        assert!(user_for_session(&conn, &token).unwrap().is_some());
    }

    #[test]
    fn login_is_case_insensitive_on_username() {
        let conn = with_user();
        assert!(login(&conn, "Vinicius", "senhaforte123").is_ok());
    }

    #[test]
    fn login_with_wrong_password_fails() {
        let conn = with_user();
        assert!(matches!(
            login(&conn, "vinicius", "errada"),
            Err(LoginError::Invalid)
        ));
    }

    #[test]
    fn login_with_unknown_user_fails_the_same_way() {
        // Mesma variante de erro que senha errada — distinguir revelaria
        // quais usuários existem.
        let conn = with_user();
        assert!(matches!(
            login(&conn, "ninguem", "qualquer"),
            Err(LoginError::Invalid)
        ));
    }

    #[test]
    fn session_token_is_not_stored_in_plaintext() {
        let conn = with_user();
        let token = login(&conn, "vinicius", "senhaforte123").unwrap();
        let stored: String = conn
            .query_row("SELECT token_hash FROM web_sessions", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, token);
    }

    #[test]
    fn unknown_session_resolves_to_nothing() {
        let conn = with_user();
        assert!(user_for_session(&conn, "inventado").unwrap().is_none());
    }

    #[test]
    fn expired_session_is_rejected_and_cleaned_up() {
        let conn = with_user();
        let token = login(&conn, "vinicius", "senhaforte123").unwrap();
        conn.execute(
            "UPDATE web_sessions SET expires_at = ?1",
            params![now_ms() - 1],
        )
        .unwrap();

        assert!(user_for_session(&conn, &token).unwrap().is_none());
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM web_sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "sessão expirada tem que sair da tabela");
    }

    #[test]
    fn logout_invalidates_the_session() {
        let conn = with_user();
        let token = login(&conn, "vinicius", "senhaforte123").unwrap();
        logout(&conn, &token).unwrap();
        assert!(user_for_session(&conn, &token).unwrap().is_none());
    }

    #[test]
    fn logout_does_not_touch_other_sessions() {
        // Sair no celular não pode derrubar a sessão do PC.
        let conn = with_user();
        let pc = login(&conn, "vinicius", "senhaforte123").unwrap();
        let celular = login(&conn, "vinicius", "senhaforte123").unwrap();
        logout(&conn, &celular).unwrap();
        assert!(user_for_session(&conn, &pc).unwrap().is_some());
    }

    // ─── força bruta ────────────────────────────────────────────────────

    #[test]
    fn account_locks_after_repeated_failures() {
        let conn = with_user();
        for _ in 0..MAX_FAILED {
            assert!(matches!(
                login(&conn, "vinicius", "errada"),
                Err(LoginError::Invalid)
            ));
        }
        // A senha certa também é recusada enquanto está travado — é o
        // ponto: um atacante não distingue o acerto.
        assert!(matches!(
            login(&conn, "vinicius", "senhaforte123"),
            Err(LoginError::Locked { .. })
        ));
    }

    #[test]
    fn lock_expires_and_lets_the_user_back_in() {
        let conn = with_user();
        for _ in 0..MAX_FAILED {
            let _ = login(&conn, "vinicius", "errada");
        }
        conn.execute(
            "UPDATE login_attempts SET locked_until = ?1",
            params![now_ms() - 1],
        )
        .unwrap();
        assert!(login(&conn, "vinicius", "senhaforte123").is_ok());
    }

    #[test]
    fn successful_login_resets_the_failure_counter() {
        let conn = with_user();
        for _ in 0..(MAX_FAILED - 1) {
            let _ = login(&conn, "vinicius", "errada");
        }
        login(&conn, "vinicius", "senhaforte123").unwrap();

        // Sem o reset, mais um erro travaria a conta de alguém que já
        // provou saber a senha.
        assert!(matches!(
            login(&conn, "vinicius", "errada"),
            Err(LoginError::Invalid)
        ));
    }

    #[test]
    fn lockout_is_per_username() {
        let conn = with_user();
        create_user(&conn, "outro", "senhaforte456").unwrap();
        for _ in 0..MAX_FAILED {
            let _ = login(&conn, "vinicius", "errada");
        }
        assert!(login(&conn, "outro", "senhaforte456").is_ok());
    }

    #[test]
    fn generated_password_is_long_and_unambiguous() {
        let p = generate_password();
        assert_eq!(p.len(), 20);
        for bad in ['0', 'O', 'l', 'I', '1'] {
            assert!(!p.contains(bad), "senha gerada tem caractere ambíguo: {p}");
        }
    }
}
