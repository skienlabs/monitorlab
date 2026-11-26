export interface Me {
  id: number;
  email: string;
  name: string;
  role: string;
  role_id: number;
  permissions: string[];
  server_scope: string[] | null;
  must_change_password: boolean;
  title?: string;
  phone?: string;
  avatar?: string | null;
}

export interface Disk {
  mount: string;
  device: string;
  fs: string;
  total: number;
  used: number;
  removable: boolean;
}

export interface Metrics {
  ts: number;
  cpu: number;
  cpu_cores?: number[];
  mem_total: number;
  mem_used: number;
  swap_total: number;
  swap_used: number;
  load: [number, number, number] | null;
  uptime: number;
  net_rx_bps: number;
  net_tx_bps: number;
  disk_read_bps: number;
  disk_write_bps: number;
  disks: Disk[];
  process_count: number;
  docker: { running: number; paused: number; stopped: number; total: number; images: number; unhealthy: number } | null;
  mssql: { instances: { name: string; state: string; cpu: number; mem: number }[] } | null;
  k8s?: { nodes: number; nodes_ready: number; pods: number; pods_running: number; pods_failing: number; workloads: number; workloads_degraded: number; not_ready_nodes: string[]; failing: string[] } | null;
  iis: { sites_total: number; sites_started: number; pools_total: number; pools_started: number; worker_cpu: number; worker_mem: number } | null;
}

export interface Hello {
  agent_version: string;
  hostname: string;
  os: "linux" | "windows" | "macos" | "other";
  os_name: string;
  os_version: string;
  kernel: string;
  arch: string;
  cpu_model: string;
  cpu_cores: number;
  physical_cores: number | null;
  total_memory: number;
  boot_time: number;
  ips: string[];
  capabilities: { docker: boolean; docker_version: string | null; mssql: boolean; iis: boolean; kubernetes?: boolean; kubernetes_version?: string | null; services?: boolean; system_logs?: boolean; self_update?: boolean; postgres?: boolean };
  interval_secs: number;
}

export type Status = "ok" | "warning" | "critical" | "offline" | "pending";

export interface Server {
  id: string;
  name: string;
  is_local: boolean;
  maintenance_until?: number | null;
  custom_alert_rules?: boolean;
  update_available?: boolean;
  agent_update?: { at: number; ok: boolean; message: string } | null;
  enrolled: boolean;
  token_pending: boolean;
  online: boolean;
  status: Status;
  reasons: string[];
  remote: string | null;
  connected_at: number | null;
  created_at: number;
  last_seen_at: number | null;
  hello: Hello | null;
  latest: Metrics | null;
}

export interface Container {
  id: string;
  name: string;
  names: string[];
  image: string;
  image_id: string;
  command: string;
  created: number;
  state: string;
  status: string;
  health: "healthy" | "unhealthy" | "starting" | null;
  ports: { ip: string | null; private: number; public: number | null; proto: string }[];
  project: string | null;
  service: string | null;
  labels: Record<string, string>;
  networks: string[];
  mounts: { type: string | null; name: string | null; source: string | null; destination: string | null; rw: boolean | null }[];
}

export interface LogLine {
  ts?: string;
  s: number;
  text: string;
  src?: string;
  lvl?: number;
}
