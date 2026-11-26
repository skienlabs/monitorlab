use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn d_interval() -> u32 {
    5
}
fn d_true() -> bool {
    true
}

/// Persistent agent configuration (`agent.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// e.g. `https://hub.example.com:8443` (https/wss or http/ws).
    pub hub_url: String,
    pub agent_id: String,
    pub secret: String,
    /// SHA-256 fingerprint of the hub's TLS certificate (hex, colons optional).
    /// When set, the certificate is pinned and CA validation is skipped.
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default = "d_interval")]
    pub interval_secs: u32,
    #[serde(default)]
    pub docker: DockerConfig,
    #[serde(default)]
    pub mssql: MssqlConfig,
    #[serde(default)]
    pub postgres: PostgresConfig,
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default)]
    pub iis: IisConfig,
    #[serde(default)]
    pub kubernetes: K8sConfig,
    /// Set when running inside a container with the host's `/` mounted here.
    #[serde(default)]
    pub host_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerConfig {
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Unix socket path. Defaults to `DOCKER_HOST` or `/var/run/docker.sock`.
    #[serde(default)]
    pub socket: Option<String>,
    /// Keep per-container CPU / memory for the hub's history charts (one stats stream per container).
    #[serde(default = "d_true")]
    pub history: bool,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self { enabled: true, socket: None, history: true }
    }
}

/// Interactive shell on this server, opened from the hub by users with the "Server shell" permission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfig {
    #[serde(default = "shell_on")]
    pub enabled: bool,
}

fn shell_on() -> bool {
    true
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl ShellConfig {
    /// For the hub's built-in agent: `MONITORLAB_HOST_SHELL=0` turns it off.
    pub fn from_env() -> Self {
        let off = std::env::var("MONITORLAB_HOST_SHELL").map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no")).unwrap_or(false);
        Self { enabled: !off }
    }
}

/// Optional PostgreSQL monitoring. Add a read-only role (for example a member of `pg_monitor`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostgresConfig {
    #[serde(default)]
    pub connection: Option<PostgresConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConnection {
    #[serde(default = "pg_host")]
    pub host: String,
    #[serde(default = "pg_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "pg_db")]
    pub database: String,
    /// `disable` (default) or `require` (TLS, certificate not verified, like libpq's `require`).
    #[serde(default = "pg_ssl")]
    pub sslmode: String,
}

fn pg_host() -> String { "127.0.0.1".into() }
fn pg_port() -> u16 { 5432 }
fn pg_db() -> String { "postgres".into() }
fn pg_ssl() -> String { "disable".into() }

impl PostgresConfig {
    /// Settings for the hub's built-in agent: `MONITORLAB_PG_HOST`, `_PORT`, `_USER`, `_PASSWORD`, `_DATABASE`, `_SSLMODE`.
    pub fn from_env() -> Self {
        let e = |k: &str| std::env::var(format!("MONITORLAB_PG_{k}")).ok().filter(|v| !v.is_empty());
        match e("USER") {
            Some(user) => Self {
                connection: Some(PostgresConnection {
                    host: e("HOST").unwrap_or_else(pg_host),
                    port: e("PORT").and_then(|p| p.parse().ok()).unwrap_or(5432),
                    user,
                    password: e("PASSWORD").unwrap_or_default(),
                    database: e("DATABASE").unwrap_or_else(pg_db),
                    sslmode: e("SSLMODE").unwrap_or_else(pg_ssl),
                }),
            },
            None => Self::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MssqlConfig {
    /// `None` = auto-detect.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Override the directory that contains `errorlog` / `ERRORLOG`.
    #[serde(default)]
    pub errorlog_dir: Option<String>,
    /// Optional read-only login for DMV queries (databases, sessions, memory).
    #[serde(default)]
    pub connection: Option<MssqlConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MssqlConnection {
    #[serde(default = "d_host")]
    pub host: String,
    #[serde(default = "d_port")]
    pub port: u16,
    pub user: String,
    pub password: String,
    /// Accept the server's self-signed certificate (SQL Server default).
    #[serde(default = "d_true")]
    pub trust_cert: bool,
}

fn d_host() -> String {
    "127.0.0.1".into()
}
fn d_port() -> u16 {
    1433
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct K8sConfig {
    /// `None` = auto-detect (k3s kubeconfig, `KUBECONFIG`, in-cluster service account).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Path of a kubeconfig file. The agent only needs read access plus the rights for the
    /// actions you use (restart, scale, delete pods, exec).
    #[serde(default)]
    pub kubeconfig: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IisConfig {
    /// `None` = auto-detect.
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub fn default_config_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into());
        Path::new(&base).join("MonitorLab").join("agent.toml")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/MonitorLab/agent.toml")
    } else {
        PathBuf::from("/etc/monitorlab/agent.toml")
    }
}

impl AgentConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: AgentConfig = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
        let header = "# MonitorLab agent configuration. Keep this file private: it contains the agent secret.\n";
        write_private(path, format!("{header}{text}").as_bytes())
    }
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    // Enforce mode even if the file pre-existed with looser permissions.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_parses() {
        let c: AgentConfig = toml::from_str("hub_url='https://h:8443'\nagent_id='a'\nsecret='s'\n").unwrap();
        assert_eq!(c.interval_secs, 5);
        assert!(c.docker.enabled);
        assert!(c.mssql.enabled.is_none());
    }
}
