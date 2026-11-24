//! Host metrics via `sysinfo`. Rates (network, disk I/O) are computed from cumulative
//! counters and wall-clock time between samples, so any sampling interval is correct.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{json, Value};
use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
};
use monitorlab_common::{AgentHello, Capabilities, DiskInfo, MetricsSample, OsFamily, PROTOCOL_VERSION, VERSION};

use crate::util::{host_path, now_ms};

/// Network interfaces that only carry traffic already counted on a physical NIC.
pub fn is_virtual_iface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "lo"
        || n.starts_with("veth")
        || n.starts_with("docker")
        || n.starts_with("br-")
        || n.starts_with("virbr")
        || n.starts_with("cni")
        || n.starts_with("flannel")
        || n.starts_with("cali")
        || n.starts_with("vxlan")
        || n.starts_with("tun")
        || n.contains("loopback")
        || n.starts_with("isatap")
        || n.starts_with("teredo")
}

const SKIP_FS: &[&str] = &[
    "overlay", "tmpfs", "devtmpfs", "squashfs", "proc", "sysfs", "cgroup", "cgroup2", "nsfs", "autofs", "devpts",
    "mqueue", "debugfs", "tracefs", "securityfs", "pstore", "bpf", "configfs", "fusectl", "hugetlbfs", "ramfs",
    "fuse.lxcfs", "binfmt_misc", "efivarfs", "iso9660",
];

fn skip_mount(mount: &str) -> bool {
    mount.starts_with("/snap/")
        || mount.starts_with("/var/lib/docker/")
        || mount.starts_with("/var/lib/containerd/")
        || mount.starts_with("/run")
        || mount.starts_with("/dev")
        || mount.starts_with("/proc")
        || mount.starts_with("/sys")
        || mount.starts_with("/etc/")
}

pub struct Collector {
    sys: System,
    nets: Networks,
    disks: Disks,
    disks_listed_at: Instant,
    last_at: Instant,
    last_net: HashMap<String, (u64, u64)>,
    last_disk_io: HashMap<String, (u64, u64)>,
    host_root: Option<String>,
    with_processes: bool,
    /// Host `/sys/class/net` when running in a container on its own network namespace.
    host_net: Option<std::path::PathBuf>,
}

impl Collector {
    pub fn new(host_root: Option<String>, with_processes: bool) -> Self {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_cpu_usage();
        if with_processes {
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, proc_kind());
        }
        let mut c = Self {
            sys,
            nets: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            disks_listed_at: Instant::now(),
            last_at: Instant::now(),
            last_net: HashMap::new(),
            last_disk_io: HashMap::new(),
            host_net: crate::hostnet::detect(host_root.as_deref()),
            host_root,
            with_processes,
        };
        if let Some(dir) = &c.host_net {
            tracing::info!("network metrics read from the host ({})", dir.display());
        }
        // Prime counters so the first real sample has a baseline.
        c.net_totals();
        c.disk_io_totals();
        c
    }

    fn net_totals(&mut self) -> (f64, f64, f64) {
        let mut rx = 0u64;
        let mut tx = 0u64;
        let mut cur = HashMap::new();
        if let Some(dir) = self.host_net.clone() {
            for i in crate::hostnet::read_ifaces(&dir).into_iter().filter(|i| i.physical) {
                if let Some((pr, pt)) = self.last_net.get(&i.name) {
                    rx += i.rx.saturating_sub(*pr);
                    tx += i.tx.saturating_sub(*pt);
                }
                cur.insert(i.name, (i.rx, i.tx));
            }
            self.last_net = cur;
            return (rx as f64, tx as f64, 0.0);
        }
        for (name, data) in self.nets.list() {
            if is_virtual_iface(name) {
                continue;
            }
            let (r, t) = (data.total_received(), data.total_transmitted());
            if let Some((pr, pt)) = self.last_net.get(name) {
                rx += r.saturating_sub(*pr);
                tx += t.saturating_sub(*pt);
            }
            cur.insert(name.clone(), (r, t));
        }
        self.last_net = cur;
        (rx as f64, tx as f64, 0.0)
    }

    fn disk_io_totals(&mut self) -> (f64, f64) {
        #[cfg(target_os = "linux")]
        if let Some(stats) = crate::diskstats::read(self.host_root.as_deref()) {
            let mut rd = 0u64;
            let mut wr = 0u64;
            let mut cur = HashMap::new();
            for (name, r, w) in stats {
                if let Some((pr, pw)) = self.last_disk_io.get(&name) {
                    rd += r.saturating_sub(*pr);
                    wr += w.saturating_sub(*pw);
                }
                cur.insert(name, (r, w));
            }
            self.last_disk_io = cur;
            return (rd as f64, wr as f64);
        }
        let mut rd = 0u64;
        let mut wr = 0u64;
        let mut cur = HashMap::new();
        for d in self.disks.list() {
            let key = d.name().to_string_lossy().to_string();
            if cur.contains_key(&key) {
                continue; // same device mounted twice
            }
            let u = d.usage();
            if let Some((pr, pw)) = self.last_disk_io.get(&key) {
                rd += u.total_read_bytes.saturating_sub(*pr);
                wr += u.total_written_bytes.saturating_sub(*pw);
            }
            cur.insert(key, (u.total_read_bytes, u.total_written_bytes));
        }
        self.last_disk_io = cur;
        (rd as f64, wr as f64)
    }

    pub fn sample(&mut self) -> MetricsSample {
        let elapsed = self.last_at.elapsed().as_secs_f64().max(0.001);
        self.last_at = Instant::now();

        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        if self.with_processes {
            self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, proc_kind());
        }
        self.nets.refresh(true);
        if self.disks_listed_at.elapsed().as_secs() > 60 {
            self.disks = Disks::new_with_refreshed_list();
            self.disks_listed_at = Instant::now();
        } else {
            self.disks.refresh(false);
        }

        let (rx, tx, _) = self.net_totals();
        let (rd, wr) = self.disk_io_totals();

        let load = if cfg!(windows) {
            None
        } else {
            let l = System::load_average();
            Some([l.one, l.five, l.fifteen])
        };

        MetricsSample {
            ts: now_ms(),
            cpu: finite(self.sys.global_cpu_usage()),
            cpu_cores: self.sys.cpus().iter().map(|c| finite(c.cpu_usage())).collect(),
            mem_total: self.sys.total_memory(),
            mem_used: self.sys.used_memory(),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            load,
            uptime: System::uptime(),
            net_rx_bps: rx / elapsed,
            net_tx_bps: tx / elapsed,
            disk_read_bps: rd / elapsed,
            disk_write_bps: wr / elapsed,
            disks: self.disk_list(),
            process_count: self.sys.processes().values().filter(|p| is_process(p)).count() as u32,
            docker: None,
            mssql: None,
            iis: None,
        }
    }

    pub fn disk_list(&self) -> Vec<DiskInfo> {
        let root = self.host_root.as_deref().filter(|r| !r.is_empty() && *r != "/");
        let mut by_device: HashMap<String, DiskInfo> = HashMap::new();
        for d in self.disks.list() {
            let fs = d.file_system().to_string_lossy().to_string();
            let mut mount = d.mount_point().to_string_lossy().to_string();
            if !cfg!(windows) {
                if SKIP_FS.contains(&fs.as_str()) {
                    continue;
                }
                if let Some(root) = root {
                    if mount == root {
                        mount = "/".into();
                    } else if let Some(rest) = mount.strip_prefix(&format!("{root}/")) {
                        mount = format!("/{rest}");
                    } else {
                        continue; // container-only mount
                    }
                }
                if skip_mount(&mount) {
                    continue;
                }
            }
            if d.total_space() == 0 {
                continue;
            }
            let info = DiskInfo {
                mount: mount.clone(),
                device: d.name().to_string_lossy().to_string(),
                fs,
                total: d.total_space(),
                used: d.total_space().saturating_sub(d.available_space()),
                removable: d.is_removable(),
            };
            // Keep the shortest mount path per device (bind mounts repeat the same device).
            match by_device.get(&info.device) {
                Some(existing) if existing.mount.len() <= info.mount.len() => {}
                _ => {
                    by_device.insert(info.device.clone(), info);
                }
            }
        }
        let mut v: Vec<DiskInfo> = by_device.into_values().collect();
        v.sort_by(|a, b| a.mount.cmp(&b.mount));
        v
    }

    pub fn system(&self) -> &System {
        &self.sys
    }

    /// CPU (normalised to 0..100 of the whole machine) and resident memory of the given processes.
    pub fn proc_usage<F: Fn(&sysinfo::Process) -> bool>(&self, pred: F) -> (f32, u64, Vec<u32>) {
        let ncpu = self.sys.cpus().len().max(1) as f32;
        let mut cpu = 0.0;
        let mut mem = 0;
        let mut pids = Vec::new();
        for (pid, p) in self.sys.processes() {
            if is_process(p) && pred(p) {
                cpu += p.cpu_usage();
                mem += p.memory();
                pids.push(pid.as_u32());
            }
        }
        (finite(cpu / ncpu), mem, pids)
    }

    pub fn proc_by_pid(&self, pid: u32) -> Option<(f32, u64)> {
        let ncpu = self.sys.cpus().len().max(1) as f32;
        self.sys.processes().get(&sysinfo::Pid::from_u32(pid)).map(|p| (finite(p.cpu_usage() / ncpu), p.memory()))
    }

    pub fn hello(&self, caps: Capabilities, interval_secs: u32) -> AgentHello {
        let root = self.host_root.as_deref();
        let (os_name, os_version) = os_release(root);
        let hostname = read_host_hostname(root).or_else(System::host_name).unwrap_or_else(|| "unknown".into());
        let cpu_model = self.sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default();
        let mut ips: Vec<String> = Vec::new();
        if self.host_net.is_some() {
            ips = crate::hostnet::host_ipv4();
        }
        for (name, data) in self.nets.list().iter().filter(|_| self.host_net.is_none()) {
            if is_virtual_iface(name) {
                continue;
            }
            for ipn in data.ip_networks() {
                let ip = ipn.addr;
                if ip.is_loopback() {
                    continue;
                }
                if let std::net::IpAddr::V6(v6) = ip {
                    if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                        continue; // link-local
                    }
                }
                ips.push(ip.to_string());
            }
        }
        ips.sort_by_key(|ip| (ip.contains(':'), ip.clone()));
        ips.dedup();

        AgentHello {
            agent_version: VERSION.to_string(),
            protocol: PROTOCOL_VERSION,
            hostname,
            os: if cfg!(windows) {
                OsFamily::Windows
            } else if cfg!(target_os = "linux") {
                OsFamily::Linux
            } else {
                OsFamily::Other
            },
            os_name,
            os_version,
            kernel: System::kernel_version().unwrap_or_default(),
            arch: System::cpu_arch(),
            cpu_model,
            cpu_cores: self.sys.cpus().len() as u32,
            physical_cores: self.sys.physical_core_count().map(|c| c as u32),
            total_memory: self.sys.total_memory(),
            boot_time: System::boot_time(),
            ips,
            capabilities: caps,
            interval_secs,
        }
    }

    /// Detailed view for the server page: OS, interfaces, disks and top processes.
    /// Takes two process snapshots ~500ms apart so CPU percentages are meaningful.
    pub fn detail(host_root: Option<String>) -> Value {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        let kind = ProcessRefreshKind::nothing().with_cpu().with_memory().with_user(sysinfo::UpdateKind::OnlyIfNotSet);
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        std::thread::sleep(std::time::Duration::from_millis(500));
        sys.refresh_cpu_usage();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        let ncpu = sys.cpus().len().max(1) as f32;

        let users = sysinfo::Users::new_with_refreshed_list();
        let mut procs: Vec<Value> = sys
            .processes()
            .iter()
            .filter(|(_, p)| is_process(p))
            .map(|(pid, p)| {
                let user = p
                    .user_id()
                    .and_then(|uid| users.get_user_by_id(uid))
                    .map(|u| u.name().to_string())
                    .unwrap_or_default();
                json!({
                    "pid": pid.as_u32(),
                    "name": p.name().to_string_lossy(),
                    "cpu": finite(p.cpu_usage() / ncpu),
                    "mem": p.memory(),
                    "user": user,
                    "start": p.start_time(),
                })
            })
            .collect();
        procs.sort_by(|a, b| {
            let ka = a["cpu"].as_f64().unwrap_or(0.0) * 1e12 + a["mem"].as_f64().unwrap_or(0.0);
            let kb = b["cpu"].as_f64().unwrap_or(0.0) * 1e12 + b["mem"].as_f64().unwrap_or(0.0);
            kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut by_mem = procs.clone();
        by_mem.sort_by_key(|p| std::cmp::Reverse(p["mem"].as_u64().unwrap_or(0)));
        procs.truncate(30);
        by_mem.truncate(30);

        let host_net = crate::hostnet::detect(host_root.as_deref());
        let nets = Networks::new_with_refreshed_list();
        let mut ifaces: Vec<Value> = if let Some(dir) = &host_net {
            let ips = crate::hostnet::host_ipv4();
            crate::hostnet::read_ifaces(dir)
                .into_iter()
                .filter(|i| i.name != "lo" && !i.name.starts_with("veth"))
                .map(|i| {
                    json!({
                        "name": i.name,
                        "mac": i.mac,
                        "ips": if i.physical { ips.clone() } else { vec![] },
                        "rx_total": i.rx,
                        "tx_total": i.tx,
                        "virtual": !i.physical,
                    })
                })
                .collect()
        } else {
            nets
            .list()
            .iter()
            .filter(|(n, _)| n.as_str() != "lo" && !n.starts_with("veth"))
            .map(|(name, d)| {
                json!({
                    "name": name,
                    "mac": d.mac_address().to_string(),
                    "ips": d.ip_networks().iter().map(|i| format!("{}/{}", i.addr, i.prefix)).collect::<Vec<_>>(),
                    "rx_total": d.total_received(),
                    "tx_total": d.total_transmitted(),
                    "virtual": is_virtual_iface(name),
                })
            })
            .collect()
        };
        ifaces.sort_by_key(|v| (v["virtual"].as_bool().unwrap_or(false), v["name"].as_str().unwrap_or("").to_string()));

        let root = host_root.as_deref();
        let (os_name, os_version) = os_release(root);
        json!({
            "hostname": read_host_hostname(root).or_else(System::host_name),
            "os_name": os_name,
            "os_version": os_version,
            "kernel": System::kernel_version(),
            "arch": System::cpu_arch(),
            "uptime": System::uptime(),
            "boot_time": System::boot_time(),
            "cpu_model": sys.cpus().first().map(|c| c.brand().trim().to_string()),
            "cpu_freq_mhz": sys.cpus().first().map(|c| c.frequency()),
            "cores": sys.cpus().len(),
            "physical_cores": sys.physical_core_count(),
            "mem_total": sys.total_memory(),
            "mem_used": sys.used_memory(),
            "mem_available": sys.available_memory(),
            "swap_total": sys.total_swap(),
            "swap_used": sys.used_swap(),
            "process_count": sys.processes().values().filter(|p| is_process(p)).count(),
            "top_cpu": procs,
            "top_mem": by_mem,
            "interfaces": ifaces,
        })
    }
}

/// sysinfo lists Linux threads as processes too; count each process once
/// (a process's CPU/RSS already includes all of its threads).
fn is_process(p: &sysinfo::Process) -> bool {
    p.thread_kind().is_none()
}

fn proc_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().with_cpu().with_memory()
}

fn finite(v: f32) -> f32 {
    if v.is_finite() {
        (v * 100.0).round() / 100.0
    } else {
        0.0
    }
}

fn read_host_hostname(root: Option<&str>) -> Option<String> {
    if cfg!(windows) || root.is_none() {
        return None;
    }
    std::fs::read_to_string(host_path(root, "/etc/hostname")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn os_release(root: Option<&str>) -> (String, String) {
    if !cfg!(windows) {
        if let Ok(text) = std::fs::read_to_string(host_path(root, "/etc/os-release")) {
            let mut name = String::new();
            let mut version = String::new();
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                    name = v.trim_matches('"').to_string();
                } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
                    version = v.trim_matches('"').to_string();
                }
            }
            if !name.is_empty() {
                return (name, version);
            }
        }
    }
    (
        System::long_os_version().or_else(System::name).unwrap_or_else(|| "Unknown".into()),
        System::os_version().unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iface_filter() {
        assert!(is_virtual_iface("veth12ab"));
        assert!(is_virtual_iface("docker0"));
        assert!(is_virtual_iface("Loopback Pseudo-Interface 1"));
        assert!(!is_virtual_iface("eth0"));
        assert!(!is_virtual_iface("Ethernet0"));
    }

    #[test]
    fn sample_works() {
        let mut c = Collector::new(None, true);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let s = c.sample();
        assert!(s.mem_total > 0);
        assert!(s.cpu >= 0.0);
        assert!(s.process_count > 0);
    }
}
