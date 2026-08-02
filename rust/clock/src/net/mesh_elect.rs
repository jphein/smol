//! The `SMOLv1 ELECT` frame: **announcing** the mesh rendezvous channel, and the
//! epoch/anti-flap machinery that makes a channel migration safe.
//!
//! # What decides the channel (and why nothing here votes)
//!
//! Per JP's directive (#269): **the mesh channel IS the elected gateway's AP
//! channel.** It is a *derived value*, not a decision. The chain is
//!
//! ```text
//! elect gateway (best internet)  ->  that gateway joins an AP  ->  mesh channel := that AP's channel
//!      net::election                    net::coexist                  (a consequence, not a vote)
//! ```
//!
//! Co-channel operation is therefore true **by construction** rather than being a
//! property to check and repair — which retires the off-channel pathology class
//! (crown on ch1, mesh on ch6, OTA dying at byte 0) instead of guarding against it.
//! And no quorum can move the mesh somewhere the gateway is not, because nobody
//! votes on the channel at all.
//!
//! # Provenance, and what was deliberately removed
//!
//! Ported from the esp32c6-watch's `crates/mesh-elect` (`50e28df`, read-only
//! reference). The donor **elects** a channel by lexicographic tally across peer
//! observations. **That election was removed on purpose** — under the derivation
//! above there is nothing to elect, and keeping a voting engine in a system with
//! nothing to vote on would read as intent to the next person who touched it.
//!
//! Removed: `Elector`, `Tally`, `Ingest`, `Slot`, and the observe/tally/winner/step
//! machinery. Kept, because they serve the announcement instead:
//!
//! | kept | why it survives |
//! |---|---|
//! | [`wire`] | the frame itself — now an ANNOUNCEMENT of a derived value, not a ballot |
//! | [`Decision`] + `supersedes` | orders announcements by epoch; rejects stale/replayed ones |
//! | [`SETTLE_MS`], [`PROBATION_MS`] | anti-flap, and both are WIRED: `Announcer::observe_channel` makes a new channel hold for `SETTLE_MS` before it costs an epoch, and `Follower::probation_expired` is the exit from a wedged high epoch |
//! | [`MARGIN_FLOOR`], [`margin_for`], [`OBS_STALE_MS`] | retained but **NOT wired** — see below |
//! | [`RENDEZVOUS_CHANNEL`] | tier 2 of the recovery ladder below |
//! | [`weight`], [`USABLE_MIN_DBM`] | still populate the frame's `w[13]` honestly — see below |
//!
//! ## `MARGIN_FLOOR` / `margin_for` / `OBS_STALE_MS` are retained but NOT wired — deliberately
//!
//! Stage 1 listed these alongside `SETTLE_MS` as "anti-flap: a challenger must hold its advantage".
//! That was aspirational and stage 2 corrected it rather than letting it stand, because a comment
//! describing behaviour the binary does not have is this codebase's most common defect shape: it
//! never fails, it just quietly under-delivers.
//!
//! The honest statement: all three operate on a **summed weight across peer observations** — the
//! `Tally`/`Ingest` machinery that was deliberately removed with the `Elector`. `margin_for` takes
//! an `incumbent_sum` smol never computes; `OBS_STALE_MS` ages observations smol never accumulates.
//! They are kept because this file is a cross-repo contract and the donor still owns the election:
//! deleting them would make the next re-sync a merge instead of a diff, and the upstream tests that
//! pin them run against this exact file. They carry item-scoped `#[allow(dead_code)]` saying so,
//! rather than a module-wide allow that would also hide a genuinely unwired new feature.
//!
//! ## The `w[13]` vector is still filled in, deliberately
//!
//! smol no longer votes, so the weight vector is vestigial *to smol*. It is
//! populated honestly from our own scan anyway, for two reasons: the frame must
//! stay **byte-identical** to the donor's or the watch cannot parse it at all, and
//! the watch still runs its own election in observe-only mode. Emitting zeros
//! would make the watch's observation compute garbage during the transition.
//! **Dropping the vote is not the same as dropping the observation.**
//!
//! # Security: authentication is load-bearing here, not a bonus
//!
//! esp-radio 0.18 hardcodes `coex_background_scan: false`, so **a scan drops the
//! association**. A leaf therefore *cannot verify a channel-change announcement
//! before acting on it* — checking costs it the very association it would need if
//! the announcement were false. It must **trust** the frame.
//!
//! An unauthenticated ELECT would then be a trivially forgeable *"everyone move to
//! channel N"* fleet-stranding primitive, with no cheap way for a leaf to detect
//! the lie. smol closes this where the donor could not: #190 gives every SMOLv1
//! frame a truncated group-HMAC-SHA256 trailer, and `should_group_mac` admits a
//! frame that starts `SMOLv1 `, is not OTA-family, and fits the MTU. `SMOLv1 ELECT `
//! satisfies all three (61 B + 9 B = 70 B <= 250 B), so ELECT is authenticated with
//! no code here.
//!
//! ⚠️ **DESIGN INVARIANT, not a style note: emit ELECT via `send_to`.** That is the
//! path that appends the trailer. The #237 arbitration frames (`ODEL`/`ODON`)
//! deliberately bypass it via `send_arb_raw`; routing ELECT that way would convert a
//! safe channel hop into a remote fleet-partition attack.
//!
//! # Prior art: this is CSA with an ESP-NOW carrier, not a novel design
//!
//! 802.11 already has the announce-then-move mechanism and a name for it: the **Channel Switch
//! Announcement** element, carried in beacons. ESP-WIFI-MESH migrates its mesh channel exactly
//! this way — CSA elements propagated by the root, announced by the node that noticed, *then* the
//! move. smol's ELECT frame is the same mechanism on a different carrier, which is why the
//! announce-before-AND-after ordering below is not a guess.
//!
//! ESP-WIFI-MESH also **requires** the mesh and the router to share a channel. That is
//! independent validation of #269's premise: co-channel by construction rather than by repair is
//! how a shipping ESP mesh does it.
//!
//! ⚠️ And the same source documents the honest limit — the migration is **not atomic**: IDF states
//! there will be *"a temporary channel discrepancy"* while nodes converge. A production system with
//! this exact architecture ships with a documented disagreement window, which is why the
//! migration-window guard in `net::mode`'s `note_crown_ap` is kept rather than deleted.
//!
//! # Recovery ladder for a leaf that missed the announcement
//!
//! An ordered ladder with early exit, not one expensive sweep. See [`recovery_ladder`], which is
//! the implementation and holds the real cost model:
//!
//! ```text
//! 0. heard the announcement        ~0
//! 1. last-known mesh channel
//! 2. RENDEZVOUS_CHANNEL (6)
//! 3. common AP channels (1, 11)
//! 4. the rest of the band, ascending
//! ```
//!
//! ⚠️ Stage 1 costed these rungs at 10-20 ms each and a full sweep at 130-260 ms. Those are
//! `esp-radio`'s **`scan_async`** dwell times and they are the WRONG model for smol, which recovers
//! by retuning the ESP-NOW PHY and listening for `DWELL_MS` (1500 ms) rather than by scanning. The
//! numbers are re-derived on [`recovery_ladder`] instead of repeated here — one place to be right.
//!
//! What DOES carry over from esp-radio 0.18 is the constraint behind the ladder's shape: it exposes
//! all-channels or exactly one channel, never a subset (`channel_bitmap` is hardcoded to 0). There
//! is no "probe these four" primitive to reach for, so a ranked sequence with early exit is not a
//! design preference, it is the only shape available.
//!
//! # What this module does NOT decide
//!
//! It does not choose an AP — that is `net::coexist::select_crown_ap`, a per-node
//! association policy taking `mesh_ch` as an INPUT. It does not choose the gateway
//! — that is `net::election`. This module only **carries and orders** the resulting
//! channel, and holds the constants that stop it thrashing.

// ── Lint policy for a VENDORED file ────────────────────────────────────────────────────────────
// smol gates `-D warnings`; the donor's crate does not, so three idioms trip here (two indexed
// loops over the fixed 13-slot weight vector, one `map_or`). They are suppressed rather than
// rewritten ON PURPOSE.
//
// This file is a CROSS-REPO CONTRACT, not ordinary smol source: both repos must compute the same
// winner and emit the same bytes or the fleet partitions. Its value is that it stays *diffable*
// against `esp32c6-watch:crates/mesh-elect` — a future sync should show only intended divergence.
// Clippy-rewriting the bodies would make every future diff noisy, and the predictable result is
// someone "helpfully" re-syncing from upstream and silently reverting the edits. Style is worth
// less than that guarantee.
//
// Scoped to this file only, so the rest of the crate keeps the strict lints. If a real defect is
// ever found in these loops, fix it UPSTREAM in the watch and re-port — do not fork the logic here.
#![allow(clippy::needless_range_loop, clippy::unnecessary_map_or)]

/// 2.4 GHz channels considered, `1..=13`. Fixed, so every table and the wire
/// frame are fixed-size.
pub const N_CHANNELS: usize = 13;


/// An AP weaker than this does not count as "usable" — a channel we can only
/// barely hear must not win on headcount. Matches smol's proven
/// `coexist::AP_USABLE_MIN`, so both repos judge usability identically.
pub const USABLE_MIN_DBM: i8 = -82;

/// Weight saturates here at the strong end: everything at/above this counts the
/// same, so standing next to an AP cannot buy extra votes.
pub const WEIGHT_CEIL_DBM: i8 = -35;

/// An observation older than this stops counting. A node that walked away must
/// not keep voting for the channel it used to see. Peers tick every ~2 s, so
/// this is ~30 missed frames.
///
/// **Must exceed [`SETTLE_MS`] and [`PROBATION_MS`]** — see [`_WINDOW_ORDERING`].
/// `#[allow(dead_code)]`: operates on a summed weight across peer observations, which smol
/// does not accumulate (the `Tally`/`Ingest` machinery was cut with the `Elector`). Retained as
/// the donor's contract surface — see the module header. Item-scoped, so a genuinely unwired
/// NEW feature still trips `-D warnings` instead of hiding behind a module-wide allow.
#[allow(dead_code)]
pub const OBS_STALE_MS: u64 = 60_000;

/// A challenger must hold its lead this long before we spend an epoch on it.
/// Flapping costs an association and tears the mesh down, so it is strictly
/// worse than a suboptimal-but-stable channel.
pub const SETTLE_MS: u64 = 30_000;

/// Floor for the margin a challenger must beat the incumbent by (in summed
/// weight). See [`margin_for`].
/// `#[allow(dead_code)]`: operates on a summed weight across peer observations, which smol
/// does not accumulate (the `Tally`/`Ingest` machinery was cut with the `Elector`). Retained as
/// the donor's contract surface — see the module header. Item-scoped, so a genuinely unwired
/// NEW feature still trips `-D warnings` instead of hiding behind a module-wide allow.
#[allow(dead_code)]
pub const MARGIN_FLOOR: u32 = 12;

/// After adopting someone else's higher-epoch decision, how long we tolerate
/// hearing nothing on it before concluding the epoch is stale and re-electing.
/// Without this, one node with a high persisted epoch can wedge the whole fleet
/// onto a dead channel permanently — the failure mode a monotonic epoch invites
/// and the spec does not address.
pub const PROBATION_MS: u64 = 45_000;

/// Rendezvous channel of last resort, used only when there is nothing to elect
/// (no AP visible anywhere). Matches smol's historical `ESP_NOW_FIXED_CHANNEL`
/// so a fleet with no infrastructure still meets. This is a DEFAULT, not a pin:
/// it is abandoned the moment a real candidate appears.
pub const RENDEZVOUS_CHANNEL: u8 = 6;

/// Staleness must outlive both decision windows, or the machine can never
/// finish making up its mind: a challenger's supporting observations would
/// expire before its settle window closed, so `winner()` would flip back and the
/// timer would re-arm forever. Same for probation. Found by
/// `tests/consensus.rs::decisive_challenger_waits_for_settle_window`, which hung
/// at exactly `SETTLE_MS == OBS_STALE_MS`. Asserted at compile time so a future
/// tuning pass cannot silently reintroduce it.
const _WINDOW_ORDERING: () = {
    assert!(OBS_STALE_MS > SETTLE_MS);
    assert!(OBS_STALE_MS > PROBATION_MS);
};

/// Map dBm to a saturating vote weight. Monotone, clamped at both ends, and
/// `0` for anything below [`USABLE_MIN_DBM`] (not usable = no vote).
///
/// Range is `1..=48` for usable signal: the `+1` floor means bare usable
/// visibility still counts for something, and the ceiling means proximity
/// saturates.
#[must_use]
pub const fn weight(rssi_dbm: i8) -> u32 {
    if rssi_dbm < USABLE_MIN_DBM {
        return 0;
    }
    let capped = if rssi_dbm > WEIGHT_CEIL_DBM {
        WEIGHT_CEIL_DBM
    } else {
        rssi_dbm
    };
    // capped ∈ [-82, -35] → 1..=48
    (capped as i32 - USABLE_MIN_DBM as i32 + 1) as u32
}

/// The margin a challenger must clear, scaled to the incumbent's score.
///
/// The spec suggests a flat ">= 6 dB aggregate", but aggregate score is a sum
/// across nodes, so a flat threshold gets *easier* to trip as the fleet grows
/// (with 5 reporters, 6 units of aggregate is barely 1 dB each). Scaling with
/// the incumbent keeps the hysteresis meaningful at any fleet size, with a floor
/// so it is never trivial when scores are small.
#[must_use]
/// `#[allow(dead_code)]`: operates on a summed weight across peer observations, which smol
/// does not accumulate (the `Tally`/`Ingest` machinery was cut with the `Elector`). Retained as
/// the donor's contract surface — see the module header. Item-scoped, so a genuinely unwired
/// NEW feature still trips `-D warnings` instead of hiding behind a module-wide allow.
#[allow(dead_code)]
pub const fn margin_for(incumbent_sum: u32) -> u32 {
    let scaled = incumbent_sum / 8;
    if scaled > MARGIN_FLOOR {
        scaled
    } else {
        MARGIN_FLOOR
    }
}

/// Channel `1..=13` to array index, or `None` if out of range. Every inbound
/// channel number goes through this — a frame claiming ch 0 or ch 99 is dropped,
/// never indexed with.
#[must_use]
pub const fn ch_index(channel: u8) -> Option<usize> {
    if channel >= 1 && channel as usize <= N_CHANNELS {
        Some(channel as usize - 1)
    } else {
        None
    }
}

/// The committed decision. Carried on every ELECT frame; higher epoch wins.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decision {
    /// Elected ESP-NOW channel, `1..=13`.
    pub channel: u8,
    /// Monotonic epoch. Higher wins; a late joiner adopts rather than re-elects.
    pub epoch: u32,
    /// Elected gateway node id, or `0` for "none known".
    pub gateway: u8,
}

impl Decision {
    /// The boot decision before anything is known or restored.
    #[must_use]
    pub const fn bootstrap() -> Self {
        Self {
            channel: RENDEZVOUS_CHANNEL,
            epoch: 0,
            gateway: 0,
        }
    }

    /// Total order for partition merge: higher epoch wins; equal epoch breaks by
    /// larger member count, then by lower channel. Never returns "equal" for two
    /// differing decisions reached with the same member count, so a merge always
    /// resolves.
    #[must_use]
    pub fn supersedes(&self, members: u8, other: &Self, other_members: u8) -> bool {
        if self.epoch != other.epoch {
            return self.epoch > other.epoch;
        }
        if members != other_members {
            return members > other_members;
        }
        self.channel < other.channel
    }
}

pub mod wire {
    //! The `SMOLv1 ELECT` frame — one fixed-width ASCII record, byte-identical in
    //! both repos.
    //!
    //! Layout (61 bytes, always):
    //!
    //! ```text
    //! "SMOLv1 ELECT " <id:3> ' ' <epoch:10> ' ' <ch:2> ' ' <gw:3> ' ' <w:26>
    //!  └── 13 ──────┘                                                  └ 13×2 ┘
    //! ```
    //!
    //! Conventions are the existing SMOLv1 ones so this is unsurprising to read
    //! alongside HELLO/TIME/RELAY: an ASCII `SMOLv1 <TAG> ` prefix, then
    //! **fixed-width zero-padded decimal** fields, single-space separated. Tag byte
    //! 7 is `'E'`, which no existing SMOLv1 tag uses (`H A B T G C S D R U F`), so
    //! there is no collision, and firmware that predates this frame classifies it as
    //! unknown and ignores it harmlessly.
    //!
    //! **Fixed width is the security property, not just a convenience.** The design
    //! spec proposed a variable candidate list (`<n_cands> [<bssid> <ch> <rssi>]*`)
    //! with a note to cap it. Because we elect a channel rather than a BSSID, the
    //! candidate set is the 13 channels of the 2.4 GHz band — a constant. So the
    //! frame has no length field, no repetition, and no bound to enforce: a
    //! malformed or hostile frame is simply not 61 bytes, or fails a digit check.
    //! There is nothing here for an attacker to grow.
    //!
    //! Well under the 250 B ESP-NOW payload cap, with room for a future field.

    use super::{ch_index, N_CHANNELS};

    /// Frame tag. Trailing space matches every other SMOLv1 prefix.
    pub const ELECT_PREFIX: &[u8] = b"SMOLv1 ELECT ";

    /// Exact encoded length. A frame of any other length is not an ELECT frame.
    pub const ELECT_LEN: usize = 13 + 3 + 1 + 10 + 1 + 2 + 1 + 3 + 1 + 2 * N_CHANNELS;

    /// A decoded ELECT frame.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct ElectFrame {
        pub node_id: u8,
        pub epoch: u32,
        pub channel: u8,
        pub gateway: u8,
        /// Per-channel weight, index 0 = ch1.
        pub w: [u8; N_CHANNELS],
    }

    /// Write `v` as `n` zero-padded ASCII digits. Values too large for the field are
    /// clamped to all-nines rather than truncated to a wrong-but-plausible number.
    fn write_num(v: u32, n: usize, out: &mut [u8]) {
        let mut v = v;
        let mut max = 1u64;
        for _ in 0..n {
            max *= 10;
        }
        if (v as u64) >= max {
            for b in out[..n].iter_mut() {
                *b = b'9';
            }
            return;
        }
        for i in (0..n).rev() {
            out[i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    }

    /// Parse exactly `n` ASCII digits. `None` on any non-digit — no lenient
    /// whitespace, no partial parse.
    fn parse_num(s: &[u8], n: usize) -> Option<u32> {
        if s.len() < n {
            return None;
        }
        let mut v: u32 = 0;
        for &b in &s[..n] {
            if !b.is_ascii_digit() {
                return None;
            }
            v = v.checked_mul(10)?.checked_add((b - b'0') as u32)?;
        }
        Some(v)
    }

    /// Encode into `out` (which must hold at least [`ELECT_LEN`]). Returns the
    /// number of bytes written, or `None` if the buffer is too small or the channel
    /// is out of range.
    #[must_use]
    pub fn encode(f: &ElectFrame, out: &mut [u8]) -> Option<usize> {
        if out.len() < ELECT_LEN || ch_index(f.channel).is_none() {
            return None;
        }
        let mut n = 0;
        out[..ELECT_PREFIX.len()].copy_from_slice(ELECT_PREFIX);
        n += ELECT_PREFIX.len();
        write_num(f.node_id as u32, 3, &mut out[n..]);
        n += 3;
        out[n] = b' ';
        n += 1;
        write_num(f.epoch, 10, &mut out[n..]);
        n += 10;
        out[n] = b' ';
        n += 1;
        write_num(f.channel as u32, 2, &mut out[n..]);
        n += 2;
        out[n] = b' ';
        n += 1;
        write_num(f.gateway as u32, 3, &mut out[n..]);
        n += 3;
        out[n] = b' ';
        n += 1;
        for i in 0..N_CHANNELS {
            // Weights are 0..=48 by construction, so 2 digits always suffice.
            write_num(f.w[i] as u32, 2, &mut out[n..]);
            n += 2;
        }
        debug_assert_eq!(n, ELECT_LEN);
        Some(n)
    }

    /// Parse a received payload. Returns `None` unless it is a well-formed ELECT
    /// frame of exactly the right length with an in-range channel.
    ///
    /// Strict by design: this is fed straight from unauthenticated broadcasts, so
    /// every field is length- and digit-checked before it reaches election state.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<ElectFrame> {
        let rest = data.strip_prefix(ELECT_PREFIX)?;
        if rest.len() != ELECT_LEN - ELECT_PREFIX.len() {
            return None;
        }
        let node_id = u8::try_from(parse_num(&rest[0..3], 3)?).ok()?;
        if rest[3] != b' ' {
            return None;
        }
        let epoch = parse_num(&rest[4..14], 10)?;
        if rest[14] != b' ' {
            return None;
        }
        let channel = u8::try_from(parse_num(&rest[15..17], 2)?).ok()?;
        if rest[17] != b' ' {
            return None;
        }
        let gateway = u8::try_from(parse_num(&rest[18..21], 3)?).ok()?;
        if rest[21] != b' ' {
            return None;
        }
        ch_index(channel)?; // reject ch 0 / ch > 13 at the boundary
        let mut w = [0u8; N_CHANNELS];
        for i in 0..N_CHANNELS {
            let off = 22 + i * 2;
            w[i] = u8::try_from(parse_num(&rest[off..off + 2], 2)?).ok()?;
        }
        Some(ElectFrame {
            node_id,
            epoch,
            channel,
            gateway,
            w,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════════════════
// SMOL-ONLY WIRING — everything below has NO counterpart in the donor crate.
// ═══════════════════════════════════════════════════════════════════════════════════════════════
//
// Everything ABOVE this banner is the vendored cross-repo contract (see the lint-policy note at the
// top): it must stay byte-diffable against `esp32c6-watch:crates/mesh-elect` so a future re-sync
// shows only intended divergence. Everything BELOW is smol's own wiring — the announce schedule,
// the leaf follow state, the recovery ladder, and the send-path seal — and the watch has no
// equivalent because it is the *observer* in this pairing, not the announcer.
//
// Keeping the split at a single line rather than interleaving is the whole reason a re-sync stays
// mechanical: diff the top half, ignore the bottom. Add smol-only code HERE, never above.

// The master switch for ACTING on an announcement is `net::election::FOLLOW_ENABLED`, not here.
//
// It looks like it belongs in this module, and it does conceptually — but `mesh_elect` is
// `espnow`-gated while `election` is `wifi`-gated, and `espnow` implies `wifi`, so a flag declared
// here would be invisible to `net::wifi`'s config parser on a wifi-only build. It also has to sit
// beside the `MetricWeights` it selects, because the same bool choosing DOMINANT vs FOLLOWING and
// enabling the follow path IS the structural coupling those two changes needed.
//
// Nothing in this file reads it: every function below takes `follow` as a PARAMETER, which is also
// what lets the host verifier exercise both states without a cfg.

/// Member count carried on every announcement-derived [`Decision`].
///
/// [`Decision::supersedes`] breaks an epoch tie by member count, because the donor merges
/// *partitions* that each ran their own election. smol has exactly one announcer — the crown — so
/// there is no headcount to compare and the arm never discriminates. Pinning it to a constant says
/// that plainly instead of inventing a number: epoch orders announcements, and the lower-channel
/// tiebreak resolves the (already pathological) two-crowns-same-epoch case.
pub const ANNOUNCER_MEMBERS: u8 = 1;

/// Frames per announcement burst. Anchored to the existing `ODEL_BURST` (`net::mode`, #237): the
/// repeat count smol already uses when a decision must not be missed and the transport will not
/// acknowledge it. Reusing the number rather than inventing one keeps the two bursts comparable
/// when someone tunes either.
pub const ANNOUNCE_BURST: u8 = 6;

/// Spacing between frames of a burst. Anchored to `PREARM_GAP_MS`/`WAKE_GAP_MS` (`net::mode`), the
/// gap smol already uses for a repeated *broadcast* that a leaf must not miss (the OTAM wake-burst
/// — the same problem shape: unacked, one in-flight slot, receiver may be busy).
///
/// ⚠️ What a burst does and does NOT buy, because the difference decides whether the recovery
/// ladder below is optional: `ANNOUNCE_BURST × ANNOUNCE_GAP_MS` is 600 ms of coverage against
/// *collision and queue loss*. It buys nothing at all for a leaf parked on a different channel —
/// that leaf is deaf for a full `DWELL_MS` (1500 ms) and would miss every frame of the burst. Leaves
/// in that state are recovered by [`recovery_ladder`], never by repeats. Raising the burst count to
/// "cover" a parked leaf would be airtime spent on a case it cannot reach.
pub const ANNOUNCE_GAP_MS: u64 = 120;

/// Steady-state repeat interval, once a burst has drained. Anchored to smol's HELLO cadence
/// (`main`'s ~2 s "I'm here" advertisement): ELECT is the channel-plane twin of HELLO — one says
/// *who* is here, the other says *where the fleet meets* — so they beat at the same rate and a
/// reader tuning one finds the other.
///
/// Airtime is not a concern at this rate and the arithmetic should be on the page rather than
/// assumed: 61 B + the 9 B group-MAC trailer = 70 B every 2 s, from the single crown. That is
/// ~280 bit/s of a 1 Mbit/s basic rate.
pub const ANNOUNCE_IDLE_MS: u64 = 2_000;

/// Where a burst is in the announce-then-move sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Nothing to say.
    Idle,
    /// Announcing the new channel while still reachable on the OLD one.
    Pre,
    /// Announcing from the NEW channel, for leaves that were asleep or parked.
    Post,
}

/// The crown's announcement schedule: announce, move, announce again.
///
/// This is CSA's shape (see the prior-art section in the module header) — announce the switch
/// before performing it, then re-announce from the far side. Announcing only *after* guarantees a
/// window where the crown is deaf to its own fleet; announcing only *before* strands whoever was
/// asleep. Both bursts carry the SAME epoch, which is stronger than ordering them: a pre-move frame
/// arriving late is byte-identical to the post-move one, so there is nothing for it to override.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Announcer {
    epoch: u32,
    channel: u8,
    phase: Phase,
    left: u8,
    last_ms: u64,
    /// Settle gate for [`Self::observe_channel`]: a candidate channel and when we first saw it.
    /// `(0, _)` = nothing pending.
    pending: (u8, u64),
}

impl Default for Announcer {
    fn default() -> Self {
        Self::new()
    }
}

impl Announcer {
    /// Boot state: nothing decided, nothing to announce.
    #[must_use]
    pub const fn new() -> Self {
        Self { epoch: 0, channel: 0, phase: Phase::Idle, left: 0, last_ms: 0, pending: (0, 0) }
    }

    /// Feed an OBSERVED channel — one read off the radio every tick, which can therefore flap.
    /// Commits through [`Self::decide`] only once the new value has held for [`SETTLE_MS`].
    /// Returns true on the tick it commits.
    ///
    /// This is the anti-flap half the donor's constants were kept for. Without it a crown whose AP
    /// oscillates between two channels burns an epoch per oscillation, and since epoch is the total
    /// order every announcement out-ranks the last — so a flapping AP would not merely be noisy, it
    /// would make `supersedes` meaningless and drag a following fleet back and forth. Flapping costs
    /// an association and tears the mesh down; a suboptimal-but-stable channel does not.
    ///
    /// The FIRST channel commits immediately (`self.channel == 0`): there is no incumbent to protect
    /// and no flap to suppress, and making a booting crown sit silent for [`SETTLE_MS`] before
    /// saying where the fleet meets would be a self-inflicted 30 s hole in exactly the window when
    /// leaves are looking hardest.
    pub fn observe_channel(&mut self, channel: u8, now_ms: u64) -> bool {
        if ch_index(channel).is_none() || channel == self.channel {
            self.pending = (0, 0); // back on the committed channel → the candidate is withdrawn
            return false;
        }
        if self.channel == 0 {
            return self.decide(channel, now_ms); // cold start: nothing to protect
        }
        if self.pending.0 != channel {
            self.pending = (channel, now_ms); // a new candidate starts its own clock
            return false;
        }
        if now_ms.saturating_sub(self.pending.1) < SETTLE_MS {
            return false;
        }
        self.pending = (0, 0);
        self.decide(channel, now_ms)
    }

    /// The epoch currently being announced.
    #[must_use]
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The channel currently being announced (0 = nothing decided yet).
    #[must_use]
    pub const fn channel(&self) -> u8 {
        self.channel
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// A new channel decision: bump the epoch and arm the PRE-move burst. Returns `false` (and
    /// changes nothing) for an out-of-range channel or a channel we are already announcing, so a
    /// caller may drive this straight from an observed value without debouncing it — a re-decision
    /// on the same channel would otherwise burn an epoch per call and defeat `supersedes`.
    pub fn decide(&mut self, channel: u8, now_ms: u64) -> bool {
        if ch_index(channel).is_none() || channel == self.channel {
            return false;
        }
        self.epoch = self.epoch.saturating_add(1);
        self.channel = channel;
        self.phase = Phase::Pre;
        self.left = ANNOUNCE_BURST;
        // Back-date so the first frame of the burst goes out on the very next tick rather than
        // waiting a gap — the pre-move window is the one where the crown is still reachable.
        self.last_ms = now_ms.saturating_sub(ANNOUNCE_GAP_MS);
        true
    }

    /// True when one frame of the current burst should go out NOW; consumes that repeat.
    pub fn due(&mut self, now_ms: u64) -> bool {
        if self.phase == Phase::Idle || self.left == 0 {
            return false;
        }
        if now_ms.saturating_sub(self.last_ms) < ANNOUNCE_GAP_MS {
            return false;
        }
        self.left -= 1;
        self.last_ms = now_ms;
        true
    }

    /// True when a steady-state repeat should go out — the burst has drained and
    /// [`ANNOUNCE_IDLE_MS`] has passed. Consumes the slot.
    ///
    /// The bursts cover a migration; this covers everything else. A leaf that boots, wakes, or
    /// finally lands on the right channel learns the current decision within one interval instead of
    /// waiting for the next migration, which on a stable fleet may never come. It is also what makes
    /// the frame observable at all on a fleet that is not migrating — which is the entire value of
    /// the observe-only landing, and therefore not optional.
    pub fn beacon_due(&mut self, now_ms: u64) -> bool {
        if self.phase == Phase::Idle || self.left > 0 {
            return false; // idle has nothing to say; a live burst is already saying it
        }
        if now_ms.saturating_sub(self.last_ms) < ANNOUNCE_IDLE_MS {
            return false;
        }
        self.last_ms = now_ms;
        true
    }

    /// True once the PRE-move burst has drained — the caller may now retune. Kept as a question the
    /// caller asks rather than a callback, so the move stays where the radio is owned.
    #[must_use]
    pub const fn clear_to_move(&self) -> bool {
        matches!(self.phase, Phase::Pre) && self.left == 0
    }

    /// Arm the POST-move burst, at the SAME epoch. Call immediately after the retune.
    pub fn moved(&mut self, now_ms: u64) {
        self.phase = Phase::Post;
        self.left = ANNOUNCE_BURST;
        self.last_ms = now_ms.saturating_sub(ANNOUNCE_GAP_MS);
    }

    /// True once the POST burst has drained too — the migration is over.
    #[must_use]
    pub const fn settled(&self) -> bool {
        matches!(self.phase, Phase::Post) && self.left == 0
    }

    /// The decision this announcer is broadcasting, for the frame builder.
    #[must_use]
    pub const fn decision(&self, gateway: u8) -> Decision {
        Decision { channel: self.channel, epoch: self.epoch, gateway }
    }
}

/// What a leaf should do with an inbound announcement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Follow {
    /// Stale, replayed, or simply not newer — ignore it. Costs nothing and, importantly, does not
    /// reset the probation clock: a replayed old frame must not look like liveness.
    Stale,
    /// Accepted, and it names the channel we are already on. Nothing to do but note we heard it.
    Confirmed,
    /// Accepted, and the fleet is moving. The caller retunes to this channel — IFF
    /// [`FOLLOW_ENABLED`].
    Move(u8),
}

/// A leaf's view of the announced channel decision.
///
/// Deliberately records what it heard even when [`FOLLOW_ENABLED`] is false. Observation and action
/// are separate concerns: the observe-only landing is worth having precisely because it lets the
/// fleet report what it *would* have done — over DIAG, on real hardware, against a real crown —
/// before anything moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Follower {
    decision: Decision,
    /// When we last accepted an announcement (0 = never). Drives [`Self::probation_expired`].
    heard_ms: u64,
    /// Announcements accepted since boot — a monotonic observability counter, so "did this board
    /// ever hear the crown" is answerable from a DIAG record instead of a serial console.
    accepted: u16,
    /// Accepted announcements that named a DIFFERENT channel than the one we were on — i.e. the
    /// ones a following leaf would have acted on.
    ///
    /// This is the number the canary roll actually needs, and it is not the same as `accepted`.
    /// In the steady state every leaf is by definition co-channel with the crown (ESP-NOW is
    /// per-channel — a leaf on another channel hears nothing at all), so `accepted` only ever
    /// proves the beacon works. `moves` is non-zero exactly when a leaf caught the crown's
    /// PRE-move burst, which is the one hard-to-hit part of the whole design and the thing
    /// observe-only exists to measure before anything is allowed to act on it.
    moves: u16,
}

impl Default for Follower {
    fn default() -> Self {
        Self::new()
    }
}

impl Follower {
    #[must_use]
    pub const fn new() -> Self {
        Self { decision: Decision::bootstrap(), heard_ms: 0, accepted: 0, moves: 0 }
    }

    #[must_use]
    pub const fn decision(&self) -> Decision {
        self.decision
    }

    #[must_use]
    pub const fn accepted(&self) -> u16 {
        self.accepted
    }

    /// See [`Self::moves`] — the count a canary roll reads.
    #[must_use]
    pub const fn moves(&self) -> u16 {
        self.moves
    }

    /// Ingest one parsed frame. `my_channel` is the channel we are on right now (0 = unknown).
    ///
    /// Ordering is [`Decision::supersedes`] verbatim — the donor's total order, so both repos agree
    /// on which of two announcements wins. Note the boot edge it implies: [`Decision::bootstrap`]
    /// sits at epoch 0 on [`RENDEZVOUS_CHANNEL`], so an epoch-0 announcement for a LOWER channel
    /// number supersedes it on the channel tiebreak. That is the donor's rule and it is harmless
    /// here (a real crown's first `decide` lands on epoch 1), but it is a rule, not an accident, so
    /// it is written down rather than discovered.
    pub fn observe(&mut self, f: &wire::ElectFrame, now_ms: u64, my_channel: u8) -> Follow {
        let d = Decision { channel: f.channel, epoch: f.epoch, gateway: f.gateway };
        if !d.supersedes(ANNOUNCER_MEMBERS, &self.decision, ANNOUNCER_MEMBERS) {
            return Follow::Stale;
        }
        self.decision = d;
        self.heard_ms = now_ms;
        self.accepted = self.accepted.saturating_add(1);
        if d.channel == my_channel {
            Follow::Confirmed
        } else {
            self.moves = self.moves.saturating_add(1);
            Follow::Move(d.channel)
        }
    }

    /// Has the adopted epoch gone quiet for longer than [`PROBATION_MS`]?
    ///
    /// This is the answer to the failure mode a monotonic epoch invites: one node with a high
    /// persisted epoch can otherwise wedge the entire fleet onto a dead channel permanently, and no
    /// later announcement can ever out-rank it. After probation the leaf stops treating the adopted
    /// decision as authoritative and falls back to the [`recovery_ladder`].
    #[must_use]
    pub fn probation_expired(&self, now_ms: u64) -> bool {
        self.heard_ms != 0 && now_ms.saturating_sub(self.heard_ms) > PROBATION_MS
    }
}

/// Channels a consumer AP is most likely to sit on, after the rendezvous. 1/6/11 are the three
/// non-overlapping 2.4 GHz allocations and the only ones most routers auto-select; 6 is already
/// [`RENDEZVOUS_CHANNEL`], so these are the remaining two.
pub const COMMON_AP_CHANNELS: [u8; 2] = [1, 11];

/// Upper bound on a ladder: every channel in the band, at most once.
pub const RECOVERY_MAX: usize = N_CHANNELS;

/// The historical blind-scan plan (`net::mode::leaf_scan_tick`'s `CANDIDATES`, "JP's roam plan").
/// Named here so [`recovery_ladder`] can *return* it verbatim when following is off — which turns
/// "the flag changes nothing on a live fleet" from a claim into something a host test asserts.
pub const LEGACY_CANDIDATES: [u8; 3] = [1, 6, 11];

/// The ordered channel-probe plan for a leaf that has to re-find the mesh, best guess first.
///
/// # Cost model — re-derived for smol, NOT inherited
///
/// The design record costs this ladder at 10–20 ms per rung and 130–260 ms for a full sweep. Those
/// numbers are `esp-radio`'s **`scan_async`** dwell times, and smol's leaf recovery does not scan:
/// `leaf_scan_tick` retunes the ESP-NOW PHY and *listens* for the crown's HELLO for `DWELL_MS`
/// (1500 ms) before hopping. So the real cost of a rung here is `DWELL_MS`, ~100× the scan figure,
/// and the real budget is 13 × 1500 ms ≈ 19.5 s to exhaust the band.
///
/// That is a much better trade than it looks, and the ranking is what makes it one. Today's plan
/// probes three channels forever; a crown that moved to ch3 is simply never found. This finds it,
/// and puts the *likely* answers first: a leaf that still remembers the last channel is back in one
/// rung, where today it may cycle up to three. Slower worst case, faster common case, and a
/// reachable band instead of an unreachable one.
///
/// (It also does not cost the association, which is the other reason not to reach for a scan: a
/// scan drops it — `coex_background_scan` is hardcoded false — and a leaf mid-recovery is exactly
/// who cannot afford that.)
///
/// # Rungs
///
/// Tier 0 of the design ladder ("heard the announcement") is not a rung: this list is what a leaf
/// walks *because* tier 0 did not happen. Rungs here are the remaining tiers, in order:
///
/// | rung | tier | why |
/// |---|---|---|
/// | 0 | 1 | `last_known` — where the mesh was when we last heard it |
/// | 1 | 2 | [`RENDEZVOUS_CHANNEL`] — the one recovery that cannot fail for timing reasons |
/// | 2,3 | 3 | [`COMMON_AP_CHANNELS`] — where a consumer AP probably landed |
/// | 4.. | 4 | the rest of the band ascending — the sweep, degraded into rungs |
///
/// Duplicates are dropped, so a leaf whose `last_known` is already the rendezvous does not spend
/// `DWELL_MS` proving it twice. `last_known == 0` ("never knew one") simply starts at rung 1.
///
/// With `follow` false this returns [`LEGACY_CANDIDATES`] unchanged — byte-for-byte today's
/// behaviour. The ladder is only reachable in a world where the channel can actually move, and
/// shipping a live behaviour change alongside a flag that is supposed to change nothing is how a
/// canary roll stops being able to attribute what it sees.
#[must_use]
pub fn recovery_ladder(last_known: u8, follow: bool) -> ([u8; RECOVERY_MAX], usize) {
    let mut out = [0u8; RECOVERY_MAX];
    let mut n = 0;

    if !follow {
        for (i, &ch) in LEGACY_CANDIDATES.iter().enumerate() {
            out[i] = ch;
            n += 1;
        }
        return (out, n);
    }

    let push = |ch: u8, out: &mut [u8; RECOVERY_MAX], n: &mut usize| {
        if ch_index(ch).is_none() {
            return; // 0 = never knew one, or a corrupt value that must not be tuned to
        }
        if out[..*n].contains(&ch) {
            return;
        }
        out[*n] = ch;
        *n += 1;
    };

    push(last_known, &mut out, &mut n);
    push(RENDEZVOUS_CHANNEL, &mut out, &mut n);
    for &ch in COMMON_AP_CHANNELS.iter() {
        push(ch, &mut out, &mut n);
    }
    for ch in 1..=(N_CHANNELS as u8) {
        push(ch, &mut out, &mut n);
    }
    (out, n)
}

// ── The send path is a SECURITY boundary, so it is a TYPE, not a comment ───────────────────────
//
// The module header states the invariant: ELECT must go out via `send_to`, which appends #190's
// group-MAC trailer, and never via `send_arb_raw`, which does not. A leaf cannot verify an
// announcement before acting on it — checking costs it the association it would need if the
// announcement were false — so an unauthenticated ELECT is a remote fleet-stranding primitive.
//
// A comment is not a mechanism. Stage A wrote that invariant down in prose and it would have
// survived any refactor that violated it, silently. So the encoded frame is now a type whose bytes
// have NO accessor: the only thing you can do with a `SealedElect` is hand it to a `GroupMacSink`,
// and the only implementation of that trait routes to `send_to`.
//
// ⚠️ What this does NOT catch, enumerated rather than assumed — every one of these is covered by
// `tools/check_elect_send_path.py`, which reads source structure, and NONE of them is covered by
// the type system alone:
//   1. Someone rewrites the one-line `GroupMacSink` impl body to call `send_arb_raw`. This is the
//      LIKELIEST shape by a distance: it is a one-line edit in the direction of "make it compile".
//   2. Someone adds a SECOND `GroupMacSink` impl that sends raw.
//   3. Someone calls `wire::encode` directly into a local buffer and sends that, bypassing the seal.
//   4. Someone hand-builds the frame from the `SMOLv1 ELECT ` literal without touching `encode`.
//   5. Someone adds a new raw `esp_now.send` call site.
//   6. Someone changes `should_group_mac` so ELECT stops being MAC'd — the trailer disappears with
//      the send path untouched. (Pinned by an ELECT case in `experiments/mac_verify`.)

/// A sink that appends the #190 group-MAC trailer to what it sends.
///
/// The name is the contract. An implementation that does not append the trailer satisfies the
/// compiler and breaks the fleet, which is why the single implementation is also checked by
/// `tools/check_elect_send_path.py` rather than trusted.
pub trait GroupMacSink {
    /// Send `frame` to `dst`, appending the group-MAC trailer.
    fn send_group_mac(&mut self, dst: &[u8; 6], frame: &[u8]);
}

/// An encoded ELECT frame that can only leave via a [`GroupMacSink`].
///
/// The buffer is private and there is no accessor, no `Deref`, no `as_bytes`. [`Self::emit`]
/// consumes `self`, so a frame cannot be sealed once and sent twice down different paths either.
pub struct SealedElect {
    buf: [u8; wire::ELECT_LEN],
}

impl SealedElect {
    /// Encode `f`. `None` for an out-of-range channel — the same rejection [`wire::encode`] makes,
    /// surfaced here so a malformed decision can never reach the air.
    #[must_use]
    pub fn seal(f: &wire::ElectFrame) -> Option<Self> {
        let mut buf = [0u8; wire::ELECT_LEN];
        let n = wire::encode(f, &mut buf)?;
        // `encode` writes a fixed-width record or nothing; a short write would mean the layout
        // constants disagree with the writer, which is a bug, not an input error.
        debug_assert_eq!(n, wire::ELECT_LEN);
        Some(Self { buf })
    }

    /// Hand the frame to the authenticated send path. Consumes `self`.
    pub fn emit<S: GroupMacSink + ?Sized>(self, sink: &mut S, dst: &[u8; 6]) {
        sink.send_group_mac(dst, &self.buf);
    }
}
