//! Alerting: threshold rules evaluated on the hub, persisted alert state and webhook
//! notifications (Microsoft Teams, Slack-compatible, generic JSON).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use monitorlab_common::MetricsSample;

use crate::auth::{audit, AuditEntry, AuthUser};
use crate::rbac;
use crate::state::{ApiError, ApiResult, S};
use crate::util::now_ms;

// =================================================================== rules

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Threshold {
    pub warn: f64,
    pub crit: f64,
    /// Minutes the value must stay above the threshold before alerting (0 = immediately).
    #[serde(default)]
    pub for_min: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertRules {
    pub enabled: bool,
    pub cpu: Threshold,
    pub mem: Threshold,
    pub disk: Threshold,
    /// Minutes without contact before an agent is reported offline.
    pub offline_min: u32,
    pub containers_unhealthy: bool,
    pub mssql_stopped: bool,
    pub iis_stopped: bool,
    /// Kubernetes: node not ready (critical), failing pods / degraded workloads (warning).
    #[serde(default = "yes")]
    pub k8s_problems: bool,
}

fn yes() -> bool {
    true
}

impl Default for AlertRules {
    fn default() -> Self {
        Self {
            enabled: true,
            cpu: Threshold { warn: 85.0, crit: 95.0, for_min: 5 },
            mem: Threshold { warn: 90.0, crit: 95.0, for_min: 5 },
            disk: Threshold { warn: 85.0, crit: 95.0, for_min: 0 },
            offline_min: 2,
            containers_unhealthy: true,
            mssql_stopped: true,
            iis_stopped: true,
            k8s_problems: true,
        }
    }
}

impl AlertRules {
    pub fn validate(&self) -> Result<(), String> {
        for (n, t, max) in [("CPU", &self.cpu, 100.0), ("Memory", &self.mem, 100.0), ("Disk", &self.disk, 100.0)] {
            if !(1.0..=max).contains(&t.warn) || !(1.0..=max).contains(&t.crit) {
                return Err(format!("{n} thresholds must be between 1 and 100"));
            }
            if t.warn > t.crit {
                return Err(format!("{n}: the warning threshold must not exceed the critical threshold"));
            }
            if t.for_min > 120 {
                return Err(format!("{n}: duration must be at most 120 minutes"));
            }
        }
        if !(1..=1440).contains(&self.offline_min) {
            return Err("Offline delay must be between 1 and 1440 minutes".into());
        }
        Ok(())
    }
}

pub async fn load_rules(state: &S) -> AlertRules {
    state
        .db
        .get_setting("alert_rules")
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
    fn parse(s: &str) -> Self {
        if s == "critical" {
            Severity::Critical
        } else {
            Severity::Warning
        }
    }
}

fn level(v: f64, t: &Threshold) -> Option<Severity> {
    if v >= t.crit {
        Some(Severity::Critical)
    } else if v >= t.warn {
        Some(Severity::Warning)
    } else {
        None
    }
}

fn pct(u: u64, t: u64) -> f64 {
    if t > 0 {
        u as f64 * 100.0 / t as f64
    } else {
        0.0
    }
}

/// Instant conditions from the latest sample. `sustained` carries the minimum CPU / memory
/// over each rule's window (None = not enough history yet), so short spikes do not alert.
pub fn evaluate(m: &MetricsSample, r: &AlertRules, sustained_cpu: Option<f64>, sustained_mem: Option<f64>) -> Vec<(String, Severity, String)> {
    let mut out = Vec::new();
    let cpu_v = if r.cpu.for_min == 0 { Some(m.cpu as f64) } else { sustained_cpu };
    if let Some(v) = cpu_v {
        if let Some(sev) = level(v, &r.cpu) {
            let dur = if r.cpu.for_min > 0 { format!(" for {}+ min", r.cpu.for_min) } else { String::new() };
            out.push(("cpu".into(), sev, format!("CPU above {:.0}%{dur} (now {:.0}%)", if sev == Severity::Critical { r.cpu.crit } else { r.cpu.warn }, m.cpu)));
        }
    }
    let mem_now = pct(m.mem_used, m.mem_total);
    let mem_v = if r.mem.for_min == 0 { Some(mem_now) } else { sustained_mem };
    if let Some(v) = mem_v {
        if let Some(sev) = level(v, &r.mem) {
            let dur = if r.mem.for_min > 0 { format!(" for {}+ min", r.mem.for_min) } else { String::new() };
            out.push(("mem".into(), sev, format!("Memory above {:.0}%{dur} (now {mem_now:.0}%)", if sev == Severity::Critical { r.mem.crit } else { r.mem.warn })));
        }
    }
    for d in &m.disks {
        let p = pct(d.used, d.total);
        if let Some(sev) = level(p, &r.disk) {
            out.push((format!("disk:{}", d.mount), sev, format!("Disk {} at {p:.0}% ({} free)", d.mount, human(d.total.saturating_sub(d.used)))));
        }
    }
    if r.containers_unhealthy {
        if let Some(dk) = &m.docker {
            if dk.unhealthy > 0 {
                out.push(("docker:unhealthy".into(), Severity::Warning, format!("{} container(s) unhealthy or restarting", dk.unhealthy)));
            }
        }
    }
    if r.mssql_stopped {
        if let Some(sq) = &m.mssql {
            for i in &sq.instances {
                if i.state == "stopped" {
                    out.push((format!("mssql:{}", i.name), Severity::Critical, format!("SQL Server instance {} is stopped", i.name)));
                }
            }
        }
    }
    if r.k8s_problems {
        if let Some(k) = &m.k8s {
            if !k.not_ready_nodes.is_empty() {
                out.push(("k8s:nodes".into(), Severity::Critical, format!("Kubernetes node(s) not ready: {}", k.not_ready_nodes.join(", "))));
            }
            if k.pods_failing > 0 {
                let more = if k.pods_failing as usize > k.failing.len() { format!(" and {} more", k.pods_failing as usize - k.failing.len()) } else { String::new() };
                out.push(("k8s:pods".into(), Severity::Warning, format!("{} Kubernetes pod(s) failing: {}{more}", k.pods_failing, k.failing.join("; "))));
            }
            if k.workloads_degraded > 0 {
                out.push(("k8s:workloads".into(), Severity::Warning, format!("{} Kubernetes workload(s) have fewer ready replicas than desired", k.workloads_degraded)));
            }
        }
    }
    if r.iis_stopped {
        if let Some(iis) = &m.iis {
            if iis.pools_started < iis.pools_total {
                out.push(("iis:pools".into(), Severity::Warning, format!("{} IIS application pool(s) stopped", iis.pools_total - iis.pools_started)));
            }
            if iis.sites_started < iis.sites_total {
                out.push(("iis:sites".into(), Severity::Warning, format!("{} IIS site(s) stopped", iis.sites_total - iis.sites_started)));
            }
        }
    }
    out
}

fn human(b: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

// =================================================================== evaluator

struct OpenAlert {
    id: i64,
    severity: Severity,
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub kind: &'static str, // firing | resolved | escalated
    pub severity: &'static str,
    pub server_id: String,
    pub server: String,
    pub rule: String,
    pub message: String,
    pub started_at: i64,
}

pub async fn evaluator_loop(state: S) {
    let started = now_ms();
    // Give agents time to reconnect after a hub restart before judging anyone offline.
    tokio::time::sleep(Duration::from_secs(45)).await;
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        if let Err(e) = evaluate_all(&state, started).await {
            tracing::warn!("alert evaluation failed: {e:#}");
        }
    }
}

async fn evaluate_all(state: &S, hub_started: i64) -> Result<()> {
    let rules = load_rules(state).await;
    let agents: Vec<(String, String, Option<i64>, bool, bool)> = state
        .db
        .run(|c| {
            let mut st = c.prepare("SELECT id, name, last_seen_at, is_local, secret_hash IS NOT NULL FROM agents")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    let now = now_ms();
    let mut events = Vec::new();

    for (id, name, last_seen, is_local, enrolled) in agents {
        let mut desired: Vec<(String, Severity, String)> = Vec::new();
        if rules.enabled && enrolled {
            match state.reg.get(&id) {
                Some(conn) => {
                    let latest = conn.latest.read().unwrap().clone();
                    if let Some(m) = latest {
                        let (cpu_win, mem_win) = (rules.cpu.for_min, rules.mem.for_min);
                        let id2 = id.clone();
                        let (scpu, smem) = state
                            .db
                            .run(move |c| {
                                let q = |mins: u32, col: &str| -> rusqlite::Result<Option<f64>> {
                                    if mins == 0 {
                                        return Ok(None);
                                    }
                                    let since = now_ms() - mins as i64 * 60_000;
                                    let (minv, n, first): (Option<f64>, i64, Option<i64>) = c.query_row(
                                        &format!("SELECT MIN({col}), COUNT(*), MIN(ts) FROM metrics WHERE agent_id = ?1 AND ts >= ?2"),
                                        params![id2, since],
                                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                                    )?;
                                    // Require the window to be (nearly) covered by samples.
                                    let covered = first.map(|f| f - since < 60_000).unwrap_or(false);
                                    Ok(if n >= 2 && covered { minv } else { None })
                                };
                                Ok((q(cpu_win, "cpu")?, q(mem_win, "mem_pct")?))
                            })
                            .await?;
                        desired = evaluate(&m, &rules, scpu, smem);
                    }
                }
                None if !is_local => {
                    let reference = last_seen.unwrap_or(0).max(hub_started);
                    let mins = (now - reference) / 60_000;
                    if now - reference > rules.offline_min as i64 * 60_000 {
                        desired.push(("offline".into(), Severity::Critical, format!("Agent offline for {mins} min")));
                    }
                }
                None => {}
            }
        }
        events.extend(reconcile(state, &id, &name, desired).await?);
    }

    if !events.is_empty() {
        let channels = load_channels(state).await?;
        for ev in events {
            for ch in channels.iter().filter(|c| c.enabled) {
                // Same filter for recoveries: a channel only hears about alerts it was told about.
                if Severity::parse(ev.severity) < Severity::parse(&ch.min_severity) {
                    continue;
                }
                let (state2, ch2, ev2) = (state.clone(), ch.clone(), ev.clone());
                tokio::spawn(async move {
                    let res = send(&state2, &ch2, &ev2).await;
                    record_delivery(&state2, ch2.id, res.err().map(|e| e.to_string())).await;
                });
            }
        }
    }
    Ok(())
}

/// Bring stored open alerts for one agent in line with the desired set; returns events to notify.
async fn reconcile(state: &S, agent_id: &str, name: &str, desired: Vec<(String, Severity, String)>) -> Result<Vec<Event>> {
    let id = agent_id.to_string();
    let name = name.to_string();
    state
        .db
        .run(move |c| {
            let now = now_ms();
            let tx = c.transaction()?;
            let mut open: HashMap<String, OpenAlert> = HashMap::new();
            let mut started: HashMap<String, i64> = HashMap::new();
            {
                let mut st = tx.prepare("SELECT id, rule, severity, started_at FROM alerts WHERE agent_id = ?1 AND resolved_at IS NULL")?;
                let rows = st.query_map([&id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))?;
                for row in rows {
                    let (aid, rule, sev, st_at) = row?;
                    started.insert(rule.clone(), st_at);
                    open.insert(rule, OpenAlert { id: aid, severity: Severity::parse(&sev) });
                }
            }
            let mut events = Vec::new();
            for (rule, sev, msg) in &desired {
                match open.remove(rule) {
                    None => {
                        tx.execute(
                            "INSERT INTO alerts (agent_id, rule, severity, message, started_at) VALUES (?1,?2,?3,?4,?5)",
                            params![id, rule, sev.as_str(), msg, now],
                        )?;
                        events.push(Event { kind: "firing", severity: sev.as_str(), server_id: id.clone(), server: name.clone(), rule: rule.clone(), message: msg.clone(), started_at: now });
                    }
                    Some(o) => {
                        tx.execute("UPDATE alerts SET severity = ?1, message = ?2 WHERE id = ?3", params![sev.as_str(), msg, o.id])?;
                        if *sev > o.severity {
                            events.push(Event {
                                kind: "escalated",
                                severity: sev.as_str(),
                                server_id: id.clone(),
                                server: name.clone(),
                                rule: rule.clone(),
                                message: msg.clone(),
                                started_at: started.get(rule).copied().unwrap_or(now),
                            });
                        }
                    }
                }
            }
            for (rule, o) in open {
                let msg: String = tx.query_row("SELECT message FROM alerts WHERE id = ?1", [o.id], |r| r.get(0))?;
                tx.execute("UPDATE alerts SET resolved_at = ?1 WHERE id = ?2", params![now, o.id])?;
                events.push(Event {
                    kind: "resolved",
                    severity: o.severity.as_str(),
                    server_id: id.clone(),
                    server: name.clone(),
                    rule: rule.clone(),
                    message: msg,
                    started_at: started.get(&rule).copied().unwrap_or(now),
                });
            }
            tx.commit()?;
            Ok(events)
        })
        .await
}

// =================================================================== channels

#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub url: String,
    pub enabled: bool,
    pub min_severity: String,
    pub last_error: Option<String>,
    pub last_sent_at: Option<i64>,
}

async fn load_channels(state: &S) -> Result<Vec<Channel>> {
    state
        .db
        .run(|c| {
            let mut st = c.prepare("SELECT id, name, kind, url, enabled, min_severity, last_error, last_sent_at FROM channels ORDER BY id")?;
            let rows = st
                .query_map([], |r| {
                    Ok(Channel {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        kind: r.get(2)?,
                        url: r.get(3)?,
                        enabled: r.get(4)?,
                        min_severity: r.get(5)?,
                        last_error: r.get(6)?,
                        last_sent_at: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
}

async fn record_delivery(state: &S, id: i64, err: Option<String>) {
    if let Some(e) = &err {
        tracing::warn!("alert notification via channel {id} failed: {e}");
    }
    let _ = state
        .db
        .run(move |c| {
            c.execute("UPDATE channels SET last_error = ?1, last_sent_at = ?2 WHERE id = ?3", params![err, now_ms(), id])?;
            Ok(())
        })
        .await;
}

fn hub_link(state: &S, server_id: &str) -> Option<String> {
    state.cfg.public_url.as_ref().map(|u| format!("{}/servers/{server_id}", u.trim_end_matches('/')))
}

pub fn payload(kind: &str, ev: &Event, link: Option<String>, brand: &str) -> Value {
    let icon = match (ev.kind, ev.severity) {
        ("resolved", _) => "✅",
        (_, "critical") => "🔴",
        _ => "🟠",
    };
    let verb = match ev.kind {
        "resolved" => "RESOLVED",
        "escalated" => "ESCALATED",
        _ => "FIRING",
    };
    let title = format!("{icon} {verb} · {} · {}", ev.server, ev.message);
    let when = format!("since {}", fmt_ts(ev.started_at));
    match kind {
        "teams" => {
            let mut body = vec![
                json!({ "type": "TextBlock", "text": format!("{icon} {verb}: {}", ev.server), "weight": "Bolder", "size": "Medium", "wrap": true }),
                json!({ "type": "TextBlock", "text": ev.message, "wrap": true }),
                json!({ "type": "FactSet", "facts": [
                    { "title": "Severity", "value": ev.severity },
                    { "title": "Rule", "value": ev.rule },
                    { "title": "Started", "value": fmt_ts(ev.started_at) },
                ]}),
            ];
            let mut card = json!({ "type": "AdaptiveCard", "$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "version": "1.4", "body": [] });
            if let Some(l) = &link {
                card["actions"] = json!([{ "type": "Action.OpenUrl", "title": format!("Open in {brand}"), "url": l }]);
            }
            card["body"] = Value::Array(std::mem::take(&mut body));
            json!({ "type": "message", "attachments": [{ "contentType": "application/vnd.microsoft.card.adaptive", "content": card }] })
        }
        "slack" => {
            let text = match &link {
                Some(l) => format!("{title} ({when}) <{l}|open>"),
                None => format!("{title} ({when})"),
            };
            json!({ "text": text })
        }
        _ => json!({
            "event": ev.kind,
            "severity": ev.severity,
            "server_id": ev.server_id,
            "server": ev.server,
            "rule": ev.rule,
            "message": ev.message,
            "started_at": ev.started_at,
            "link": link,
            "source": "monitorlab",
            "product": brand,
        }),
    }
}

fn fmt_ts(ms: i64) -> String {
    // UTC, ISO-like; receivers localise as they wish.
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

async fn send(state: &S, ch: &Channel, ev: &Event) -> Result<()> {
    if ch.kind == "email" {
        let smtp = load_smtp(state).await.ok_or_else(|| anyhow::anyhow!("SMTP is not configured (Alerts → Email server)"))?;
        let brand = state.brand();
        let (subject, text, html) = email_content(ev, hub_link(state, &ev.server_id), &brand);
        return send_email(&smtp, &recipients(&ch.url), &subject, text, html, &brand).await;
    }
    let body = payload(&ch.kind, ev, hub_link(state, &ev.server_id), &state.brand());
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).user_agent(format!("monitorlab-hub/{}", monitorlab_common::VERSION)).build()?;
    let resp = client.post(&ch.url).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text: String = resp.text().await.unwrap_or_default().chars().take(200).collect();
        anyhow::bail!("HTTP {status}: {text}");
    }
    Ok(())
}

// =================================================================== email (SMTP)

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    /// "starttls" (587), "tls" (implicit, 465) or "none" (25, trusted networks only).
    pub security: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    pub from_address: String,
    #[serde(default)]
    pub from_name: String,
    /// Accept self-signed / internal-CA certificates on the mail server.
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

pub fn recipients(s: &str) -> Vec<String> {
    s.split([',', ';', '\n']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

async fn load_smtp(state: &S) -> Option<SmtpConfig> {
    state.db.get_setting("smtp").await.ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).filter(|c: &SmtpConfig| !c.host.is_empty())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn email_content(ev: &Event, link: Option<String>, brand: &str) -> (String, String, String) {
    let verb = match ev.kind {
        "resolved" => "RESOLVED",
        "escalated" => "ESCALATED",
        _ => "FIRING",
    };
    let subject = format!("[{brand}] {verb} {} · {} · {}", ev.severity, ev.server, ev.message);
    let mut text = format!(
        "{verb}: {}\n\nServer:   {}\nSeverity: {}\nRule:     {}\nStarted:  {}\n",
        ev.message, ev.server, ev.severity, ev.rule, fmt_ts(ev.started_at)
    );
    if let Some(l) = &link {
        text.push_str(&format!("\nOpen in {brand}: {l}\n"));
    }
    let color = match (ev.kind, ev.severity) {
        ("resolved", _) => "#146c2e",
        (_, "critical") => "#b3261e",
        _ => "#8a5300",
    };
    let button = link
        .map(|l| format!(r#"<p style="margin:24px 0 0"><a href="{}" style="background:#0b57d0;color:#fff;padding:10px 20px;border-radius:999px;text-decoration:none;font-weight:500">Open in {}</a></p>"#, esc(&l), esc(brand)))
        .unwrap_or_default();
    let html = format!(
        r#"<div style="font-family:Segoe UI,Roboto,Arial,sans-serif;color:#1f1f1f;max-width:560px">
<div style="font-size:12px;font-weight:600;letter-spacing:.06em;color:{color}">{verb} · {sev}</div>
<h2 style="font-weight:500;margin:6px 0 16px">{msg}</h2>
<table style="border-collapse:collapse;font-size:14px">
<tr><td style="color:#444746;padding:4px 16px 4px 0">Server</td><td>{server}</td></tr>
<tr><td style="color:#444746;padding:4px 16px 4px 0">Rule</td><td>{rule}</td></tr>
<tr><td style="color:#444746;padding:4px 16px 4px 0">Started</td><td>{started}</td></tr>
</table>{button}
<p style="color:#6b6f73;font-size:12px;margin-top:28px">Sent by {brand_html}.</p></div>"#,
        brand_html = esc(brand),
        sev = esc(ev.severity),
        msg = esc(&ev.message),
        server = esc(&ev.server),
        rule = esc(&ev.rule),
        started = fmt_ts(ev.started_at),
    );
    (subject, text, html)
}

pub async fn send_email(cfg: &SmtpConfig, to: &[String], subject: &str, text: String, html: String, brand: &str) -> Result<()> {
    use lettre::message::{Mailbox, MultiPart};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::transport::smtp::client::{Tls, TlsParameters};
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    if to.is_empty() {
        anyhow::bail!("no recipients");
    }
    let from = Mailbox::new(
        Some(if cfg.from_name.trim().is_empty() { format!("{brand} Alerts") } else { cfg.from_name.trim().to_string() }),
        cfg.from_address.trim().parse()?,
    );
    let mut builder = Message::builder().from(from).subject(subject);
    for r in to {
        builder = builder.to(r.parse::<Mailbox>()?);
    }
    let msg = builder.multipart(MultiPart::alternative_plain_html(text, html))?;

    let params = TlsParameters::builder(cfg.host.clone())
        .dangerous_accept_invalid_certs(cfg.accept_invalid_certs)
        .dangerous_accept_invalid_hostnames(cfg.accept_invalid_certs)
        .build_rustls()?;
    let tls = match cfg.security.as_str() {
        "tls" => Tls::Wrapper(params),
        "none" => Tls::None,
        _ => Tls::Required(params),
    };
    let mut t = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(cfg.host.as_str())
        .port(cfg.port)
        .tls(tls)
        .timeout(Some(Duration::from_secs(20)));
    if !cfg.username.is_empty() {
        t = t.credentials(Credentials::new(cfg.username.clone(), cfg.password.clone()));
    }
    t.build().send(msg).await?;
    Ok(())
}

pub async fn get_smtp(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let c = load_smtp(&state).await.unwrap_or(SmtpConfig { port: 587, security: "starttls".into(), ..Default::default() });
    Ok(Json(json!({
        "host": c.host, "port": c.port, "security": c.security, "username": c.username,
        "password_set": !c.password.is_empty(), "from_address": c.from_address, "from_name": c.from_name,
        "accept_invalid_certs": c.accept_invalid_certs,
    })))
}

#[derive(Deserialize)]
pub struct SmtpBody {
    host: String,
    port: u16,
    security: String,
    #[serde(default)]
    username: String,
    /// Empty / missing keeps the stored password.
    #[serde(default)]
    password: Option<String>,
    from_address: String,
    #[serde(default)]
    from_name: String,
    #[serde(default)]
    accept_invalid_certs: bool,
}

pub async fn put_smtp(State(state): State<S>, user: AuthUser, Json(b): Json<SmtpBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let host = b.host.trim().to_string();
    if host.is_empty() || host.len() > 253 || host.contains(char::is_whitespace) || host.contains('/') {
        return Err(ApiError::bad("Enter the mail server host name, e.g. mail.example.com"));
    }
    if b.port == 0 {
        return Err(ApiError::bad("Enter a valid port"));
    }
    if !matches!(b.security.as_str(), "starttls" | "tls" | "none") {
        return Err(ApiError::bad("Security must be starttls, tls or none"));
    }
    if b.from_address.trim().parse::<lettre::Address>().is_err() {
        return Err(ApiError::bad("Enter a valid sender address"));
    }
    let old = load_smtp(&state).await.unwrap_or_default();
    let password = match b.password {
        Some(p) if !p.is_empty() => p,
        _ => if b.username.trim().is_empty() { String::new() } else { old.password },
    };
    let cfg = SmtpConfig {
        host,
        port: b.port,
        security: b.security,
        username: b.username.trim().to_string(),
        password,
        from_address: b.from_address.trim().to_string(),
        from_name: b.from_name.trim().chars().take(80).collect(),
        accept_invalid_certs: b.accept_invalid_certs,
    };
    state.db.set_setting("smtp", serde_json::to_string(&cfg).map_err(|e| anyhow::anyhow!(e))?).await?;
    audit(&state, Some(&user), "", AuditEntry { action: "smtp_updated", server: None, target: Some(format!("{}:{}", cfg.host, cfg.port)), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SmtpTest {
    to: String,
}

pub async fn test_smtp(State(state): State<S>, user: AuthUser, Json(b): Json<SmtpTest>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let cfg = load_smtp(&state).await.ok_or_else(|| ApiError::bad("Save the email server settings first"))?;
    let to = recipients(&b.to);
    if to.is_empty() || to.iter().any(|r| r.parse::<lettre::Address>().is_err()) {
        return Err(ApiError::bad("Enter a valid recipient address"));
    }
    let ev = Event {
        kind: "firing",
        severity: "warning",
        server_id: "test".into(),
        server: format!("{} test", state.brand()),
        rule: "test".into(),
        message: format!("Test email sent by {}", user.email),
        started_at: now_ms(),
    };
    let brand = state.brand();
    let (subject, text, html) = email_content(&ev, state.cfg.public_url.clone(), &brand);
    send_email(&cfg, &to, &subject, text, html, &brand)
        .await
        .map_err(|e| ApiError(axum::http::StatusCode::BAD_GATEWAY, format!("Sending failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

// =================================================================== API

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    server: Option<String>,
}

pub async fn list(State(state): State<S>, user: AuthUser, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    user.require(rbac::SERVERS_VIEW)?;
    let active = q.state.as_deref() != Some("all");
    let limit = q.limit.unwrap_or(200).clamp(1, 500);
    let before = q.before.unwrap_or(i64::MAX);
    let server = q.server.clone();
    let rows: Vec<Value> = state
        .db
        .run(move |c| {
            let mut st = c.prepare(
                "SELECT a.id, a.agent_id, COALESCE(g.name, ''), a.rule, a.severity, a.message, a.started_at, a.resolved_at, a.acked_by, a.acked_at \
                 FROM alerts a LEFT JOIN agents g ON g.id = a.agent_id \
                 WHERE a.id < ?1 AND (?2 = 0 OR a.resolved_at IS NULL) AND (?3 IS NULL OR a.agent_id = ?3) \
                 ORDER BY a.resolved_at IS NOT NULL, CASE a.severity WHEN 'critical' THEN 0 ELSE 1 END, a.id DESC LIMIT ?4",
            )?;
            let rows = st
                .query_map(params![before, active as i64, server, limit], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?,
                        "server_id": r.get::<_, String>(1)?,
                        "server": r.get::<_, String>(2)?,
                        "rule": r.get::<_, String>(3)?,
                        "severity": r.get::<_, String>(4)?,
                        "message": r.get::<_, String>(5)?,
                        "started_at": r.get::<_, i64>(6)?,
                        "resolved_at": r.get::<_, Option<i64>>(7)?,
                        "acked_by": r.get::<_, Option<String>>(8)?,
                        "acked_at": r.get::<_, Option<i64>>(9)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await?;
    let rows: Vec<Value> = rows.into_iter().filter(|r| user.can_server(r["server_id"].as_str().unwrap_or(""))).collect();
    Ok(Json(Value::Array(rows)))
}

pub async fn ack(State(state): State<S>, user: AuthUser, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let email = user.email.clone();
    let row: Option<(String, String)> = state
        .db
        .run(move |c| {
            let r: Option<(String, String)> = c.query_row("SELECT agent_id, message FROM alerts WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if r.is_some() {
                c.execute("UPDATE alerts SET acked_by = ?1, acked_at = ?2 WHERE id = ?3", params![email, now_ms(), id])?;
            }
            Ok(r)
        })
        .await?;
    let Some((agent, msg)) = row else { return Err(ApiError::not_found("Alert not found")) };
    if !user.can_server(&agent) {
        return Err(ApiError::not_found("Alert not found"));
    }
    audit(&state, Some(&user), "", AuditEntry { action: "alert_acknowledged", server: None, target: Some(msg), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn get_rules(State(state): State<S>, user: AuthUser) -> ApiResult<Json<AlertRules>> {
    user.require(rbac::SERVERS_VIEW)?;
    Ok(Json(load_rules(&state).await))
}

pub async fn put_rules(State(state): State<S>, user: AuthUser, Json(r): Json<AlertRules>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    r.validate().map_err(ApiError::bad)?;
    let text = serde_json::to_string(&r).map_err(|e| anyhow::anyhow!(e))?;
    state.db.set_setting("alert_rules", text.clone()).await?;
    *state.rules.write().unwrap() = r;
    audit(&state, Some(&user), "", AuditEntry { action: "alert_rules_updated", server: None, target: None, detail: Some(text), ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_channels(State(state): State<S>, user: AuthUser) -> ApiResult<Json<Vec<Channel>>> {
    user.require(rbac::ALERTS_MANAGE)?;
    Ok(Json(load_channels(&state).await?))
}

#[derive(Deserialize)]
pub struct ChannelBody {
    name: String,
    kind: String,
    url: String,
    #[serde(default = "d_true")]
    enabled: bool,
    #[serde(default = "d_warning")]
    min_severity: String,
}
fn d_true() -> bool {
    true
}
fn d_warning() -> String {
    "warning".into()
}

fn validate_channel(b: &ChannelBody) -> ApiResult<()> {
    if b.name.trim().is_empty() || b.name.chars().count() > 64 {
        return Err(ApiError::bad("Name must be 1–64 characters"));
    }
    if !matches!(b.kind.as_str(), "email" | "teams" | "slack" | "webhook") {
        return Err(ApiError::bad("Type must be email, teams, slack or webhook"));
    }
    let u = b.url.trim();
    if b.kind == "email" {
        let list = recipients(u);
        if list.is_empty() || list.len() > 50 || list.iter().any(|r| r.parse::<lettre::Address>().is_err()) {
            return Err(ApiError::bad("Enter one or more valid email addresses, separated by commas"));
        }
    } else if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 2048 || u.contains(char::is_whitespace) {
        return Err(ApiError::bad("Enter a valid http(s) webhook URL"));
    }
    if !matches!(b.min_severity.as_str(), "warning" | "critical") {
        return Err(ApiError::bad("Minimum severity must be warning or critical"));
    }
    Ok(())
}

pub async fn create_channel(State(state): State<S>, user: AuthUser, Json(b): Json<ChannelBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    validate_channel(&b)?;
    let name = b.name.trim().to_string();
    let n2 = name.clone();
    let id = state
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO channels (name, kind, url, enabled, min_severity, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![n2, b.kind, b.url.trim(), b.enabled, b.min_severity, now_ms()],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await?;
    audit(&state, Some(&user), "", AuditEntry { action: "channel_created", server: None, target: Some(name), detail: None, ok: true }).await;
    Ok(Json(json!({ "id": id })))
}

pub async fn update_channel(State(state): State<S>, user: AuthUser, Path(id): Path<i64>, Json(b): Json<ChannelBody>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    validate_channel(&b)?;
    let name = b.name.trim().to_string();
    let n2 = name.clone();
    let n = state
        .db
        .run(move |c| {
            Ok(c.execute(
                "UPDATE channels SET name = ?1, kind = ?2, url = ?3, enabled = ?4, min_severity = ?5 WHERE id = ?6",
                params![n2, b.kind, b.url.trim(), b.enabled, b.min_severity, id],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(ApiError::not_found("Channel not found"));
    }
    audit(&state, Some(&user), "", AuditEntry { action: "channel_updated", server: None, target: Some(name), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_channel(State(state): State<S>, user: AuthUser, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let n = state.db.run(move |c| Ok(c.execute("DELETE FROM channels WHERE id = ?1", [id])?)).await?;
    if n == 0 {
        return Err(ApiError::not_found("Channel not found"));
    }
    audit(&state, Some(&user), "", AuditEntry { action: "channel_deleted", server: None, target: Some(id.to_string()), detail: None, ok: true }).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn test_channel(State(state): State<S>, user: AuthUser, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    user.require(rbac::ALERTS_MANAGE)?;
    let ch = load_channels(&state).await?.into_iter().find(|c| c.id == id).ok_or_else(|| ApiError::not_found("Channel not found"))?;
    let ev = Event {
        kind: "firing",
        severity: "warning",
        server_id: "test".into(),
        server: format!("{} test", state.brand()),
        rule: "test".into(),
        message: format!("Test notification sent by {}", user.email),
        started_at: now_ms(),
    };
    let res = send(&state, &ch, &ev).await;
    let err = res.as_ref().err().map(|e| format!("{e}"));
    record_delivery(&state, id, err.clone()).await;
    match err {
        None => Ok(Json(json!({ "ok": true }))),
        Some(e) => Err(ApiError(axum::http::StatusCode::BAD_GATEWAY, format!("Delivery failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monitorlab_common::{DiskInfo, DockerSummary, MssqlInstanceSummary, MssqlSummary};

    fn sample() -> MetricsSample {
        MetricsSample { cpu: 50.0, mem_total: 100, mem_used: 50, ..Default::default() }
    }

    #[test]
    fn spikes_do_not_alert_without_sustained_history() {
        let r = AlertRules::default();
        let mut m = sample();
        m.cpu = 99.0;
        assert!(evaluate(&m, &r, None, None).is_empty());
        assert!(evaluate(&m, &r, Some(40.0), None).is_empty()); // min over window below threshold
        let v = evaluate(&m, &r, Some(96.0), None);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1, Severity::Critical);
    }

    #[test]
    fn disk_docker_mssql() {
        let r = AlertRules::default();
        let mut m = sample();
        m.disks = vec![DiskInfo { mount: "/".into(), total: 100, used: 90, ..Default::default() }];
        m.docker = Some(DockerSummary { unhealthy: 2, ..Default::default() });
        m.mssql = Some(MssqlSummary { instances: vec![MssqlInstanceSummary { name: "MSSQLSERVER".into(), state: "stopped".into(), ..Default::default() }] });
        let v = evaluate(&m, &r, None, None);
        let keys: Vec<_> = v.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(keys, vec!["disk:/", "docker:unhealthy", "mssql:MSSQLSERVER"]);
        assert_eq!(v[0].1, Severity::Warning);
    }

    #[test]
    fn rules_validation() {
        let mut r = AlertRules::default();
        assert!(r.validate().is_ok());
        r.cpu.warn = 99.0;
        assert!(r.validate().is_err());
        let mut r = AlertRules::default();
        r.offline_min = 0;
        assert!(r.validate().is_err());
    }

    #[test]
    fn payload_shapes() {
        let ev = Event { kind: "firing", severity: "critical", server_id: "x".into(), server: "web1".into(), rule: "cpu".into(), message: "CPU high".into(), started_at: 0 };
        assert!(payload("teams", &ev, None, "MonitorLab")["attachments"][0]["content"]["body"].is_array());
        assert!(payload("slack", &ev, Some("https://h/servers/x".into()), "MonitorLab")["text"].as_str().unwrap().contains("|open>"));
        assert_eq!(payload("webhook", &ev, None, "MonitorLab")["severity"], "critical");
        assert_eq!(fmt_ts(0), "1970-01-01 00:00:00 UTC");
        let (subj, text, html) = email_content(&ev, Some("https://h/servers/x".into()), "MonitorLab");
        assert!(subj.starts_with("[MonitorLab] FIRING critical · web1"));
        assert!(text.contains("Open in MonitorLab: https://h/servers/x"));
        assert!(html.contains("CPU high") && !html.contains("<script"));
        assert_eq!(recipients("a@x.com, b@y.com;\nc@z.com"), vec!["a@x.com", "b@y.com", "c@z.com"]);
        assert_eq!(fmt_ts(1_700_000_000_000), "2023-11-14 22:13:20 UTC");
    }
}
