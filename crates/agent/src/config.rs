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
    pub iis: IisConfig,
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
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self { enabled: true, socket: None }
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
pub struct IisConfig {
    /// `None` = auto-detect.
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub fn default_config_path() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into());
        Path::new(&base).join("MonitorLab").join("agent.toml")
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
