extern crate std;

use std::boxed::Box;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::vec::Vec;

use crate::{Ring, RingPair};

/// Fewer iterations under miri, which is slow.
const STRESS: u64 = if cfg!(miri) { 500 } else { 200_000 };

/// Counts its drops into a shared counter.
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn fifo_full_and_empty() {
    let mut ring: Ring<u32, 4> = Ring::new();
    let (mut tx, mut rx) = ring.split();
    assert_eq!(rx.pop(), None);
    for i in 0..4 {
        tx.push(i).unwrap();
    }
    assert_eq!(tx.push(99), Err(99));
    assert_eq!(tx.free_slots(), 0);
    for i in 0..4 {
        assert_eq!(rx.pop(), Some(i));
    }
    assert_eq!(rx.pop(), None);
}

#[test]
fn wraps_many_times() {
    let mut ring: Ring<u64, 4> = Ring::new();
    let (mut tx, mut rx) = ring.split();
    for i in 0..1000 {
        tx.push(i).unwrap();
        tx.push(i + 1).unwrap();
        assert_eq!(rx.pop(), Some(i));
        assert_eq!(rx.pop(), Some(i + 1));
    }
}

#[test]
fn push_iter_keeps_items_that_do_not_fit() {
    let mut ring: Ring<u32, 8> = Ring::new();
    let (mut tx, mut rx) = ring.split();
    let mut items = 0..20;
    assert_eq!(tx.push_iter(&mut items), 8);
    assert_eq!(items.next(), Some(8));
    let mut out = [0u32; 5];
    assert_eq!(rx.pop_slice(&mut out), 5);
    assert_eq!(out, [0, 1, 2, 3, 4]);
    assert_eq!(tx.push_slice(&[100, 101, 102, 103, 104, 105]), 5);
    let mut all = Vec::new();
    rx.pop_with(usize::MAX, |v| all.push(v));
    assert_eq!(all, [5, 6, 7, 100, 101, 102, 103, 104]);
}

#[test]
fn pop_with_respects_max() {
    let mut ring: Ring<u32, 8> = Ring::new();
    let (mut tx, mut rx) = ring.split();
    tx.push_slice(&[1, 2, 3, 4, 5]);
    assert_eq!(rx.pop_with(2, |_| {}), 2);
    assert_eq!(rx.ready_items(), 3);
    assert_eq!(rx.pop_with(0, |_| {}), 0);
    assert_eq!(rx.pop(), Some(3));
}

#[test]
fn resplit_continues() {
    let mut ring: Ring<u32, 4> = Ring::new();
    {
        let (mut tx, mut rx) = ring.split();
        tx.push_slice(&[1, 2, 3]);
        assert_eq!(rx.pop(), Some(1));
    }
    let (mut tx, mut rx) = ring.split();
    tx.push(4).unwrap();
    tx.push(5).unwrap();
    assert_eq!(tx.push(6), Err(6));
    let mut out = [0; 4];
    assert_eq!(rx.pop_slice(&mut out), 4);
    assert_eq!(out, [2, 3, 4, 5]);
}

#[test]
fn drop_frees_unread_items_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let mut ring: Ring<Counted, 8> = Ring::new();
        let (mut tx, mut rx) = ring.split();
        for _ in 0..6 {
            assert!(tx.push(Counted(drops.clone())).is_ok());
        }
        drop(rx.pop());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
    assert_eq!(drops.load(Ordering::Relaxed), 6);
}

#[test]
fn leaked_handles_do_not_cause_double_drop() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let mut ring: Ring<Counted, 4> = Ring::new();
        let (mut tx, mut rx) = ring.split();
        for _ in 0..3 {
            assert!(tx.push(Counted(drops.clone())).is_ok());
        }
        rx.pop_with(2, drop);
        core::mem::forget(rx);
        core::mem::forget(tx);
    }
    assert_eq!(drops.load(Ordering::Relaxed), 3);
}

#[test]
fn panic_in_pop_callback_drops_each_item_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    {
        let mut ring: Ring<Counted, 8> = Ring::new();
        {
            let (mut tx, mut rx) = ring.split();
            for _ in 0..5 {
                assert!(tx.push(Counted(drops.clone())).is_ok());
            }
            let mut calls = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rx.pop_with(5, |item| {
                    calls += 1;
                    if calls == 2 {
                        panic!("callback failure");
                    }
                    drop(item);
                });
            }));
            assert!(result.is_err());
            // Two were read: the first dropped by the callback, the second
            // dropped during unwinding.
            assert_eq!(drops.load(Ordering::Relaxed), 2);
            // Dropping the consumer published its position, so the producer
            // sees the two freed slots.
            drop(rx);
            assert_eq!(tx.free_slots(), 5);
        }
    }
    assert_eq!(drops.load(Ordering::Relaxed), 5);
}

#[test]
fn zero_sized_items() {
    let mut ring: Ring<(), 2> = Ring::new();
    let (mut tx, mut rx) = ring.split();
    tx.push(()).unwrap();
    tx.push(()).unwrap();
    assert_eq!(tx.push(()), Err(()));
    assert_eq!(rx.pop_with(10, |()| {}), 2);
}

#[test]
fn threads_scoped_batches_in_order() {
    let mut ring: Box<Ring<u64, 64>> = Box::default();
    let (mut tx, mut rx) = ring.split();
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut items = 0..STRESS;
            while items.start < STRESS {
                if tx.push_iter(&mut items) == 0 {
                    std::hint::spin_loop();
                }
            }
        });
        let mut expected = 0;
        while expected < STRESS {
            let n = rx.pop_with(16, |v| {
                assert_eq!(v, expected);
                expected += 1;
            });
            if n == 0 {
                std::hint::spin_loop();
            }
        }
    });
}

#[test]
fn threads_owned_handles() {
    let (mut tx, mut rx) = Ring::<Box<u64>, 16>::new().split_arc();
    let producer = std::thread::spawn(move || {
        for i in 0..STRESS {
            let mut item = Box::new(i);
            loop {
                match tx.push(item) {
                    Ok(()) => break,
                    Err(back) => {
                        item = back;
                        std::thread::yield_now();
                    }
                }
            }
        }
    });
    let mut expected = 0;
    while expected < STRESS {
        match rx.pop() {
            Some(v) => {
                assert_eq!(*v, expected);
                expected += 1;
            }
            None => std::thread::yield_now(),
        }
    }
    producer.join().unwrap();
}

#[test]
fn pair_caps_outstanding_requests() {
    let mut pair: RingPair<u32, u32, 4> = RingPair::new();
    let (mut client, mut server) = pair.split();
    assert_eq!(client.submit_iter(&mut (0..10)), 4);
    assert_eq!(client.submit(10), Err(10));
    assert_eq!(client.outstanding(), 4);

    let mut taken = Vec::new();
    server.take(2, |r| taken.push(r));
    assert_eq!(taken, [0, 1]);
    // The client still cannot submit: those two are not completed yet.
    assert_eq!(client.submit(10), Err(10));
    assert_eq!(server.complete_iter(&mut taken.iter().map(|r| r * 10)), 2);
    let mut done = Vec::new();
    assert_eq!(client.poll(8, |c| done.push(c)), 2);
    assert_eq!(done, [0, 10]);
    assert_eq!(client.outstanding(), 2);
    client.submit(10).unwrap();
}

#[test]
fn pair_threaded_echo() {
    let (mut client, mut server) = RingPair::<u64, u64, 8>::new().split_arc();
    let total = STRESS / 4;
    let srv = std::thread::spawn(move || {
        let mut served = 0;
        let mut batch = Vec::with_capacity(8);
        while served < total {
            server.take(8, |r| batch.push(r + 1));
            let n = batch.len();
            served += n as u64;
            // One completion per request always fits.
            assert_eq!(server.complete_iter(&mut batch.drain(..)), n);
            std::thread::yield_now();
        }
    });
    let mut next = 0;
    let mut expected = 1;
    while expected <= total {
        next += client.submit_iter(&mut (next..total)) as u64;
        client.poll(8, |c| {
            assert_eq!(c, expected);
            expected += 1;
        });
    }
    srv.join().unwrap();
}
