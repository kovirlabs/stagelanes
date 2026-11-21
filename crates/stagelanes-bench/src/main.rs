//! stagelanes-bench: pipeline benchmark.
//!
//! Synthetic load generator, latency histograms, a counter of payload bytes
//! touched by stage cores, and allocation and lock counters for stage cores
//! after startup. Checks spec requirements R1 and R2. Writes run files to
//! `../paralanes/atlas/runs/<machine>/<YYYY-MM-DD>-pipeline.json`.
//!
//! Status: stub. Built in M6 (spec: pipeline half of `cf-bench`).

#![forbid(unsafe_code)]

fn main() {
    eprintln!("stagelanes-bench: not implemented yet (M6)");
    std::process::exit(2);
}
