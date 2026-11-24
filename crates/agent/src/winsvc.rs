//! Windows service entry point (`monitorlab-agent service --config ...`, started by the SCM).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::config::AgentConfig;

pub const SERVICE_NAME: &str = "MonitorLabAgent";

static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

pub fn run_dispatcher(config: PathBuf) -> Result<()> {
    let _ = CONFIG_PATH.set(config);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        tracing::error!("service failed: {e:#}");
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, exit: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(exit),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    }
}

fn run_service() -> Result<()> {
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let handler = move |ev| match ev {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let handle = service_control_handler::register(SERVICE_NAME, handler)?;
    handle.set_service_status(status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
    ))?;

    let path = CONFIG_PATH.get().cloned().unwrap_or_else(crate::config::default_config_path);
    let result = (|| -> Result<()> {
        let cfg = AgentConfig::load(&path)?;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        rt.block_on(async move {
            tokio::select! {
                r = crate::conn::run_forever(cfg) => r,
                _ = stop_rx.changed() => Ok(()),
            }
        })
    })();

    let code = if result.is_ok() { 0 } else { 1 };
    handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty(), code))?;
    result
}
