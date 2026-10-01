//! `stackfree N` on the station's status line: the bytes at the bottom of the main task's stack
//! that nothing has written since the station started (tapstone#132, 2026-10-01).
//!
//! Both shrines rebooted with stack-overflow panics on a build whose stack gate passed, because the
//! gate measured one frame and the overflow was a call chain under it. The static gate
//! (`tools/check_stack_depth.py`) now sums the chain; this is its runtime witness. A board that
//! prints `stackfree 9000` for 90 minutes has had at least 9000 B to spare the whole time,
//! interrupts included, whatever the gate believed.
//!
//! How. [`paint`] fills `[_stack_end + GUARD_SKIP, sp - MARGIN)` with a sentinel; [`free`] counts
//! the sentinel words up from the bottom until the first one something overwrote.
//!
//! - **The guard word is skipped.** esp-rtos keeps a guard word `ESP_HAL_CONFIG_STACK_GUARD_OFFSET`
//!   (60) bytes above `_stack_end` and arms a write watchpoint on it. Painting it is a stack-overflow
//!   panic on the spot. That is very likely what #398 saw on the S3 ("re-painting after
//!   esp_hal::init crashed the box into a 99-boot exception loop"). The first `GUARD_SKIP` bytes
//!   are never painted or counted, so `free` under-reports by at most that much. That is the safe
//!   direction: those bytes are the ones the guard fires in anyway.
//! - **Interrupts are masked while painting.** On esp-rtos an interrupt runs on the interrupted
//!   task's stack, below `sp`. Unmasked, one could push its frame where the loop is about to write.
//!   The paint is ~10k word stores, tens of microseconds.
//! - **Nothing at or above `sp - MARGIN` is touched**, so no live frame is (see `crate::stack_paint`
//!   for the longer argument; this is the same write, kept separate because `paint` is a bench
//!   feature and this one ships in the station).

/// Sentinel word (the same as `crate::stack_paint`'s, so a dump reads the same).
const SENTINEL: u32 = 0xA5A5_A5A5;
/// Bytes under the live frame left unpainted (the compiler's scratch below our locals).
const MARGIN: usize = 256;
/// Bytes above `_stack_end` neither painted nor counted: the esp-rtos guard word sits at +60, and
/// 256 keeps clear of it with room if the offset is ever raised.
const GUARD_SKIP: usize = 256;

unsafe extern "C" {
    /// Low address of the main task's stack (it grows down towards this).
    static _stack_end: u8;
}

fn base() -> usize {
    (core::ptr::addr_of!(_stack_end) as usize + GUARD_SKIP + 3) & !3
}

/// The address of a local in this frame: a stand-in for `sp`. Not inlined, or the frame it
/// measures would move.
#[inline(never)]
fn frame_floor() -> usize {
    let probe = 0u32;
    core::ptr::addr_of!(probe) as usize
}

/// The painted range's top. Fixed at the first paint, so `free` never scans past what was painted.
static TOP: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Paint the unused stack. Once, from the station's constructor, after the radio's boot peak.
#[inline(never)]
pub fn paint() {
    use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex};
    let base = base();
    let top = frame_floor().saturating_sub(MARGIN) & !3;
    CriticalSectionRawMutex::new().lock(|| {
        let mut a = base;
        while a + 4 <= top {
            // Volatile: to the compiler these stores are dead; the point is that `free` reads them.
            unsafe { core::ptr::write_volatile(a as *mut u32, SENTINEL) };
            a += 4;
        }
    });
    TOP.store(top.max(base), core::sync::atomic::Ordering::Relaxed);
}

/// Bytes still holding the sentinel, counted up from the bottom. `None` before [`paint`].
pub fn free() -> Option<u32> {
    let top = TOP.load(core::sync::atomic::Ordering::Relaxed);
    if top == 0 {
        return None;
    }
    let base = base();
    let mut a = base;
    while a < top && unsafe { core::ptr::read_volatile(a as *const u32) } == SENTINEL {
        a += 4;
    }
    Some((a - base) as u32)
}
