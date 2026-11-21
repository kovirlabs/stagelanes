//! Submission and completion ring pair.

use core::ops::Deref;

use crate::spsc::{Consumer, Producer, Ring};

/// A submission ring and a completion ring of equal capacity.
///
/// The client submits requests of type `S` and polls completions of type
/// `C`; the server takes requests and pushes completions. The client never
/// has more than `N` requests outstanding, so as long as the server pushes
/// exactly one completion per request, the completion ring always has room
/// and the server never waits on the client.
pub struct RingPair<S, C, const N: usize> {
    sq: Ring<S, N>,
    cq: Ring<C, N>,
}

impl<S, C, const N: usize> Default for RingPair<S, C, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, C, const N: usize> RingPair<S, C, N> {
    /// Creates an empty pair.
    pub fn new() -> Self {
        Self {
            sq: Ring::new(),
            cq: Ring::new(),
        }
    }

    /// Splits into a client and a server that borrow the pair.
    ///
    /// The outstanding-request count starts at zero, so split a pair whose
    /// rings are empty.
    #[allow(clippy::type_complexity)]
    pub fn split(
        &mut self,
    ) -> (
        Client<S, C, N, &Ring<S, N>, &Ring<C, N>>,
        Server<S, C, N, &Ring<S, N>, &Ring<C, N>>,
    ) {
        let (sq_p, sq_c) = self.sq.split();
        let (cq_p, cq_c) = self.cq.split();
        (
            Client {
                sq: sq_p,
                cq: cq_c,
                outstanding: 0,
            },
            Server { sq: sq_c, cq: cq_p },
        )
    }

    /// Owned version of [`RingPair::split`].
    #[cfg(feature = "alloc")]
    #[allow(clippy::type_complexity)]
    pub fn split_arc(
        self,
    ) -> (
        Client<S, C, N, crate::ArcRing<S, N>, crate::ArcRing<C, N>>,
        Server<S, C, N, crate::ArcRing<S, N>, crate::ArcRing<C, N>>,
    ) {
        let (sq_p, sq_c) = self.sq.split_arc();
        let (cq_p, cq_c) = self.cq.split_arc();
        (
            Client {
                sq: sq_p,
                cq: cq_c,
                outstanding: 0,
            },
            Server { sq: sq_c, cq: cq_p },
        )
    }
}

/// The submitting side of a [`RingPair`].
pub struct Client<S, C, const N: usize, RS, RC>
where
    RS: Deref<Target = Ring<S, N>>,
    RC: Deref<Target = Ring<C, N>>,
{
    sq: Producer<S, N, RS>,
    cq: Consumer<C, N, RC>,
    outstanding: usize,
}

impl<S, C, const N: usize, RS, RC> Client<S, C, N, RS, RC>
where
    RS: Deref<Target = Ring<S, N>>,
    RC: Deref<Target = Ring<C, N>>,
{
    /// Submits one request, or gives it back if `N` are already outstanding
    /// or the submission ring is full.
    pub fn submit(&mut self, request: S) -> Result<(), S> {
        if self.outstanding == N {
            return Err(request);
        }
        self.sq.push(request)?;
        self.outstanding += 1;
        Ok(())
    }

    /// Submits as many requests from `iter` as the limit allows, with one
    /// publication. Returns how many were submitted.
    pub fn submit_iter<I: Iterator<Item = S>>(&mut self, iter: &mut I) -> usize {
        let room = N - self.outstanding;
        let n = self.sq.push_iter(&mut iter.by_ref().take(room));
        self.outstanding += n;
        n
    }

    /// Takes up to `max` completions, calling `f` on each. Returns how many.
    pub fn poll(&mut self, max: usize, f: impl FnMut(C)) -> usize {
        let n = self.cq.pop_with(max, f);
        self.outstanding -= n;
        n
    }

    /// Requests submitted and not yet completed.
    pub fn outstanding(&self) -> usize {
        self.outstanding
    }
}

/// The serving side of a [`RingPair`].
pub struct Server<S, C, const N: usize, RS, RC>
where
    RS: Deref<Target = Ring<S, N>>,
    RC: Deref<Target = Ring<C, N>>,
{
    sq: Consumer<S, N, RS>,
    cq: Producer<C, N, RC>,
}

impl<S, C, const N: usize, RS, RC> Server<S, C, N, RS, RC>
where
    RS: Deref<Target = Ring<S, N>>,
    RC: Deref<Target = Ring<C, N>>,
{
    /// Takes up to `max` requests, calling `f` on each. Returns how many.
    pub fn take(&mut self, max: usize, f: impl FnMut(S)) -> usize {
        self.sq.pop_with(max, f)
    }

    /// Pushes one completion. Fails only if the server has pushed more
    /// completions than it took requests.
    pub fn complete(&mut self, completion: C) -> Result<(), C> {
        self.cq.push(completion)
    }

    /// Pushes completions from `iter` with one publication. Returns how many.
    pub fn complete_iter<I: Iterator<Item = C>>(&mut self, iter: &mut I) -> usize {
        self.cq.push_iter(iter)
    }
}
