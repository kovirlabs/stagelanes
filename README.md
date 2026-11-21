# stagelanes

**Status: pre-alpha (0.0.1 prepared).** The building blocks exist; the stage
pipeline that uses them does not yet. No performance figures are published.

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
| `stagelanes-pipe` | Stage threads placed by topology, batched handoff | Not built (M6) |
| `stagelanes-bench` | Pipeline benchmark | Not built (M6) |

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

## What is not proven

- **Core-type placement is unproven.** No machine in the current test set has
  mixed core types, so detection and placement by core class are untested on
  real hybrid hardware. (Class detection is unit-tested against synthetic
  Windows and Linux data only.)
- **Placement by socket** will be checked on a two-socket Xeon server
  (PowerEdge R440, 2x Xeon Silver 4214) by the two-socket runs in M2 and M6.
  It has not been run there yet.
- The Linux and macOS code paths are compile-checked on the Windows build
  machine and run in CI; they have not yet been run on the project's own Linux
  and macOS machines.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
