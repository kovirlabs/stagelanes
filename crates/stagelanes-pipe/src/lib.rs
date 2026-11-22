//! stagelanes-pipe: the stage pipeline.
//!
//! An assembly line of pinned stages: each stage is one thread on one core,
//! doing one job, and stages hand items to each other in batches over
//! `stagelanes-ring` SPSC rings. The pieces:
//!
//! - [`link`]: a one-way stage-to-stage link (a ring plus a close flag).
//!   Sending into a full link and receiving from an empty one busy-poll: no
//!   system call, no lock, no allocation.
//! - [`CorePlan`]: picks cores from the machine's topology: one logical CPU
//!   per physical core, on one socket, with housekeeping cores set aside.
//! - [`spawn_stage`]: starts a stage thread pinned to its core, at a chosen
//!   priority, and reports what it did, including how many allocations it
//!   made after declaring itself [ready](StageCtx::ready) (counted when the
//!   program installs [`stagelanes_mem::CountingAlloc`]).
//!
//! The rules a stage follows after `ready`: no allocation, no lock, no
//! system call. Items should be handles into memory set up before the
//! stage started, such as [`stagelanes_mem::Block`]s from a pool.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use stagelanes_arch::{OsCpu, Priority, Topology};
use stagelanes_ring::{ArcRing, Consumer, Producer, Ring};

/// Errors from building a pipeline.
#[derive(Debug)]
pub enum Error {
    /// The core plan has no more cores on that socket.
    NotEnoughCores {
        /// The socket.
        socket: usize,
        /// Cores asked for.
        wanted: usize,
        /// Cores left.
        left: usize,
    },
    /// The OS refused to start a thread.
    Spawn(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotEnoughCores {
                socket,
                wanted,
                left,
            } => write!(
                f,
                "socket {socket}: {wanted} more cores wanted, {left} left"
            ),
            Error::Spawn(e) => write!(f, "could not start a stage thread: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// The sending end of a [`link`].
pub struct Tx<T, const N: usize> {
    ring: Producer<T, N, ArcRing<T, N>>,
    closed: Arc<AtomicBool>,
}

/// The receiving end of a [`link`].
pub struct Rx<T, const N: usize> {
    ring: Consumer<T, N, ArcRing<T, N>>,
    closed: Arc<AtomicBool>,
}

/// A one-way link of `N` slots between two stages. Dropping the [`Tx`]
/// closes it; the [`Rx`] sees the close only after it has received every
/// item sent before it.
pub fn link<T: Send, const N: usize>() -> (Tx<T, N>, Rx<T, N>) {
    let (p, c) = Ring::<T, N>::new().split_arc();
    let closed = Arc::new(AtomicBool::new(false));
    (
        Tx {
            ring: p,
            closed: closed.clone(),
        },
        Rx { ring: c, closed },
    )
}

impl<T: Send, const N: usize> Tx<T, N> {
    /// Sends `item`, busy-polling while the link is full (backpressure).
    pub fn send(&mut self, mut item: T) {
        loop {
            match self.ring.push(item) {
                Ok(()) => return,
                Err(back) => {
                    item = back;
                    core::hint::spin_loop();
                }
            }
        }
    }

    /// Sends every item of `items`, in order, busy-polling while full.
    pub fn send_all<I: IntoIterator<Item = T>>(&mut self, items: I) {
        let mut it = items.into_iter().peekable();
        while it.peek().is_some() {
            if self.ring.push_iter(&mut it) == 0 {
                core::hint::spin_loop();
            }
        }
    }

    /// Free slots right now.
    pub fn free(&self) -> usize {
        self.ring.free_slots()
    }
}

impl<T, const N: usize> Drop for Tx<T, N> {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

impl<T: Send, const N: usize> Rx<T, N> {
    /// Receives up to `max` items, handing each to `f`, without waiting.
    /// Returns `Some(n)` (possibly 0), or `None` once the link is closed and
    /// every item has been received.
    pub fn recv_with(&mut self, max: usize, mut f: impl FnMut(T)) -> Option<usize> {
        let n = self.ring.pop_with(max, &mut f);
        if n > 0 {
            return Some(n);
        }
        if self.closed.load(Ordering::Acquire) {
            // Items sent before the close are visible now: drain them.
            let n = self.ring.pop_with(max, &mut f);
            return if n > 0 { Some(n) } else { None };
        }
        Some(0)
    }

    /// Items waiting right now.
    pub fn ready(&self) -> usize {
        self.ring.ready_items()
    }
}

/// Cores for stages, chosen from the topology: one logical CPU per physical
/// core (its first SMT thread) on one socket, in core order.
#[derive(Clone, Debug)]
pub struct CorePlan {
    socket: usize,
    cpus: Vec<OsCpu>,
}

impl CorePlan {
    /// Every physical core of `socket`, first SMT thread each.
    pub fn for_socket(topo: &Topology, socket: usize) -> Self {
        let cpus = topo
            .cores
            .iter()
            .filter(|c| c.socket == socket)
            .filter_map(|c| c.cpus.first().map(|&i| topo.cpus[i].os))
            .collect();
        CorePlan { socket, cpus }
    }

    /// Takes the next `n` cores.
    pub fn take(&mut self, n: usize) -> Result<Vec<OsCpu>, Error> {
        if n > self.cpus.len() {
            return Err(Error::NotEnoughCores {
                socket: self.socket,
                wanted: n,
                left: self.cpus.len(),
            });
        }
        Ok(self.cpus.drain(..n).collect())
    }

    /// Cores not yet taken.
    pub fn left(&self) -> usize {
        self.cpus.len()
    }
}

/// How to start a stage.
#[derive(Clone, Debug)]
pub struct StageSpec {
    /// Name, for reports and the thread name.
    pub name: String,
    /// The core to pin to; `None` leaves the thread unpinned.
    pub cpu: Option<OsCpu>,
    /// Scheduling priority.
    pub priority: Priority,
}

/// What a running stage gets: start-up bookkeeping and idle accounting.
pub struct StageCtx {
    ready: bool,
    busy_polls: u64,
    idle_polls: u64,
}

impl StageCtx {
    /// Ends start-up. From here on the stage must not allocate, lock or make
    /// system calls; its allocations are counted from this point.
    pub fn ready(&mut self) {
        stagelanes_mem::alloc_count::arm_current_thread();
        self.ready = true;
    }

    /// Records a poll that found work.
    pub fn busy(&mut self) {
        self.busy_polls += 1;
    }

    /// Records a poll that found nothing, and spins once (no system call).
    pub fn idle(&mut self) {
        self.idle_polls += 1;
        core::hint::spin_loop();
    }
}

/// What a stage reports when it ends.
#[derive(Clone, Debug)]
pub struct StageReport {
    /// The stage's name.
    pub name: String,
    /// The core it was pinned to.
    pub cpu: Option<OsCpu>,
    /// Whether it called [`StageCtx::ready`].
    pub ready: bool,
    /// Allocations after `ready` (0 unless the program installs
    /// [`stagelanes_mem::CountingAlloc`], or if the stage kept the rule).
    pub allocations_after_ready: u64,
    /// Polls that found work, and polls that found none.
    pub busy_polls: u64,
    /// See `busy_polls`.
    pub idle_polls: u64,
    /// Pinning or priority problems; the stage still ran.
    pub setup_errors: Vec<String>,
}

/// Starts a stage: a thread pinned to `spec.cpu` at `spec.priority`, running
/// `body`. The handle returns the stage's report and `body`'s result.
pub fn spawn_stage<R: Send + 'static>(
    spec: StageSpec,
    body: impl FnOnce(&mut StageCtx) -> R + Send + 'static,
) -> Result<JoinHandle<(StageReport, R)>, Error> {
    std::thread::Builder::new()
        .name(spec.name.clone())
        .spawn(move || {
            let mut setup_errors = Vec::new();
            // Plain nested `if let`s: let-chains need Rust 1.88, and this
            // workspace builds on 1.85.
            if let Some(cpu) = spec.cpu {
                if let Err(e) = stagelanes_arch::pin_current_thread(cpu) {
                    setup_errors.push(format!("pin to {cpu}: {e}"));
                }
            }
            if spec.priority != Priority::Normal {
                if let Err(e) = stagelanes_arch::set_current_thread_priority(spec.priority) {
                    setup_errors.push(format!("priority {:?}: {e}", spec.priority));
                }
            }
            let mut ctx = StageCtx {
                ready: false,
                busy_polls: 0,
                idle_polls: 0,
            };
            let out = body(&mut ctx);
            let allocations = stagelanes_mem::alloc_count::disarm_current_thread();
            (
                StageReport {
                    name: spec.name,
                    cpu: spec.cpu,
                    ready: ctx.ready,
                    allocations_after_ready: if ctx.ready { allocations } else { 0 },
                    busy_polls: ctx.busy_polls,
                    idle_polls: ctx.idle_polls,
                    setup_errors,
                },
                out,
            )
        })
        .map_err(Error::Spawn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_delivers_in_order_and_closes_after_draining() {
        let (mut tx, mut rx) = link::<u32, 8>();
        let sender = std::thread::spawn(move || {
            tx.send_all(0..1000);
            for i in 1000..1010 {
                tx.send(i);
            }
        });
        let mut got = Vec::new();
        while rx.recv_with(16, |x| got.push(x)).is_some() {}
        sender.join().expect("sender");
        assert_eq!(got, (0..1010).collect::<Vec<_>>());
    }

    #[test]
    fn stage_reports_and_runs_on_its_core() {
        let topo = Topology::detect().expect("topology");
        let mut plan = CorePlan::for_socket(&topo, 0);
        assert!(plan.left() >= 1);
        let cpu = plan.take(1).expect("a core")[0];
        let h = spawn_stage(
            StageSpec {
                name: "t".into(),
                cpu: Some(cpu),
                priority: Priority::Normal,
            },
            move |ctx| {
                ctx.ready();
                ctx.busy();
                ctx.idle();
                stagelanes_arch::current_cpu()
            },
        )
        .expect("spawn");
        let (report, on) = h.join().expect("join");
        assert!(report.ready);
        assert_eq!((report.busy_polls, report.idle_polls), (1, 1));
        if report.setup_errors.is_empty() {
            assert_eq!(on, Some(cpu), "ran on its pinned core");
        }
    }

    #[test]
    fn plan_refuses_too_many_cores() {
        let topo = Topology::detect().expect("topology");
        let mut plan = CorePlan::for_socket(&topo, 0);
        let n = plan.left();
        assert!(plan.take(n + 1).is_err());
        assert_eq!(plan.take(n).expect("all").len(), n);
    }
}
