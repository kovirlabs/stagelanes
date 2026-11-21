//! Linux topology from sysfs. Reads files under a root directory so tests can
//! point it at a fixture tree; on a live system the root is
//! `/sys/devices/system`.
//!
//! Files read, per online CPU `N` under `<root>/cpu/cpuN/`:
//! `topology/physical_package_id`, `topology/core_cpus_list` (or the older
//! `thread_siblings_list`), `cache/index*/{level,type,size,
//! coherency_line_size,shared_cpu_list}`, `cpufreq/cpuinfo_max_freq` (kHz),
//! `cpu_capacity` (Arm big.LITTLE). Also `<root>/cpu/online`,
//! `<root>/node/node*/cpulist`, and for Intel hybrid CPUs
//! `<root>/../cpu_core/cpus` and `<root>/../cpu_atom/cpus`.

use std::fs;
use std::path::Path;

use crate::assemble::{RawCache, RawCpu, RawTopology, rank_classes};
use crate::{CacheKind, Error, OsCpu};

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

/// Parses a kernel CPU list such as `0-3,8,10-11`.
pub(crate) fn parse_cpu_list(s: &str) -> Result<Vec<u32>, Error> {
    let bad = || Error::Parse(format!("bad CPU list {s:?}"));
    let mut cpus = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                let a: u32 = a.parse().map_err(|_| bad())?;
                let b: u32 = b.parse().map_err(|_| bad())?;
                if b < a {
                    return Err(bad());
                }
                cpus.extend(a..=b);
            }
            None => cpus.push(part.parse().map_err(|_| bad())?),
        }
    }
    Ok(cpus)
}

/// Parses a cache size such as `32K`, `1024K` or `16M`.
pub(crate) fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (digits, mult) = match s.as_bytes().last()? {
        b'K' => (&s[..s.len() - 1], 1u64 << 10),
        b'M' => (&s[..s.len() - 1], 1 << 20),
        b'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    digits.parse::<u64>().ok().map(|n| n * mult)
}

fn cpu_list_file(path: &Path) -> Option<Vec<u32>> {
    read_trimmed(path).and_then(|s| parse_cpu_list(&s).ok())
}

pub(crate) fn read(root: &Path) -> Result<RawTopology, Error> {
    let cpu_dir = root.join("cpu");
    let online = read_trimmed(&cpu_dir.join("online"))
        .ok_or_else(|| Error::Parse(format!("cannot read {}", cpu_dir.join("online").display())))?;
    let online = parse_cpu_list(&online)?;

    // NUMA node of each CPU.
    let mut node_of: Vec<(u32, u32)> = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("node")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(node) = name
                .to_str()
                .and_then(|n| n.strip_prefix("node"))
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            for cpu in cpu_list_file(&entry.path().join("cpulist")).unwrap_or_default() {
                node_of.push((cpu, node));
            }
        }
    }

    // Intel hybrid: P-cores are listed under cpu_core, E-cores under cpu_atom.
    let devices = root.parent().unwrap_or(root);
    let intel_p = cpu_list_file(&devices.join("cpu_core").join("cpus"));
    let intel_e = cpu_list_file(&devices.join("cpu_atom").join("cpus"));

    let mut raw = RawTopology::default();
    let mut capacities: Vec<u64> = Vec::new();
    let mut have_capacity = true;
    for &n in &online {
        let dir = cpu_dir.join(format!("cpu{n}"));
        let topo = dir.join("topology");
        let package: i64 = read_trimmed(&topo.join("physical_package_id"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // The lowest CPU sharing this core identifies the core.
        let siblings = cpu_list_file(&topo.join("core_cpus_list"))
            .or_else(|| cpu_list_file(&topo.join("thread_siblings_list")))
            .unwrap_or_else(|| vec![n]);
        let core_key = u64::from(siblings.iter().copied().min().unwrap_or(n));
        let max_mhz = read_trimmed(&dir.join("cpufreq").join("cpuinfo_max_freq"))
            .and_then(|s| s.parse::<u64>().ok())
            .map(|khz| (khz / 1000) as u32);
        match read_trimmed(&dir.join("cpu_capacity")).and_then(|s| s.parse().ok()) {
            Some(c) => capacities.push(c),
            None => have_capacity = false,
        }
        let class = match (&intel_p, &intel_e) {
            (Some(p), Some(_)) if p.contains(&n) => 1,
            _ => 0,
        };
        raw.cpus.push(RawCpu {
            os: OsCpu {
                group: 0,
                number: n,
            },
            core_key,
            package_key: package.max(0) as u64,
            numa_node: node_of.iter().find(|(c, _)| *c == n).map(|&(_, node)| node),
            class,
            max_mhz,
        });

        let Ok(entries) = fs::read_dir(dir.join("cache")) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_index = entry
                .file_name()
                .to_str()
                .is_some_and(|s| s.starts_with("index"));
            if !is_index {
                continue;
            }
            let p = entry.path();
            let level = read_trimmed(&p.join("level")).and_then(|s| s.parse().ok());
            let kind = match read_trimmed(&p.join("type")).as_deref() {
                Some("Data") => Some(CacheKind::Data),
                Some("Instruction") => Some(CacheKind::Instruction),
                Some("Unified") => Some(CacheKind::Unified),
                _ => None,
            };
            let size = read_trimmed(&p.join("size")).and_then(|s| parse_size(&s));
            let shared = cpu_list_file(&p.join("shared_cpu_list"));
            let (Some(level), Some(kind), Some(size), Some(shared)) = (level, kind, size, shared)
            else {
                continue;
            };
            raw.caches.push(RawCache {
                level,
                kind,
                size_bytes: size,
                line_bytes: read_trimmed(&p.join("coherency_line_size"))
                    .and_then(|s| s.parse().ok()),
                cpus: shared
                    .into_iter()
                    .map(|number| OsCpu { group: 0, number })
                    .collect(),
            });
        }
    }

    // Arm big.LITTLE: rank the distinct capacities.
    if have_capacity && !capacities.is_empty() {
        for (cpu, class) in raw.cpus.iter_mut().zip(rank_classes(&capacities)) {
            cpu.class = class;
        }
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn cpu_lists() {
        assert_eq!(
            parse_cpu_list("0-3,8,10-11\n").unwrap(),
            [0, 1, 2, 3, 8, 10, 11]
        );
        assert_eq!(parse_cpu_list("5").unwrap(), [5]);
        assert_eq!(parse_cpu_list("").unwrap(), [] as [u32; 0]);
        assert!(parse_cpu_list("3-1").is_err());
        assert!(parse_cpu_list("x").is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("32K"), Some(32 << 10));
        assert_eq!(parse_size("16M"), Some(16 << 20));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size(""), None);
    }

    /// A scratch directory removed on drop.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("stagelanes-arch-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Tree(dir)
        }

        fn file(&self, rel: &str, contents: &str) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, contents).unwrap();
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Two sockets, two cores each, two threads per core, one NUMA node per
    /// socket: the shape of the Dell guest in small.
    fn two_socket_tree() -> Tree {
        let t = Tree::new("two-socket");
        let s = "system";
        t.file(&format!("{s}/cpu/online"), "0-7\n");
        // Linux numbers the second threads after all first threads.
        for cpu in 0..8u32 {
            let socket = (cpu / 2) % 2;
            let core = cpu % 2 + socket * 2; // global core number
            let first = core % 2 + socket * 2;
            let siblings = format!("{},{}", first, first + 4);
            let d = format!("{s}/cpu/cpu{cpu}");
            t.file(
                &format!("{d}/topology/physical_package_id"),
                &socket.to_string(),
            );
            t.file(&format!("{d}/topology/core_cpus_list"), &siblings);
            t.file(&format!("{d}/cpufreq/cpuinfo_max_freq"), "3200000\n");
            let caches = [
                ("index0", "1", "Data", "32K", siblings.clone()),
                ("index1", "1", "Instruction", "32K", siblings.clone()),
                ("index2", "2", "Unified", "1024K", siblings.clone()),
                (
                    "index3",
                    "3",
                    "Unified",
                    "16896K",
                    if socket == 0 { "0-1,4-5" } else { "2-3,6-7" }.to_owned(),
                ),
            ];
            for (idx, level, ty, size, shared) in caches {
                let c = format!("{d}/cache/{idx}");
                t.file(&format!("{c}/level"), level);
                t.file(&format!("{c}/type"), ty);
                t.file(&format!("{c}/size"), size);
                t.file(&format!("{c}/coherency_line_size"), "64");
                t.file(&format!("{c}/shared_cpu_list"), &shared);
            }
        }
        t.file(&format!("{s}/node/node0/cpulist"), "0-1,4-5\n");
        t.file(&format!("{s}/node/node1/cpulist"), "2-3,6-7\n");
        t
    }

    #[test]
    fn two_sockets_two_nodes() {
        let t = two_socket_tree();
        let raw = read(&t.0.join("system")).unwrap();
        let topo = crate::assemble::assemble(raw, "test").unwrap();
        assert_eq!(topo.cpus.len(), 8);
        assert_eq!(topo.cores.len(), 4);
        assert_eq!(topo.sockets(), 2);
        assert_eq!(topo.numa_nodes(), [0, 1]);
        assert_eq!(topo.classes(), [0]);
        for core in &topo.cores {
            assert_eq!(core.cpus.len(), 2);
            assert_eq!(core.os_max_mhz, Some(3200));
            assert_eq!(core.numa_node, Some(core.socket as u32));
            let l3: Vec<_> = topo
                .core_caches(core.index)
                .filter(|c| c.level == 3)
                .collect();
            assert_eq!(l3.len(), 1);
            assert_eq!(l3[0].cpus.len(), 4);
        }
        // 4 cores x (L1d, L1i, L2) + 2 L3.
        assert_eq!(topo.caches.len(), 14);
        // CPU 0 and CPU 4 are one core.
        assert_eq!(topo.cpus[0].core, topo.cpus[4].core);
    }

    #[test]
    fn arm_capacity_and_intel_hybrid_classes() {
        let t = Tree::new("hybrid");
        t.file("system/cpu/online", "0-3");
        for cpu in 0..4 {
            t.file(
                &format!("system/cpu/cpu{cpu}/topology/physical_package_id"),
                "0",
            );
            let cap = if cpu < 2 { "512" } else { "1024" };
            t.file(&format!("system/cpu/cpu{cpu}/cpu_capacity"), cap);
        }
        let raw = read(&t.0.join("system")).unwrap();
        let classes: Vec<u8> = raw.cpus.iter().map(|c| c.class).collect();
        assert_eq!(classes, [0, 0, 1, 1]);

        let t = Tree::new("intel-hybrid");
        t.file("system/cpu/online", "0-2");
        for cpu in 0..3 {
            t.file(
                &format!("system/cpu/cpu{cpu}/topology/physical_package_id"),
                "0",
            );
        }
        t.file("cpu_core/cpus", "0-1");
        t.file("cpu_atom/cpus", "2");
        let raw = read(&t.0.join("system")).unwrap();
        let classes: Vec<u8> = raw.cpus.iter().map(|c| c.class).collect();
        assert_eq!(classes, [1, 1, 0]);
    }

    #[test]
    fn missing_root_is_an_error() {
        assert!(read(Path::new("/nonexistent/stagelanes")).is_err());
    }
}
