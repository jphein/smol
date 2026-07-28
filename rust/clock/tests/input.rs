//! Host tests for the shared BOOT-button gesture machine (`src/input.rs`).
//!
//! Gated on `hostsim` like the bard tests, and run the same way — add `--test input`:
//! `cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu
//!  --lib --test bard --test input`
//!
//! Why these exist: this is the input EVERY screen shares (menu, Snake, the Bard's pause, a page
//! turn), all of its rules are timing, and timing is what pressing a button at the bench cannot
//! systematically check — JP's "a short tap does not register" was a 40 ms floor nobody could see.
//! `Gesture` is HAL-free precisely so the awkward timelines can be replayed here: taps shorter than
//! one poll, holds observed only on release because the crown's sub-tick stretched, bounce bursts,
//! and deliberate double taps.
//!
//! Every test asserts only what `poll` REPORTS. The phase is private on purpose: a test that
//! inspected it would keep passing through a refactor that broke the button.
#![cfg(feature = "hostsim")]
use clock::input::{Gesture, Press, DEBOUNCE_MS, LONG_PRESS_MS, SETTLE_MS};

/// The real poll cadence (`main::SUBTICK_MS`), which is what makes the constants mean anything.
const SUBTICK: u64 = 20;

/// Drive a timeline of `(pressed, now_ms)` samples and collect everything reported.
fn run(g: &mut Gesture, timeline: &[(bool, u64)]) -> std::vec::Vec<Press> {
    timeline
        .iter()
        .filter_map(|&(pressed, t)| g.poll(pressed, t))
        .collect()
}

/// Sample every `SUBTICK` from `t0` while `pressed`, for `ms`, returning the next timestamp.
fn hold(g: &mut Gesture, out: &mut std::vec::Vec<Press>, pressed: bool, t0: u64, ms: u64) -> u64 {
    let mut t = t0;
    while t < t0 + ms {
        if let Some(p) = g.poll(pressed, t) {
            out.push(p);
        }
        t += SUBTICK;
    }
    t
}

#[test]
fn a_tap_shorter_than_one_poll_still_registers() {
    // THE BUG (JP, 2026-07-27): the press was observed on one poll and gone by the next, and the
    // machine wrote it off as bounce — so any tap under ~40 ms silently did nothing.
    let mut g = Gesture::new();
    assert_eq!(run(&mut g, &[(true, 0), (false, SUBTICK)]), [Press::Short]);

    // Even at the extreme: seen once, released 1 ms later (a sub-millisecond contact the sampler
    // happened to catch). Still a tap — anything the sampler sees at all is a tap.
    let mut g = Gesture::new();
    assert_eq!(run(&mut g, &[(true, 0), (false, 1)]), [Press::Short]);
    assert!(DEBOUNCE_MS < SUBTICK, "the settle must resolve on the very next poll");
}

#[test]
fn a_normal_tap_reports_exactly_one_short() {
    // ~100 ms press, sampled every 20 ms: settles, times the hold, reports on release. The "exactly
    // one" is the part that matters — a Short that double-reported would pause and resume the Bard
    // in the same press, i.e. look like nothing happened.
    let mut g = Gesture::new();
    let mut out = std::vec::Vec::new();
    let t = hold(&mut g, &mut out, true, 0, 100);
    if let Some(p) = g.poll(false, t) {
        out.push(p);
    }
    assert_eq!(out, [Press::Short]);
}

#[test]
fn a_long_hold_fires_once_while_held_and_swallows_the_release() {
    let mut g = Gesture::new();
    let mut out = std::vec::Vec::new();
    // Held well past the threshold: Long must fire the instant it is crossed (so "back to the menu"
    // feels immediate) and exactly once, however many polls follow.
    let t = hold(&mut g, &mut out, true, 0, LONG_PRESS_MS + 200);
    assert_eq!(out, [Press::Long], "Long must fire once, while still held");
    // The release must NOT also report a tap.
    assert_eq!(g.poll(false, t), None, "the release after a Long must be swallowed");
}

#[test]
fn a_hold_seen_only_on_release_is_still_long() {
    // The crown's sub-tick stretches during a WiFi burst (main.rs's HARDWARE-WATCH note), so a
    // deliberate hold can cross 700 ms with NO poll inside the window. Classifying by "did we get a
    // poll in time" would report it as a tap — which in the Bard means pausing instead of leaving to
    // the menu, i.e. the user cannot get out of the screen.
    let mut g = Gesture::new();
    let out = run(
        &mut g,
        &[
            (true, 0),
            (true, SUBTICK),                 // settles, hold timing starts at 0
            (false, LONG_PRESS_MS + 100),    // the next poll the burst allowed
        ],
    );
    assert_eq!(out, [Press::Long], "a 800 ms hold must be Long even if only its release was polled");

    // And the boundary in the other direction: 1 ms under the threshold is still a tap.
    let mut g = Gesture::new();
    let out = run(&mut g, &[(true, 0), (true, SUBTICK), (false, LONG_PRESS_MS - 1)]);
    assert_eq!(out, [Press::Short]);
}

#[test]
fn release_bounce_cannot_report_a_phantom_second_tap() {
    // The hazard the new lockout exists for. Accepting a fast tap means a single sampled low is
    // enough to report one — so contact bounce on RELEASE, if a sample catches it, would look like a
    // fresh press. In the Bard that phantom would resume the narration the real press just paused.
    let mut g = Gesture::new();
    let mut out = std::vec::Vec::new();
    out.extend(run(&mut g, &[(true, 0), (true, 20), (false, 40)])); // real tap
    assert_eq!(out, [Press::Short]);
    // Bounce burst inside the lockout: several samples, both levels, all ignored.
    assert_eq!(run(&mut g, &[(true, 45), (false, 50), (true, 55), (false, 60)]), []);
    // A real press once the window has passed still works — the lockout must not be sticky.
    let t = 40 + SETTLE_MS;
    assert_eq!(run(&mut g, &[(true, t), (false, t + SUBTICK)]), [Press::Short]);
}

#[test]
fn a_press_arriving_during_the_lockout_is_not_lost_forever() {
    // The subtle way a lockout goes wrong: if leaving it required seeing a RELEASED level, a button
    // pressed during the window and still held afterwards would strand the machine — the press would
    // never be seen, and nothing would work again until the user let go and pressed a third time.
    let mut g = Gesture::new();
    assert_eq!(run(&mut g, &[(true, 0), (false, 20)]), [Press::Short]);
    // Press again inside the lockout and KEEP holding it, long past the window.
    let mut out = std::vec::Vec::new();
    out.extend(run(&mut g, &[(true, 30)])); // ignored (still locked out)
    let t = hold(&mut g, &mut out, true, 20 + SETTLE_MS, LONG_PRESS_MS + 100);
    assert_eq!(out, [Press::Long], "a held press must be picked up when the lockout expires");
    assert_eq!(g.poll(false, t), None);
}

#[test]
fn deliberate_double_taps_both_report() {
    // A human double-tap is ~150 ms apart; the lockout is 40 ms, so both must land. (Snake and the
    // page-turn both depend on repeated taps being repeatable.)
    let mut g = Gesture::new();
    let mut out = std::vec::Vec::new();
    for i in 0..4u64 {
        let base = i * 150;
        out.extend(run(&mut g, &[(true, base), (true, base + 20), (false, base + 60)]));
    }
    assert_eq!(out, [Press::Short; 4], "four taps at 150 ms must report four Shorts");
    assert!(
        SETTLE_MS < 150,
        "the lockout must stay well under a human double-tap interval"
    );
}

#[test]
fn an_idle_button_reports_nothing_however_long_it_idles() {
    // The boring invariant that would make everything else moot: no spontaneous gestures. Includes
    // the far end of the clock, where a saturating_sub bug would surface.
    let mut g = Gesture::new();
    for t in (0..5_000).step_by(SUBTICK as usize) {
        assert_eq!(g.poll(false, t), None);
    }
    assert_eq!(g.poll(false, u64::MAX), None);
    // A monotonic clock that appears to go BACKWARDS (a caller bug, but this must not panic or fire).
    let mut g = Gesture::new();
    assert_eq!(run(&mut g, &[(true, 1_000), (true, 500), (false, 400)]), [Press::Short]);
}
