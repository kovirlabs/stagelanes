//! Parser for the buffer `GetLogicalProcessorInformationEx(RelationAll)`
//! fills: a packed list of variable-size `SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX`
//! records. Reads the bytes at their documented offsets, so it needs no
//! `unsafe` and runs on any OS.
//!
//! Record header: `Relationship: u32` at 0, `Size: u32` at 4, body at 8.
//! Body layouts (offsets from the body start):
//!
//! - `PROCESSOR_RELATIONSHIP` (core, package): `Flags: u8` 0,
//!   `EfficiencyClass: u8` 1, `GroupCount: u16` 22, `GroupMask[]` 24.
//! - `NUMA_NODE_RELATIONSHIP`: `NodeNumber: u32` 0, `GroupCount: u16` 22,
//!   `GroupMask[]` 24.
//! - `CACHE_RELATIONSHIP`: `Level: u8` 0, `LineSize: u16` 2,
//!   `CacheSize: u32` 4, `Type: u32` 8, `GroupCount: u16` 30,
//!   `GroupMask[]` 32.
//! - `GROUP_AFFINITY` (16 bytes on 64-bit): `Mask: usize` 0, `Group: u16` 8.
//!
//! A `GroupCount` of 0 comes from Windows versions before it existed and
//! means one mask.

use crate::assemble::{RawCache, RawCpu, RawTopology};
use crate::{CacheKind, Error, OsCpu};

const RELATION_PROCESSOR_CORE: u32 = 0;
const RELATION_NUMA_NODE: u32 = 1;
const RELATION_CACHE: u32 = 2;
const RELATION_PROCESSOR_PACKAGE: u32 = 3;

/// Size of `GROUP_AFFINITY` on 64-bit Windows.
const GROUP_AFFINITY_SIZE: usize = 16;

fn read_u8(b: &[u8], at: usize) -> Result<u8, Error> {
    b.get(at).copied().ok_or_else(short)
}

fn read_u16(b: &[u8], at: usize) -> Result<u16, Error> {
    let s = b.get(at..at + 2).ok_or_else(short)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32(b: &[u8], at: usize) -> Result<u32, Error> {
    let s = b.get(at..at + 4).ok_or_else(short)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64(b: &[u8], at: usize) -> Result<u64, Error> {
    let s = b.get(at..at + 8).ok_or_else(short)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

fn short() -> Error {
    Error::Parse("processor information record is truncated".into())
}

/// Expands the `GroupMask` array at `at` into CPUs.
fn masks(body: &[u8], count_at: usize, at: usize) -> Result<Vec<OsCpu>, Error> {
    let count = read_u16(body, count_at)?.max(1) as usize;
    let mut cpus = Vec::new();
    for i in 0..count {
        let base = at + i * GROUP_AFFINITY_SIZE;
        let mask = read_u64(body, base)?;
        let group = read_u16(body, base + 8)?;
        for bit in 0..64 {
            if mask & (1 << bit) != 0 {
                cpus.push(OsCpu { group, number: bit });
            }
        }
    }
    Ok(cpus)
}

/// One parsed record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Record {
    Core {
        efficiency_class: u8,
        cpus: Vec<OsCpu>,
    },
    Package {
        cpus: Vec<OsCpu>,
    },
    Numa {
        node: u32,
        cpus: Vec<OsCpu>,
    },
    Cache {
        level: u8,
        line: u16,
        size: u32,
        kind: Option<CacheKind>,
        cpus: Vec<OsCpu>,
    },
}

/// Splits the buffer into records, skipping relationships not used here.
pub(crate) fn records(buf: &[u8]) -> Result<Vec<Record>, Error> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < buf.len() {
        let relationship = read_u32(buf, at)?;
        let size = read_u32(buf, at + 4)? as usize;
        if size < 8 || at + size > buf.len() {
            return Err(Error::Parse(format!("bad record size {size}")));
        }
        let body = &buf[at + 8..at + size];
        match relationship {
            RELATION_PROCESSOR_CORE => out.push(Record::Core {
                efficiency_class: read_u8(body, 1)?,
                cpus: masks(body, 22, 24)?,
            }),
            RELATION_PROCESSOR_PACKAGE => out.push(Record::Package {
                cpus: masks(body, 22, 24)?,
            }),
            RELATION_NUMA_NODE => out.push(Record::Numa {
                node: read_u32(body, 0)?,
                cpus: masks(body, 22, 24)?,
            }),
            RELATION_CACHE => out.push(Record::Cache {
                level: read_u8(body, 0)?,
                line: read_u16(body, 2)?,
                size: read_u32(body, 4)?,
                kind: match read_u32(body, 8)? {
                    0 => Some(CacheKind::Unified),
                    1 => Some(CacheKind::Instruction),
                    2 => Some(CacheKind::Data),
                    _ => None, // trace cache
                },
                cpus: masks(body, 30, 32)?,
            }),
            _ => {}
        }
        at += size;
    }
    Ok(out)
}

/// Builds raw topology records. `max_mhz` maps a CPU to its OS-reported
/// maximum frequency.
pub(crate) fn to_raw(
    records: &[Record],
    max_mhz: impl Fn(OsCpu) -> Option<u32>,
) -> Result<RawTopology, Error> {
    let package_of = |cpu: OsCpu| {
        records
            .iter()
            .filter_map(|r| match r {
                Record::Package { cpus } => Some(cpus),
                _ => None,
            })
            .position(|cpus| cpus.contains(&cpu))
    };
    let node_of = |cpu: OsCpu| {
        records.iter().find_map(|r| match r {
            Record::Numa { node, cpus } if cpus.contains(&cpu) => Some(*node),
            _ => None,
        })
    };

    let mut raw = RawTopology::default();
    let mut core_index = 0u64;
    for r in records {
        let Record::Core {
            efficiency_class,
            cpus,
        } = r
        else {
            continue;
        };
        for &cpu in cpus {
            let package = package_of(cpu)
                .ok_or_else(|| Error::Parse(format!("CPU {cpu} is in no package")))?;
            raw.cpus.push(RawCpu {
                os: cpu,
                core_key: core_index,
                package_key: package as u64,
                numa_node: node_of(cpu),
                class: *efficiency_class,
                max_mhz: max_mhz(cpu),
            });
        }
        core_index += 1;
    }
    for r in records {
        if let Record::Cache {
            level,
            line,
            size,
            kind: Some(kind),
            cpus,
        } = r
        {
            raw.caches.push(RawCache {
                level: *level,
                kind: *kind,
                size_bytes: u64::from(*size),
                line_bytes: (*line > 0).then_some(u32::from(*line)),
                cpus: cpus.clone(),
            });
        }
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(rel: u32, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&rel.to_le_bytes());
        v.extend_from_slice(&((8 + body.len()) as u32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    fn affinity(group: u16, mask: u64) -> Vec<u8> {
        let mut v = mask.to_le_bytes().to_vec();
        v.extend_from_slice(&group.to_le_bytes());
        v.extend_from_slice(&[0; 6]);
        v
    }

    fn processor(rel: u32, class: u8, groups: &[(u16, u64)]) -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[0] = u8::from(rel == RELATION_PROCESSOR_CORE); // LTP_PC_SMT
        b[1] = class;
        b[22..24].copy_from_slice(&(groups.len() as u16).to_le_bytes());
        for &(g, m) in groups {
            b.extend(affinity(g, m));
        }
        header(rel, &b)
    }

    fn numa(node: u32, group: u16, mask: u64) -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[0..4].copy_from_slice(&node.to_le_bytes());
        b[22..24].copy_from_slice(&1u16.to_le_bytes());
        b.extend(affinity(group, mask));
        header(RELATION_NUMA_NODE, &b)
    }

    fn cache(level: u8, kind: u32, size: u32, group: u16, mask: u64) -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0] = level;
        b[1] = 8;
        b[2..4].copy_from_slice(&64u16.to_le_bytes());
        b[4..8].copy_from_slice(&size.to_le_bytes());
        b[8..12].copy_from_slice(&kind.to_le_bytes());
        b[30..32].copy_from_slice(&1u16.to_le_bytes());
        b.extend(affinity(group, mask));
        header(RELATION_CACHE, &b)
    }

    /// Two hybrid cores (one SMT P-core, one E-core) on a package that spans
    /// two processor groups, plus caches.
    #[test]
    fn parses_cores_packages_nodes_caches() {
        let mut buf = Vec::new();
        buf.extend(processor(RELATION_PROCESSOR_CORE, 1, &[(0, 0b11)]));
        buf.extend(processor(RELATION_PROCESSOR_CORE, 0, &[(1, 0b1)]));
        buf.extend(processor(
            RELATION_PROCESSOR_PACKAGE,
            0,
            &[(0, 0b11), (1, 0b1)],
        ));
        buf.extend(numa(0, 0, 0b11));
        buf.extend(numa(1, 1, 0b1));
        buf.extend(cache(1, 2, 48 << 10, 0, 0b11));
        buf.extend(cache(3, 0, 32 << 20, 0, 0b11));
        buf.extend(cache(2, 3, 1, 0, 0b11)); // trace cache, ignored
        buf.extend(header(4, &[0; 16])); // RelationGroup, skipped

        let recs = records(&buf).unwrap();
        assert_eq!(recs.len(), 8);
        let raw = to_raw(&recs, |c| Some(3000 + c.number)).unwrap();
        assert_eq!(raw.cpus.len(), 3);
        let e = &raw.cpus[2];
        assert_eq!(
            e.os,
            OsCpu {
                group: 1,
                number: 0
            }
        );
        assert_eq!((e.class, e.numa_node, e.package_key), (0, Some(1), 0));
        assert_eq!(raw.cpus[0].core_key, raw.cpus[1].core_key);
        assert_eq!(raw.caches.len(), 2);
        assert_eq!(raw.caches[1].size_bytes, 32 << 20);
        assert_eq!(raw.caches[0].line_bytes, Some(64));

        let topo = crate::assemble::assemble(raw, "test").unwrap();
        assert_eq!(topo.cores.len(), 2);
        assert_eq!(topo.classes(), [0, 1]);
        assert_eq!(topo.numa_nodes(), [0, 1]);
        assert_eq!(topo.sockets(), 1);
        assert_eq!(topo.cores[0].os_max_mhz, Some(3001));
    }

    #[test]
    fn rejects_truncated_records() {
        let mut buf = processor(RELATION_PROCESSOR_CORE, 0, &[(0, 1)]);
        buf.truncate(buf.len() - 4);
        assert!(records(&buf).is_err());
    }
}
