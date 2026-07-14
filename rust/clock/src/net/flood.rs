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

/// Send 1-in-N emits as an `H=1` direct probe while latched + downlink-present, so a
/// leaf that moved back into range re-tests its uplink cheaply (~N × emit-interval).
pub const PROBE_EVERY: u32 = 8;

/// The leaf's single-hop ⇄ multi-hop escalation state machine (pure).
///
/// - Starts single-hop (`H=1`, plain `RELAY`).
/// - [`on_relay_exhausted`] latches multi-hop when a message goes fully un-ACKed
///   (the gateway heard NOTHING directly) → subsequent emits use `RELAY2` at [`MAX_HOP`].
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
}

impl HopLatch {
    pub const fn new() -> Self {
        Self { latched: false, emit_count: 0, ack_streak: 0 }
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

    /// A message exhausted `RELAY_MAX_TRIES` with ZERO fragments ACKed → the gateway
    /// can't hear us directly. Latch multi-hop (idempotent).
    pub fn on_relay_exhausted(&mut self, any_frag_acked: bool) {
        if !any_frag_acked {
            self.latched = true;
            self.ack_streak = 0;
        }
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
