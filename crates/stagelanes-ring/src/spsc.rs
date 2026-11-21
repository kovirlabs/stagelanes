//! Bounded single-producer, single-consumer ring with batched publication.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::Deref;

use crate::sync::{AtomicUsize, CachePadded, Cell, Ordering};

/// Position and cached view of the other side, private to one handle.
///
/// It lives inside the ring rather than in the handle so that the ring's
/// `Drop` always knows exactly which slots hold live values, even if a handle
/// was leaked or a callback panicked half-way through a batch. Only the
/// owning handle touches it, so `Relaxed` is enough.
struct Side {
    /// Next index this side will use (write index for the producer, read
    /// index for the consumer). Free-running, wraps at `usize::MAX`.
    pos: AtomicUsize,
    /// Last value seen of the other side's published index.
    cached_other: AtomicUsize,
}

impl Side {
    fn new() -> Self {
        Self {
            pos: AtomicUsize::new(0),
            cached_other: AtomicUsize::new(0),
        }
    }
}

/// A bounded lock-free queue for one producer and one consumer.
///
/// `N` must be a power of two. Storage is inline, so the ring needs no
/// allocator; put large rings in a `Box` or a static. The producer and
/// consumer each own their own cache lines and publish their position once
/// per batch, so a batch of `k` items costs one cache-line transfer each way
/// rather than `k`.
///
/// Get handles with [`Ring::split`] (borrowed, for scoped threads) or
/// [`Ring::split_arc`] (owned, needs the `alloc` feature).
pub struct Ring<T, const N: usize> {
    /// Published read index, written by the consumer.
    head: CachePadded<AtomicUsize>,
    /// Published write index, written by the producer.
    tail: CachePadded<AtomicUsize>,
    prod: CachePadded<Side>,
    cons: CachePadded<Side>,
    slots: [Cell<MaybeUninit<T>>; N],
}

// SAFETY: values of `T` move from the producer's thread to the consumer's
// thread through the ring, so `T: Send` is required and sufficient. Each slot
// is accessed by at most one side at a time: the producer writes slot `i`
// only while `i` is outside the published range `[head, tail)`, and the
// consumer reads it only once it is inside. The Release store of `tail`
// (`head`) and the matching Acquire load on the other side order the slot
// access before the hand-over.
unsafe impl<T: Send, const N: usize> Sync for Ring<T, N> {}

// SAFETY: as above; the ring owns its `T` values, which may move threads.
unsafe impl<T: Send, const N: usize> Send for Ring<T, N> {}

impl<T, const N: usize> Default for Ring<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Ring<T, N> {
    const MASK: usize = {
        assert!(
            N.is_power_of_two(),
            "ring capacity must be a non-zero power of two"
        );
        N - 1
    };

    /// Creates an empty ring.
    ///
    /// # Panics
    ///
    /// Fails to compile if `N` is not a non-zero power of two.
    pub fn new() -> Self {
        let _ = Self::MASK;
        Self {
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
            prod: CachePadded(Side::new()),
            cons: CachePadded(Side::new()),
            slots: core::array::from_fn(|_| Cell::new(MaybeUninit::uninit())),
        }
    }

    /// Number of slots.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Splits the ring into a producer and a consumer that borrow it.
    ///
    /// The `&mut` borrow guarantees there is exactly one of each while they
    /// live. After both are dropped the ring can be split again and the new
    /// handles continue where the old ones stopped.
    pub fn split(&mut self) -> (Producer<T, N, &Self>, Consumer<T, N, &Self>) {
        let ring: &Self = self;
        (
            Producer {
                ring,
                _t: PhantomData,
            },
            Consumer {
                ring,
                _t: PhantomData,
            },
        )
    }

    /// Moves the ring to the heap and returns owned handles, which can be
    /// sent to threads that outlive the caller. The ring is freed when both
    /// handles are dropped.
    #[cfg(feature = "alloc")]
    pub fn split_arc(self) -> (Producer<T, N, ArcRing<T, N>>, Consumer<T, N, ArcRing<T, N>>) {
        let ring = alloc::sync::Arc::new(self);
        (
            Producer {
                ring: ring.clone(),
                _t: PhantomData,
            },
            Consumer {
                ring,
                _t: PhantomData,
            },
        )
    }

    fn slot(&self, index: usize) -> &Cell<MaybeUninit<T>> {
        &self.slots[index & Self::MASK]
    }
}

impl<T, const N: usize> Drop for Ring<T, N> {
    fn drop(&mut self) {
        // The private positions are exact even if a handle published less
        // than it wrote or read (see `Side`).
        let mut i = self.cons.pos.load(Ordering::Relaxed);
        let end = self.prod.pos.load(Ordering::Relaxed);
        while i != end {
            self.slot(i).with_mut(|p| {
                // SAFETY: `&mut self` means no handle exists, and every index
                // in `[consumer pos, producer pos)` holds a value that was
                // written and not yet read, so it is initialised and dropped
                // exactly once here.
                unsafe { (*p).assume_init_drop() }
            });
            i = i.wrapping_add(1);
        }
    }
}

/// A shared, heap-allocated ring, as returned by [`Ring::split_arc`].
#[cfg(feature = "alloc")]
pub type ArcRing<T, const N: usize> = alloc::sync::Arc<Ring<T, N>>;

/// The writing end of a [`Ring`]. `R` is `&Ring` or [`ArcRing`].
pub struct Producer<T, const N: usize, R: Deref<Target = Ring<T, N>>> {
    ring: R,
    _t: PhantomData<T>,
}

/// The reading end of a [`Ring`]. `R` is `&Ring` or [`ArcRing`].
pub struct Consumer<T, const N: usize, R: Deref<Target = Ring<T, N>>> {
    ring: R,
    _t: PhantomData<T>,
}

impl<T, const N: usize, R: Deref<Target = Ring<T, N>>> Producer<T, N, R> {
    /// Free slots, refreshing the cached consumer position only if the cache
    /// shows fewer than `want`.
    fn free(&self, tail: usize, want: usize) -> usize {
        let side = &self.ring.prod;
        let head = side.cached_other.load(Ordering::Relaxed);
        let free = N - tail.wrapping_sub(head);
        if free >= want {
            return free;
        }
        // Acquire pairs with the consumer's Release store of `head`: its
        // reads of those slots happen before we overwrite them.
        let head = self.ring.head.load(Ordering::Acquire);
        side.cached_other.store(head, Ordering::Relaxed);
        N - tail.wrapping_sub(head)
    }

    /// Writes `value` into the slot at `tail`.
    fn write(&mut self, tail: usize, value: T) {
        self.ring.slot(tail).with_mut(|p| {
            // SAFETY: the caller checked via `free` that slot `tail` is not in
            // the consumer's range `[head, tail)`, and only this handle writes,
            // so nothing else accesses the slot now.
            unsafe { (*p).write(value) };
        });
        self.ring
            .prod
            .pos
            .store(tail.wrapping_add(1), Ordering::Relaxed);
    }

    fn publish(&self, tail: usize) {
        // Release: the slot writes before this become visible to a consumer
        // that Acquire-loads the new tail.
        self.ring.tail.store(tail, Ordering::Release);
    }

    /// Pushes one value, or gives it back if the ring is full.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let tail = self.ring.prod.pos.load(Ordering::Relaxed);
        if self.free(tail, 1) == 0 {
            return Err(value);
        }
        self.write(tail, value);
        self.publish(tail.wrapping_add(1));
        Ok(())
    }

    /// Pushes items from `iter` while there is room, publishing them all with
    /// one store. Items are taken from the iterator only when they fit, so
    /// none are lost. Returns how many were pushed.
    pub fn push_iter<I: Iterator<Item = T>>(&mut self, iter: &mut I) -> usize {
        let start = self.ring.prod.pos.load(Ordering::Relaxed);
        let free = self.free(start, N);
        let mut tail = start;
        for _ in 0..free {
            let Some(value) = iter.next() else { break };
            self.write(tail, value);
            tail = tail.wrapping_add(1);
        }
        let pushed = tail.wrapping_sub(start);
        if pushed > 0 {
            self.publish(tail);
        }
        pushed
    }

    /// Pushes a prefix of `items` that fits, with one publication. Returns
    /// how many were pushed.
    pub fn push_slice(&mut self, items: &[T]) -> usize
    where
        T: Copy,
    {
        self.push_iter(&mut items.iter().copied())
    }

    /// Free slots as last seen. May be lower than the truth, never higher.
    pub fn free_slots(&self) -> usize {
        let tail = self.ring.prod.pos.load(Ordering::Relaxed);
        self.free(tail, N)
    }

    /// Ring capacity.
    pub const fn capacity(&self) -> usize {
        N
    }
}

impl<T, const N: usize, R: Deref<Target = Ring<T, N>>> Consumer<T, N, R> {
    /// Items ready, refreshing the cached producer position only if the
    /// cache shows fewer than `want`.
    fn ready(&self, head: usize, want: usize) -> usize {
        let side = &self.ring.cons;
        let tail = side.cached_other.load(Ordering::Relaxed);
        let ready = tail.wrapping_sub(head);
        if ready >= want {
            return ready;
        }
        // Acquire pairs with the producer's Release store of `tail`: its
        // slot writes happen before our reads.
        let tail = self.ring.tail.load(Ordering::Acquire);
        side.cached_other.store(tail, Ordering::Relaxed);
        tail.wrapping_sub(head)
    }

    fn read(&mut self, head: usize) -> T {
        // Advance the private position first: if a caller's callback later
        // panics, the ring will not drop this value a second time.
        self.ring
            .cons
            .pos
            .store(head.wrapping_add(1), Ordering::Relaxed);
        self.ring.slot(head).with_mut(|p| {
            // SAFETY: `ready` showed slot `head` inside `[head, tail)`, so the
            // producer initialised it and will not touch it until we publish
            // a head past it. The private position was advanced above, so the
            // value is moved out exactly once.
            unsafe { (*p).assume_init_read() }
        })
    }

    fn publish(&self, head: usize) {
        // Release: our reads of the slots happen before the producer, which
        // Acquire-loads `head`, reuses them.
        self.ring.head.store(head, Ordering::Release);
    }

    /// Pops one value, or `None` if the ring is empty.
    pub fn pop(&mut self) -> Option<T> {
        let head = self.ring.cons.pos.load(Ordering::Relaxed);
        if self.ready(head, 1) == 0 {
            return None;
        }
        let value = self.read(head);
        self.publish(head.wrapping_add(1));
        Some(value)
    }

    /// Pops up to `max` values, calling `f` on each, then publishes them all
    /// with one store. Returns how many were popped.
    pub fn pop_with(&mut self, max: usize, mut f: impl FnMut(T)) -> usize {
        let start = self.ring.cons.pos.load(Ordering::Relaxed);
        let n = self.ready(start, max.min(N)).min(max);
        let mut head = start;
        for _ in 0..n {
            let value = self.read(head);
            head = head.wrapping_add(1);
            f(value);
        }
        if n > 0 {
            self.publish(head);
        }
        n
    }

    /// Pops into the front of `out`, with one publication. Returns how many
    /// were written.
    pub fn pop_slice(&mut self, out: &mut [T]) -> usize
    where
        T: Copy,
    {
        let mut i = 0;
        self.pop_with(out.len(), |v| {
            out[i] = v;
            i += 1;
        })
    }

    /// Items ready as last seen. May be lower than the truth, never higher.
    pub fn ready_items(&self) -> usize {
        let head = self.ring.cons.pos.load(Ordering::Relaxed);
        self.ready(head, N)
    }

    /// Ring capacity.
    pub const fn capacity(&self) -> usize {
        N
    }
}

impl<T, const N: usize, R: Deref<Target = Ring<T, N>>> Drop for Consumer<T, N, R> {
    fn drop(&mut self) {
        // A panic inside `pop_with` can leave reads unpublished; publish them
        // so a producer waiting for room is not stuck.
        let head = self.ring.cons.pos.load(Ordering::Relaxed);
        self.publish(head);
    }
}

impl<T, const N: usize, R: Deref<Target = Ring<T, N>>> Drop for Producer<T, N, R> {
    fn drop(&mut self) {
        // Likewise for writes left unpublished by a panicking iterator.
        let tail = self.ring.prod.pos.load(Ordering::Relaxed);
        self.publish(tail);
    }
}
