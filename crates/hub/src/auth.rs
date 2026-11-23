//! Authentication: Argon2id password hashes, opaque server-side sessions in an HttpOnly
//! SameSite=Strict cookie, login throttling and the `AuthUser` extractor.

use std::net::SocketAddr;

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::header::{COOKIE, SET_COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::{ApiError, ApiResult, S};
use crate::util::{now_ms, random_token, sha256_hex};

pub const COOKIE_NAME: &str = "vigil_session";
const ABSOLUTE_SESSION_MS: i64 = 7 * 86_400_000;

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(pw.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false)
}

pub fn validate_password(pw: &str, email: &str) -> Result<(), ApiError> {
    if pw.chars().count() < 10 {
        return Err(ApiError::bad("Password must be at least 10 characters"));
    }
    if pw.len() > 256 {
        return Err(ApiError::bad("Password is too long"));
    }
    if pw.eq_ignore_ascii_case(email) {
        return Err(ApiError::bad("Password must not be the email address"));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role_id: i64,
    pub role: String,
    pub perms: Vec<String>,
    /// `None` = all servers.
    pub scope: Option<Vec<String>>,
    pub must_change_password: bool,
    pub ip: String,
    pub token_hash: String,
}

impl AuthUser {
    pub fn has(&self, perm: &str) -> bool {
        self.perms.iter().any(|p| p == perm)
    }
    pub fn require(&self, perm: &str) -> ApiResult<()> {
        if self.has(perm) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!("Missing permission: {perm}")))
        }
    }
    pub fn can_server(&self, id: &str) -> bool {
        match &self.scope {
            None => true,
            Some(list) => list.iter().any(|s| s == id),
        }
    }
    pub fn me(&self) -> Value {
        json!({
            "id": self.id,
            "email": self.email,
            "name": self.name,
            "role": self.role,
            "role_id": self.role_id,
            "permissions": self.perms,
            "server_scope": self.scope,
            "must_change_password": self.must_change_password,
        })
    }
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    for h in headers.get_all(COOKIE) {
        let Ok(s) = h.to_str() else { continue };
        for part in s.split(';') {
            if let Some(v) = part.trim().strip_prefix(&format!("{COOKIE_NAME}=")) {
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

pub fn peer_ip(state: &S, parts: &Parts) -> String {
    let addr = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
    state.client_ip(&addr, &parts.headers)
}

async fn load_session(state: &S, parts: &Parts) -> ApiResult<AuthUser> {
    let token = cookie_value(&parts.headers).ok_or_else(ApiError::unauthorized)?;
    let token_hash = sha256_hex(&token);
    let idle_ms = state.cfg.session_idle_hours.max(1) as i64 * 3_600_000;
    let ip = peer_ip(state, parts);
    let th = token_hash.clone();
    let user = state
        .db
        .run(move |c| {
            let row = c
                .query_row(
                    "SELECT u.id, u.email, u.name, u.role_id, r.name, r.permissions, u.server_scope, u.must_change_password, \
                     s.last_seen_at, s.expires_at \
                     FROM sessions s JOIN users u ON u.id = s.user_id JOIN roles r ON r.id = u.role_id \
                     WHERE s.token_hash = ?1 AND u.disabled = 0",
                    [&th],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, String>(5)?,
                            r.get::<_, Option<String>>(6)?,
                            r.get::<_, bool>(7)?,
                            r.get::<_, i64>(8)?,
                            r.get::<_, i64>(9)?,
                        ))
                    },
                )
                .optional()?;
            let Some((id, email, name, role_id, role, perms, scope, must_change, last_seen, expires)) = row else {
                return Ok(None);
            };
            let now = now_ms();
            if now > expires || now - last_seen > idle_ms {
                c.execute("DELETE FROM sessions WHERE token_hash = ?1", [&th])?;
                return Ok(None);
            }
            if now - last_seen > 60_000 {
                c.execute("UPDATE sessions SET last_seen_at = ?1 WHERE token_hash = ?2", params![now, th])?;
            }
            let perms: Vec<String> = serde_json::from_str(&perms).unwrap_or_default();
            let scope: Option<Vec<String>> = scope.and_then(|s| serde_json::from_str(&s).ok());
            Ok(Some(AuthUser {
                id,
                email,
                name,
                role_id,
                role,
                perms,
                scope,
                must_change_password: must_change,
                ip: String::new(),
                token_hash: th,
            }))
        })
        .await?;
    let mut user = user.ok_or_else(ApiError::unauthorized)?;
    user.ip = ip;
    Ok(user)
}

impl FromRequestParts<S> for AuthUser {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let u = load_session(state, parts).await?;
        if u.must_change_password {
            return Err(ApiError(StatusCode::FORBIDDEN, "password_change_required".into()));
        }
        Ok(u)
    }
}

/// Like `AuthUser` but also admits users who still must change their password.
pub struct AuthAny(pub AuthUser);

impl FromRequestParts<S> for AuthAny {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Ok(AuthAny(load_session(state, parts).await?))
    }
}

/// Request metadata for unauthenticated handlers.
pub struct ClientMeta {
    pub ip: String,
    pub user_agent: String,
}

impl FromRequestParts<S> for ClientMeta {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientMeta {
            ip: peer_ip(state, parts),
            user_agent: parts.headers.get(USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(200).collect(),
        })
    }
}

// ------------------------------------------------------------------ audit

pub struct AuditEntry<'a> {
    pub action: &'a str,
    pub server: Option<(&'a str, &'a str)>,
    pub target: Option<String>,
    pub detail: Option<String>,
    pub ok: bool,
}

pub async fn audit(state: &S, user: Option<&AuthUser>, ip: &str, e: AuditEntry<'_>) {
    let (uid, email) = user.map(|u| (Some(u.id), u.email.clone())).unwrap_or((None, String::new()));
    let ip = if ip.is_empty() { user.map(|u| u.ip.clone()).unwrap_or_default() } else { ip.to_string() };
    let action = e.action.to_string();
    let (sid, sname) = e.server.map(|(a, b)| (Some(a.to_string()), Some(b.to_string()))).unwrap_or((None, None));
    let (target, detail, ok) = (e.target, e.detail, e.ok);
    let res = state
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO audit (ts, user_id, user_email, ip, action, server_id, server_name, target, detail, ok) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![now_ms(), uid, email, ip, action, sid, sname, target, detail, ok],
            )?;
            Ok(())
        })
        .await;
    if let Err(e) = res {
        tracing::error!("audit write failed: {e:#}");
    }
}

// ------------------------------------------------------------------ handlers

#[derive(Deserialize)]
pub struct LoginBody {
    email: String,
    password: String,
}

fn session_cookie(state: &S, token: &str, max_age: i64) -> HeaderValue {
    let secure = if state.secure_cookies() { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"))
        .expect("valid cookie")
}

/// A real Argon2 hash used for unknown accounts so both paths take the same time.
pub fn dummy_hash() -> String {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| hash_password(&random_token(16)).unwrap_or_default()).clone()
}

pub async fn login(State(state): State<S>, meta: ClientMeta, Json(body): Json<LoginBody>) -> ApiResult<Response> {
    let email = body.email.trim().to_lowercase();
    let ip_key = format!("ip:{}", meta.ip);
    let user_key = format!("user:{email}");
    if let Some(left) = state.throttle.blocked(&ip_key).or_else(|| state.throttle.blocked(&user_key)) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, format!("Too many failed attempts. Try again in {left}s.")));
    }
    if email.is_empty() || body.password.is_empty() || body.password.len() > 256 {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "Invalid email or password".into()));
    }

    let e2 = email.clone();
    let row = state
        .db
        .run(move |c| {
            Ok(c.query_row("SELECT id, password_hash, disabled FROM users WHERE email = ?1", [&e2], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?))
            })
            .optional()?)
        })
        .await?;

    let (hash, uid, disabled) = match &row {
        Some((id, h, d)) => (h.clone(), Some(*id), *d),
        None => (dummy_hash(), None, false),
    };
    let pw = body.password.clone();
    let ok = tokio::task::spawn_blocking(move || verify_password(&pw, &hash)).await.unwrap_or(false);

    if !ok || uid.is_none() || disabled {
        state.throttle.fail(&user_key, 5, 900, 300);
        state.throttle.fail(&ip_key, 20, 900, 900);
        audit(&state, None, &meta.ip, AuditEntry { action: "login_failed", server: None, target: Some(email), detail: None, ok: false }).await;
        return Err(ApiError(StatusCode::UNAUTHORIZED, "Invalid email or password".into()));
    }
    state.throttle.clear(&user_key);
    let uid = uid.unwrap();

    let token = random_token(32);
    let th = sha256_hex(&token);
    let (ip, ua) = (meta.ip.clone(), meta.user_agent.clone());
    state
        .db
        .run(move |c| {
            let now = now_ms();
            c.execute(
                "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at, ip, user_agent) VALUES (?1,?2,?3,?3,?4,?5,?6)",
                params![th, uid, now, now + ABSOLUTE_SESSION_MS, ip, ua],
            )?;
            c.execute("UPDATE users SET last_login_at = ?1 WHERE id = ?2", params![now, uid])?;
            Ok(())
        })
        .await?;

    let user = user_by_token(&state, &token, &meta.ip).await?;
    audit(&state, Some(&user), &meta.ip, AuditEntry { action: "login", server: None, target: None, detail: None, ok: true }).await;

    let mut resp = Json(user.me()).into_response();
    resp.headers_mut().insert(SET_COOKIE, session_cookie(&state, &token, ABSOLUTE_SESSION_MS / 1000));
    Ok(resp)
}

async fn user_by_token(state: &S, token: &str, ip: &str) -> ApiResult<AuthUser> {
    let req = axum::http::Request::builder()
        .header(COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (parts, _) = req.into_parts();
    let mut u = load_session(state, &parts).await?;
    u.ip = ip.to_string();
    Ok(u)
}

pub async fn logout(State(state): State<S>, AuthAny(user): AuthAny) -> ApiResult<Response> {
    let th = user.token_hash.clone();
    state.db.run(move |c| Ok(c.execute("DELETE FROM sessions WHERE token_hash = ?1", [th])?)).await?;
    audit(&state, Some(&user), "", AuditEntry { action: "logout", server: None, target: None, detail: None, ok: true }).await;
    let mut resp = Json(json!({ "ok": true })).into_response();
    resp.headers_mut().insert(SET_COOKIE, session_cookie(&state, "", 0));
    Ok(resp)
}

pub async fn me(AuthAny(user): AuthAny) -> Json<Value> {
    Json(user.me())
}

#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

pub async fn change_password(State(state): State<S>, AuthAny(user): AuthAny, Json(body): Json<PasswordBody>) -> ApiResult<Json<Value>> {
    validate_password(&body.new, &user.email)?;
    if body.new == body.current {
        return Err(ApiError::bad("The new password must be different"));
    }
    let uid = user.id;
    let hash: String = state.db.run(move |c| Ok(c.query_row("SELECT password_hash FROM users WHERE id = ?1", [uid], |r| r.get(0))?)).await?;
    let cur = body.current.clone();
    if !tokio::task::spawn_blocking(move || verify_password(&cur, &hash)).await.unwrap_or(false) {
        state.throttle.fail(&format!("user:{}", user.email), 5, 900, 300);
        return Err(ApiError::bad("Current password is incorrect"));
    }
    let new = body.new.clone();
    let new_hash = tokio::task::spawn_blocking(move || hash_password(&new)).await.map_err(|e| anyhow::anyhow!("{e}"))??;
    let keep = user.token_hash.clone();
    state
        .db
        .run(move |c| {
            c.execute("UPDATE users SET password_hash = ?1, must_change_password = 0 WHERE id = ?2", params![new_hash, uid])?;
            // Sign out every other session of this user.
            c.execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash <> ?2", params![uid, keep])?;
            Ok(())
        })
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "password_changed", server: None, target: None, detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}
