//! Disk I/O counters straight from `/proc/diskstats` (Linux).
//!
//! `/proc/diskstats` is not namespaced, so this works the same for an agent on the host
//! and for the hub watching its own server from inside a container. Only whole physical
//! disks are counted (those with a `device` link under `/sys/block`), so partitions,
//! LVM/device-mapper volumes, md arrays, loop and zram devices don't double-count I/O.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const SECTOR: u64 = 512; // diskstats always counts 512-byte sectors

/// `(device, bytes_read, bytes_written)` per physical disk, or `None` if unreadable.
pub fn read(host_root: Option<&str>) -> Option<Vec<(String, u64, u64)>> {
    let text = std::fs::read_to_string("/proc/diskstats").ok()?;
    let physical = physical_disks(host_root);
    if physical.is_empty() {
        return None;
    }
    Some(parse(&text, &physical))
}

fn sys_block(host_root: Option<&str>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(r) = host_root.filter(|r| !r.is_empty() && *r != "/") {
        v.push(Path::new(r).join("sys/block"));
    }
    v.push(PathBuf::from("/sys/block"));
    v
}

fn physical_disks(host_root: Option<&str>) -> HashSet<String> {
    for dir in sys_block(host_root) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let set: HashSet<String> = rd
            .flatten()
            .filter(|e| e.path().join("device").exists())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        if !set.is_empty() {
            return set;
        }
    }
    HashSet::new()
}

pub fn parse(text: &str, physical: &HashSet<String>) -> Vec<(String, u64, u64)> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 || !physical.contains(f[2]) {
                return None;
            }
            let r: u64 = f[5].parse().ok()?;
            let w: u64 = f[9].parse().ok()?;
            Some((f[2].to_string(), r * SECTOR, w * SECTOR))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_whole_disks_only() {
        let text = "\
 252       0 vda 1000 10 2000 50 3000 20 4000 60 0 100 200
 252       1 vda1 10 0 20 1 30 0 40 1 0 2 3
 253       0 dm-0 900 0 1800 40 2900 0 3900 50 0 90 100
   7       0 loop0 5 0 10 0 0 0 0 0 0 0 0
";
        let phys: HashSet<String> = ["vda".to_string()].into_iter().collect();
        let v = parse(text, &phys);
        assert_eq!(v, vec![("vda".to_string(), 2000 * 512, 4000 * 512)]);
    }
}
