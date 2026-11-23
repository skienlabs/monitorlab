use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::config::HubConfig;
use crate::db::Db;
use crate::registry::Registry;
use crate::tls::TlsInfo;
use crate::util::now_ms;

pub struct AppState {
    pub cfg: HubConfig,
    pub db: Db,
    pub reg: Registry,
    pub tls: Option<TlsInfo>,
    pub throttle: Throttle,
}

pub type S = Arc<AppState>;

impl AppState {
    pub fn client_ip(&self, addr: &SocketAddr, headers: &HeaderMap) -> String {
        if self.cfg.trust_proxy {
            if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
                if let Some(first) = v.split(',').next() {
                    let ip = first.trim();
                    if !ip.is_empty() {
                        return ip.to_string();
                    }
                }
            }
        }
        addr.ip().to_string()
    }

    pub fn secure_cookies(&self) -> bool {
        self.tls.is_some() || self.cfg.secure_cookies
    }
}

// ------------------------------------------------------------------ errors

#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self(StatusCode::FORBIDDEN, msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }
    pub fn unauthorized() -> Self {
        Self(StatusCode::UNAUTHORIZED, "authentication required".into())
    }
    pub fn unavailable(msg: impl Into<String>) -> Self {
        Self(StatusCode::SERVICE_UNAVAILABLE, msg.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("internal error: {e:#}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

// ------------------------------------------------------------------ brute-force throttle

#[derive(Default)]
pub struct Throttle {
    map: Mutex<HashMap<String, (u32, i64, i64)>>, // failures, window_start, blocked_until
}

impl Throttle {
    /// Returns seconds remaining if the key is currently blocked.
    pub fn blocked(&self, key: &str) -> Option<i64> {
        let m = self.map.lock().unwrap();
        m.get(key).and_then(|(_, _, until)| {
            let left = (*until - now_ms()) / 1000;
            (left > 0).then_some(left.max(1))
        })
    }

    pub fn fail(&self, key: &str, limit: u32, window_s: i64, block_s: i64) {
        let now = now_ms();
        let mut m = self.map.lock().unwrap();
        if m.len() > 50_000 {
            m.retain(|_, v| v.2 > now || now - v.1 < window_s * 1000);
        }
        let e = m.entry(key.to_string()).or_insert((0, now, 0));
        if now - e.1 > window_s * 1000 {
            *e = (0, now, 0);
        }
        e.0 += 1;
        if e.0 >= limit {
            e.2 = now + block_s * 1000;
            e.0 = 0;
            e.1 = now;
        }
    }

    pub fn clear(&self, key: &str) {
        self.map.lock().unwrap().remove(key);
    }
}
