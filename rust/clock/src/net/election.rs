//! Configurable best-gateway election — PURE core (no `esp-hal`/`esp-wifi`, no alloc, no float).
//!
//! JP directive: "nodes join the mesh and elect the BEST gateway" + "what makes the best gateway
//! must be configurable". The crown (coexist gateway) rides its AP's channel (single radio), so a
//! crown on an OFF-channel AP (e.g. a strong ch1 AP while the mesh is on ch6) is deaf to the mesh
//! AND its own OTA fetch stalls at byte 0 (#204/#217). The historical election keyed ONLY on
//! lowest-node-id (with RSSI merely staggering *recovery* takeover timing, #51) — so a co-channel
//! board was never PREFERRED at election time; a bad (off-channel) crown was only healed reactively
//! by #204/#217 shed → strand-guard. This module makes co-channel capability (and RSSI / NTP /
//! uptime, weighted by config) a FIRST-CLASS election input, so the best gateway is *elected*, not
//! merely self-healed onto.
//!
//! MECHANISM (reuses the proven #51 stagger — NO wire change, NO preemption of a live owner):
//! a board scores ITSELF into a `gateway_fitness`, and higher fitness → SHORTER claim backoff, so
//! the best board claims a vacant/dead-owner slot FIRST; weaker boards observe its fresh retained
//! `MC` and ADOPT it (the #51/#114/#122 no-flap stability contract is preserved verbatim — a board
//! only ever compares its own score against a timing threshold, never a peer's). The same backoff
//! gates the empty-MC (cold-boot) claim via the monotonic uptime clock, so at cold boot a
//! co-channel board crowns first without waiting for a #204/#217 shed cycle. NEVER-CROWNLESS: the
//! backoff is bounded ([`MAX_ELECT_TIERS`]), so a sole off-channel board still claims after its
//! (capped) wait.
//!
//! PURE + host-tested verbatim by `experiments/election_verify` (`#[path]`-include, mirroring
//! `coexist`/`wire`/`flood`/`etx`). The firmware (`net::wifi`) seeds [`FitnessInputs`] from the
//! live `RadioManager` and feeds the backoff into the `mqtt_session` election resolver.

/// The self-observed signals a candidate scores at claim time. Every field is already tracked on
/// the `RadioManager` (see the seed sites in `net::mode`), so fitness costs no new radio work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FitnessInputs {
    /// The board's AP channel == the fixed mesh channel (`ESP_NOW_FIXED_CHANNEL`). THE disease-fix
    /// signal: an off-channel crown is OTA-deaf regardless of RSSI (#217 rung-3), so co-channel
    /// dominates the default weights.
    pub co_channel: bool,
    /// Live RSSI-to-AP (dBm, signed; weaker = more negative). Bucketed by [`rssi_score`].
    pub ap_rssi: i8,
    /// Monotonic ms since boot (the loop clock == uptime). A longer-lived board is a more stable
    /// crown; also the STATELESS deferral clock for the cold-boot empty-MC claim.
    pub uptime_ms: u64,
}

/// Configurable per-signal weights (0 = ignore that signal). Fixed integer math (no_std, no float).
/// The retained operator-lever topic `smol/mesh/elect` re-weights these; see [`parse_elect_config`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetricWeights {
    pub co_channel: u8,
    pub rssi: u8,
    pub uptime: u8,
}

impl MetricWeights {
    /// CO-CHANNEL-DOMINANT — the #217 rung-3 / id5 disease fix, and the shipped default while
    /// leaves cannot follow a crown to a new channel. `co_channel` (100) alone outranks the maximum
    /// of every other signal combined (`rssi` 10·2 + `uptime` 1·2 = 22), so a co-channel board
    /// ALWAYS beats a stronger OFF-channel board. An off-channel crown is OTA-deaf on a fleet
    /// pinned to ch6, so this is not a preference — it is a veto, and it has to be.
    ///
    /// The veto is delivered by BACKOFF TIER, which is why the magnitude matters and not just the
    /// ordering: `elect_backoff_ms` normalises the fitness deficit to `MAX_ELECT_TIERS` (2), so with
    /// `max_fitness` 122 one tier spans ≈ 61 points. `co_channel: 100` pushes any off-channel board
    /// past a whole tier boundary while a co-channel board's worst deficit is 22 — separation is
    /// GUARANTEED, not merely likely. Keep that in mind before tuning this down.
    pub const DOMINANT: Self = Self { co_channel: 100, rssi: 10, uptime: 1 };

    /// CO-CHANNEL AS A HYSTERESIS MARGIN — valid only where leaves can follow a channel migration.
    ///
    /// **1. Why 10, derived rather than chosen.** `rssi_score` buckets at −65/−78 dBm weighted 10,
    /// so ONE BUCKET = 10 points, and one bucket is exactly the jitter amplitude of a board parked
    /// on a bucket boundary. Setting the margin equal to one bucket makes a one-bucket challenger
    /// TIE (jitter cannot move the fleet) and a two-bucket challenger WIN (a real gain migrates
    /// it). It is the floor below which flap returns. Everything after this is context.
    ///
    /// **2. Calibration anchor, stated as a DIVERGENCE and not a convergence.** batman-adv exposes
    /// the same knob as `gw_sel_class`, defaulting to 20 of ~255 TQ ≈ **8%** of its metric range.
    /// Ours is 10 of a `max_fitness` of 32 ≈ **31%** — roughly 4× stickier. That is deliberate and
    /// defensible rather than a coincidence to be pleased about: a batman-adv gateway switch makes
    /// clients re-pick, while a smol channel migration makes **every leaf follow or strand**.
    /// Higher disruption earns a higher bar.
    ///
    /// **3. No claim is made about how batman-adv arrived at 20.** There is no source for it, and a
    /// wrong reason underneath a right conclusion is the most durable kind of error.
    ///
    /// **4. The worked unit error, named so it is not repeated.** `10/122 = 8.2%` looks like a
    /// beautiful match for batman's 8% and is wrong: 122 is the PRE-re-scale `max_fitness`, which
    /// still contains the `co_channel: 100` being removed. Post-re-scale the max is **32**.
    ///
    /// ⚠️ Margin-to-RANGE is the misleading comparison whenever ranges compress; margin-to-NOISE is
    /// the invariant, and ours is 1× noise by construction. Do not shave below it — the disruption
    /// asymmetry forbids a sub-noise margin. It does not demand a super-noise one.
    ///
    /// ⚠️ AND THE RE-SCALE CHANGES THE MECHANISM, not only the number — this is the part a reader
    /// tuning it must not miss. With `max_fitness` 32, one backoff tier spans ≈ 16 points, so a
    /// 10-point margin is **SUB-TIER**: unlike `DOMINANT`, it does NOT guarantee tier separation,
    /// and two boards within one tier are resolved by the `node_id·200 ms` sub-tier tiebreak
    /// instead. So `co_channel` here is a genuine *preference*, not a veto. The anti-flap
    /// protection for a channel MIGRATION lives elsewhere and deliberately so — `SETTLE_MS` and
    /// `margin_for` in `net::mesh_elect`, which govern the channel decision rather than the gateway
    /// one (they are two different elections; see that module's header). Claiming this weight is
    /// "the anti-flap floor" for the migration would be a comment describing behaviour the binary
    /// does not have.
    pub const FOLLOWING: Self = Self { co_channel: 10, rssi: 10, uptime: 1 };

    /// The shipped weights, selected by whether leaves can FOLLOW a channel migration.
    ///
    /// This signature is the coupling, and it is a signature rather than a comment on purpose. The
    /// re-scale and the follow capability cannot ship apart: re-scaling first re-arms exactly the
    /// disease #217 fixed (a stronger off-channel board wins the crown and a ch6-pinned fleet
    /// cannot follow it), and a note saying "don't ship these separately" is the kind of guarantee
    /// this codebase has repeatedly watched fail. There is no way to obtain weights without stating
    /// which world you are in.
    pub const fn default_for(follow: bool) -> Self {
        if follow {
            Self::FOLLOWING
        } else {
            Self::DOMINANT
        }
    }

    /// Theoretical maximum fitness for these weights — the deficit reference in [`elect_backoff_ms`],
    /// making the tiering scale-invariant to the weight magnitudes. `const` for use at call sites.
    pub const fn max_fitness(&self) -> u16 {
        self.co_channel as u16
            + self.rssi as u16 * RSSI_SCORE_MAX
            + self.uptime as u16 * UPTIME_SCORE_MAX
    }
}

/// The election policy the retained `smol/mesh/elect` topic selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElectConfig {
    /// Best-gateway (default). Carries the (possibly re-weighted) metric.
    BestGateway(MetricWeights),
    /// The ESCAPE HATCH (team-lead decision): fall back to the historical lowest-id claim +
    /// RSSI-only recovery stagger ([`legacy_recovery_backoff_ms`]). Selected by publishing the
    /// literal payload `legacy` to `smol/mesh/elect`. A genuine 1:1 rollback (no fitness math).
    Legacy,
}

/// #278/#269 MASTER SWITCH: may a leaf ACT on a `SMOLv1 ELECT` channel announcement?
///
/// Default OFF. A leaf still parses, epoch-orders and reports every announcement — observing and
/// acting are separate, and the observe-only landing is what lets the fleet be measured before it
/// is moved. #278's closing checklist gates the watch's `ELECT_ENFORCE` on the smol roll showing
/// ONE clean channel migration first; this is the smol half of that ordering.
///
/// ⚠️ It lives HERE, in the gateway-election module, rather than in `net::mesh_elect` where the
/// following actually happens — and the reason is worth stating because the obvious placement is
/// the wrong one. `mesh_elect` is `espnow`-gated while `election` is `wifi`-gated, and `espnow`
/// implies `wifi`, so `election` is visible to strictly more of the tree; a flag in `mesh_elect`
/// could not be read by `net::wifi`'s config parser at all on a wifi-only build. Putting it beside
/// the weights it selects also makes the coupling impossible to miss: the same `bool` chooses
/// `DOMINANT` vs `FOLLOWING` and enables the follow path. Every pure function takes it as a
/// PARAMETER rather than reading this const, so both host verifiers can exercise both states.
///
/// A plain `const` and not a Cargo feature, deliberately: a default-off feature adds an axis to the
/// build matrix and a fresh exclusions surface, and — worse — the shipped fleet image would then
/// not CONTAIN the follow path, so the canary roll could not exercise the thing it exists to prove.
pub const FOLLOW_ENABLED: bool = false;

/// The largest lever weight for which co-channel dominance is still EXPRESSIBLE in a `u8`.
///
/// Derived, not chosen: dominance needs `co_channel > rssi·2 + uptime·2`, and `co_channel` is a
/// `u8`, so the floor `2r + 2u + 1` must fit in 255 ⇒ `r + u ≤ 127`. Capping each at 63 satisfies
/// that with room (126 ≤ 127). Above this the operator lever could ask for a weighting in which no
/// legal `co_channel` value dominates at all, and [`parse_elect_config`] would be clamping toward
/// an invariant it could never reach.
pub const DOMINANCE_LEVER_CAP: u8 = 63;

/// The smallest `co_channel` for which co-channel capability outranks EVERYTHING else combined.
#[must_use]
pub const fn dominance_floor(w: &MetricWeights) -> u8 {
    let need = w.rssi as u16 * RSSI_SCORE_MAX + w.uptime as u16 * UPTIME_SCORE_MAX + 1;
    if need > u8::MAX as u16 {
        u8::MAX
    } else {
        need as u8
    }
}

/// Does `co_channel` alone strictly outrank the maximum of every other signal combined?
///
/// ONE predicate, used in three places — the compile-time assertion on [`MetricWeights::DOMINANT`],
/// the runtime clamp in [`parse_elect_config`], and the host test. The lever is a SECOND WRITER of
/// these weights, so an invariant enforced only at compile time binds only half the writers; and
/// three separate spellings of the same rule is how two of them drift.
#[must_use]
pub const fn co_channel_dominates(w: &MetricWeights) -> bool {
    (w.co_channel as u16) > w.rssi as u16 * RSSI_SCORE_MAX + w.uptime as u16 * UPTIME_SCORE_MAX
}

/// THE id5 INVARIANT, stated so that it holds in BOTH flag states.
///
/// The id5 bug is not "an off-channel board won the crown" — it is "an off-channel crown STRANDS a
/// fleet that cannot follow it". The stranding is the harm, and inability to follow is its
/// precondition. So the same predicate covers both worlds: with following OFF the fleet cannot
/// follow, and only co-channel dominance prevents the harm; with following ON the precondition is
/// gone and a stronger off-channel gateway is a migration rather than a stranding — which is the
/// entire point of #269.
///
/// Writing the two states as one predicate is what lets `election_verify` assert the id5 case in
/// both, rather than asserting one thing here and a different thing there and hoping the pair still
/// means something.
/// `#[allow(dead_code)]`: this predicate is a STATEMENT of the invariant, not a runtime branch —
/// the firmware enforces it (the clamp, the const assert) rather than querying it, so nothing calls
/// it outside a `const` block, which does not count as a use. Deleting it is not an option: it is
/// the single spelling of the id5 rule that `experiments/election_verify` asserts in BOTH flag
/// states, and the whole reason the two states can be checked against one property instead of two
/// unrelated ones. Same shape and same reasoning as `refuse_leaf_lock_off_channel` below.
#[allow(dead_code)]
#[must_use]
pub const fn off_channel_crown_can_strand(w: &MetricWeights, follow: bool) -> bool {
    !follow && !co_channel_dominates(w)
}

/// The disease fix, asserted at compile time rather than described. If someone tunes `DOMINANT`
/// such that a strong off-channel board can out-score a co-channel one, the build stops — which is
/// the half of the invariant a `const` can carry. The other half (the runtime lever) is
/// [`parse_elect_config`]'s clamp, because a compile-time assert binds only one of the two writers.
const _DOMINANT_ACTUALLY_DOMINATES: () = {
    assert!(co_channel_dominates(&MetricWeights::DOMINANT));
    assert!(!off_channel_crown_can_strand(&MetricWeights::DOMINANT, false));
    // And the cap really does keep the floor ATTAINABLE — the arithmetic, not the intent. Written
    // through `dominance_floor` rather than by re-deriving `2r + 2u + 1` here, so the two cannot
    // drift apart: if the floor ever saturated, the clamp in `parse_elect_config` would be reaching
    // for a dominance that no legal `co_channel` value could reach, and would look enforced while
    // being unenforceable.
    let at_cap = MetricWeights {
        co_channel: 0,
        rssi: DOMINANCE_LEVER_CAP,
        uptime: DOMINANCE_LEVER_CAP,
    };
    let floor = dominance_floor(&at_cap);
    assert!(floor < u8::MAX, "the floor saturated — DOMINANCE_LEVER_CAP is too high");
    let clamped_at_cap = MetricWeights {
        co_channel: floor,
        rssi: DOMINANCE_LEVER_CAP,
        uptime: DOMINANCE_LEVER_CAP,
    };
    assert!(co_channel_dominates(&clamped_at_cap));
};

/// Max RSSI bucket value (`rssi_score` ∈ 0..=2).
const RSSI_SCORE_MAX: u16 = 2;
/// Max uptime bucket value (`uptime_score` ∈ 0..=2).
const UPTIME_SCORE_MAX: u16 = 2;
/// One uptime bucket = 5 min (0: <5m, 1: <10m, 2: ≥10m). A crown that has held for tens of minutes
/// scores full uptime; a just-booted board scores 0 (so a fresh board doesn't outrank a stable one
/// on uptime alone).
const UPTIME_STEP_MS: u64 = 300_000;

/// Backoff step per fitness tier. MUST exceed the recovery-burst cadence (`REELECT_RETRY_MS` = 10 s,
/// in `mode.rs`) so a weaker board always gets an adopt-burst BETWEEN the stronger board's claim and
/// its own claim threshold — that's what keeps the winner stable (no competing claim; the lowest-id
/// flush resolver never fires to undo it). Same value + rationale as the historical `RSSI_BUCKET_STEP_MS`.
pub const ELECT_TIER_STEP_MS: u64 = 15_000;

/// Max backoff tiers — BOUNDS worst-case takeover / cold-boot-crownless latency at
/// `MAX_ELECT_TIERS × ELECT_TIER_STEP_MS` = 30 s (+ the sub-tier node-id term). Three tiers {0,1,2}
/// = the same 0–30 s envelope as the historical 3-bucket RSSI backoff, so best-gateway never waits
/// longer than legacy did. Co-channel boards land in {0,1}, off-channel in {2} (see [`elect_backoff_ms`]).
pub const MAX_ELECT_TIERS: u64 = 2;

/// RSSI → 0..=2 bucket (higher = stronger). Thresholds match the historical `reelect_backoff_ms`
/// buckets (−65 / −78 dBm) so `Legacy` and the RSSI term of best-gateway agree on the STA range.
#[inline]
pub fn rssi_score(rssi: i8) -> u16 {
    if rssi >= -65 {
        2
    } else if rssi >= -78 {
        1
    } else {
        0
    }
}

/// Uptime(ms) → 0..=`UPTIME_SCORE_MAX` bucket (higher = longer-lived).
#[inline]
fn uptime_score(uptime_ms: u64) -> u16 {
    (uptime_ms / UPTIME_STEP_MS).min(UPTIME_SCORE_MAX as u64) as u16
}

/// Higher = better gateway. Pure, saturating, integer. Also the advisory value a future MC field
/// could carry for observability (Phase 1 keeps the wire unchanged — fitness is purely local).
pub fn gateway_fitness(i: &FitnessInputs, w: &MetricWeights) -> u16 {
    (w.co_channel as u16) * (i.co_channel as u16)
        + (w.rssi as u16) * rssi_score(i.ap_rssi)
        + (w.uptime as u16) * uptime_score(i.uptime_ms)
}

/// Best-gateway claim backoff (ms). Higher fitness → SHORTER wait, so the best board claims a
/// vacant/dead slot first. The fitness DEFICIT (below the weights' max) is normalized to
/// 0..=[`MAX_ELECT_TIERS`] (ceil), making the ordering scale-invariant to the weight magnitudes;
/// `node_id·200 ms` is the sub-tier final tiebreak (fleet ids are single/low-double-digit, so it
/// never separates tiers — same convention as the historical backoff). Pure + deterministic.
pub fn elect_backoff_ms(i: &FitnessInputs, w: &MetricWeights, node_id: u8) -> u64 {
    let maxf = w.max_fitness().max(1) as u64;
    let fit = gateway_fitness(i, w) as u64;
    let deficit = maxf.saturating_sub(fit);
    // ceil(deficit * MAX_ELECT_TIERS / maxf), clamped — best board (deficit 0) → tier 0 → no wait.
    let tiers = (deficit * MAX_ELECT_TIERS).div_ceil(maxf);
    tiers.min(MAX_ELECT_TIERS) * ELECT_TIER_STEP_MS + (node_id as u64) * 200
}

/// LAYER 2 (crown-migration override): should a CO-CHANNEL board SEIZE an owner proven OFF-channel?
/// True iff we are co-channel with a KNOWN mesh channel, the owner is not us, and its advertised
/// MC channel is KNOWN (`!= 0`) and != the mesh channel. An off-channel crown is the OTA-deaf WRONG
/// gateway (the #204/#217 disease), so the better (co-channel) board takes it over IMMEDIATELY rather
/// than deferring to it (the dead/ghost or off-channel incumbent). A live co-channel owner
/// (`owner_ch == mesh_ch`) or an unknown-channel owner (`owner_ch == 0`) is NOT seized — those go
/// through the normal liveness/lowest-id arms. Pure + deterministic.
pub fn seize_off_channel_owner(
    co_channel: bool,
    mesh_ch: u8,
    node_id: u8,
    owner_id: u8,
    owner_ch: u8,
) -> bool {
    co_channel && mesh_ch != 0 && owner_id != node_id && owner_ch != 0 && owner_ch != mesh_ch
}

/// LAYER 2 (symmetric YIELD — makes the seize STICK): should an OFF-channel board ADOPT a LIVE
/// co-channel owner regardless of node-id? True iff we are NOT co-channel (and know it), the owner is
/// not us, its MC channel == the mesh channel (a co-channel crown), and it is alive. Without this the
/// historical lowest-id rule lets a LOWER-id off-channel crown (id5) re-claim the crown from the
/// co-channel board (id7) every flush → endless flap; with it the off-channel board yields, so the
/// co-channel seize converges to a stable co-channel crown. Pure + deterministic. (A co-channel board
/// never yields — it seizes instead; the two predicates are mutually exclusive on `co_channel`.)
#[allow(clippy::too_many_arguments)]
pub fn yield_to_co_channel_owner(
    co_channel_known: bool,
    co_channel: bool,
    mesh_ch: u8,
    node_id: u8,
    owner_id: u8,
    owner_ch: u8,
    owner_alive: bool,
) -> bool {
    co_channel_known
        && !co_channel
        && mesh_ch != 0
        && owner_id != node_id
        && owner_ch == mesh_ch
        && owner_alive
}

/// LAYER 2 reliability: should a CO-CHANNEL-capable board REFUSE to leaf-lock to (settle under) an
/// owner whose MC channel is a KNOWN off-channel? True iff our AP channel == the mesh channel AND the
/// owner's channel is known (`!= 0`) AND != the mesh channel. Refusing the lock (skip the
/// scan-lock + owner-silence reset) keeps the leaf RE-ELECTING, so the co-channel SEIZE re-runs each
/// recovery burst and fires RELIABLY — fixing the racy ~2/3 seize where a happy leaf-lock (co_channel
/// transiently unknown at the boot tick) stopped the bursts that would have seized. A co-channel owner
/// (`owner_ch == mesh`) or an unknown owner channel (`0`) → lock normally (false). Pure + deterministic.
/// `#[allow(dead_code)]`: the firmware caller (`net/mode.rs`'s leaf-lock decision) is `espnow`-gated
/// while this module is `wifi`-gated, so a `wifi`-only build compiles it unused. Deleting it is NOT
/// an option — `experiments/election_verify` `#[path]`-includes this file and asserts five cases
/// against this exact function, so it is the tested half of the API, the same shape as
/// `cfgsched::{ticks_to_cover, peek}`. Item-scoped so the rest of `election` stays under `-D`.
#[allow(dead_code)]
pub fn refuse_leaf_lock_off_channel(my_ap_ch: u8, mesh_ch: u8, owner_ch: u8) -> bool {
    mesh_ch != 0 && my_ap_ch == mesh_ch && owner_ch != 0 && owner_ch != mesh_ch
}

/// `Legacy` recovery backoff — reproduces the historical `reelect_backoff_ms(rssi, node_id)` EXACTLY
/// (bucket 0/1/2 × 15 s + id·200 ms), so `ElectConfig::Legacy` is a byte-faithful rollback of the
/// election timing. Kept here (pure) so the regression is host-pinned alongside best-gateway.
pub fn legacy_recovery_backoff_ms(rssi: i8, node_id: u8) -> u64 {
    let bucket: u64 = if rssi >= -65 {
        0
    } else if rssi >= -78 {
        1
    } else {
        2
    };
    bucket * ELECT_TIER_STEP_MS + (node_id as u64) * 200
}

/// Parse the retained `smol/mesh/elect` payload → policy. Panic-free (checked UTF-8, no indexing):
///   * empty / whitespace / non-UTF-8 / no recognized token ⇒ `BestGateway(default_for(follow))` —
///     best-gateway is ON by default, and the retain-clear restores it (team-lead decision).
///   * `legacy` (case-insensitive) ⇒ `Legacy` (the escape hatch).
///   * keyed weights `c<n>r<n>n<n>u<n>` (any order, any subset; missing keys inherit the default;
///     values clamp to 255; unknown letters + their digits are ignored) ⇒ `BestGateway(weights)`.
///     e.g. `c100r10n5u1` = the following-off default; `c0r100` = RSSI-dominant, co-channel off.
///
/// # The clamp: an invariant must bind EVERY writer, not just the compile-time one
///
/// This topic is a SECOND WRITER of the weights, and it can re-arm the id5 disease with no code
/// change at all — `c10r10` demotes co-channel dominance from a retained MQTT payload, and nothing
/// in the firmware would notice. A `const` assertion on [`MetricWeights::DOMINANT`] binds the
/// compile-time writer and says nothing whatever about this one.
///
/// So while following is off, the parsed weights are clamped back to dominance before they are
/// returned. Note it is derived from the PARSED weights rather than being a literal: the lever
/// writes `r` and `u` too, so a hardcoded floor of 22 would be silently invalidated the first time
/// someone raised `r`. The `r`/`u` cap at [`DOMINANCE_LEVER_CAP`] closes the remaining hole —
/// without it a payload like `c255r200` asks for a weighting in which NO legal `co_channel` value
/// dominates, and the clamp would be reaching for an invariant it could never attain.
///
/// The result is a property that holds for every possible payload rather than for the ones that
/// happened to be tested: with `follow == false`, [`co_channel_dominates`] is true of whatever
/// comes back. `election_verify` asserts exactly that, over a sweep rather than a handful of cases.
///
/// # Readback reports APPLIED state
///
/// `net::wifi`'s readback logs the weights this function RETURNS, so an operator who publishes
/// `c10r10` reads back the clamped values and learns immediately that their knob was overruled and
/// by how much. That is the same rule the `n0` slot already follows: a readback reports what was
/// applied, never what was sent. Echoing the request would claim an application that never
/// happened.
pub fn parse_elect_config(payload: &[u8], follow: bool) -> ElectConfig {
    let default = MetricWeights::default_for(follow);
    let s = match core::str::from_utf8(payload) {
        Ok(s) => s.trim(),
        Err(_) => return ElectConfig::BestGateway(default),
    };
    if s.is_empty() {
        return ElectConfig::BestGateway(default);
    }
    if s.eq_ignore_ascii_case("legacy") {
        return ElectConfig::Legacy;
    }
    let mut w = default;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let key = b[i];
        i += 1;
        let mut val: u16 = 0;
        let mut got = false;
        while i < b.len() && b[i].is_ascii_digit() {
            got = true;
            val = (val * 10 + (b[i] - b'0') as u16).min(255);
            i += 1;
        }
        if got {
            let v = val as u8;
            match key {
                b'c' | b'C' => w.co_channel = v,
                b'r' | b'R' => w.rssi = v,
                // #269: `n` (ntp) was removed as a fitness input — see `gateway_fitness`.
                //
                // This arm is a TOMBSTONE, and it is load-bearing beyond compatibility.
                // Functionally it matches the `_` fallthrough (both ignore the key), so its whole
                // value is what it TELLS a future reader: `n` is RESERVED, not available. Without
                // it, someone reintroduces `n` for a new meaning and every stale retained
                // `c…r…n…u…` payload still sitting in the broker silently re-weights their new
                // feature — a live-config landmine with no compile-time trace. An explicit
                // tombstone prevents key REUSE; a fallthrough invites it.
                //
                // It also keeps the older promise: a pre-#269 retained payload still parses, and
                // its `n` is consumed and DISCARDED rather than shifting the remaining weights.
                b'n' | b'N' => {}
                b'u' | b'U' => w.uptime = v,
                _ => {}
            }
        }
    }
    if !follow {
        // Cap first, so the floor below is always attainable, then raise `c` to it. Order matters:
        // clamping `c` against an unreachable floor would leave dominance broken while looking like
        // it had been enforced.
        if w.rssi > DOMINANCE_LEVER_CAP {
            w.rssi = DOMINANCE_LEVER_CAP;
        }
        if w.uptime > DOMINANCE_LEVER_CAP {
            w.uptime = DOMINANCE_LEVER_CAP;
        }
        let floor = dominance_floor(&w);
        if w.co_channel < floor {
            w.co_channel = floor;
        }
        debug_assert!(co_channel_dominates(&w));
    }
    ElectConfig::BestGateway(w)
}
