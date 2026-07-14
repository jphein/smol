//! #13 routed multi-hop mesh — the PURE managed-flood decision core.
//!
//! ## What this is
//! smol's uplink relay is single-hop today: a leaf out of direct ESP-NOW range of
//! the elected gateway is stranded. #13 adds Meshtastic-lineage **managed flood**:
//! a hop-limit (`H`) + an `(origin, msgid)` seen-set + a forward path, rooted at the
//! #76-elected owner, table-free (rides roam/re-election for free).
//!
//! This module is the **pure** brain — no esp-hal / esp-wifi deps, host-unit-testable
//! (see `scratch/13-multihop/flood_verify`), mirroring the `cast`/`ble` pure split.
//! It owns three decision pieces the live relay path in `net/mode.rs` drives:
//!   1. [`SeenSet`] — the bounded `(origin, msgid)` loop/dup guard.
//!   2. [`forward_decision`] — what a node does with an inbound multi-hop RELAY2 frame.
//!   3. [`HopLatch`] — the leaf's single-hop⇄multi-hop escalation state machine with
//!      un-latch hysteresis (so a leaf that moves back into range stops flooding).
//!   4. [`ChannelPark`] (#126) — a stranded leaf's channel-bias state machine: sweep +
//!      burst to find the channel that draws a forward/ACK, then park on it (throughput).
//!
//! ## The byte-identical invariant (team-lead's gate)
//! A non-escalated leaf sends plain `SMOLv1 RELAY` with an implicit `H=1`; nodes
//! forward ONLY `H>1` frames (a new `RELAY2` tag). So when every node hears the
//! gateway directly, no `RELAY2` ever exists → no node ever forwards → behaviour is
//! byte-identical to today (the canary proves it: `fwd=0` on every node). Multi-hop
//! only engages for a genuinely-stranded leaf (see [`HopLatch`]).

/// v1 hop ceiling: a stranded leaf originates `RELAY2` at this `H`; one relay hop
/// decrements it to 1 and delivers to the gateway. 3-hop (`>2`) is a follow-up.
pub const MAX_HOP: u8 = 2;

/// Capacity of the `(origin, msgid, frag)` seen-set ring. Sized to comfortably cover
/// the in-flight window across the small fleet (a few origins × their recent msgids ×
/// up to `RELAY_MAX_FRAGS` fragments each); drop-oldest on overflow. Small fixed `.bss`,
/// no alloc. 16 slots ≈ 4 fully-fragmented messages in flight — ample for the stranded
/// single-leaf case #13 v1 targets.
pub const SEEN_RING: usize = 16;

/// A bounded ring of recently-seen `(origin_id, msgid, frag)` — the loop/dup guard
/// that makes the flood terminate. DISTINCT from `mode.rs`'s `DONE_RING` (keyed on
/// `(src_mac, msgid)` for post-completion re-ACK dedup): `src_mac` changes at every
/// hop, but `origin_id` is stamped by the true source and survives forwarding, so
/// only an origin-anchored key can recognise "I already forwarded this frame".
///
/// KEYED PER-FRAGMENT (`frag` included): a RELAY message is FRAGMENTED — every
/// fragment rides its own frame sharing the message `msgid` (telemetry > `RELAY_CHUNK`
/// spans 2+ frames). A per-`(origin, msgid)` key would mark the whole message "seen"
/// on fragment 0 and a relay would then DROP fragments 1..N → the gateway could never
/// reassemble a multi-fragment message. `frag` in the key makes each fragment
/// independently forward-once. (A relay's per-frame forward mirrors the leaf's
/// per-frame broadcast; the gateway reassembles from the forwarded frames + dedups
/// late retransmits via its own `DONE_RING`, so the gateway never consults this set —
/// see the RELAY2 service arm.)
pub struct SeenSet {
    ring: [Option<(u8, u16, u8)>; SEEN_RING],
    cursor: usize,
}

impl SeenSet {
    pub const fn new() -> Self {
        Self { ring: [None; SEEN_RING], cursor: 0 }
    }

    /// True if `(origin, msgid, frag)` is already in the ring.
    pub fn contains(&self, origin: u8, msgid: u16, frag: u8) -> bool {
        self.ring.contains(&Some((origin, msgid, frag)))
    }

    /// Record `(origin, msgid, frag)` (drop-oldest on overflow). Idempotent — recording
    /// an already-present key is a no-op (keeps the ring from filling with dups).
    pub fn insert(&mut self, origin: u8, msgid: u16, frag: u8) {
        if self.contains(origin, msgid, frag) {
            return;
        }
        self.ring[self.cursor] = Some((origin, msgid, frag));
        self.cursor = (self.cursor + 1) % SEEN_RING;
    }

    /// Atomic "have I seen this fragment?" + record. Returns true if it was ALREADY
    /// seen (caller drops as a dup); false if it's new (caller processes + it's now
    /// recorded).
    pub fn seen_or_insert(&mut self, origin: u8, msgid: u16, frag: u8) -> bool {
        if self.contains(origin, msgid, frag) {
            return true;
        }
        self.insert(origin, msgid, frag);
        false
    }
}

impl Default for SeenSet {
    fn default() -> Self {
        Self::new()
    }
}

/// What a node should do with an inbound multi-hop `RELAY2` frame, decided purely
/// from `(is_gateway, hop, already_seen)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardAction {
    /// Already in the seen-set — drop, bump `dedup_hits`. (Bounds the flood.)
    DedupDrop,
    /// This node is the elected gateway (the flood's sink) — reassemble the fragment
    /// keyed by `origin`, and (on completion) flood a `RELAYACK2` back. Never re-forward.
    Reassemble,
    /// A relay node with hops left — re-broadcast as `RELAY2` at `hop - 1`, bump
    /// `fwd_count`/`fwd_ok`, record in the seen-set.
    Forward { hop: u8 },
    /// Hop budget exhausted (`hop <= 1` at a non-gateway) — drop, bump `ttl_drops`.
    TtlDrop,
}

/// Decide the fate of an inbound `RELAY2` (multi-hop uplink) fragment. Pure: the
/// caller supplies liveness (`is_gateway`) + the already-seen result, and applies
/// the returned action (forwarding, reassembling, or dropping) + the matching counter.
///
/// `hop` is the frame's current hop-limit (as received, before decrement). A gateway
/// always reassembles (it's the sink); a relay forwards while `hop > 1`, else `TtlDrop`.
pub fn forward_decision(is_gateway: bool, hop: u8, already_seen: bool) -> ForwardAction {
    if already_seen {
        return ForwardAction::DedupDrop;
    }
    if is_gateway {
        return ForwardAction::Reassemble;
    }
    if hop > 1 {
        ForwardAction::Forward { hop: hop - 1 }
    } else {
        ForwardAction::TtlDrop
    }
}

/// Number of consecutive successful direct-uplink probes required to drop the
/// multi-hop latch — the hysteresis that stops a marginal/asymmetric link from
/// flapping a leaf between single- and multi-hop.
pub const UNLATCH_STREAK: u8 = 2;

/// Number of CONSECUTIVE fully-un-ACKed messages required to escalate INTO multi-hop.
/// The down→up hysteresis (mirror of [`UNLATCH_STREAK`]): a genuinely-stranded leaf has
/// EVERY message fully un-ACKed and latches in `ESCALATE_STREAK × emit-interval` (~45 s at
/// 15 s), while a single transient full-loss in an otherwise-healthy all-hear mesh does NOT
/// escalate — any ACKed message resets the streak. This is what keeps the byte-identical
/// invariant (`fwd=0`) intact under normal packet loss: without it, ONE dropped message would
/// latch a leaf to `RELAY2` and start a bounded forward-swarm that the C0 canary reads as failure.
pub const ESCALATE_STREAK: u8 = 3;

/// Send 1-in-N emits as an `H=1` direct probe while latched + downlink-present, so a
/// leaf that moved back into range re-tests its uplink cheaply (~N × emit-interval).
pub const PROBE_EVERY: u32 = 8;

/// The leaf's single-hop ⇄ multi-hop escalation state machine (pure).
///
/// - Starts single-hop (`H=1`, plain `RELAY`).
/// - [`on_relay_exhausted`] latches multi-hop only after [`ESCALATE_STREAK`] CONSECUTIVE
///   fully-un-ACKed messages (genuine stranding, not a transient loss) → subsequent emits use
///   `RELAY2` at [`MAX_HOP`]. [`on_uplink_progress`] resets that streak the moment the gateway
///   ACKs anything — so normal packet loss never escalates (preserves the `fwd=0` invariant).
/// - While latched, [`should_probe`] fires a 1-in-[`PROBE_EVERY`] plain-`RELAY` (`H=1`)
///   probe, but ONLY when the caller reports the owner's HELLO is heard directly
///   (`downlink_up` — else the leaf is definitely still stranded; don't waste airtime).
/// - [`on_direct_ack`] counts a successful probe; after [`UNLATCH_STREAK`] consecutive,
///   the latch drops back to single-hop. Any miss ([`on_probe_miss`]) resets the streak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HopLatch {
    latched: bool,
    emit_count: u32,
    ack_streak: u8,
    /// Consecutive fully-un-ACKed messages since the last ACK — the escalation (down→up)
    /// hysteresis counter. Reaches [`ESCALATE_STREAK`] ⇒ latch; any ACK resets it to 0.
    unack_streak: u8,
}

impl HopLatch {
    pub const fn new() -> Self {
        Self { latched: false, emit_count: 0, ack_streak: 0, unack_streak: 0 }
    }

    /// Are we currently in multi-hop mode?
    pub fn latched(&self) -> bool {
        self.latched
    }

    /// The hop-limit to originate the NEXT emit with, given whether this emit is a
    /// probe. Single-hop (latch off) or a probe → `1` (plain `RELAY`); latched
    /// non-probe → [`MAX_HOP`] (`RELAY2`).
    pub fn origin_hop(&self, is_probe: bool) -> u8 {
        if self.latched && !is_probe {
            MAX_HOP
        } else {
            1
        }
    }

    /// Call once per relay emit (before choosing the frame). Advances the probe
    /// counter and returns whether THIS emit should be a direct `H=1` probe: only
    /// while latched AND `downlink_up` (owner HELLO heard directly), on the 1-in-N tick.
    pub fn should_probe(&mut self, downlink_up: bool) -> bool {
        if !self.latched {
            return false;
        }
        self.emit_count = self.emit_count.wrapping_add(1);
        downlink_up && self.emit_count.is_multiple_of(PROBE_EVERY)
    }

    /// A message exhausted `RELAY_MAX_TRIES`. `any_frag_acked` = the gateway confirmed ≥1
    /// fragment (directly, or via the flooded RELAYACK2 once already multi-hop) → it can hear
    /// us, so reset the escalation streak. ZERO acks = a fully-lost message; latch multi-hop
    /// ONLY after [`ESCALATE_STREAK`] such messages IN A ROW, so a single transient full-loss
    /// in a healthy all-hear mesh does NOT escalate (that would break the `fwd=0` invariant).
    pub fn on_relay_exhausted(&mut self, any_frag_acked: bool) {
        if any_frag_acked {
            self.unack_streak = 0;
            return;
        }
        self.unack_streak = self.unack_streak.saturating_add(1);
        if self.unack_streak >= ESCALATE_STREAK {
            self.latched = true;
            self.ack_streak = 0;
        }
    }

    /// Our uplink made progress — a message was fully ACKed (or any fragment ACKed): proof the
    /// gateway hears us, so reset the escalation streak. Does NOT un-latch (that needs the
    /// direct-ACK probe streak — a latched leaf whose `RELAY2` is ACKed via the flood is still
    /// stranded on the DIRECT path). Called from the leaf's per-message complete path.
    pub fn on_uplink_progress(&mut self) {
        self.unack_streak = 0;
    }

    /// A DIRECT RELAYACK arrived (the gateway heard an `H=1` frame from us). While
    /// latched, count it toward un-latching; after [`UNLATCH_STREAK`] consecutive,
    /// drop the latch. When not latched this just (re)confirms single-hop health.
    pub fn on_direct_ack(&mut self) {
        if !self.latched {
            return;
        }
        self.ack_streak = self.ack_streak.saturating_add(1);
        if self.ack_streak >= UNLATCH_STREAK {
            self.latched = false;
            self.ack_streak = 0;
            self.emit_count = 0;
        }
    }

    /// A probe went un-ACKed (uplink still down) — reset the streak, stay latched.
    pub fn on_probe_miss(&mut self) {
        if self.latched {
            self.ack_streak = 0;
        }
    }
}

impl Default for HopLatch {
    fn default() -> Self {
        Self::new()
    }
}

// ---- #126 channel parking (latched-leaf multi-hop throughput) --------------
//
// A stranded (latched) leaf never hears its elected owner's HELLO, so it never LOCKS a
// channel — `leaf_scan_tick` keeps it blind-hopping 1/6/11. Its uplink therefore lands on
// the relay's channel only ~1/3 of the time (channel coincidence), and the flooded-back
// RELAYACK2 hits the same ~1/3 in reverse. `ChannelPark` biases a LATCHED leaf toward the
// channel that actually drew a forward/ACK, raising delivery toward ~1/1 (issue #126).
//
// The ACK-feedback bootstrap (chicken-and-egg: parking wants to key on "which channel drew a
// forward/ACK", but that signal can't exist until we've emitted there): a `Sweeping` phase
// dwells per candidate and fires short EMISSION BURSTS (K frames/channel, ≥1 guaranteed before
// hopping) so a returning RELAYACK2 — or a relay re-broadcasting OUR origin — can be attributed
// to the channel that worked. `on_feedback` then PARKS there; parked, the leaf simply HOLDS the
// channel and lets the normal telemetry cadence flow (no extra airtime) — the bias IS the win.
//
// INVARIANT: parking engages ONLY while latched (`sync`), so a healthy (never-latched) leaf's
// blind scan + byte-identical `fwd=0` behaviour is completely untouched. Un-latch (uplink
// recovered) or a #76 re-election (`on_rechannel`) disengages/restarts the sweep.

/// Candidate ESP-NOW channels a leaf sweeps while unlocked (JP's roam plan). SINGLE SOURCE OF
/// TRUTH — `mode.rs::leaf_scan_tick`'s blind scan, `current_channel`, and #126 parking all index
/// this, so the scan plan and the park plan can never silently drift apart.
// #126 wip (Stage A): the whole ChannelPark block is host-tested in flood_verify but UNWIRED in the
// clock crate — the allow(dead_code)s DROP in Stage B when leaf_scan_tick/emit start driving it.
#[allow(dead_code)]
pub const PARK_CHANNELS: [u8; 3] = [1, 6, 11];

/// Per-candidate dwell while SWEEPING (ms). Matches `leaf_scan_tick`'s blind-scan dwell, so
/// parking only re-orders WHICH channel a stranded leaf favours, not the dwell shape.
#[allow(dead_code)]
pub const PARK_DWELL_MS: u64 = 1500;

/// Min gap between emission bursts within one dwell (ms). A "burst" re-broadcasts the leaf's
/// staged uplink so a relay on THIS channel forwards it + the gateway floods a RELAYACK2 back —
/// the ACK-feedback bootstrap. Only fires while LATCHED + sweeping (stranded); never on a healthy leaf.
#[allow(dead_code)]
pub const PARK_BURST_EVERY_MS: u64 = 400;

/// Emission bursts per candidate before the sweep may hop off it — the "emit K frames per channel
/// before hopping" bootstrap. Guarantees every candidate is actually probed, never skipped.
#[allow(dead_code)]
pub const PARK_BURSTS_PER_DWELL: u8 = 3;

/// A PARKED channel that draws no forward/ACK for this long (ms) is assumed cold (relay roamed /
/// re-elected) ⇒ resume sweeping. > 1 telemetry cycle (~15 s) so a single lost ACK on a live-but-
/// lossy channel doesn't abandon a good park.
#[allow(dead_code)]
pub const PARK_SILENCE_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParkPhase {
    /// Not stranded (or uplink recovered) — blind scan governs; parking is inert.
    Idle,
    /// Cycling candidates + bursting, waiting for the first forward/ACK to attribute.
    Sweeping,
    /// A candidate drew feedback — HOLD it (both uplink + RELAYACK2 land on the relay's channel).
    Parked,
}

/// The leaf's #126 channel-parking state machine (pure). Companion to [`HopLatch`]: `HopLatch`
/// answers *single- or multi-hop?*, `ChannelPark` answers *which channel should a stranded leaf
/// be on?*. Driven by the live `leaf_scan_tick`/emit path (which supplies `now` + acts on the
/// outputs); host-unit-tested in `flood_verify` (state machine AND the emit-trigger contract).
#[allow(dead_code)] // #126 wip: unwired in the clock crate until Stage B (see the block note above).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelPark {
    phase: ParkPhase,
    /// Current candidate index into [`PARK_CHANNELS`].
    idx: u8,
    /// When we arrived on the current candidate (sweeping dwell clock).
    dwell_started_ms: u64,
    /// Emission bursts already fired on the current candidate this dwell.
    bursts_this_dwell: u8,
    /// Last burst emit time (spacing clock).
    last_burst_ms: u64,
    /// Last forward/ACK attributed to the parked channel (silence clock).
    last_feedback_ms: u64,
}

#[allow(dead_code)] // #126 wip: the whole API is exercised by flood_verify; wired in Stage B.
impl ChannelPark {
    pub const fn new() -> Self {
        Self {
            phase: ParkPhase::Idle,
            idx: 0,
            dwell_started_ms: 0,
            bursts_this_dwell: 0,
            last_burst_ms: 0,
            last_feedback_ms: 0,
        }
    }

    /// Is parking active (leaf is stranded)? `false` ⇒ the blind scan governs the channel.
    pub fn engaged(&self) -> bool {
        !matches!(self.phase, ParkPhase::Idle)
    }

    /// Have we locked onto a working channel (as opposed to still sweeping)?
    pub fn parked(&self) -> bool {
        matches!(self.phase, ParkPhase::Parked)
    }

    /// The channel the leaf should be on right now, or `None` while [`ParkPhase::Idle`] (the
    /// caller falls back to the normal blind scan). Indexes [`PARK_CHANNELS`].
    pub fn channel(&self) -> Option<u8> {
        match self.phase {
            ParkPhase::Idle => None,
            _ => Some(PARK_CHANNELS[(self.idx as usize) % PARK_CHANNELS.len()]),
        }
    }

    fn start_sweep(&mut self, idx: u8, now: u64) {
        self.phase = ParkPhase::Sweeping;
        self.idx = idx % PARK_CHANNELS.len() as u8;
        self.dwell_started_ms = now;
        self.bursts_this_dwell = 0;
        self.last_burst_ms = 0;
    }

    /// Drive from the escalation latch each tick (edge-safe / idempotent). Latched + inert ⇒
    /// engage, sweeping from candidate 0. Un-latched (uplink recovered) + engaged ⇒ disengage to
    /// [`ParkPhase::Idle`] so the blind scan resumes. Otherwise a no-op.
    pub fn sync(&mut self, latched: bool, now: u64) {
        if latched && !self.engaged() {
            self.start_sweep(0, now);
        } else if !latched && self.engaged() {
            self.phase = ParkPhase::Idle;
        }
    }

    /// #76: the elected owner / its channel changed under us, so the swept/parked channel is
    /// stale — restart the sweep from candidate 0. No-op while [`ParkPhase::Idle`] (not stranded).
    pub fn on_rechannel(&mut self, now: u64) {
        if self.engaged() {
            self.start_sweep(0, now);
        }
    }

    /// Advance dwell/park timing. SWEEPING: hop to the next candidate once the dwell elapsed AND
    /// ≥1 burst went out on this one (so no candidate is skipped un-probed — the caller MUST call
    /// [`should_burst`] each tick BEFORE `tick`, see `flood_verify`'s trigger-contract test).
    /// PARKED: if the channel has been silent past [`PARK_SILENCE_MS`] it went cold ⇒ resume
    /// sweeping from it. IDLE: nothing.
    pub fn tick(&mut self, now: u64) {
        match self.phase {
            ParkPhase::Sweeping => {
                let dwell_done = now.saturating_sub(self.dwell_started_ms) >= PARK_DWELL_MS;
                if dwell_done && self.bursts_this_dwell >= 1 {
                    let next = (self.idx + 1) % PARK_CHANNELS.len() as u8;
                    self.start_sweep(next, now);
                }
            }
            ParkPhase::Parked => {
                if now.saturating_sub(self.last_feedback_ms) >= PARK_SILENCE_MS {
                    self.start_sweep(self.idx, now); // re-sweep from the last good channel
                }
            }
            ParkPhase::Idle => {}
        }
    }

    /// Should the leaf fire an emission BURST this tick? Only while SWEEPING (the ACK-feedback
    /// bootstrap): up to [`PARK_BURSTS_PER_DWELL`] per candidate, spaced ≥ [`PARK_BURST_EVERY_MS`]
    /// (the first of each dwell fires immediately). PARKED/IDLE ⇒ `false` (normal telemetry cadence
    /// carries the held channel). MUTATES the burst counters — call at most once per tick.
    pub fn should_burst(&mut self, now: u64) -> bool {
        if self.phase != ParkPhase::Sweeping {
            return false;
        }
        if self.bursts_this_dwell >= PARK_BURSTS_PER_DWELL {
            return false;
        }
        if self.bursts_this_dwell > 0
            && now.saturating_sub(self.last_burst_ms) < PARK_BURST_EVERY_MS
        {
            return false; // not yet time for the next burst of this dwell
        }
        self.bursts_this_dwell = self.bursts_this_dwell.saturating_add(1);
        self.last_burst_ms = now;
        true
    }

    /// A forward/ACK signal — a RELAYACK2 addressed to us, or a relay re-broadcasting OUR origin —
    /// arrived while on the current channel ⇒ that channel WORKS. SWEEPING ⇒ PARK here. PARKED ⇒
    /// refresh freshness so the silence timer doesn't abandon a live channel. IDLE ⇒ ignore
    /// (a non-stranded leaf's ACKs are normal single-hop traffic, not park feedback).
    pub fn on_feedback(&mut self, now: u64) {
        match self.phase {
            ParkPhase::Sweeping => {
                self.phase = ParkPhase::Parked;
                self.last_feedback_ms = now;
            }
            ParkPhase::Parked => {
                self.last_feedback_ms = now;
            }
            ParkPhase::Idle => {}
        }
    }
}

impl Default for ChannelPark {
    fn default() -> Self {
        Self::new()
    }
}
