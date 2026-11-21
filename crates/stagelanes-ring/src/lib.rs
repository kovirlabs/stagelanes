//! stagelanes-ring: lock-free rings.
//!
//! Inter-stage queues and submission/completion pairs for pipeline stages
//! that hand off pointers in batches. `no_std`, no allocation on the push or
//! pop path, no locks, no syscalls.
//!
//! - [`Ring`]: a bounded single-producer, single-consumer queue. Each side
//!   publishes its position once per batch ([`Producer::push_iter`],
//!   [`Consumer::pop_with`]), so a batch moves for one cache-line transfer
//!   each way.
//! - [`RingPair`]: a submission ring and a completion ring, with the number of
//!   outstanding requests capped at the ring size so completions never block.
//!
//! The `alloc` feature (on by default) adds owned handles through
//! [`Ring::split_arc`] for threads that outlive their creator.
//!
//! Checked with loom (`RUSTFLAGS="--cfg loom" cargo test -p stagelanes-ring
//! --release`) and miri (`cargo +nightly miri test -p stagelanes-ring`).
//!
//! # Example
//!
//! (Compiled but not run as a doctest: under `--cfg loom` the ring's atomics
//! work only inside a loom model. The unit tests run the same pattern.)
//!
//! ```no_run
//! use stagelanes_ring::Ring;
//!
//! let mut ring: Ring<u32, 8> = Ring::new();
//! let (mut tx, mut rx) = ring.split();
//! std::thread::scope(|s| {
//!     s.spawn(move || {
//!         let mut items = 0..100;
//!         while items.len() > 0 {
//!             tx.push_iter(&mut items);
//!         }
//!     });
//!     let mut sum = 0;
//!     let mut seen = 0;
//!     while seen < 100 {
//!         seen += rx.pop_with(8, |v| sum += v);
//!     }
//!     assert_eq!(sum, (0..100).sum());
//! });
//! ```

#![cfg_attr(not(loom), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod pair;
mod spsc;
mod sync;

pub use pair::{Client, RingPair, Server};
#[cfg(feature = "alloc")]
pub use spsc::ArcRing;
pub use spsc::{Consumer, Producer, Ring};

#[cfg(all(test, not(loom)))]
mod tests;
