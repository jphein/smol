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
//! | [`SETTLE_MS`], [`MARGIN_FLOOR`], [`PROBATION_MS`], [`margin_for`] | anti-flap: a challenger must hold its advantage before the fleet moves |
//! | [`RENDEZVOUS_CHANNEL`] | tier 2 of the recovery ladder below |
//! | [`weight`], [`USABLE_MIN_DBM`] | still populate the frame's `w[13]` honestly — see below |
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
//! # Recovery ladder for a leaf that missed the announcement
//!
//! esp-radio 0.18 exposes all-channels or exactly one channel — never a subset
//! (`channel_bitmap` is hardcoded to 0). A full sweep costs **130-260 ms**
//! (13 x `Active{min:10,max:20}`); a single-channel probe costs ~10-20 ms. So
//! recovery is an ordered ladder with early exit, not one expensive sweep:
//!
//! ```text
//! 0. heard the announcement        ~0
//! 1. last-known mesh channel       ~10-20 ms
//! 2. RENDEZVOUS_CHANNEL (6)        ~10-20 ms
//! 3. common AP channels (1, 11)    ~10-20 ms each
//! 4. full sweep                    130-260 ms
//! ```
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
pub const OBS_STALE_MS: u64 = 60_000;

/// A challenger must hold its lead this long before we spend an epoch on it.
/// Flapping costs an association and tears the mesh down, so it is strictly
/// worse than a suboptimal-but-stable channel.
pub const SETTLE_MS: u64 = 30_000;

/// Floor for the margin a challenger must beat the incumbent by (in summed
/// weight). See [`margin_for`].
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
