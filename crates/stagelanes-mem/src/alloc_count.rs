//! A counting global allocator, to prove that stage threads do not allocate
//! after start-up (M6 gate).
//!
//! A program installs it with
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: stagelanes_mem::CountingAlloc = stagelanes_mem::CountingAlloc;
//! ```
//!
//! A thread calls [`arm_current_thread`] once its start-up is done; from
//! then on every allocation, zeroed allocation and reallocation it makes is
//! counted, until [`disarm_current_thread`] returns the count. Threads that
//! never arm are not counted. Counting uses thread-local cells with constant
//! initialisers, so it never allocates or recurses.

use core::cell::Cell;
use std::alloc::{GlobalAlloc, Layout, System};

/// The system allocator, counting allocations of armed threads.
#[derive(Clone, Copy, Debug, Default)]
pub struct CountingAlloc;

std::thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    // `try_with`: during thread teardown the cells may already be gone.
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
    });
}

// SAFETY: every method forwards its arguments unchanged to `System`, which
// upholds the `GlobalAlloc` contract; counting only touches thread-local
// `Cell`s and never allocates.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller's guarantees for `alloc` hold for this call.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller's guarantees for `alloc_zeroed` hold for this call.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from this allocator, which got
        // them from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: `ptr` and `layout` came from this allocator, which got
        // them from `System`; the caller's guarantees for `new_size` hold.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Starts counting the calling thread's allocations, from zero.
pub fn arm_current_thread() {
    COUNT.with(|c| c.set(0));
    ARMED.with(|a| a.set(true));
}

/// Stops counting and returns the number of allocations the calling thread
/// made since [`arm_current_thread`].
pub fn disarm_current_thread() -> u64 {
    ARMED.with(|a| a.set(false));
    COUNT.with(Cell::get)
}

/// The calling thread's count so far (0 if it never armed).
pub fn current_thread_count() -> u64 {
    COUNT.with(Cell::get)
}
