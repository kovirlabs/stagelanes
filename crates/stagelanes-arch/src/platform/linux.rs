//! Linux: sysfs for topology, `sched_setaffinity` and `sched_getcpu`.

use std::path::Path;

use super::sysfs;
use crate::assemble::RawTopology;
use crate::{Error, OsCpu};

pub(crate) const SOURCE: &str =
    "Linux /sys/devices/system; frequency from cpufreq cpuinfo_max_freq";

pub(crate) fn detect() -> Result<RawTopology, Error> {
    sysfs::read(Path::new("/sys/devices/system"))
}

pub(crate) fn pin_current_thread(cpu: OsCpu) -> Result<(), Error> {
    let max = 8 * core::mem::size_of::<libc::cpu_set_t>() as u32;
    if cpu.group != 0 || cpu.number >= max {
        return Err(Error::Parse(format!("CPU {cpu} out of range")));
    }
    // SAFETY: `cpu_set_t` is plain data for which all-zero is the empty set.
    let mut set: libc::cpu_set_t = unsafe { core::mem::zeroed() };
    // SAFETY: `cpu.number` was checked to be below the set's capacity.
    unsafe { libc::CPU_SET(cpu.number as usize, &mut set) };
    // SAFETY: pid 0 is the calling thread; `set` is a valid, initialised
    // cpu_set_t of the size passed.
    let rc = unsafe { libc::sched_setaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &set) };
    if rc != 0 {
        return Err(Error::Os {
            call: "sched_setaffinity",
            code: i64::from(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)),
        });
    }
    Ok(())
}

pub(crate) fn current_cpu() -> Option<OsCpu> {
    // SAFETY: no arguments, no preconditions.
    let n = unsafe { libc::sched_getcpu() };
    u32::try_from(n)
        .ok()
        .map(|number| OsCpu { group: 0, number })
}
