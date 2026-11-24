//! MonitorLab agent library.
//!
//! The same code runs in two places:
//! * the standalone `monitorlab-agent` binary on monitored servers (dials out to the hub), and
//! * inside the hub process, to monitor the hub's own host (wired up with in-memory channels).

pub mod config;
pub mod conn;
pub mod docker;
pub mod hostnet;
pub mod iis;
pub mod install;
pub mod mssql;
pub mod session;
pub mod system;
pub mod textlog;
pub mod util;

#[cfg(windows)]
pub mod winsvc;

pub use session::{Agent, AgentOptions};
