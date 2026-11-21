//! Other architectures: no prefetch, a nanosecond clock for ticks, no timed
//! add chain.

pub(crate) const HAS_ADD_CHAIN: bool = false;

pub(crate) const ADDS_PER_ROUND: u64 = 1;

#[inline(always)]
pub(crate) fn prefetch_read(_ptr: *const u8) {}

pub(crate) fn ticks() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as u64
}

pub(crate) fn dependent_adds(_rounds: u64) {}
