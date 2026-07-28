//! #21/#56 keyed-CFG relay SCHEDULING — the pure decisions behind "does a config the
//! dashboard set actually reach the leaf it names".
//!
//! Extracted from `mode::RadioManager::broadcast_cached_configs` + `wifi::CfgCache` because
//! both had a failure mode that only a whole-fleet observation could see, and that no
//! observation on the board could: a config that is silently never relayed looks exactly
//! like a config that was relayed and applied, because nothing anywhere counts either one.
//!
//! ## The two bugs this module exists to make impossible
//!
//! **1. Tail starvation.** The relay walked the cache from index 0 every tick and emitted
//! every entry back-to-back. `CfgCache` is APPEND-ordered and never reorders, so a config
//! published *today* is always the LAST frame of the burst — and the head of the burst was
//! occupied by entries for node ids that are not boards any more. Whatever truncates a
//! 19-frame unspaced ESP-NOW burst therefore truncated the SAME entries every 10 s, forever.
//! A rotating cursor makes starvation structurally impossible: every slot is visited within
//! [`ticks_to_cover`] ticks no matter how few frames a given tick gets onto the air.
//!
//! **2. Cap overflow with no eviction.** `CfgCache::set` dropped a NEW `(id, key)` when full
//! and never evicted, so membership froze at whatever the first 16 arrivals were. Worse, a
//! retained config that is DELETED arrives as an empty payload and was cached as a permanent
//! empty-valued ghost — so merely *testing* a config burned a slot for good. Measured on the
//! live fleet 2026-07-28: 19 distinct `(id, key)` pairs wanted 16 slots. [`evict_slot`] lets a
//! real value reclaim a slot from a redundant clear, which is the correct priority — an empty
//! value means "keep current / board default", which is what a leaf that never hears it does
//! anyway, so a clear is the one entry that is safe to forget.
//!
//! Raising `CFG_CACHE_CAP` is deliberately NOT the fix: `vals` is
//! `[[u8; CFG_VALUE_MAX]; CAP]`, so +16 slots is ~1.3 KB of `.bss`, and esp-hal shrinks
//! `.stack` silently as `.bss` grows — the stack floor has ~2.2 KB of headroom, and spending
//! most of it here to paper over an eviction bug would trade a config bug for a stack bug.
//!
//! PURE: no esp-hal, no alloc, no HAL. Host-tested verbatim by
//! `experiments/cfg_relay_verify` (`#[path]`-include, like `wire`/`coexist`/`flood`).

/// Keyed-CFG frames emitted per relay tick (the ~10 s cadence in `main`).
///
/// The point is NOT airtime — 19 × ~80 B per 10 s is nothing. The point is that a bounded,
/// spaced emission cannot have a permanently-unreached tail: with a rotating cursor, coverage
/// is guaranteed even if the radio only ever gets the first frame or two of a tick out. 4 per
/// tick covers a full 16-slot cache in 4 ticks (~40 s), well inside the tolerance of an
/// edge-triggered, idempotent apply on the leaf side.
pub const CFG_RELAY_PER_TICK: usize = 4;

/// Ticks needed to visit every one of `count` slots at least once. The bound
/// `experiments/cfg_relay_verify` asserts against — the anti-starvation contract.
pub const fn ticks_to_cover(count: usize) -> usize {
    if count == 0 {
        0
    } else {
        count.div_ceil(CFG_RELAY_PER_TICK)
    }
}

/// A rotating cursor over the crown's `cfg_cache` slots.
///
/// Holds only "where the last tick stopped". Deliberately stores an INDEX and not an
/// `(id, key)`: the cache can shrink or be refilled in a different order under a crown
/// handover, and an index that is merely stale wraps harmlessly on the next tick, whereas a
/// remembered key that has vanished would need a search that could fail.
pub struct RelayCursor {
    next: usize,
}

impl Default for RelayCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayCursor {
    pub const fn new() -> Self {
        Self { next: 0 }
    }

    /// Fill `out` with up to [`CFG_RELAY_PER_TICK`] slot indices to relay this tick and
    /// return how many were written, advancing the cursor past them.
    ///
    /// Total and panic-free: `count == 0` writes nothing; a cursor left beyond a shrunken
    /// `count` wraps to 0; a `count` below `CFG_RELAY_PER_TICK` yields each slot exactly
    /// once (never the same slot twice in one tick, which would waste the tick's budget on
    /// a duplicate while another slot went unvisited).
    pub fn take(&mut self, count: usize, out: &mut [usize; CFG_RELAY_PER_TICK]) -> usize {
        if count == 0 {
            self.next = 0;
            return 0;
        }
        if self.next >= count {
            self.next = 0; // cache shrank (or first call) — resume from the top
        }
        let n = count.min(CFG_RELAY_PER_TICK);
        for k in 0..n {
            out[k] = (self.next + k) % count;
        }
        self.next = (self.next + n) % count;
        n
    }

    /// The next slot index this cursor will emit (test/observability only).
    pub fn peek(&self) -> usize {
        self.next
    }
}

/// Which slot a FULL cache may reuse for a new `(id, key)`: the first slot holding an EMPTY
/// value, or `None` if every slot carries a real config (the caller then drops and COUNTS it
/// — a dropped config must never be signalled by a log line alone, because release images are
/// serial-silent and the log is therefore not observability).
///
/// `lens` is the cache's per-slot value length, in slot order.
pub fn evict_slot(lens: &[u8]) -> Option<usize> {
    lens.iter().position(|&l| l == 0)
}
