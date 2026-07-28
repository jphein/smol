//! #21/#56 host guard for the keyed-CFG relay schedule. `#[path]`-includes the REAL
//! `net/cfgsched.rs` (no drift) and asserts the one property whose absence killed the entire
//! leaf control plane on 2026-07-28: **no cached config may be starved.**
//!
//! ## What went wrong, and why no existing test could have caught it
//!
//! Every Home Assistant control for a LEAF board was silently inert. The relay walked the
//! crown's append-ordered `cfg_cache` from slot 0 on every ~10 s tick and emitted every entry
//! back-to-back over ESP-NOW, unacknowledged. So the entries at the TAIL — which is always the
//! configs published most recently, i.e. exactly the ones a person just changed — were the ones
//! a truncated burst dropped, and it dropped the SAME ones every tick, forever. Measured: 37/37
//! DIAG samples and 44/44 status samples on a live leaf unchanged across a 13-minute window
//! covering ~78 relay ticks. Not lossy. Deterministic.
//!
//! Separately, `CfgCache` was full (19 wanted (id,key) pairs, 16 slots) and never evicted, so
//! membership froze at the first 16 arrivals — and a config that gets DELETED arrives as an
//! empty payload and used to occupy a slot permanently, so merely *testing* a control cost a
//! slot for good.
//!
//! Neither bug is reachable by a unit test of `CfgCache::set` or of `broadcast_config`: each of
//! those is individually correct. The bug lives in the SCHEDULE — in which entries get a turn,
//! over time. So that is what this asserts: coverage, from any starting state, as a property.
//!
//! Run: `cargo run --manifest-path experiments/cfg_relay_verify/Cargo.toml`

#[path = "../../../rust/clock/src/net/cfgsched.rs"]
mod cfgsched;

use cfgsched::*;

/// Run `ticks` relay ticks over a `count`-slot cache and return how many times each slot was
/// emitted. This is the whole contract: a slot with 0 emissions is a config a person set in
/// Home Assistant that the board will never hear.
fn coverage(cursor: &mut RelayCursor, count: usize, ticks: usize) -> Vec<usize> {
    let mut hits = vec![0usize; count];
    let mut out = [0usize; CFG_RELAY_MAX_BURST];
    for _ in 0..ticks {
        let n = cursor.take(count, &mut out);
        assert!(n <= CFG_RELAY_MAX_BURST, "a tick may never exceed the burst budget");
        assert!(n <= count, "a tick may never emit more frames than there are slots");
        for &i in out.iter().take(n) {
            assert!(i < count, "slot index {i} out of range for a {count}-slot cache");
            hits[i] += 1;
        }
    }
    hits
}

/// Consume the PRIMING tick so what follows measures the ROTATING CURSOR in steady state.
///
/// Load-bearing: priming sweeps the whole cache in one tick, so every coverage assertion below
/// would pass trivially on a fresh cursor and would no longer be testing the thing that broke.
/// The anti-starvation contract rests on the cursor, so the cursor is what gets tested.
fn warm(cursor: &mut RelayCursor, count: usize) {
    let mut out = [0usize; CFG_RELAY_MAX_BURST];
    let n = cursor.take(count, &mut out);
    assert_eq!(
        n,
        count.min(CFG_RELAY_MAX_BURST),
        "the first tick at count={count} must PRIME (full sweep), not emit a steady-state slice"
    );
}

fn main() {
    // ---- THE REGRESSION: no slot may be starved -------------------------------------------
    // The old relay always started at 0, so with a burst that only got the first K frames out,
    // slots >= K were emitted ZERO times no matter how long you waited. Assert the opposite for
    // every cache size the crown can hold: after `ticks_to_cover`, EVERY slot has had a turn.
    for count in 1..=16 {
        let mut c = RelayCursor::new();
        warm(&mut c, count); // measure the CURSOR, not the priming sweep
        let hits = coverage(&mut c, count, ticks_to_cover(count));
        for (i, &h) in hits.iter().enumerate() {
            assert!(
                h >= 1,
                "STARVED: slot {i} of {count} never relayed within ticks_to_cover({count}) = {} \
                 ticks — this is the 2026-07-28 bug: a config nobody will ever receive",
                ticks_to_cover(count)
            );
        }
    }

    // Starvation-freedom must hold from an ARBITRARY cursor position too, not just from fresh.
    // A crown that has been up for hours is never at cursor 0, and the bug only ever showed up
    // on a crown that had been running — a from-zero-only test would have passed all day.
    //
    // Reach each position by PRE-ROLLING `skip` ticks rather than by hunting for a target index:
    // the cursor advances by `min(count, PER_TICK)`, so for most `count` it only ever visits a
    // subset of indices, and a `while peek() != start` walk would spin forever. Every REACHABLE
    // state is covered by some `skip` in 0..count, which is exactly the set that matters.
    for count in 1..=16 {
        for skip in 0..count {
            let mut c = RelayCursor::new();
            warm(&mut c, count);
            let mut out = [0usize; CFG_RELAY_MAX_BURST];
            for _ in 0..skip {
                c.take(count, &mut out);
            }
            let start = c.peek();
            let hits = coverage(&mut c, count, ticks_to_cover(count));
            assert!(
                hits.iter().all(|&h| h >= 1),
                "STARVED from cursor {start} of {count}: {hits:?}"
            );
        }
    }

    // ---- fairness: nobody gets a second turn before everybody gets a first ----------------
    // A tick must not spend its budget re-sending a slot it already covered while another slot
    // waits. (A modulo cursor with count < PER_TICK is the easy way to get this wrong.)
    for count in 1..=16 {
        let mut c = RelayCursor::new();
        warm(&mut c, count);
        let hits = coverage(&mut c, count, ticks_to_cover(count));
        let (lo, hi) = (hits.iter().min().unwrap(), hits.iter().max().unwrap());
        assert!(
            hi - lo <= 1,
            "unfair coverage for {count} slots: {hits:?} (spread {lo}..{hi})"
        );
    }

    // A single tick must never emit the same slot twice — that wastes the tick's budget on a
    // duplicate while a different config goes unrelayed.
    for count in 1..=16 {
        let mut c = RelayCursor::new();
        let mut out = [0usize; CFG_RELAY_MAX_BURST];
        for _ in 0..(count * 3) {
            let n = c.take(count, &mut out);
            let mut seen = out[..n].to_vec();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(
                seen.len(),
                n,
                "duplicate slot within one tick ({count} slots): {:?}",
                &out[..n]
            );
        }
    }

    // ---- total / panic-free edges ---------------------------------------------------------
    // Empty cache (a crown that has drained no configs yet, or a fresh crown after a handover).
    let mut c = RelayCursor::new();
    let mut out = [0usize; CFG_RELAY_MAX_BURST];
    assert_eq!(c.take(0, &mut out), 0, "an empty cache emits nothing");
    assert_eq!(c.peek(), 0, "an empty cache resets the cursor");
    assert_eq!(ticks_to_cover(0), 0);

    // The cache SHRANK under the cursor (crown handover refills it smaller / differently
    // ordered). A stale index must wrap, never index out of bounds, and coverage must recover.
    let mut c = RelayCursor::new();
    warm(&mut c, 16);
    coverage(&mut c, 16, 3); // cursor now well past 2
    assert!(c.peek() > 2);
    let hits = coverage(&mut c, 2, ticks_to_cover(2) + 1);
    assert!(hits.iter().all(|&h| h >= 1), "coverage must survive a cache shrink: {hits:?}");

    // ---- PRIMING: a fresh crown, and a control somebody just added ------------------------
    // The handover case. A new crown's cfg_cache starts EMPTY, so until it is swept once NO leaf
    // config is reachable at all — measured live: a board took the crown at up=16 s reporting
    // cfgq=0/16. The first tick at a nonzero count must sweep EVERYTHING.
    for count in 1..=16 {
        let mut c = RelayCursor::new();
        let mut out = [0usize; CFG_RELAY_MAX_BURST];
        let n = c.take(count, &mut out);
        assert_eq!(n, count, "a fresh crown must PRIME all {count} slots in one tick, got {n}");
        let mut seen = out[..n].to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count, "a priming sweep must name every slot exactly once");
    }

    // ...and the tick AFTER priming must fall back to the steady-state budget, or the burst is
    // not an optimisation, it is the delivery mechanism — which is what we are moving away from.
    let mut c = RelayCursor::new();
    let mut out = [0usize; CFG_RELAY_MAX_BURST];
    assert_eq!(c.take(16, &mut out), 16, "prime");
    assert_eq!(
        c.take(16, &mut out),
        CFG_RELAY_PER_TICK,
        "the tick after priming must be steady-state, not another full sweep"
    );

    // GROWTH re-primes: somebody adds a control, the cache goes N -> N+1, and the new entry is
    // appended LAST. Under a pure cursor the one config a person is waiting on waits a full
    // rotation; priming on growth puts it on the air immediately.
    let mut c = RelayCursor::new();
    warm(&mut c, 8);
    assert_eq!(c.take(8, &mut out), CFG_RELAY_PER_TICK, "steady state at 8");
    let n = c.take(9, &mut out);
    assert_eq!(n, 9, "cache growth 8 -> 9 must re-prime a full sweep, got {n}");
    // And the newly-appended slot (index 8) must actually be in that sweep.
    assert!(out[..n].contains(&8), "the NEW entry must be in the priming sweep: {:?}", &out[..n]);

    // An empty cache RE-ARMS priming, so a crown that is demoted and later regains the role
    // primes again rather than trickling 4 slots per tick through the window where nothing works.
    let mut c = RelayCursor::new();
    warm(&mut c, 10);
    assert_eq!(c.take(10, &mut out), CFG_RELAY_PER_TICK, "steady state");
    assert_eq!(c.take(0, &mut out), 0, "demoted / cache cleared");
    assert_eq!(c.take(10, &mut out), 10, "re-promoted: must prime again");

    // A repeated identical count must NOT prime — otherwise every tick is a full burst and the
    // cursor is decorative.
    let mut c = RelayCursor::new();
    warm(&mut c, 12);
    for _ in 0..10 {
        assert_eq!(c.take(12, &mut out), CFG_RELAY_PER_TICK, "steady state must stay steady");
    }

    // ---- eviction: a real config outranks a redundant clear -------------------------------
    // A DELETED retained config arrives as an empty payload. It used to occupy a slot forever,
    // so a full cache could never admit a new control again — the silent death of the control
    // plane. An empty value means "keep current / board default", which is what a leaf that
    // never hears it does anyway, so it is the one entry safe to reclaim.
    assert_eq!(evict_slot(&[3, 0, 5, 7]), Some(1), "reclaim the cleared slot");
    assert_eq!(evict_slot(&[0, 0, 5]), Some(0), "first cleared slot wins (stable choice)");
    assert_eq!(
        evict_slot(&[3, 4, 5]),
        None,
        "every slot holds a real config → caller must DROP and COUNT, never silently overwrite"
    );
    assert_eq!(evict_slot(&[]), None, "empty slice is not a panic");

    // ---- the bound is honest --------------------------------------------------------------
    // ticks_to_cover must be the real ceiling, not an optimistic one: a caller reads it as
    // "worst-case seconds until a config lands / 10".
    for count in 1..=16 {
        let need = ticks_to_cover(count);
        let mut c = RelayCursor::new();
        warm(&mut c, count);
        let hits = coverage(&mut c, count, need.saturating_sub(1));
        if need > 1 {
            assert!(
                hits.iter().any(|&h| h == 0),
                "ticks_to_cover({count}) = {need} is LOOSE — coverage completed a tick early, \
                 so the advertised worst case is wrong"
            );
        }
    }

    println!(
        "cfg_relay_verify: OK — no starvation for 1..=16 slots from any cursor (measured in \
         STEADY STATE, priming consumed first); priming sweeps all slots on a fresh crown and on \
         cache growth, then falls back to PER_TICK={CFG_RELAY_PER_TICK}; MAX_BURST={CFG_RELAY_MAX_BURST}; \
         steady-state worst-case cover for a full 16-slot cache = {} ticks (~{} s)",
        ticks_to_cover(16),
        ticks_to_cover(16) * 10
    );
}
