//! Best-gateway election host guard. `#[path]`-includes the REAL `net/election.rs` (no drift) and
//! asserts the configurable fitness, the bounded fitness→backoff tiering, co-channel dominance (the
//! id5 ch1-vs-ch6 bug), and the config parser. `cargo run`.

#[path = "../../../rust/clock/src/net/election.rs"]
mod election;

use election::*;

fn inp(co_channel: bool, ap_rssi: i8, uptime_ms: u64) -> FitnessInputs {
    FitnessInputs { co_channel, ap_rssi, uptime_ms }
}

/// Build a `c<n>r<n>u<n>` lever payload into `buf`, returning the used slice. Hand-rolled rather
/// than `format!` so the sweep below allocates nothing and stays as cheap as the parser it drives.
fn fmt_cru(buf: &mut [u8; 24], c: u8, r: u8, u: u8) -> &[u8] {
    let mut n = 0;
    for (key, val) in [(b'c', c), (b'r', r), (b'u', u)] {
        buf[n] = key;
        n += 1;
        if val >= 100 {
            buf[n] = b'0' + val / 100;
            n += 1;
        }
        if val >= 10 {
            buf[n] = b'0' + (val / 10) % 10;
            n += 1;
        }
        buf[n] = b'0' + val % 10;
        n += 1;
    }
    &buf[..n]
}

fn main() {
    // The DOMINANT weights — what ships while `FOLLOW_ENABLED` is false. Named explicitly rather
    // than taken from a `DEFAULT`, because after #269 there is no single default: the weights are a
    // function of whether leaves can follow, and every caller has to say which world it is in.
    let w = MetricWeights::default_for(false);
    assert_eq!(w, MetricWeights::DOMINANT, "following off => the co-channel veto ships");
    assert!(!FOLLOW_ENABLED, "and the shipped flag IS off — #278 gates the flip on a fleet roll");

    // ---- gateway_fitness (weighted, higher = better) --------------------------------------
    // co-channel dominates: a co-channel board with the WORST usable RSSI still outscores the
    // BEST off-channel board (co=100 vs off-channel max = rssi 2·10 + uptime 2·1 = 22).
    let co_weak = gateway_fitness(&inp(true, -82, 0), &w);
    let off_best = gateway_fitness(&inp(false, -60, 10 * 60_000), &w);
    assert!(co_weak > off_best, "co-channel dominance: {co_weak} !> {off_best}");
    // full-signal co-channel board = max fitness for the default weights.
    assert_eq!(
        gateway_fitness(&inp(true, -60, 10 * 60_000), &w),
        MetricWeights::DOMINANT.max_fitness(),
        "full-signal co-channel = max fitness"
    );
    // RSSI + uptime order WITHIN co-channel.
    let co_strong = gateway_fitness(&inp(true, -60, 0), &w);
    assert!(co_strong > co_weak, "stronger rssi scores higher among co-channel");

    // ---- elect_backoff_ms: higher fitness → shorter wait; co-channel ALWAYS beats off-channel --
    // The exact id5 bug, numerically: a co-channel HIGH-id board must claim BEFORE a stronger
    // OFF-channel LOW-id board (backoff strictly smaller despite the higher node id).
    let co_hi_id = elect_backoff_ms(&inp(true, -70, 0), &w, 9);
    let off_lo_id = elect_backoff_ms(&inp(false, -55, 10 * 60_000), &w, 3);
    assert!(
        co_hi_id < off_lo_id,
        "co-channel id9 ({co_hi_id}ms) must claim before off-channel id3 ({off_lo_id}ms) — the id5 bug"
    );
    // Bounded: no board EVER waits longer than the legacy 0–30 s envelope (+ the sub-tier id term).
    let worst = elect_backoff_ms(&inp(false, -90, 0), &w, 254);
    assert!(
        worst <= MAX_ELECT_TIERS * ELECT_TIER_STEP_MS + 254 * 200,
        "backoff bounded to MAX_ELECT_TIERS: {worst}"
    );
    // Best board (co-channel, strong, long uptime) = tier 0 → only the sub-tier id term.
    assert_eq!(
        elect_backoff_ms(&inp(true, -55, 10 * 60_000), &w, 5),
        5 * 200,
        "best board waits only the node-id tiebreak"
    );
    // Monotonic in fitness: stronger co-channel never waits LONGER than a weaker co-channel (same id).
    let s = elect_backoff_ms(&inp(true, -60, 0), &w, 7);
    let x = elect_backoff_ms(&inp(true, -82, 0), &w, 7);
    assert!(s <= x, "monotonic: stronger ({s}) !<= weaker ({x})");
    // Tier gap ≥ one step so a weaker board gets an adopt-burst before its own claim window.
    let co_tier = elect_backoff_ms(&inp(true, -82, 0), &w, 0);
    let off_tier = elect_backoff_ms(&inp(false, -55, 0), &w, 0);
    assert!(
        off_tier.saturating_sub(co_tier) >= ELECT_TIER_STEP_MS,
        "co-channel vs off-channel separated by >= one tier ({co_tier} .. {off_tier})"
    );

    // ---- re-weighting via config changes ordering -----------------------------------------
    // RSSI-dominant with co-channel OFF: now the stronger board wins regardless of channel.
    let rssi_only = match parse_elect_config(b"c0r100", true) {
        ElectConfig::BestGateway(w) => w,
        _ => panic!("expected BestGateway"),
    };
    let a = elect_backoff_ms(&inp(true, -85, 0), &rssi_only, 1); // co-channel but WEAK
    let b = elect_backoff_ms(&inp(false, -55, 0), &rssi_only, 1); // off-channel but STRONG
    assert!(b < a, "rssi-dominant config: strong off-channel now beats weak co-channel");

    // ---- parse_elect_config ----------------------------------------------------------------
    assert_eq!(parse_elect_config(b"", false), ElectConfig::BestGateway(MetricWeights::DOMINANT), "empty → default (best-gateway ON)");
    assert_eq!(parse_elect_config(b"   ", false), ElectConfig::BestGateway(MetricWeights::DOMINANT), "whitespace → default");
    assert_eq!(parse_elect_config(b"legacy", false), ElectConfig::Legacy, "legacy keyword → escape hatch");
    assert_eq!(parse_elect_config(b"LEGACY", false), ElectConfig::Legacy, "case-insensitive legacy");
    assert_eq!(parse_elect_config(b"c100r10n5u1", false), ElectConfig::BestGateway(MetricWeights::DOMINANT), "explicit default weights");
    // subset: missing keys inherit DEFAULT.
    assert_eq!(
        parse_elect_config(b"r20", false),
        ElectConfig::BestGateway(MetricWeights { co_channel: 100, rssi: 20, uptime: 1 }),
        "subset inherits default for missing keys"
    );
    // clamp + ignore junk.
    assert_eq!(
        parse_elect_config(b"c999x7r3", false),
        ElectConfig::BestGateway(MetricWeights { co_channel: 255, rssi: 3, uptime: 1 }),
        "clamp to 255 + ignore unknown key 'x'"
    );
    // garbage → default (fail toward the intended default behavior).
    assert_eq!(parse_elect_config(b"????", false), ElectConfig::BestGateway(MetricWeights::DOMINANT), "garbage → default");

    // ---- #269 THE id5 INVARIANT, asserted in BOTH flag states -------------------------------
    // The numeric backoff assertion above is the id5 bug in the world it was found in — a fleet
    // that CANNOT follow a crown to another channel. It does not survive the re-scale, and it
    // should not: under following, an off-channel crown is a migration rather than a stranding.
    //
    // So the property that holds in both worlds is stated once, over the harm rather than over the
    // mechanism: an off-channel crown strands the fleet iff leaves cannot follow it AND co-channel
    // does not dominate. Inability to follow is exactly what the flag names, which is why one
    // predicate covers both states instead of two unrelated assertions that only look like a pair.
    for follow in [false, true] {
        let w = MetricWeights::default_for(follow);
        assert!(
            !off_channel_crown_can_strand(&w, follow),
            "id5 cannot recur with follow={follow} (weights {w:?})"
        );
    }
    // …and the two states get there by DIFFERENT means, which is the whole content of the coupling:
    assert!(
        co_channel_dominates(&MetricWeights::DOMINANT),
        "following off: the veto is what prevents the stranding"
    );
    assert!(
        !co_channel_dominates(&MetricWeights::FOLLOWING),
        "following on: the veto is deliberately GONE — otherwise #269 could never migrate"
    );

    // The margin is exactly one RSSI bucket, which is the derivation the comment claims.
    assert_eq!(
        MetricWeights::FOLLOWING.co_channel, MetricWeights::FOLLOWING.rssi,
        "margin == one rssi bucket == the jitter amplitude at the −65/−78 boundaries"
    );
    // A ONE-bucket challenger TIES (jitter must not move the fleet); TWO buckets WIN (a real gain
    // must). −60 is bucket 2, −70 bucket 1, −85 bucket 0; uptime held equal so only rssi varies.
    let f = MetricWeights::FOLLOWING;
    let co_1bucket = gateway_fitness(&inp(true, -85, 0), &f); // co-channel, bucket 0
    let off_1bucket = gateway_fitness(&inp(false, -70, 0), &f); // off-channel, bucket 1
    let off_2bucket = gateway_fitness(&inp(false, -60, 0), &f); // off-channel, bucket 2
    assert_eq!(co_1bucket, off_1bucket, "a one-bucket challenger only TIES: {co_1bucket} vs {off_1bucket}");
    assert!(off_2bucket > co_1bucket, "a two-bucket challenger WINS: {off_2bucket} > {co_1bucket}");
    // The post-re-scale max, named because the `10/122 = 8.2%` unit error used the PRE-re-scale
    // denominator (which still contained the co_channel: 100 being removed). The real share is
    // 10/32 ≈ 31%, ~4x batman-adv's ~8% — a deliberate divergence, not a convergence.
    assert_eq!(f.max_fitness(), 32, "post-re-scale max_fitness");

    // ---- #269 THE RUNTIME LEVER CANNOT RE-ARM THE DISEASE -----------------------------------
    // The retained `smol/mesh/elect` topic is a SECOND WRITER of these weights and needs no code
    // change to demote co-channel dominance — `c10r10` does it. A compile-time assert binds the
    // other writer and says nothing about this one. Asserted over a SWEEP rather than the handful
    // of payloads someone thought of, because "the cases I imagined" is how the first version of
    // this invariant would have been passed.
    for c in [0u8, 1, 9, 10, 22, 23, 100, 200, 255] {
        for r in [0u8, 1, 10, 50, 63, 64, 200, 255] {
            for u in [0u8, 1, 63, 255] {
                let mut buf = [0u8; 24];
                let payload = fmt_cru(&mut buf, c, r, u);
                match parse_elect_config(payload, false) {
                    ElectConfig::BestGateway(got) => assert!(
                        co_channel_dominates(&got),
                        "following OFF: `{}` must not be able to demote dominance — got {got:?}",
                        core::str::from_utf8(payload).unwrap()
                    ),
                    ElectConfig::Legacy => panic!("keyed weights are never Legacy"),
                }
                // With following ON the operator gets what they asked for: the clamp exists to
                // protect a fleet that cannot follow, and imposing it in both worlds would silently
                // veto exactly the migration #269 is for.
                match parse_elect_config(payload, true) {
                    ElectConfig::BestGateway(got) => {
                        assert_eq!(got.co_channel, c, "following ON: `c{c}` applied verbatim");
                        assert_eq!(got.rssi, r, "following ON: `r{r}` applied verbatim");
                        assert_eq!(got.uptime, u, "following ON: `u{u}` applied verbatim");
                    }
                    ElectConfig::Legacy => panic!("keyed weights are never Legacy"),
                }
            }
        }
    }
    // The worked example from the finding: `c10r10` is precisely the payload that re-arms id5.
    let clamped = match parse_elect_config(b"c10r10", false) {
        ElectConfig::BestGateway(w) => w,
        _ => panic!("expected BestGateway"),
    };
    assert_eq!(clamped.co_channel, 23, "c10r10 → co_channel clamped up to 2·10 + 2·1 + 1");
    assert!(co_channel_dominates(&clamped));
    // An extreme lever is capped so the floor stays EXPRESSIBLE in a u8, rather than clamped
    // toward a dominance that no legal `co_channel` could reach.
    let capped = match parse_elect_config(b"c255r200u200", false) {
        ElectConfig::BestGateway(w) => w,
        _ => panic!("expected BestGateway"),
    };
    assert_eq!((capped.rssi, capped.uptime), (DOMINANCE_LEVER_CAP, DOMINANCE_LEVER_CAP));
    assert!(co_channel_dominates(&capped), "even at the cap, dominance holds");

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

    println!("election_verify: ALL CHECKS PASSED (co-channel dominance + weighted fitness + bounded/monotonic backoff tiering + config parser + legacy backoff + #269 id5-in-both-flag-states + the one-bucket margin + a 288-payload sweep proving the lever cannot re-arm the disease)");
}
