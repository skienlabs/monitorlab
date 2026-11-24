//! Host network view for an agent running inside a container on a Docker network (e.g. the hub's
//! built-in agent on `skiensuite-net`). The container's own interfaces only carry its own traffic,
//! so counters come from the host's sysfs, reachable through the read-only host-root mount
//! (`/:/host:ro` → `/host/sys/class/net`). Plain file reads: no extra capabilities needed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct HostIface {
    pub name: String,
    pub rx: u64,
    pub tx: u64,
    /// Backed by a device (a real / virtio NIC), as opposed to veth, bridges, loopback.
    pub physical: bool,
    pub mac: Option<String>,
}

fn class_net(root: &Path) -> PathBuf {
    root.join("sys").join("class").join("net")
}

fn iface_names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default()
}

/// Host network data is used only when it differs from what the container itself sees,
/// i.e. the agent really is in its own network namespace. With host networking (or no
/// host-root mount) this returns `None` and the normal sysinfo path is used.
pub fn detect(host_root: Option<&str>) -> Option<PathBuf> {
    let root = host_root.filter(|r| !r.is_empty() && *r != "/")?;
    let host_dir = class_net(Path::new(root));
    let host = iface_names(&host_dir);
    if host.is_empty() {
        return None;
    }
    let own = iface_names(Path::new("/sys/class/net"));
    (host != own).then_some(host_dir)
}

fn read_u64(p: &Path) -> Option<u64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

pub fn read_ifaces(dir: &Path) -> Vec<HostIface> {
    let mut out: Vec<HostIface> = iface_names(dir)
        .into_iter()
        .filter_map(|name| {
            let d = dir.join(&name);
            let rx = read_u64(&d.join("statistics").join("rx_bytes"))?;
            let tx = read_u64(&d.join("statistics").join("tx_bytes"))?;
            let mac = std::fs::read_to_string(d.join("address")).ok().map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
            Some(HostIface { physical: d.join("device").exists(), name, rx, tx, mac })
        })
        .collect();
    // Some virtual machines expose no `device` links at all; fall back to name heuristics there.
    if !out.iter().any(|i| i.physical) {
        for i in out.iter_mut() {
            i.physical = !crate::system::is_virtual_iface(&i.name);
        }
    }
    out
}

/// Local IPv4 addresses of the host from `/proc/1/net/fib_trie` (PID 1 = host init when the
/// container shares the host PID namespace). Best effort: empty if unreadable.
pub fn host_ipv4() -> Vec<String> {
    std::fs::read_to_string("/proc/1/net/fib_trie").map(|t| parse_fib_local(&t)).unwrap_or_default()
}

pub fn parse_fib_local(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<String> = None;
    for line in text.lines() {
        let t = line.trim_start_matches([' ', '|', '-', '+']).trim();
        if line.contains("/32 host LOCAL") {
            if let Some(ip) = last.take() {
                if !ip.starts_with("127.") && !is_docker_gateway(&ip) && !out.contains(&ip) {
                    out.push(ip);
                }
            }
        } else if t.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) && !t.contains('/') {
            last = Some(t.split_whitespace().next().unwrap_or("").to_string());
        }
    }
    out
}

/// Docker bridge gateways (172.16.0.0/12, x.x.x.1) are noise when listing a host's addresses.
fn is_docker_gateway(ip: &str) -> bool {
    let p: Vec<u8> = ip.split('.').filter_map(|x| x.parse().ok()).collect();
    p.len() == 4 && p[0] == 172 && (16..=31).contains(&p[1]) && p[3] == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fib_trie_local_addresses() {
        let t = "Main:\n  +-- 0.0.0.0/0 3 0 5\n     |-- 0.0.0.0\n        /0 universe UNICAST\n     +-- 10.128.0.0/24 2 0 2\n        |-- 10.128.0.7\n           /32 host LOCAL\n     +-- 127.0.0.0/8 2 0 2\n        |-- 127.0.0.1\n           /32 host LOCAL\n     +-- 172.17.0.0/16 2 0 2\n        |-- 172.17.0.1\n           /32 host LOCAL\nLocal:\n        |-- 10.128.0.7\n           /32 host LOCAL\n";
        assert_eq!(parse_fib_local(t), vec!["10.128.0.7"]);
    }

    #[test]
    fn sysfs_ifaces() {
        let dir = std::env::temp_dir().join(format!("ml-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, rx, tx, dev) in [("ens4", 1000u64, 500u64, true), ("veth1a", 7, 9, false), ("docker0", 3, 4, false)] {
            let d = dir.join(name).join("statistics");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("rx_bytes"), format!("{rx}\n")).unwrap();
            std::fs::write(d.join("tx_bytes"), format!("{tx}\n")).unwrap();
            std::fs::write(dir.join(name).join("address"), "42:01:0a:80:00:07\n").unwrap();
            if dev {
                std::fs::create_dir_all(dir.join(name).join("device")).unwrap();
            }
        }
        let v = read_ifaces(&dir);
        let phys: Vec<_> = v.iter().filter(|i| i.physical).map(|i| i.name.as_str()).collect();
        assert_eq!(phys, vec!["ens4"]);
        let ens4 = v.iter().find(|i| i.name == "ens4").unwrap();
        assert_eq!((ens4.rx, ens4.tx), (1000, 500));
        assert_eq!(ens4.mac.as_deref(), Some("42:01:0a:80:00:07"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
