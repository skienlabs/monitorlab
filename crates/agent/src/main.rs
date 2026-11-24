use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use monitorlab_agent::config::{default_config_path, AgentConfig};
use monitorlab_agent::install::InstallArgs;

#[derive(Parser)]
#[command(name = "monitorlab-agent", version = monitorlab_common::VERSION, about = "Lightweight MonitorLab monitoring agent")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the agent in the foreground.
    Run {
        #[arg(long, env = "MONITORLAB_AGENT_CONFIG")]
        config: Option<PathBuf>,
    },
    /// Enroll with the hub and install as a system service (systemd / Windows service).
    Install {
        /// Hub URL, e.g. https://monitor.example.com:8443
        #[arg(long)]
        hub: String,
        /// One-time enrollment token from the hub's "Add server" dialog.
        #[arg(long)]
        token: String,
        /// SHA-256 fingerprint of the hub certificate (required for self-signed hubs).
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Enroll with the hub and only write the config file.
    Enroll {
        #[arg(long)]
        hub: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Remove the system service.
    Uninstall {
        /// Also delete the configuration (and on Linux the binary and service user).
        #[arg(long)]
        purge: bool,
    },
    /// Internal: entry point used by the Windows service manager.
    #[command(hide = true)]
    Service {
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn init_logging(to_file: Option<PathBuf>) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("MONITORLAB_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,bollard=warn,tiberius=warn"));
    match to_file {
        Some(path) => {
            if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
                let _ = std::fs::rename(&path, path.with_extension("log.old"));
            }
            match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(f))
                    .init(),
                Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
            }
        }
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run { config } => {
            init_logging(None);
            let path = config.unwrap_or_else(default_config_path);
            let cfg = AgentConfig::load(&path)?;
            tracing::info!("monitorlab-agent {} starting (config {})", monitorlab_common::VERSION, path.display());
            runtime()?.block_on(async {
                tokio::select! {
                    r = monitorlab_agent::conn::run_forever(cfg) => r,
                    _ = tokio::signal::ctrl_c() => { tracing::info!("shutting down"); Ok(()) }
                }
            })
        }
        Cmd::Install { hub, token, fingerprint, config } => {
            runtime()?.block_on(monitorlab_agent::install::install(InstallArgs { hub, token, fingerprint, config }))
        }
        Cmd::Enroll { hub, token, fingerprint, config } => {
            runtime()?.block_on(monitorlab_agent::install::enroll_and_save(&InstallArgs { hub, token, fingerprint, config }))?;
            Ok(())
        }
        Cmd::Uninstall { purge } => monitorlab_agent::install::uninstall(purge),
        Cmd::Service { config } => {
            let path = config.unwrap_or_else(default_config_path);
            #[cfg(windows)]
            {
                init_logging(path.parent().map(|d| d.join("agent.log")));
                monitorlab_agent::winsvc::run_dispatcher(path)
            }
            #[cfg(not(windows))]
            {
                let _ = path;
                anyhow::bail!("the `service` command is only used on Windows; use `run` instead")
            }
        }
    }
}
