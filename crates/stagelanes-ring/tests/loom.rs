//! Loom models of the rings. Run with
//! `RUSTFLAGS="--cfg loom" cargo test -p stagelanes-ring --release`.
#![cfg(loom)]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;
use stagelanes_ring::{Ring, RingPair};

/// Single pushes and pops through a ring small enough to wrap and fill.
#[test]
fn spsc_single_items_wrap() {
    loom::model(|| {
        let (mut tx, mut rx) = Ring::<usize, 2>::new().split_arc();
        let producer = thread::spawn(move || {
            let mut i = 0;
            while i < 3 {
                if tx.push(i).is_ok() {
                    i += 1;
                } else {
                    thread::yield_now();
                }
            }
        });
        let mut expected = 0;
        while expected < 3 {
            match rx.pop() {
                Some(v) => {
                    assert_eq!(v, expected);
                    expected += 1;
                }
                None => thread::yield_now(),
            }
        }
        producer.join().unwrap();
    });
}

/// Batched publication on both sides.
#[test]
fn spsc_batches() {
    loom::model(|| {
        let (mut tx, mut rx) = Ring::<usize, 2>::new().split_arc();
        let producer = thread::spawn(move || {
            let mut items = 0..4;
            while items.start < 4 {
                if tx.push_iter(&mut items) == 0 {
                    thread::yield_now();
                }
            }
        });
        let mut expected = 0;
        while expected < 4 {
            let n = rx.pop_with(2, |v| {
                assert_eq!(v, expected);
                expected += 1;
            });
            if n == 0 {
                thread::yield_now();
            }
        }
        producer.join().unwrap();
    });
}

struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Owned values pushed while the consumer is still reading; every value is
/// dropped exactly once, by the consumer or by the ring when it is freed.
#[test]
fn spsc_drops_each_value_once() {
    loom::model(|| {
        let drops = Arc::new(AtomicUsize::new(0));
        let (mut tx, mut rx) = Ring::<Counted, 2>::new().split_arc();
        let d = drops.clone();
        let producer = thread::spawn(move || {
            for _ in 0..2 {
                // Ring has room for both, so pushes never fail.
                assert!(tx.push(Counted(d.clone())).is_ok());
            }
        });
        drop(rx.pop());
        producer.join().unwrap();
        drop(rx);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    });
}

/// A request crosses the submission ring and its completion comes back.
#[test]
fn pair_round_trip() {
    loom::model(|| {
        let (mut client, mut server) = RingPair::<usize, usize, 2>::new().split_arc();
        let srv = thread::spawn(move || {
            let mut served = 0;
            while served < 2 {
                let mut got = None;
                if server.take(1, |r| got = Some(r)) == 1 {
                    server.complete(got.unwrap() * 10).unwrap();
                    served += 1;
                } else {
                    thread::yield_now();
                }
            }
        });
        client.submit(1).unwrap();
        client.submit(2).unwrap();
        let mut results = Vec::new();
        while results.len() < 2 {
            if client.poll(2, |c| results.push(c)) == 0 {
                thread::yield_now();
            }
        }
        assert_eq!(results, [10, 20]);
        assert_eq!(client.outstanding(), 0);
        srv.join().unwrap();
    });
}
