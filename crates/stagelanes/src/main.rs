//! stagelanes: command-line tool for the stage runtime.
//!
//! `stagelanes topology` prints sockets, NUMA nodes, cores, core classes,
//! cache sizes and frequencies. `--measure` adds the clock each core reaches
//! under a single-threaded load.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::process::ExitCode;

use stagelanes_arch::{CacheKind, Topology, measure_core_mhz};

const USAGE: &str = "usage: stagelanes topology [--measure]

  topology    print sockets, NUMA nodes, cores, core classes, caches and
              frequencies of this machine
  --measure   also measure the clock each core reaches running one thread
              (pins a thread to each core in turn; run on a quiet machine)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["topology"] => topology(false),
        ["topology", "--measure"] | ["--measure", "topology"] => topology(true),
        ["-h"] | ["--help"] | ["help"] => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn topology(measure: bool) -> ExitCode {
    let topo = match Topology::detect() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("stagelanes: topology detection failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let measured: Option<Vec<Result<u32, String>>> = measure.then(|| {
        topo.cores
            .iter()
            .map(|c| measure_core_mhz(topo.cpus[c.cpus[0]].os).map_err(|e| e.to_string()))
            .collect()
    });
    print!("{}", render(&topo, measured.as_deref()));
    ExitCode::SUCCESS
}

/// Formats a byte count the way cache sizes are usually written.
fn size(bytes: u64) -> String {
    const K: u64 = 1 << 10;
    const M: u64 = 1 << 20;
    if bytes >= M && bytes % M == 0 {
        format!("{}M", bytes / M)
    } else if bytes >= K && bytes % K == 0 {
        format!("{}K", bytes / K)
    } else {
        format!("{bytes}B")
    }
}

/// Compresses sorted numbers into ranges: `0-3,8,10-11`.
fn ranges(nums: &[u32]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < nums.len() {
        let mut j = i;
        while j + 1 < nums.len() && nums[j + 1] == nums[j] + 1 {
            j += 1;
        }
        if !out.is_empty() {
            out.push(',');
        }
        if j > i {
            let _ = write!(out, "{}-{}", nums[i], nums[j]);
        } else {
            let _ = write!(out, "{}", nums[i]);
        }
        i = j + 1;
    }
    out
}

fn os_cpus(topo: &Topology, cpus: &[usize]) -> String {
    let groups: Vec<u16> = cpus.iter().map(|&i| topo.cpus[i].os.group).collect();
    if groups.iter().all(|&g| g == 0) {
        let nums: Vec<u32> = cpus.iter().map(|&i| topo.cpus[i].os.number).collect();
        ranges(&nums)
    } else {
        cpus.iter()
            .map(|&i| topo.cpus[i].os.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn cache_name(level: u8, kind: CacheKind) -> String {
    format!("L{level}{kind}")
}

fn render(topo: &Topology, measured: Option<&[Result<u32, String>]>) -> String {
    let mut o = String::new();
    let nodes = topo.numa_nodes();
    let classes = topo.classes();
    let _ = writeln!(o, "source: {}", topo.source);
    let _ = writeln!(
        o,
        "sockets: {}  NUMA nodes: {}  cores: {}  logical CPUs: {}  core classes: {}",
        topo.sockets(),
        if nodes.is_empty() {
            "not reported".to_owned()
        } else {
            format!("{} ({})", nodes.len(), ranges(&nodes))
        },
        topo.cores.len(),
        topo.cpus.len(),
        classes.len(),
    );
    if classes.len() > 1 {
        for class in &classes {
            let n = topo.cores.iter().filter(|c| c.class == *class).count();
            let _ = writeln!(o, "  class {class}: {n} cores (higher class = faster core)");
        }
    }

    // One column per cache level and kind present.
    let mut kinds: Vec<(u8, CacheKind)> = topo.caches.iter().map(|c| (c.level, c.kind)).collect();
    kinds.sort_unstable();
    kinds.dedup();

    let mut header = vec![
        "core".to_owned(),
        "socket".to_owned(),
        "node".to_owned(),
        "class".to_owned(),
        "cpus".to_owned(),
    ];
    header.extend(kinds.iter().map(|&(l, k)| cache_name(l, k)));
    header.push("os max MHz".to_owned());
    if measured.is_some() {
        header.push("measured MHz".to_owned());
    }
    let mut rows = vec![header];
    for core in &topo.cores {
        let mut row = vec![
            core.index.to_string(),
            core.socket.to_string(),
            core.numa_node.map_or("-".to_owned(), |n| n.to_string()),
            core.class.to_string(),
            os_cpus(topo, &core.cpus),
        ];
        for &(level, kind) in &kinds {
            let cell = topo
                .core_caches(core.index)
                .find(|c| c.level == level && c.kind == kind)
                .map_or("-".to_owned(), |c| size(c.size_bytes));
            row.push(cell);
        }
        row.push(core.os_max_mhz.map_or("-".to_owned(), |m| m.to_string()));
        if let Some(m) = measured {
            row.push(match &m[core.index] {
                Ok(mhz) => mhz.to_string(),
                Err(e) => format!("error: {e}"),
            });
        }
        rows.push(row);
    }
    let widths: Vec<usize> = (0..rows[0].len())
        .map(|i| rows.iter().map(|r| r[i].len()).max().unwrap_or(0))
        .collect();
    let _ = writeln!(o);
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        let _ = writeln!(o, "{}", line.join("  ").trim_end());
    }

    let _ = writeln!(o, "\ncaches:");
    for &(level, kind) in &kinds {
        let instances: Vec<_> = topo
            .caches
            .iter()
            .filter(|c| c.level == level && c.kind == kind)
            .collect();
        let mut sizes: Vec<u64> = instances.iter().map(|c| c.size_bytes).collect();
        sizes.sort_unstable();
        sizes.dedup();
        let sizes: Vec<String> = sizes.into_iter().map(size).collect();
        let line = instances
            .iter()
            .find_map(|c| c.line_bytes)
            .map_or(String::new(), |l| format!(", {l} B lines"));
        let _ = writeln!(
            o,
            "  {:<4} {} x{}{line}",
            cache_name(level, kind),
            sizes.join("/"),
            instances.len(),
        );
        for c in &instances {
            if c.cpus.len() > topo.cores[topo.cpus[c.cpus[0]].core].cpus.len() {
                let _ = writeln!(
                    o,
                    "         {} shared by CPUs {}",
                    size(c.size_bytes),
                    os_cpus(topo, &c.cpus)
                );
            }
        }
    }
    if measured.is_some() {
        let _ = writeln!(
            o,
            "\nmeasured MHz: best of 7 timed runs of dependent integer adds on one \
             thread pinned to the core's first CPU, after a 30 ms warm-up"
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_ranges() {
        assert_eq!(size(32 << 10), "32K");
        assert_eq!(size(32 << 20), "32M");
        assert_eq!(size(16896 << 10), "16896K");
        assert_eq!(size(100), "100B");
        assert_eq!(ranges(&[0, 1, 2, 3, 8, 10, 11]), "0-3,8,10-11");
        assert_eq!(ranges(&[]), "");
    }

    #[test]
    fn renders_this_machine() {
        if let Ok(t) = Topology::detect() {
            let text = render(&t, None);
            assert!(text.contains("logical CPUs"));
        }
    }
}
