//! Uptime monitors run by the hub: HTTP(S) endpoints (status, keyword, response time,
//! TLS certificate expiry) and TCP ports. Failures raise alerts like server rules do.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::alerts::Severity;
use crate::auth::{audit, AuditEntry, AuthUser};
use crate::rbac;
use crate::state::{ApiError, ApiResult, S};
use crate::util::now_ms;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub method: String,
    pub expect_status: String,
    pub keyword: String,
    pub interval_secs: i64,
    pub timeout_secs: i64,
    pub fail_threshold: i64,
    pub ssl_warn_days: i64,
    pub ignore_tls: bool,
    pub slow_ms: i64,
    #[allow(dead_code)]
    pub enabled: bool,
    pub last_check: Option<i64>,
    pub cert_expires: Option<i64>,
}

const SELECT: &str = "SELECT id, name, kind, target, method, expect_status, keyword, interval_secs, timeout_secs, fail_threshold, ssl_warn_days, ignore_tls, slow_ms, enabled, last_check, cert_expires FROM monitors";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Monitor> {
    Ok(Monitor {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        target: r.get(3)?,
        method: r.get(4)?,
        expect_status: r.get(5)?,
        keyword: r.get(6)?,
        interval_secs: r.get(7)?,
        timeout_secs: r.get(8)?,
        fail_threshold: r.get(9)?,
        ssl_warn_days: r.get(10)?,
        ignore_tls: r.get(11)?,
        slow_ms: r.get(12)?,
        enabled: r.get(13)?,
        last_check: r.get(14)?,
        cert_expires: r.get(15)?,
    })
}

/// "200-399", "200,204", "2xx" → does `code` match?
pub fn status_ok(spec: &str, code: u16) -> bool {
    let spec = spec.trim();
    if spec.is_empty() {
        return (200..400).contains(&code);
    }
    spec.split(',').map(str::trim).filter(|p| !p.is_empty()).any(|p| {
        if let Some(c) = p.strip_suffix("xx") {
            return c.parse::<u16>().map(|h| code / 100 == h).unwrap_or(false);
        }
        match p.split_once('-') {
            Some((a, b)) => matches!((a.trim().parse::<u16>(), b.trim().parse::<u16>()), (Ok(a), Ok(b)) if (a..=b).contains(&code)),
            None => p.parse::<u16>().map(|v| v == code).unwrap_or(false),
        }
    })
}

#[derive(Debug, Default)]
pub struct CheckResult {
    pub ok: bool,
    pub ms: Option<i64>,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub cert: Option<CertInfo>,
}

#[derive(Debug, Clone)]
pub struct CertInfo {
    pub not_after: i64,
    pub issuer: String,
    pub subject: String,
    /// Problem found when validating the chain (expired, wrong name, unknown issuer).
    pub problem: Option<String>,
}

// ------------------------------------------------------------------ TLS certificate

#[derive(Debug)]
struct Capture {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    leaf: Mutex<Option<Vec<u8>>>,
    problem: Mutex<Option<String>>,
}

impl rustls::client::danger::ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end: &rustls::pki_types::CertificateDer<'_>,
        inter: &[rustls::pki_types::CertificateDer<'_>],
        name: &rustls::pki_types::ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.leaf.lock().unwrap() = Some(end.to_vec());
        if let Err(e) = self.inner.verify_server_cert(end, inter, name, ocsp, now) {
            *self.problem.lock().unwrap() = Some(e.to_string());
        }
        // Always continue: we only want to read the certificate.
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(m, c, d)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn roots() -> Arc<rustls::RootCertStore> {
    static ROOTS: std::sync::OnceLock<Arc<rustls::RootCertStore>> = std::sync::OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut r = rustls::RootCertStore::empty();
            for c in rustls_native_certs::load_native_certs().certs {
                let _ = r.add(c);
            }
            Arc::new(r)
        })
        .clone()
}

pub async fn fetch_cert(host: &str, port: u16, timeout: Duration) -> anyhow::Result<CertInfo> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(roots(), provider.clone()).build()?;
    let cap = Arc::new(Capture { inner, leaf: Mutex::new(None), problem: Mutex::new(None) });
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(cap.clone())
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())?;
    let fut = async {
        let tcp = tokio::net::TcpStream::connect((host, port)).await?;
        let _tls = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(name, tcp).await?;
        Ok::<_, anyhow::Error>(())
    };
    tokio::time::timeout(timeout, fut).await.map_err(|_| anyhow::anyhow!("TLS handshake timed out"))??;
    let der = cap.leaf.lock().unwrap().clone().ok_or_else(|| anyhow::anyhow!("no certificate presented"))?;
    let problem = cap.problem.lock().unwrap().clone();
    let info = {
        let (_, x) = x509_parser::parse_x509_certificate(&der).map_err(|e| anyhow::anyhow!("bad certificate: {e}"))?;
        let issuer = x.issuer().iter_organization().next().and_then(|o| o.as_str().ok()).map(String::from).unwrap_or_else(|| x.issuer().to_string());
        let subject = x.subject().iter_common_name().next().and_then(|o| o.as_str().ok()).map(String::from).unwrap_or_else(|| x.subject().to_string());
        CertInfo { not_after: x.validity().not_after.timestamp() * 1000, issuer, subject, problem }
    };
    Ok(info)
}

// ------------------------------------------------------------------ checks

pub async fn check(m: &Monitor, want_cert: bool) -> CheckResult {
    let timeout = Duration::from_secs(m.timeout_secs.clamp(1, 60) as u64);
    match m.kind.as_str() {
        "tcp" => {
            let started = Instant::now();
            match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(m.target.trim())).await {
                Ok(Ok(_)) => CheckResult { ok: true, ms: Some(started.elapsed().as_millis() as i64), ..Default::default() },
                Ok(Err(e)) => CheckResult { ok: false, error: Some(format!("connection failed: {e}")), ..Default::default() },
                Err(_) => CheckResult { ok: false, error: Some(format!("no answer within {}s", timeout.as_secs())), ..Default::default() },
            }
        }
        _ => {
            let url = match reqwest::Url::parse(m.target.trim()) {
                Ok(u) => u,
                Err(e) => return CheckResult { ok: false, error: Some(format!("invalid URL: {e}")), ..Default::default() },
            };
            let client = reqwest::Client::builder()
                .timeout(timeout)
                .connect_timeout(timeout)
                .danger_accept_invalid_certs(m.ignore_tls)
                .redirect(reqwest::redirect::Policy::limited(5))
                .user_agent(concat!("MonitorLab-Uptime/", env!("CARGO_PKG_VERSION")))
                .build();
            let client = match client {
                Ok(c) => c,
                Err(e) => return CheckResult { ok: false, error: Some(e.to_string()), ..Default::default() },
            };
            let method = reqwest::Method::from_bytes(m.method.to_uppercase().as_bytes()).unwrap_or(reqwest::Method::GET);
            let started = Instant::now();
            let mut r = match client.request(method, url.clone()).send().await {
                Ok(res) => {
                    let ms = started.elapsed().as_millis() as i64;
                    let code = res.status().as_u16();
                    let mut ok = status_ok(&m.expect_status, code);
                    let mut error = if ok { None } else { Some(format!("HTTP {code} (expected {})", if m.expect_status.is_empty() { "200-399" } else { &m.expect_status })) };
                    if ok && !m.keyword.is_empty() {
                        // Read at most 2 MB looking for the keyword.
                        let body = tokio::time::timeout(timeout, async {
                            let mut res = res;
                            let mut buf = Vec::new();
                            while let Ok(Some(chunk)) = res.chunk().await {
                                buf.extend_from_slice(&chunk);
                                if buf.len() > 2 * 1024 * 1024 {
                                    break;
                                }
                            }
                            buf
                        })
                        .await
                        .unwrap_or_default();
                        if !String::from_utf8_lossy(&body).contains(&m.keyword) {
                            ok = false;
                            error = Some(format!("keyword \"{}\" not found in the response", m.keyword));
                        }
                    }
                    CheckResult { ok, ms: Some(ms), status: Some(code), error, cert: None }
                }
                Err(e) => {
                    let mut msg = e.to_string();
                    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
                    while let Some(s) = src {
                        msg = format!("{msg}: {s}");
                        src = s.source();
                    }
                    CheckResult { ok: false, error: Some(if e.is_timeout() { format!("no answer within {}s", timeout.as_secs()) } else { msg }), ..Default::default() }
                }
            };
            if want_cert && url.scheme() == "https" {
                if let Some(host) = url.host_str() {
                    match fetch_cert(host, url.port_or_known_default().unwrap_or(443), timeout).await {
                        Ok(c) => r.cert = Some(c),
                        Err(e) => tracing::debug!("monitor {}: certificate check failed: {e:#}", m.id),
                    }
                }
            }
            r
        }
    }
}

/// Alerts for one monitor after a check.
fn desired(m: &Monitor, fails: i64, last: &CheckResult, cert_expires: Option<i64>, cert_problem: Option<&str>) -> Vec<(String, Severity, String)> {
    let mut d = Vec::new();
    if fails >= m.fail_threshold.max(1) {
        d.push(("down".into(), Severity::Critical, format!("{} is down: {}", m.name, last.error.clone().unwrap_or_else(|| "check failed".into()))));
    } else if last.ok && m.slow_ms > 0 && last.ms.unwrap_or(0) > m.slow_ms {
        d.push(("slow".into(), Severity::Warning, format!("{} is slow: {} ms (limit {} ms)", m.name, last.ms.unwrap_or(0), m.slow_ms)));
    }
    if let Some(exp) = cert_expires {
        let days = (exp - now_ms()) / 86_400_000;
        if exp <= now_ms() {
            d.push(("ssl".into(), Severity::Critical, format!("SSL certificate of {} has expired", m.name)));
        } else if days < 7 {
            d.push(("ssl".into(), Severity::Critical, format!("SSL certificate of {} expires in {days} day(s)", m.name)));
        } else if days < m.ssl_warn_days {
            d.push(("ssl".into(), Severity::Warning, format!("SSL certificate of {} expires in {days} days", m.name)));
        }
    }
    if let (Some(p), false) = (cert_problem, m.ignore_tls) {
        if !d.iter().any(|x| x.0 == "ssl") {
            d.push(("ssl".into(), Severity::Warning, format!("SSL certificate of {} is not valid: {p}", m.name)));
        }
    }
    d
}

async fn run_one(state: &S, m: Monitor) -> anyhow::Result<()> {
    // Certificates change rarely: read them every 6 hours (and on the first check).
    let want_cert = m.kind == "http" && (m.cert_expires.is_none() || m.last_check.map(|t| (now_ms() / 21_600_000) != (t / 21_600_000)).unwrap_or(true));
    let r = check(&m, want_cert).await;
    let now = now_ms();
    let id = m.id;
    let (ok, ms, status, err) = (r.ok, r.ms, r.status.map(|s| s as i64), r.error.clone());
    let cert = r.cert.clone();
    let (fails, cert_expires, cert_problem): (i64, Option<i64>, Option<String>) = state
        .db
        .run(move |c| {
            c.execute("INSERT OR REPLACE INTO monitor_results (monitor_id, ts, ok, ms, status, error) VALUES (?1,?2,?3,?4,?5,?6)", params![id, now, ok, ms, status, err])?;
            c.execute(
                "UPDATE monitors SET last_check = ?2, last_ok = CASE WHEN ?3 THEN ?2 ELSE last_ok END, last_ms = ?4, last_status = ?5, last_error = ?6, \
                 fails = CASE WHEN ?3 THEN 0 ELSE fails + 1 END WHERE id = ?1",
                params![id, now, ok, ms, status, err],
            )?;
            if let Some(ci) = &cert {
                c.execute(
                    "UPDATE monitors SET cert_expires = ?2, cert_issuer = ?3, cert_subject = ?4 WHERE id = ?1",
                    params![id, ci.not_after, ci.issuer, ci.subject],
                )?;
            }
            let (f, ce): (i64, Option<i64>) = c.query_row("SELECT fails, cert_expires FROM monitors WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok((f, ce, cert.and_then(|c| c.problem)))
        })
        .await?;
    let desired = desired(&m, fails, &r, cert_expires, cert_problem.as_deref());
    let events = crate::alerts::reconcile(state, &format!("monitor:{}", m.id), &m.name, desired).await?;
    crate::alerts::notify(state, events).await;
    Ok(())
}

pub async fn scheduler(state: S) {
    let in_flight: Arc<Mutex<HashSet<i64>>> = Default::default();
    let sem = Arc::new(tokio::sync::Semaphore::new(16));
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        let due: Vec<Monitor> = match state
            .db
            .run(|c| {
                let mut st = c.prepare(&format!("{SELECT} WHERE enabled = 1"))?;
                let v = st.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
                Ok(v)
            })
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("monitors: {e:#}");
                continue;
            }
        };
        let now = now_ms();
        for m in due {
            let interval = m.interval_secs.clamp(10, 86_400) * 1000;
            if m.last_check.map(|t| now - t < interval).unwrap_or(false) || !in_flight.lock().unwrap().insert(m.id) {
                continue;
            }
            let (state, fl, sem) = (state.clone(), in_flight.clone(), sem.clone());
            tokio::spawn(async move {
                let _permit = sem.acquire().await;
                let id = m.id;
                if let Err(e) = run_one(&state, m).await {
                    tracing::warn!("monitor {id}: {e:#}");
                }
                fl.lock().unwrap().remove(&id);
            });
        }
    }
}

// ------------------------------------------------------------------ API

#[derive(Deserialize)]
pub struct MonitorBody {
    name: String,
    kind: String,
    target: String,
    #[serde(default = "d_get")]
    method: String,
    #[serde(default)]
    expect_status: String,
    #[serde(default)]
    keyword: String,
    #[serde(default = "d_60")]
    interval_secs: i64,
    #[serde(default = "d_10")]
    timeout_secs: i64,
    #[serde(default = "d_2")]
    fail_threshold: i64,
    #[serde(default = "d_14")]
    ssl_warn_days: i64,
    #[serde(default)]
    ignore_tls: bool,
    #[serde(default)]
    slow_ms: i64,
    #[serde(default = "d_true")]
    enabled: bool,
}
fn d_get() -> String {
    "GET".into()
}
fn d_60() -> i64 {
    60
}
fn d_10() -> i64 {
    10
}
fn d_2() -> i64 {
    2
}
fn d_14() -> i64 {
    14
}
fn d_true() -> bool {
    true
}

fn validate(b: &MonitorBody) -> ApiResult<()> {
    if b.name.trim().is_empty() || b.name.len() > 100 {
        return Err(ApiError::bad("Give the monitor a name (max 100 characters)"));
    }
    match b.kind.as_str() {
        "http" => {
            let u = reqwest::Url::parse(b.target.trim()).map_err(|_| ApiError::bad("Enter a full URL, e.g. https://example.com/health"))?;
            if !matches!(u.scheme(), "http" | "https") {
                return Err(ApiError::bad("Only http:// and https:// URLs can be checked"));
            }
            if !["GET", "HEAD", "POST", "OPTIONS"].contains(&b.method.to_uppercase().as_str()) {
                return Err(ApiError::bad("Method must be GET, HEAD, POST or OPTIONS"));
            }
        }
        "tcp" => {
            let (h, p) = b.target.trim().rsplit_once(':').ok_or_else(|| ApiError::bad("Enter host:port"))?;
            if h.is_empty() || p.parse::<u16>().map(|p| p == 0).unwrap_or(true) {
                return Err(ApiError::bad("Enter host:port with a valid port"));
            }
        }
        _ => return Err(ApiError::bad("Type must be http or tcp")),
    }
    if !(10..=86_400).contains(&b.interval_secs) {
        return Err(ApiError::bad("Interval must be between 10 seconds and 24 hours"));
    }
    if !(1..=60).contains(&b.timeout_secs) || !(1..=20).contains(&b.fail_threshold) || !(1..=365).contains(&b.ssl_warn_days) || b.slow_ms < 0 {
        return Err(ApiError::bad("Check the timeout (1–60 s), failures before alerting (1–20) and SSL warning days (1–365)"));
    }
    if !b.expect_status.trim().is_empty() && !b.expect_status.split(',').all(|p| {
        let p = p.trim();
        p.strip_suffix("xx").map(|c| c.parse::<u8>().is_ok()).unwrap_or(false) || p.split('-').all(|x| x.trim().parse::<u16>().is_ok())
    }) {
        return Err(ApiError::bad("Expected status: e.g. 200-399, 200,204 or 2xx"));
    }
    Ok(())
}

pub async fn list(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_VIEW)?;
    let rows = state
        .db
        .run(|c| {
            let now = now_ms();
            let mut st = c.prepare(
                "SELECT id, name, kind, target, method, expect_status, keyword, interval_secs, timeout_secs, fail_threshold, ssl_warn_days, ignore_tls, slow_ms, enabled, \
                 last_check, last_ok, last_ms, last_error, last_status, fails, cert_expires, cert_issuer, cert_subject FROM monitors ORDER BY name",
            )?;
            let mut out = Vec::new();
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let up = |since: i64| -> rusqlite::Result<(Option<f64>, Option<f64>)> {
                    c.query_row(
                        "SELECT AVG(ok) * 100.0, AVG(CASE WHEN ok THEN ms END) FROM monitor_results WHERE monitor_id = ?1 AND ts >= ?2",
                        params![id, since],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                };
                let (u24, ms24) = up(now - 86_400_000)?;
                let (u7, _) = up(now - 7 * 86_400_000)?;
                let (u30, _) = up(now - 30 * 86_400_000)?;
                let mut recent: Vec<Value> = {
                    let mut q = c.prepare_cached("SELECT ts, ok, ms FROM monitor_results WHERE monitor_id = ?1 ORDER BY ts DESC LIMIT 60")?;
                    let v = q.query_map([id], |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, bool>(1)?, r.get::<_, Option<i64>>(2)?])))?.collect::<Result<Vec<_>, _>>()?;
                    v
                };
                recent.reverse();
                let fails: i64 = r.get(19)?;
                let threshold: i64 = r.get(9)?;
                let enabled: bool = r.get(13)?;
                let last_check: Option<i64> = r.get(14)?;
                out.push(json!({
                    "id": id, "name": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?, "target": r.get::<_, String>(3)?,
                    "method": r.get::<_, String>(4)?, "expect_status": r.get::<_, String>(5)?, "keyword": r.get::<_, String>(6)?,
                    "interval_secs": r.get::<_, i64>(7)?, "timeout_secs": r.get::<_, i64>(8)?, "fail_threshold": threshold,
                    "ssl_warn_days": r.get::<_, i64>(10)?, "ignore_tls": r.get::<_, bool>(11)?, "slow_ms": r.get::<_, i64>(12)?, "enabled": enabled,
                    "last_check": last_check, "last_ok": r.get::<_, Option<i64>>(15)?, "last_ms": r.get::<_, Option<i64>>(16)?,
                    "last_error": r.get::<_, Option<String>>(17)?, "last_status": r.get::<_, Option<i64>>(18)?, "fails": fails,
                    "cert_expires": r.get::<_, Option<i64>>(20)?, "cert_issuer": r.get::<_, Option<String>>(21)?, "cert_subject": r.get::<_, Option<String>>(22)?,
                    "state": if !enabled { "paused" } else if last_check.is_none() { "pending" } else if fails >= threshold { "down" } else if fails > 0 { "degraded" } else { "up" },
                    "uptime_24h": u24, "uptime_7d": u7, "uptime_30d": u30, "avg_ms_24h": ms24.map(|v| v.round()),
                    "recent": recent,
                }));
            }
            Ok(out)
        })
        .await?;
    Ok(Json(Value::Array(rows)))
}

#[derive(Deserialize)]
pub struct ResultsQuery {
    #[serde(default)]
    range: Option<String>,
}

pub async fn results(State(state): State<S>, user: AuthUser, Path(id): Path<i64>, Query(q): Query<ResultsQuery>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_VIEW)?;
    let (lookback, bucket) = match q.range.as_deref().unwrap_or("24h") {
        "1h" => (3_600_000, 0),
        "24h" => (86_400_000, 5 * 60_000),
        "7d" => (7 * 86_400_000, 30 * 60_000),
        "30d" => (30 * 86_400_000, 2 * 3_600_000),
        _ => (86_400_000, 5 * 60_000),
    };
    let since = now_ms() - lookback;
    let out = state
        .db
        .run(move |c| {
            let exists: Option<i64> = c.query_row("SELECT id FROM monitors WHERE id = ?1", [id], |r| r.get(0)).optional()?;
            if exists.is_none() {
                return Ok(None);
            }
            let (mut ts, mut ms, mut up) = (Vec::new(), Vec::new(), Vec::new());
            if bucket == 0 {
                let mut st = c.prepare("SELECT ts, ms, ok FROM monitor_results WHERE monitor_id = ?1 AND ts >= ?2 ORDER BY ts")?;
                let mut rows = st.query(params![id, since])?;
                while let Some(r) = rows.next()? {
                    ts.push(json!(r.get::<_, i64>(0)? / 1000));
                    ms.push(json!(r.get::<_, Option<i64>>(1)?));
                    up.push(json!(if r.get::<_, bool>(2)? { 100.0 } else { 0.0 }));
                }
            } else {
                let mut st = c.prepare(
                    "SELECT (ts / ?3) * ?3 t, AVG(CASE WHEN ok THEN ms END), AVG(ok) * 100.0 FROM monitor_results WHERE monitor_id = ?1 AND ts >= ?2 GROUP BY t ORDER BY t",
                )?;
                let mut rows = st.query(params![id, since, bucket])?;
                while let Some(r) = rows.next()? {
                    ts.push(json!(r.get::<_, i64>(0)? / 1000));
                    ms.push(json!(r.get::<_, Option<f64>>(1)?.map(|v| v.round())));
                    up.push(json!(r.get::<_, Option<f64>>(2)?.map(|v| (v * 10.0).round() / 10.0)));
                }
            }
            // Outages: consecutive failed checks.
            let mut incidents = Vec::new();
            let mut st = c.prepare("SELECT ts, ok, error FROM monitor_results WHERE monitor_id = ?1 AND ts >= ?2 ORDER BY ts")?;
            let mut rows = st.query(params![id, now_ms() - 30 * 86_400_000])?;
            let mut open: Option<(i64, String, i64)> = None;
            while let Some(r) = rows.next()? {
                let (t, ok, err): (i64, bool, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?);
                match (&mut open, ok) {
                    (None, false) => open = Some((t, err.unwrap_or_default(), 1)),
                    (Some(o), false) => o.2 += 1,
                    (Some(_), true) => {
                        let (start, e, n) = open.take().unwrap();
                        incidents.push(json!({ "start": start, "end": t, "error": e, "checks": n }));
                    }
                    (None, true) => {}
                }
            }
            if let Some((start, e, n)) = open {
                incidents.push(json!({ "start": start, "end": null, "error": e, "checks": n }));
            }
            incidents.reverse();
            Ok(Some(json!({ "ts": ts, "series": { "ms": ms, "up": up }, "incidents": incidents })))
        })
        .await?;
    out.map(Json).ok_or_else(|| ApiError::not_found("Monitor not found"))
}

pub async fn create(State(state): State<S>, user: AuthUser, Json(b): Json<MonitorBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    validate(&b)?;
    let name = b.name.trim().to_string();
    let n2 = name.clone();
    let id: i64 = state
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO monitors (name, kind, target, method, expect_status, keyword, interval_secs, timeout_secs, fail_threshold, ssl_warn_days, ignore_tls, slow_ms, enabled, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![n2, b.kind, b.target.trim(), b.method.to_uppercase(), b.expect_status.trim(), b.keyword, b.interval_secs, b.timeout_secs, b.fail_threshold, b.ssl_warn_days, b.ignore_tls, b.slow_ms, b.enabled, now_ms()],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "monitor_created", server: None, target: Some(name), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub async fn update(State(state): State<S>, user: AuthUser, Path(id): Path<i64>, Json(b): Json<MonitorBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    validate(&b)?;
    let name = b.name.trim().to_string();
    let n2 = name.clone();
    let n = state
        .db
        .run(move |c| {
            Ok(c.execute(
                "UPDATE monitors SET name=?2, kind=?3, target=?4, method=?5, expect_status=?6, keyword=?7, interval_secs=?8, timeout_secs=?9, fail_threshold=?10, \
                 ssl_warn_days=?11, ignore_tls=?12, slow_ms=?13, enabled=?14, last_check = NULL, cert_expires = NULL WHERE id = ?1",
                params![id, n2, b.kind, b.target.trim(), b.method.to_uppercase(), b.expect_status.trim(), b.keyword, b.interval_secs, b.timeout_secs, b.fail_threshold, b.ssl_warn_days, b.ignore_tls, b.slow_ms, b.enabled],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(ApiError::not_found("Monitor not found"));
    }
    if !b.enabled {
        // Paused monitors don't keep alerts open.
        let _ = crate::alerts::reconcile(&state, &format!("monitor:{id}"), &name, vec![]).await;
    }
    audit(&state, Some(&user), "", AuditEntry { action: "monitor_updated", server: None, target: Some(name), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(State(state): State<S>, user: AuthUser, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let name: Option<String> = state
        .db
        .run(move |c| {
            let n: Option<String> = c.query_row("SELECT name FROM monitors WHERE id = ?1", [id], |r| r.get(0)).optional()?;
            c.execute("DELETE FROM monitors WHERE id = ?1", [id])?;
            c.execute("DELETE FROM monitor_results WHERE monitor_id = ?1", [id])?;
            c.execute("UPDATE alerts SET resolved_at = ?2 WHERE agent_id = ?1 AND resolved_at IS NULL", params![format!("monitor:{id}"), now_ms()])?;
            c.execute("DELETE FROM maintenance WHERE target = ?1", [format!("monitor:{id}")])?;
            Ok(n)
        })
        .await?;
    let Some(name) = name else { return Err(ApiError::not_found("Monitor not found")) };
    audit(&state, Some(&user), "", AuditEntry { action: "monitor_deleted", server: None, target: Some(name), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

/// Run a check now (also used to try a monitor before saving it).
pub async fn check_now(State(state): State<S>, user: AuthUser, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let m: Option<Monitor> = state.db.run(move |c| Ok(c.query_row(&format!("{SELECT} WHERE id = ?1"), [id], row).optional()?)).await?;
    let mut m = m.ok_or_else(|| ApiError::not_found("Monitor not found"))?;
    m.cert_expires = None; // re-read the certificate
    run_one(&state, m).await.map_err(|e| ApiError::bad(e.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn test(State(_state): State<S>, user: AuthUser, Json(b): Json<MonitorBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    validate(&b)?;
    let m = Monitor {
        id: 0,
        name: b.name,
        kind: b.kind,
        target: b.target,
        method: b.method,
        expect_status: b.expect_status,
        keyword: b.keyword,
        interval_secs: b.interval_secs,
        timeout_secs: b.timeout_secs,
        fail_threshold: b.fail_threshold,
        ssl_warn_days: b.ssl_warn_days,
        ignore_tls: b.ignore_tls,
        slow_ms: b.slow_ms,
        enabled: true,
        last_check: None,
        cert_expires: None,
    };
    let r = check(&m, true).await;
    Ok(Json(json!({
        "ok": r.ok, "ms": r.ms, "status": r.status, "error": r.error,
        "cert": r.cert.map(|c| json!({ "expires": c.not_after, "issuer": c.issuer, "subject": c.subject, "problem": c.problem })),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert!(status_ok("", 200));
        assert!(status_ok("200-399", 301));
        assert!(!status_ok("200-399", 404));
        assert!(status_ok("200,204", 204));
        assert!(status_ok("2xx, 401", 401));
        assert!(!status_ok("2xx", 500));
    }

    #[test]
    fn alert_rules() {
        let m = Monitor {
            id: 1, name: "site".into(), kind: "http".into(), target: String::new(), method: "GET".into(), expect_status: String::new(), keyword: String::new(),
            interval_secs: 60, timeout_secs: 10, fail_threshold: 2, ssl_warn_days: 14, ignore_tls: false, slow_ms: 500, enabled: true, last_check: None, cert_expires: None,
        };
        let down = CheckResult { ok: false, error: Some("HTTP 500".into()), ..Default::default() };
        assert!(desired(&m, 1, &down, None, None).is_empty(), "one failure is not an outage yet");
        assert_eq!(desired(&m, 2, &down, None, None)[0].0, "down");
        let slow = CheckResult { ok: true, ms: Some(900), ..Default::default() };
        assert_eq!(desired(&m, 0, &slow, None, None)[0].0, "slow");
        let soon = now_ms() + 3 * 86_400_000;
        let d = desired(&m, 0, &CheckResult { ok: true, ms: Some(10), ..Default::default() }, Some(soon), None);
        assert_eq!((d[0].0.as_str(), d[0].1), ("ssl", Severity::Critical));
        let later = now_ms() + 10 * 86_400_000;
        assert_eq!(desired(&m, 0, &CheckResult { ok: true, ..Default::default() }, Some(later), None)[0].1, Severity::Warning);
    }
}
