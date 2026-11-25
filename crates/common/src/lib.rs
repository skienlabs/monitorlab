//! Shared wire protocol between the MonitorLab hub and its agents.
//!
//! Transport: a single WebSocket per agent, opened *by the agent* (outbound only),
//! carrying JSON text frames. The hub sends [`HubMsg`], the agent sends [`AgentMsg`].
//!
//! Two interaction styles are multiplexed on the socket:
//! * unary RPC  — `HubMsg::Request{id}` answered by exactly one `AgentMsg::Reply{id}`
//! * streams    — `HubMsg::Open{sid}` followed by any number of `AgentMsg::Stream{sid}`
//!   and terminated by `AgentMsg::StreamEnd{sid}` (agent) or `HubMsg::Close{sid}` (hub).
//!
//! Security note: requests never carry arbitrary file paths or shell strings that the
//! agent would interpret on the host. Log files are addressed by logical ids (IIS site id,
//! SQL Server instance), which the agent resolves itself.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum AgentMsg {
    Hello(AgentHello),
    Metrics(MetricsSample),
    Reply {
        id: u64,
        ok: bool,
        #[serde(default)]
        data: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Stream {
        sid: u64,
        data: StreamData,
    },
    StreamEnd {
        sid: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HubMsg {
    Request { id: u64, req: Request },
    Open { sid: u64, req: StreamRequest },
    /// Terminal input, base64 encoded raw bytes.
    Input { sid: u64, b64: String },
    Resize { sid: u64, cols: u16, rows: u16 },
    Close { sid: u64 },
    Ping,
}

// ---------------------------------------------------------------------------------------
// Agent identity
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsFamily {
    Linux,
    Windows,
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub docker: bool,
    #[serde(default)]
    pub docker_version: Option<String>,
    pub mssql: bool,
    pub iis: bool,
    /// Kubernetes API reachable (k3s/k8s on this server).
    #[serde(default)]
    pub kubernetes: bool,
    #[serde(default)]
    pub kubernetes_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHello {
    pub agent_version: String,
    pub protocol: u32,
    pub hostname: String,
    pub os: OsFamily,
    pub os_name: String,
    pub os_version: String,
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_cores: u32,
    #[serde(default)]
    pub physical_cores: Option<u32>,
    pub total_memory: u64,
    pub boot_time: u64,
    #[serde(default)]
    pub ips: Vec<String>,
    pub capabilities: Capabilities,
    /// Interval (seconds) at which the agent pushes `Metrics`.
    pub interval_secs: u32,
}

// ---------------------------------------------------------------------------------------
// Periodic metrics
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSample {
    /// Unix epoch milliseconds.
    pub ts: i64,
    /// Overall CPU usage, 0..100.
    pub cpu: f32,
    #[serde(default)]
    pub cpu_cores: Vec<f32>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    #[serde(default)]
    pub load: Option<[f64; 3]>,
    pub uptime: u64,
    /// Bytes per second, summed over physical interfaces.
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    #[serde(default)]
    pub disks: Vec<DiskInfo>,
    pub process_count: u32,
    #[serde(default)]
    pub docker: Option<DockerSummary>,
    #[serde(default)]
    pub mssql: Option<MssqlSummary>,
    #[serde(default)]
    pub iis: Option<IisSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k8s: Option<K8sSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskInfo {
    pub mount: String,
    pub device: String,
    pub fs: String,
    pub total: u64,
    pub used: u64,
    #[serde(default)]
    pub removable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DockerSummary {
    pub running: u32,
    pub paused: u32,
    pub stopped: u32,
    pub total: u32,
    pub images: u32,
    /// Containers that are restarting or unhealthy.
    #[serde(default)]
    pub unhealthy: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MssqlSummary {
    pub instances: Vec<MssqlInstanceSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MssqlInstanceSummary {
    pub name: String,
    /// running | stopped | unknown ...
    pub state: String,
    pub cpu: f32,
    pub mem: u64,
}

/// Cluster health in each metrics sample (refreshed about every 30 s).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct K8sSummary {
    pub nodes: u32,
    pub nodes_ready: u32,
    pub pods: u32,
    pub pods_running: u32,
    /// CrashLoopBackOff, ImagePullBackOff, Failed, or Pending for more than 5 minutes.
    pub pods_failing: u32,
    pub workloads: u32,
    /// Deployments / StatefulSets / DaemonSets with fewer ready replicas than desired.
    pub workloads_degraded: u32,
    #[serde(default)]
    pub not_ready_nodes: Vec<String>,
    /// `namespace/name: reason`, at most 10.
    #[serde(default)]
    pub failing: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IisSummary {
    pub sites_total: u32,
    pub sites_started: u32,
    pub pools_total: u32,
    pub pools_started: u32,
    pub worker_cpu: f32,
    pub worker_mem: u64,
}

// ---------------------------------------------------------------------------------------
// Unary requests
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum K8sKind {
    Pod,
    Deployment,
    StatefulSet,
    DaemonSet,
    Service,
    Node,
    Namespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneTarget {
    /// Stopped containers.
    Containers,
    /// Dangling (untagged) images.
    ImagesDangling,
    /// All images not used by any container.
    ImagesUnused,
    /// Volumes not used by any container (anonymous and named).
    Volumes,
    BuildCache,
    Networks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IisSiteAction {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IisPoolAction {
    Start,
    Stop,
    Recycle,
}

fn default_lines() -> u32 {
    500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// OS details, top processes, disks, network interfaces.
    SystemDetail,

    DockerContainers,
    DockerInspect { id: String },
    DockerContainerAction { id: String, action: ContainerAction },
    DockerContainerRemove {
        id: String,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        volumes: bool,
    },
    DockerContainerRename { id: String, name: String },
    /// Re-create a container with the same settings (optionally pulling its image first),
    /// like Portainer's "Recreate" / compose's "up" after a pull.
    DockerContainerRecreate {
        id: String,
        #[serde(default)]
        pull: bool,
    },
    K8sOverview,
    K8sNamespaces,
    K8sPods {
        #[serde(default)]
        namespace: Option<String>,
    },
    K8sWorkloads {
        #[serde(default)]
        namespace: Option<String>,
    },
    K8sServices {
        #[serde(default)]
        namespace: Option<String>,
    },
    K8sEvents {
        #[serde(default)]
        namespace: Option<String>,
    },
    /// One object as JSON (managedFields removed), for the details view.
    K8sGet { kind: K8sKind, namespace: Option<String>, name: String },
    K8sPodDelete {
        namespace: String,
        name: String,
        #[serde(default)]
        force: bool,
    },
    K8sRolloutRestart { kind: K8sKind, namespace: String, name: String },
    K8sScale { kind: K8sKind, namespace: String, name: String, replicas: u32 },
    K8sNodeCordon { name: String, unschedulable: bool },
    DockerImages,
    DockerImageRemove {
        id: String,
        #[serde(default)]
        force: bool,
    },
    DockerVolumes,
    DockerVolumeRemove { name: String },
    DockerNetworks,
    DockerNetworkRemove { id: String },
    DockerDiskUsage,
    DockerPrune { target: PruneTarget },

    MssqlStatus,
    MssqlErrorLog {
        #[serde(default)]
        instance: Option<String>,
        #[serde(default = "default_lines")]
        lines: u32,
    },

    IisSites,
    IisAppPools,
    IisSiteAction { name: String, action: IisSiteAction },
    IisAppPoolAction { name: String, action: IisPoolAction },
    IisLogFiles { site_id: u64 },
    IisLogTail {
        site_id: u64,
        #[serde(default)]
        file: Option<String>,
        #[serde(default = "default_lines")]
        lines: u32,
    },
}

impl Request {
    /// Stable short name, used for audit logs.
    pub fn name(&self) -> &'static str {
        match self {
            Request::SystemDetail => "system_detail",
            Request::DockerContainers => "docker_containers",
            Request::DockerInspect { .. } => "docker_inspect",
            Request::DockerContainerAction { .. } => "docker_container_action",
            Request::DockerContainerRemove { .. } => "docker_container_remove",
            Request::DockerContainerRename { .. } => "docker_container_rename",
            Request::DockerContainerRecreate { .. } => "docker_container_recreate",
            Request::K8sOverview => "k8s_overview",
            Request::K8sNamespaces => "k8s_namespaces",
            Request::K8sPods { .. } => "k8s_pods",
            Request::K8sWorkloads { .. } => "k8s_workloads",
            Request::K8sServices { .. } => "k8s_services",
            Request::K8sEvents { .. } => "k8s_events",
            Request::K8sGet { .. } => "k8s_get",
            Request::K8sPodDelete { .. } => "k8s_pod_delete",
            Request::K8sRolloutRestart { .. } => "k8s_rollout_restart",
            Request::K8sScale { .. } => "k8s_scale",
            Request::K8sNodeCordon { .. } => "k8s_node_cordon",
            Request::DockerImages => "docker_images",
            Request::DockerImageRemove { .. } => "docker_image_remove",
            Request::DockerVolumes => "docker_volumes",
            Request::DockerVolumeRemove { .. } => "docker_volume_remove",
            Request::DockerNetworks => "docker_networks",
            Request::DockerNetworkRemove { .. } => "docker_network_remove",
            Request::DockerDiskUsage => "docker_disk_usage",
            Request::DockerPrune { .. } => "docker_prune",
            Request::MssqlStatus => "mssql_status",
            Request::MssqlErrorLog { .. } => "mssql_error_log",
            Request::IisSites => "iis_sites",
            Request::IisAppPools => "iis_app_pools",
            Request::IisSiteAction { .. } => "iis_site_action",
            Request::IisAppPoolAction { .. } => "iis_app_pool_action",
            Request::IisLogFiles { .. } => "iis_log_files",
            Request::IisLogTail { .. } => "iis_log_tail",
        }
    }
}

// ---------------------------------------------------------------------------------------
// Streams
// ---------------------------------------------------------------------------------------

fn default_tail() -> u32 {
    300
}
fn default_true() -> bool {
    true
}
fn default_cols() -> u16 {
    120
}
fn default_rows() -> u16 {
    32
}
fn default_live_ms() -> u32 {
    2000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamRequest {
    ContainerLogs {
        id: String,
        #[serde(default = "default_tail")]
        tail: u32,
        /// Unix seconds; when set, `tail` is ignored by Docker semantics (all since).
        #[serde(default)]
        since: Option<i64>,
        #[serde(default = "default_true")]
        follow: bool,
    },
    ContainerExec {
        id: String,
        /// Defaults to an auto-detected shell (bash, falling back to sh).
        #[serde(default)]
        cmd: Option<Vec<String>>,
        #[serde(default)]
        user: Option<String>,
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
    },
    ContainerStats { id: String },
    /// Live CPU / memory of every running container, one batch every ~2 s.
    ContainerStatsAll {},
    K8sPodLogs {
        namespace: String,
        name: String,
        #[serde(default)]
        container: Option<String>,
        #[serde(default = "default_tail")]
        tail: u32,
        /// Unix seconds; when set, replaces `tail` (used to resume after a reconnect).
        #[serde(default)]
        since: Option<i64>,
        #[serde(default = "default_true")]
        follow: bool,
        /// Logs of the previous (crashed) container instance.
        #[serde(default)]
        previous: bool,
    },
    K8sPodExec {
        namespace: String,
        name: String,
        #[serde(default)]
        container: Option<String>,
        #[serde(default)]
        cmd: Option<Vec<String>>,
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
    },
    SystemLive {
        #[serde(default = "default_live_ms")]
        interval_ms: u32,
    },
    IisLogFollow {
        site_id: u64,
        #[serde(default = "default_tail")]
        tail: u32,
    },
    MssqlLogFollow {
        #[serde(default)]
        instance: Option<String>,
        #[serde(default = "default_tail")]
        tail: u32,
    },
}

impl StreamRequest {
    pub fn name(&self) -> &'static str {
        match self {
            StreamRequest::ContainerLogs { .. } => "container_logs",
            StreamRequest::ContainerExec { .. } => "container_exec",
            StreamRequest::ContainerStats { .. } => "container_stats",
            StreamRequest::ContainerStatsAll { .. } => "container_stats_all",
            StreamRequest::K8sPodLogs { .. } => "k8s_pod_logs",
            StreamRequest::K8sPodExec { .. } => "k8s_pod_exec",
            StreamRequest::SystemLive { .. } => "system_live",
            StreamRequest::IisLogFollow { .. } => "iis_log_follow",
            StreamRequest::MssqlLogFollow { .. } => "mssql_log_follow",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    /// RFC3339 timestamp as produced by the source, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    /// 1 = stdout, 2 = stderr, 0 = unknown/file.
    pub s: u8,
    pub text: String,
}

/// Compact per-container sample for the containers list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerLiveStat {
    pub id: String,
    /// Docker convention: 100 = one full CPU core.
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_limit: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContainerStatsSample {
    pub ts: i64,
    pub cpu: f64,
    pub mem_used: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub blk_read: u64,
    pub blk_write: u64,
    pub pids: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum StreamData {
    Lines { lines: Vec<LogLine> },
    /// Raw terminal output, base64.
    Bytes { b64: String },
    Stats(ContainerStatsSample),
    StatsAll { ts: i64, items: Vec<ContainerLiveStat> },
    Live(MetricsSample),
    Info { msg: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_request() {
        let r = Request::DockerContainerAction { id: "abc".into(), action: ContainerAction::Restart };
        let s = serde_json::to_string(&HubMsg::Request { id: 7, req: r }).unwrap();
        assert!(s.contains("\"op\":\"docker_container_action\""));
        let back: HubMsg = serde_json::from_str(&s).unwrap();
        matches!(back, HubMsg::Request { id: 7, .. });
    }

    #[test]
    fn parse_from_browser_json() {
        let r: Request = serde_json::from_str(r#"{"op":"docker_prune","target":"build_cache"}"#).unwrap();
        assert!(matches!(r, Request::DockerPrune { target: PruneTarget::BuildCache }));
        let s: StreamRequest = serde_json::from_str(r#"{"kind":"container_logs","id":"x"}"#).unwrap();
        assert!(matches!(s, StreamRequest::ContainerLogs { tail: 300, follow: true, .. }));
    }

    #[test]
    fn agent_msgs() {
        let m = AgentMsg::Stream { sid: 1, data: StreamData::Lines { lines: vec![LogLine { ts: None, s: 1, text: "hi".into() }] } };
        let s = serde_json::to_string(&m).unwrap();
        let _: AgentMsg = serde_json::from_str(&s).unwrap();
        let m = AgentMsg::Metrics(MetricsSample::default());
        let s = serde_json::to_string(&m).unwrap();
        let _: AgentMsg = serde_json::from_str(&s).unwrap();
    }
}
