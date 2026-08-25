//! #381 crown SELF-OTA gating — the pure decision behind "may the crown update itself yet".
//!
//! Extracted from `main`'s `do_install` expression for the same reason `cfgsched` was extracted
//! from the relay: the failure mode is invisible on the board. A crown that declines its own
//! install **skips** it — the `&&` short-circuits before `take_install_request()`, so the command
//! is preserved and nothing fails, logs, or retries. That is byte-for-byte the same non-event as a
//! crown that is already up to date, which is how a whole fleet roll can report success with the
//! crown still on the old build.
//!
//! # The lock this module exists to break (#381, observed live on the 917 roll, 2026-08-02)
//!
//! "The gateway updates itself LAST" is enforced by two gates, and a permanently-deaf leaf pins
//! both of them ON forever:
//!
//! * `leaf_ota_pending` — RAM latch, set when the crown ARMS a relay, cleared only by a terminal
//!   `record_leaf_ota`. A leaf the crown cannot even address (`MacUnknown`) never produces a
//!   terminal outcome, so the latch never clears.
//! * `leaf_installs_outstanding` — derived from the RETAINED install topic. A leaf that never
//!   installs never clears its retained install, so this never drops either.
//!
//! Meanwhile the pre-relay retry (`MacUnknown` / `FetchFailed`, `!reached_leaf()`) is **uncapped
//! on purpose** — #134: a pre-relay failure is gateway-local and says nothing about the leaf or
//! the image, so the retained install must survive for the next attempt or the NEXT CROWN
//! (crown-portable, #111). Capping it re-introduces the #134 bug where a fetch-broken crown burned
//! an order a healthy successor would have finished. Observed: crown id5 at `retry=35+` for deaf
//! id50, with its own install silently skipped the whole time.
//!
//! # The fix, and what it deliberately does NOT do
//!
//! The pre-relay retry keeps running and the retained install is **never** touched here — #134 and
//! #111 are preserved exactly. What changes is that after [`LEAF_OTA_SELF_GATE_RETRIES`]
//! consecutive pre-relay failures **with no relay reaching any leaf in between**, that install
//! stops being allowed to hold the crown's own update hostage. The install stays armed and
//! retained; only its veto over `do_install` expires.
//!
//! This is #195's shape, mirrored. There, a crown whose OWN self-fetch keeps failing stops
//! re-triggering while deliberately keeping the retained arm (`self_ota_fetch_capped`). Here, a
//! crown whose RELAY keeps failing pre-handoff stops suppressing itself, while deliberately
//! keeping the retained install. Both bound a hammer without destroying an order.
//!
//! # Why "with no relay reaching any leaf in between" is load-bearing
//!
//! `leaf_ota_fetch_retries` is a single counter, not per-leaf, and the latch it releases is a
//! single bool. If the counter only ever reset on a TERMINAL outcome, a stale count from deaf
//! leaf A would still read as "stalled" while a perfectly healthy relay to leaf B was mid-flight —
//! and the crown would reboot into its own install underneath it. `record_leaf_ota` therefore
//! resets the counter on the post-handoff branch too (`reached_leaf()`), which makes the counter
//! mean *"consecutive pre-relay failures with no successful handoff in between"* — exactly the
//! condition under which holding the crown back buys nothing.

/// Consecutive pre-relay failures (`MacUnknown` / `FetchFailed`, no handoff in between) after
/// which the armed leaf install stops vetoing the crown's own self-OTA.
///
/// 3, deliberately the same number as `LEAF_OTA_MAX_RETRIES` (the post-handoff clear cap) and
/// `SELF_OTA_MAX_RETRIES` (#195's self-fetch cap) — one retry budget across the whole OTA path is
/// one number for an operator to hold. It is a SEPARATE constant rather than a reuse because it
/// governs a different thing: those two decide whether to abandon an order, this one only decides
/// whether an order may keep blocking someone else. At the ~30 s relay-flush cadence this is a
/// ~90 s grace period before the crown stops waiting — long enough to ride out a transient
/// roster miss, short enough that it cannot silently outlive a fleet roll.
pub const LEAF_OTA_SELF_GATE_RETRIES: u8 = 3;

/// Has the currently-armed leaf install failed pre-relay often enough to lose its veto?
///
/// Pure predicate over the counter `record_leaf_ota` maintains. `fetch_retries` saturates at
/// `u8::MAX`, so this stays true once tripped until something resets the counter (a terminal
/// outcome, or a relay that reaches a leaf).
#[inline]
pub fn leaf_veto_expired(fetch_retries: u8) -> bool {
    fetch_retries >= LEAF_OTA_SELF_GATE_RETRIES
}

/// May the crown run its OWN pending install?
///
/// The ordinary answer is the #40/#3 rule — not while a relay is armed in this session
/// (`leaf_ota_pending`) and not while any leaf still holds a retained install
/// (`leaf_installs_outstanding`), so the gateway updates itself LAST. The #381 escape is that a
/// leaf install which has repeatedly failed *before ever reaching a leaf* no longer counts as a
/// reason to wait.
///
/// Note the escape overrides BOTH gates, not just the RAM latch. That is required, not sloppy: a
/// deaf leaf pins `leaf_installs_outstanding` through its retained topic, so releasing only the
/// latch would leave the crown just as stuck — which is precisely the state the 2026-08-02
/// mitigation had to clear the retained topic by hand to escape.
#[inline]
pub fn crown_may_self_install(
    leaf_ota_pending: bool,
    leaf_installs_outstanding: bool,
    fetch_retries: u8,
) -> bool {
    leaf_veto_expired(fetch_retries) || (!leaf_ota_pending && !leaf_installs_outstanding)
}

// No `#[cfg(test)] mod tests` here, deliberately. This module is `espnow`-gated, so it is not in
// the `hostsim` lib and `cargo test` never compiles it — an in-file test suite would be a suite
// nothing runs, which is the exact shape #367's verifier-wiring gate exists to catch. The
// assertions live in `experiments/381_gate_verify/src/main.rs`, which `tools/gate.sh` discovers by
// glob and runs on every gate.
