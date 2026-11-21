# Changelog

All crates in this workspace share one version.

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
