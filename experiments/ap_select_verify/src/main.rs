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

    // ---- OTA-enable gate ------------------------------------------------------------------
    assert!(ota_enabled(CrownState::Normal), "OTA only when normal");
    assert!(!ota_enabled(CrownState::Degraded), "OTA disabled when degraded");
    assert!(!ota_enabled(CrownState::Shed), "OTA disabled when shed");

    println!("ap_select_verify: ALL CHECKS PASSED (co-channel preference + usable floor + hysteresis + strand-guard state machine + OTA-enable gate)");
}
