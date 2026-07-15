//! Host verification of the PURE #13 managed-flood core. Includes the real
//! `net/flood.rs` verbatim (#[path], no drift) and exercises the seen-set, the
//! forward decision, and the hop-latch hysteresis. Run: `cargo run` — panics on failure.

#[path = "../../../rust/clock/src/net/flood.rs"]
mod flood;

use flood::{
    forward_decision, ChannelPark, ForwardAction, HopLatch, SeenSet, ESCALATE_STREAK, MAX_HOP,
    PARK_DWELL_MS, PARK_STALE_MS, PROBE_EVERY, SEEN_RING, UNLATCH_STREAK,
};

/// Drive a fresh latch into multi-hop the way a genuinely-stranded leaf does: ESCALATE_STREAK
/// consecutive fully-un-ACKed messages.
fn latch_via_stranding(l: &mut HopLatch) {
    for _ in 0..ESCALATE_STREAK {
        l.on_relay_exhausted(false);
    }
}

fn main() {
    // --- SeenSet (keyed per-FRAGMENT: origin, msgid, frag) ----------------
    let mut s = SeenSet::new();
    assert!(!s.contains(7, 100, 0), "empty");
    assert!(!s.seen_or_insert(7, 100, 0), "first sight is new");
    assert!(s.contains(7, 100, 0), "recorded");
    assert!(s.seen_or_insert(7, 100, 0), "second sight is a dup");
    assert!(!s.seen_or_insert(7, 101, 0), "diff msgid is new");
    assert!(!s.seen_or_insert(8, 100, 0), "diff origin is new");
    // REGRESSION (the multi-fragment bug): a DIFFERENT fragment of the SAME message is
    // NOT a dup — else a relay drops frags 1..N and the gateway never reassembles a
    // multi-fragment message. Each fragment must be independently forward-once.
    assert!(!s.seen_or_insert(7, 100, 1), "frag 1 of a seen message is NEW (multi-frag)");
    assert!(!s.seen_or_insert(7, 100, 2), "frag 2 of a seen message is NEW (multi-frag)");
    assert!(s.seen_or_insert(7, 100, 1), "re-heard frag 1 is a dup");
    // idempotent insert doesn't consume extra ring slots
    s.insert(7, 100, 0);
    s.insert(7, 100, 0);
    // drop-oldest overflow: fill past capacity, the earliest ages out.
    let mut r = SeenSet::new();
    for m in 0..(SEEN_RING as u16 + 5) {
        assert!(!r.seen_or_insert(1, m, 0), "each new msgid is new on insert");
    }
    assert!(!r.contains(1, 0, 0), "oldest (msgid 0) aged out after overflow");
    assert!(r.contains(1, SEEN_RING as u16 + 4, 0), "newest retained");

    // --- forward_decision -------------------------------------------------
    // already-seen → DedupDrop regardless of role/hop.
    assert_eq!(forward_decision(false, 2, true), ForwardAction::DedupDrop);
    assert_eq!(forward_decision(true, 2, true), ForwardAction::DedupDrop);
    // gateway (sink) → Reassemble, never forward.
    assert_eq!(forward_decision(true, 2, false), ForwardAction::Reassemble);
    assert_eq!(forward_decision(true, 1, false), ForwardAction::Reassemble);
    // relay with hops left → Forward at hop-1.
    assert_eq!(forward_decision(false, 2, false), ForwardAction::Forward { hop: 1 });
    assert_eq!(forward_decision(false, 3, false), ForwardAction::Forward { hop: 2 });
    // relay, hop exhausted (<=1) → TtlDrop (a RELAY2 that decremented to 1 at a
    // non-gateway means it never reached the sink → drop, count ttl_drops).
    assert_eq!(forward_decision(false, 1, false), ForwardAction::TtlDrop);
    assert_eq!(forward_decision(false, 0, false), ForwardAction::TtlDrop);

    // --- HopLatch: escalation (CONSECUTIVE-un-ACK hysteresis) -------------
    let mut l = HopLatch::new();
    assert!(!l.latched());
    assert_eq!(l.origin_hop(false), 1, "single-hop by default (plain RELAY)");
    // a partially-acked exhaust does NOT latch (gateway heard us, just lossy).
    l.on_relay_exhausted(true);
    assert!(!l.latched(), "partial ack ⇒ not stranded");
    // REGRESSION (C0 canary): fewer than ESCALATE_STREAK CONSECUTIVE full-un-ACKs must NOT latch —
    // a single transient full-loss in a healthy all-hear mesh can't be allowed to escalate (that
    // was the bug the bench caught: one dropped msg → hop=2 → forward-swarm → fwd!=0).
    for i in 1..ESCALATE_STREAK {
        l.on_relay_exhausted(false);
        assert!(!l.latched(), "{i} consecutive full-un-ACKs (< ESCALATE_STREAK) ⇒ no latch");
    }
    // any ACK RESETS the streak — so losses must be CONSECUTIVE, not merely cumulative.
    l.on_uplink_progress();
    for _ in 1..ESCALATE_STREAK {
        l.on_relay_exhausted(false);
    }
    assert!(!l.latched(), "progress reset the streak ⇒ still no latch");
    // ESCALATE_STREAK consecutive full-un-ACKs (genuine stranding) DO latch.
    l.on_uplink_progress();
    latch_via_stranding(&mut l);
    assert!(l.latched(), "ESCALATE_STREAK consecutive full-un-ACKs ⇒ stranded ⇒ latch");
    assert_eq!(l.origin_hop(false), MAX_HOP, "latched non-probe emits at MAX_HOP (RELAY2)");
    assert_eq!(l.origin_hop(true), 1, "a probe always emits H=1 (plain RELAY)");

    // --- HopLatch: probe gating (Gate A = downlink_up) --------------------
    let mut l2 = HopLatch::new();
    latch_via_stranding(&mut l2); // latch
    // while downlink is DOWN, never probe (don't waste airtime — definitely stranded).
    for _ in 0..(PROBE_EVERY * 3) {
        assert!(!l2.should_probe(false), "no probe while downlink down");
    }
    // with downlink UP, probe fires on the 1-in-PROBE_EVERY tick.
    let mut probes = 0;
    for _ in 0..(PROBE_EVERY * 2) {
        if l2.should_probe(true) {
            probes += 1;
        }
    }
    assert_eq!(probes, 2, "exactly 2 probes across 2×PROBE_EVERY ticks");

    // --- HopLatch: un-latch hysteresis (no flap) --------------------------
    let mut l3 = HopLatch::new();
    latch_via_stranding(&mut l3);
    assert!(l3.latched());
    l3.on_direct_ack(); // streak 1 of UNLATCH_STREAK
    assert!(l3.latched(), "one direct ack is not enough (hysteresis)");
    l3.on_probe_miss(); // marginal RF: a miss resets the streak
    assert!(l3.latched(), "still latched after a miss");
    // now K consecutive good probes → unlatch.
    for _ in 0..UNLATCH_STREAK {
        l3.on_direct_ack();
    }
    assert!(!l3.latched(), "UNLATCH_STREAK consecutive direct acks drop the latch");
    assert_eq!(l3.origin_hop(false), 1, "back to single-hop");
    // a not-latched direct ack is a harmless no-op.
    l3.on_direct_ack();
    assert!(!l3.latched());

    // --- #126 ChannelPark: latched-leaf channel parking ------------------
    // The #123 LESSON: last campaign only the HopLatch MATH was host-tested, not the trigger WIRING,
    // so an on-air bug slipped past green builds. ChannelPark owns the ENTIRE latched-leaf channel
    // decision (poll = the channel to be on; on_signal = a relay-echo/RELAYACK2 arrived; reset =
    // un-latched), so exercising these here IS the wiring coverage — mode.rs only applies + feeds.
    const C: [u8; 3] = [1, 6, 11]; // must match mode.rs's CANDIDATES

    // (a) BOOTSTRAP HUNT: with no signal yet, round-robin the candidates every PARK_DWELL_MS. poll
    //     returns Some only on an actual change (so the radio re-tunes exactly as sparsely as the
    //     round-robin), None mid-dwell.
    let mut p = ChannelPark::new();
    assert_eq!(p.poll(0, &C), Some(1), "first selection tunes to candidate 0");
    assert_eq!(p.poll(500, &C), None, "mid-dwell: no re-tune");
    assert_eq!(p.poll(PARK_DWELL_MS, &C), Some(6), "dwell elapsed ⇒ hop to candidate 1");
    assert_eq!(p.poll(2 * PARK_DWELL_MS, &C), Some(11), "hop to candidate 2");
    assert_eq!(p.poll(3 * PARK_DWELL_MS, &C), Some(1), "round-robin wraps to candidate 0");
    assert_eq!(p.parked(), None, "no signal yet ⇒ not parked (still hunting)");

    // (b) PARK ON SIGNAL: a relay echoed our UP2 (or a RELAYACK2 for us arrived) while on candidate 0
    //     ⇒ park there. Subsequent polls DWELL on it (no hop) for the whole freshness window.
    p.on_signal(3 * PARK_DWELL_MS); // signalled while current == candidate 0 (from the poll above)
    assert_eq!(p.parked(), Some(1), "signal ⇒ parked on the channel we're physically on");
    assert_eq!(p.poll(4 * PARK_DWELL_MS, &C), None, "parked+fresh ⇒ dwell, do NOT hop");
    assert_eq!(p.poll(3 * PARK_DWELL_MS + PARK_STALE_MS - 1, &C), None, "still parked just under stale");
    assert_eq!(p.parked(), Some(1), "park held across the fresh window");

    // (c) FRESHNESS REFRESH: another signal while parked extends the window (a leaf that keeps
    //     drawing ACKs stays parked indefinitely).
    let t_refresh = 4 * PARK_DWELL_MS;
    p.on_signal(t_refresh);
    assert_eq!(p.poll(t_refresh + PARK_STALE_MS - 1, &C), None, "refreshed park still fresh");
    assert_eq!(p.parked(), Some(1), "refresh held the park");

    // (d) STALENESS RECOVERY: no signal for PARK_STALE_MS ⇒ forget the park and resume the hunt
    //     (self-healing when the relay roams / dies).
    let after_stale = t_refresh + PARK_STALE_MS;
    let ch = p.poll(after_stale, &C).expect("stale park ⇒ re-tune (resume hunt)");
    assert_ne!(ch, 1, "resumed hunt moves off the stale parked channel");
    assert_eq!(p.parked(), None, "stale park cleared");

    // (e) RESET (un-latch): forget the park immediately; the next poll re-bootstraps from ch 0.
    p.on_signal(after_stale); // re-park first
    assert!(p.parked().is_some(), "re-parked");
    p.reset();
    assert_eq!(p.parked(), None, "reset (un-latch) clears the park");
    assert!(p.poll(after_stale + 10 * PARK_DWELL_MS, &C).is_some(), "post-reset re-tune (re-bootstrap)");

    // (f) DEFENSIVE: a signal before any poll (current == unset sentinel) must NOT park on 0.
    let mut q = ChannelPark::new();
    q.on_signal(100);
    assert_eq!(q.parked(), None, "no park on the unset-channel sentinel");

    println!("flood_verify: ALL CHECKS PASSED (SeenSet + forward_decision + HopLatch + ChannelPark)");
}
