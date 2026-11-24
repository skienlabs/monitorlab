//! Agent-facing endpoints: `/api/agent/connect` (authenticated session) and
//! `/api/agent/enroll` (one-time token exchange). Also runs the in-process local agent.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use monitorlab_common::{AgentMsg, HubMsg};

use crate::metrics;
use crate::registry::AgentConn;
use crate::state::S;
use crate::util::{ct_eq, now_ms, random_token, sha256_hex};

const MAX_AGENT_MSG: usize = 32 * 1024 * 1024;

pub async fn connect(
    State(state): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let ip = state.client_ip(&addr, &headers);
    let throttle_key = format!("agent:{ip}");
    if state.throttle.blocked(&throttle_key).is_some() {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Some((agent_id, secret)) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|v| v.split_once(':'))
        .map(|(a, b)| (a.to_string(), b.to_string()))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let id2 = agent_id.clone();
    let stored: Option<Option<String>> = state
        .db
        .run(move |c| Ok(c.query_row("SELECT secret_hash FROM agents WHERE id = ?1", [id2], |r| r.get(0)).optional()?))
        .await
        .unwrap_or(None);
    let valid = matches!(&stored, Some(Some(h)) if ct_eq(h, &sha256_hex(&secret)));
    if !valid {
        state.throttle.fail(&throttle_key, 20, 600, 600);
        tracing::warn!("rejected agent connection from {ip} (agent id {agent_id})");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.max_message_size(MAX_AGENT_MSG).on_upgrade(move |socket| async move {
        run_ws_agent(state, agent_id, ip, socket).await;
    })
}

async fn run_ws_agent(state: S, agent_id: String, ip: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (hub_tx, mut hub_rx) = mpsc::channel::<HubMsg>(256);
    let (agent_tx, agent_rx) = mpsc::channel::<AgentMsg>(512);

    let writer = tokio::spawn(async move {
        while let Some(m) = hub_rx.recv().await {
            let Ok(text) = serde_json::to_string(&m) else { continue };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(t) => match serde_json::from_str::<AgentMsg>(&t) {
                    Ok(m) => {
                        if agent_tx.send(m).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::debug!("bad agent message: {e}"),
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    run_session(state, agent_id, ip, hub_tx, agent_rx).await;
    reader.abort();
    // The writer exits once every HubMsg sender is gone.
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
}

/// Transport-independent hub side of an agent session.
pub async fn run_session(state: S, agent_id: String, remote: String, hub_tx: mpsc::Sender<HubMsg>, mut agent_rx: mpsc::Receiver<AgentMsg>) {
    let conn: Arc<AgentConn> = state.reg.register(&agent_id, remote.clone(), hub_tx.clone());
    tracing::info!("agent {agent_id} connected from {remote}");
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    ping.tick().await;

    loop {
        tokio::select! {
            m = agent_rx.recv() => {
                let Some(m) = m else { break };
                if let Some(persist) = conn.dispatch(m).await {
                    persist_msg(&state, &agent_id, persist).await;
                }
            }
            _ = ping.tick() => {
                let idle = now_ms() as u64 - conn.last_msg_at.load(Ordering::Relaxed);
                if idle > 75_000 {
                    tracing::warn!("agent {agent_id} idle for {}s, dropping", idle / 1000);
                    break;
                }
                if hub_tx.send(HubMsg::Ping).await.is_err() {
                    break;
                }
            }
            _ = conn.shutdown.notified() => break,
        }
    }
    state.reg.unregister(&conn);
    let id = agent_id.clone();
    let _ = state.db.run(move |c| Ok(c.execute("UPDATE agents SET last_seen_at = ?1 WHERE id = ?2", params![now_ms(), id])?)).await;
    tracing::info!("agent {agent_id} disconnected");
}

async fn persist_msg(state: &S, agent_id: &str, m: AgentMsg) {
    match m {
        AgentMsg::Hello(h) => {
            let id = agent_id.to_string();
            let hello = serde_json::to_string(&h).unwrap_or_default();
            let host = h.hostname.clone();
            let _ = state
                .db
                .run(move |c| {
                    c.execute(
                        "UPDATE agents SET hello = ?1, last_seen_at = ?2, name = CASE WHEN name = '' THEN ?3 ELSE name END WHERE id = ?4",
                        params![hello, now_ms(), host, id],
                    )?;
                    Ok(())
                })
                .await;
        }
        AgentMsg::Metrics(m) => {
            let id = agent_id.to_string();
            if let Err(e) = metrics::insert(&state.db, id.clone(), m).await {
                tracing::warn!("metrics insert failed: {e:#}");
            }
            let _ = state.db.run(move |c| Ok(c.execute("UPDATE agents SET last_seen_at = ?1 WHERE id = ?2", params![now_ms(), id])?)).await;
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ enrollment

#[derive(Deserialize)]
struct EnrollReq {
    token: String,
    #[serde(default)]
    hostname: String,
}

pub async fn enroll(
    State(state): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let ip = state.client_ip(&addr, &headers);
    let key = format!("enroll:{ip}");
    if state.throttle.blocked(&key).is_some() {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    ws.max_message_size(64 * 1024).on_upgrade(move |mut socket| async move {
        let first = tokio::time::timeout(Duration::from_secs(15), socket.recv()).await;
        let Ok(Some(Ok(Message::Text(t)))) = first else { return };
        let reply = match serde_json::from_str::<EnrollReq>(&t) {
            Ok(r) => match do_enroll(&state, r, &ip).await {
                Ok(v) => v,
                Err(msg) => {
                    state.throttle.fail(&key, 10, 600, 900);
                    json!({ "error": msg })
                }
            },
            Err(_) => json!({ "error": "malformed request" }),
        };
        let _ = socket.send(Message::Text(reply.to_string().into())).await;
        let _ = socket.close().await;
    })
}

async fn do_enroll(state: &S, r: EnrollReq, ip: &str) -> Result<serde_json::Value, String> {
    let token_hash = sha256_hex(r.token.trim());
    let secret = random_token(32);
    let secret_hash = sha256_hex(&secret);
    let hostname: String = r.hostname.chars().take(128).collect();
    let res = state
        .db
        .run(move |c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT id, name FROM agents WHERE enroll_token_hash = ?1 AND enroll_expires_at > ?2",
                    params![token_hash, now_ms()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((id, name)) = row else { return Ok(None) };
            let name = if name.is_empty() { hostname } else { name };
            c.execute(
                "UPDATE agents SET secret_hash = ?1, enroll_token_hash = NULL, enroll_expires_at = NULL, enrolled_at = ?2, name = ?3 WHERE id = ?4",
                params![secret_hash, now_ms(), name, id],
            )?;
            Ok(Some((id, name)))
        })
        .await
        .map_err(|_| "internal error".to_string())?;
    match res {
        Some((id, name)) => {
            // A re-enrollment replaces the old secret: kick any connection still using it.
            state.reg.disconnect(&id);
            tracing::info!("agent {id} ({name}) enrolled from {ip}");
            crate::auth::audit(
                state,
                None,
                ip,
                crate::auth::AuditEntry { action: "agent_enrolled", server: Some((&id, &name)), target: None, detail: None, ok: true },
            )
            .await;
            Ok(json!({ "agent_id": id, "secret": secret, "name": name }))
        }
        None => Err("invalid or expired enrollment token".into()),
    }
}

// ------------------------------------------------------------------ local agent

pub async fn start_local_agent(state: S) -> anyhow::Result<()> {
    let id: String = state
        .db
        .run(|c| {
            let existing: Option<String> = c.query_row("SELECT id FROM agents WHERE is_local = 1", [], |r| r.get(0)).optional()?;
            match existing {
                Some(id) => Ok(id),
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    c.execute(
                        "INSERT INTO agents (id, name, secret_hash, created_at, enrolled_at, is_local) VALUES (?1, '', '', ?2, ?2, 1)",
                        params![id, now_ms()],
                    )?;
                    Ok(id)
                }
            }
        })
        .await?;

    let opts = monitorlab_agent::AgentOptions {
        interval_secs: 5,
        docker_enabled: true,
        docker_socket: None,
        mssql: Default::default(),
        iis_enabled: Some(false),
        host_root: state.cfg.host_root.clone(),
    };
    tokio::spawn(async move {
        let agent = monitorlab_agent::Agent::new(opts);
        loop {
            let (hub_tx, hub_rx) = mpsc::channel::<HubMsg>(256);
            let (agent_tx, agent_rx) = mpsc::channel::<AgentMsg>(512);
            let serve = tokio::spawn(agent.clone().serve(hub_rx, agent_tx));
            run_session(state.clone(), id.clone(), "local".into(), hub_tx, agent_rx).await;
            // `serve` winds down by itself once its channels close.
            let _ = tokio::time::timeout(Duration::from_secs(5), serve).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    Ok(())
}
