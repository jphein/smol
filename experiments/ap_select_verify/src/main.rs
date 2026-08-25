//! #217 rung-3 host guard. `#[path]`-includes the REAL `net/coexist.rs` (no drift) and asserts
//! the co-channel selector + the never-crownless strand-guard state machine. `cargo run`.

#[path = "../../../rust/clock/src/net/coexist.rs"]
mod coexist;

use coexist::*;

fn ap(b: u8, ch: u8, rssi: i8) -> ApView {
    ApView { bssid: [b, b, b, b, b, b], channel: ch, rssi }
}

fn main() {
    const MESH: u8 = 6;

    // ---- selector -------------------------------------------------------------------------
    // 1. co-channel present → pick it EVEN THOUGH stronger ch1 APs exist (the exact id5 bug).
    let scan = [ap(1, 1, -67), ap(2, 1, -69), ap(3, 6, -60)];
    assert_eq!(
        select_crown_ap(&scan, MESH, None),
        CrownApDecision::CoChannel { bssid: [3; 6], ch: 6 },
        "prefer the ch6 AP over stronger ch1 APs"
    );
    // 2. best-RSSI AMONG co-channel.
    let scan2 = [ap(3, 6, -70), ap(4, 6, -58), ap(5, 6, -75)];
    assert_eq!(
        select_crown_ap(&scan2, MESH, None),
        CrownApDecision::CoChannel { bssid: [4; 6], ch: 6 },
        "best-rssi co-channel"
    );
    // 2b. STABLE tie-break: equal-RSSI co-channel APs → the SAME (lowest-bssid) pick regardless of
    // scan order, so two crown candidates never diverge/oscillate (nebula-ota review item).
    let tie_a = [ap(2, 6, -60), ap(5, 6, -60)];
    let tie_b = [ap(5, 6, -60), ap(2, 6, -60)]; // reversed scan order
    assert_eq!(
        select_crown_ap(&tie_a, MESH, None),
        select_crown_ap(&tie_b, MESH, None),
        "tie-break is order-independent"
    );
    assert_eq!(
        select_crown_ap(&tie_a, MESH, None),
        CrownApDecision::CoChannel { bssid: [2; 6], ch: 6 },
        "tie → deterministic lowest bssid"
    );
    // 3. only off-channel → OffChannelFallback (strand signal), best rssi.
    let scan3 = [ap(1, 1, -67), ap(2, 11, -55)];
    assert_eq!(
        select_crown_ap(&scan3, MESH, None),
        CrownApDecision::OffChannelFallback { bssid: [2; 6], ch: 11 },
        "no co-channel → best off-channel"
    );
    // 4. empty → NoAp.
    assert_eq!(select_crown_ap(&[], MESH, None), CrownApDecision::NoAp, "no aps");
    // 5. co-channel below the usable floor is excluded → off-channel fallback.
    let scan5 = [ap(3, 6, -88), ap(1, 1, -60)];
    assert_eq!(
        select_crown_ap(&scan5, MESH, None),
        CrownApDecision::OffChannelFallback { bssid: [1; 6], ch: 1 },
        "co-channel below AP_USABLE_MIN excluded"
    );
    // 6. hysteresis: incumbent co-channel, new co-channel within margin → STAY (no flap).
    let scan6 = [ap(3, 6, -70), ap(4, 6, -66)]; // +4 dB < 6 dB margin
    assert_eq!(
        select_crown_ap(&scan6, MESH, Some(ap(3, 6, -70))),
        CrownApDecision::CoChannel { bssid: [3; 6], ch: 6 },
        "hysteresis: stay on incumbent within margin"
    );
    // 7. hysteresis: new co-channel beats incumbent by > margin → SWITCH.
    let scan7 = [ap(3, 6, -74), ap(4, 6, -60)]; // +14 dB > 6
    assert_eq!(
        select_crown_ap(&scan7, MESH, Some(ap(3, 6, -74))),
        CrownApDecision::CoChannel { bssid: [4; 6], ch: 6 },
        "hysteresis: switch when new beats incumbent by margin"
    );

    // ---- strand-guard state machine -------------------------------------------------------
    let no_ctx = CrownCtx { reassoc_exhausted: false, better_successor_cc: false };
    let exhausted = CrownCtx { reassoc_exhausted: true, better_successor_cc: false };
    let succ = CrownCtx { reassoc_exhausted: false, better_successor_cc: true };
    let co = CrownApDecision::CoChannel { bssid: [3; 6], ch: 6 };
    let off = CrownApDecision::OffChannelFallback { bssid: [1; 6], ch: 1 };

    assert_eq!(crown_next_state(CrownState::Normal, co, 0, no_ctx), CrownState::Normal, "co-channel stays normal");
    assert_eq!(crown_next_state(CrownState::Normal, off, 0, no_ctx), CrownState::Normal, "off-channel + not-exhausted: keep trying");
    assert_eq!(crown_next_state(CrownState::Normal, off, 0, exhausted), CrownState::Shed, "off-channel + exhausted → shed");
    assert_eq!(crown_next_state(CrownState::Shed, off, 1, no_ctx), CrownState::Shed, "shed, reclaims<MAX → shed");
    assert_eq!(crown_next_state(CrownState::Shed, off, SHED_RECLAIM_MAX, no_ctx), CrownState::Degraded, "STRAND-GUARD: reclaims>=MAX → degraded (never crownless)");
    assert_eq!(crown_next_state(CrownState::Shed, co, 5, no_ctx), CrownState::Normal, "shed but co-channel appeared → recover to normal");
    assert_eq!(crown_next_state(CrownState::Degraded, co, 0, no_ctx), CrownState::Normal, "degraded + co-channel returns → normal");
    assert_eq!(crown_next_state(CrownState::Degraded, off, 9, succ), CrownState::Shed, "degraded yields to a cc=1 successor");
    assert_eq!(crown_next_state(CrownState::Degraded, off, 9, no_ctx), CrownState::Degraded, "degraded + no co-channel + no successor → STAY (never crownless)");

    // ---- #335 Deferred: an UNANSWERED question moves NOTHING -------------------------------
    // The whole point of the variant. `NoAp` and `OffChannelFallback` are findings the ladder is
    // entitled to act on; a deferral is not a finding, and before #335 it had no way to say so —
    // both skip-shaped returns in `reassoc_ch6_prefer` spelled themselves `NoAp`, so a mesh-OTA
    // relay and a failed scan each fed the shed ladder evidence they did not have.
    //
    // Neutrality is asserted from EVERY state and under EVERY ctx that would otherwise move it,
    // because "it happens not to move right now" and "it cannot move" are different claims and
    // only the second is the invariant.
    let defer = CrownApDecision::Deferred;
    assert_eq!(crown_next_state(CrownState::Normal, defer, 0, no_ctx), CrownState::Normal, "deferred: normal stays normal");
    assert_eq!(crown_next_state(CrownState::Normal, defer, 0, exhausted), CrownState::Normal, "DEFERRED MUST NOT SHED: exhausted+deferred is NOT off-channel evidence");
    assert_eq!(crown_next_state(CrownState::Shed, defer, SHED_RECLAIM_MAX, no_ctx), CrownState::Shed, "DEFERRED MUST NOT ESCALATE: reclaims>=MAX but nothing was measured");
    assert_eq!(crown_next_state(CrownState::Shed, defer, 0, no_ctx), CrownState::Shed, "deferred: shed stays shed");
    assert_eq!(crown_next_state(CrownState::Degraded, defer, 9, succ), CrownState::Degraded, "DEFERRED MUST NOT YIELD: a successor claim is not acted on without a scan");
    assert_eq!(crown_next_state(CrownState::Degraded, defer, 9, no_ctx), CrownState::Degraded, "deferred: degraded stays degraded");
    // And it must not RECOVER either — neutral is neutral in both directions. A deferral is not
    // evidence that a co-channel AP appeared, so it cannot promote a shed crown back to Normal.
    assert_eq!(crown_next_state(CrownState::Shed, defer, 0, no_ctx), CrownState::Shed, "deferred does not promote: shed !-> normal without a co-channel finding");
    assert_eq!(crown_next_state(CrownState::Degraded, defer, 0, no_ctx), CrownState::Degraded, "deferred does not promote: degraded !-> normal without a co-channel finding");

    // ---- OTA-enable gate ------------------------------------------------------------------
    assert!(ota_enabled(CrownState::Normal), "OTA only when normal");
    assert!(!ota_enabled(CrownState::Degraded), "OTA disabled when degraded");
    assert!(!ota_enabled(CrownState::Shed), "OTA disabled when shed");

    println!("ap_select_verify: ALL CHECKS PASSED (co-channel preference + usable floor + hysteresis + strand-guard state machine + OTA-enable gate)");
}
