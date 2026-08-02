//! SMOL-ONLY half of the guard (#278 stage 2) — the announce schedule, the leaf follow state, the
//! recovery ladder, and the send-path seal. None of this exists in the donor crate, so it is kept in
//! its own file: `consensus.rs` and `wire_tests.rs` stay verbatim-diffable against
//! `esp32c6-watch:crates/mesh-elect/tests/`, exactly as the source file itself is split.

use crate::mesh_elect::{
    recovery_ladder, wire, Announcer, Decision, Follow, Follower, GroupMacSink, Phase, SealedElect,
    ANNOUNCE_BURST, ANNOUNCE_GAP_MS, ANNOUNCE_IDLE_MS, COMMON_AP_CHANNELS, FOLLOW_ENABLED,
    LEGACY_CANDIDATES, N_CHANNELS, PROBATION_MS, RENDEZVOUS_CHANNEL, SETTLE_MS,
};

fn frame(node_id: u8, epoch: u32, channel: u8, gateway: u8) -> wire::ElectFrame {
    wire::ElectFrame { node_id, epoch, channel, gateway, w: [0u8; N_CHANNELS] }
}

/// The whole point of the CSA shape: leaves learn the target while the crown is still reachable,
/// and again from the far side. Both bursts carry the SAME epoch, so a late pre-move frame is
/// byte-identical to a post-move one and has nothing to override.
pub fn announces_before_and_after_the_move() {
    let mut a = Announcer::new();
    assert_eq!(a.phase(), Phase::Idle, "boot: nothing to announce");
    assert!(!a.due(0), "an idle announcer never emits");

    let t0 = 10_000u64;
    assert!(a.decide(11, t0), "a real channel decision arms the pre-move burst");
    assert_eq!(a.epoch(), 1, "first decision is epoch 1");
    assert_eq!(a.phase(), Phase::Pre);
    let pre_epoch = a.epoch();

    for i in 0..ANNOUNCE_BURST {
        let t = t0 + u64::from(i) * ANNOUNCE_GAP_MS;
        assert!(a.due(t), "pre-move frame {i} is due at {t}");
        assert!(!a.due(t), "a frame is consumed once, not twice");
        assert!(!a.clear_to_move() || i == ANNOUNCE_BURST - 1, "no move mid-burst");
    }
    assert!(a.clear_to_move(), "pre-move burst drained → the caller may retune");
    assert!(!a.due(t0 + 10 * ANNOUNCE_GAP_MS), "a drained burst stops emitting");

    let t1 = t0 + 5_000;
    a.moved(t1);
    assert_eq!(a.phase(), Phase::Post);
    assert_eq!(a.epoch(), pre_epoch, "the post-move burst carries the SAME epoch");
    assert!(!a.settled(), "the post burst has not drained yet");
    for i in 0..ANNOUNCE_BURST {
        assert!(a.due(t1 + u64::from(i) * ANNOUNCE_GAP_MS), "post-move frame {i} is due");
    }
    assert!(a.settled(), "migration over once both bursts have drained");

    let d = a.decision(8);
    assert_eq!(d, Decision { channel: 11, epoch: 1, gateway: 8 }, "the announced decision");
}

/// Driving `decide` from an OBSERVED channel means it gets called every tick with the same value.
/// Burning an epoch per call would make every announcement out-rank the last and defeat
/// `supersedes` entirely, so a redundant decision must be a no-op.
pub fn a_redundant_decision_does_not_burn_an_epoch() {
    let mut a = Announcer::new();
    assert!(a.decide(6, 0));
    assert_eq!(a.epoch(), 1);
    for t in 0..50u64 {
        assert!(!a.decide(6, t * 100), "re-deciding the same channel changes nothing");
    }
    assert_eq!(a.epoch(), 1, "epoch is spent on real moves only");
    assert!(a.decide(11, 5_000), "a genuinely new channel does advance it");
    assert_eq!(a.epoch(), 2);
}

/// A corrupt or unknown channel must never reach the air — the frame builder would encode it and
/// every leaf that honoured it would tune to nothing.
pub fn refuses_an_out_of_range_channel() {
    let mut a = Announcer::new();
    for bad in [0u8, 14, 99, 255] {
        assert!(!a.decide(bad, 0), "ch{bad} is not a 2.4 GHz channel");
        assert_eq!(a.epoch(), 0, "a rejected decision does not advance the epoch");
        assert_eq!(a.phase(), Phase::Idle);
    }
}

/// Epoch orders announcements; a replay of an older one is inert. Critically it must ALSO not look
/// like liveness — a replayed frame resetting the probation clock would let an attacker (or a stuck
/// repeater) hold the fleet on a dead epoch forever with no new information.
pub fn orders_by_epoch_and_ignores_replay() {
    let mut f = Follower::new();
    assert_eq!(f.accepted(), 0, "boot: nothing heard");
    assert_eq!(f.decision(), Decision::bootstrap());

    assert_eq!(f.observe(&frame(8, 3, 11, 8), 1_000, 6), Follow::Move(11), "newer epoch → move");
    assert_eq!(f.decision().epoch, 3);
    assert_eq!(f.accepted(), 1);

    assert_eq!(f.observe(&frame(8, 3, 11, 8), 2_000, 11), Follow::Stale, "same epoch is not newer");
    assert_eq!(f.observe(&frame(9, 1, 1, 9), 3_000, 11), Follow::Stale, "an older epoch is a replay");
    assert_eq!(f.accepted(), 1, "a rejected frame is not counted as heard");
    assert_eq!(f.decision().channel, 11, "a replay cannot drag the fleet back");

    assert_eq!(f.observe(&frame(8, 4, 11, 8), 4_000, 11), Follow::Confirmed, "already on ch11");
    assert_eq!(f.accepted(), 2);
}

/// `m` is the number the canary roll reads, and it is NOT `n`. In the steady state every leaf is
/// co-channel with the crown (ESP-NOW is per-channel — a leaf elsewhere hears nothing), so `n`
/// only ever proves the beacon works. `m` counts announcements naming a different channel, which
/// is the crown's PRE-move burst being caught — the one hard-to-hit part of the design.
pub fn counts_would_be_moves_separately_from_hearings() {
    let mut f = Follower::new();
    assert_eq!((f.accepted(), f.moves()), (0, 0));

    // Steady state on ch6: heard, but nothing to act on.
    for (i, ep) in (1..=5u32).enumerate() {
        assert_eq!(f.observe(&frame(8, ep, 6, 8), 1_000 * (i as u64 + 1), 6), Follow::Confirmed);
    }
    assert_eq!((f.accepted(), f.moves()), (5, 0), "beacon proven, no move seen");

    // The crown's pre-move burst, heard on the OLD channel: this is the observation that matters.
    assert_eq!(f.observe(&frame(8, 6, 11, 8), 10_000, 6), Follow::Move(11));
    assert_eq!((f.accepted(), f.moves()), (6, 1), "one would-be move");

    // Post-move repeats from the new channel are confirmations, not further moves.
    assert_eq!(f.observe(&frame(8, 7, 11, 8), 11_000, 11), Follow::Confirmed);
    assert_eq!((f.accepted(), f.moves()), (7, 1), "a confirmation is not a second move");
}

/// The anti-flap gate on the OBSERVED channel. A crown reads its channel off the radio every
/// subtick; an AP that oscillates must not burn an epoch per oscillation, because epoch is the
/// total order and every announcement would then out-rank the last.
pub fn an_observed_channel_must_settle_before_it_costs_an_epoch() {
    let mut a = Announcer::new();

    // Cold start commits IMMEDIATELY — no incumbent to protect, and a booting crown must not sit
    // silent for SETTLE_MS in exactly the window leaves are looking hardest.
    assert!(a.observe_channel(6, 0), "the first channel commits at once");
    assert_eq!((a.epoch(), a.channel()), (1, 6));

    // A challenger appears and must hold its lead.
    let t = 100_000u64;
    assert!(!a.observe_channel(11, t), "a new candidate does not commit on sight");
    assert!(!a.observe_channel(11, t + SETTLE_MS - 1), "not until the window closes");
    assert_eq!(a.epoch(), 1, "and it has cost nothing yet");
    assert!(a.observe_channel(11, t + SETTLE_MS), "held its lead → commit");
    assert_eq!((a.epoch(), a.channel()), (2, 11));

    // A FLAP: the candidate keeps changing, so nothing ever holds long enough.
    let mut a = Announcer::new();
    assert!(a.observe_channel(6, 0));
    for i in 0..200u64 {
        let ch = if i % 2 == 0 { 1 } else { 11 };
        assert!(!a.observe_channel(ch, i * SETTLE_MS), "flapping never commits (i={i})");
    }
    assert_eq!(a.epoch(), 1, "200 oscillations, zero epochs spent");

    // Returning to the committed channel withdraws the candidate rather than half-arming it.
    let mut a = Announcer::new();
    assert!(a.observe_channel(6, 0));
    assert!(!a.observe_channel(11, 1_000));
    assert!(!a.observe_channel(6, 2_000), "back home → no-op");
    assert!(!a.observe_channel(11, 2_000 + SETTLE_MS), "the candidate's clock restarted");
    assert_eq!(a.epoch(), 1);
}

/// The steady-state repeat is what makes the frame observable on a fleet that is not migrating —
/// which is the entire value of the observe-only landing, so it is asserted rather than assumed.
pub fn beacons_between_migrations() {
    let mut a = Announcer::new();
    assert!(!a.beacon_due(u64::MAX), "an idle announcer never beacons");

    // A realistic boot offset, not 0. `decide` back-dates `last_ms` by one gap so the first frame
    // goes out immediately, and `saturating_sub` floors that at 0 — so at now_ms == 0 exactly, the
    // first frame waits a gap instead. Harmless on hardware (a crown decides seconds into its
    // uptime, after association) but it is a real edge and the test should not hide it by only ever
    // starting at zero.
    let t0 = 30_000u64;
    assert!(a.decide(6, t0));
    assert!(!a.beacon_due(t0 + ANNOUNCE_IDLE_MS * 10), "a LIVE burst is already saying it");
    for i in 0..ANNOUNCE_BURST {
        assert!(a.due(t0 + u64::from(i) * ANNOUNCE_GAP_MS));
    }

    let drained = t0 + u64::from(ANNOUNCE_BURST - 1) * ANNOUNCE_GAP_MS;
    assert!(!a.beacon_due(drained), "not yet — the burst's last frame was just sent");
    assert!(a.beacon_due(drained + ANNOUNCE_IDLE_MS), "one repeat per beacon interval");
    assert!(!a.beacon_due(drained + ANNOUNCE_IDLE_MS), "and exactly one");
    assert!(a.beacon_due(drained + 2 * ANNOUNCE_IDLE_MS), "then another");
}

/// The failure mode a monotonic epoch invites: one node with a high persisted epoch out-ranks every
/// later announcement and wedges the fleet onto a dead channel permanently. Probation is the exit.
pub fn probation_expires_on_a_dead_epoch() {
    let mut f = Follower::new();
    assert!(!f.probation_expired(u64::MAX), "never-heard is not on probation");

    let t = 100_000u64;
    assert_eq!(f.observe(&frame(8, 9, 11, 8), t, 6), Follow::Move(11));
    assert!(!f.probation_expired(t), "just heard");
    assert!(!f.probation_expired(t + PROBATION_MS), "exactly at the window is still inside it");
    assert!(f.probation_expired(t + PROBATION_MS + 1), "silence past the window → re-elect");

    // A replayed old frame must NOT look like liveness.
    let stale = t + PROBATION_MS + 1;
    assert_eq!(f.observe(&frame(8, 2, 1, 8), stale, 11), Follow::Stale);
    assert!(f.probation_expired(stale), "a replay does not reset probation");
}

/// The flag is supposed to change NOTHING on a live fleet until it is flipped. This is that claim,
/// asserted instead of stated: with following off the ladder IS today's blind-scan plan.
pub fn the_ladder_is_the_legacy_plan_while_following_is_off() {
    for last_known in [0u8, 1, 6, 11, 3, 13] {
        let (plan, n) = recovery_ladder(last_known, false);
        assert_eq!(n, LEGACY_CANDIDATES.len(), "legacy plan length (last_known={last_known})");
        assert_eq!(&plan[..n], &LEGACY_CANDIDATES[..], "byte-identical to `leaf_scan_tick`'s CANDIDATES");
    }
    assert!(!FOLLOW_ENABLED, "and the shipped default IS off — see #278's flip criterion");
}

/// Best guess first, and never spend a 1500 ms dwell proving the same channel twice.
pub fn the_ladder_ranks_and_dedupes() {
    // last-known first, then rendezvous, then the common APs.
    let (plan, n) = recovery_ladder(3, true);
    assert_eq!(&plan[..4], &[3, RENDEZVOUS_CHANNEL, COMMON_AP_CHANNELS[0], COMMON_AP_CHANNELS[1]]);
    assert_eq!(n, N_CHANNELS, "then the rest of the band");

    // last_known == rendezvous: the duplicate is dropped, not dwelled on.
    let (plan, n) = recovery_ladder(RENDEZVOUS_CHANNEL, true);
    assert_eq!(plan[0], RENDEZVOUS_CHANNEL);
    assert_eq!(plan[1], COMMON_AP_CHANNELS[0], "no second dwell on the rendezvous");
    assert_eq!(n, N_CHANNELS);

    // never knew one → start at the rendezvous.
    let (plan, n) = recovery_ladder(0, true);
    assert_eq!(plan[0], RENDEZVOUS_CHANNEL, "no last-known → rendezvous is rung 0");
    assert_eq!(n, N_CHANNELS);

    // a corrupt last-known is refused, not tuned to.
    for bad in [14u8, 99, 255] {
        let (plan, _) = recovery_ladder(bad, true);
        assert_eq!(plan[0], RENDEZVOUS_CHANNEL, "ch{bad} never enters the plan");
    }
}

/// Today's plan can never find a crown outside 1/6/11. The ladder's whole reach argument is that
/// it eventually probes every channel, exactly once.
pub fn the_ladder_reaches_the_whole_band() {
    for last_known in 0..=13u8 {
        let (plan, n) = recovery_ladder(last_known, true);
        assert_eq!(n, N_CHANNELS, "every channel is reachable (last_known={last_known})");
        for ch in 1..=(N_CHANNELS as u8) {
            assert_eq!(
                plan[..n].iter().filter(|&&c| c == ch).count(),
                1,
                "ch{ch} appears exactly once (last_known={last_known})"
            );
        }
    }
}

/// A sink that records rather than transmits — and note that implementing this trait is the ONLY
/// way to observe a `SealedElect`'s bytes at all. That is the invariant, exercised: there is no
/// accessor to reach for, so a caller that wants the frame must go through the send path.
struct RecordingSink {
    last: [u8; wire::ELECT_LEN],
    dst: [u8; 6],
    sends: usize,
}

impl GroupMacSink for RecordingSink {
    fn send_group_mac(&mut self, dst: &[u8; 6], frame: &[u8]) {
        assert_eq!(frame.len(), wire::ELECT_LEN, "a sealed frame is always the fixed record");
        self.last.copy_from_slice(frame);
        self.dst = *dst;
        self.sends += 1;
    }
}

/// The sealed frame is byte-identical to what the cross-repo encoder produces — sealing must not be
/// a second, divergent encoder — and it reaches the sink unmodified.
pub fn sealing_preserves_the_cross_repo_bytes() {
    let f = frame(5, 42, 11, 8);
    let mut expect = [0u8; wire::ELECT_LEN];
    let n = wire::encode(&f, &mut expect).expect("the reference encoder accepts it");
    assert_eq!(n, wire::ELECT_LEN);

    let mut sink = RecordingSink { last: [0u8; wire::ELECT_LEN], dst: [0u8; 6], sends: 0 };
    let bcast = [0xffu8; 6];
    SealedElect::seal(&f).expect("sealable").emit(&mut sink, &bcast);

    assert_eq!(sink.sends, 1, "emit sends exactly once");
    assert_eq!(sink.dst, bcast);
    assert_eq!(sink.last, expect, "sealing is the reference encoding, not a second one");
    assert!(sink.last.starts_with(wire::ELECT_PREFIX), "and it is still a SMOLv1 ELECT frame");
    assert_eq!(wire::parse(&sink.last), Some(f), "round-trips through the real parser");
}

/// `seal` inherits `encode`'s rejection, so a malformed decision cannot reach a sink at all.
pub fn sealing_refuses_a_frame_that_would_strand_a_leaf() {
    for bad in [0u8, 14, 255] {
        assert!(SealedElect::seal(&frame(5, 1, bad, 8)).is_none(), "ch{bad} is not sealable");
    }
}
