//! Host verification of the PURE #13 managed-flood core. Includes the real
//! `net/flood.rs` verbatim (#[path], no drift) and exercises the seen-set, the
//! forward decision, and the hop-latch hysteresis. Run: `cargo run` — panics on failure.

#[path = "../../../rust/clock/src/net/flood.rs"]
mod flood;

use flood::{
    forward_decision, ChannelPark, ForwardAction, HopLatch, SeenSet, ESCALATE_STREAK, MAX_HOP,
    PARK_BURSTS_PER_DWELL, PARK_BURST_EVERY_MS, PARK_CHANNELS, PARK_DWELL_MS, PARK_SILENCE_MS,
    PROBE_EVERY, SEEN_RING, UNLATCH_STREAK,
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

    // --- #126 ChannelPark: engage / disengage from the latch -------------
    let mut p = ChannelPark::new();
    assert!(!p.engaged(), "fresh park is inert");
    assert_eq!(p.channel(), None, "idle ⇒ None ⇒ blind scan governs");
    assert!(!p.should_burst(0), "idle never bursts");
    p.on_feedback(0);
    assert!(!p.engaged(), "feedback while idle is ignored (single-hop ACKs aren't park signals)");
    // latched ⇒ engage, sweeping from candidate 0.
    p.sync(true, 0);
    assert!(p.engaged() && !p.parked(), "latched ⇒ sweeping");
    assert_eq!(p.channel(), Some(PARK_CHANNELS[0]), "sweep starts on candidate 0");
    // un-latched (uplink recovered) ⇒ disengage back to the blind scan.
    p.sync(false, 10);
    assert!(!p.engaged(), "un-latch disengages parking");
    assert_eq!(p.channel(), None, "disengaged ⇒ blind scan governs again");

    // --- ChannelPark: burst bootstrap (spacing + per-dwell cap) -----------
    let mut sp = ChannelPark::new();
    sp.sync(true, 0);
    assert!(sp.should_burst(0), "first burst of a dwell fires immediately");
    assert!(!sp.should_burst(PARK_BURST_EVERY_MS - 1), "next burst gated until PARK_BURST_EVERY_MS");
    assert!(sp.should_burst(PARK_BURST_EVERY_MS), "next burst fires after the spacing gap");
    // exactly PARK_BURSTS_PER_DWELL bursts fire per dwell, the rest are capped.
    let mut cap = ChannelPark::new();
    cap.sync(true, 0);
    let mut fired = 0u8;
    let mut t = 0u64;
    for _ in 0..(PARK_BURSTS_PER_DWELL + 3) {
        if cap.should_burst(t) {
            fired += 1;
        }
        t += PARK_BURST_EVERY_MS;
    }
    assert_eq!(fired, PARK_BURSTS_PER_DWELL, "exactly PARK_BURSTS_PER_DWELL bursts per dwell");

    // --- ChannelPark: TRIGGER CONTRACT (the #123 lesson) ------------------
    // The emit-trigger wiring is where on-air bugs lived. Contract: the sweep must NOT hop off a
    // candidate until ≥1 burst has actually probed it (no channel skipped), and it must visit
    // 1→6→11→1 in order. This exercises the exact should_burst()-then-tick() call sequence the
    // live `leaf_scan_tick` uses.
    let mut noburst = ChannelPark::new();
    noburst.sync(true, 0);
    noburst.tick(PARK_DWELL_MS * 4); // dwell long past, but ZERO bursts fired
    assert_eq!(noburst.channel(), Some(PARK_CHANNELS[0]), "no hop until ≥1 burst probes the candidate");
    assert!(noburst.should_burst(PARK_DWELL_MS * 4), "now probe it");
    noburst.tick(PARK_DWELL_MS * 4 + PARK_DWELL_MS); // dwell elapsed AND probed ⇒ hop
    assert_eq!(noburst.channel(), Some(PARK_CHANNELS[1]), "hops once the candidate has been probed");
    // full rotation 1→6→11→1, one burst + one full dwell per candidate.
    let mut sw = ChannelPark::new();
    sw.sync(true, 0);
    let mut clk = 0u64;
    for expect in [PARK_CHANNELS[0], PARK_CHANNELS[1], PARK_CHANNELS[2], PARK_CHANNELS[0]] {
        assert_eq!(sw.channel(), Some(expect), "sweep visits each candidate in [1,6,11] order");
        assert!(sw.should_burst(clk), "≥1 burst per candidate before hopping");
        clk += PARK_DWELL_MS;
        sw.tick(clk);
    }

    // --- ChannelPark: park on feedback, hold, un-park on silence ----------
    let mut pk = ChannelPark::new();
    pk.sync(true, 0);
    assert!(pk.should_burst(0));
    pk.tick(PARK_DWELL_MS); // hop to candidate 1 (ch6)
    assert_eq!(pk.channel(), Some(PARK_CHANNELS[1]));
    pk.on_feedback(PARK_DWELL_MS); // a RELAYACK2 came back on ch6 ⇒ PARK
    assert!(pk.parked(), "feedback while sweeping parks on the current channel");
    assert_eq!(pk.channel(), Some(PARK_CHANNELS[1]), "parked on the channel that drew the ACK");
    assert!(!pk.should_burst(PARK_DWELL_MS + 100), "parked ⇒ no bursts (telemetry carries the held channel)");
    // holds well within the silence window; a refresh resets the silence clock.
    pk.tick(PARK_DWELL_MS + PARK_SILENCE_MS - 1);
    assert!(pk.parked(), "still parked within PARK_SILENCE_MS");
    pk.on_feedback(PARK_DWELL_MS + PARK_SILENCE_MS - 1); // refresh
    pk.tick(PARK_DWELL_MS + PARK_SILENCE_MS - 1 + PARK_SILENCE_MS - 1);
    assert!(pk.parked(), "a refresh keeps it parked past the original window");
    // sustained silence past PARK_SILENCE_MS ⇒ channel went cold ⇒ resume sweeping from it.
    let last_fb = PARK_DWELL_MS + PARK_SILENCE_MS - 1;
    pk.tick(last_fb + PARK_SILENCE_MS + 1);
    assert!(pk.engaged() && !pk.parked(), "cold parked channel ⇒ resume sweeping");
    assert_eq!(pk.channel(), Some(PARK_CHANNELS[1]), "re-sweep starts from the last good channel");

    // --- ChannelPark: #76 re-election restarts the sweep ------------------
    let mut rc = ChannelPark::new();
    rc.sync(true, 0);
    rc.should_burst(0);
    rc.tick(PARK_DWELL_MS); // → candidate 1
    rc.should_burst(PARK_DWELL_MS);
    rc.tick(PARK_DWELL_MS * 2); // → candidate 2
    assert_eq!(rc.channel(), Some(PARK_CHANNELS[2]));
    rc.on_rechannel(PARK_DWELL_MS * 2 + 50); // owner/channel changed under us
    assert!(rc.engaged() && !rc.parked(), "re-election ⇒ back to sweeping");
    assert_eq!(rc.channel(), Some(PARK_CHANNELS[0]), "#76 re-election restarts the sweep at candidate 0");
    let mut rc2 = ChannelPark::new();
    rc2.on_rechannel(0);
    assert!(!rc2.engaged(), "on_rechannel while idle stays idle (not stranded)");

    // --- ChannelPark: INVARIANT — a never-latched leaf is fully inert ------
    // (mirrors HopLatch's fwd=0 gate: no parking, no bursts, feedback ignored, channel None).
    let mut inert = ChannelPark::new();
    for t in [0u64, 500, PARK_DWELL_MS, PARK_DWELL_MS * 2, PARK_SILENCE_MS] {
        inert.sync(false, t); // healthy leaf: never latched
        assert!(!inert.should_burst(t), "inert leaf never bursts");
        inert.tick(t);
        inert.on_feedback(t);
        assert_eq!(inert.channel(), None, "inert leaf never overrides the blind scan");
        assert!(!inert.engaged(), "inert leaf never engages parking");
    }

    println!("flood_verify: ALL CHECKS PASSED (SeenSet + forward_decision + HopLatch + #126 ChannelPark)");
}
