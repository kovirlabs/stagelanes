//! stagelanes-bench: the pipeline benchmark (M6; spec R1, R2).
//!
//! ```text
//! stagelanes-bench --machine ryzen [--machine-desc TEXT] [--out DIR] [--note TEXT]
//!                  [--seconds 10] [--warmup 2] [--repeats 3] [--loads 0.5,0.9]
//!                  [--workers 4] [--parse-ns 500] [--work-ns 4000] [--batch 8]
//!                  [--socket 0] [--priority normal|high|time-critical]
//! ```
//!
//! A synthetic assembly line on pinned cores of one socket (the socket's
//! first core is left to the OS):
//!
//! ```text
//! source -> parse -> worker x W -> sink
//!    ^                              |
//!    +------- blocks returned ------+
//! ```
//!
//! - Items are 256-byte blocks from a `stagelanes-mem` pool set up before
//!   the stages start; stages pass the block handles over
//!   `stagelanes-pipe` links. Stages read and write only the 64-byte header,
//!   through accessors that count the bytes they touch (R1).
//! - Parse and the workers do calibrated CPU work per item (`--parse-ns`,
//!   `--work-ns`); the workers are the bottleneck by default.
//! - An unpaced run measures capacity (items/s). Then the source releases
//!   batches of `--batch` items at each `--loads` fraction of that capacity,
//!   with random (Poisson) gaps between batches by default, or evenly
//!   spaced with `--arrivals fixed` (which never queues below capacity). Latency runs from an item's *scheduled*
//!   release to the sink, so a source that falls behind cannot hide delay.
//! - `--pool-node N` places the block pool on NUMA node N (two-socket
//!   machines: compare the stages' own node with the other one).
//! - The single-core baseline does the same work for each item on one core
//!   (release, parse, work and sink inline), measured the same way.
//!
//! The run file (`<out>/<date>-pipeline.json`, never overwritten) holds the
//! latency percentiles per load, the R2 ratio p99(90%) / p99(50%), every
//! stage's allocations after start-up (counted by `CountingAlloc`), each
//! stage's share of wall time spent on the CPU (a stage that blocked on a
//! lock or a system call would fall below 1), voluntary context switches
//! where the OS reports them per thread (Linux), and the bytes stage cores
//! touched per item.

#![forbid(unsafe_code)]

mod meta;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use stagelanes_arch::{OsCpu, Priority, Topology, ticks, ticks_per_second};
use stagelanes_mem::{Block, BlockPool, CountingAlloc, Region, RegionConfig};
use stagelanes_pipe::{CorePlan, Rx, StageCtx, StageReport, StageSpec, Tx, link, spawn_stage};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Slots per stage link.
const SLOTS: usize = 1024;
/// Bytes per item block.
const BLOCK: usize = 256;
/// Header bytes stages may touch; the rest of the block is payload.
const HEADER: usize = 64;
/// Blocks in the pool, and slots in the return link.
const POOL_BLOCKS: usize = 4096;

type Error = Box<dyn std::error::Error>;

// ---------- configuration ----------

#[derive(Clone, Debug)]
struct Args {
    machine: String,
    machine_desc: String,
    out: Option<PathBuf>,
    note: String,
    seconds: f64,
    warmup: f64,
    repeats: usize,
    loads: Vec<f64>,
    workers: usize,
    parse_ns: f64,
    work_ns: f64,
    batch: usize,
    socket: usize,
    priority: Priority,
    poisson: bool,
    pool_node: Option<u32>,
}

fn usage() -> ! {
    eprintln!(
        "usage: stagelanes-bench --machine <name> [--machine-desc TEXT] [--out DIR] [--note TEXT] \
         [--seconds S] [--warmup S] [--repeats N] [--loads 0.5,0.9] [--workers W] \
         [--parse-ns NS] [--work-ns NS] [--batch B] [--socket S] \
         [--priority normal|high|time-critical] [--arrivals poisson|fixed] [--pool-node N]"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut a = Args {
        machine: String::new(),
        machine_desc: String::new(),
        out: None,
        note: String::new(),
        seconds: 10.0,
        warmup: 2.0,
        repeats: 3,
        loads: vec![0.5, 0.9],
        workers: 4,
        parse_ns: 500.0,
        work_ns: 4000.0,
        batch: 8,
        socket: 0,
        priority: Priority::Normal,
        poisson: true,
        pool_node: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        let num = |s: String| s.parse::<f64>().unwrap_or_else(|_| usage());
        match flag.as_str() {
            "--machine" => a.machine = val(),
            "--machine-desc" => a.machine_desc = val(),
            "--out" => a.out = Some(val().into()),
            "--note" => a.note = val(),
            "--seconds" => a.seconds = num(val()),
            "--warmup" => a.warmup = num(val()),
            "--repeats" => a.repeats = (num(val()) as usize).max(1),
            "--loads" => {
                a.loads = val()
                    .split(',')
                    .map(|s| s.parse().unwrap_or_else(|_| usage()))
                    .collect()
            }
            "--workers" => a.workers = (num(val()) as usize).max(1),
            "--parse-ns" => a.parse_ns = num(val()),
            "--work-ns" => a.work_ns = num(val()),
            "--batch" => a.batch = (num(val()) as usize).max(1),
            "--socket" => a.socket = num(val()) as usize,
            "--pool-node" => a.pool_node = Some(num(val()) as u32),
            "--arrivals" => {
                a.poisson = match val().as_str() {
                    "poisson" => true,
                    "fixed" => false,
                    _ => usage(),
                }
            }
            "--priority" => {
                a.priority = match val().as_str() {
                    "normal" => Priority::Normal,
                    "high" => Priority::High,
                    "time-critical" => Priority::TimeCritical,
                    _ => usage(),
                }
            }
            _ => usage(),
        }
    }
    if a.machine.is_empty() {
        usage();
    }
    a
}

// ---------- calibrated work ----------

/// `iters` rounds of xorshift64*: CPU work the compiler cannot remove.
fn spin_work(mut x: u64, iters: u64) -> u64 {
    for _ in 0..iters {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    }
    x
}

/// Iterations of [`spin_work`] per nanosecond, measured on `cpu`.
fn calibrate_on(cpu: OsCpu) -> Result<f64, Error> {
    std::thread::spawn(move || -> Result<f64, String> {
        stagelanes_arch::pin_current_thread(cpu).map_err(|e| e.to_string())?;
        let iters = 20_000_000u64;
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            std::hint::black_box(spin_work(std::hint::black_box(7), iters));
            best = best.min(t.elapsed().as_secs_f64());
        }
        Ok(iters as f64 / (best * 1e9))
    })
    .join()
    .map_err(|_| "calibration thread panicked")?
    .map_err(Into::into)
}

// ---------- latency histogram ----------

/// Log-linear histogram: 16 buckets per power of two (about 4.4%
/// resolution) from 1 ns to 2^41 ns. Recording never allocates.
struct Hist {
    counts: Box<[u64]>,
    n: u64,
    max: u64,
    sum: u128,
}

const PER_OCTAVE: f64 = 16.0;

impl Hist {
    fn new() -> Self {
        Hist {
            counts: vec![0u64; 41 * PER_OCTAVE as usize].into_boxed_slice(),
            n: 0,
            max: 0,
            sum: 0,
        }
    }

    fn record(&mut self, ns: u64) {
        let b = if ns <= 1 {
            0
        } else {
            ((ns as f64).log2() * PER_OCTAVE) as usize
        };
        let last = self.counts.len() - 1;
        self.counts[b.min(last)] += 1;
        self.n += 1;
        self.max = self.max.max(ns);
        self.sum += u128::from(ns);
    }

    /// The upper edge of the bucket holding quantile `q`, capped at the max.
    fn quantile(&self, q: f64) -> u64 {
        if self.n == 0 {
            return 0;
        }
        let want = ((self.n as f64) * q).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (b, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= want {
                return (2f64.powf((b + 1) as f64 / PER_OCTAVE) as u64).min(self.max);
            }
        }
        self.max
    }

    fn merge(&mut self, o: &Hist) {
        for (a, b) in self.counts.iter_mut().zip(o.counts.iter()) {
            *a += b;
        }
        self.n += o.n;
        self.max = self.max.max(o.max);
        self.sum += o.sum;
    }

    fn json(&self) -> Value {
        json!({
            "n": self.n,
            "p50_ns": self.quantile(0.50),
            "p90_ns": self.quantile(0.90),
            "p99_ns": self.quantile(0.99),
            "p99_9_ns": self.quantile(0.999),
            "max_ns": self.max,
            "mean_ns": if self.n == 0 { 0 } else { (self.sum / u128::from(self.n)) as u64 },
        })
    }
}

// ---------- items and header access ----------

struct Item {
    block: Block,
    /// Scheduled release time, in ticks.
    t0: u64,
}

/// Header accessors: the only way stages touch a block. They count the
/// bytes touched and cannot reach past the header.
struct Touch(u64);

impl Touch {
    fn read(&mut self, b: &Block, at: usize) -> u64 {
        let h = &b.as_slice()[..HEADER];
        let mut v = [0u8; 8];
        v.copy_from_slice(&h[at..at + 8]);
        self.0 += 8;
        u64::from_le_bytes(v)
    }

    fn write(&mut self, b: &mut Block, at: usize, v: u64) {
        let h = &mut b.as_mut_slice()[..HEADER];
        h[at..at + 8].copy_from_slice(&v.to_le_bytes());
        self.0 += 8;
    }
}

// Header layout: 0 sequence number, 8 parse result, 16 work result.
const H_SEQ: usize = 0;
const H_PARSE: usize = 8;
const H_WORK: usize = 16;

// ---------- per-stage accounting ----------

#[derive(Clone, Copy, Debug, Default)]
struct Usage {
    cpu_s: f64,
    wall_s: f64,
    voluntary_switches: Option<u64>,
}

/// Thread CPU time and wall time from start-up to the end of measurement.
/// Both readings are taken outside the counted (ready) section.
struct Meter {
    cpu0: cpu_time::ThreadTime,
    w0: u64,
    switches0: Option<u64>,
}

impl Meter {
    fn start(ctx: &mut StageCtx) -> Self {
        let m = Meter {
            cpu0: cpu_time::ThreadTime::now(),
            w0: ticks(),
            switches0: voluntary_switches(),
        };
        ctx.ready();
        m
    }

    fn stop(self, ctx: &mut StageCtx, ns_per_tick: f64) -> Usage {
        let w1 = ticks();
        ctx.finish();
        Usage {
            cpu_s: self.cpu0.elapsed().as_secs_f64(),
            wall_s: w1.saturating_sub(self.w0) as f64 * ns_per_tick / 1e9,
            voluntary_switches: voluntary_switches()
                .zip(self.switches0)
                .map(|(a, b)| a.saturating_sub(b)),
        }
    }
}

/// Voluntary context switches of the calling thread, where the OS reports
/// them per thread (Linux `/proc/thread-self/status`).
fn voluntary_switches() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/thread-self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with("voluntary_ctxt_switches:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[derive(Default)]
struct Out {
    usage: Usage,
    touched: u64,
    items: u64,
    hist: Option<Hist>,
    /// Items released in the measured window (source and baseline).
    released: u64,
}

struct Run {
    hist: Hist,
    /// Items released in the measured window per second.
    items_per_s: f64,
    stages: Vec<(StageReport, Out)>,
}

// ---------- timing ----------

#[derive(Clone, Copy)]
struct Cfg {
    workers: usize,
    parse_iters: u64,
    work_iters: u64,
    batch: usize,
    priority: Priority,
    warmup_s: f64,
    run_s: f64,
    tps: f64,
    poisson: bool,
    pool_node: Option<u32>,
}

impl Cfg {
    fn ns_per_tick(&self) -> f64 {
        1e9 / self.tps
    }

    /// (start, measure_from, end) in ticks, starting shortly from now so
    /// every stage is spinning before the first release.
    fn window(&self) -> (u64, u64, u64) {
        let start = ticks() + (self.tps * 0.2) as u64;
        let from = start + (self.tps * self.warmup_s) as u64;
        (start, from, from + (self.tps * self.run_s) as u64)
    }

    /// Ticks between batch releases at `rate` items/s.
    fn period(&self, rate: Option<f64>) -> Option<f64> {
        rate.map(|r| self.tps * self.batch as f64 / r)
    }
}

/// The gap to the next batch release: `mean` ticks, either fixed or drawn
/// from an exponential distribution (Poisson arrivals) with a seeded
/// xorshift generator, so every run sees the same sequence.
fn gap(mean: f64, poisson: bool, rng: &mut u64) -> f64 {
    if !poisson {
        return mean;
    }
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    let u = ((*rng >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    -mean * u.ln()
}

fn spec(name: String, cpu: OsCpu, priority: Priority) -> StageSpec {
    StageSpec {
        name,
        cpu: Some(cpu),
        priority,
    }
}

// ---------- the pipeline ----------

/// One run of the pipeline. `cores`: source, parse, workers..., sink.
fn run_pipeline(cfg: &Cfg, cores: &[OsCpu], rate: Option<f64>) -> Result<Run, Error> {
    let w = cfg.workers;
    let (src_cpu, parse_cpu, sink_cpu) = (cores[0], cores[1], cores[2 + w]);
    let (start, from, end) = cfg.window();
    let period = cfg.period(rate);
    let npt = cfg.ns_per_tick();
    let c = *cfg;

    let (to_parse, parse_rx) = link::<Item, SLOTS>();
    let mut to_workers = Vec::with_capacity(w);
    let mut worker_rx = Vec::with_capacity(w);
    let mut worker_tx = Vec::with_capacity(w);
    let mut sink_rx = Vec::with_capacity(w);
    for _ in 0..w {
        let (tx, rx) = link::<Item, SLOTS>();
        to_workers.push(tx);
        worker_rx.push(rx);
        let (tx, rx) = link::<Item, SLOTS>();
        worker_tx.push(tx);
        sink_rx.push(rx);
    }
    let (ret_tx, ret_rx) = link::<Block, POOL_BLOCKS>();
    let pool = BlockPool::new(
        Region::new(&RegionConfig {
            numa_node: cfg.pool_node,
            ..RegionConfig::new(POOL_BLOCKS * BLOCK)
        })?,
        BLOCK,
    )?;

    let mut handles = Vec::new();

    // Source: owns the pool, releases batches on schedule, recycles blocks.
    handles.push(spawn_stage(spec("source".into(), src_cpu, c.priority), {
        let mut pool: BlockPool = pool;
        let mut ret_rx: Rx<Block, POOL_BLOCKS> = ret_rx;
        let mut to_parse: Tx<Item, SLOTS> = to_parse;
        move |ctx: &mut StageCtx| {
            let m = Meter::start(ctx);
            let mut t = Touch(0);
            let (mut seq, mut released) = (0u64, 0u64);
            let mut next = start as f64;
            let mut rng = 0x9E37_79B9_7F4A_7C15u64;
            while ticks() < start {
                core::hint::spin_loop();
            }
            loop {
                ret_rx.recv_with(usize::MAX, |b| {
                    let _ = pool.put(b);
                });
                let now = ticks();
                if now >= end {
                    break;
                }
                let t0 = match period {
                    Some(_) if (now as f64) < next => {
                        ctx.idle();
                        continue;
                    }
                    Some(_) => next as u64,
                    None => now,
                };
                if pool.available() < c.batch {
                    ctx.idle();
                    continue;
                }
                for _ in 0..c.batch {
                    let mut block = pool.take().expect("checked available");
                    t.write(&mut block, H_SEQ, seq);
                    to_parse.send(Item { block, t0 });
                    seq += 1;
                }
                if t0 >= from {
                    released += c.batch as u64;
                }
                if let Some(p) = period {
                    next += gap(p, c.poisson, &mut rng);
                }
                ctx.busy();
            }
            drop(to_parse);
            // Take every block back before ending (the sink closes the link).
            while ret_rx
                .recv_with(usize::MAX, |b| {
                    let _ = pool.put(b);
                })
                .is_some()
            {
                core::hint::spin_loop();
            }
            Out {
                usage: m.stop(ctx, npt),
                touched: t.0,
                items: seq,
                released,
                ..Out::default()
            }
        }
    })?);

    // Parse: per-item work, then route to a worker by sequence number.
    handles.push(spawn_stage(spec("parse".into(), parse_cpu, c.priority), {
        let mut rx = parse_rx;
        let mut outs = to_workers;
        move |ctx: &mut StageCtx| {
            let m = Meter::start(ctx);
            let mut t = Touch(0);
            let mut items = 0u64;
            let workers = outs.len() as u64;
            loop {
                let got = rx.recv_with(c.batch * 4, |mut it| {
                    let seq = t.read(&it.block, H_SEQ);
                    let r = spin_work(seq | 1, c.parse_iters);
                    t.write(&mut it.block, H_PARSE, r);
                    outs[(seq % workers) as usize].send(it);
                    items += 1;
                });
                match got {
                    None => break,
                    Some(0) => ctx.idle(),
                    Some(_) => ctx.busy(),
                }
            }
            drop(outs);
            Out {
                usage: m.stop(ctx, npt),
                touched: t.0,
                items,
                ..Out::default()
            }
        }
    })?);

    // Workers: the main per-item work.
    for (i, (mut rx, mut tx)) in worker_rx.into_iter().zip(worker_tx).enumerate() {
        handles.push(spawn_stage(
            spec(format!("worker{i}"), cores[2 + i], c.priority),
            move |ctx: &mut StageCtx| {
                let m = Meter::start(ctx);
                let mut t = Touch(0);
                let mut items = 0u64;
                loop {
                    let got = rx.recv_with(c.batch * 4, |mut it| {
                        let p = t.read(&it.block, H_PARSE);
                        let r = spin_work(p, c.work_iters);
                        t.write(&mut it.block, H_WORK, r);
                        tx.send(it);
                        items += 1;
                    });
                    match got {
                        None => break,
                        Some(0) => ctx.idle(),
                        Some(_) => ctx.busy(),
                    }
                }
                drop(tx);
                Out {
                    usage: m.stop(ctx, npt),
                    touched: t.0,
                    items,
                    ..Out::default()
                }
            },
        )?);
    }

    // Sink: latency from scheduled release; blocks go back to the source.
    handles.push(spawn_stage(spec("sink".into(), sink_cpu, c.priority), {
        let mut rxs = sink_rx;
        let mut ret = ret_tx;
        move |ctx: &mut StageCtx| {
            let mut hist = Hist::new();
            let mut open = vec![true; rxs.len()];
            let m = Meter::start(ctx);
            let mut t = Touch(0);
            let (mut items, mut check) = (0u64, 0u64);
            loop {
                let (mut any, mut got) = (false, 0);
                for (i, rx) in rxs.iter_mut().enumerate() {
                    if !open[i] {
                        continue;
                    }
                    let r = rx.recv_with(64, |it| {
                        let now = ticks();
                        check ^= t.read(&it.block, H_WORK);
                        if it.t0 >= from && it.t0 < end {
                            hist.record((now.saturating_sub(it.t0) as f64 * npt) as u64);
                        }
                        ret.send(it.block);
                        items += 1;
                    });
                    match r {
                        None => open[i] = false,
                        Some(n) => {
                            any = true;
                            got += n;
                        }
                    }
                }
                if !any {
                    break;
                }
                if got == 0 {
                    ctx.idle();
                } else {
                    ctx.busy();
                }
            }
            drop(ret);
            std::hint::black_box(check);
            Out {
                usage: m.stop(ctx, npt),
                touched: t.0,
                items,
                hist: Some(hist),
                ..Out::default()
            }
        }
    })?);

    let mut stages = Vec::new();
    for h in handles {
        stages.push(h.join().map_err(|_| "a stage panicked")?);
    }
    let released = stages[0].1.released;
    let hist = stages
        .iter_mut()
        .find_map(|(_, o)| o.hist.take())
        .ok_or("sink returned no histogram")?;
    Ok(Run {
        hist,
        items_per_s: released as f64 / cfg.run_s,
        stages,
    })
}

// ---------- the single-core baseline ----------

/// The same per-item work on one core: release, parse, work and sink inline.
fn run_baseline(cfg: &Cfg, cpu: OsCpu, rate: Option<f64>) -> Result<Run, Error> {
    let (start, from, end) = cfg.window();
    let period = cfg.period(rate);
    let npt = cfg.ns_per_tick();
    let c = *cfg;
    let mut pool = BlockPool::new(Region::new(&RegionConfig::new(BLOCK))?, BLOCK)?;
    let h = spawn_stage(spec("single-core".into(), cpu, c.priority), move |ctx| {
        let mut hist = Hist::new();
        let mut block = pool.take().expect("one block");
        let m = Meter::start(ctx);
        let mut t = Touch(0);
        let (mut seq, mut released, mut check) = (0u64, 0u64, 0u64);
        let mut next = start as f64;
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        while ticks() < start {
            core::hint::spin_loop();
        }
        loop {
            let now = ticks();
            if now >= end {
                break;
            }
            let t0 = match period {
                Some(_) if (now as f64) < next => {
                    ctx.idle();
                    continue;
                }
                Some(_) => next as u64,
                None => now,
            };
            for _ in 0..c.batch {
                t.write(&mut block, H_SEQ, seq);
                let s = t.read(&block, H_SEQ);
                let p = spin_work(s | 1, c.parse_iters);
                t.write(&mut block, H_PARSE, p);
                let p = t.read(&block, H_PARSE);
                let r = spin_work(p, c.work_iters);
                t.write(&mut block, H_WORK, r);
                check ^= t.read(&block, H_WORK);
                if t0 >= from {
                    hist.record((ticks().saturating_sub(t0) as f64 * npt) as u64);
                }
                seq += 1;
            }
            if t0 >= from {
                released += c.batch as u64;
            }
            if let Some(p) = period {
                next += gap(p, c.poisson, &mut rng);
            }
            ctx.busy();
        }
        let usage = m.stop(ctx, npt);
        std::hint::black_box(check);
        let _ = pool.put(block);
        Out {
            usage,
            touched: t.0,
            items: seq,
            hist: Some(hist),
            released,
        }
    })?;
    let (report, mut out) = h.join().map_err(|_| "baseline panicked")?;
    let hist = out.hist.take().ok_or("baseline returned no histogram")?;
    Ok(Run {
        hist,
        items_per_s: out.released as f64 / cfg.run_s,
        stages: vec![(report, out)],
    })
}

// ---------- reporting ----------

fn on_cpu(o: &Out) -> f64 {
    if o.usage.wall_s > 0.0 {
        o.usage.cpu_s / o.usage.wall_s
    } else {
        0.0
    }
}

fn per_item(o: &Out) -> f64 {
    if o.items > 0 {
        o.touched as f64 / o.items as f64
    } else {
        0.0
    }
}

fn stages_json(stages: &[(StageReport, Out)]) -> Value {
    Value::Array(
        stages
            .iter()
            .map(|(r, o)| {
                json!({
                    "stage": r.name,
                    "cpu": r.cpu.map(|c| c.to_string()),
                    "items": o.items,
                    "allocations_after_ready": r.allocations_after_ready,
                    "on_cpu_fraction": on_cpu(o),
                    "cpu_s": o.usage.cpu_s,
                    "wall_s": o.usage.wall_s,
                    "voluntary_context_switches": o.usage.voluntary_switches,
                    "busy_polls": r.busy_polls,
                    "idle_polls": r.idle_polls,
                    "bytes_touched_per_item": per_item(o),
                    "setup_errors": r.setup_errors,
                })
            })
            .collect(),
    )
}

fn runs_json(runs: &[Run]) -> Vec<Value> {
    runs.iter()
        .map(|r| {
            json!({
                "achieved_items_per_s": r.items_per_s,
                "latency": r.hist.json(),
                "stages": stages_json(&r.stages),
            })
        })
        .collect()
}

/// The ideal pipeline: the same seeded arrivals through a zero-overhead model
/// with the same shape (one parse server, then item `seq` to worker
/// `seq % W`), each worker taking `W / capacity` seconds per item so the
/// model has the measured capacity. Nothing else costs time: no handoff,
/// no cache misses, no interrupts. Its latency is the queueing delay that
/// random arrivals cause by themselves at this load.
fn ideal(cfg: &Cfg, ipn: f64, capacity: f64, load: f64) -> Hist {
    let w = cfg.workers;
    let parse_s = cfg.parse_iters as f64 / ipn / 1e9;
    let work_s = w as f64 / capacity;
    let mean_gap = cfg.batch as f64 / (load * capacity);
    let mut hist = Hist::new();
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let (mut t, mut parse_free, mut seq) = (0.0f64, 0.0f64, 0usize);
    let mut worker_free = vec![0.0f64; w];
    while t < cfg.warmup_s + cfg.run_s {
        for _ in 0..cfg.batch {
            parse_free = parse_free.max(t) + parse_s;
            let k = seq % w;
            worker_free[k] = worker_free[k].max(parse_free) + work_s;
            if t >= cfg.warmup_s {
                hist.record(((worker_free[k] - t) * 1e9) as u64);
            }
            seq += 1;
        }
        t += gap(mean_gap, cfg.poisson, &mut rng);
    }
    hist
}

fn us(ns: u64) -> String {
    format!("{:.1} us", ns as f64 / 1000.0)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

struct LoadRuns {
    load: f64,
    pipe: Vec<Run>,
    base: Vec<Run>,
}

/// p99 at `hi` over p99 at `lo`, per repeat.
fn ratios(hi: &[Run], lo: &[Run]) -> Vec<f64> {
    hi.iter()
        .zip(lo)
        .map(|(h, l)| h.hist.quantile(0.99) as f64 / l.hist.quantile(0.99).max(1) as f64)
        .collect()
}

fn out_path(args: &Args) -> PathBuf {
    let dir = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../paralanes/atlas/runs")
            .join(&args.machine)
    });
    let date = meta::utc_date();
    let mut path = dir.join(format!("{date}-pipeline.json"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{date}-pipeline-{n}.json"));
        n += 1;
    }
    path
}

fn real_main(args: &Args) -> Result<(), Error> {
    let started = format!("{} {}", meta::utc_date(), meta::utc_time());
    let topo = Topology::detect()?;
    let mut plan = CorePlan::for_socket(&topo, args.socket);
    let housekeeping = plan.take(1)?;
    let cores = plan.take(3 + args.workers)?;
    let tps = ticks_per_second(Duration::from_millis(250)) as f64;
    let ipn = calibrate_on(cores[1])?;
    let cfg = Cfg {
        workers: args.workers,
        parse_iters: (args.parse_ns * ipn).max(1.0) as u64,
        work_iters: (args.work_ns * ipn).max(1.0) as u64,
        batch: args.batch,
        priority: args.priority,
        warmup_s: args.warmup,
        run_s: args.seconds,
        tps,
        poisson: args.poisson,
        pool_node: args.pool_node,
    };
    let cores_s: Vec<String> = cores.iter().map(ToString::to_string).collect();
    eprintln!(
        "stage cores {} (housekeeping {}), {:.0} ticks/s, {:.3} work iterations/ns",
        cores_s.join(","),
        housekeeping[0],
        tps,
        ipn
    );

    eprintln!("capacity: unpaced runs");
    let cap_pipe = run_pipeline(&cfg, &cores, None)?;
    let cap_base = run_baseline(&cfg, cores[1], None)?;
    eprintln!(
        "  pipeline {:.0} items/s, single core {:.0} items/s",
        cap_pipe.items_per_s, cap_base.items_per_s
    );

    let mut by_load = Vec::new();
    for &load in &args.loads {
        let mut lr = LoadRuns {
            load,
            pipe: Vec::new(),
            base: Vec::new(),
        };
        for rep in 0..args.repeats {
            let p = run_pipeline(&cfg, &cores, Some(load * cap_pipe.items_per_s))?;
            let b = run_baseline(&cfg, cores[1], Some(load * cap_base.items_per_s))?;
            eprintln!(
                "  load {:.0}% rep {}: pipeline p50 {} p99 {} p99.9 {} | single core p50 {} p99 {}",
                load * 100.0,
                rep + 1,
                us(p.hist.quantile(0.5)),
                us(p.hist.quantile(0.99)),
                us(p.hist.quantile(0.999)),
                us(b.hist.quantile(0.5)),
                us(b.hist.quantile(0.99)),
            );
            lr.pipe.push(p);
            lr.base.push(b);
        }
        by_load.push(lr);
    }

    // R2: p99 at the highest load over p99 at the lowest, per repeat.
    let lo = by_load
        .iter()
        .min_by(|a, b| a.load.total_cmp(&b.load))
        .ok_or("no loads")?;
    let hi = by_load
        .iter()
        .max_by(|a, b| a.load.total_cmp(&b.load))
        .ok_or("no loads")?;
    let pipe_ratios = ratios(&hi.pipe, &lo.pipe);
    let base_ratios = ratios(&hi.base, &lo.base);
    let r2 = median(pipe_ratios.clone());
    let cfg_ref = &cfg;
    let ideal_ratio = ideal(&cfg, ipn, cap_pipe.items_per_s, hi.load).quantile(0.99) as f64
        / ideal(&cfg, ipn, cap_pipe.items_per_s, lo.load)
            .quantile(0.99)
            .max(1) as f64;

    let all: Vec<&Run> = [&cap_pipe, &cap_base]
        .into_iter()
        .chain(by_load.iter().flat_map(|l| l.pipe.iter().chain(&l.base)))
        .collect();
    let outs = || all.iter().flat_map(|r| r.stages.iter());
    let allocs = outs()
        .map(|(r, _)| r.allocations_after_ready)
        .max()
        .unwrap_or(0);
    let min_on_cpu = outs().map(|(_, o)| on_cpu(o)).fold(f64::INFINITY, f64::min);
    let switches: Option<u64> = outs()
        .map(|(_, o)| o.usage.voluntary_switches)
        .try_fold(0u64, |acc, s| s.map(|s| acc + s));
    let touched_max = outs().map(|(_, o)| per_item(o)).fold(0.0, f64::max);

    let loads_json: Vec<Value> = by_load
        .iter()
        .map(|l| {
            let mut pp = Hist::new();
            let mut pb = Hist::new();
            l.pipe.iter().for_each(|r| pp.merge(&r.hist));
            l.base.iter().for_each(|r| pb.merge(&r.hist));
            json!({
                "load": l.load,
                "pipeline": {
                    "target_items_per_s": l.load * cap_pipe.items_per_s,
                    "pooled": pp.json(),
                    "repeats": runs_json(&l.pipe),
                },
                "ideal_queue": ideal(cfg_ref, ipn, cap_pipe.items_per_s, l.load).json(),
                "single_core": {
                    "target_items_per_s": l.load * cap_base.items_per_s,
                    "pooled": pb.json(),
                    "repeats": runs_json(&l.base),
                },
            })
        })
        .collect();

    let doc = json!({
        "kind": "pipeline",
        "benchmark": "stagelanes-bench (M6; spec R1, R2)",
        "started_utc": started,
        "machine": {
            "name": args.machine,
            "description": args.machine_desc,
            "os_build": meta::os_build(),
            "topology": {
                "source": topo.source,
                "sockets": topo.sockets(),
                "cores": topo.cores.len(),
                "logical_cpus": topo.cpus.len(),
            },
            "gpu": "not used (CPU-only benchmark)",
            "network": "not used (single machine)",
        },
        "build": {
            "rustc": meta::rustc(),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "git": meta::git(),
        },
        "command": meta::command(),
        "note": args.note,
        "config": {
            "socket": args.socket,
            "housekeeping_cpu": housekeeping[0].to_string(),
            "stage_cpus": cores_s,
            "layout": format!("source, parse, {} workers, sink; one logical CPU per physical core", args.workers),
            "priority": format!("{:?}", args.priority),
            "arrivals": if args.poisson { "poisson: exponential gaps between batches, seeded (same sequence every run)" } else { "fixed: evenly spaced batches" },
            "pool_numa_node": match args.pool_node { Some(n) => json!(n), None => json!("OS default (first touch)") },
            "batch": args.batch,
            "link_slots": SLOTS,
            "block_bytes": BLOCK,
            "header_bytes": HEADER,
            "pool_blocks": POOL_BLOCKS,
            "parse_ns": args.parse_ns,
            "work_ns": args.work_ns,
            "work_iterations_per_ns": ipn,
            "parse_iterations": cfg.parse_iters,
            "work_iterations": cfg.work_iters,
            "ticks_per_second": tps,
            "warmup_s": args.warmup,
            "measure_s": args.seconds,
            "repeats": args.repeats,
            "latency": "from each item's scheduled release to the sink; histogram bucket upper edge, 16 buckets per power of two (about 4.4%)",
        },
        "capacity": {
            "pipeline_items_per_s": cap_pipe.items_per_s,
            "single_core_items_per_s": cap_base.items_per_s,
            "pipeline_stages": stages_json(&cap_pipe.stages),
        },
        "loads": loads_json,
        "r2": {
            "rule": "p99 at the highest load is at most 3x p99 at the lowest load",
            "low_load": lo.load,
            "high_load": hi.load,
            "pipeline_ratio_per_repeat": pipe_ratios,
            "pipeline_ratio_median": r2,
            "single_core_ratio_per_repeat": base_ratios,
            "ideal_queue_ratio": ideal_ratio,
            "pass": r2 <= 3.0,
        },
        "stage_rules": {
            "max_allocations_after_ready": allocs,
            "min_on_cpu_fraction": min_on_cpu,
            "voluntary_context_switches_total": match switches {
                Some(s) => json!(s),
                None => json!("not reported per thread by this OS"),
            },
            "locks": "stage code uses no lock types and links are lock-free SPSC rings; the on-CPU fraction (and voluntary context switches on Linux) would show a stage that blocked",
            "max_bytes_touched_per_item": touched_max,
            "r1": format!("stages touch only the {HEADER}-byte header of each {BLOCK}-byte block; the payload is never addressed"),
        },
    });

    let path = out_path(args);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&doc)? + "\n")?;

    println!(
        "R2: p99 at {:.0}% / p99 at {:.0}% = {:.2} (median of {}), {}",
        hi.load * 100.0,
        lo.load * 100.0,
        r2,
        pipe_ratios.len(),
        if r2 <= 3.0 { "pass" } else { "FAIL" }
    );
    println!("ideal zero-overhead queue ratio (same arrivals): {ideal_ratio:.2}");
    println!(
        "single-core ratio {:.2}; capacity: pipeline {:.0}/s, single core {:.0}/s",
        median(base_ratios),
        cap_pipe.items_per_s,
        cap_base.items_per_s
    );
    println!(
        "stage rules: max allocations after ready {allocs}, min on-CPU fraction {min_on_cpu:.3}, \
         max bytes touched per item {touched_max:.0}"
    );
    println!("wrote {}", path.display());
    Ok(())
}

fn main() {
    let args = parse_args();
    if let Err(e) = real_main(&args) {
        eprintln!("stagelanes-bench: {e}");
        std::process::exit(1);
    }
}
