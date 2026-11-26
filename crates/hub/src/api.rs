//! Browser-facing REST + WebSocket API.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::SinkExt;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use monitorlab_common::{AgentHello, MetricsSample, Request, StreamRequest};

use crate::auth::{audit, hash_password, validate_password, AuditEntry, AuthUser};
use crate::rbac;
use crate::registry::{AgentConn, StreamEvent};
use crate::state::{ApiError, ApiResult, S};
use crate::util::{now_ms, random_token, sha256_hex};

fn some_value<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

// =================================================================== servers

#[derive(Debug, Clone)]
struct AgentRow {
    id: String,
    name: String,
    hello: Option<AgentHello>,
    enrolled: bool,
    token_pending: bool,
    created_at: i64,
    last_seen_at: Option<i64>,
    is_local: bool,
}

async fn load_agents(state: &S, only: Option<String>) -> ApiResult<Vec<AgentRow>> {
    Ok(state
        .db
        .run(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, name, hello, secret_hash IS NOT NULL, \
                 (enroll_token_hash IS NOT NULL AND enroll_expires_at > ?1), created_at, last_seen_at, is_local \
                 FROM agents WHERE (?2 IS NULL OR id = ?2) ORDER BY is_local DESC, name COLLATE NOCASE",
            )?;
            let rows = stmt
                .query_map(params![now_ms(), only], |r| {
                    let hello: Option<String> = r.get(2)?;
                    Ok(AgentRow {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        hello: hello.and_then(|h| serde_json::from_str(&h).ok()),
                        enrolled: r.get(3)?,
                        token_pending: r.get(4)?,
                        created_at: r.get(5)?,
                        last_seen_at: r.get(6)?,
                        is_local: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?)
}

/// Health of the latest sample using the configured alert thresholds (instant values).
fn health(m: &MetricsSample, rules: &crate::alerts::AlertRules) -> (&'static str, Vec<String>) {
    let issues = crate::alerts::evaluate(m, rules, Some(m.cpu as f64), Some(if m.mem_total > 0 { m.mem_used as f64 * 100.0 / m.mem_total as f64 } else { 0.0 }));
    let crit = issues.iter().any(|i| i.1 == crate::alerts::Severity::Critical);
    let reasons: Vec<String> = issues.into_iter().map(|i| i.2).collect();
    (if crit { "critical" } else if reasons.is_empty() { "ok" } else { "warning" }, reasons)
}

fn server_json(row: &AgentRow, conn: Option<Arc<AgentConn>>, full: bool, rules: &crate::alerts::AlertRules) -> Value {
    let online = conn.is_some();
    let hello = conn.as_ref().and_then(|c| c.hello.read().unwrap().clone()).or_else(|| row.hello.clone());
    let latest = conn.as_ref().and_then(|c| c.latest.read().unwrap().clone());
    let (status, reasons) = if !row.enrolled {
        ("pending", vec![])
    } else if !online {
        ("offline", vec![])
    } else if let Some(m) = &latest {
        let (s, r) = health(m, rules);
        (s, r)
    } else {
        ("ok", vec![])
    };
    let mut latest_v = latest.as_ref().map(|m| serde_json::to_value(m).unwrap_or(Value::Null)).unwrap_or(Value::Null);
    if !full {
        if let Value::Object(o) = &mut latest_v {
            o.remove("cpu_cores");
        }
    }
    json!({
        "id": row.id,
        "name": if row.name.is_empty() { hello.as_ref().map(|h| h.hostname.clone()).unwrap_or_else(|| "New server".into()) } else { row.name.clone() },
        "is_local": row.is_local,
        "enrolled": row.enrolled,
        "token_pending": row.token_pending,
        "online": online,
        "status": status,
        "reasons": reasons,
        "remote": conn.as_ref().map(|c| c.remote.clone()),
        "connected_at": conn.as_ref().map(|c| c.connected_at),
        "created_at": row.created_at,
        "last_seen_at": if online { Some(now_ms()) } else { row.last_seen_at },
        "hello": hello,
        "latest": latest_v,
    })
}

async fn get_server_row(state: &S, user: &AuthUser, id: &str) -> ApiResult<AgentRow> {
    user.require(rbac::SERVERS_VIEW)?;
    if !user.can_server(id) {
        return Err(ApiError::not_found("Server not found"));
    }
    load_agents(state, Some(id.to_string())).await?.into_iter().next().ok_or_else(|| ApiError::not_found("Server not found"))
}

pub async fn list_servers(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_VIEW)?;
    let rows = load_agents(&state, None).await?;
    let rules = state.rules.read().unwrap().clone();
    let overrides = crate::alerts::load_overrides(&state).await;
    let windows = crate::alerts::active_maintenance(&state).await;
    let list: Vec<Value> = rows
        .iter()
        .filter(|r| user.can_server(&r.id))
        .map(|r| {
            let mut v = server_json(r, state.reg.get(&r.id), false, overrides.get(&r.id).unwrap_or(&rules));
            decorate(&mut v, &r.id, &windows, overrides.contains_key(&r.id));
            v
        })
        .collect();
    Ok(Json(Value::Array(list)))
}

pub async fn get_server(State(state): State<S>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let row = get_server_row(&state, &user, &id).await?;
    let rules = state.rules.read().unwrap().clone();
    let overrides = crate::alerts::load_overrides(&state).await;
    let windows = crate::alerts::active_maintenance(&state).await;
    let mut v = server_json(&row, state.reg.get(&id), true, overrides.get(&id).unwrap_or(&rules));
    decorate(&mut v, &id, &windows, overrides.contains_key(&id));
    Ok(Json(v))
}

/// Maintenance, custom-rule and agent-update details added to a server's JSON.
fn decorate(v: &mut Value, id: &str, windows: &[(Option<String>, i64)], custom_rules: bool) {
    v["maintenance_until"] = json!(crate::alerts::maintenance_until(windows, id));
    v["custom_alert_rules"] = json!(custom_rules);
    v["agent_update"] = crate::updates::status(id);
    let agent_version = v["hello"]["agent_version"].as_str().unwrap_or("").to_string();
    v["update_available"] = json!(!agent_version.is_empty() && crate::updates::newer(monitorlab_common::VERSION, &agent_version) && !v["is_local"].as_bool().unwrap_or(false));
}

#[derive(Deserialize)]
pub struct MetricsQuery {
    #[serde(default = "d_range")]
    range: String,
}
fn d_range() -> String {
    "1h".into()
}

pub async fn server_metrics(State(state): State<S>, user: AuthUser, Path(id): Path<String>, Query(q): Query<MetricsQuery>) -> ApiResult<Json<Value>> {
    let row = get_server_row(&state, &user, &id).await?;
    let interval = row.hello.as_ref().map(|h| h.interval_secs).unwrap_or(10);
    Ok(Json(crate::metrics::query(&state.db, id, q.range, interval).await?))
}

pub async fn server_rpc(State(state): State<S>, user: AuthUser, Path(id): Path<String>, Json(req): Json<Request>) -> ApiResult<Json<Value>> {
    let row = get_server_row(&state, &user, &id).await?;
    user.require(rbac::for_request(&req))?;
    let conn = state.reg.get(&id).ok_or_else(|| ApiError::unavailable("Server is offline"))?;
    let mutating = rbac::is_mutating(&req);
    let detail = mutating.then(|| serde_json::to_string(&req).unwrap_or_default());
    let name = req.name();
    let timeout = Duration::from_secs(rbac::timeout_secs(&req));
    let res = conn.rpc(req, timeout).await;
    if mutating {
        audit(
            &state,
            Some(&user),
            "",
            AuditEntry {
                action: name,
                server: Some((&row.id, &row.name)),
                target: None,
                detail: match &res {
                    Ok(_) => detail,
                    Err(e) => Some(format!("{} — error: {e}", detail.unwrap_or_default())),
                },
                ok: res.is_ok(),
            },
        )
        .await;
    }
    res.map(Json).map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))
}

// ------------------------------------------------------------------ browser streams (WebSocket)

/// Reject cross-site requests / WebSocket hijacking: the Origin must match the Host the browser
/// used (or the forwarded host behind a trusted proxy, or MONITORLAB_PUBLIC_URL).
pub fn same_origin(state: &S, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else { return false };
    let mut hosts: Vec<&str> = Vec::new();
    if let Some(h) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        hosts.push(h);
    }
    if state.cfg.trust_proxy {
        if let Some(h) = headers.get("x-forwarded-host").and_then(|v| v.to_str().ok()) {
            hosts.push(h);
        }
    }
    origin_matches(origin, &hosts, state.cfg.public_url.as_deref())
}

/// "host[:port]" lower-cased with the scheme's default port made explicit.
fn authority(host: &str, scheme: &str) -> String {
    let h = host.trim().trim_end_matches('/').to_ascii_lowercase();
    let has_port = if h.starts_with('[') { h.contains("]:") } else { h.contains(':') };
    if has_port {
        h
    } else {
        format!("{h}:{}", if scheme == "https" { 443 } else { 80 })
    }
}

pub fn origin_matches(origin: &str, hosts: &[&str], public_url: Option<&str>) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else { return false };
    let scheme = scheme.to_ascii_lowercase();
    let o = authority(rest, &scheme);
    // A Host header without a port means the default port of the scheme the browser used.
    if hosts.iter().any(|h| authority(h, &scheme) == o) {
        return true;
    }
    if let Some((ps, prest)) = public_url.and_then(|u| u.split_once("://")) {
        let ps = ps.to_ascii_lowercase();
        let pauth = authority(prest.split('/').next().unwrap_or(""), &ps);
        return ps == scheme && pauth == o;
    }
    false
}

#[cfg(test)]
mod origin_tests {
    use super::origin_matches;

    #[test]
    fn origins() {
        // direct access
        assert!(origin_matches("https://127.0.0.1:8443", &["127.0.0.1:8443"], None));
        assert!(!origin_matches("https://evil.example", &["127.0.0.1:8443"], None));
        assert!(!origin_matches("https://127.0.0.1:9999", &["127.0.0.1:8443"], None));
        // behind a proxy that forwards Host without the default port (Nginx Proxy Manager)
        assert!(origin_matches("https://monitorlab.skienlab.com", &["monitorlab.skienlab.com"], None));
        assert!(origin_matches("https://MonitorLab.skienlab.com", &["monitorlab.skienlab.com:443"], None));
        // proxy rewrote Host to the upstream; public URL vouches for the origin
        assert!(origin_matches("https://monitorlab.skienlab.com", &["monitorlab:8090"], Some("https://monitorlab.skienlab.com/")));
        assert!(!origin_matches("http://monitorlab.skienlab.com", &["monitorlab:8090"], Some("https://monitorlab.skienlab.com")));
        assert!(!origin_matches("https://evil.example", &["monitorlab:8090"], Some("https://monitorlab.skienlab.com")));
        assert!(!origin_matches("null", &["monitorlab:8090"], None));
        assert!(origin_matches("http://[::1]:8080", &["[::1]:8080"], None));
    }
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ClientMsg {
    Input { b64: String },
    Resize { cols: u16, rows: u16 },
}

pub async fn server_stream(
    State(state): State<S>,
    user: AuthUser,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !same_origin(&state, &headers) {
        return ApiError::forbidden("cross-origin WebSocket rejected").into_response();
    }
    let row = match get_server_row(&state, &user, &id).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(conn) = state.reg.get(&id) else {
        return ApiError::unavailable("Server is offline").into_response();
    };
    ws.max_message_size(1024 * 1024).on_upgrade(move |socket| browser_stream(state, user, row, conn, socket))
}

async fn send_json(socket: &mut WebSocket, v: Value) -> bool {
    socket.send(Message::Text(v.to_string().into())).await.is_ok()
}

async fn browser_stream(state: S, user: AuthUser, row: AgentRow, conn: Arc<AgentConn>, mut socket: WebSocket) {
    let first = tokio::time::timeout(Duration::from_secs(15), socket.recv()).await;
    let Ok(Some(Ok(Message::Text(t)))) = first else { return };
    let req: StreamRequest = match serde_json::from_str(&t) {
        Ok(r) => r,
        Err(e) => {
            send_json(&mut socket, json!({ "t": "end", "error": format!("bad request: {e}") })).await;
            let _ = socket.close().await;
            return;
        }
    };
    let perm = rbac::for_stream(&req);
    if !user.has(perm) {
        send_json(&mut socket, json!({ "t": "end", "error": format!("Missing permission: {perm}") })).await;
        let _ = socket.close().await;
        return;
    }
    // Interactive sessions: keystrokes are forwarded, and every session is audited.
    let exec_audit = match &req {
        StreamRequest::ContainerExec { id, cmd, .. } => Some(("container_exec", format!("{id} {}", cmd.as_ref().map(|c| c.join(" ")).unwrap_or_default()))),
        StreamRequest::HostShell { shell, .. } => Some(("host_shell", shell.clone().unwrap_or_else(|| "auto".into()))),
        StreamRequest::K8sPodExec { namespace, name, container, cmd, .. } => Some((
            "pod_exec",
            format!("{namespace}/{name}{} {}", container.as_ref().map(|c| format!(" [{c}]")).unwrap_or_default(), cmd.as_ref().map(|c| c.join(" ")).unwrap_or_default()),
        )),
        _ => None,
    };
    let is_exec = exec_audit.is_some();
    if let Some((action, target)) = exec_audit {
        audit(&state, Some(&user), "", AuditEntry { action, server: Some((&row.id, &row.name)), target: Some(target.trim().to_string()), detail: None, ok: true }).await;
    }
    let mut h = match conn.open_stream(req).await {
        Ok(h) => h,
        Err(e) => {
            send_json(&mut socket, json!({ "t": "end", "error": e.to_string() })).await;
            let _ = socket.close().await;
            return;
        }
    };
    let mut keepalive = tokio::time::interval(Duration::from_secs(25));
    keepalive.tick().await;
    loop {
        tokio::select! {
            ev = h.recv() => match ev {
                Some(StreamEvent::Data(d)) => {
                    if !send_json(&mut socket, json!({ "t": "data", "d": d })).await { break; }
                }
                Some(StreamEvent::End(error)) => {
                    send_json(&mut socket, json!({ "t": "end", "error": error })).await;
                    break;
                }
                None => {
                    send_json(&mut socket, json!({ "t": "end", "error": "stream closed" })).await;
                    break;
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) if is_exec => {
                    match serde_json::from_str::<ClientMsg>(&t) {
                        Ok(ClientMsg::Input { b64 }) => { if b64.len() <= 64 * 1024 { h.input(b64).await; } }
                        Ok(ClientMsg::Resize { cols, rows }) => { h.resize(cols.clamp(10, 500), rows.clamp(5, 300)).await; }
                        Err(_) => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            _ = keepalive.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    let _ = socket.close().await;
    drop(h);
}

// ------------------------------------------------------------------ server administration

#[derive(Deserialize)]
pub struct NameBody {
    name: String,
}

fn clean_name(n: &str) -> ApiResult<String> {
    let n = n.trim();
    if n.is_empty() || n.chars().count() > 64 {
        return Err(ApiError::bad("Name must be 1–64 characters"));
    }
    Ok(n.to_string())
}

fn token_response(id: &str, name: &str, token: &str, expires: i64) -> Value {
    json!({ "id": id, "name": name, "token": token, "expires_at": expires })
}

pub async fn create_server(State(state): State<S>, user: AuthUser, Json(b): Json<NameBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_MANAGE)?;
    let name = clean_name(&b.name)?;
    let id = uuid::Uuid::new_v4().to_string();
    let token = random_token(24);
    let expires = now_ms() + 24 * 3_600_000;
    let (id2, name2, th) = (id.clone(), name.clone(), sha256_hex(&token));
    state
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO agents (id, name, enroll_token_hash, enroll_expires_at, created_at) VALUES (?1,?2,?3,?4,?5)",
                params![id2, name2, th, expires, now_ms()],
            )?;
            Ok(())
        })
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "server_added", server: Some((&id, &name)), target: None, detail: None, ok: true }).await;
    Ok(Json(token_response(&id, &name, &token, expires)))
}

pub async fn new_enroll_token(State(state): State<S>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_MANAGE)?;
    let row = get_server_row(&state, &user, &id).await?;
    if row.is_local {
        return Err(ApiError::bad("The hub's own agent does not need enrollment"));
    }
    let token = random_token(24);
    let expires = now_ms() + 24 * 3_600_000;
    let (id2, th) = (id.clone(), sha256_hex(&token));
    state
        .db
        .run(move |c| Ok(c.execute("UPDATE agents SET enroll_token_hash = ?1, enroll_expires_at = ?2 WHERE id = ?3", params![th, expires, id2])?))
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "server_token_created", server: Some((&id, &row.name)), target: None, detail: None, ok: true }).await;
    Ok(Json(token_response(&id, &row.name, &token, expires)))
}

pub async fn rename_server(State(state): State<S>, user: AuthUser, Path(id): Path<String>, Json(b): Json<NameBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_MANAGE)?;
    let row = get_server_row(&state, &user, &id).await?;
    let name = clean_name(&b.name)?;
    let (id2, n2) = (id.clone(), name.clone());
    state.db.run(move |c| Ok(c.execute("UPDATE agents SET name = ?1 WHERE id = ?2", params![n2, id2])?)).await?;
    audit(&state, Some(&user), "", AuditEntry { action: "server_renamed", server: Some((&id, &name)), target: Some(row.name), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_server(State(state): State<S>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_MANAGE)?;
    let row = get_server_row(&state, &user, &id).await?;
    if row.is_local {
        return Err(ApiError::bad("The hub's own server cannot be removed (set MONITORLAB_LOCAL_AGENT=false to disable it)"));
    }
    let id2 = id.clone();
    state
        .db
        .run(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM metrics WHERE agent_id = ?1", [&id2])?;
            tx.execute("DELETE FROM metrics_5m WHERE agent_id = ?1", [&id2])?;
            tx.execute("DELETE FROM alerts WHERE agent_id = ?1", [&id2])?;
            tx.execute("DELETE FROM agents WHERE id = ?1", [&id2])?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    state.reg.disconnect(&id);
    audit(&state, Some(&user), "", AuditEntry { action: "server_removed", server: Some((&id, &row.name)), target: None, detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn hub_info(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    let _ = user;
    let has = |f: &str| crate::updates::agent_path(&state, f).is_file();
    Ok(Json(json!({
        "version": monitorlab_common::VERSION,
        "tls": state.tls.is_some(),
        "self_signed": state.tls.as_ref().map(|t| t.self_signed).unwrap_or(false),
        "fingerprint": state.tls.as_ref().map(|t| t.fingerprint.clone()),
        "spki_pin": state.tls.as_ref().and_then(|t| t.spki_pin.clone()),
        "public_url": state.cfg.public_url,
        "downloads": {
            "linux_amd64": has("monitorlab-agent-linux-amd64"),
            "linux_arm64": has("monitorlab-agent-linux-arm64"),
            "windows_amd64": has("monitorlab-agent-windows-amd64.exe"),
            "macos_arm64": has("monitorlab-agent-macos-arm64"),
            "macos_amd64": has("monitorlab-agent-macos-amd64"),
        }
    })))
}

const INSTALL_SH: &str = include_str!("../../../deploy/install.sh");

/// Public downloads (agent binaries carry no secrets). Strict allow-list of file names.
pub async fn download(State(state): State<S>, Path(file): Path<String>) -> Response {
    if file == "install.sh" {
        return ([(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")], INSTALL_SH).into_response();
    }
    const ALLOWED: &[&str] = &[
        "monitorlab-agent-linux-amd64",
        "monitorlab-agent-linux-arm64",
        "monitorlab-agent-windows-amd64.exe",
        "monitorlab-agent-macos-arm64",
        "monitorlab-agent-macos-amd64",
    ];
    if !ALLOWED.contains(&file.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = crate::updates::agent_path(&state, &file);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}\"")),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "agent binary not bundled with this hub").into_response(),
    }
}

// =================================================================== users & roles

fn perms_of(json_text: &str) -> Vec<String> {
    serde_json::from_str(json_text).unwrap_or_default()
}

/// Active users (other than `exclude`) whose role can manage users.
fn admin_count(c: &rusqlite::Connection, exclude: i64) -> rusqlite::Result<i64> {
    let mut stmt = c.prepare("SELECT r.permissions FROM users u JOIN roles r ON r.id = u.role_id WHERE u.disabled = 0 AND u.id <> ?1")?;
    let rows = stmt.query_map([exclude], |r| r.get::<_, String>(0))?;
    let mut n = 0;
    for p in rows {
        if perms_of(&p?).iter().any(|x| x == rbac::USERS_MANAGE) {
            n += 1;
        }
    }
    Ok(n)
}

fn role_perms(c: &rusqlite::Connection, role_id: i64) -> rusqlite::Result<Option<Vec<String>>> {
    c.query_row("SELECT permissions FROM roles WHERE id = ?1", [role_id], |r| r.get::<_, String>(0))
        .optional()
        .map(|o| o.map(|p| perms_of(&p)))
}

pub async fn list_users(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let v = state
        .db
        .run(|c| {
            let mut stmt = c.prepare(
                "SELECT u.id, u.email, u.name, u.role_id, r.name, u.server_scope, u.disabled, u.must_change_password, u.created_at, u.last_login_at, \
                 (SELECT COUNT(*) FROM sessions s WHERE s.user_id = u.id AND s.expires_at > ?1), u.avatar_version, u.title \
                 FROM users u JOIN roles r ON r.id = u.role_id ORDER BY u.email",
            )?;
            let rows = stmt
                .query_map([now_ms()], |r| {
                    let scope: Option<String> = r.get(5)?;
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "email": r.get::<_, String>(1)?,
                        "name": r.get::<_, String>(2)?,
                        "role_id": r.get::<_, i64>(3)?,
                        "role": r.get::<_, String>(4)?,
                        "server_scope": scope.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                        "disabled": r.get::<_, bool>(6)?,
                        "must_change_password": r.get::<_, bool>(7)?,
                        "created_at": r.get::<_, i64>(8)?,
                        "last_login_at": r.get::<_, Option<i64>>(9)?,
                        "sessions": r.get::<_, i64>(10)?,
                        "avatar": crate::auth::avatar_url(r.get::<_, i64>(0)?, r.get::<_, i64>(11)?),
                        "title": r.get::<_, String>(12)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(rows))
        })
        .await?;
    Ok(Json(v))
}

fn validate_email(e: &str) -> ApiResult<String> {
    let e = e.trim().to_lowercase();
    let ok = e.len() <= 254 && e.split_once('@').map(|(a, b)| !a.is_empty() && !b.is_empty() && !b.starts_with('.')).unwrap_or(false) && !e.contains(char::is_whitespace);
    if ok {
        Ok(e)
    } else {
        Err(ApiError::bad("Enter a valid email address"))
    }
}

fn scope_to_db(v: &Value) -> ApiResult<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::Array(a) => {
            let ids: Vec<String> = a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
            if ids.len() != a.len() {
                return Err(ApiError::bad("server_scope must be a list of server ids or null"));
            }
            Ok(Some(serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into())))
        }
        _ => Err(ApiError::bad("server_scope must be a list of server ids or null")),
    }
}

#[derive(Deserialize)]
pub struct CreateUser {
    email: String,
    #[serde(default)]
    name: String,
    password: String,
    role_id: i64,
    #[serde(default)]
    server_scope: Value,
    #[serde(default = "d_true")]
    must_change_password: bool,
}
fn d_true() -> bool {
    true
}

pub async fn create_user(State(state): State<S>, user: AuthUser, Json(b): Json<CreateUser>) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let email = validate_email(&b.email)?;
    validate_password(&b.password, &email)?;
    let scope = scope_to_db(&b.server_scope)?;
    let pw = b.password.clone();
    let hash = tokio::task::spawn_blocking(move || hash_password(&pw)).await.map_err(|e| anyhow::anyhow!("{e}"))??;
    let name: String = b.name.trim().chars().take(100).collect();
    let (e2, role_id, must) = (email.clone(), b.role_id, b.must_change_password);
    let id = state
        .db
        .run(move |c| {
            if role_perms(c, role_id)?.is_none() {
                return Ok(Err("Unknown role"));
            }
            let exists: Option<i64> = c.query_row("SELECT id FROM users WHERE email = ?1", [&e2], |r| r.get(0)).optional()?;
            if exists.is_some() {
                return Ok(Err("A user with this email already exists"));
            }
            c.execute(
                "INSERT INTO users (email, name, password_hash, role_id, server_scope, must_change_password, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![e2, name, hash, role_id, scope, must, now_ms()],
            )?;
            Ok(Ok(c.last_insert_rowid()))
        })
        .await?
        .map_err(ApiError::bad)?;
    audit(&state, Some(&user), "", AuditEntry { action: "user_created", server: None, target: Some(email), detail: None, ok: true }).await;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct UpdateUser {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role_id: Option<i64>,
    #[serde(default, deserialize_with = "some_value")]
    server_scope: Option<Value>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    must_change_password: Option<bool>,
}

pub async fn update_user(State(state): State<S>, user: AuthUser, Path(uid): Path<i64>, Json(b): Json<UpdateUser>) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let is_self = uid == user.id;
    if is_self && (b.disabled == Some(true) || b.role_id.is_some_and(|r| r != user.role_id) || b.server_scope.as_ref().is_some_and(|v| !v.is_null())) {
        return Err(ApiError::bad("You cannot disable yourself, change your own role or restrict your own server access"));
    }
    let scope = match &b.server_scope {
        Some(v) => Some(scope_to_db(v)?),
        None => None,
    };
    let new_hash = match &b.password {
        Some(pw) if !pw.is_empty() => {
            let email: String = {
                let id = uid;
                state.db.run(move |c| Ok(c.query_row("SELECT email FROM users WHERE id = ?1", [id], |r| r.get(0)).optional()?)).await?
            }
            .ok_or_else(|| ApiError::not_found("User not found"))?;
            validate_password(pw, &email)?;
            let pw = pw.clone();
            Some(tokio::task::spawn_blocking(move || hash_password(&pw)).await.map_err(|e| anyhow::anyhow!("{e}"))??)
        }
        _ => None,
    };
    let target_email = state
        .db
        .run(move |c| {
            let tx = c.transaction()?;
            let cur: Option<(String, i64, bool)> = tx
                .query_row("SELECT email, role_id, disabled FROM users WHERE id = ?1", [uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            let Some((email, cur_role, _)) = cur else { return Ok(Err("User not found".to_string())) };
            let cur_is_admin = role_perms(&tx, cur_role)?.unwrap_or_default().iter().any(|p| p == rbac::USERS_MANAGE);
            let new_role = b.role_id.unwrap_or(cur_role);
            let Some(new_perms) = role_perms(&tx, new_role)? else { return Ok(Err("Unknown role".to_string())) };
            let stays_admin = new_perms.iter().any(|p| p == rbac::USERS_MANAGE) && b.disabled != Some(true);
            if cur_is_admin && !stays_admin && admin_count(&tx, uid)? == 0 {
                return Ok(Err("This is the last administrator; keep at least one active user who can manage users".to_string()));
            }
            if let Some(n) = &b.name {
                tx.execute("UPDATE users SET name = ?1 WHERE id = ?2", params![n.trim().chars().take(100).collect::<String>(), uid])?;
            }
            if b.role_id.is_some() {
                tx.execute("UPDATE users SET role_id = ?1 WHERE id = ?2", params![new_role, uid])?;
            }
            if let Some(s) = &scope {
                tx.execute("UPDATE users SET server_scope = ?1 WHERE id = ?2", params![s, uid])?;
            }
            if let Some(d) = b.disabled {
                tx.execute("UPDATE users SET disabled = ?1 WHERE id = ?2", params![d, uid])?;
                if d {
                    tx.execute("DELETE FROM sessions WHERE user_id = ?1", [uid])?;
                }
            }
            if let Some(h) = &new_hash {
                let must = b.must_change_password.unwrap_or(true);
                tx.execute("UPDATE users SET password_hash = ?1, must_change_password = ?2 WHERE id = ?3", params![h, must, uid])?;
                tx.execute("DELETE FROM sessions WHERE user_id = ?1", [uid])?;
            } else if let Some(m) = b.must_change_password {
                tx.execute("UPDATE users SET must_change_password = ?1 WHERE id = ?2", params![m, uid])?;
            }
            tx.commit()?;
            Ok(Ok(email))
        })
        .await?
        .map_err(ApiError::bad)?;
    audit(&state, Some(&user), "", AuditEntry { action: "user_updated", server: None, target: Some(target_email), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_user(State(state): State<S>, user: AuthUser, Path(uid): Path<i64>) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    if uid == user.id {
        return Err(ApiError::bad("You cannot delete your own account"));
    }
    let email = state
        .db
        .run(move |c| {
            let row: Option<(String, i64)> =
                c.query_row("SELECT email, role_id FROM users WHERE id = ?1", [uid], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let Some((email, role)) = row else { return Ok(Err("User not found")) };
            let is_admin = role_perms(c, role)?.unwrap_or_default().iter().any(|p| p == rbac::USERS_MANAGE);
            if is_admin && admin_count(c, uid)? == 0 {
                return Ok(Err("Cannot delete the last administrator"));
            }
            c.execute("DELETE FROM sessions WHERE user_id = ?1", [uid])?;
            c.execute("DELETE FROM users WHERE id = ?1", [uid])?;
            Ok(Ok(email))
        })
        .await?
        .map_err(ApiError::bad)?;
    audit(&state, Some(&user), "", AuditEntry { action: "user_deleted", server: None, target: Some(email), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_roles(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let v = state
        .db
        .run(|c| {
            let mut stmt = c.prepare(
                "SELECT r.id, r.name, r.description, r.permissions, r.builtin, (SELECT COUNT(*) FROM users u WHERE u.role_id = r.id) \
                 FROM roles r ORDER BY r.builtin DESC, r.id",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    let name: String = r.get(1)?;
                    let builtin: bool = r.get(4)?;
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "name": name,
                        "description": r.get::<_, String>(2)?,
                        "permissions": perms_of(&r.get::<_, String>(3)?),
                        "builtin": builtin,
                        "locked": builtin && name == "Administrator",
                        "users": r.get::<_, i64>(5)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(rows))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct RoleBody {
    name: String,
    #[serde(default)]
    description: String,
    permissions: Vec<String>,
}

fn validate_role(b: &RoleBody) -> ApiResult<(String, String, String)> {
    let name = clean_name(&b.name)?;
    if let Some(bad) = b.permissions.iter().find(|p| !rbac::is_known(p)) {
        return Err(ApiError::bad(format!("Unknown permission {bad}")));
    }
    let mut perms = b.permissions.clone();
    perms.sort();
    perms.dedup();
    Ok((name, b.description.trim().chars().take(300).collect(), serde_json::to_string(&perms).unwrap_or_else(|_| "[]".into())))
}

pub async fn create_role(State(state): State<S>, user: AuthUser, Json(b): Json<RoleBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let (name, desc, perms) = validate_role(&b)?;
    let n2 = name.clone();
    let id = state
        .db
        .run(move |c| {
            let exists: Option<i64> = c.query_row("SELECT id FROM roles WHERE name = ?1", [&n2], |r| r.get(0)).optional()?;
            if exists.is_some() {
                return Ok(Err("A role with this name already exists"));
            }
            c.execute("INSERT INTO roles (name, description, permissions) VALUES (?1,?2,?3)", params![n2, desc, perms])?;
            Ok(Ok(c.last_insert_rowid()))
        })
        .await?
        .map_err(ApiError::bad)?;
    audit(&state, Some(&user), "", AuditEntry { action: "role_created", server: None, target: Some(name), detail: Some(serde_json::to_string(&b.permissions).unwrap_or_default()), ok: true }).await;
    Ok(Json(json!({ "id": id })))
}

pub async fn update_role(State(state): State<S>, user: AuthUser, Path(rid): Path<i64>, Json(b): Json<RoleBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let (name, desc, perms) = validate_role(&b)?;
    let grants_admin = b.permissions.iter().any(|p| p == rbac::USERS_MANAGE);
    let n2 = name.clone();
    state
        .db
        .run(move |c| {
            let tx = c.transaction()?;
            let cur: Option<(String, bool)> = tx.query_row("SELECT name, builtin FROM roles WHERE id = ?1", [rid], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let Some((cur_name, builtin)) = cur else { return Ok(Err("Role not found")) };
            if builtin && cur_name == "Administrator" {
                return Ok(Err("The Administrator role cannot be modified"));
            }
            if builtin && cur_name != n2 {
                return Ok(Err("Built-in roles cannot be renamed"));
            }
            let clash: Option<i64> = tx.query_row("SELECT id FROM roles WHERE name = ?1 AND id <> ?2", params![n2, rid], |r| r.get(0)).optional()?;
            if clash.is_some() {
                return Ok(Err("A role with this name already exists"));
            }
            tx.execute("UPDATE roles SET name = ?1, description = ?2, permissions = ?3 WHERE id = ?4", params![n2, desc, perms, rid])?;
            if !grants_admin && admin_count(&tx, -1)? == 0 {
                return Ok(Err("This change would leave no active administrator"));
            }
            tx.commit()?;
            Ok(Ok(()))
        })
        .await?
        .map_err(ApiError::bad)?;
    audit(&state, Some(&user), "", AuditEntry { action: "role_updated", server: None, target: Some(name), detail: Some(serde_json::to_string(&b.permissions).unwrap_or_default()), ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_role(State(state): State<S>, user: AuthUser, Path(rid): Path<i64>) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let name = state
        .db
        .run(move |c| {
            let cur: Option<(String, bool)> = c.query_row("SELECT name, builtin FROM roles WHERE id = ?1", [rid], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let Some((name, builtin)) = cur else { return Ok(Err("Role not found")) };
            if builtin {
                return Ok(Err("Built-in roles cannot be deleted"));
            }
            let used: i64 = c.query_row("SELECT COUNT(*) FROM users WHERE role_id = ?1", [rid], |r| r.get(0))?;
            if used > 0 {
                return Ok(Err("Role is assigned to users; reassign them first"));
            }
            c.execute("DELETE FROM roles WHERE id = ?1", [rid])?;
            Ok(Ok(name))
        })
        .await?
        .map_err(ApiError::bad)?;
    audit(&state, Some(&user), "", AuditEntry { action: "role_deleted", server: None, target: Some(name), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn permissions(user: AuthUser) -> Json<Value> {
    let _ = user;
    Json(serde_json::to_value(rbac::CATALOG).unwrap_or(Value::Null))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    q: Option<String>,
}

pub async fn audit_log(State(state): State<S>, user: AuthUser, Query(q): Query<AuditQuery>) -> ApiResult<Json<Value>> {
    user.require(rbac::AUDIT_VIEW)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let before = q.before.unwrap_or(i64::MAX);
    let needle = q.q.map(|s| format!("%{}%", s.trim().replace(['%', '_'], ""))).filter(|s| s.len() > 2);
    let v = state
        .db
        .run(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, ts, user_email, ip, action, server_id, server_name, target, detail, ok FROM audit \
                 WHERE id < ?1 AND (?2 IS NULL OR user_email LIKE ?2 OR action LIKE ?2 OR server_name LIKE ?2 OR target LIKE ?2 OR ip LIKE ?2) \
                 ORDER BY id DESC LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![before, needle, limit], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "ts": r.get::<_, i64>(1)?,
                        "user": r.get::<_, String>(2)?,
                        "ip": r.get::<_, String>(3)?,
                        "action": r.get::<_, String>(4)?,
                        "server_id": r.get::<_, Option<String>>(5)?,
                        "server": r.get::<_, Option<String>>(6)?,
                        "target": r.get::<_, Option<String>>(7)?,
                        "detail": r.get::<_, Option<String>>(8)?,
                        "ok": r.get::<_, bool>(9)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(rows))
        })
        .await?;
    Ok(Json(v))
}

// =================================================================== branding

/// Build identifier baked into the image by CI (short commit SHA), if any.
pub fn build_id() -> Option<&'static str> {
    option_env!("MONITORLAB_BUILD").filter(|b| !b.is_empty())
}

/// Public: the sign-in page needs the product name before anyone signs in.
pub async fn get_branding(State(state): State<S>) -> Json<Value> {
    Json(json!({
        "name": state.brand(),
        "company": state.company(),
        "version": monitorlab_common::VERSION,
        "build": build_id(),
    }))
}

#[derive(Deserialize)]
pub struct BrandingBody {
    name: String,
    #[serde(default)]
    company: Option<String>,
}

fn clean_label(v: &str, what: &str, max: usize) -> ApiResult<String> {
    let v = v.trim().to_string();
    if v.is_empty() || v.chars().count() > max || v.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad(format!("{what} must be 1–{max} characters")));
    }
    Ok(v)
}

pub async fn put_branding(State(state): State<S>, user: AuthUser, Json(b): Json<BrandingBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::USERS_MANAGE)?;
    let name = clean_label(&b.name, "Name", 40)?;
    let company = match &b.company {
        Some(c) => Some(clean_label(c, "Company name", 60)?),
        None => None,
    };
    state.db.set_setting("brand_name", name.clone()).await?;
    let old = std::mem::replace(&mut *state.brand.write().unwrap(), name.clone());
    if let Some(c) = &company {
        state.db.set_setting("company_name", c.clone()).await?;
        *state.company.write().unwrap() = c.clone();
    }
    let detail = format!("was: {old}{}", company.as_ref().map(|c| format!("; company: {c}")).unwrap_or_default());
    audit(&state, Some(&user), "", AuditEntry { action: "branding_updated", server: None, target: Some(name), detail: Some(detail), ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}
