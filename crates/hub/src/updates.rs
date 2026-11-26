//! Agent self-update: when an agent older than the hub connects (and auto-update is on),
//! the hub asks it to download the bundled binary for its platform and restart into it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::Json;
use monitorlab_common::{AgentHello, OsFamily, Request, VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{audit, AuditEntry, AuthUser};
use crate::rbac;
use crate::state::{ApiError, ApiResult, S};
use crate::util::now_ms;

/// Per agent: (last attempt ms, outcome). Failed automatic attempts are retried hourly.
static STATUS: Mutex<Option<HashMap<String, (i64, Result<String, String>)>>> = Mutex::new(None);
static HASHES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn set_status(id: &str, r: Result<String, String>) {
    STATUS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id.to_string(), (now_ms(), r));
}

pub fn status(id: &str) -> Value {
    match STATUS.lock().unwrap().as_ref().and_then(|m| m.get(id).cloned()) {
        Some((at, Ok(v))) => json!({ "at": at, "ok": true, "message": v }),
        Some((at, Err(e))) => json!({ "at": at, "ok": false, "message": e }),
        None => Value::Null,
    }
}

/// `1.10.0` > `1.9.3`; non-numeric parts compare as 0.
pub fn newer(a: &str, b: &str) -> bool {
    let p = |v: &str| -> Vec<u64> { v.split(['.', '-', '+']).take(3).map(|x| x.parse().unwrap_or(0)).collect() };
    p(a) > p(b)
}

pub fn file_for(h: &AgentHello) -> Option<&'static str> {
    match (h.os, h.arch.as_str()) {
        (OsFamily::Linux, "x86_64") => Some("monitorlab-agent-linux-amd64"),
        (OsFamily::Linux, "aarch64") => Some("monitorlab-agent-linux-arm64"),
        (OsFamily::Windows, "x86_64") => Some("monitorlab-agent-windows-amd64.exe"),
        (OsFamily::MacOs, "aarch64") => Some("monitorlab-agent-macos-arm64"),
        (OsFamily::MacOs, "x86_64") => Some("monitorlab-agent-macos-amd64"),
        _ => None,
    }
}

async fn sha256_of(state: &S, file: &str) -> Option<String> {
    if let Some(h) = HASHES.lock().unwrap().as_ref().and_then(|m| m.get(file).cloned()) {
        return Some(h);
    }
    let bytes = tokio::fs::read(state.cfg.agents_dir.join(file)).await.ok()?;
    let h = hex::encode(Sha256::digest(&bytes));
    HASHES.lock().unwrap().get_or_insert_with(HashMap::new).insert(file.to_string(), h.clone());
    Some(h)
}

pub async fn auto_enabled(state: &S) -> bool {
    state.db.get_setting("agent_auto_update").await.ok().flatten().map(|v| v != "false").unwrap_or(true)
}

/// Ask one agent to update now. Returns a human-readable outcome.
pub async fn update_agent(state: &S, id: &str) -> Result<String, String> {
    let conn = state.reg.get(id).ok_or("server is offline")?;
    let hello = conn.hello.read().unwrap().clone().ok_or("agent has not said hello yet")?;
    if !hello.capabilities.self_update {
        return Err(if newer(VERSION, &hello.agent_version) {
            format!("agent {} cannot update itself; re-run the installer on the server once", hello.agent_version)
        } else {
            "this agent cannot be updated from the hub".into()
        });
    }
    if !newer(VERSION, &hello.agent_version) {
        return Ok(format!("already on {}", hello.agent_version));
    }
    let file = file_for(&hello).ok_or("no agent build for this platform")?;
    let sha = sha256_of(state, file).await.ok_or("agent binary is not bundled with this hub")?;
    let req = Request::SelfUpdate { file: file.into(), sha256: sha, version: VERSION.into() };
    match conn.rpc(req, Duration::from_secs(900)).await {
        Ok(_) => Ok(format!("updating {} → {VERSION}; the agent restarts in a few seconds", hello.agent_version)),
        Err(e) => Err(e.to_string()),
    }
}

/// Called for every agent hello.
pub fn on_hello(state: &S, agent_id: &str, h: &AgentHello) {
    if !h.capabilities.self_update || !newer(VERSION, &h.agent_version) {
        return;
    }
    let recent_failure = STATUS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(agent_id).cloned())
        .map(|(at, r)| r.is_err() && now_ms() - at < 3_600_000)
        .unwrap_or(false);
    if recent_failure {
        return;
    }
    let (state, id, from) = (state.clone(), agent_id.to_string(), h.agent_version.clone());
    tokio::spawn(async move {
        if !auto_enabled(&state).await {
            return;
        }
        // Let the session settle (registration, first metrics) before asking.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let r = update_agent(&state, &id).await;
        match &r {
            Ok(m) => tracing::info!("agent {id}: {m}"),
            Err(e) => tracing::warn!("agent {id} update {from} → {VERSION} failed: {e}"),
        }
        audit(&state, None, "", AuditEntry {
            action: "agent_update",
            server: Some((&id, &id)),
            target: Some(format!("{from} → {VERSION}")),
            detail: Some(match &r { Ok(m) => m.clone(), Err(e) => e.clone() }),
            ok: r.is_ok(),
        })
        .await;
        set_status(&id, r);
    });
}

pub async fn update_now(State(state): State<S>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_MANAGE)?;
    if !user.can_server(&id) {
        return Err(ApiError::not_found("Server not found"));
    }
    let r = update_agent(&state, &id).await;
    audit(&state, Some(&user), "", AuditEntry {
        action: "agent_update",
        server: Some((&id, &id)),
        target: Some(VERSION.to_string()),
        detail: Some(match &r { Ok(m) => m.clone(), Err(e) => e.clone() }),
        ok: r.is_ok(),
    })
    .await;
    set_status(&id, r.clone());
    match r {
        Ok(m) => Ok(Json(json!({ "ok": true, "message": m }))),
        Err(e) => Err(ApiError::bad(e)),
    }
}

pub async fn get_settings(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_MANAGE)?;
    Ok(Json(json!({ "auto_update": auto_enabled(&state).await, "hub_version": VERSION })))
}

#[derive(serde::Deserialize)]
pub struct UpdateSettings {
    auto_update: bool,
}

pub async fn put_settings(State(state): State<S>, user: AuthUser, Json(b): Json<UpdateSettings>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_MANAGE)?;
    state.db.set_setting("agent_auto_update", if b.auto_update { "true".into() } else { "false".into() }).await?;
    audit(&state, Some(&user), "", AuditEntry { action: "settings_changed", server: None, target: Some("agent auto-update".into()), detail: Some(b.auto_update.to_string()), ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::newer("1.3.0", "1.2.0"));
        assert!(super::newer("1.10.0", "1.9.9"));
        assert!(!super::newer("1.2.0", "1.2.0"));
        assert!(!super::newer("1.2.0", "1.3.0"));
    }
}
