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
    /// The board holds real (NTP-authoritative) time, not a free-running clock — a better gateway
    /// can serve TIME frames. `synced_at != 0` upstream.
    pub ntp_holder: bool,
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
    pub ntp: u8,
    pub uptime: u8,
}

impl MetricWeights {
    /// The SHIPPED default (JP: "elect the BEST gateway" ⇒ best-gateway is the default behavior,
    /// team-lead decision 2026-07-20). CO-CHANNEL-DOMINANT: `co_channel` (100) alone outranks the
    /// maximum of every other signal combined (`rssi` 10·2 + `ntp` 5 + `uptime` 1·2 = 27), so a
    /// co-channel board ALWAYS beats a stronger OFF-channel board (the id5 ch1-vs-ch6 bug). Absent /
    /// empty / malformed config falls back to THIS (not to legacy) — best-gateway is on by default.
    pub const DEFAULT: Self = Self { co_channel: 100, rssi: 10, ntp: 5, uptime: 1 };

    /// Theoretical maximum fitness for these weights — the deficit reference in [`elect_backoff_ms`],
    /// making the tiering scale-invariant to the weight magnitudes. `const` for use at call sites.
    pub const fn max_fitness(&self) -> u16 {
        self.co_channel as u16
            + self.rssi as u16 * RSSI_SCORE_MAX
            + self.ntp as u16
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
        + (w.ntp as u16) * (i.ntp_holder as u16)
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
    let tiers = ((deficit * MAX_ELECT_TIERS) + (maxf - 1)) / maxf;
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
///   * empty / whitespace / non-UTF-8 / no recognized token ⇒ `BestGateway(DEFAULT)` — best-gateway
///     is ON by default, and the retain-clear restores it (team-lead decision).
///   * `legacy` (case-insensitive) ⇒ `Legacy` (the escape hatch).
///   * keyed weights `c<n>r<n>n<n>u<n>` (any order, any subset; missing keys inherit `DEFAULT`;
///     values clamp to 255; unknown letters + their digits are ignored) ⇒ `BestGateway(weights)`.
///     e.g. `c100r10n5u1` = the default; `c0r100` = RSSI-dominant with co-channel off.
pub fn parse_elect_config(payload: &[u8]) -> ElectConfig {
    let s = match core::str::from_utf8(payload) {
        Ok(s) => s.trim(),
        Err(_) => return ElectConfig::BestGateway(MetricWeights::DEFAULT),
    };
    if s.is_empty() {
        return ElectConfig::BestGateway(MetricWeights::DEFAULT);
    }
    if s.eq_ignore_ascii_case("legacy") {
        return ElectConfig::Legacy;
    }
    let mut w = MetricWeights::DEFAULT;
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
                b'n' | b'N' => w.ntp = v,
                b'u' | b'U' => w.uptime = v,
                _ => {}
            }
        }
    }
    ElectConfig::BestGateway(w)
}
