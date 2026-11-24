//! Role-based access control. Every agent request and stream maps to exactly one permission;
//! the `match` statements are exhaustive so new request types cannot be added unguarded.

use serde::Serialize;
use monitorlab_common::{Request, StreamRequest};

pub const SERVERS_VIEW: &str = "servers.view";
pub const DOCKER_VIEW: &str = "docker.view";
pub const LOGS_VIEW: &str = "logs.view";
pub const CONTAINERS_CONTROL: &str = "containers.control";
pub const CONTAINERS_EXEC: &str = "containers.exec";
pub const CONTAINERS_DELETE: &str = "containers.delete";
pub const DOCKER_CLEANUP: &str = "docker.cleanup";
pub const MSSQL_VIEW: &str = "mssql.view";
pub const IIS_VIEW: &str = "iis.view";
pub const IIS_CONTROL: &str = "iis.control";
pub const SERVERS_MANAGE: &str = "servers.manage";
pub const USERS_MANAGE: &str = "users.manage";
pub const AUDIT_VIEW: &str = "audit.view";
pub const ALERTS_MANAGE: &str = "alerts.manage";

#[derive(Debug, Clone, Serialize)]
pub struct PermInfo {
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub dangerous: bool,
}

pub const CATALOG: &[PermInfo] = &[
    PermInfo { key: SERVERS_VIEW, group: "Monitoring", label: "View servers", description: "Dashboards, metrics and charts", dangerous: false },
    PermInfo { key: DOCKER_VIEW, group: "Monitoring", label: "View Docker", description: "Containers, images, volumes, networks and live stats", dangerous: false },
    PermInfo { key: MSSQL_VIEW, group: "Monitoring", label: "View SQL Server", description: "Instance status, databases and counters", dangerous: false },
    PermInfo { key: IIS_VIEW, group: "Monitoring", label: "View IIS", description: "Sites, bindings and application pools", dangerous: false },
    PermInfo { key: LOGS_VIEW, group: "Monitoring", label: "View logs", description: "Container, IIS and SQL Server logs", dangerous: false },
    PermInfo { key: CONTAINERS_CONTROL, group: "Operations", label: "Control containers", description: "Start, stop, restart, pause and kill containers", dangerous: false },
    PermInfo { key: IIS_CONTROL, group: "Operations", label: "Control IIS", description: "Start/stop sites, recycle application pools", dangerous: false },
    PermInfo { key: CONTAINERS_EXEC, group: "Operations", label: "Container terminal", description: "Open an interactive shell inside containers", dangerous: true },
    PermInfo { key: CONTAINERS_DELETE, group: "Destructive", label: "Delete containers", description: "Remove containers", dangerous: true },
    PermInfo { key: DOCKER_CLEANUP, group: "Destructive", label: "Docker cleanup", description: "Delete images, volumes, networks; prune build cache", dangerous: true },
    PermInfo { key: ALERTS_MANAGE, group: "Operations", label: "Manage alerts", description: "Edit alert thresholds and notification channels, acknowledge alerts", dangerous: false },
    PermInfo { key: SERVERS_MANAGE, group: "Administration", label: "Manage servers", description: "Add, rename and remove servers / agents", dangerous: true },
    PermInfo { key: USERS_MANAGE, group: "Administration", label: "Manage users & roles", description: "Create users, assign roles and server access", dangerous: true },
    PermInfo { key: AUDIT_VIEW, group: "Administration", label: "View audit log", description: "See who did what and when", dangerous: false },
];

pub fn all() -> Vec<String> {
    CATALOG.iter().map(|p| p.key.to_string()).collect()
}

pub fn is_known(p: &str) -> bool {
    CATALOG.iter().any(|c| c.key == p)
}

/// Built-in roles created on first start: (name, description, permissions, editable).
pub fn builtin_roles() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        ("Administrator", "Full access to everything", CATALOG.iter().map(|p| p.key).collect()),
        (
            "Operator",
            "View everything, read logs, control containers and IIS, open terminals. Cannot delete.",
            vec![SERVERS_VIEW, DOCKER_VIEW, MSSQL_VIEW, IIS_VIEW, LOGS_VIEW, CONTAINERS_CONTROL, IIS_CONTROL, CONTAINERS_EXEC, ALERTS_MANAGE],
        ),
        (
            "Tester",
            "View servers and logs, restart containers. No terminal, no deletes.",
            vec![SERVERS_VIEW, DOCKER_VIEW, MSSQL_VIEW, IIS_VIEW, LOGS_VIEW, CONTAINERS_CONTROL],
        ),
        ("Viewer", "Read-only dashboards and logs", vec![SERVERS_VIEW, DOCKER_VIEW, MSSQL_VIEW, IIS_VIEW, LOGS_VIEW]),
    ]
}

pub fn for_request(r: &Request) -> &'static str {
    match r {
        Request::SystemDetail => SERVERS_VIEW,
        Request::DockerContainers
        | Request::DockerInspect { .. }
        | Request::DockerImages
        | Request::DockerVolumes
        | Request::DockerNetworks
        | Request::DockerDiskUsage => DOCKER_VIEW,
        Request::DockerContainerAction { .. } => CONTAINERS_CONTROL,
        Request::DockerContainerRemove { .. } => CONTAINERS_DELETE,
        Request::DockerImageRemove { .. }
        | Request::DockerVolumeRemove { .. }
        | Request::DockerNetworkRemove { .. }
        | Request::DockerPrune { .. } => DOCKER_CLEANUP,
        Request::MssqlStatus => MSSQL_VIEW,
        Request::MssqlErrorLog { .. } => LOGS_VIEW,
        Request::IisSites | Request::IisAppPools | Request::IisLogFiles { .. } => IIS_VIEW,
        Request::IisSiteAction { .. } | Request::IisAppPoolAction { .. } => IIS_CONTROL,
        Request::IisLogTail { .. } => LOGS_VIEW,
    }
}

/// Requests that change state are written to the audit log.
pub fn is_mutating(r: &Request) -> bool {
    matches!(
        r,
        Request::DockerContainerAction { .. }
            | Request::DockerContainerRemove { .. }
            | Request::DockerImageRemove { .. }
            | Request::DockerVolumeRemove { .. }
            | Request::DockerNetworkRemove { .. }
            | Request::DockerPrune { .. }
            | Request::IisSiteAction { .. }
            | Request::IisAppPoolAction { .. }
    )
}

pub fn for_stream(r: &StreamRequest) -> &'static str {
    match r {
        StreamRequest::ContainerLogs { .. } => LOGS_VIEW,
        StreamRequest::ContainerExec { .. } => CONTAINERS_EXEC,
        StreamRequest::ContainerStats { .. } | StreamRequest::ContainerStatsAll { .. } => DOCKER_VIEW,
        StreamRequest::SystemLive { .. } => SERVERS_VIEW,
        StreamRequest::IisLogFollow { .. } => LOGS_VIEW,
        StreamRequest::MssqlLogFollow { .. } => LOGS_VIEW,
    }
}

/// Seconds to wait for an agent reply.
pub fn timeout_secs(r: &Request) -> u64 {
    match r {
        Request::DockerPrune { .. } | Request::DockerImageRemove { .. } => 900,
        Request::DockerContainerAction { .. } | Request::DockerContainerRemove { .. } => 120,
        Request::DockerDiskUsage | Request::IisSiteAction { .. } | Request::IisAppPoolAction { .. } => 120,
        _ => 30,
    }
}
