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
pub const K8S_VIEW: &str = "k8s.view";
pub const K8S_CONTROL: &str = "k8s.control";
pub const K8S_EXEC: &str = "k8s.exec";
pub const SERVICES_VIEW: &str = "services.view";
pub const SERVICES_CONTROL: &str = "services.control";
pub const POSTGRES_VIEW: &str = "postgres.view";
pub const POSTGRES_CONTROL: &str = "postgres.control";

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
    PermInfo { key: K8S_VIEW, group: "Monitoring", label: "View Kubernetes", description: "Nodes, workloads, pods, services and events", dangerous: false },
    PermInfo { key: POSTGRES_VIEW, group: "Monitoring", label: "View PostgreSQL", description: "Databases, connections, replication and running queries (query text can contain data)", dangerous: false },
    PermInfo { key: SERVICES_VIEW, group: "Monitoring", label: "View services", description: "systemd units / Windows services and their state", dangerous: false },
    PermInfo { key: LOGS_VIEW, group: "Monitoring", label: "View logs", description: "Container, pod, IIS and SQL Server logs", dangerous: false },
    PermInfo { key: CONTAINERS_CONTROL, group: "Operations", label: "Control containers", description: "Start, stop, restart, pause and kill containers", dangerous: false },
    PermInfo { key: IIS_CONTROL, group: "Operations", label: "Control IIS", description: "Start/stop sites, recycle application pools", dangerous: false },
    PermInfo { key: K8S_CONTROL, group: "Operations", label: "Control Kubernetes", description: "Restart rollouts, scale workloads, delete pods, cordon nodes", dangerous: false },
    PermInfo { key: POSTGRES_CONTROL, group: "Operations", label: "Control PostgreSQL sessions", description: "Cancel queries and terminate connections", dangerous: true },
    PermInfo { key: SERVICES_CONTROL, group: "Operations", label: "Control services", description: "Start, stop, restart, enable and disable system services", dangerous: true },
    PermInfo { key: CONTAINERS_EXEC, group: "Operations", label: "Container terminal", description: "Open an interactive shell inside containers", dangerous: true },
    PermInfo { key: K8S_EXEC, group: "Operations", label: "Pod terminal", description: "Open an interactive shell inside Kubernetes pods", dangerous: true },
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

/// Permissions added to built-in roles by upgrades: (settings flag, role, permissions).
/// Applied once, so an administrator's later edits to those roles are respected.
pub fn role_upgrades() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        ("rbac_v1_2_k8s", "Operator", vec![K8S_VIEW, K8S_CONTROL, K8S_EXEC]),
        ("rbac_v1_2_k8s", "Tester", vec![K8S_VIEW]),
        ("rbac_v1_2_k8s", "Viewer", vec![K8S_VIEW]),
        ("rbac_v1_3_services", "Operator", vec![SERVICES_VIEW, SERVICES_CONTROL]),
        ("rbac_v1_3_services", "Tester", vec![SERVICES_VIEW]),
        ("rbac_v1_3_services", "Viewer", vec![SERVICES_VIEW]),
        ("rbac_v1_3_postgres", "Operator", vec![POSTGRES_VIEW, POSTGRES_CONTROL]),
        ("rbac_v1_3_postgres", "Tester", vec![POSTGRES_VIEW]),
        ("rbac_v1_3_postgres", "Viewer", vec![POSTGRES_VIEW]),
    ]
}

/// Built-in roles created on first start: (name, description, permissions, editable).
pub fn builtin_roles() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        ("Administrator", "Full access to everything", CATALOG.iter().map(|p| p.key).collect()),
        (
            "Operator",
            "View everything, read logs, control containers and IIS, open terminals. Cannot delete.",
            vec![SERVERS_VIEW, DOCKER_VIEW, MSSQL_VIEW, IIS_VIEW, K8S_VIEW, SERVICES_VIEW, POSTGRES_VIEW, LOGS_VIEW, CONTAINERS_CONTROL, IIS_CONTROL, K8S_CONTROL, SERVICES_CONTROL, POSTGRES_CONTROL, CONTAINERS_EXEC, K8S_EXEC, ALERTS_MANAGE],
        ),
        (
            "Tester",
            "View servers and logs, restart containers. No terminal, no deletes.",
            vec![SERVERS_VIEW, DOCKER_VIEW, MSSQL_VIEW, IIS_VIEW, K8S_VIEW, SERVICES_VIEW, POSTGRES_VIEW, LOGS_VIEW, CONTAINERS_CONTROL],
        ),
        ("Viewer", "Read-only dashboards and logs", vec![SERVERS_VIEW, DOCKER_VIEW, MSSQL_VIEW, IIS_VIEW, K8S_VIEW, SERVICES_VIEW, POSTGRES_VIEW, LOGS_VIEW]),
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
        Request::DockerContainerAction { .. } | Request::DockerContainerRename { .. } => CONTAINERS_CONTROL,
        // Recreating destroys the old container (its writable layer is lost).
        Request::DockerContainerRemove { .. } | Request::DockerContainerRecreate { .. } => CONTAINERS_DELETE,
        Request::DockerImageRemove { .. }
        | Request::DockerVolumeRemove { .. }
        | Request::DockerNetworkRemove { .. }
        | Request::DockerPrune { .. } => DOCKER_CLEANUP,
        Request::MssqlStatus => MSSQL_VIEW,
        Request::MssqlErrorLog { .. } => LOGS_VIEW,
        Request::IisSites | Request::IisAppPools | Request::IisLogFiles { .. } => IIS_VIEW,
        Request::IisSiteAction { .. } | Request::IisAppPoolAction { .. } => IIS_CONTROL,
        Request::IisLogTail { .. } => LOGS_VIEW,
        Request::K8sOverview
        | Request::K8sNamespaces
        | Request::K8sPods { .. }
        | Request::K8sWorkloads { .. }
        | Request::K8sServices { .. }
        | Request::K8sEvents { .. }
        | Request::K8sGet { .. } => K8S_VIEW,
        Request::K8sPodDelete { .. } | Request::K8sRolloutRestart { .. } | Request::K8sScale { .. } | Request::K8sNodeCordon { .. } | Request::K8sCronJobTrigger { .. } => K8S_CONTROL,
        Request::K8sResources { .. } | Request::K8sNodeDetail { .. } | Request::K8sNamespaceUsage => K8S_VIEW,
        Request::ServicesList => SERVICES_VIEW,
        Request::ScheduleList => SERVICES_VIEW,
        Request::ScheduleAction { .. } => SERVICES_CONTROL,
        Request::PgOverview | Request::PgActivity => POSTGRES_VIEW,
        Request::PgBackend { .. } => POSTGRES_CONTROL,
        Request::ServiceAction { .. } => SERVICES_CONTROL,
        Request::DockerStacks | Request::DockerStackFile { .. } => DOCKER_VIEW,
        // Redeploying replaces containers (like recreate); start / stop / restart are routine.
        Request::DockerStackAction { action, .. } => match action {
            monitorlab_common::StackAction::Redeploy | monitorlab_common::StackAction::PullRedeploy => CONTAINERS_DELETE,
            _ => CONTAINERS_CONTROL,
        },
        Request::SelfUpdate { .. } => SERVERS_MANAGE,
    }
}

/// Requests that change state are written to the audit log.
pub fn is_mutating(r: &Request) -> bool {
    matches!(
        r,
        Request::DockerContainerAction { .. }
            | Request::DockerContainerRemove { .. }
            | Request::DockerContainerRename { .. }
            | Request::DockerContainerRecreate { .. }
            | Request::DockerImageRemove { .. }
            | Request::DockerVolumeRemove { .. }
            | Request::DockerNetworkRemove { .. }
            | Request::DockerPrune { .. }
            | Request::IisSiteAction { .. }
            | Request::IisAppPoolAction { .. }
            | Request::K8sPodDelete { .. }
            | Request::K8sRolloutRestart { .. }
            | Request::K8sScale { .. }
            | Request::K8sNodeCordon { .. }
            | Request::K8sCronJobTrigger { .. }
            | Request::ServiceAction { .. }
            | Request::PgBackend { .. }
            | Request::ScheduleAction { .. }
            | Request::DockerStackAction { .. }
            | Request::SelfUpdate { .. }
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
        StreamRequest::K8sPodLogs { .. } => LOGS_VIEW,
        StreamRequest::K8sPodExec { .. } => K8S_EXEC,
        StreamRequest::StackLogs { .. } => LOGS_VIEW,
        StreamRequest::SystemLogs { .. } => LOGS_VIEW,
    }
}

/// Seconds to wait for an agent reply.
pub fn timeout_secs(r: &Request) -> u64 {
    match r {
        Request::DockerPrune { .. } | Request::DockerImageRemove { .. } | Request::DockerContainerRecreate { .. } | Request::DockerStackAction { .. } | Request::SelfUpdate { .. } => 900,
        Request::DockerContainerAction { .. } | Request::DockerContainerRemove { .. } => 120,
        Request::DockerDiskUsage | Request::IisSiteAction { .. } | Request::IisAppPoolAction { .. } => 120,
        _ => 30,
    }
}
