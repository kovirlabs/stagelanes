# Changelog

All crates in this workspace share one version.

## 0.1.0 — unreleased

Release 2. Milestone M6: the stage pipeline.

### Added

- `stagelanes-pipe`: `link::<T, N>()`, a one-way stage-to-stage link over an
  SPSC ring with a close flag (busy-polled, no system call, lock or
  allocation); `CorePlan` (one logical CPU per physical core of a socket);
  `spawn_stage`, a stage thread pinned to its core at a `Priority`, whose
  `StageCtx::ready` and `finish` bracket the section in which allocations
  are counted, and whose `StageReport` gives the count, poll counts and
  setup errors.
- `stagelanes-mem`: `CountingAlloc`, a global allocator that counts the
  allocations of threads that armed counting (`alloc_count` module).
- `stagelanes-bench`: the pipeline benchmark. Source, parse, workers and
  sink on pinned cores; fixed or Poisson arrivals at chosen load fractions;
  latency from scheduled release; a single-core baseline; an ideal
  zero-overhead queue model fed the same arrivals; per-stage allocations,
  on-CPU share, voluntary context switches (Linux) and bytes touched;
  `--socket` and `--pool-node` for two-socket machines. Writes run files to
  `paralanes/atlas/runs/<machine>/`.
- `stagelanes-arch`: `Priority` and `set_current_thread_priority`;
  `DurableWriter` (IoRing batch writes and flush on Windows, positioned
  writes and `sync_data` elsewhere).

## 0.0.1 — unreleased

First release, mainly to hold the crate names. Milestone M2.

### Added

- `stagelanes-ring`: `Ring<T, N>`, a bounded `no_std` single-producer,
  single-consumer ring with inline storage. `push_iter` and `pop_with`
  publish a whole batch with one store. Borrowed handles (`split`) and owned
  handles (`split_arc`, `alloc` feature, on by default). `RingPair<S, C, N>`
  pairs a submission ring with a completion ring and caps outstanding
  requests so completions never block. Checked with loom and miri.
- `stagelanes-mem`: `Region`, pinned memory with large pages, NUMA placement
  and page locking on Windows and Linux, locking only on macOS; a size cap
  per region. `BlockPool` splits a region into fixed-size blocks whose
  take and put never allocate.
- `stagelanes-arch`: `Topology::detect` (sockets, NUMA nodes, cores, core
  classes, caches, OS-reported maximum frequency) on Windows, Linux and
  macOS; `pin_current_thread`, `current_cpu`, `prefetch_read`, `ticks`,
  `ticks_per_second`, `measure_core_mhz`.
- `stagelanes`: the `stagelanes topology [--measure]` command.
