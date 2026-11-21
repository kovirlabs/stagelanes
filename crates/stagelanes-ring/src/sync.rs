//! Atomics and cells that switch to loom's checked versions under `cfg(loom)`.

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{AtomicUsize, Ordering};

/// An `UnsafeCell` with loom's closure-based access API on both builds.
#[cfg(not(loom))]
pub(crate) struct Cell<T>(core::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> Cell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(core::cell::UnsafeCell::new(value))
    }

    /// Calls `f` with a raw pointer to the contents. The caller must uphold
    /// the aliasing rules for whatever it does through the pointer.
    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}

#[cfg(loom)]
pub(crate) struct Cell<T>(loom::cell::UnsafeCell<T>);

#[cfg(loom)]
impl<T> Cell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(loom::cell::UnsafeCell::new(value))
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        self.0.with_mut(f)
    }
}

/// Pads and aligns a value to 128 bytes so two indices written by different
/// cores never share a cache line. 128 covers the adjacent-line prefetch on
/// x86-64 and the 128-byte lines on Apple silicon.
#[repr(align(128))]
pub(crate) struct CachePadded<T>(pub(crate) T);

impl<T> core::ops::Deref for CachePadded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
