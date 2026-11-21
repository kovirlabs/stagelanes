//! x86-64: `prefetcht0`, `rdtsc` and a timed add chain.

use core::arch::asm;

pub(crate) const HAS_ADD_CHAIN: bool = true;

/// Additions per round of [`dependent_adds`].
pub(crate) const ADDS_PER_ROUND: u64 = 8;

#[inline(always)]
pub(crate) fn prefetch_read(ptr: *const u8) {
    // SAFETY: `prefetcht0` is a hint. It never faults, reads no data into
    // registers and has no architectural effect, whatever the address.
    unsafe {
        asm!("prefetcht0 [{0}]", in(reg) ptr, options(nostack, preserves_flags, readonly));
    }
}

#[inline(always)]
pub(crate) fn ticks() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: `rdtsc` reads the time-stamp counter into edx:eax and has no
    // other effect. It is available on every x86-64 CPU.
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Runs `rounds` rounds of eight dependent 1-cycle additions. The loop
/// counter's decrement and branch run alongside the chain, so a round costs
/// eight cycles on cores that retire one dependent add per cycle.
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
            "add {x}, 1",
            "add {x}, 1",
            "add {x}, 1",
            "add {x}, 1",
            "add {x}, 1",
            "add {x}, 1",
            "add {x}, 1",
            "add {x}, 1",
            "dec {n}",
            "jnz 2b",
            x = inout(reg) x,
            n = inout(reg) rounds => _,
            options(nomem, nostack),
        );
    }
    core::hint::black_box(x);
}
