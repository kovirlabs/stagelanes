//! Turns the per-OS raw records into a [`Topology`].

use std::collections::BTreeMap;

use crate::{Cache, CacheKind, Core, Error, LogicalCpu, OsCpu, Topology};

/// One logical CPU as a platform module reports it.
#[derive(Clone, Debug)]
pub(crate) struct RawCpu {
    pub os: OsCpu,
    /// Any value equal for all threads of one core and distinct otherwise.
    pub core_key: u64,
    /// Any value equal for all cores of one package and distinct otherwise.
    pub package_key: u64,
    pub numa_node: Option<u32>,
    /// Higher is faster.
    pub class: u8,
    pub max_mhz: Option<u32>,
}

/// One cache instance as a platform module reports it.
#[derive(Clone, Debug)]
pub(crate) struct RawCache {
    pub level: u8,
    pub kind: CacheKind,
    pub size_bytes: u64,
    pub line_bytes: Option<u32>,
    pub cpus: Vec<OsCpu>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RawTopology {
    pub cpus: Vec<RawCpu>,
    pub caches: Vec<RawCache>,
}

pub(crate) fn assemble(raw: RawTopology, source: &'static str) -> Result<Topology, Error> {
    let mut raw_cpus = raw.cpus;
    if raw_cpus.is_empty() {
        return Err(Error::Parse("no CPUs reported".into()));
    }
    raw_cpus.sort_by_key(|c| c.os);
    if raw_cpus.windows(2).any(|w| w[0].os == w[1].os) {
        return Err(Error::Parse("a CPU is reported twice".into()));
    }

    // Sockets and cores are numbered in order of their first CPU.
    let mut socket_of_key: BTreeMap<u64, usize> = BTreeMap::new();
    let mut core_of_key: BTreeMap<(u64, u64), usize> = BTreeMap::new();
    let mut cores: Vec<Core> = Vec::new();
    let mut cpus: Vec<LogicalCpu> = Vec::with_capacity(raw_cpus.len());
    for (index, rc) in raw_cpus.iter().enumerate() {
        let next_socket = socket_of_key.len();
        let socket = *socket_of_key.entry(rc.package_key).or_insert(next_socket);
        let key = (rc.package_key, rc.core_key);
        let core = match core_of_key.get(&key) {
            Some(&core) => {
                let c = &mut cores[core];
                if c.class != rc.class || c.numa_node != rc.numa_node {
                    return Err(Error::Parse(format!(
                        "threads of core {core} disagree on class or NUMA node"
                    )));
                }
                c.os_max_mhz = c.os_max_mhz.max(rc.max_mhz);
                c.cpus.push(index);
                core
            }
            None => {
                let core = cores.len();
                core_of_key.insert(key, core);
                cores.push(Core {
                    index: core,
                    socket,
                    numa_node: rc.numa_node,
                    class: rc.class,
                    cpus: vec![index],
                    os_max_mhz: rc.max_mhz,
                });
                core
            }
        };
        cpus.push(LogicalCpu {
            index,
            os: rc.os,
            core,
        });
    }

    let index_of = |os: &OsCpu| raw_cpus.binary_search_by_key(os, |c| c.os).ok();
    let mut caches: Vec<Cache> = Vec::with_capacity(raw.caches.len());
    for rc in raw.caches {
        let mut members: Vec<usize> = rc.cpus.iter().filter_map(index_of).collect();
        members.sort_unstable();
        members.dedup();
        if members.is_empty() {
            // A cache of CPUs outside this process's view; skip it.
            continue;
        }
        let cache = Cache {
            level: rc.level,
            kind: rc.kind,
            size_bytes: rc.size_bytes,
            line_bytes: rc.line_bytes,
            cpus: members,
        };
        if !caches.contains(&cache) {
            caches.push(cache);
        }
    }
    caches.sort_by(|a, b| (a.level, a.kind, &a.cpus).cmp(&(b.level, b.kind, &b.cpus)));

    Ok(Topology {
        cpus,
        cores,
        caches,
        source,
    })
}

/// Ranks distinct values ascending: the smallest gets class 0.
pub(crate) fn rank_classes<K: Ord + Copy>(values: &[K]) -> Vec<u8> {
    let mut distinct: Vec<K> = values.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    values
        .iter()
        .map(|v| distinct.binary_search(v).unwrap_or(0).min(u8::MAX as usize) as u8)
        .collect()
}
