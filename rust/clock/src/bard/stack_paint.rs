//! bard (#300): stack high-water measurement for the bench (feature `stack-paint`, OFF by
//! default and NEVER in the canonical fleet list).
//!
//! T9.5 restored the runtime stack to 14240 B and gated the build at 12288 B, but "14240 should
//! be enough" is still an assumption. This turns it into a number: fill the unused stack with a
//! sentinel at boot, run a story, then find how far down the sentinel was overwritten.
//!
//! WHY THIS IS NOT UB. The paint writes ONLY to `[_stack_end, sp - MARGIN)` — memory strictly
//! BELOW the current frame, which holds no live object: it is the space future frames will use,
//! which is exactly what we want to observe. Nothing at or above `sp` is touched, so no local,
//! spill slot or saved register can be clobbered. `MARGIN` also absorbs two things worth naming:
//! the compiler's own scratch just below our locals, and an interrupt frame arriving mid-paint
//! (it lands just under `sp`, inside the margin, so neither it nor our writes corrupt the other).
//! An excursion deeper than `sp - MARGIN` is real usage and is measured; one that stays inside
//! the margin is missed, understating the result by at most `MARGIN` — the safe direction for a
//! headroom check is to under-report free space, and this over-reports usage instead.

/// Sentinel word. Distinctive, non-zero, and not a plausible pointer or small integer.
pub const SENTINEL: u32 = 0xA5A5_A5A5;
/// Bytes below the live frame left unpainted (compiler scratch + room for an interrupt frame).
pub const MARGIN: usize = 256;

/// Bytes still holding the sentinel, counting up from the bottom of the stack region.
///
/// The scanner, kept pure so it is host-testable: the firmware calls this with a slice of the
/// region it actually painted, and the test calls it with a synthetic buffer.
pub fn untouched_bytes(painted: &[u32]) -> usize {
    painted
        .iter()
        .position(|&w| w != SENTINEL)
        .unwrap_or(painted.len())
        * core::mem::size_of::<u32>()
}

/// Device-only half: the linker symbols and the raw writes. Gated out of the host lib, where
/// `_stack_end` would be an unresolved symbol.
#[cfg(not(feature = "hostsim"))]
mod device {
    use super::{untouched_bytes, MARGIN, SENTINEL};

    unsafe extern "C" {
        /// Low address of the stack region (it grows DOWN from `_stack_start`).
        static _stack_end: u8;
        /// High address — where `sp` starts.
        static _stack_start: u8;
    }

    /// `(low, high)` addresses of the stack region.
    fn region() -> (usize, usize) {
        (
            core::ptr::addr_of!(_stack_end) as usize,
            core::ptr::addr_of!(_stack_start) as usize,
        )
    }

    /// Total stack region size in bytes.
    pub fn region_bytes() -> u32 {
        let (low, high) = region();
        high.saturating_sub(low) as u32
    }

    /// Address of a local in THIS frame — a portable stand-in for reading `sp`, and the reason
    /// `#[inline(never)]` matters: inlining would move the frame we are measuring against.
    #[inline(never)]
    fn frame_floor() -> usize {
        let probe = 0u32;
        core::ptr::addr_of!(probe) as usize
    }

    /// Fill the unused stack with [`SENTINEL`]. Call as early in `main` as possible, before the
    /// radio brings up deep call chains. See the module doc for why this is sound.
    #[inline(never)]
    pub fn paint() {
        let (low, _) = region();
        let top = frame_floor().saturating_sub(MARGIN);
        // Round the base up to a word so the writes stay aligned even if the symbol moves.
        let base = (low + 3) & !3usize;
        let mut addr = base;
        while addr + 4 <= top {
            // Volatile: from the compiler's view these writes are dead — the whole point is that
            // something else observes them later.
            unsafe { core::ptr::write_volatile(addr as *mut u32, SENTINEL) };
            addr += 4;
        }
    }

    /// Bytes of stack used, measured from the top of the region.
    ///
    /// Scans only `[low, sp - MARGIN)`: the range we painted, below the live frame, so forming a
    /// slice over it aliases nothing.
    pub fn high_water() -> u32 {
        let (low, high) = region();
        let base = (low + 3) & !3usize;
        let top = frame_floor().saturating_sub(MARGIN).max(base);
        let words = (top - base) / core::mem::size_of::<u32>();
        let painted = unsafe { core::slice::from_raw_parts(base as *const u32, words) };
        (high.saturating_sub(low)).saturating_sub(untouched_bytes(painted)) as u32
    }
}

#[cfg(not(feature = "hostsim"))]
pub use device::{high_water, paint, region_bytes};
