//! SQL Server running as a native service (Linux `mssql-server` or Windows `MSSQLSERVER` / `MSSQL$NAME`).
//!
//! Works without credentials: service state, process CPU/RAM, version (from ERRORLOG) and
//! the error log itself. With an optional read-only login it also queries DMVs.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{bail, Result};
use serde_json::{json, Value};
use monitorlab_common::{MssqlInstanceSummary, MssqlSummary};

use crate::config::MssqlConfig;
use crate::system::Collector;
use crate::textlog;

#[derive(Debug, Clone)]
pub struct Instance {
    /// Instance name (`MSSQLSERVER` for the default instance).
    pub name: String,
    /// Service / unit name.
    pub service: String,
    pub state: String,
    pub pid: Option<u32>,
    pub errorlog: Option<PathBuf>,
}

pub struct Mssql {
    cfg: MssqlConfig,
    #[cfg_attr(windows, allow(dead_code))]
    host_root: Option<String>,
    cache: Mutex<Option<(Instant, Vec<Instance>)>>,
}

impl Mssql {
    pub fn new(cfg: MssqlConfig, host_root: Option<String>) -> Self {
        Self { cfg, host_root, cache: Mutex::new(None) }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled != Some(false)
    }

    /// Discover instances. Cached for 30s because it shells out / reads the registry.
    pub fn instances(&self) -> Vec<Instance> {
        if !self.enabled() {
            return vec![];
        }
        if let Ok(guard) = self.cache.lock() {
            if let Some((at, list)) = guard.as_ref() {
                if at.elapsed().as_secs() < 30 {
                    return list.clone();
                }
            }
        }
        let list = self.discover();
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some((Instant::now(), list.clone()));
        }
        list
    }

    pub fn invalidate(&self) {
        if let Ok(mut g) = self.cache.lock() {
            *g = None;
        }
    }

    pub fn detected(&self) -> bool {
        !self.instances().is_empty()
    }

    #[cfg(not(windows))]
    fn discover(&self) -> Vec<Instance> {
        use crate::util::host_path;
        let root = self.host_root.as_deref();
        let installed = host_path(root, "/opt/mssql/bin/sqlservr").exists() || host_path(root, "/var/opt/mssql").is_dir();
        if !installed && self.cfg.enabled != Some(true) {
            return vec![];
        }
        let state = if root.is_none() {
            match std::process::Command::new("systemctl").args(["is-active", "mssql-server"]).output() {
                Ok(o) => {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    match s.as_str() {
                        "active" => "running".to_string(),
                        "inactive" | "failed" => "stopped".to_string(),
                        "" => "unknown".to_string(),
                        other => other.to_string(),
                    }
                }
                Err(_) => "unknown".into(),
            }
        } else {
            "unknown".into()
        };
        let dir = self
            .cfg
            .errorlog_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| host_path(root, "/var/opt/mssql/log"));
        let errorlog = ["errorlog", "ERRORLOG"].iter().map(|n| dir.join(n)).find(|p| p.exists());
        vec![Instance { name: "MSSQLSERVER".into(), service: "mssql-server".into(), state, pid: None, errorlog }]
    }

    #[cfg(windows)]
    fn discover(&self) -> Vec<Instance> {
        win::discover(&self.cfg)
    }

    /// Per-instance CPU/RAM for the periodic metrics.
    pub fn summary(&self, col: &Collector) -> Option<MssqlSummary> {
        let instances = self.instances();
        if instances.is_empty() {
            return None;
        }
        let out = instances
            .iter()
            .map(|i| {
                let (cpu, mem, running) = usage_for(i, col);
                let state = if i.state == "unknown" && running { "running".to_string() } else { i.state.clone() };
                MssqlInstanceSummary { name: i.name.clone(), state, cpu, mem }
            })
            .collect();
        Some(MssqlSummary { instances: out })
    }

    pub fn status(&self, col: &Collector) -> Value {
        self.invalidate();
        let instances = self.instances();
        let list: Vec<Value> = instances
            .iter()
            .map(|i| {
                let (cpu, mem, running) = usage_for(i, col);
                let version = i.errorlog.as_ref().and_then(|p| textlog::head_text(p, 4096).ok()).and_then(|t| {
                    t.lines()
                        .find(|l| l.contains("Microsoft SQL Server"))
                        .map(|l| {
                            // ERRORLOG lines are "<date> <time> Server      Microsoft SQL Server 2022 (RTM-CU12) ..."
                            l.find("Microsoft SQL Server").map(|idx| l[idx..].trim().to_string()).unwrap_or_default()
                        })
                });
                let edition = i.errorlog.as_ref().and_then(|p| textlog::head_text(p, 8192).ok()).and_then(|t| {
                    t.lines().find(|l| l.contains(" Edition")).map(|l| l.split_whitespace().skip(3).collect::<Vec<_>>().join(" "))
                });
                json!({
                    "name": i.name,
                    "service": i.service,
                    "state": if i.state == "unknown" && running { "running" } else { i.state.as_str() },
                    "pid": i.pid,
                    "cpu": cpu,
                    "mem": mem,
                    "errorlog": i.errorlog.as_ref().map(|p| p.display().to_string()),
                    "version": version,
                    "edition": edition,
                })
            })
            .collect();
        json!({ "instances": list, "query_configured": self.cfg.connection.is_some() })
    }

    pub fn errorlog_path(&self, instance: Option<&str>) -> Result<PathBuf> {
        let list = self.instances();
        let inst = match instance {
            Some(n) => list.iter().find(|i| i.name.eq_ignore_ascii_case(n)),
            None => list.first(),
        };
        let Some(inst) = inst else { bail!("SQL Server instance not found") };
        match &inst.errorlog {
            Some(p) => Ok(p.clone()),
            None => bail!("error log for {} not found or not readable by the agent", inst.name),
        }
    }

    pub fn error_log(&self, instance: Option<&str>, lines: u32) -> Result<Value> {
        let p = self.errorlog_path(instance)?;
        let lines = textlog::tail_lines(&p, lines.clamp(1, 20_000) as usize, 8 * 1024 * 1024)?;
        Ok(json!({ "path": p.display().to_string(), "lines": lines }))
    }

    /// Optional DMV queries.
    pub async fn query(&self) -> Option<Value> {
        let conn = self.cfg.connection.clone()?;
        #[cfg(feature = "mssql-query")]
        {
            match tokio::time::timeout(std::time::Duration::from_secs(15), query::run(conn)).await {
                Ok(Ok(v)) => Some(v),
                Ok(Err(e)) => Some(json!({ "error": e.to_string() })),
                Err(_) => Some(json!({ "error": "query timed out" })),
            }
        }
        #[cfg(not(feature = "mssql-query"))]
        {
            let _ = conn;
            Some(json!({ "error": "agent built without mssql-query feature" }))
        }
    }
}

fn usage_for(i: &Instance, col: &Collector) -> (f32, u64, bool) {
    if let Some(pid) = i.pid {
        if let Some((cpu, mem)) = col.proc_by_pid(pid) {
            return (cpu, mem, true);
        }
        return (0.0, 0, false);
    }
    // Linux: `sqlservr` runs as a watchdog parent + engine child; sum both.
    let (cpu, mem, pids) = col.proc_usage(|p| {
        let n = p.name().to_string_lossy();
        n == "sqlservr" || n.eq_ignore_ascii_case("sqlservr.exe")
    });
    (cpu, mem, !pids.is_empty())
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    pub fn discover(cfg: &MssqlConfig) -> Vec<Instance> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let Ok(names) = hklm.open_subkey_with_flags(r"SOFTWARE\Microsoft\Microsoft SQL Server\Instance Names\SQL", KEY_READ) else {
            return vec![];
        };
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok();
        let mut out = Vec::new();
        for item in names.enum_values().flatten() {
            let (inst_name, value) = item;
            let inst_id = value.to_string();
            let service = if inst_name.eq_ignore_ascii_case("MSSQLSERVER") {
                "MSSQLSERVER".to_string()
            } else {
                format!("MSSQL${inst_name}")
            };
            let (state, pid) = manager
                .as_ref()
                .and_then(|m| m.open_service(&service, ServiceAccess::QUERY_STATUS).ok())
                .and_then(|s| s.query_status().ok())
                .map(|st| {
                    let state = match st.current_state {
                        ServiceState::Running => "running",
                        ServiceState::Stopped => "stopped",
                        ServiceState::StartPending => "starting",
                        ServiceState::StopPending => "stopping",
                        ServiceState::Paused => "paused",
                        _ => "unknown",
                    };
                    (state.to_string(), st.process_id)
                })
                .unwrap_or_else(|| ("unknown".to_string(), None));

            let errorlog = cfg
                .errorlog_dir
                .as_ref()
                .map(|d| PathBuf::from(d).join("ERRORLOG"))
                .or_else(|| errorlog_from_registry(&hklm, &inst_id));
            out.push(Instance { name: inst_name, service, state, pid, errorlog });
        }
        out
    }

    fn errorlog_from_registry(hklm: &RegKey, inst_id: &str) -> Option<PathBuf> {
        let key = hklm
            .open_subkey_with_flags(format!(r"SOFTWARE\Microsoft\Microsoft SQL Server\{inst_id}\MSSQLServer\Parameters"), KEY_READ)
            .ok()?;
        for item in key.enum_values().flatten() {
            let v = item.1.to_string();
            if let Some(p) = v.strip_prefix("-e") {
                return Some(PathBuf::from(p.trim_matches('"')));
            }
        }
        None
    }
}

#[cfg(feature = "mssql-query")]
mod query {
    use super::*;
    use crate::config::MssqlConnection;
    use tiberius::{AuthMethod, Client, Config};
    use tokio::net::TcpStream;
    use tokio_util::compat::TokioAsyncWriteCompatExt;

    pub async fn run(c: MssqlConnection) -> Result<Value> {
        let mut config = Config::new();
        config.host(&c.host);
        config.port(c.port);
        config.authentication(AuthMethod::sql_server(&c.user, &c.password));
        config.application_name("monitorlab-agent");
        if c.trust_cert {
            config.trust_cert();
        }
        let tcp = TcpStream::connect(config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        let mut client = Client::connect(config, tcp.compat_write()).await?;

        let row = client
            .query(
                "SELECT CAST(@@VERSION AS NVARCHAR(400)), CAST(@@SERVERNAME AS NVARCHAR(256)), \
                 CONVERT(NVARCHAR(33), (SELECT sqlserver_start_time FROM sys.dm_os_sys_info), 126), \
                 CAST((SELECT COUNT(*) FROM sys.dm_exec_sessions WHERE is_user_process = 1) AS BIGINT), \
                 CAST((SELECT COUNT(*) FROM sys.dm_exec_requests WHERE session_id <> @@SPID AND session_id > 50) AS BIGINT), \
                 CAST((SELECT physical_memory_in_use_kb FROM sys.dm_os_process_memory) AS BIGINT), \
                 CAST((SELECT committed_target_kb FROM sys.dm_os_sys_info) AS BIGINT)",
                &[],
            )
            .await?
            .into_row()
            .await?;
        let mut info = json!({});
        if let Some(r) = row {
            info = json!({
                "version": r.get::<&str, _>(0),
                "server_name": r.get::<&str, _>(1),
                "start_time": r.get::<&str, _>(2),
                "user_sessions": r.get::<i64, _>(3),
                "active_requests": r.get::<i64, _>(4),
                "memory_in_use_kb": r.get::<i64, _>(5),
                "target_memory_kb": r.get::<i64, _>(6),
            });
        }

        let dbs = client
            .query(
                "SELECT d.name, d.state_desc, d.recovery_model_desc, \
                 CAST(SUM(CASE WHEN mf.type = 0 THEN CAST(mf.size AS BIGINT) ELSE 0 END) * 8 / 1024 AS BIGINT) AS data_mb, \
                 CAST(SUM(CASE WHEN mf.type = 1 THEN CAST(mf.size AS BIGINT) ELSE 0 END) * 8 / 1024 AS BIGINT) AS log_mb \
                 FROM sys.databases d LEFT JOIN sys.master_files mf ON mf.database_id = d.database_id \
                 GROUP BY d.name, d.state_desc, d.recovery_model_desc ORDER BY d.name",
                &[],
            )
            .await?
            .into_first_result()
            .await?;
        let databases: Vec<Value> = dbs
            .iter()
            .map(|r| {
                json!({
                    "name": r.get::<&str, _>(0),
                    "state": r.get::<&str, _>(1),
                    "recovery_model": r.get::<&str, _>(2),
                    "data_mb": r.get::<i64, _>(3),
                    "log_mb": r.get::<i64, _>(4),
                })
            })
            .collect();

        let counters = client
            .query(
                "SELECT RTRIM(counter_name), CAST(cntr_value AS BIGINT) FROM sys.dm_os_performance_counters \
                 WHERE (object_name LIKE '%Buffer Manager%' AND counter_name = 'Page life expectancy') \
                 OR (object_name LIKE '%General Statistics%' AND counter_name IN ('User Connections', 'Processes blocked')) \
                 OR (object_name LIKE '%SQL Statistics%' AND counter_name IN ('Batch Requests/sec')) \
                 OR (object_name LIKE '%Locks%' AND instance_name = '_Total' AND counter_name IN ('Number of Deadlocks/sec'))",
                &[],
            )
            .await?
            .into_first_result()
            .await?;
        let mut perf = serde_json::Map::new();
        for r in &counters {
            if let (Some(name), Some(v)) = (r.get::<&str, _>(0), r.get::<i64, _>(1)) {
                perf.entry(name.to_string()).or_insert(json!(v));
            }
        }

        Ok(json!({ "info": info, "databases": databases, "counters": perf }))
    }
}
