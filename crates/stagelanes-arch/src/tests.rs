use super::*;

/// Structural checks that hold on any machine.
fn check_invariants(t: &Topology) {
    assert!(!t.cpus.is_empty());
    for (i, cpu) in t.cpus.iter().enumerate() {
        assert_eq!(cpu.index, i);
        assert!(t.cores[cpu.core].cpus.contains(&i));
    }
    for (i, core) in t.cores.iter().enumerate() {
        assert_eq!(core.index, i);
        assert!(!core.cpus.is_empty());
        assert!(core.socket < t.sockets());
    }
    let all_cpus: usize = t.cores.iter().map(|c| c.cpus.len()).sum();
    assert_eq!(all_cpus, t.cpus.len());
    for cache in &t.caches {
        assert!(cache.level >= 1 && cache.size_bytes > 0 && !cache.cpus.is_empty());
    }
}

#[test]
fn detects_this_machine() {
    match Topology::detect() {
        Ok(t) => check_invariants(&t),
        Err(Error::Unsupported(_)) => {}
        Err(e) => panic!("detection failed: {e}"),
    }
}

/// Pins to each CPU in turn and checks the thread lands there. A CPU the
/// process may not use (a cgroup cpuset or `taskset` on Linux) refuses the
/// pin; that is allowed, but at least one pin must work.
#[test]
fn pins_to_every_cpu() {
    let Ok(t) = Topology::detect() else { return };
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut pinned = 0;
            for cpu in &t.cpus {
                match pin_current_thread(cpu.os) {
                    Ok(()) => {
                        pinned += 1;
                        if let Some(now) = current_cpu() {
                            assert_eq!(now, cpu.os, "pinned to {} but running on {now}", cpu.os);
                        }
                    }
                    Err(Error::Unsupported(_)) => return,
                    Err(e) => eprintln!("pinning to {} refused: {e}", cpu.os),
                }
            }
            assert!(pinned > 0, "no CPU accepted a pin");
        });
    });
}

#[test]
fn ticks_advance() {
    let a = ticks();
    std::thread::sleep(Duration::from_millis(2));
    let b = ticks();
    assert!(b > a);
    assert!(ticks_per_second(Duration::from_millis(20)) > 1_000_000);
}

#[test]
fn prefetch_never_faults() {
    let x = [1u8; 64];
    prefetch_read(x.as_ptr());
    prefetch_read(core::ptr::null::<u8>());
    prefetch_read(usize::MAX as *const u8);
}

#[test]
fn measures_a_plausible_clock() {
    let Ok(t) = Topology::detect() else { return };
    match measure_core_mhz(t.cpus[0].os) {
        // Wide bounds: this checks the method, not the machine.
        Ok(mhz) => assert!((200..10_000).contains(&mhz), "measured {mhz} MHz"),
        Err(Error::Unsupported(_)) => {}
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn class_ranking() {
    assert_eq!(
        assemble::rank_classes(&[1024u64, 512, 1024, 256]),
        [2, 1, 2, 0]
    );
    assert_eq!(assemble::rank_classes::<u64>(&[]), [] as [u8; 0]);
}

#[test]
fn sets_thread_priority() {
    // On its own thread, so the test runner's threads keep their priority.
    std::thread::spawn(|| {
        set_current_thread_priority(Priority::Normal).expect("Normal is always allowed");
        for p in [Priority::High, Priority::TimeCritical] {
            match set_current_thread_priority(p) {
                Ok(()) => {}
                // Linux without CAP_SYS_NICE, and macOS, may refuse; Windows
                // allows both levels in a normal-class process.
                Err(Error::Os { .. } | Error::Unsupported(_)) => {}
                Err(e) => panic!("{p:?}: {e}"),
            }
        }
        set_current_thread_priority(Priority::Normal).expect("back to Normal");
    })
    .join()
    .expect("priority thread");
}

#[test]
fn durable_writer_writes_every_chunk() {
    let dir = std::env::temp_dir().join(format!("stagelanes-durable-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("batch.bin");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("open");
    let chunks: Vec<Vec<u8>> = (0..3u8)
        .map(|i| {
            (0..(1000 + 777 * usize::from(i)))
                .map(|j| (j as u8) ^ i)
                .collect()
        })
        .collect();
    let refs: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
    let mut w = DurableWriter::new().expect("writer");
    eprintln!("durable writer method: {}", w.method());
    w.write(&file, 5, &refs).expect("durable write");
    // A second batch after the first, through the same writer.
    w.write(
        &file,
        5 + refs.iter().map(|c| c.len() as u64).sum::<u64>(),
        &refs[..1],
    )
    .expect("second durable write");
    drop(file);
    let got = std::fs::read(&path).expect("read back");
    let mut want = vec![0u8; 5];
    for c in &chunks {
        want.extend_from_slice(c);
    }
    want.extend_from_slice(&chunks[0]);
    assert_eq!(got, want);
    let _ = std::fs::remove_dir_all(&dir);
}
