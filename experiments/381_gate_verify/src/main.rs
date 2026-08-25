//! #381 crown SELF-OTA gate host guard. `#[path]`-includes the REAL `net/otagate.rs` (no drift)
//! and pins the bug found live on the 917 roll (2026-08-02): a permanently-deaf leaf's armed
//! install held BOTH self-install gates on indefinitely, so the crown SKIPPED its own update —
//! indistinguishable from being up to date, so the roll reported success with the crown on the old
//! build. `cargo run` — panics on failure.
//!
//! The two properties that must not drift apart:
//!   1. the #40/#3 order survives — a HEALTHY relay still makes the crown wait;
//!   2. an install that has repeatedly failed BEFORE reaching a leaf stops being a reason to wait.
//! A change that satisfies only (1) restores the lock; only (2) lets the crown reboot into its own
//! install mid-relay. Both are asserted here.

#[path = "../../../rust/clock/src/net/otagate.rs"]
mod otagate;

use otagate::*;

fn main() {
    // ---- 1. Nothing pending: the ordinary case, the crown installs -----------------------------
    assert!(
        crown_may_self_install(false, false, 0),
        "an idle crown with no leaf install must run its own"
    );

    // ---- 2. The #40 order survives — a HEALTHY relay still blocks -------------------------------
    // Below the threshold every combination of the two legacy gates must still suppress. This is
    // the half that a naive "just release the gate" fix would break, letting the crown reboot into
    // its own install while a relay it armed is genuinely in flight.
    for n in 0..LEAF_OTA_SELF_GATE_RETRIES {
        assert!(
            !crown_may_self_install(true, false, n),
            "armed relay (retry={n}) must still suppress the crown's own install"
        );
        assert!(
            !crown_may_self_install(false, true, n),
            "outstanding retained install (retry={n}) must still suppress"
        );
        assert!(
            !crown_may_self_install(true, true, n),
            "both gates set (retry={n}) must still suppress"
        );
    }

    // ---- 3. #381: a deaf leaf's install loses its veto over BOTH gates ---------------------------
    // Releasing only the RAM latch is not enough and this asserts why: a deaf leaf pins
    // `leaf_installs_outstanding` through its RETAINED topic, which is what forced the 2026-08-02
    // mitigation to clear that topic by hand. Both must release together or the crown stays stuck.
    assert!(
        crown_may_self_install(true, true, LEAF_OTA_SELF_GATE_RETRIES),
        "at the threshold the veto expires and the crown proceeds despite both gates"
    );
    assert!(
        crown_may_self_install(true, true, 35),
        "the observed id5/id50 state (retry=35) must not hold the crown"
    );
    assert!(
        crown_may_self_install(true, true, u8::MAX),
        "the saturated counter must not wrap back into suppression"
    );

    // ---- 4. The threshold is exactly where the constant says --------------------------------------
    assert!(
        !leaf_veto_expired(LEAF_OTA_SELF_GATE_RETRIES - 1),
        "one short of the threshold still vetoes"
    );
    assert!(
        leaf_veto_expired(LEAF_OTA_SELF_GATE_RETRIES),
        "the threshold itself releases"
    );

    // ---- 5. The release is monotone in the retry count ------------------------------------------
    // Guards against a future "window" refactor (release between N and M, suppress again after)
    // that would make the lock reappear at high retry counts — the precise regime where the bug
    // was observed. Once released, always released, for every gate combination.
    for n in LEAF_OTA_SELF_GATE_RETRIES..=u8::MAX {
        assert!(
            crown_may_self_install(true, true, n),
            "retry={n} is past the threshold and must stay released"
        );
    }

    // ---- 6. The counter below the threshold is not itself a release ------------------------------
    // i.e. the escape hatch cannot be reached by the legacy gates alone being clear — that path is
    // already covered by case 1, but this pins that `leaf_veto_expired` is the ONLY override.
    assert!(
        !leaf_veto_expired(0),
        "a fresh counter must not read as an expired veto"
    );

    println!(
        "381_gate_verify: OK — crown self-OTA gate ({} pre-relay retries to veto expiry)",
        LEAF_OTA_SELF_GATE_RETRIES
    );
}
