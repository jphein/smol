//! Host verification of the PURE #164 per-peer link-quality (ETX) core. Includes the
//! real `net/etx.rs` verbatim (#[path], no drift) and exercises the reach shift-register
//! THROUGH its observable output — the 0..=255 cost mapping (0 = perfect, 255 =
//! INFINITY/unheard), the recency weighting, and monotonicity. Run: `cargo run` — panics
//! on failure.
//!
//! Behaviour-focused: asserts SENTINELS (fresh = INFINITY, all-heard ≈ 0) and ORDERING
//! (recent-good < old-good; more-delivered < less-delivered) rather than exact internal
//! register/cost values, so a re-tuned mapping that preserves the contract stays green.

#[path = "../../../rust/clock/src/net/etx.rs"]
mod etx;

use etx::{LinkQuality, INFINITY};

/// Build a LinkQuality by ticking a hello-reception pattern (index 0 = oldest tick).
fn cost_of(pattern: &[bool]) -> u8 {
    let mut lq = LinkQuality::new();
    for &heard in pattern {
        lq.tick(heard);
    }
    lq.cost()
}

fn main() {
    // --- new(): an unheard peer costs INFINITY ----------------------------
    assert_eq!(LinkQuality::new().cost(), INFINITY, "fresh/unheard peer costs INFINITY");

    // --- a single miss from empty stays unheard (decay shifts in a 0) ------
    assert_eq!(cost_of(&[false]), INFINITY, "one miss from empty leaves the register empty");

    // --- one heard hello: cost drops below INFINITY -----------------------
    let one = cost_of(&[true]);
    assert!(one < INFINITY, "one heard hello ⇒ cost < INFINITY (got {one})");

    // --- perfect link: 16 heard ⇒ near-zero cost --------------------------
    let full = cost_of(&[true; 16]);
    assert!(full <= 2, "an all-heard link is ~perfect (cost <= 2), got {full}");

    // --- decay to silence: heard, then 16 misses ⇒ back to INFINITY -------
    let mut lq = LinkQuality::new();
    for _ in 0..16 {
        lq.tick(true);
    }
    for _ in 0..16 {
        lq.tick(false);
    }
    assert_eq!(lq.cost(), INFINITY, "16 consecutive misses fully decay a link back to INFINITY");

    // --- recency weighting: recent-good beats old-good (the whole point) ---
    // recent_good: 8 misses THEN 8 heard  ⇒ the 8 set bits are the most-recent slots.
    // old_good:    8 heard  THEN 8 misses ⇒ those bits shifted down to the oldest slots.
    let recent_good = cost_of(&[
        false, false, false, false, false, false, false, false, true, true, true, true, true,
        true, true, true,
    ]);
    let old_good = cost_of(&[
        true, true, true, true, true, true, true, true, false, false, false, false, false, false,
        false, false,
    ]);
    assert!(
        recent_good < old_good,
        "recent hellos matter more than old ones (recency weighting): recent={recent_good} old={old_good}"
    );

    // --- monotonicity: more recent delivery ⇒ strictly lower cost ----------
    let none = cost_of(&[false; 16]);
    let quarter = cost_of(&[
        false, false, false, false, false, false, false, false, false, false, false, false, true,
        true, true, true,
    ]); // recent 4 heard
    let half = cost_of(&[
        false, false, false, false, false, false, false, false, true, true, true, true, true, true,
        true, true,
    ]); // recent 8 heard
    assert_eq!(none, INFINITY, "zero delivery is INFINITY");
    assert!(
        full < half && half < quarter && quarter < none,
        "cost strictly decreases as recent delivery improves: full={full} half={half} quarter={quarter} none={none}"
    );

    // --- const-constructibility (must live in a .bss Node with no heap) ----
    const _CONST_OK: LinkQuality = LinkQuality::new();

    println!("etx_verify: all assertions passed");
}
