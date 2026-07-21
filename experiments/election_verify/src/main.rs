//! Best-gateway election host guard. `#[path]`-includes the REAL `net/election.rs` (no drift) and
//! asserts the configurable fitness, the bounded fitness→backoff tiering, co-channel dominance (the
//! id5 ch1-vs-ch6 bug), and the config parser. `cargo run`.

#[path = "../../../rust/clock/src/net/election.rs"]
mod election;

use election::*;

fn inp(co_channel: bool, ap_rssi: i8, ntp_holder: bool, uptime_ms: u64) -> FitnessInputs {
    FitnessInputs { co_channel, ap_rssi, ntp_holder, uptime_ms }
}

fn main() {
    let w = MetricWeights::DEFAULT;

    // ---- gateway_fitness (weighted, higher = better) --------------------------------------
    // co-channel dominates: a co-channel board with the WORST usable RSSI still outscores the
    // BEST off-channel board (co=100 vs off-channel max = rssi 2·10 + ntp 5 + uptime 2·1 = 27).
    let co_weak = gateway_fitness(&inp(true, -82, false, 0), &w);
    let off_best = gateway_fitness(&inp(false, -60, true, 10 * 60_000), &w);
    assert!(co_weak > off_best, "co-channel dominance: {co_weak} !> {off_best}");
    // full-signal co-channel board = max fitness for the default weights.
    assert_eq!(
        gateway_fitness(&inp(true, -60, true, 10 * 60_000), &w),
        MetricWeights::DEFAULT.max_fitness(),
        "full-signal co-channel = max fitness"
    );
    // RSSI + uptime + ntp order WITHIN co-channel.
    let co_strong = gateway_fitness(&inp(true, -60, false, 0), &w);
    assert!(co_strong > co_weak, "stronger rssi scores higher among co-channel");

    // ---- elect_backoff_ms: higher fitness → shorter wait; co-channel ALWAYS beats off-channel --
    // The exact id5 bug, numerically: a co-channel HIGH-id board must claim BEFORE a stronger
    // OFF-channel LOW-id board (backoff strictly smaller despite the higher node id).
    let co_hi_id = elect_backoff_ms(&inp(true, -70, false, 0), &w, 9);
    let off_lo_id = elect_backoff_ms(&inp(false, -55, true, 10 * 60_000), &w, 3);
    assert!(
        co_hi_id < off_lo_id,
        "co-channel id9 ({co_hi_id}ms) must claim before off-channel id3 ({off_lo_id}ms) — the id5 bug"
    );
    // Bounded: no board EVER waits longer than the legacy 0–30 s envelope (+ the sub-tier id term).
    let worst = elect_backoff_ms(&inp(false, -90, false, 0), &w, 254);
    assert!(
        worst <= MAX_ELECT_TIERS * ELECT_TIER_STEP_MS + 254 * 200,
        "backoff bounded to MAX_ELECT_TIERS: {worst}"
    );
    // Best board (co-channel, strong, ntp, long uptime) = tier 0 → only the sub-tier id term.
    assert_eq!(
        elect_backoff_ms(&inp(true, -55, true, 10 * 60_000), &w, 5),
        5 * 200,
        "best board waits only the node-id tiebreak"
    );
    // Monotonic in fitness: stronger co-channel never waits LONGER than a weaker co-channel (same id).
    let s = elect_backoff_ms(&inp(true, -60, false, 0), &w, 7);
    let x = elect_backoff_ms(&inp(true, -82, false, 0), &w, 7);
    assert!(s <= x, "monotonic: stronger ({s}) !<= weaker ({x})");
    // Tier gap ≥ one step so a weaker board gets an adopt-burst before its own claim window.
    let co_tier = elect_backoff_ms(&inp(true, -82, false, 0), &w, 0);
    let off_tier = elect_backoff_ms(&inp(false, -55, false, 0), &w, 0);
    assert!(
        off_tier.saturating_sub(co_tier) >= ELECT_TIER_STEP_MS,
        "co-channel vs off-channel separated by >= one tier ({co_tier} .. {off_tier})"
    );

    // ---- re-weighting via config changes ordering -----------------------------------------
    // RSSI-dominant with co-channel OFF: now the stronger board wins regardless of channel.
    let rssi_only = match parse_elect_config(b"c0r100") {
        ElectConfig::BestGateway(w) => w,
        _ => panic!("expected BestGateway"),
    };
    let a = elect_backoff_ms(&inp(true, -85, false, 0), &rssi_only, 1); // co-channel but WEAK
    let b = elect_backoff_ms(&inp(false, -55, false, 0), &rssi_only, 1); // off-channel but STRONG
    assert!(b < a, "rssi-dominant config: strong off-channel now beats weak co-channel");

    // ---- parse_elect_config ----------------------------------------------------------------
    assert_eq!(parse_elect_config(b""), ElectConfig::BestGateway(MetricWeights::DEFAULT), "empty → default (best-gateway ON)");
    assert_eq!(parse_elect_config(b"   "), ElectConfig::BestGateway(MetricWeights::DEFAULT), "whitespace → default");
    assert_eq!(parse_elect_config(b"legacy"), ElectConfig::Legacy, "legacy keyword → escape hatch");
    assert_eq!(parse_elect_config(b"LEGACY"), ElectConfig::Legacy, "case-insensitive legacy");
    assert_eq!(parse_elect_config(b"c100r10n5u1"), ElectConfig::BestGateway(MetricWeights::DEFAULT), "explicit default weights");
    // subset: missing keys inherit DEFAULT.
    assert_eq!(
        parse_elect_config(b"r20"),
        ElectConfig::BestGateway(MetricWeights { co_channel: 100, rssi: 20, ntp: 5, uptime: 1 }),
        "subset inherits default for missing keys"
    );
    // clamp + ignore junk.
    assert_eq!(
        parse_elect_config(b"c999x7r3"),
        ElectConfig::BestGateway(MetricWeights { co_channel: 255, rssi: 3, ntp: 5, uptime: 1 }),
        "clamp to 255 + ignore unknown key 'x'"
    );
    // garbage → default (fail toward the intended default behavior).
    assert_eq!(parse_elect_config(b"????"), ElectConfig::BestGateway(MetricWeights::DEFAULT), "garbage → default");

    // ---- LAYER 2: co-channel seizes a proven off-channel owner (crown migration) -----------
    const MESH: u8 = 6;
    // co-channel board (mesh 6) vs owner on ch1 (the id5-was-ch1-crown ghost) → SEIZE.
    assert!(seize_off_channel_owner(true, MESH, 7, 5, 1), "co-channel seizes off-channel owner id5@ch1");
    // co-channel owner (ch == mesh) → do NOT seize (it's a valid crown).
    assert!(!seize_off_channel_owner(true, MESH, 7, 5, MESH), "never seize a co-channel owner");
    // owner channel unknown (0) → do NOT seize (fall through to liveness arms).
    assert!(!seize_off_channel_owner(true, MESH, 7, 5, 0), "unknown owner channel → no seize");
    // we are NOT co-channel → never seize (only the better board preempts).
    assert!(!seize_off_channel_owner(false, MESH, 7, 5, 1), "non-co-channel board never seizes");
    // our mesh channel unknown → never seize (safe).
    assert!(!seize_off_channel_owner(true, 0, 7, 5, 1), "unknown mesh channel → no seize");
    // owner is self → never seize.
    assert!(!seize_off_channel_owner(true, MESH, 7, 7, 1), "never seize self");

    // ---- LAYER 2 symmetric YIELD: off-channel board adopts a live co-channel owner (no flap) ----
    // id5 (off-channel, ch known) reading MC|7|6 (co-channel owner, alive) → YIELD (adopt id7).
    assert!(yield_to_co_channel_owner(true, false, MESH, 5, 7, MESH, true), "off-channel yields to live co-channel owner id7");
    // a CO-channel board never yields (it seizes instead) — mutually exclusive on co_channel.
    assert!(!yield_to_co_channel_owner(true, true, MESH, 5, 7, MESH, true), "co-channel board never yields");
    // owner is off-channel (ch1) → do NOT yield (that's a seize case for a co-channel board).
    assert!(!yield_to_co_channel_owner(true, false, MESH, 5, 7, 1, true), "don't yield to an off-channel owner");
    // co-channel owner but DEAD → do NOT yield (never follow a dead crown; fall through to takeover).
    assert!(!yield_to_co_channel_owner(true, false, MESH, 5, 7, MESH, false), "don't yield to a dead co-channel owner");
    // channel not yet known → do NOT yield (fail-safe until learned).
    assert!(!yield_to_co_channel_owner(false, false, MESH, 5, 7, MESH, true), "no yield until channel known");
    // owner is self → never yield.
    assert!(!yield_to_co_channel_owner(true, false, MESH, 7, 7, MESH, true), "never yield to self");

    // ---- reliability: refuse leaf-lock to a known off-channel owner (fixes the racy ~2/3 seize) ---
    // co-channel board (ap ch6) + owner on ch1 → REFUSE the lock (keep re-electing until seize).
    assert!(refuse_leaf_lock_off_channel(MESH, MESH, 1), "co-channel refuses lock to off-channel owner");
    // owner is co-channel (ch == mesh) → lock normally.
    assert!(!refuse_leaf_lock_off_channel(MESH, MESH, MESH), "lock to a co-channel owner");
    // owner channel unknown (0) → lock normally (fail-safe).
    assert!(!refuse_leaf_lock_off_channel(MESH, MESH, 0), "unknown owner channel → lock normally");
    // WE are not co-channel (ap ch1) → lock normally (we're not the better crown; follow the mesh).
    assert!(!refuse_leaf_lock_off_channel(1, MESH, 1), "non-co-channel board locks normally");
    // our AP channel unknown (0) → lock normally.
    assert!(!refuse_leaf_lock_off_channel(0, MESH, 1), "unknown own channel → lock normally");

    // ---- Legacy backoff is a byte-faithful 1:1 of the historical reelect_backoff_ms ---------
    assert_eq!(legacy_recovery_backoff_ms(-60, 5), 0 * ELECT_TIER_STEP_MS + 5 * 200, "legacy strong bucket 0");
    assert_eq!(legacy_recovery_backoff_ms(-70, 5), 1 * ELECT_TIER_STEP_MS + 5 * 200, "legacy mid bucket 1");
    assert_eq!(legacy_recovery_backoff_ms(-85, 5), 2 * ELECT_TIER_STEP_MS + 5 * 200, "legacy weak bucket 2");

    println!("election_verify: ALL CHECKS PASSED (co-channel dominance + weighted fitness + bounded/monotonic backoff tiering + config parser + legacy backoff)");
}
