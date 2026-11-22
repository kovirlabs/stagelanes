//! macOS: `sysctl` performance levels. User code cannot pin threads or ask
//! which CPU it runs on, and the OS reports no CPU numbers, so CPUs are
//! numbered here: performance level 0 (the fastest) first.
//!
//! Keys read: `hw.packages`, `hw.nperflevels`, and per level `N`
//! `hw.perflevelN.{physicalcpu,logicalcpu,l1dcachesize,l1icachesize,
//! l2cachesize,cpusperl2,l3cachesize,cpusperl3}`, plus `hw.cachelinesize`.
//! Apple silicon reports no maximum frequency through sysctl.
//!
//! Not yet run on a Mac: checked by agent C in M2.

use std::ffi::CString;

use crate::assemble::{RawCache, RawCpu, RawTopology};
use crate::{CacheKind, Error, OsCpu, Priority};

pub(crate) const SOURCE: &str = "macOS sysctl hw.perflevel*";

fn sysctl_u64(name: &str) -> Option<u64> {
    let cname = CString::new(name).ok()?;
    let mut buf = [0u8; 8];
    let mut len: libc::size_t = buf.len();
    // SAFETY: `cname` is NUL-terminated; `buf` is writable for `len` bytes
    // and the call writes at most `len` bytes and updates `len`; no new
    // value is set (null, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            core::ptr::null_mut(),
            0,
        )
    };
    match (rc, len) {
        (0, 4) => Some(u64::from(u32::from_ne_bytes([
            buf[0], buf[1], buf[2], buf[3],
        ]))),
        (0, 8) => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

pub(crate) fn detect() -> Result<RawTopology, Error> {
    let levels = sysctl_u64("hw.nperflevels").unwrap_or(1).max(1);
    let line = sysctl_u64("hw.cachelinesize").map(|v| v as u32);
    let packages = sysctl_u64("hw.packages").unwrap_or(1);
    if packages > 1 {
        return Err(Error::Unsupported("more than one package on macOS"));
    }
    let mut raw = RawTopology::default();
    let mut next_cpu = 0u32;
    let mut next_core = 0u64;
    for level in 0..levels {
        // Intel Macs have no perflevel keys; fall back to the same names
        // under hw.* there.
        let key = |k: &str| {
            sysctl_u64(&format!("hw.perflevel{level}.{k}")).or_else(|| {
                (levels == 1)
                    .then(|| sysctl_u64(&format!("hw.{k}")))
                    .flatten()
            })
        };
        let physical = key("physicalcpu").ok_or_else(|| Error::Parse("no physicalcpu".into()))?;
        let logical = key("logicalcpu").unwrap_or(physical);
        let threads = (logical / physical.max(1)).max(1);
        // Level 0 is the fastest; make it the highest class.
        let class = (levels - 1 - level) as u8;

        let first_cpu = next_cpu;
        let mut core_cpus: Vec<Vec<OsCpu>> = Vec::new();
        for _ in 0..physical {
            let mut cpus = Vec::new();
            for _ in 0..threads {
                let os = OsCpu {
                    group: 0,
                    number: next_cpu,
                };
                next_cpu += 1;
                raw.cpus.push(RawCpu {
                    os,
                    core_key: next_core,
                    package_key: 0,
                    numa_node: None,
                    class,
                    max_mhz: None,
                });
                cpus.push(os);
            }
            next_core += 1;
            core_cpus.push(cpus);
        }

        for (k, kind) in [
            ("l1dcachesize", CacheKind::Data),
            ("l1icachesize", CacheKind::Instruction),
        ] {
            if let Some(size) = key(k) {
                for cpus in &core_cpus {
                    raw.caches.push(RawCache {
                        level: 1,
                        kind,
                        size_bytes: size,
                        line_bytes: line,
                        cpus: cpus.clone(),
                    });
                }
            }
        }
        for (lvl, size_key, share_key) in [
            (2u8, "l2cachesize", "cpusperl2"),
            (3, "l3cachesize", "cpusperl3"),
        ] {
            let Some(size) = key(size_key) else { continue };
            let per = key(share_key).unwrap_or(logical).max(1) as u32;
            let level_cpus: Vec<OsCpu> = (first_cpu..next_cpu)
                .map(|number| OsCpu { group: 0, number })
                .collect();
            for chunk in level_cpus.chunks(per as usize) {
                raw.caches.push(RawCache {
                    level: lvl,
                    kind: CacheKind::Unified,
                    size_bytes: size,
                    line_bytes: line,
                    cpus: chunk.to_vec(),
                });
            }
        }
    }
    Ok(raw)
}

pub(crate) fn pin_current_thread(_cpu: OsCpu) -> Result<(), Error> {
    Err(Error::Unsupported(
        "thread pinning (macOS has no CPU affinity API)",
    ))
}

pub(crate) fn current_cpu() -> Option<OsCpu> {
    None
}

pub(crate) fn set_current_thread_priority(priority: Priority) -> Result<(), Error> {
    match priority {
        Priority::Normal => Ok(()),
        _ => Err(Error::Unsupported("raising thread priority")),
    }
}
