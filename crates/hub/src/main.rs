mod agent_ws;
mod alerts;
mod api;
mod auth;
mod config;
mod db;
mod metrics;
mod rbac;
mod registry;
mod state;
mod tls;
mod util;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use clap::Parser;
use rusqlite::{params, OptionalExtension};
use rust_embed::RustEmbed;
use serde_json::json;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;

use crate::config::HubConfig;
use crate::db::Db;
use crate::registry::Registry;
use crate::state::{AppState, Throttle, S};
use crate::util::{now_ms, random_password};

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'; object-src 'none'";

async fn security_headers(req: Request, next: Next) -> Response {
    let is_api = req.uri().path().starts_with("/api/");
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert("permissions-policy", HeaderValue::from_static("camera=(), microphone=(), geolocation=()"));
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    if is_api {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    resp
}

/// State-changing API calls must carry the `X-MonitorLab-Csrf` header (which a cross-site form or
/// simple request cannot set) and, when present, a same-origin `Origin`.
async fn csrf_guard(State(state): State<S>, req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        let has_header = req.headers().get("x-monitorlab-csrf").map(|v| v == "1").unwrap_or(false);
        let origin_ok = req.headers().get(header::ORIGIN).is_none() || api::same_origin(&state, req.headers());
        if !has_header || !origin_ok {
            return (StatusCode::FORBIDDEN, Json(json!({ "error": "CSRF check failed" }))).into_response();
        }
    }
    next.run(req).await
}

async fn static_files(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
    }
    let (file, name) = match Assets::get(path).filter(|_| !path.is_empty()) {
        Some(f) => (f, path.to_string()),
        None => match Assets::get("index.html") {
            Some(f) => (f, "index.html".to_string()),
            None => return (StatusCode::NOT_FOUND, "UI not built").into_response(),
        },
    };
    let mime = mime_guess::from_path(&name).first_or_octet_stream();
    let cache = if name.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(file.data.into_owned()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn api_not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

fn router(state: S) -> Router {
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/password", post(auth::change_password))
        .route("/servers", get(api::list_servers).post(api::create_server))
        .route("/servers/{id}", get(api::get_server).patch(api::rename_server).delete(api::delete_server))
        .route("/servers/{id}/metrics", get(api::server_metrics))
        .route("/servers/{id}/rpc", post(api::server_rpc))
        .route("/servers/{id}/stream", get(api::server_stream))
        .route("/servers/{id}/enroll-token", post(api::new_enroll_token))
        .route("/users", get(api::list_users).post(api::create_user))
        .route("/users/{id}", patch(api::update_user).delete(api::delete_user))
        .route("/roles", get(api::list_roles).post(api::create_role))
        .route("/roles/{id}", patch(api::update_role).delete(api::delete_role))
        .route("/permissions", get(api::permissions))
        .route("/audit", get(api::audit_log))
        .route("/alerts", get(alerts::list))
        .route("/alerts/{id}/ack", post(alerts::ack))
        .route("/alert-rules", get(alerts::get_rules).put(alerts::put_rules))
        .route("/alert-channels", get(alerts::list_channels).post(alerts::create_channel))
        .route("/alert-channels/{id}", put(alerts::update_channel).delete(alerts::delete_channel))
        .route("/alert-channels/{id}/test", post(alerts::test_channel))
        .route("/smtp", get(alerts::get_smtp).put(alerts::put_smtp))
        .route("/smtp/test", post(alerts::test_smtp))
        .route("/hub/info", get(api::hub_info))
        .route("/agent/connect", get(agent_ws::connect))
        .route("/agent/enroll", get(agent_ws::enroll))
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/branding", get(api::get_branding).put(api::put_branding))
        .fallback(api_not_found)
        .layer(middleware::from_fn_with_state(state.clone(), csrf_guard));

    Router::new()
        .nest("/api", api)
        .route("/download/{file}", get(api::download))
        .fallback(static_files)
        .layer(middleware::from_fn(security_headers))
        .layer(CompressionLayer::new())
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .with_state(state)
}

async fn bootstrap(db: &Db, cfg: &HubConfig) -> Result<()> {
    let roles = rbac::builtin_roles();
    let full = serde_json::to_string(&rbac::all())?;
    db.run(move |c| {
        for (name, desc, perms) in roles {
            let exists: Option<i64> = c.query_row("SELECT id FROM roles WHERE name = ?1", [name], |r| r.get(0)).optional()?;
            if exists.is_none() {
                c.execute(
                    "INSERT INTO roles (name, description, permissions, builtin) VALUES (?1, ?2, ?3, 1)",
                    params![name, desc, serde_json::to_string(&perms)?],
                )?;
            }
        }
        // Administrator always has every permission (including ones added in upgrades).
        c.execute("UPDATE roles SET permissions = ?1 WHERE name = 'Administrator' AND builtin = 1", [full])?;
        Ok(())
    })
    .await?;

    let users: i64 = db.run(|c| Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)).await?;
    if users == 0 {
        let email = cfg.admin_email.trim().to_lowercase();
        let (password, generated) = match &cfg.admin_password {
            Some(p) if p.len() >= 10 => (p.clone(), false),
            Some(_) => anyhow::bail!("MONITORLAB_ADMIN_PASSWORD must be at least 10 characters"),
            None => (random_password(20), true),
        };
        let hash = auth::hash_password(&password)?;
        let e2 = email.clone();
        db.run(move |c| {
            let role: i64 = c.query_row("SELECT id FROM roles WHERE name = 'Administrator'", [], |r| r.get(0))?;
            c.execute(
                "INSERT INTO users (email, name, password_hash, role_id, must_change_password, created_at) VALUES (?1, 'Administrator', ?2, ?3, ?4, ?5)",
                params![e2, hash, role, generated, now_ms()],
            )?;
            Ok(())
        })
        .await?;
        if generated {
            let note = cfg.data_dir.join("initial-admin-password.txt");
            let _ = std::fs::write(&note, format!("email: {email}\npassword: {password}\n(you will be asked to change it at first login; delete this file afterwards)\n"));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o600));
            }
            tracing::warn!("==========================================================");
            tracing::warn!(" Initial administrator account created");
            tracing::warn!("   email:    {email}");
            tracing::warn!("   password: {password}");
            tracing::warn!(" (also saved to {}; change it at first login)", note.display());
            tracing::warn!("==========================================================");
        } else {
            tracing::info!("initial administrator {email} created from MONITORLAB_ADMIN_PASSWORD");
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}

/// `monitorlab-hub healthcheck`: used by the Docker HEALTHCHECK. Reads the same environment
/// as the hub and probes its own /api/health. Exit code 0 = healthy.
fn healthcheck() -> i32 {
    let cfg = HubConfig::parse_from(["monitorlab-hub"]);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let ip = if cfg.listen.ip().is_unspecified() {
        if cfg.listen.is_ipv6() { "[::1]".to_string() } else { "127.0.0.1".to_string() }
    } else if cfg.listen.is_ipv6() {
        format!("[{}]", cfg.listen.ip())
    } else {
        cfg.listen.ip().to_string()
    };
    let scheme = if cfg.tls == config::TlsMode::Off { "http" } else { "https" };
    let url = format!("{scheme}://{ip}:{}/api/health", cfg.listen.port());
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(_) => return 1,
    };
    rt.block_on(async {
        // Local loopback probe of our own certificate, so certificate validation is skipped.
        let client = match reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(std::time::Duration::from_secs(5)).build() {
            Ok(c) => c,
            Err(_) => return 1,
        };
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => 0,
            Ok(r) => {
                eprintln!("unhealthy: {url} returned {}", r.status());
                1
            }
            Err(e) => {
                eprintln!("unhealthy: {url}: {e}");
                1
            }
        }
    })
}

fn main() -> Result<()> {
    // Decide before any async runtime exists: the health probe builds its own small runtime.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(healthcheck());
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run())
}

async fn run() -> Result<()> {
    let cfg = HubConfig::parse();
    // One process-wide TLS crypto provider (ring) for every rustls user: hub TLS, webhooks, SMTP.
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MONITORLAB_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=warn,bollard=warn")),
        )
        .init();

    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    let db = Db::open(&cfg.data_dir.join("monitorlab.db"))?;
    bootstrap(&db, &cfg).await?;
    let tls = tls::load(&cfg)?;

    let state: S = Arc::new(AppState {
        cfg: cfg.clone(),
        db: db.clone(),
        reg: Registry::default(),
        tls: tls.clone(),
        throttle: Throttle::default(),
        rules: std::sync::RwLock::new(Default::default()),
        brand: std::sync::RwLock::new(cfg.brand_name.clone()),
        company: std::sync::RwLock::new(cfg.company_name.clone()),
    });
    if let Some(c) = db.get_setting("company_name").await?.filter(|n| !n.trim().is_empty()) {
        *state.company.write().unwrap() = c;
    }
    if let Some(name) = db.get_setting("brand_name").await?.filter(|n| !n.trim().is_empty()) {
        *state.brand.write().unwrap() = name;
    }
    *state.rules.write().unwrap() = alerts::load_rules(&state).await;

    // Warm the constant-time dummy hash so the first failed login is not faster.
    tokio::task::spawn_blocking(auth::dummy_hash);

    tokio::spawn(metrics::maintenance_loop(db.clone(), cfg.retention_raw_days, cfg.retention_days));
    tokio::spawn(alerts::evaluator_loop(state.clone()));
    if cfg.local_agent {
        agent_ws::start_local_agent(state.clone()).await?;
    }

    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tracing::info!("MonitorLab hub {}{} listening on {}://{}", monitorlab_common::VERSION, api::build_id().map(|b| format!(" (build {b})")).unwrap_or_default(), if tls.is_some() { "https" } else { "http" }, cfg.listen);
    if let Some(t) = &tls {
        tracing::info!("TLS certificate SHA-256 fingerprint: {}", t.fingerprint);
        let handle = axum_server::Handle::new();
        let h2 = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            h2.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
        });
        axum_server::bind_rustls(cfg.listen, axum_server::tls_rustls::RustlsConfig::from_config(t.config.clone()))
            .handle(handle)
            .serve(app)
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
        axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
    }
    Ok(())
}
