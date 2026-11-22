//! stagelanes-arch: topology detection and CPU specifics.
//!
//! - [`Topology::detect`] reads each logical CPU's core, socket, NUMA node,
//!   core class, caches and OS-reported maximum frequency.
//! - [`measure_core_mhz`] measures the clock a core actually reaches under a
//!   single-threaded load, for machines where the OS reports only the base
//!   frequency.
//! - [`pin_current_thread`] and [`current_cpu`] pin and locate threads.
//! - [`prefetch_read`] and [`ticks`] are the hot-path helpers.
//!
//! Sources by OS: Windows `GetLogicalProcessorInformationEx` and
//! `CallNtPowerInformation`; Linux `/sys/devices/system`; macOS `sysctl`
//! (`hw.perflevel*`). macOS cannot pin threads, so pinning returns
//! [`Error::Unsupported`] there.
//!
//! Core classes: a higher class is a faster core. On a machine with one kind
//! of core every core has class 0. No machine in the current test set has
//! mixed core types, so class detection on hybrid CPUs is untested.

use std::fmt;
use std::time::Duration;

mod assemble;
mod platform;

/// Identifies a logical CPU to the OS: a processor group and a number within
/// it on Windows; group 0 and the kernel's CPU number on Linux; group 0 and a
/// sequential number on macOS, which has no OS CPU numbers for user code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OsCpu {
    /// Windows processor group; 0 elsewhere.
    pub group: u16,
    /// Number within the group.
    pub number: u32,
}

impl fmt::Display for OsCpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.group == 0 {
            write!(f, "{}", self.number)
        } else {
            write!(f, "{}:{}", self.group, self.number)
        }
    }
}

/// One hardware thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalCpu {
    /// Position in [`Topology::cpus`].
    pub index: usize,
    /// The OS identifier, used for pinning.
    pub os: OsCpu,
    /// Index into [`Topology::cores`].
    pub core: usize,
}

/// One physical core and its hardware threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Core {
    /// Position in [`Topology::cores`].
    pub index: usize,
    /// Socket (package) index, numbered from 0 in OS CPU order.
    pub socket: usize,
    /// NUMA node as numbered by the OS, if the OS reports one.
    pub numa_node: Option<u32>,
    /// Core class; higher is faster. 0 on machines with one kind of core.
    pub class: u8,
    /// Indexes into [`Topology::cpus`] of this core's hardware threads.
    pub cpus: Vec<usize>,
    /// Maximum frequency in MHz as the OS reports it. Windows reports the
    /// nominal (base) clock here, not the boost clock; use
    /// [`measure_core_mhz`] for the clock actually reached.
    pub os_max_mhz: Option<u32>,
}

/// Kind of cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CacheKind {
    /// Data only.
    Data,
    /// Instructions only.
    Instruction,
    /// Data and instructions.
    Unified,
}

impl fmt::Display for CacheKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CacheKind::Data => "d",
            CacheKind::Instruction => "i",
            CacheKind::Unified => "",
        })
    }
}

/// One cache instance and the logical CPUs that share it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cache {
    /// 1 for L1, 2 for L2 and so on.
    pub level: u8,
    /// Data, instruction or unified.
    pub kind: CacheKind,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Line size in bytes, if reported.
    pub line_bytes: Option<u32>,
    /// Indexes into [`Topology::cpus`] of the CPUs sharing this cache.
    pub cpus: Vec<usize>,
}

/// The CPU topology of this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    /// Logical CPUs in OS order.
    pub cpus: Vec<LogicalCpu>,
    /// Physical cores, ordered by their first logical CPU.
    pub cores: Vec<Core>,
    /// Cache instances, ordered by level, kind and first CPU.
    pub caches: Vec<Cache>,
    /// Where the data came from.
    pub source: &'static str,
}

impl Topology {
    /// Reads the topology from the OS.
    pub fn detect() -> Result<Self, Error> {
        let raw = platform::detect()?;
        assemble::assemble(raw, platform::SOURCE)
    }

    /// Number of sockets.
    pub fn sockets(&self) -> usize {
        self.cores.iter().map(|c| c.socket + 1).max().unwrap_or(0)
    }

    /// NUMA nodes in use, ascending. Empty if the OS reports none.
    pub fn numa_nodes(&self) -> Vec<u32> {
        let mut nodes: Vec<u32> = self.cores.iter().filter_map(|c| c.numa_node).collect();
        nodes.sort_unstable();
        nodes.dedup();
        nodes
    }

    /// Core classes present, ascending.
    pub fn classes(&self) -> Vec<u8> {
        let mut classes: Vec<u8> = self.cores.iter().map(|c| c.class).collect();
        classes.sort_unstable();
        classes.dedup();
        classes
    }

    /// Caches that serve `core`, ordered by level then kind.
    pub fn core_caches(&self, core: usize) -> impl Iterator<Item = &Cache> {
        let cpus = &self.cores[core].cpus;
        self.caches
            .iter()
            .filter(move |c| c.cpus.iter().any(|cpu| cpus.contains(cpu)))
    }
}

/// Errors from topology detection and pinning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The OS does not offer this operation.
    Unsupported(&'static str),
    /// An OS call failed with this error code.
    Os {
        /// Name of the call.
        call: &'static str,
        /// OS error code (Win32 error, NTSTATUS or errno).
        code: i64,
    },
    /// The OS returned data that could not be understood.
    Parse(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(what) => write!(f, "not supported on this OS: {what}"),
            Error::Os { call, code } => write!(f, "{call} failed with code {code}"),
            Error::Parse(what) => write!(f, "could not parse topology data: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// Pins the calling thread to one logical CPU.
///
/// Call this at stage start-up, not on the hot path: it is a syscall.
pub fn pin_current_thread(cpu: OsCpu) -> Result<(), Error> {
    platform::pin_current_thread(cpu)
}

/// Scheduling priority for a stage thread, so ordinary threads of other
/// programs cannot pre-empt it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// The OS default.
    Normal,
    /// Above normal threads. Windows: `THREAD_PRIORITY_HIGHEST`. Linux: nice
    /// -10 (needs `CAP_SYS_NICE`).
    High,
    /// The top of the normal range. Windows: `THREAD_PRIORITY_TIME_CRITICAL`
    /// (priority 15 in a normal-class process; no privilege needed). Linux:
    /// `SCHED_FIFO` priority 50 (needs `CAP_SYS_NICE` or root). A thread at
    /// this level that spins can starve the rest of its core, so use it only
    /// on cores reserved for stages.
    TimeCritical,
}

/// Sets the calling thread's scheduling priority.
///
/// Call this at stage start-up, not on the hot path: it is a syscall.
/// macOS has no such control for ordinary threads and reports
/// `Unsupported` for anything but `Normal`.
pub fn set_current_thread_priority(priority: Priority) -> Result<(), Error> {
    platform::set_current_thread_priority(priority)
}

/// The logical CPU the calling thread is running on now, if the OS says.
pub fn current_cpu() -> Option<OsCpu> {
    platform::current_cpu()
}

/// Hints the CPU to pull the cache line holding `ptr` into L1 for reading.
///
/// Never faults, whatever the address. A no-op on architectures without a
/// prefetch instruction wired up here.
#[inline(always)]
pub fn prefetch_read<T>(ptr: *const T) {
    platform::cpu::prefetch_read(ptr.cast());
}

/// Reads the CPU's free-running tick counter: the time-stamp counter on
/// x86-64, `CNTVCT_EL0` on AArch64, a monotonic clock in nanoseconds
/// elsewhere. Ticks run at a fixed rate that is not the core clock; get the
/// rate with [`ticks_per_second`].
#[inline(always)]
pub fn ticks() -> u64 {
    platform::cpu::ticks()
}

/// Measures the tick rate of [`ticks`] against the OS clock over `window`.
pub fn ticks_per_second(window: Duration) -> u64 {
    let start_t = ticks();
    let start = std::time::Instant::now();
    while start.elapsed() < window {
        std::hint::spin_loop();
    }
    let elapsed = start.elapsed();
    let dt = ticks().wrapping_sub(start_t);
    (dt as f64 / elapsed.as_secs_f64()) as u64
}

/// Measures the clock frequency `cpu` reaches running one thread, in MHz.
///
/// Spawns a thread pinned to `cpu`, warms the core up, then times a chain of
/// dependent integer additions, each of which takes one cycle on the cores
/// this targets. Reports the best of several samples. Other load on the
/// core lowers the result; run it on a quiet machine.
///
/// Returns [`Error::Unsupported`] where threads cannot be pinned (macOS) or
/// on architectures without the timing loop (anything but x86-64 and
/// AArch64).
pub fn measure_core_mhz(cpu: OsCpu) -> Result<u32, Error> {
    if !platform::cpu::HAS_ADD_CHAIN {
        return Err(Error::Unsupported("clock measurement on this architecture"));
    }
    std::thread::scope(|s| {
        s.spawn(|| {
            pin_current_thread(cpu)?;
            // Let the core leave its idle state and reach its boost clock.
            let warm = std::time::Instant::now();
            while warm.elapsed() < Duration::from_millis(30) {
                platform::cpu::dependent_adds(10_000);
            }
            let rounds = 200_000;
            let mut best = 0.0f64;
            for _ in 0..7 {
                let start = std::time::Instant::now();
                platform::cpu::dependent_adds(rounds);
                let secs = start.elapsed().as_secs_f64();
                let adds = (rounds * platform::cpu::ADDS_PER_ROUND) as f64;
                best = best.max(adds / secs / 1e6);
            }
            Ok(best.round() as u32)
        })
        .join()
        .unwrap_or_else(|_| Err(Error::Parse("measurement thread panicked".into())))
    })
}

#[cfg(test)]
mod tests;
