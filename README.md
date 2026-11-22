# stagelanes

**Status: alpha (0.1.0 prepared, milestone M6).** The stage pipeline and its
benchmark are built and measured on the Ryzen desktop; the two-socket run on
the Dell is still to come. Figures below come from run files in
`paralanes/atlas/runs/ryzen/`.

Rust runtime that reads each core's cache size, top frequency and socket, then pins pipeline stages to the cores that suit them, with lock-free batched handoff between stages. Runs on Windows and Linux; the `topology` command also runs on macOS.

Part of the paralanes project: `paralanes` (umbrella: spec, plan, atlas,
network join, appliance image), `bytelanes` (GPU data ops), `stagelanes`
(stage placement runtime) and `diagsteno` (strain and vibration processing).

## Crates

| Crate | What it does | State |
| --- | --- | --- |
| `stagelanes-ring` | `no_std` single-producer, single-consumer ring with batched publication, and a submission/completion ring pair | Built; checked with loom and miri |
| `stagelanes-mem` | Pinned memory regions (large pages, NUMA placement, locked pages) with a size cap, and a per-core block pool that never allocates after start-up | Built; tested on Windows |
| `stagelanes-arch` | Topology detection (sockets, NUMA nodes, cores, core classes, caches, frequency), thread pinning, prefetch, tick counter, per-core clock measurement | Built; tested on Windows; Linux parser tested against fixture trees |
| `stagelanes` | CLI: `stagelanes topology [--measure]` | Built |
| `stagelanes-pipe` | Stage-to-stage links (busy-polled SPSC rings with a close flag), a core plan from the topology, pinned stage threads at a chosen priority with an allocation count after start-up | Built; tested on Windows |
| `stagelanes-bench` | Pipeline benchmark: latency at 50% and 90% load, single-core baseline, ideal-queue reference, allocations, on-CPU share and bytes touched per stage | Built; measured on the Ryzen |

```
cargo run -p stagelanes -- topology            # what the OS reports
cargo run -p stagelanes -- topology --measure  # plus the clock each core reaches
```

Windows reports a core's base clock as its maximum, not its boost clock, so
`--measure` times a chain of dependent additions on one pinned thread per
core to find the clock actually reached.

Large pages on Windows need the "Lock pages in memory" user right
(`SeLockMemoryPrivilege`). Without it, `LargePages::Prefer` falls back to
normal locked pages and `LargePages::Require` fails with the reason. On Linux
large pages come from the reserved huge page pool
(`/proc/sys/vm/nr_hugepages`), and locking is limited by `ulimit -l`.

## Pipeline results (Ryzen 7 5700X, Windows 11)

`stagelanes-bench` runs source, parse, 4 workers and sink on 7 pinned
cores, with core 0 left to the OS. 256-byte blocks travel between stages as
handles; stages touch only the 64-byte header.

| | Result | Run file |
| --- | --- | --- |
| Capacity | 999,500 items/s, against 221,924 on one core | `2026-11-22-pipeline.json` |
| Evenly spaced arrivals, time-critical stage threads | p99 10.6 µs at 50% load, 11.1 µs at 90% (1.04×); worst item 65 µs | `2026-11-22-pipeline-3.json` |
| Evenly spaced arrivals, normal priority | one repeat in three collapsed to p99 19 ms when Windows preempted a stage | `2026-11-22-pipeline-2.json` |
| Poisson arrivals, time-critical | p99 35.7 µs at 50% and 193.6 µs at 90%, within 0–5% of a zero-overhead queue model fed the same arrivals | `2026-11-22-pipeline-4.json` |
| Stage rules | 0 allocations after start-up in every stage of every run; every stage on the CPU ≥ 98.5% of wall time; ≤ 48 header bytes touched per item | all four |

On Windows, run stage threads at `Priority::TimeCritical` on cores reserved
for them: at normal priority other threads can preempt a stage, and at 90%
load each millisecond of stall takes about nine to drain.

```
cargo run --release -p stagelanes-bench -- --machine <name> --arrivals fixed --priority time-critical
```

## What is not proven

- **Core-type placement is unproven.** No machine in the current test set has
  mixed core types, so detection and placement by core class are untested on
  real hybrid hardware. (Class detection is unit-tested against synthetic
  Windows and Linux data only.)
- **Placement by socket** will be checked on a two-socket Xeon server
  (PowerEdge R440, 2x Xeon Silver 4214) by the two-socket run in M6
  (`stagelanes-bench --socket S --pool-node N`). It has not been run there
  yet.
- **Zero locks** is shown by construction (no lock types in stage code) and
  by the on-CPU share; Windows gives no per-thread context-switch count. The
  Linux run records voluntary context switches per stage.
- The Linux and macOS code paths are compile-checked on the Windows build
  machine and run in CI; they have not yet been run on the project's own Linux
  and macOS machines.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
