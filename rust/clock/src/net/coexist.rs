//! #217 rung-3 — co-channel-preferred crown AP selection + strand-guard (PURE core).
//!
//! Single-radio coexist (mode.rs §top): while associated to an AP the PHY sits on that AP's
//! channel, so ESP-NOW works ONLY on that channel. The mesh runs on `ESP_NOW_FIXED_CHANNEL`
//! (=6); if the crown associates to an off-channel AP (e.g. a strong ch1 AP), it is deaf to the
//! mesh AND its own OTA fetch stalls at byte 0. Rung-3 makes the crown PREFER a `jplovescl` AP
//! that is ALREADY on the mesh channel, keeping the mesh on ch6 (leaves unaffected).
//!
//! This module is PURE (no `esp-hal`/`esp-wifi`, no alloc, no float) so it is host-tested verbatim
//! by `experiments/ap_select_verify` (`#[path]`-include, mirroring `wire`/`flood`/`etx`). The
//! firmware (`net::wifi` / `net::mode`) constructs [`ApView`]s from `esp_wifi` scan results and
//! feeds the decisions to the WiFi association + crown state machine.

/// A scanned AP, already filtered to the target SSID by the caller. BSSID is the full 6 octets
/// (used to PIN association via `ClientConfiguration.bssid`); `rssi` is dBm (negative).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApView {
    pub bssid: [u8; 6],
    pub channel: u8,
    pub rssi: i8,
}

/// Outcome of [`select_crown_ap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrownApDecision {
    /// Pin association to this co-channel (== mesh) AP → a healthy, OTA-capable crown.
    CoChannel { bssid: [u8; 6], ch: u8 },
    /// No USABLE co-channel AP; the best off-channel AP (keeps WiFi/MQTT alive) — this is the
    /// STRAND signal that feeds the [`crown_next_state`] shed→degraded ladder.
    OffChannelFallback { bssid: [u8; 6], ch: u8 },
    /// No target-SSID AP visible at all.
    ///
    /// ⚠️ This is EVIDENCE: "I looked and there was nothing." It feeds the shed ladder. Do not
    /// reach for it to mean "I did not look" — that is [`Deferred`](Self::Deferred), and the two
    /// were conflated until #335.
    NoAp,
    /// **The question was not answered.** No scan happened, or it failed, or the answer did not
    /// arrive in time — so this carries NO information about the AP situation.
    ///
    /// ── WHY A FOURTH VARIANT RATHER THAN REUSING `NoAp` (#335, Addendum A.5) ──────────────────
    /// `NoAp` and `OffChannelFallback` are both *findings*; the ladder is entitled to act on them
    /// and does. A failure to look is not a finding, and spending it as one sheds a crown on the
    /// strength of a scan that never ran. The tree already said so, at
    /// `mode.rs::reassoc_ch6_prefer`: *"the only 'skip' value available is `NoAp` — which the
    /// caller cannot distinguish from 'no usable AP exists' and would act on by shedding the
    /// crown … If this path ever needs guarding, give the decision an explicit `Deferred` variant
    /// first — do NOT reuse `NoAp`."* This is that variant; that note is now discharged.
    ///
    /// The same vocabulary already exists one layer down — `may_scan`'s **"DEFER, NEVER ABANDON"**
    /// — so this is the decision type catching up with a concept the heap gate already had.
    ///
    /// **Contract for producers:** emitting `Deferred` obliges you to leave whatever drives the
    /// retry untouched, so the question gets asked again on a later tick. A deferred-and-forgotten
    /// scan is worse than a reboot, because it strands the node while it still looks alive.
    ///
    /// **Contract for consumers:** treat it as a NO-OP. [`crown_next_state`] returns the current
    /// state unchanged for it — not "not co-channel", which is what a `_` arm would have silently
    /// made it.
    Deferred,
}

/// A co-channel AP weaker than this (dBm) is excluded — it can't sustain the ~1 MB pull. Best-RSSI
/// selection already picks the strongest; this is only the exclusion floor. (#217 rung-3, Q6.)
pub const AP_USABLE_MIN: i8 = -82;
/// Only switch off the currently-associated co-channel AP if a DIFFERENT one beats it by this many
/// dB — anti-flap hysteresis for the 2×ch1 + 1×ch6 topology.
pub const HYST_MARGIN_DB: i8 = 6;

/// Pick the crown's AP. Prefers the best-RSSI co-channel (== `mesh_ch`) AP; hysteresis-latches the
/// current co-channel AP unless a different one clears [`HYST_MARGIN_DB`]. Falls back to the best
/// off-channel AP ONLY when no usable co-channel AP exists (→ strand ladder). Pure + deterministic.
///
/// `current` is the incumbent association (use the live `get_rssi()` for its `rssi`, not a stale
/// scan entry — nebula caveat 3).
pub fn select_crown_ap(aps: &[ApView], mesh_ch: u8, current: Option<ApView>) -> CrownApDecision {
    let best_co = aps
        .iter()
        .filter(|a| a.channel == mesh_ch && a.rssi >= AP_USABLE_MIN)
        .max_by_key(|a| (a.rssi, core::cmp::Reverse(a.bssid)));
    if let Some(co) = best_co {
        if let Some(cur) = current {
            if cur.channel == mesh_ch && cur.rssi >= AP_USABLE_MIN {
                // Latch the incumbent co-channel AP unless a DIFFERENT one clears the margin.
                if co.bssid != cur.bssid && co.rssi.saturating_sub(cur.rssi) > HYST_MARGIN_DB {
                    return CrownApDecision::CoChannel { bssid: co.bssid, ch: mesh_ch };
                }
                return CrownApDecision::CoChannel { bssid: cur.bssid, ch: mesh_ch };
            }
        }
        return CrownApDecision::CoChannel { bssid: co.bssid, ch: mesh_ch };
    }
    // No usable co-channel AP → best usable off-channel, else strongest of anything, else none.
    let best_any = aps
        .iter()
        .filter(|a| a.rssi >= AP_USABLE_MIN)
        .max_by_key(|a| (a.rssi, core::cmp::Reverse(a.bssid)))
        .or_else(|| aps.iter().max_by_key(|a| (a.rssi, core::cmp::Reverse(a.bssid))));
    match best_any {
        Some(a) => CrownApDecision::OffChannelFallback { bssid: a.bssid, ch: a.channel },
        None => CrownApDecision::NoAp,
    }
}

/// Crown coexist state (extends the existing claim/hold/shed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrownState {
    /// Associated to a co-channel AP; OTA enabled; DIAG `cc=1`.
    Normal,
    /// Abdicated (existing #204 2b shed path); waiting for a successor.
    Shed,
    /// STRAND-GUARD latch: associated to the best off-channel AP; MQTT bridge + mesh KEPT ALIVE;
    /// OTA disabled; DIAG `cc=0 degraded=1`. The fleet is NEVER left crownless.
    Degraded,
}

/// After this many shed→re-claim cycles with no co-channel AP (no better successor took over), a
/// board LATCHES [`CrownState::Degraded`] instead of shed-looping into a crownless gap.
pub const SHED_RECLAIM_MAX: u8 = 3;

/// Inputs to [`crown_next_state`] the selector decision doesn't carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrownCtx {
    /// This off-channel episode exhausted its bounded co-channel reassoc cycles (#204 2b).
    pub reassoc_exhausted: bool,
    /// A co-channel-capable board (HELLO `cc=1`) is claiming — a degraded crown yields to it.
    pub better_successor_cc: bool,
}

/// STRAND-GUARD transition (design §5.5). INVARIANT: never yields a path that leaves the fleet
/// crownless — a sole off-channel board LATCHES `Degraded` rather than shed-loop. Drives the
/// OTA-enable gate ([`ota_enabled`]) upstream. Pure.
pub fn crown_next_state(
    cur: CrownState,
    dec: CrownApDecision,
    shed_reclaims: u8,
    ctx: CrownCtx,
) -> CrownState {
    // #335: an UNANSWERED question moves nothing, in either direction. Checked first, before any
    // state arm, because every arm below is written in terms of "co-channel or not" — and a
    // deferral is neither. This is also why the arms no longer use a `_` wildcard: a wildcard
    // would have absorbed this variant silently and given it "not co-channel" semantics, which is
    // precisely the shed-on-a-scan-that-never-ran bug the variant exists to prevent. Adding a
    // fifth variant must now FAIL TO COMPILE here until someone decides what it means to the
    // ladder.
    if matches!(dec, CrownApDecision::Deferred) {
        return cur;
    }
    match cur {
        CrownState::Normal => match dec {
            CrownApDecision::CoChannel { .. } => CrownState::Normal,
            CrownApDecision::OffChannelFallback { .. } | CrownApDecision::NoAp => {
                if ctx.reassoc_exhausted {
                    CrownState::Shed
                } else {
                    CrownState::Normal
                }
            }
            // Unreachable — handled above. Spelled out so the match stays exhaustive without a
            // wildcard, which is the property that makes a future variant a compile error.
            CrownApDecision::Deferred => cur,
        },
        CrownState::Shed => match dec {
            CrownApDecision::CoChannel { .. } => CrownState::Normal,
            CrownApDecision::OffChannelFallback { .. } | CrownApDecision::NoAp => {
                if shed_reclaims >= SHED_RECLAIM_MAX {
                    CrownState::Degraded
                } else {
                    CrownState::Shed
                }
            }
            CrownApDecision::Deferred => cur,
        },
        CrownState::Degraded => match dec {
            CrownApDecision::CoChannel { .. } => CrownState::Normal,
            CrownApDecision::OffChannelFallback { .. } | CrownApDecision::NoAp => {
                if ctx.better_successor_cc {
                    CrownState::Shed
                } else {
                    CrownState::Degraded
                }
            }
            CrownApDecision::Deferred => cur,
        },
    }
}

/// OTA is attempted ONLY as a healthy co-channel crown (`Normal`) — never while `Shed`/`Degraded`,
/// so an off-channel crown keeps MQTT/mesh alive without firing offset-0 stall storms.
#[inline]
pub fn ota_enabled(state: CrownState) -> bool {
    matches!(state, CrownState::Normal)
}
