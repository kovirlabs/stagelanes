//! AArch64: `prfm`, `CNTVCT_EL0` and a timed add chain.

use core::arch::asm;

pub(crate) const HAS_ADD_CHAIN: bool = true;

/// Additions per round of [`dependent_adds`].
pub(crate) const ADDS_PER_ROUND: u64 = 8;

#[inline(always)]
pub(crate) fn prefetch_read(ptr: *const u8) {
    // SAFETY: `prfm` is a hint. It never faults and has no architectural
    // effect, whatever the address.
    unsafe {
        asm!("prfm pldl1keep, [{0}]", in(reg) ptr, options(nostack, preserves_flags, readonly));
    }
}

#[inline(always)]
pub(crate) fn ticks() -> u64 {
    let t: u64;
    // SAFETY: reads the virtual counter, which Linux and macOS expose to user
    // code (EL0). No other effect.
    unsafe {
        asm!("mrs {0}, cntvct_el0", out(reg) t, options(nomem, nostack, preserves_flags));
    }
    t
}

/// Runs `rounds` rounds of eight dependent 1-cycle additions; see the x86-64
/// version.
#[inline(never)]
pub(crate) fn dependent_adds(rounds: u64) {
    if rounds == 0 {
        return;
    }
    let mut x: u64 = 0;
    // SAFETY: register-only arithmetic and a counted loop; touches no memory
    // and no stack. `n` is non-zero so the loop terminates.
    unsafe {
        asm!(
            "2:",
            "add {x}, {x}, #1",
            "add {x}, {x}, #1",
            "add {x}, {x}, #1",
            "add {x}, {x}, #1",
            "add {x}, {x}, #1",
            "add {x}, {x}, #1",
            "add {x}, {x}, #1",
            "add {x}, {x}, #1",
            "subs {n}, {n}, #1",
            "b.ne 2b",
            x = inout(reg) x,
            n = inout(reg) rounds => _,
            options(nomem, nostack),
        );
    }
    core::hint::black_box(x);
}
