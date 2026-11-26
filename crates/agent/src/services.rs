//! System services and system logs.
//!
//! Linux: systemd over D-Bus (works for a native agent and for the hub's built-in agent,
//! which reaches the host's bus through `<host_root>/run/dbus/system_bus_socket`) and
//! journald through `journalctl -o json`.
//! Windows: services and the Event Log through PowerShell (CIM / Get-WinEvent).

use anyhow::{anyhow, bail, Result};
use monitorlab_common::{AgentMsg, LogLine, ServiceActionKind, StreamData};
use serde_json::{json, Value};
use tokio::sync::mpsc;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Services {
    host_root: Option<String>,
}

/// journald priority → (stream, text level) helpers shared by both platforms.
fn line(ts: Option<String>, text: String, src: Option<String>, lvl: Option<u8>) -> LogLine {
    LogLine { ts, s: if lvl.map(|l| l <= 3).unwrap_or(false) { 2 } else { 1 }, text, src, lvl }
}

/// Unix µs → RFC 3339 with microseconds (the log viewer parses it).
pub fn rfc3339_us(us: i64) -> String {
    let secs = us.div_euclid(1_000_000);
    let frac = us.rem_euclid(1_000_000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{frac:06}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Units the hub refuses to stop / disable: losing them can lock you out of the server.
pub fn protected(name: &str, is_hub: bool) -> bool {
    let n = name.trim_end_matches(".service");
    n.starts_with("systemd-")
        || n.starts_with("dbus")
        || n == "ssh"
        || n == "sshd"
        || n.starts_with("monitorlab")
        || n == "MonitorLabAgent"
        || n == "RpcSs"
        || n == "WinRM"
        || n == "TermService"
        || (is_hub && (n == "docker" || n == "containerd"))
}

/// "90" -> "1min 30s"; used for systemd's monotonic timer values.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn human_span(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    let mut p = Vec::new();
    for (v, u) in [(d, "d"), (h, "h"), (m, "min"), (s, "s")] {
        if v > 0 {
            p.push(format!("{v}{u}"));
        }
    }
    if p.is_empty() {
        "0s".into()
    } else {
        p.join(" ")
    }
}

// ====================================================================== Linux

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    type UnitRow = (String, String, String, String, String, String, OwnedObjectPath, u32, String, OwnedObjectPath);

    const DEST: &str = "org.freedesktop.systemd1";
    const PATH: &str = "/org/freedesktop/systemd1";
    const MGR: &str = "org.freedesktop.systemd1.Manager";

    impl Services {
        pub fn new(host_root: Option<String>) -> Self {
            Self { host_root }
        }

        fn is_hub(&self) -> bool {
            self.host_root.as_deref().map(|r| !r.is_empty() && r != "/").unwrap_or(false)
        }

        async fn bus(&self) -> Result<zbus::Connection> {
            let addr = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").ok().or_else(|| {
                let p = crate::util::host_path(self.host_root.as_deref(), "/run/dbus/system_bus_socket");
                (self.is_hub() && p.exists()).then(|| format!("unix:path={}", p.display()))
            });
            let fut = async {
                Ok::<_, anyhow::Error>(match addr {
                    Some(a) => zbus::connection::Builder::address(a.as_str())?.build().await?,
                    None => zbus::Connection::system().await?,
                })
            };
            tokio::time::timeout(Duration::from_secs(5), fut).await.map_err(|_| anyhow!("system D-Bus did not answer"))?
        }

        async fn units(conn: &zbus::Connection) -> Result<Vec<UnitRow>> {
            let reply = conn.call_method(Some(DEST), PATH, Some(MGR), "ListUnits", &()).await?;
            Ok(reply.body().deserialize()?)
        }

        pub async fn available(&self) -> bool {
            match self.bus().await {
                Ok(c) => Self::units(&c).await.is_ok(),
                Err(_) => false,
            }
        }

        pub async fn failed(&self) -> Option<Vec<String>> {
            let c = self.bus().await.ok()?;
            let units = Self::units(&c).await.ok()?;
            Some(units.into_iter().filter(|u| u.3 == "failed").map(|u| u.0).collect())
        }

        pub async fn list(&self) -> Result<Value> {
            let conn = self.bus().await?;
            let units = Self::units(&conn).await?;
            let files: Vec<(String, String)> = conn
                .call_method(Some(DEST), PATH, Some(MGR), "ListUnitFiles", &())
                .await?
                .body()
                .deserialize()
                .unwrap_or_default();
            let file_state: HashMap<String, String> =
                files.into_iter().filter_map(|(p, st)| p.rsplit('/').next().map(|n| (n.to_string(), st))).collect();
            let mut seen = std::collections::HashSet::new();
            let mut rows = Vec::new();
            for (name, desc, load, active, sub, _, path, _, _, _) in units.into_iter().filter(|u| u.0.ends_with(".service")) {
                seen.insert(name.clone());
                let mut row = json!({
                    "name": name, "description": desc, "load": load, "active": active, "sub": sub,
                    "enabled": file_state.get(&name).cloned().unwrap_or_else(|| "static".into()),
                    "protected": protected(&name, self.is_hub()),
                });
                if active == "active" || active == "failed" || active == "activating" {
                    let props = async {
                        let r = conn.call_method(Some(DEST), path.as_str(), Some("org.freedesktop.DBus.Properties"), "GetAll", &("org.freedesktop.systemd1.Service")).await?;
                        Ok::<HashMap<String, OwnedValue>, anyhow::Error>(r.body().deserialize()?)
                    };
                    if let Ok(Ok(p)) = tokio::time::timeout(Duration::from_secs(2), props).await {
                        let num = |k: &str| p.get(k).and_then(|v| u64::try_from(v).ok()).filter(|v| *v != u64::MAX);
                        row["mem"] = json!(num("MemoryCurrent"));
                        row["cpu_ns"] = json!(num("CPUUsageNSec"));
                        row["tasks"] = json!(num("TasksCurrent"));
                        row["pid"] = json!(p.get("MainPID").and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0));
                        row["restarts"] = json!(p.get("NRestarts").and_then(|v| u32::try_from(v).ok()));
                        row["result"] = json!(p.get("Result").and_then(|v| <&str>::try_from(v).ok()));
                    }
                    if let Ok(Ok(r)) = tokio::time::timeout(
                        Duration::from_secs(2),
                        conn.call_method(Some(DEST), path.as_str(), Some("org.freedesktop.DBus.Properties"), "Get", &("org.freedesktop.systemd1.Unit", "ActiveEnterTimestamp")),
                    )
                    .await
                    {
                        if let Ok(v) = r.body().deserialize::<zbus::zvariant::OwnedValue>() {
                            if let Ok(us) = u64::try_from(&v) {
                                if us > 0 {
                                    row["since"] = json!(us / 1000);
                                }
                            }
                        }
                    }
                }
                rows.push(row);
            }
            // Installed but never loaded (disabled) services.
            for (name, st) in file_state {
                if name.ends_with(".service") && !name.contains('@') && !seen.contains(&name) {
                    rows.push(json!({ "name": name, "description": "", "load": "not-loaded", "active": "inactive", "sub": "dead", "enabled": st, "protected": protected(&name, self.is_hub()) }));
                }
            }
            rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            Ok(json!({ "platform": "systemd", "services": rows }))
        }

        /// systemd timers with their next / last run, the service they start and its last result.
        pub async fn timers(&self) -> Result<Vec<Value>> {
            use zbus::zvariant::Value as ZV;
            let conn = match self.bus().await {
                Ok(c) => c,
                Err(_) => return Ok(vec![]), // no systemd: cron entries may still be available
            };
            let units = Self::units(&conn).await?;
            let files: Vec<(String, String)> = conn.call_method(Some(DEST), PATH, Some(MGR), "ListUnitFiles", &()).await?.body().deserialize().unwrap_or_default();
            let mut file_path: HashMap<String, String> = HashMap::new();
            let mut file_state: HashMap<String, String> = HashMap::new();
            for (p, st) in files {
                if let Some(n) = p.rsplit('/').next() {
                    file_path.insert(n.to_string(), p.clone());
                    file_state.insert(n.to_string(), st);
                }
            }
            let by_name: HashMap<&str, &UnitRow> = units.iter().map(|u| (u.0.as_str(), u)).collect();
            let root = self.host_root.clone();
            let unit_file = |name: &str| -> (Option<String>, Option<String>, Option<String>) {
                // (ExecStart, User, Description) from the unit file, if readable.
                let Some(p) = file_path.get(name) else { return (None, None, None) };
                let text = std::fs::read_to_string(crate::util::host_path(root.as_deref(), p)).unwrap_or_default();
                let find = |k: &str| text.lines().find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim().to_string())).filter(|v| !v.is_empty());
                (find("ExecStart="), find("User="), find("Description="))
            };
            let props = |path: &str, iface: &str| {
                let conn = conn.clone();
                let path = path.to_string();
                let iface = iface.to_string();
                async move {
                    let r = tokio::time::timeout(Duration::from_secs(2), conn.call_method(Some(DEST), path.as_str(), Some("org.freedesktop.DBus.Properties"), "GetAll", &(iface.as_str(),))).await.ok()?.ok()?;
                    r.body().deserialize::<HashMap<String, OwnedValue>>().ok()
                }
            };
            let mut out = Vec::new();
            for u in units.iter().filter(|u| u.0.ends_with(".timer")) {
                let name = u.0.clone();
                let Some(p) = props(u.6.as_str(), "org.freedesktop.systemd1.Timer").await else { continue };
                let num = |k: &str| p.get(k).and_then(|v| u64::try_from(v).ok()).filter(|v| *v != 0 && *v != u64::MAX);
                let service = p.get("Unit").and_then(|v| <&str>::try_from(v).ok()).map(String::from).unwrap_or_else(|| name.replace(".timer", ".service"));
                let mut specs: Vec<String> = Vec::new();
                if let Some(v) = p.get("TimersCalendar") {
                    if let ZV::Array(a) = &**v {
                        for e in a.iter() {
                            if let ZV::Structure(s) = e {
                                if let Some(sp) = s.fields().get(1).and_then(|f| <&str>::try_from(f).ok()) {
                                    specs.push(format!("OnCalendar={sp}"));
                                }
                            }
                        }
                    }
                }
                if let Some(v) = p.get("TimersMonotonic") {
                    if let ZV::Array(a) = &**v {
                        for e in a.iter() {
                            if let ZV::Structure(s) = e {
                                let f = s.fields();
                                if let (Some(base), Some(us)) = (f.first().and_then(|x| <&str>::try_from(x).ok()), f.get(1).and_then(|x| u64::try_from(x).ok())) {
                                    specs.push(format!("{base}={}", human_span(us / 1_000_000)));
                                }
                            }
                        }
                    }
                }
                let (exec, user, desc) = unit_file(&service);
                let (_, _, tdesc) = unit_file(&name);
                // Result of the service the timer starts, when systemd still has it loaded.
                let mut last_result = None;
                let mut exit_ms = None;
                let mut svc_state = None;
                if let Some(su) = by_name.get(service.as_str()) {
                    svc_state = Some(format!("{} ({})", su.3, su.4));
                    if let Some(sp) = props(su.6.as_str(), "org.freedesktop.systemd1.Service").await {
                        last_result = sp.get("Result").and_then(|v| <&str>::try_from(v).ok()).map(String::from);
                        exit_ms = sp.get("ExecMainExitTimestamp").and_then(|v| u64::try_from(v).ok()).filter(|v| *v > 0 && *v != u64::MAX).map(|v| v / 1000);
                        if let (Some("success"), Some(code)) = (last_result.as_deref(), sp.get("ExecMainStatus").and_then(|v| i32::try_from(v).ok())) {
                            if code != 0 {
                                last_result = Some(format!("exit {code}"));
                            }
                        }
                    }
                }
                let last_trigger = num("LastTriggerUSec").map(|v| v / 1000);
                out.push(json!({
                    "id": format!("timer:{name}"), "kind": "timer", "name": name.trim_end_matches(".timer"),
                    "description": if !u.1.is_empty() { u.1.clone() } else { tdesc.or(desc).unwrap_or_default() },
                    "command": exec.unwrap_or_default(), "user": user.unwrap_or_else(|| "root".into()),
                    "schedule": specs.join("; "), "summary": "",
                    "source": file_path.get(&name).cloned().unwrap_or_default(),
                    "unit": service, "timer_unit": name,
                    "enabled": file_state.get(&name).map(|s| s == "enabled" || s == "static" || s == "enabled-runtime").unwrap_or(true),
                    "state": svc_state.unwrap_or_else(|| u.3.clone()),
                    "next_run": num("NextElapseUSecRealtime").map(|v| v / 1000), "next_runs": [],
                    "last_run": last_trigger.or(exit_ms), "last_result": last_result,
                }));
            }
            out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            Ok(out)
        }

        pub async fn task_action(&self, _: &str, _: ServiceActionKind) -> Result<Value> {
            bail!("use the timer's service for systemd units")
        }

        pub async fn action(&self, name: &str, action: ServiceActionKind) -> Result<Value> {
            if name.is_empty() || name.contains('/') || name.len() > 256 {
                bail!("invalid unit name");
            }
            let unit = if name.contains('.') { name.to_string() } else { format!("{name}.service") };
            let risky = matches!(action, ServiceActionKind::Stop | ServiceActionKind::Disable)
                || (self.is_hub() && matches!(action, ServiceActionKind::Restart));
            if risky && protected(&unit, self.is_hub()) {
                bail!("{unit} is protected: stopping it could cut off access to this server (or stop MonitorLab itself). Do this on the server itself if you really need to.");
            }
            let conn = self.bus().await?;
            let args = (unit.as_str(), "replace");
            let job = |m: &'static str| conn.call_method(Some(DEST), PATH, Some(MGR), m, &args);
            let r = match action {
                ServiceActionKind::Start => job("StartUnit").await.map(|_| ()),
                ServiceActionKind::Stop => job("StopUnit").await.map(|_| ()),
                ServiceActionKind::Restart => job("RestartUnit").await.map(|_| ()),
                ServiceActionKind::Reload => job("ReloadUnit").await.map(|_| ()),
                ServiceActionKind::Enable => conn.call_method(Some(DEST), PATH, Some(MGR), "EnableUnitFiles", &(vec![unit.as_str()], false, false)).await.map(|_| ()),
                ServiceActionKind::Disable => conn.call_method(Some(DEST), PATH, Some(MGR), "DisableUnitFiles", &(vec![unit.as_str()], false)).await.map(|_| ()),
            };
            if let Err(e) = r {
                let msg = e.to_string();
                if msg.contains("AccessDenied") || msg.contains("Interactive authentication required") {
                    bail!("systemd refused: the agent is not allowed to manage services (re-run the agent installer to add the permission)");
                }
                bail!("systemd: {msg}");
            }
            if matches!(action, ServiceActionKind::Enable | ServiceActionKind::Disable) {
                let _ = conn.call_method(Some(DEST), PATH, Some(MGR), "Reload", &()).await;
            }
            // Give the job a moment so the next listing shows the new state.
            tokio::time::sleep(Duration::from_millis(800)).await;
            Ok(json!({ "ok": true }))
        }

        fn journalctl() -> String {
            std::env::var("MONITORLAB_JOURNALCTL").unwrap_or_else(|_| "journalctl".into())
        }

        pub async fn logs_available(&self) -> bool {
            tokio::process::Command::new(Self::journalctl())
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        }

        #[allow(clippy::too_many_arguments)]
        pub async fn stream_logs(
            &self,
            unit: Option<String>,
            priority: Option<u8>,
            tail: u32,
            since: Option<i64>,
            follow: bool,
            grep: Option<String>,
            sid: u64,
            out: &mpsc::Sender<AgentMsg>,
        ) -> Result<()> {
            let mut cmd = tokio::process::Command::new(Self::journalctl());
            cmd.args(["-o", "json", "--no-pager", "--output-fields=MESSAGE,PRIORITY,_SYSTEMD_UNIT,SYSLOG_IDENTIFIER,_COMM"]);
            if self.is_hub() {
                cmd.arg("--root").arg(self.host_root.as_deref().unwrap_or("/"));
            }
            if let Some(u) = unit.filter(|u| !u.is_empty()) {
                if u.starts_with('-') {
                    bail!("invalid unit");
                }
                cmd.arg("-u").arg(u);
            }
            if let Some(p) = priority {
                cmd.arg("-p").arg(p.min(7).to_string());
            }
            if let Some(g) = grep.filter(|g| !g.is_empty()) {
                // Literal match: escape regex metacharacters (journalctl --grep takes a PCRE).
                let mut esc = String::new();
                for c in g.chars().take(500) {
                    if !c.is_alphanumeric() && c != ' ' && c != '_' {
                        esc.push('\\');
                    }
                    esc.push(c);
                }
                cmd.arg("--case-sensitive=yes").arg("-g").arg(esc);
            }
            match since {
                Some(t) => {
                    cmd.arg("--since").arg(format!("@{t}"));
                }
                None => {
                    cmd.arg("-n").arg(tail.clamp(1, 50_000).to_string());
                }
            }
            if follow {
                cmd.arg("-f");
            }
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let mut child = cmd.spawn().map_err(|e| anyhow!("cannot run journalctl: {e}"))?;
            let stdout = child.stdout.take().ok_or_else(|| anyhow!("no output"))?;
            let mut lines = BufReader::new(stdout).lines();
            let mut batch: Vec<LogLine> = Vec::new();
            let mut tick = tokio::time::interval(Duration::from_millis(150));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    l = lines.next_line() => match l? {
                        Some(l) => {
                            if let Some(ll) = parse_journal_json(&l) {
                                batch.push(ll);
                            }
                            if batch.len() >= 500 {
                                send(out, sid, &mut batch).await?;
                            }
                        }
                        None => {
                            send(out, sid, &mut batch).await?;
                            let st = child.wait_with_output().await?;
                            if !st.status.success() {
                                let err = String::from_utf8_lossy(&st.stderr).trim().to_string();
                                if !err.is_empty() {
                                    bail!("journalctl: {err}");
                                }
                            }
                            return Ok(());
                        }
                    },
                    _ = tick.tick() => send(out, sid, &mut batch).await?,
                }
            }
        }
    }

    pub fn parse_journal_json(l: &str) -> Option<LogLine> {
        let v: Value = serde_json::from_str(l).ok()?;
        let us: i64 = v.get("__REALTIME_TIMESTAMP")?.as_str()?.parse().ok()?;
        let msg = match v.get("MESSAGE") {
            Some(Value::String(s)) => s.clone(),
            // Binary / non-UTF-8 messages come as byte arrays.
            Some(Value::Array(a)) => String::from_utf8_lossy(&a.iter().filter_map(|x| x.as_u64().map(|b| b as u8)).collect::<Vec<u8>>()).into_owned(),
            _ => String::new(),
        };
        let prio = v.get("PRIORITY").and_then(Value::as_str).and_then(|p| p.parse::<u8>().ok());
        let src = ["_SYSTEMD_UNIT", "SYSLOG_IDENTIFIER", "_COMM"].iter().find_map(|k| v.get(*k).and_then(Value::as_str).map(str::to_string));
        Some(line(Some(rfc3339_us(us)), msg.trim_end().to_string(), src, prio))
    }
}

// ====================================================================== Windows

#[cfg(windows)]
mod imp {
    use super::*;
    use std::time::Duration;

    async fn ps(script: &str) -> Result<String> {
        let script = format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.Encoding]::UTF8; {script}");
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]);
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = tokio::time::timeout(Duration::from_secs(60), cmd.output()).await.map_err(|_| anyhow!("PowerShell timed out"))??;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn as_array(v: Value) -> Vec<Value> {
        match v {
            Value::Array(a) => a,
            Value::Null => vec![],
            o => vec![o],
        }
    }

    fn safe_name(n: &str) -> Result<&str> {
        if n.is_empty() || n.len() > 256 || !n.chars().all(|c| c.is_alphanumeric() || " _-.$@{}".contains(c)) {
            bail!("invalid service name");
        }
        Ok(n)
    }

    impl Services {
        pub fn new(host_root: Option<String>) -> Self {
            Self { host_root }
        }

        pub async fn available(&self) -> bool {
            true
        }

        pub async fn logs_available(&self) -> bool {
            true
        }

        /// Automatic services that stopped with an error (exit code other than 0 / 1077 "never started").
        pub async fn failed(&self) -> Option<Vec<String>> {
            let out = ps("Get-CimInstance Win32_Service -Filter \"StartMode='Auto' AND State='Stopped' AND ExitCode<>0 AND ExitCode<>1077\" | Select-Object -ExpandProperty Name | ConvertTo-Json -Compress").await.ok()?;
            let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
            Some(as_array(v).into_iter().filter_map(|x| x.as_str().map(String::from)).collect())
        }

        pub async fn list(&self) -> Result<Value> {
            let out = ps(
                "$p = @{}; Get-Process | ForEach-Object { $p[$_.Id] = $_.WorkingSet64 }; \
                 Get-CimInstance Win32_Service | ForEach-Object { [pscustomobject]@{ name=$_.Name; description=$_.DisplayName; active=$_.State; \
                 enabled=$_.StartMode; pid=$_.ProcessId; mem=$(if ($_.ProcessId -gt 0) { $p[[int]$_.ProcessId] } else { $null }); \
                 account=$_.StartName; exit=$_.ExitCode; path=$_.PathName } } | ConvertTo-Json -Compress",
            )
            .await?;
            let rows: Vec<Value> = as_array(serde_json::from_str(out.trim())?)
                .into_iter()
                .map(|mut r| {
                    let name = r["name"].as_str().unwrap_or_default().to_string();
                    let state = r["active"].as_str().unwrap_or_default().to_string();
                    r["active"] = json!(match state.as_str() {
                        "Running" => "active",
                        "Stopped" => if r["exit"].as_u64().map(|e| e != 0 && e != 1077).unwrap_or(false) && r["enabled"] == "Auto" { "failed" } else { "inactive" },
                        "Start Pending" => "activating",
                        "Stop Pending" => "deactivating",
                        _ => "unknown",
                    });
                    r["sub"] = json!(state);
                    r["enabled"] = json!(match r["enabled"].as_str().unwrap_or_default() {
                        "Auto" => "enabled",
                        "Manual" => "manual",
                        "Disabled" => "disabled",
                        o => o,
                    });
                    r["protected"] = json!(protected(&name, false));
                    r
                })
                .collect();
            Ok(json!({ "platform": "windows", "services": rows }))
        }

        /// Windows Task Scheduler tasks with triggers, actions, last / next run and last result.
        pub async fn timers(&self) -> Result<Vec<Value>> {
            let out = ps(
                "$ms = { param($d) if ($d -and $d.Year -gt 2000) { [DateTimeOffset]::new($d).ToUnixTimeMilliseconds() } else { $null } }; \
                 $info = @{}; Get-ScheduledTask | Get-ScheduledTaskInfo -ErrorAction SilentlyContinue | ForEach-Object { $info[$_.TaskPath + $_.TaskName] = $_ }; \
                 Get-ScheduledTask | ForEach-Object { $i = $info[$_.TaskPath + $_.TaskName]; \
                 [pscustomobject]@{ path=$_.TaskPath; name=$_.TaskName; state=$_.State.ToString(); desc=$_.Description; user=$_.Principal.UserId; \
                 triggers=@($_.Triggers | ForEach-Object { ($_.CimClass.CimClassName -replace 'MSFT_Task','' -replace 'Trigger','') + $(if ($_.StartBoundary) { ' from ' + $_.StartBoundary } else { '' }) + $(if ($_.Repetition.Interval) { ' every ' + $_.Repetition.Interval } else { '' }) + $(if (-not $_.Enabled) { ' (off)' } else { '' }) }); \
                 actions=@($_.Actions | ForEach-Object { (($_.Execute + ' ' + $_.Arguments).Trim()) }); \
                 last=$(if ($i) { & $ms $i.LastRunTime } else { $null }); next=$(if ($i) { & $ms $i.NextRunTime } else { $null }); \
                 result=$(if ($i) { $i.LastTaskResult } else { $null }) } } | ConvertTo-Json -Compress -Depth 3",
            )
            .await?;
            let mut rows = Vec::new();
            for r in as_array(serde_json::from_str(out.trim()).unwrap_or(Value::Null)) {
                let path = r["path"].as_str().unwrap_or("\\").to_string();
                let name = r["name"].as_str().unwrap_or_default().to_string();
                let state = r["state"].as_str().unwrap_or_default().to_string();
                let strs = |k: &str| -> Vec<String> { as_array(r[k].clone()).into_iter().filter_map(|v| v.as_str().map(String::from)).collect() };
                // 0 = success; 267009 (0x41301) = running; 267011 (0x41303) = has not run yet.
                let result = r["result"].as_i64().map(|c| match c {
                    0 => "success".to_string(),
                    267_009 => "running".to_string(),
                    267_011 => "never run".to_string(),
                    267_010 => "disabled".to_string(),
                    c => format!("0x{:X}", c as u32),
                });
                rows.push(json!({
                    "id": format!("task:{path}{name}"), "kind": "task", "name": name, "folder": path,
                    "description": r["desc"], "command": strs("actions").join(" ; "), "user": r["user"],
                    "schedule": strs("triggers").join("; "), "summary": "",
                    "source": format!("{path}{name}"),
                    "enabled": state != "Disabled", "state": state,
                    "system": path.to_lowercase().starts_with("\\microsoft\\"),
                    "next_run": r["next"], "next_runs": [], "last_run": r["last"], "last_result": result,
                }));
            }
            rows.sort_by(|a, b| a["name"].as_str().map(str::to_lowercase).cmp(&b["name"].as_str().map(str::to_lowercase)));
            Ok(rows)
        }

        /// Run / stop / enable / disable a scheduled task by its full path (`\Folder\Name`).
        pub async fn task_action(&self, full: &str, action: ServiceActionKind) -> Result<Value> {
            if full.is_empty() || full.len() > 400 || !full.starts_with('\\') || full.contains("..") || !full.chars().all(|c| c.is_alphanumeric() || " _-.$@{}()\\".contains(c)) {
                bail!("invalid task name");
            }
            if full.to_lowercase().starts_with("\\microsoft\\") {
                bail!("built-in Windows tasks are read-only here");
            }
            let (path, name) = full.rsplit_once('\\').map(|(a, b)| (format!("{a}\\"), b.to_string())).unwrap_or(("\\".into(), full.to_string()));
            let verb = match action {
                ServiceActionKind::Start => "Start-ScheduledTask",
                ServiceActionKind::Stop => "Stop-ScheduledTask",
                ServiceActionKind::Enable => "Enable-ScheduledTask",
                ServiceActionKind::Disable => "Disable-ScheduledTask",
                _ => bail!("action not supported for scheduled tasks"),
            };
            ps(&format!("{verb} -TaskPath '{path}' -TaskName '{name}' | Out-Null")).await?;
            Ok(json!({ "ok": true }))
        }

        pub async fn action(&self, name: &str, action: ServiceActionKind) -> Result<Value> {
            let n = safe_name(name)?;
            if matches!(action, ServiceActionKind::Stop | ServiceActionKind::Disable) && protected(n, false) {
                bail!("{n} is protected: stopping it could cut off access to this server or stop MonitorLab itself");
            }
            let q = n.replace('\'', "''");
            let script = match action {
                ServiceActionKind::Start => format!("Start-Service -Name '{q}'"),
                ServiceActionKind::Stop => format!("Stop-Service -Name '{q}' -Force"),
                ServiceActionKind::Restart | ServiceActionKind::Reload => format!("Restart-Service -Name '{q}' -Force"),
                ServiceActionKind::Enable => format!("Set-Service -Name '{q}' -StartupType Automatic"),
                ServiceActionKind::Disable => format!("Set-Service -Name '{q}' -StartupType Disabled"),
            };
            ps(&script).await?;
            Ok(json!({ "ok": true }))
        }

        #[allow(clippy::too_many_arguments)]
        pub async fn stream_logs(
            &self,
            log: Option<String>,
            source: Option<String>,
            priority: Option<u8>,
            tail: u32,
            since: Option<i64>,
            follow: bool,
            grep: Option<String>,
            sid: u64,
            out: &mpsc::Sender<AgentMsg>,
        ) -> Result<()> {
            let grep = grep.filter(|g| !g.is_empty());
            if let Some(g) = &grep {
                if g.len() > 200 || !g.chars().all(|c| c.is_alphanumeric() || " _-.\\".contains(c)) {
                    bail!("the text filter may only contain letters, digits, spaces and _ - . \\");
                }
            }
            let log = log.filter(|l| !l.is_empty()).unwrap_or_else(|| "System".into());
            if !log.chars().all(|c| c.is_alphanumeric() || " -/_".contains(c)) {
                bail!("invalid log name");
            }
            // journald-style priority → Event Log levels (1 critical, 2 error, 3 warning, 4/0 info, 5 verbose).
            let levels: Vec<u8> = match priority {
                Some(p) if p <= 2 => vec![1],
                Some(3) => vec![1, 2],
                Some(4) | Some(5) => vec![1, 2, 3],
                Some(6) => vec![0, 1, 2, 3, 4],
                _ => vec![],
            };
            let mut cond: Vec<String> = Vec::new();
            if !levels.is_empty() {
                cond.push(format!("({})", levels.iter().map(|l| format!("Level={l}")).collect::<Vec<_>>().join(" or ")));
            }
            if let Some(src) = source.filter(|s| !s.is_empty()) {
                let s = safe_name(&src)?;
                cond.push(format!("Provider[@Name='{s}']"));
            }
            let fetch = |extra: Option<String>, max: Option<u32>| {
                let mut c = cond.clone();
                if let Some(e) = extra {
                    c.push(e);
                }
                let xpath = if c.is_empty() { "*".to_string() } else { format!("*[System[{}]]", c.join(" and ")) };
                // With a text filter, read the whole (size-capped) log and keep the newest matches.
                let max = if grep.is_some() { String::new() } else { max.map(|m| format!("-MaxEvents {m}")).unwrap_or_default() };
                let narrow = match &grep {
                    Some(g) => format!("| Where-Object {{ $_.Message -like '*{g}*' }} | Select-Object -First {} ", tail.clamp(1, 5000)),
                    None => String::new(),
                };
                format!(
                    "try {{ Get-WinEvent -LogName '{log}' -FilterXPath \"{xpath}\" {max} -ErrorAction Stop {narrow}| ForEach-Object {{ [pscustomobject]@{{ id=$_.RecordId; t=$_.TimeCreated.ToUniversalTime().ToString('o'); l=$_.Level; p=$_.ProviderName; e=$_.Id; m=$_.Message }} }} | ConvertTo-Json -Compress }} catch [Exception] {{ if ($_.Exception.Message -match 'No events were found') {{ '[]' }} else {{ throw }} }}"
                )
            };
            let to_line = |v: &Value| -> LogLine {
                let lvl = match v["l"].as_u64().unwrap_or(4) {
                    1 => 2,
                    2 => 3,
                    3 => 4,
                    5 => 7,
                    _ => 6,
                };
                let msg = v["m"].as_str().unwrap_or_default().replace("\r\n", " ").replace('\n', " ");
                line(v["t"].as_str().map(String::from), format!("[{}] {msg}", v["e"]), v["p"].as_str().map(String::from), Some(lvl))
            };
            let first = match since {
                Some(t) => fetch(Some(format!("TimeCreated[@SystemTime>='{}']", rfc3339_us(t * 1_000_000).replace(".000000Z", ".000Z"))), Some(5000)),
                None => fetch(None, Some(tail.clamp(1, 5000))),
            };
            let mut rows = as_array(serde_json::from_str(ps(&first).await?.trim()).unwrap_or(Value::Null));
            rows.sort_by_key(|v| v["id"].as_u64().unwrap_or(0));
            let mut last = rows.last().and_then(|v| v["id"].as_u64()).unwrap_or(0);
            let mut batch: Vec<LogLine> = rows.iter().map(to_line).collect();
            send(out, sid, &mut batch).await?;
            if !follow {
                return Ok(());
            }
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let q = fetch(Some(format!("EventRecordID>{last}")), Some(2000));
                let mut rows = as_array(serde_json::from_str(ps(&q).await?.trim()).unwrap_or(Value::Null));
                rows.sort_by_key(|v| v["id"].as_u64().unwrap_or(0));
                if let Some(l) = rows.last().and_then(|v| v["id"].as_u64()) {
                    last = l;
                }
                let mut batch: Vec<LogLine> = rows.iter().map(to_line).collect();
                send(out, sid, &mut batch).await?;
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::*;
    impl Services {
        pub fn new(host_root: Option<String>) -> Self {
            Self { host_root }
        }
        pub async fn available(&self) -> bool {
            false
        }
        pub async fn logs_available(&self) -> bool {
            false
        }
        pub async fn failed(&self) -> Option<Vec<String>> {
            None
        }
        pub async fn list(&self) -> Result<Value> {
            bail!("not supported on this platform")
        }
        pub async fn timers(&self) -> Result<Vec<Value>> {
            Ok(vec![])
        }
        pub async fn task_action(&self, _: &str, _: ServiceActionKind) -> Result<Value> {
            bail!("not supported on this platform")
        }
        pub async fn action(&self, _: &str, _: ServiceActionKind) -> Result<Value> {
            bail!("not supported on this platform")
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::parse_journal_json;

#[allow(dead_code)]
async fn send(out: &mpsc::Sender<AgentMsg>, sid: u64, batch: &mut Vec<LogLine>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let lines = std::mem::take(batch);
    out.send(AgentMsg::Stream { sid, data: StreamData::Lines { lines } }).await.map_err(|_| anyhow!("closed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339_us(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(rfc3339_us(1_795_651_200_000_123), "2026-11-26T00:00:00.000123Z");
    }

    #[test]
    fn protection() {
        assert!(protected("sshd.service", false));
        assert!(protected("systemd-journald.service", false));
        assert!(protected("docker.service", true));
        assert!(!protected("docker.service", false));
        assert!(!protected("nginx.service", true));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn journal_lines() {
        let l = parse_journal_json(r#"{"__REALTIME_TIMESTAMP":"1795651200000000","PRIORITY":"3","MESSAGE":"boom","_SYSTEMD_UNIT":"nginx.service"}"#).unwrap();
        assert_eq!((l.lvl, l.s, l.src.as_deref(), l.text.as_str()), (Some(3), 2, Some("nginx.service"), "boom"));
        assert_eq!(l.ts.as_deref(), Some("2026-11-26T00:00:00.000000Z"));
        let b = parse_journal_json(r#"{"__REALTIME_TIMESTAMP":"1","MESSAGE":[104,105],"SYSLOG_IDENTIFIER":"kernel"}"#).unwrap();
        assert_eq!((b.text.as_str(), b.src.as_deref(), b.lvl), ("hi", Some("kernel"), None));
    }
}
