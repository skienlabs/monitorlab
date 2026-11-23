use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TlsMode {
    /// Use /data/tls/{cert,key}.pem, generating a self-signed pair on first start.
    Auto,
    /// Use the certificate and key given by --tls-cert / --tls-key.
    Files,
    /// Plain HTTP (put a TLS-terminating reverse proxy in front).
    Off,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "vigil-hub", version = vigil_common::VERSION, about = "Vigil central hub")]
pub struct HubConfig {
    #[arg(long, env = "VIGIL_LISTEN", default_value = "0.0.0.0:8443")]
    pub listen: SocketAddr,

    #[arg(long, env = "VIGIL_DATA_DIR", default_value = "/data")]
    pub data_dir: PathBuf,

    #[arg(long, env = "VIGIL_TLS", value_enum, default_value = "auto")]
    pub tls: TlsMode,

    #[arg(long, env = "VIGIL_TLS_CERT")]
    pub tls_cert: Option<PathBuf>,

    #[arg(long, env = "VIGIL_TLS_KEY")]
    pub tls_key: Option<PathBuf>,

    /// Extra DNS names / IPs for the auto-generated certificate (comma separated).
    #[arg(long, env = "VIGIL_TLS_SANS", default_value = "")]
    pub tls_sans: String,

    /// URL agents use to reach the hub, e.g. https://monitor.corp.local:8443.
    /// Defaults to the address the admin's browser is using.
    #[arg(long, env = "VIGIL_PUBLIC_URL")]
    pub public_url: Option<String>,

    /// Monitor the machine the hub runs on.
    #[arg(long, env = "VIGIL_LOCAL_AGENT", default_value_t = true, action = clap::ArgAction::Set)]
    pub local_agent: bool,

    /// Host filesystem mount point inside the container (e.g. /host), for accurate disk metrics.
    #[arg(long, env = "VIGIL_HOST_ROOT")]
    pub host_root: Option<String>,

    /// Directory containing agent binaries offered for download.
    #[arg(long, env = "VIGIL_AGENTS_DIR", default_value = "/opt/vigil/agents")]
    pub agents_dir: PathBuf,

    #[arg(long, env = "VIGIL_ADMIN_EMAIL", default_value = "admin@vigil.local")]
    pub admin_email: String,

    /// Initial admin password. If unset a random one is generated and printed once.
    #[arg(long, env = "VIGIL_ADMIN_PASSWORD", hide_env_values = true)]
    pub admin_password: Option<String>,

    /// Trust X-Forwarded-For / X-Forwarded-Proto from a reverse proxy.
    #[arg(long, env = "VIGIL_TRUST_PROXY", default_value_t = false, action = clap::ArgAction::Set)]
    pub trust_proxy: bool,

    /// Force the Secure flag on cookies (automatic when the hub serves TLS itself).
    #[arg(long, env = "VIGIL_SECURE_COOKIES", default_value_t = false, action = clap::ArgAction::Set)]
    pub secure_cookies: bool,

    /// Days of raw (10s) metrics to keep.
    #[arg(long, env = "VIGIL_RETENTION_RAW_DAYS", default_value_t = 3)]
    pub retention_raw_days: u32,

    /// Days of 5-minute rollups to keep.
    #[arg(long, env = "VIGIL_RETENTION_DAYS", default_value_t = 90)]
    pub retention_days: u32,

    /// Session idle timeout in hours.
    #[arg(long, env = "VIGIL_SESSION_IDLE_HOURS", default_value_t = 12)]
    pub session_idle_hours: u32,
}
