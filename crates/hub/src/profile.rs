//! Self-service profile: name, job title, phone, email, profile photo and active sessions.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{audit, verify_password, AuditEntry, AuthAny, AuthUser};
use crate::state::{ApiError, ApiResult, S};
use crate::util::now_ms;

/// Photos are resized in the browser to 256×256; this is a generous ceiling.
pub const MAX_AVATAR: usize = 512 * 1024;

#[derive(Deserialize)]
pub struct ProfileBody {
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    phone: String,
    /// Only needed when changing the email address.
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    current_password: Option<String>,
}

fn clean(s: &str, max: usize, what: &str) -> ApiResult<String> {
    let t = s.trim();
    if t.chars().count() > max {
        return Err(ApiError::bad(format!("{what} is too long (max {max} characters)")));
    }
    if t.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad(format!("{what} contains invalid characters")));
    }
    Ok(t.to_string())
}

pub fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !e.contains(char::is_whitespace) && e.len() <= 254
}

pub async fn update_profile(State(state): State<S>, AuthAny(user): AuthAny, Json(b): Json<ProfileBody>) -> ApiResult<Json<Value>> {
    let name = clean(&b.name, 80, "Name")?;
    let title = clean(&b.title, 80, "Job title")?;
    let phone = clean(&b.phone, 32, "Phone")?;
    if !phone.is_empty() && !phone.chars().all(|c| c.is_ascii_digit() || " +-().".contains(c)) {
        return Err(ApiError::bad("Phone may only contain digits, spaces and + - ( ) ."));
    }
    let new_email = match b.email.as_deref().map(|e| e.trim().to_lowercase()) {
        Some(e) if e != user.email.to_lowercase() => {
            if !valid_email(&e) {
                return Err(ApiError::bad("Enter a valid email address"));
            }
            // Changing the sign-in address needs the current password.
            let pw = b.current_password.clone().unwrap_or_default();
            let uid = user.id;
            let hash: String = state.db.run(move |c| Ok(c.query_row("SELECT password_hash FROM users WHERE id = ?1", [uid], |r| r.get(0))?)).await?;
            if pw.is_empty() || !tokio::task::spawn_blocking(move || verify_password(&pw, &hash)).await.unwrap_or(false) {
                state.throttle.fail(&format!("user:{}", user.email), 5, 900, 300);
                return Err(ApiError::bad("Current password is incorrect"));
            }
            Some(e)
        }
        _ => None,
    };
    let uid = user.id;
    let ne = new_email.clone();
    let taken = state
        .db
        .run(move |c| {
            if let Some(e) = &ne {
                let other: Option<i64> = c.query_row("SELECT id FROM users WHERE email = ?1 AND id <> ?2", params![e, uid], |r| r.get(0)).optional()?;
                if other.is_some() {
                    return Ok(true);
                }
                c.execute("UPDATE users SET email = ?1 WHERE id = ?2", params![e, uid])?;
            }
            c.execute("UPDATE users SET name = ?1, title = ?2, phone = ?3 WHERE id = ?4", params![name, title, phone, uid])?;
            Ok(false)
        })
        .await?;
    if taken {
        return Err(ApiError::bad("Another user already has that email address"));
    }
    let detail = new_email.as_ref().map(|e| format!("email changed from {} to {e}", user.email));
    audit(&state, Some(&user), "", AuditEntry { action: "profile_updated", server: None, target: None, detail, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

/// Identify the image by its bytes (never trust the client's content type). SVG is refused on purpose.
pub fn sniff_image(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

pub async fn put_avatar(State(state): State<S>, AuthAny(user): AuthAny, body: Bytes) -> ApiResult<Json<Value>> {
    if body.len() > MAX_AVATAR {
        return Err(ApiError::bad("The photo is too large (max 512 KB)"));
    }
    let Some(kind) = sniff_image(&body) else {
        return Err(ApiError::bad("Upload a PNG, JPEG or WebP image"));
    };
    let uid = user.id;
    let data = body.to_vec();
    let v = now_ms();
    state
        .db
        .run(move |c| {
            c.execute("UPDATE users SET avatar = ?1, avatar_type = ?2, avatar_version = ?3 WHERE id = ?4", params![data, kind, v, uid])?;
            Ok(())
        })
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "profile_photo_changed", server: None, target: None, detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true, "avatar": crate::auth::avatar_url(uid, v) })))
}

pub async fn delete_avatar(State(state): State<S>, AuthAny(user): AuthAny) -> ApiResult<Json<Value>> {
    let uid = user.id;
    state
        .db
        .run(move |c| {
            c.execute("UPDATE users SET avatar = NULL, avatar_type = '', avatar_version = 0 WHERE id = ?1", [uid])?;
            Ok(())
        })
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "profile_photo_removed", server: None, target: None, detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

/// Any signed-in user may see profile photos (they appear in the audit log, user lists, …).
pub async fn get_avatar(State(state): State<S>, AuthAny(_user): AuthAny, Path(id): Path<i64>) -> ApiResult<Response> {
    let row: Option<(Vec<u8>, String)> = state
        .db
        .run(move |c| {
            Ok(c.query_row("SELECT avatar, avatar_type FROM users WHERE id = ?1 AND avatar IS NOT NULL", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?)
        })
        .await?;
    let Some((data, kind)) = row else { return Err(ApiError::not_found("No photo")) };
    let mut res = (StatusCode::OK, data).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&kind).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    // URLs carry ?v=<version>, so the browser may keep them.
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; sandbox"));
    Ok(res)
}

/// Sessions are identified to the browser by a prefix of their token hash (never the token).
fn session_key(token_hash: &str) -> String {
    token_hash.chars().take(16).collect()
}

pub async fn list_sessions(State(state): State<S>, AuthAny(user): AuthAny) -> ApiResult<Json<Value>> {
    let uid = user.id;
    let me = user.token_hash.clone();
    let rows = state
        .db
        .run(move |c| {
            let mut st = c.prepare(
                "SELECT token_hash, created_at, last_seen_at, ip, user_agent FROM sessions WHERE user_id = ?1 AND expires_at > ?2 ORDER BY last_seen_at DESC",
            )?;
            let v = st
                .query_map(params![uid, now_ms()], |r| {
                    let th: String = r.get(0)?;
                    Ok(json!({
                        "id": session_key(&th),
                        "current": th == me,
                        "created_at": r.get::<_, i64>(1)?,
                        "last_seen_at": r.get::<_, i64>(2)?,
                        "ip": r.get::<_, String>(3)?,
                        "user_agent": r.get::<_, String>(4)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .await?;
    Ok(Json(Value::Array(rows)))
}

pub async fn revoke_session(State(state): State<S>, user: AuthUser, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    if key.len() != 16 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::bad("Invalid session"));
    }
    if session_key(&user.token_hash) == key {
        return Err(ApiError::bad("Use Sign out to end this session"));
    }
    let uid = user.id;
    let k = key.clone();
    let n = state
        .db
        .run(move |c| Ok(c.execute("DELETE FROM sessions WHERE user_id = ?1 AND substr(token_hash, 1, 16) = ?2", params![uid, k])?))
        .await?;
    if n == 0 {
        return Err(ApiError::not_found("Session not found"));
    }
    audit(&state, Some(&user), "", AuditEntry { action: "session_revoked", server: None, target: Some(key), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn revoke_other_sessions(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    let uid = user.id;
    let keep = user.token_hash.clone();
    let n = state
        .db
        .run(move |c| Ok(c.execute("DELETE FROM sessions WHERE user_id = ?1 AND token_hash <> ?2", params![uid, keep])?))
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "sessions_revoked", server: None, target: None, detail: Some(format!("{n} other session(s)")), ok: true }).await;
    Ok(Json(json!({ "ok": true, "revoked": n })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_only_raster_images() {
        assert_eq!(sniff_image(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]), Some("image/png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"RIFF\x10\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg xmlns='http://www.w3.org/2000/svg'/>"), None);
        assert_eq!(sniff_image(b"GIF89a"), None);
    }

    #[test]
    fn email_validation() {
        assert!(valid_email("a.b@skienlab.com"));
        assert!(!valid_email("nope"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.com"));
    }
}
