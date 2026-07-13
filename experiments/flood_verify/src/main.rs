//! Host verification of the PURE #13 managed-flood core. Includes the real
//! `net/flood.rs` verbatim (#[path], no drift) and exercises the seen-set, the
//! forward decision, and the hop-latch hysteresis. Run: `cargo run` — panics on failure.

#[path = "../../../rust/clock/src/net/flood.rs"]
mod flood;

use flood::{
    forward_decision, ForwardAction, HopLatch, SeenSet, MAX_HOP, PROBE_EVERY, SEEN_RING,
    UNLATCH_STREAK,
};

fn main() {
    // --- SeenSet ----------------------------------------------------------
    let mut s = SeenSet::new();
    assert!(!s.contains(7, 100), "empty");
    assert!(!s.seen_or_insert(7, 100), "first sight is new");
    assert!(s.contains(7, 100), "recorded");
    assert!(s.seen_or_insert(7, 100), "second sight is a dup");
    assert!(!s.seen_or_insert(7, 101), "diff msgid is new");
    assert!(!s.seen_or_insert(8, 100), "diff origin is new");
    // idempotent insert doesn't consume extra ring slots
    s.insert(7, 100);
    s.insert(7, 100);
    // drop-oldest overflow: fill past capacity, the earliest ages out.
    let mut r = SeenSet::new();
    for m in 0..(SEEN_RING as u16 + 5) {
        assert!(!r.seen_or_insert(1, m), "each new msgid is new on insert");
    }
    assert!(!r.contains(1, 0), "oldest (msgid 0) aged out after overflow");
    assert!(r.contains(1, SEEN_RING as u16 + 4), "newest retained");

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

    // --- HopLatch: escalation ---------------------------------------------
    let mut l = HopLatch::new();
    assert!(!l.latched());
    assert_eq!(l.origin_hop(false), 1, "single-hop by default (plain RELAY)");
    // a partially-acked exhaust does NOT latch (gateway heard us, just lossy).
    l.on_relay_exhausted(true);
    assert!(!l.latched(), "partial ack ⇒ not stranded");
    // a fully-unacked exhaust latches multi-hop.
    l.on_relay_exhausted(false);
    assert!(l.latched(), "zero ack ⇒ stranded ⇒ latch");
    assert_eq!(l.origin_hop(false), MAX_HOP, "latched non-probe emits at MAX_HOP (RELAY2)");
    assert_eq!(l.origin_hop(true), 1, "a probe always emits H=1 (plain RELAY)");

    // --- HopLatch: probe gating (Gate A = downlink_up) --------------------
    let mut l2 = HopLatch::new();
    l2.on_relay_exhausted(false); // latch
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
    l3.on_relay_exhausted(false);
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

    println!("flood_verify: ALL CHECKS PASSED (SeenSet + forward_decision + HopLatch)");
}
