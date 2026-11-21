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
