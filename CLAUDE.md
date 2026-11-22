# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# stagelanes

Rust runtime that reads each core's cache size, top frequency and socket,
then pins pipeline stages to the cores that suit them, with lock-free
batched handoff between stages. Runs on Windows and Linux; the topology
command also runs on macOS.

Read `../paralanes/docs/PLAN.md` and `../paralanes/docs/SPEC.md` before any
change. The plan sets milestone order and which machine does what. The spec is
the technical source of truth and uses the real crate names; the earlier
`cf-` placeholder names appear in the table below for reference. Where plan and spec disagree, the
plan is current; note the conflict in your status file.

> DRAFT. Items marked TBD are open decisions for Evan.

## Repo map

| Repo | Role | Depends on | Licence |
| --- | --- | --- | --- |
| `paralanes` | Spec, plan, atlas, network join, appliance image, all status files | All three below | GPL-3.0-only |
| `bytelanes` | Bulk data ops on any GPU through wgpu, each with a CPU reference. Owns the op format, which is the contract between all repos | Nothing above it | Apache-2.0 |
| `stagelanes` | Topology-aware stage runtime for Windows and Linux: pinning, rings, pinned memory, stage placement | Nothing above it | Apache-2.0 |
| `diagsteno` | The Diagnostic Stenographer: strain gauge and accelerometer signal processing, the demo workload | `bytelanes`, `stagelanes` | GPL-3.0-only |

Sibling repos are expected as checkouts next to each other (`../paralanes`,
`../bytelanes`, `../stagelanes`, `../diagsteno`). Use path dependencies during
development; switch to published versions only when Evan says so.

## Crates

| Crate | Spec name | Kind | Owns | `unsafe` |
| --- | --- | --- | --- | --- |
| `stagelanes-ring` | `cf-ring` | `no_std` lib | Lock-free rings: inter-stage queues and submission/completion pairs | yes |
| `stagelanes-mem` | `cf-mem` | lib | Pinned memory pools: large pages and NUMA on Linux and Windows, page-locked memory on macOS | yes |
| `stagelanes-arch` | `cf-arch` | lib | Topology detection (cache, frequency, socket, core class), core pinning, prefetch, cycle counters | yes |
| `stagelanes-pipe` | `cf-pipe` | lib | Stage threads, core and socket plan, batched handoff | no |
| `stagelanes-bench` | `cf-bench` (pipeline half) | bin | Synthetic load generator, latency histograms, bytes-touched counter, allocation and lock counters (R1, R2) | no |
| `stagelanes` | none | bin | CLI: `stagelanes topology` | no |

## Milestones in this repo

| # | Milestone | Agent |
| --- | --- | --- |
| M2 | Rings, pinned memory, topology detection on Windows and Linux; 0.0.1 prepared | B builds (Ryzen); A checks Linux and two sockets (Dell); `stagelanes topology` checked on all three machines |
| M6 | Stage pipeline: stages placed by topology, batched handoff. Release 2: `stagelanes` 0.1 | B builds and measures on bare metal; A repeats on two sockets |

## Which agent are you

Three agents work in parallel, one per machine. Identify your machine first
(`uname -sm`, `lscpu` or `sysctl -a | grep machdep.cpu`, `systeminfo` on
Windows, `vulkaninfo --summary`) and act only as that agent. If the milestone
you were given is not assigned to your machine in
`../paralanes/docs/PLAN.md`, stop and say so.

| Agent | Machine | OS |
| --- | --- | --- |
| A | Dell PowerEdge R440, 2x Xeon Silver 4214 (12 cores each, 24 total), 160 GB ECC, no GPU | Linux guest on Proxmox, cores pinned, both NUMA nodes exposed |
| B | Desktop, Ryzen 7 5700X (8 cores, AM4 Mini-ITX), Radeon RX 7800 XT 16 GB on PCIe 3.0 x16, 1 GbE (details: `docs/machines.md` in paralanes) | Windows |
| C | MacBook Pro, M5 Pro, 24 GB unified memory | macOS, AArch64 |

The Ryzen is the main machine for this repo because it runs on bare metal. On
the Ryzen, pipeline benchmarks (M2, M6) must not run at the same time as GPU
benchmarks (M3, M5). Windows cannot fully fence cores off from the OS: measure
on a quiet system and report the latency distribution, not one number.

## Hard rules

- **Agents never merge, tag or publish.** Prepare the pull request, changelog
  and version bump; Evan does the rest. Crates carry `publish = false` until
  Evan removes it.
- **One milestone per session.** Stop at the gate. Do not start the next
  milestone unprompted.
- **No unmeasured numbers.** A performance figure appears in a README, the
  atlas or a status file only with the run file that produced it. Every run
  file records machine, OS, kernel or build number, GPU driver version, wgpu
  backend, GPU host link (PCIe generation and width, or unified memory),
  network link type and speed for runs across machines, Rust version and the
  exact command.
- **A negative result is a result.** Publish it plainly.
- **Data rule.** Use generated data or data Evan confirms he owns. Never
  commit machine data from an employer or customer.
- **No `#[cfg(target_os)]` or `#[cfg(target_arch)]`** outside a module named
  `platform`.
- **Look up current docs** before using wgpu, tokio, io_uring or any
  networking crate API. Do not write them from memory.
- **Do not write NVIDIA or CUDA code.** That path is deferred until NVIDIA
  hardware exists.
- **Stage cores take no locks, do not allocate and make no syscalls after
  startup.** Stages hand off pointers in batches over `stagelanes-ring`.
- **A data partition is owned by one core** and its memory stays on that
  core's socket.
- **`unsafe`** only in `stagelanes-ring`, `stagelanes-mem` and
  `stagelanes-arch`. Every block gets a `// SAFETY:` comment (clippy denies
  undocumented unsafe blocks). The other crates have `#![forbid(unsafe_code)]`.
- **OS and arch code** lives only in a `platform` module inside
  `stagelanes-arch` and `stagelanes-mem`.
- `stagelanes-ring` stays `#![no_std]`.
- **Core-type placement is unproven.** No machine in the set has mixed core
  types; say so in the README and do not claim it.

## Status files

All status files live in the umbrella repo: write
`../paralanes/docs/status/M<n>.md` (shape defined in `../paralanes/CLAUDE.md`),
whichever repo the work was in, and commit it on your milestone branch there.

At the start of every session: rebase your branch on `main`, then read every
file in `../paralanes/docs/status/`.

## Branches

- `main` is Evan's. Never push to it.
- Work on `m<n>-<short-topic>` (for example `m3-rx7800-ops`). One branch per
  milestone per repo.
- Small commits, one logical change each. Run fmt, clippy and tests before
  each commit.

## Commands

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo +nightly miri test -p stagelanes-ring
RUSTFLAGS="--cfg loom" cargo test -p stagelanes-ring --release
cargo run -p stagelanes -- topology
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy and test on Ubuntu, Windows
and macOS, plus separate miri and loom jobs for `stagelanes-ring`. Run the same
set locally before each commit.

Single crate or single test:

```
cargo test -p stagelanes-ring
cargo test -p stagelanes-ring <test_name_substring>
cargo test -p stagelanes-ring <test_name> -- --exact --nocapture
```

The loom line above is POSIX shell syntax. In PowerShell set the variable first:
`$env:RUSTFLAGS="--cfg loom"; cargo test -p stagelanes-ring --release`, then
clear it (`Remove-Item Env:RUSTFLAGS`) so later builds don't reuse the loom cfg.

## Workspace setup

- Edition 2024, `rust-version = "1.85"`, resolver 3. Version, licence and
  `publish = false` are inherited from `[workspace.package]`; every crate sets
  `[lints] workspace = true`.
- Workspace lints: `missing_docs` warns, but clippy runs with `-D warnings`, so
  an undocumented public item fails CI. `unsafe_op_in_unsafe_fn` and
  `clippy::undocumented_unsafe_blocks` are deny.
- `cfg(loom)` is declared in `check-cfg`; loom-specific code in
  `stagelanes-ring` goes behind `#[cfg(loom)]`.
- Current state: M2 (ring, mem, arch, `stagelanes topology`) and M6 (pipe,
  bench) are built. `stagelanes-pipe` depends on ring, mem and arch;
  `stagelanes-bench` installs `CountingAlloc` as its global allocator.

Pipeline benchmark output from `stagelanes-bench` goes to
`../paralanes/atlas/runs/<machine>/<YYYY-MM-DD>-pipeline.json`; do not edit
run files after they are committed.

## Release rule

Agents prepare releases (changelog, README, version bump in a pull request).
Only Evan merges, tags and publishes.

## Open decisions (TBD)

- GitHub owner for the four repos
