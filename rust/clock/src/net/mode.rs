//! Phase 3 — ESP-NOW peer messaging + honest WiFi <-> ESP-NOW switching.
//!
//! ======================================================================
//! THE SINGLE-RADIO CONSTRAINT (this is the whole point of this module)
//! ======================================================================
//!
//! The ESP32-C3 has exactly ONE 2.4 GHz radio and one PHY. It can be tuned to
//! exactly ONE channel at any instant. WiFi (infrastructure STA) and ESP-NOW
//! are NOT two independent radios — they are two ways of using the same PHY.
//! Consequences we cannot engineer away:
//!
//!   * While associated to an AP, the radio MUST sit on that AP's channel.
//!     ESP-NOW frames can still be TX/RX'd, but only on that same channel, so
//!     every ESP-NOW peer must already be on the AP's channel.
//!
//!   * ESP-NOW itself is connectionless and channel-specific: a receiver only
//!     hears frames sent on the channel it is currently tuned to.
//!
//! There are therefore exactly two honest ways to run both:
//!
//!   (a) COEXIST  — stay associated to the AP and pin ESP-NOW to the AP's
//!                  channel. Pro: WiFi (NTP/weather) stays available. Con: all
//!                  peers must discover and match the AP's channel, which can
//!                  change (e.g. band steering), and DTIM power-save adds RX
//!                  latency for ESP-NOW.
//!
//!   (b) TIME-SHARE — bring WiFi up in a short BURST (associate, NTP, weather),
//!                  then DROP the WiFi association and pin the radio to a FIXED,
//!                  well-known ESP-NOW channel that all peers agree on. Pro:
//!                  deterministic ESP-NOW channel, lower power. Con: no WiFi
//!                  while in ESP-NOW mode; re-syncing time means another burst.
//!
//! `RadioManager::switch()` implements BOTH, selected by `Mode`. `main` uses
//! the TIME-SHARE strategy by default (WiFi burst for NTP, then ESP-NOW on a
//! fixed channel) because it needs no channel discovery between peers.
//!
//! Because esp-wifi's `Interfaces` hands out BOTH the WiFi `sta` device and the
//! `esp_now` handle from the SAME radio init, "switching" here does not tear
//! the radio down; it (a) chooses which stack we actively service and (b) sets
//! the PHY channel accordingly. Dropping the STA `WifiDevice` + calling
//! `controller.stop()` is what actually frees the airtime for TIME-SHARE mode.

extern crate alloc;

use esp_hal::{
    rng::Rng,
    time::Instant,
    timer::timg::TimerGroup,
};
use esp_wifi::{
    esp_now::{EspNow, EspNowWifiInterface, PeerInfo, BROADCAST_ADDRESS},
    wifi::{WifiController, WifiMode},
    EspWifiController,
};

use crate::led::{Led, LedState};
use crate::net::WifiPeripherals;

/// Fixed ESP-NOW channel used in TIME-SHARE mode. All smol units must agree on
/// this value (1..=13). 6 is a common, low-congestion default.
const ESP_NOW_FIXED_CHANNEL: u8 = 6;

// =========================================================================
// Peer handshake protocol (drives the blue status LED).
// =========================================================================
//
// ESP-NOW is connectionless: sending a broadcast tells you NOTHING about who
// (if anyone) received it. To honestly distinguish "I can hear a peer" from
// "a peer and I have a working two-way link", we run a tiny explicit handshake
// on top of ESP-NOW broadcasts:
//
//   * Every unit periodically BROADCASTS a HELLO beacon carrying its own id.
//   * When unit B hears unit A's HELLO, B learns A exists (A is "detected") and
//     replies with a *unicast* ACK echoing A's id ("I, B, heard you, A").
//   * When A receives an ACK carrying A's own id, A now has proof the frame it
//     sent was received by someone AND that someone is talking back — i.e. the
//     link is bidirectional. A is "connected".
//
// Mapping to LED states (see crate::led):
//   * heard a HELLO within PEER_STALE_MS, but no fresh ACK-for-us  -> Detected
//   * received an ACK addressed to our id within PEER_STALE_MS      -> Connected
//   * neither within PEER_STALE_MS                                  -> Idle (off)
//
// Everything is edge-free and timestamp-based: we just remember the monotonic
// time of the last relevant event and compare against `now` each tick, so a
// peer going away naturally decays Connected -> Detected -> Idle as its frames
// stop arriving. No allocation, no fixed peer table — one remote peer is enough
// to light the LED, which matches the "is anyone out there / are we linked"
// question the LED answers.

/// A frame is considered "recent" for this long. Beyond it the corresponding
/// LED state decays (Connected/Detected -> lower). ~3 s per the spec: long
/// enough to ride over a couple of missed beacons, short enough that unplugging
/// the peer visibly drops the LED within a few seconds.
const PEER_STALE_MS: u64 = 3_000;

/// Wire tags. Kept as short ASCII prefixes so payloads stay tiny and are
/// human-readable in a serial sniffer. `SMOLv1` namespaces us off other
/// ESP-NOW traffic on the channel.
const HELLO_PREFIX: &[u8] = b"SMOLv1 HELLO "; // + 3-digit id
const ACK_PREFIX: &[u8] = b"SMOLv1 ACK "; // + 3-digit id (the id being acked)
/// BENCH beacon tag: `"SMOLv1 BEACON "` + 3-digit id + `" "` + 5-digit seq +
/// `" "` + 5-digit echo_seq (the last seq we heard FROM the peer). Sent only by
/// BENCH mode, on top of the normal HELLO/ACK handshake, so the LED path is
/// untouched. See [`RadioManager::broadcast_beacon`].
const BEACON_PREFIX: &[u8] = b"SMOLv1 BEACON "; // + "NNN SSSSS EEEEE"
/// Mesh time-sync tag: `"SMOLv1 TIME "` + 3-digit id + `" "` + 10-digit Unix
/// time + `" "` + 10-digit `synced_at`. 10 digits spans the full u32 range
/// (max 4_294_967_295), fixed-width and sniffer-readable — the same discipline
/// as BEACON. A *separate* frame from HELLO on purpose: the LED handshake wire
/// format is hardware-verified and must not change. Broadcast on the same ~2 s
/// tick as HELLO (see `main`). See [`RadioManager::broadcast_time`].
///
/// SECURITY: ESP-NOW here is unauthenticated and unencrypted, so ANY device on
/// the channel can inject a TIME frame with an arbitrary, far-future
/// `synced_at` and thereby hijack every mesh clock. Acceptable for a hobby mesh
/// on a private fixed channel; harden with a signed payload or an ESP-NOW LMK
/// if it ever matters. Documented, not defended.
const TIME_PREFIX: &[u8] = b"SMOLv1 TIME "; // + "NNN UUUUUUUUUU SSSSSSSSSS"
/// Relay-bridge tags (see the "Relay bridge" section below). RELAY carries a
/// fragment of a leaf's telemetry uplink; RELAYACK is the gateway's per-message
/// received-fragment bitmap so the leaf can retransmit gaps. Distinct tags to
/// keep the SMOLv1 namespace clean for the in-flight MMO-snake frames (issue #5).
/// The trailing space on RELAY disambiguates it from RELAYACK at parse time
/// (`"SMOLv1 RELAY "[12]` is ' ' where RELAYACK has 'A'), so match order is moot.
const RELAY_PREFIX: &[u8] = b"SMOLv1 RELAY "; // + "NNN MMMMM F C " + chunk
const RELAYACK_PREFIX: &[u8] = b"SMOLv1 RELAYACK "; // + "MMMMM BBB"

use crate::mesh_snake::snake_core::{self, SnkFrame};

/// Depth of the decoded MMO-snake RX ring. A full 16-peer broadcast burst plus
/// background traffic can land between two 20 ms `service()` calls; 8 covers a
/// realistic burst and, like the ESP-NOW hardware queue, DROPS OLDEST on
/// overflow — which the game's absolute-state + staleness tolerates by design.
const SNK_RX_RING: usize = 8;

/// A tiny FIFO of decoded [`SnkFrame`]s buffered by `service()` for `main` to
/// drain into the game's `PeerTable` each subtick. Keeps `mode.rs` free of game
/// state (mirrors the `TimeTracker`/`take_time_offer` split). All `Copy`, fixed
/// size → `.bss`, no heap.
struct SnkInbox {
    buf: [Option<SnkFrame>; SNK_RX_RING],
    head: usize,
    tail: usize,
    len: usize,
}

impl SnkInbox {
    const fn new() -> Self {
        Self { buf: [None; SNK_RX_RING], head: 0, tail: 0, len: 0 }
    }

    /// Push a frame; drop the OLDEST if full (matches the RX-queue policy).
    fn push(&mut self, f: SnkFrame) {
        self.buf[self.head] = Some(f);
        self.head = (self.head + 1) % SNK_RX_RING;
        if self.len < SNK_RX_RING {
            self.len += 1;
        } else {
            self.tail = (self.tail + 1) % SNK_RX_RING; // overwrote the tail
        }
    }

    /// Pop the oldest buffered frame, or `None` if empty.
    fn pop(&mut self) -> Option<SnkFrame> {
        if self.len == 0 {
            return None;
        }
        let f = self.buf[self.tail].take();
        self.tail = (self.tail + 1) % SNK_RX_RING;
        self.len -= 1;
        f
    }
}

/// Parsed inbound frame. The `'a` borrows the RX buffer for `Relay`'s payload
/// chunk (copied out immediately in `service`); every other variant carries only
/// copied scalars, so `'a` is used by exactly one variant — which is allowed.
enum Frame<'a> {
    /// An MMO Mesh Snake state snapshot (issue #5): the decoded 18 B SMOLv1 SNK
    /// frame. Scalar-only (no borrow).
    Snk(SnkFrame),
    /// A peer HELLO beacon (LED handshake); carries the sender's id.
    Hello(u8),
    /// An acknowledgement (LED handshake); carries the id of the unit acked.
    Ack(u8),
    /// A BENCH beacon: (sender_id, sender_seq, echo_seq). `echo_seq` is the last
    /// of OUR seqs the sender had heard when it sent this — an echo_seq that
    /// matches a seq we recently sent lets us compute round-trip time.
    Beacon { seq: u32, echo: u32 },
    /// A mesh time offer: the peer's current Unix-time estimate plus the
    /// `synced_at` that time descends from (the Unix instant of the peer's last
    /// authoritative NTP sync; 0 = never synced). `main` adopts it iff
    /// `synced_at` is strictly newer than ours — see `main::should_adopt`.
    Time { unix: u32, synced_at: u32 },
    /// One fragment of a leaf's relay-uplink telemetry: sender id, per-source
    /// rolling `msgid`, fragment index/count, and up to `RELAY_CHUNK` payload
    /// bytes. `chunk` borrows the RX buffer. See the "Relay bridge" section.
    Relay {
        src_id: u8,
        msgid: u16,
        frag: u8,
        count: u8,
        chunk: &'a [u8],
    },
    /// A gateway's acknowledgement of a relay message: the `msgid` and the u8
    /// bitmap of fragments received so far, so the leaf resends only the gaps.
    RelayAck { msgid: u16, bitmap: u8 },
}

/// Tracks the two timestamps that define the peer link state. Monotonic ms
/// (`Instant::now().duration_since_epoch().as_millis()`); 0 = "never seen".
struct PeerTracker {
    /// Last time we heard ANY peer HELLO (proves we can hear a peer).
    last_hello_ms: u64,
    /// Last time we received an ACK addressed to OUR id (proves a peer heard
    /// us -> the link is bidirectional).
    last_ack_for_us_ms: u64,
}

impl PeerTracker {
    const fn new() -> Self {
        Self {
            last_hello_ms: 0,
            last_ack_for_us_ms: 0,
        }
    }

    /// Fresh iff seen and within the staleness window at `now_ms`.
    #[inline]
    fn fresh(stamp_ms: u64, now_ms: u64) -> bool {
        stamp_ms != 0 && now_ms.saturating_sub(stamp_ms) <= PEER_STALE_MS
    }

    /// Collapse the two timestamps into the current [`LedState`] peer state.
    /// Connected takes priority (bidirectional proof implies we also heard the
    /// peer), then Detected, else Idle.
    fn state(&self, now_ms: u64) -> LedState {
        if Self::fresh(self.last_ack_for_us_ms, now_ms) {
            LedState::Connected
        } else if Self::fresh(self.last_hello_ms, now_ms) {
            LedState::PeerDetected
        } else {
            LedState::Idle
        }
    }
}

// =========================================================================
// Mesh time sync (inherited absolute sync-timestamp — see main::should_adopt).
// =========================================================================
//
// Each node tracks `synced_at` = the Unix time at which its own clock was last
// set AUTHORITATIVELY (NTP at boot, or inherited on adoption). It advertises
// `(current_unix_estimate, synced_at)` in a TIME frame and adopts a peer's
// offer IFF `peer.synced_at > my.synced_at` (strict), inheriting the peer's
// `synced_at` rather than "now". Freshness therefore travels WITH the time, so
// no chain of adoptions can inflate a `synced_at` beyond the origin NTP node's
// — the mesh converges and stops swapping (loop-free). A never-synced node
// (`synced_at == 0`) adopts from anyone real; two never-synced nodes ignore
// each other (0 is not strictly > 0), so free-runners never fight.
//
// This tracker only BUFFERS the best (freshest) offer heard since `main` last
// took it; the adopt DECISION and the clock re-anchor live in `main`, keeping
// this module free of clock-representation knowledge (the single-radio realities
// documented at the top of the file are already enough for one module to own).

/// Buffers the freshest peer TIME offer seen since the last
/// [`RadioManager::take_time_offer`]. Mirrors the small-tracker pattern of
/// [`PeerTracker`]/[`BenchTracker`].
struct TimeTracker {
    /// The peer's advertised current-time estimate (Unix seconds) from the best
    /// offer buffered so far.
    best_unix: u32,
    /// That offer's `synced_at` — the freshness key the adopt decision compares.
    best_synced_at: u32,
    /// Whether an un-taken offer is currently buffered.
    have: bool,
}

impl TimeTracker {
    const fn new() -> Self {
        Self {
            best_unix: 0,
            best_synced_at: 0,
            have: false,
        }
    }
}

// =========================================================================
// BENCH link statistics (ESP-NOW mesh test — see src/bench.rs).
// =========================================================================
//
// BENCH mode adds a periodic BEACON on top of the HELLO/ACK handshake. Each
// BEACON carries our own incrementing `seq` and an `echo` = the highest seq we
// have heard FROM the peer. RTT is measured by remembering, in a tiny ring, the
// send time of each of our recent seqs; when a peer BEACON echoes one of them
// back, RTT = now − send_time[echoed_seq]. TX/RX rates and packet-loss are
// derived from counters + the peer's observed seq gaps. All of this is passive
// bookkeeping updated inside `service()`/`broadcast_beacon()`; the clock/LED
// path never touches it.

/// How many recent outbound BEACON seqs we remember send-times for, to match an
/// echoed seq back to its send time for RTT. Power of two for cheap masking; 32
/// beacons at a few Hz covers several seconds of in-flight seqs.
const RTT_RING: usize = 32;

/// Rolling BENCH counters/gauges owned by [`RadioManager`]. Snapshotted into a
/// [`BenchStats`] for the UI via [`RadioManager::bench_stats`].
struct BenchTracker {
    /// Our next outbound BEACON sequence number (monotonic, wraps naturally).
    tx_seq: u32,
    /// Send time (monotonic ms) of each recent seq, indexed by `seq % RTT_RING`.
    /// 0 = slot unused/expired.
    send_time: [u64; RTT_RING],
    /// Total BEACONs we've transmitted (for TX/sec).
    tx_count: u32,
    /// Total peer BEACONs we've received (for RX/sec).
    rx_count: u32,
    /// Highest peer seq seen so far (for gap/loss detection); `None` until first.
    peer_last_seq: Option<u32>,
    /// Count of peer seqs we inferred missing (gaps in the peer's seq stream).
    lost_count: u32,
    /// Most recent RTT sample in ms (from an echoed seq), if any.
    last_rtt_ms: Option<u32>,
    /// Most recent RSSI (dBm) from a peer BEACON's RX control info, if any.
    last_rssi: Option<i32>,
    /// Window start (ms) + counts at window start, for per-second rates.
    rate_window_start_ms: u64,
    tx_at_window: u32,
    rx_at_window: u32,
    /// Latest computed rates (updated once per rate window).
    tx_per_s: u32,
    rx_per_s: u32,
}

impl BenchTracker {
    const fn new() -> Self {
        Self {
            tx_seq: 0,
            send_time: [0; RTT_RING],
            tx_count: 0,
            rx_count: 0,
            peer_last_seq: None,
            lost_count: 0,
            last_rtt_ms: None,
            last_rssi: None,
            rate_window_start_ms: 0,
            tx_at_window: 0,
            rx_at_window: 0,
            tx_per_s: 0,
            rx_per_s: 0,
        }
    }

    /// Recompute per-second TX/RX rates roughly once per second.
    fn tick_rates(&mut self, now_ms: u64) {
        if self.rate_window_start_ms == 0 {
            self.rate_window_start_ms = now_ms;
            self.tx_at_window = self.tx_count;
            self.rx_at_window = self.rx_count;
            return;
        }
        let elapsed = now_ms.saturating_sub(self.rate_window_start_ms);
        if elapsed >= 1000 {
            // Scale to a per-second figure over the (≈1 s) window.
            self.tx_per_s =
                ((self.tx_count.wrapping_sub(self.tx_at_window)) as u64 * 1000 / elapsed) as u32;
            self.rx_per_s =
                ((self.rx_count.wrapping_sub(self.rx_at_window)) as u64 * 1000 / elapsed) as u32;
            self.rate_window_start_ms = now_ms;
            self.tx_at_window = self.tx_count;
            self.rx_at_window = self.rx_count;
        }
    }

    /// Packet-loss percent = lost / (received + lost), 0 if nothing seen yet.
    fn loss_pct(&self) -> u8 {
        let denom = self.rx_count + self.lost_count;
        if denom == 0 {
            0
        } else {
            ((self.lost_count as u64 * 100) / denom as u64) as u8
        }
    }
}

/// Immutable snapshot of the BENCH link stats for the UI (see `src/bench.rs`).
/// FPS is measured by the render loop, not here, so it is added by the caller.
#[derive(Clone, Copy)]
pub struct BenchStats {
    /// BEACON transmissions per second (our outbound rate).
    pub tx_per_s: u32,
    /// Peer BEACONs received per second (inbound rate).
    pub rx_per_s: u32,
    /// Latest round-trip time in ms (from an echoed seq), or `None`.
    pub rtt_ms: Option<u32>,
    /// Estimated packet-loss percent from peer seq gaps (0..100).
    pub loss_pct: u8,
    /// Latest peer RSSI in dBm, or `None` if no BEACON heard yet.
    pub rssi: Option<i32>,
    /// Current peer link state (Idle / PeerDetected / Connected).
    pub link: LedState,
}

// =========================================================================
// Relay bridge (ESP-NOW -> internet telemetry, single hop).
// =========================================================================
//
// Spec: scratch/smol/relay-bridge-spec.md; feasibility: nebula-espnow-gateway.md.
//
// A LEAF (no WiFi at boot) periodically fragments its short telemetry into
// `SMOLv1 RELAY` frames and BROADCASTS them. A GATEWAY (associated at boot)
// reassembles keyed by (src MAC, msgid), unicasts a `SMOLv1 RELAYACK` bitmap so
// the leaf retransmits only missing fragments (bounded to RELAY_MAX_TRIES),
// buffers completed messages, and every RELAY_FLUSH_INTERVAL_MS runs a WiFi burst
// to UDP them to a fixed collector, then returns to ESP-NOW ch6.
//
// SINGLE-RADIO AIRTIME COST: a flush burst tunes the one PHY to the AP's channel,
// so the mesh is DEAF on ch6 for the ~seconds it lasts (the documented one-radio
// trade-off — see the module header). Flushes are tens of seconds apart and
// telemetry is loss-tolerant, so this is acceptable; retransmit rides over it.
//
// HONESTY (compile-verified only): the flush uses the PROVEN TIME-SHARE burst
// (disconnect -> associate -> UDP -> re-pin ch6), the same pattern boot NTP uses,
// NOT true concurrent COEXIST. Per Nebula, STA-associated + ESP-NOW RX
// reliability / DTIM latency under real COEXIST is UNVERIFIED on this board, so we
// deliberately pause RX during the burst rather than depend on it.
//
// SECURITY: like every SMOLv1 frame, RELAY is unauthenticated + unencrypted — any
// on-channel device can inject telemetry attributed to any src id, or spoof a
// RELAYACK. Fine for a hobby mesh; sign or LMK-encrypt if it ever matters.
//
// OUT OF SCOPE this run (documented stubs, NOT implemented):
//   * DOWNLINK (collector -> leaf): needs a poll/queue on the gateway + unicast
//     fragmentation back to the leaf MAC. `service` already has the leaf MAC on
//     every RX, and `send_to` already unicasts, so the hook exists — but the
//     collector-side queue/protocol is unspecified, so it is deferred.
//   * MULTI-HOP (leaf -> relay -> ... -> gateway): needs a next-hop/TTL routing
//     header + a loop-prevention seen-set + a shared-channel invariant across
//     every node (+200-400 LOC). This is single-hop uplink only.

/// Max telemetry payload per RELAY fragment (bytes). On the wire a frame is the
/// fixed 27-byte ASCII header + this, comfortably under the 250 B ESP-NOW limit.
const RELAY_CHUNK: usize = 64;
/// Max fragments per message. Kept <= 8 so the received-fragment bitmap is a
/// single `u8`; => max reassembled telemetry = CHUNK * FRAGS bytes.
const RELAY_MAX_FRAGS: usize = 4;
/// Max reassembled message length (bytes) = 256. A leaf truncates telemetry to
/// this (documented) — this is SHORT-telemetry relay, not bulk transfer.
const RELAY_MAX_MSG: usize = RELAY_CHUNK * RELAY_MAX_FRAGS;
/// Max RELAY frame length on the wire (header + full chunk); sizes stack buffers.
const RELAY_FRAME_MAX: usize = 27 + RELAY_CHUNK;
/// Max RELAYACK frame length ("SMOLv1 RELAYACK " + "MMMMM BBB" = 25); rounded up.
const RELAYACK_FRAME_MAX: usize = 32;
/// Concurrent (src_mac, msgid) reassemblies a gateway tracks. Bounded table.
const REASSEMBLY_SLOTS: usize = 3;
/// Completed messages a gateway buffers between flushes. Bounded queue.
const GATEWAY_QUEUE: usize = 4;
/// Leaf retransmit ceiling per message (telemetry is loss-tolerant).
const RELAY_MAX_TRIES: u8 = 3;
/// Drop a partial reassembly whose newest fragment is older than this.
const RELAY_STALE_MS: u64 = 10_000;
/// Leaf re-emits fresh telemetry this often.
const RELAY_EMIT_INTERVAL_MS: u64 = 15_000;
/// Leaf waits this long for a fuller RELAYACK before retransmitting the gaps.
const RELAY_RETX_MS: u64 = 2_000;
/// Gateway flushes its queue this often (if non-empty), or at once when full.
const RELAY_FLUSH_INTERVAL_MS: u64 = 30_000;

/// Low `count` bits set — the "all fragments received" mask. `count` is validated
/// 1..=RELAY_MAX_FRAGS (<= 8) before this is used, so the shift never overflows.
#[inline]
fn frag_mask(count: u8) -> u8 {
    if count >= 8 {
        0xFF
    } else {
        (1u8 << count) - 1
    }
}

/// True once every fragment `0..count` of a message has been received.
#[inline]
fn all_received(got: u8, count: u8) -> bool {
    got & frag_mask(count) == frag_mask(count)
}

/// One in-progress gateway reassembly, keyed by (src_mac, msgid). `Copy` only so
/// the fixed-size table can be array-initialised in a `const fn`.
#[derive(Clone, Copy)]
struct ReasmSlot {
    used: bool,
    src_mac: [u8; 6],
    src_id: u8,
    msgid: u16,
    count: u8,
    got: u8,          // received-fragment bitmap
    total_len: usize, // set once the FINAL fragment arrives; 0 until then
    last_ms: u64,     // newest-fragment time, for staleness eviction
    buf: [u8; RELAY_MAX_MSG],
}

impl ReasmSlot {
    const fn new() -> Self {
        Self {
            used: false,
            src_mac: [0; 6],
            src_id: 0,
            msgid: 0,
            count: 0,
            got: 0,
            total_len: 0,
            last_ms: 0,
            buf: [0; RELAY_MAX_MSG],
        }
    }
}

/// One completed message buffered for the next uplink flush.
#[derive(Clone, Copy)]
struct GwMsg {
    used: bool,
    src_id: u8,
    len: usize,
    buf: [u8; RELAY_MAX_MSG],
}

impl GwMsg {
    const fn new() -> Self {
        Self {
            used: false,
            src_id: 0,
            len: 0,
            buf: [0; RELAY_MAX_MSG],
        }
    }
}

/// A leaf's single outstanding outbound message (its own telemetry), retained so
/// it can retransmit whichever fragments the gateway's RELAYACK still shows as
/// missing. One at a time — a fresh emit supersedes the previous.
#[derive(Clone, Copy)]
struct RelayTx {
    active: bool,
    msgid: u16,
    count: u8,
    acked: u8, // fragments the gateway has confirmed (from RELAYACK)
    tries: u8,
    total_len: usize,
    last_ms: u64,
    buf: [u8; RELAY_MAX_MSG],
}

impl RelayTx {
    const fn new() -> Self {
        Self {
            active: false,
            msgid: 0,
            count: 0,
            acked: 0,
            tries: 0,
            total_len: 0,
            last_ms: 0,
            buf: [0; RELAY_MAX_MSG],
        }
    }
}

/// All relay-bridge state. A node is a leaf OR a gateway (decided at boot from
/// whether the NTP burst associated); it carries both roles' fixed-capacity state
/// but only exercises one. Nothing here grows on the heap.
struct Relay {
    is_gateway: bool,
    // --- leaf (uplink source) ---
    next_msgid: u16,
    tx: RelayTx,
    last_emit_ms: u64,
    // --- gateway (reassemble + buffer + flush) ---
    reasm: [ReasmSlot; REASSEMBLY_SLOTS],
    queue: [GwMsg; GATEWAY_QUEUE],
    last_flush_ms: u64,
}

impl Relay {
    const fn new() -> Self {
        Self {
            is_gateway: false,
            next_msgid: 0,
            tx: RelayTx::new(),
            last_emit_ms: 0,
            reasm: [ReasmSlot::new(); REASSEMBLY_SLOTS],
            queue: [GwMsg::new(); GATEWAY_QUEUE],
            last_flush_ms: 0,
        }
    }

    /// Gateway: accept one inbound fragment; returns (current bitmap, complete).
    /// `chunk` is copied in immediately (no borrow kept). On completion the
    /// assembled message is moved into the flush queue and the slot is freed.
    fn accept(
        &mut self,
        src_mac: [u8; 6],
        hdr: (u8, u16, u8, u8),
        chunk: &[u8],
        now: u64,
    ) -> (u8, bool) {
        // `hdr` = (src_id, msgid, frag, count) straight off the RELAY frame —
        // bundled into a tuple to keep the argument count reasonable.
        let (src_id, msgid, frag, count) = hdr;
        // Reject anything outside our caps; a bad frame just reads as "nothing".
        if count == 0
            || count as usize > RELAY_MAX_FRAGS
            || frag >= count
            || chunk.len() > RELAY_CHUNK
        {
            return (0, false);
        }
        let Some(idx) = self.slot_for(&src_mac, msgid, now) else {
            // Table full of fresh (non-stale) other messages: drop (bounded).
            return (0, false);
        };
        {
            let s = &mut self.reasm[idx];
            s.src_id = src_id;
            s.count = count;
            let off = frag as usize * RELAY_CHUNK;
            let end = off + chunk.len();
            s.buf[off..end].copy_from_slice(chunk);
            s.got |= 1u8 << frag;
            if frag == count - 1 {
                s.total_len = end; // the final fragment fixes the message length
            }
            s.last_ms = now;
        }
        let (got, complete, total_len) = {
            let s = &self.reasm[idx];
            (s.got, all_received(s.got, s.count), s.total_len)
        };
        if complete {
            self.enqueue(idx, total_len);
            self.reasm[idx].used = false; // free the slot
        }
        (got, complete)
    }

    /// Find the slot for (src_mac, msgid): an existing match, else a free slot,
    /// else the stalest slot to evict. `None` only if every slot holds a fresh
    /// (non-stale) different message — then the fragment is dropped.
    fn slot_for(&mut self, src_mac: &[u8; 6], msgid: u16, now: u64) -> Option<usize> {
        for (i, s) in self.reasm.iter().enumerate() {
            if s.used && s.msgid == msgid && &s.src_mac == src_mac {
                return Some(i);
            }
        }
        let mut victim: Option<usize> = None;
        let mut oldest = u64::MAX;
        for (i, s) in self.reasm.iter().enumerate() {
            if !s.used {
                victim = Some(i);
                break;
            }
            if now.saturating_sub(s.last_ms) >= RELAY_STALE_MS && s.last_ms < oldest {
                oldest = s.last_ms;
                victim = Some(i);
            }
        }
        let i = victim?;
        self.reasm[i] = ReasmSlot::new();
        self.reasm[i].used = true;
        self.reasm[i].src_mac = *src_mac;
        self.reasm[i].msgid = msgid;
        Some(i)
    }

    /// Move a completed reassembly into a free flush-queue slot (drop if full —
    /// bounded, loss-tolerant). Copies the buffer out first to avoid aliasing the
    /// reasm and queue arrays at once.
    fn enqueue(&mut self, reasm_idx: usize, total_len: usize) {
        let len = total_len.min(RELAY_MAX_MSG);
        let src_id = self.reasm[reasm_idx].src_id;
        let src_buf = self.reasm[reasm_idx].buf; // [u8; RELAY_MAX_MSG] is Copy
        let Some(qi) = self.queue.iter().position(|q| !q.used) else {
            log::warn!("smol: relay queue full; dropping a reassembled telemetry msg");
            return;
        };
        let q = &mut self.queue[qi];
        q.used = true;
        q.src_id = src_id;
        q.len = len;
        q.buf[..len].copy_from_slice(&src_buf[..len]);
    }

    /// Leaf: stage a fresh telemetry message for (fragmented) broadcast; returns
    /// the fragment count to send (0 if empty). Truncates to RELAY_MAX_MSG.
    fn stage_tx(&mut self, telemetry: &[u8], now: u64) -> u8 {
        let len = telemetry.len().min(RELAY_MAX_MSG);
        if len == 0 {
            return 0;
        }
        let count = len.div_ceil(RELAY_CHUNK) as u8; // 1..=RELAY_MAX_FRAGS
        self.tx = RelayTx::new();
        self.tx.active = true;
        self.tx.msgid = self.next_msgid;
        self.next_msgid = self.next_msgid.wrapping_add(1);
        self.tx.count = count;
        self.tx.total_len = len;
        self.tx.tries = 1;
        self.tx.last_ms = now;
        self.tx.buf[..len].copy_from_slice(&telemetry[..len]);
        count
    }

    /// Leaf: fold in a gateway's cumulative RELAYACK bitmap; deactivate once every
    /// fragment is confirmed. No-op unless it matches our outstanding message.
    fn apply_ack(&mut self, msgid: u16, bitmap: u8) {
        if self.tx.active && self.tx.msgid == msgid {
            self.tx.acked |= bitmap;
            if all_received(self.tx.acked, self.tx.count) {
                self.tx.active = false;
            }
        }
    }
}

/// Which stack the single radio is currently servicing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Associated to an AP; WiFi (NTP/weather) available. If ESP-NOW is used in
    /// this mode it is pinned to the AP's channel (COEXIST strategy).
    WifiSta,
    /// WiFi association dropped; radio pinned to `ESP_NOW_FIXED_CHANNEL` for
    /// deterministic peer messaging (TIME-SHARE strategy).
    EspNow,
}

/// Owns the one radio for its whole lifetime and exposes the ESP-NOW handle.
///
/// The `EspWifiController` and `WifiController` are kept alive for `'static`
/// (leaked once at construction) so the radio stays initialised for the life
/// of the program — we never re-run `esp_wifi::init`, which is both expensive
/// and, per esp-wifi's docs, must happen exactly once.
pub struct RadioManager {
    controller: WifiController<'static>,
    esp_now: EspNow<'static>,
    /// The WiFi STA device, handed out once by esp-wifi's `Interfaces`. Held as
    /// an `Option` and BORROWED (never dropped) so it survives the boot NTP burst
    /// and is available again for periodic relay flushes (`flush_telemetry`). The
    /// smoltcp stack is built/dropped inside each burst, so between bursts this is
    /// just an idle handle that doesn't contend with ESP-NOW on ch6.
    sta: Option<esp_wifi::wifi::WifiDevice<'static>>,
    /// Kept for the SNTP ephemeral-port seed during the burst.
    rng: Rng,
    mode: Mode,
    /// Our short device id, embedded in HELLO beacons and matched against the
    /// id carried by inbound ACKs to detect a bidirectional link.
    id: u8,
    /// Handshake state driving the blue LED (see the protocol comment above).
    peers: PeerTracker,
    /// BENCH link statistics (only exercised while in BENCH mode).
    bench: BenchTracker,
    /// Freshest pending mesh time offer (see the mesh-time section above).
    time: TimeTracker,
    /// Relay-bridge state (leaf uplink + gateway reassembly/flush; bounded).
    relay: Relay,
    /// Decoded MMO-snake frames buffered for `main` to drain (issue #5).
    snk: SnkInbox,
}

/// Monotonic milliseconds since boot — the single time base for both the
/// handshake staleness checks and the LED blink phase.
#[inline]
pub fn now_ms() -> u64 {
    Instant::now().duration_since_epoch().as_millis()
}

impl RadioManager {
    /// Initialise the radio once. Starts in `WifiSta` mode so the caller can do
    /// an NTP burst before switching to ESP-NOW.
    pub fn new(p: WifiPeripherals, id: u8) -> Option<Self> {
        // esp-wifi needs a heap; use the single shared region (see net::init_heap).
        super::init_heap();

        let timg0 = TimerGroup::new(p.timg0);
        let rng = Rng::new(p.rng);
        // esp-wifi's `init` takes the RNG by value; `Rng` is a `Copy` handle
        // (not the entropy itself), so we keep our own copy for the SNTP
        // ephemeral-port seed while also handing one to `init`.
        let ctrl: EspWifiController<'static> = esp_wifi::init(timg0.timer0, rng).ok()?;
        let ctrl: &'static EspWifiController<'static> =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(ctrl));

        let (mut controller, interfaces) = esp_wifi::wifi::new(ctrl, p.wifi).ok()?;
        controller.set_mode(WifiMode::Sta).ok()?;
        controller.start().ok()?;

        Some(Self {
            controller,
            esp_now: interfaces.esp_now,
            // Keep the STA device alive for the NTP burst; dropped afterward.
            sta: Some(interfaces.sta),
            rng,
            mode: Mode::WifiSta,
            id,
            peers: PeerTracker::new(),
            bench: BenchTracker::new(),
            time: TimeTracker::new(),
            relay: Relay::new(),
            snk: SnkInbox::new(),
        })
    }

    /// Run the real WiFi -> DHCP -> SNTP burst using the STA device, driving the
    /// caller's `tick` closure throughout (the `espnow` build fast-blinks the
    /// blue LED so "WiFi/NTP in progress" is visible). Returns the synced Unix
    /// time, or `None` on any timeout.
    ///
    /// We now BORROW the STA device instead of `take()`+drop: keeping it alive
    /// lets a gateway re-associate for periodic relay flushes (`flush_telemetry`,
    /// which resurrects the `switch(Mode::WifiSta)` arm). The smoltcp interface is
    /// built and dropped INSIDE `run_ntp_burst`, so no live stack lingers to
    /// contend with ESP-NOW between bursts.
    pub fn burst_ntp(&mut self, tick: &mut dyn FnMut()) -> Option<u32> {
        // Disjoint field borrows: &mut self.controller, &mut *sta, Copy of rng.
        let sta = self.sta.as_mut()?;
        crate::net::wifi::run_ntp_burst(&mut self.controller, sta, self.rng, tick)
    }

    /// Current radio mode. Part of the public API (a caller may inspect which
    /// stack is live before choosing to broadcast); not used by `main` today.
    #[allow(dead_code)]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Switch which stack the single radio services.
    ///
    /// * `Mode::WifiSta`  -> COEXIST: keep/So resume WiFi. ESP-NOW, if used,
    ///   rides the AP's current channel (we read it and pin ESP-NOW to it).
    /// * `Mode::EspNow`   -> TIME-SHARE: drop the WiFi association to free the
    ///   air, then pin the PHY to `ESP_NOW_FIXED_CHANNEL`.
    pub fn switch(&mut self, mode: Mode) -> Result<(), ()> {
        if self.mode == mode {
            return Ok(());
        }
        match mode {
            Mode::EspNow => {
                // TIME-SHARE: relinquish the AP association so nothing else
                // steers the channel, then pin the fixed ESP-NOW channel.
                let _ = self.controller.disconnect();
                // Keep the MAC/PHY powered (do NOT stop the controller) so the
                // esp_now handle stays valid; just retune the channel.
                self.esp_now
                    .set_channel(ESP_NOW_FIXED_CHANNEL)
                    .map_err(|_| ())?;
                log::info!(
                    "smol: radio -> ESP-NOW (time-share) on ch {}",
                    ESP_NOW_FIXED_CHANNEL
                );
            }
            Mode::WifiSta => {
                // COEXIST: come back to the AP. Re-associating retunes the PHY
                // to the AP's channel automatically; ESP-NOW then coexists on
                // that channel. (Caller must have valid credentials set.)
                let _ = self.controller.connect();
                log::info!("smol: radio -> WiFi STA (coexist)");
            }
        }
        self.mode = mode;
        Ok(())
    }

    /// Broadcast one HELLO beacon carrying our id: `"SMOLv1 HELLO NNN"`.
    ///
    /// This is the periodic "I'm here" advertisement other units listen for.
    /// Safe in either radio mode; in `WifiSta` it rides the AP channel, in
    /// `EspNow` the fixed one. Called by `main` a few times per second.
    pub fn broadcast_hello(&mut self) {
        let mut msg = [0u8; 16];
        let len = encode_id_frame(HELLO_PREFIX, self.id, &mut msg);
        self.send_to(&BROADCAST_ADDRESS, &msg[..len]);
    }

    /// Broadcast one BENCH BEACON carrying our id, our next seq, and the highest
    /// peer seq we've heard (the echo). Called by BENCH mode a few times/sec IN
    /// ADDITION to the normal HELLO; unused in the other modes. Records the send
    /// time so a peer echoing this seq back lets us measure RTT.
    pub fn broadcast_beacon(&mut self) {
        let seq = self.bench.tx_seq;
        let echo = self.bench.peer_last_seq.unwrap_or(0);
        // Remember when we sent this seq (for RTT when it's echoed back). Use a
        // non-zero clamp so a genuine "sent at t=0" still reads as "used".
        let now = now_ms();
        self.bench.send_time[(seq as usize) & (RTT_RING - 1)] = now.max(1);
        self.bench.tx_seq = self.bench.tx_seq.wrapping_add(1);
        self.bench.tx_count = self.bench.tx_count.wrapping_add(1);

        let mut msg = [0u8; 32];
        let len = encode_beacon(self.id, seq, echo, &mut msg);
        self.send_to(&BROADCAST_ADDRESS, &msg[..len]);
    }

    // --- Mesh time sync (see the TimeTracker section + main::should_adopt) ---

    /// Broadcast one TIME frame: our current Unix-time estimate + the `synced_at`
    /// it descends from. `main` calls this on the SAME ~2 s tick as HELLO so a
    /// peer whose sync is older can adopt ours. Fixed-width ASCII like BEACON;
    /// safe in either radio mode (rides whichever channel is currently tuned).
    pub fn broadcast_time(&mut self, unix: u32, synced_at: u32) {
        // 37 bytes on the wire; 40 leaves the same small headroom BEACON uses.
        let mut msg = [0u8; 40];
        let len = encode_time(self.id, unix, synced_at, &mut msg);
        self.send_to(&BROADCAST_ADDRESS, &msg[..len]);
    }

    /// Broadcast one MMO-snake state frame (issue #5). 18 B; `main` calls this
    /// on the per-id phase-jittered 200 ms schedule while MeshSnake is active.
    pub fn broadcast_snk(&mut self, f: &SnkFrame) {
        let mut msg = [0u8; 24];
        if let Some(len) = snake_core::encode_snk(f, &mut msg) {
            self.send_to(&BROADCAST_ADDRESS, &msg[..len]);
        }
    }

    /// Drain one buffered MMO-snake frame (oldest first), or `None` if empty.
    /// `main` loops this each subtick into `MeshSnake::ingest`.
    pub fn take_snk(&mut self) -> Option<SnkFrame> {
        self.snk.pop()
    }

    /// Take the freshest buffered peer TIME offer, clearing it so a later call
    /// only sees offers that arrive afterward. Returns `(peer_unix,
    /// peer_synced_at)`; `main` decides via `should_adopt` whether to re-anchor.
    pub fn take_time_offer(&mut self) -> Option<(u32, u32)> {
        if self.time.have {
            self.time.have = false;
            Some((self.time.best_unix, self.time.best_synced_at))
        } else {
            None
        }
    }

    // --- Relay bridge (see the "Relay bridge" section) -----------------------

    /// Leaf only: is it time to emit a fresh telemetry message? Always false on a
    /// gateway (gateways relay leaves' telemetry, they don't originate RELAY).
    pub fn relay_emit_due(&self, now: u64) -> bool {
        !self.relay.is_gateway
            && (self.relay.last_emit_ms == 0
                || now.saturating_sub(self.relay.last_emit_ms) >= RELAY_EMIT_INTERVAL_MS)
    }

    /// Leaf only: fragment `telemetry` into RELAY frames and BROADCAST them all,
    /// staging the message for bounded retransmit. No-op on a gateway.
    pub fn relay_emit(&mut self, telemetry: &[u8], now: u64) {
        if self.relay.is_gateway {
            return;
        }
        let count = self.relay.stage_tx(telemetry, now);
        if count == 0 {
            return;
        }
        self.relay.last_emit_ms = now;
        let (msgid, total_len) = (self.relay.tx.msgid, self.relay.tx.total_len);
        for frag in 0..count {
            let off = frag as usize * RELAY_CHUNK;
            let end = (off + RELAY_CHUNK).min(total_len);
            // Encode into a LOCAL frame buffer (copying the chunk out of tx.buf)
            // BEFORE send_to, so no borrow of self.relay is held across the
            // &mut-self send call.
            let mut fb = [0u8; RELAY_FRAME_MAX];
            let len = encode_relay(self.id, msgid, frag, count, &self.relay.tx.buf[off..end], &mut fb);
            self.send_to(&BROADCAST_ADDRESS, &fb[..len]);
        }
        log::info!("smol: relay emit msgid {} ({} frag)", msgid, count);
    }

    /// Leaf only: retransmit the fragments still unacked, bounded to
    /// RELAY_MAX_TRIES. No-op once fully acked, out of tries, too soon since the
    /// last send, or on a gateway.
    pub fn relay_retransmit(&mut self, now: u64) {
        if self.relay.is_gateway || !self.relay.tx.active {
            return;
        }
        if all_received(self.relay.tx.acked, self.relay.tx.count) {
            self.relay.tx.active = false;
            return;
        }
        if self.relay.tx.tries >= RELAY_MAX_TRIES {
            self.relay.tx.active = false; // give up — telemetry is loss-tolerant
            return;
        }
        if now.saturating_sub(self.relay.tx.last_ms) < RELAY_RETX_MS {
            return; // give the gateway time to ACK before resending
        }
        let (msgid, count, acked, total_len) = (
            self.relay.tx.msgid,
            self.relay.tx.count,
            self.relay.tx.acked,
            self.relay.tx.total_len,
        );
        for frag in 0..count {
            if acked & (1u8 << frag) != 0 {
                continue; // already confirmed
            }
            let off = frag as usize * RELAY_CHUNK;
            let end = (off + RELAY_CHUNK).min(total_len);
            let mut fb = [0u8; RELAY_FRAME_MAX];
            let len = encode_relay(self.id, msgid, frag, count, &self.relay.tx.buf[off..end], &mut fb);
            self.send_to(&BROADCAST_ADDRESS, &fb[..len]);
        }
        self.relay.tx.tries += 1;
        self.relay.tx.last_ms = now;
    }

    /// Gateway only: are there buffered messages due for a flush burst (queue full,
    /// or the flush interval has elapsed with a non-empty queue)?
    pub fn relay_ready_to_flush(&self, now: u64) -> bool {
        if !self.relay.is_gateway {
            return false;
        }
        let pending = self.relay.queue.iter().filter(|q| q.used).count();
        if pending == 0 {
            return false;
        }
        pending >= GATEWAY_QUEUE
            || self.relay.last_flush_ms == 0
            || now.saturating_sub(self.relay.last_flush_ms) >= RELAY_FLUSH_INTERVAL_MS
    }

    /// Gateway only: run a WiFi burst and UDP each buffered message to the fixed
    /// collector as `"<src_id> <telemetry>"`, then return to ESP-NOW ch6. This
    /// EXERCISES the (formerly dead) `switch(Mode::WifiSta)` arm. The mesh is deaf
    /// on ch6 for the burst's duration (single radio); `tick` fast-blinks the LED
    /// like the boot NTP burst. Clears the queue only on success. Returns success.
    pub fn flush_telemetry(&mut self, tick: &mut dyn FnMut()) -> bool {
        if !self.relay.is_gateway {
            return false;
        }
        log::info!("smol: relay flush -> WiFi burst (mesh deaf on ch6 during it)");
        // Resurrected COEXIST arm: re-associate to the AP (retunes the PHY off
        // ch6). run_udp_flush waits for the association, so we only trigger it.
        let _ = self.switch(Mode::WifiSta);
        let sta = self.sta.as_mut();
        let ok = match sta {
            None => false,
            Some(sta) => {
                // Gather queued messages as (src_id, &payload). Disjoint borrows:
                // &self.relay (via items), &mut self.controller, &mut *sta.
                let empty: &[u8] = &[];
                let mut items: [(u8, &[u8]); GATEWAY_QUEUE] = [(0u8, empty); GATEWAY_QUEUE];
                let mut n = 0;
                for q in self.relay.queue.iter() {
                    if q.used {
                        items[n] = (q.src_id, &q.buf[..q.len]);
                        n += 1;
                    }
                }
                crate::net::wifi::run_udp_flush(
                    &mut self.controller,
                    sta,
                    self.rng,
                    &items[..n],
                    tick,
                )
            }
        };
        // Back to deterministic ESP-NOW ch6 regardless of flush outcome.
        let _ = self.switch(Mode::EspNow);
        if ok {
            for q in self.relay.queue.iter_mut() {
                q.used = false;
            }
            self.relay.last_flush_ms = now_ms();
            log::info!("smol: relay flush done");
        }
        ok
    }

    /// Snapshot the current BENCH link stats for the UI. `fps` is measured by the
    /// render loop (this module can't see frame timing), so the caller passes it
    /// in and it is not part of this snapshot. Also refreshes the per-second
    /// TX/RX rate windows off `now_ms`.
    pub fn bench_stats(&mut self, now_ms: u64) -> BenchStats {
        self.bench.tick_rates(now_ms);
        BenchStats {
            tx_per_s: self.bench.tx_per_s,
            rx_per_s: self.bench.rx_per_s,
            rtt_ms: self.bench.last_rtt_ms,
            loss_pct: self.bench.loss_pct(),
            rssi: self.bench.last_rssi,
            link: self.peers.state(now_ms),
        }
    }

    /// Low-level send helper: fire one frame and wait for the TX callback so we
    /// don't overrun the single in-flight ESP-NOW send slot.
    fn send_to(&mut self, dst: &[u8; 6], data: &[u8]) {
        match self.esp_now.send(dst, data) {
            Ok(waiter) => {
                let _ = waiter.wait();
            }
            Err(e) => log::warn!("smol: esp-now send failed: {:?}", e),
        }
    }

    /// Service inbound ESP-NOW traffic and advance the handshake.
    ///
    /// Drains up to a few queued frames (bounded so we never block the render
    /// loop), and for each recognised HELLO/ACK updates the [`PeerTracker`]
    /// timestamps. On hearing a peer HELLO it also (a) registers that peer so we
    /// can unicast back, and (b) replies with an ACK echoing the peer's id —
    /// this is the reply that lets the *other* unit reach the Connected state.
    ///
    /// Returns an optional short display string for the OLED bottom line (the
    /// most recent recognised frame), so the clock UI can show peer activity.
    pub fn service(&mut self) -> Option<alloc::string::String> {
        // Bound the drain: ESP-NOW's RX queue is 10 deep. The MMO-snake game
        // (netcode §2) bursts up to ~16 peers/round + background; raise the
        // per-call drain to 24 so a full burst is absorbed without dropping,
        // while staying BOUNDED so a pathological flood can't stall the 1 Hz
        // clock tick or the LED. Each parse is a cheap prefix match.
        let mut label: Option<alloc::string::String> = None;
        for _ in 0..24 {
            let Some(recv) = self.esp_now.receive() else {
                break;
            };
            let src = recv.info.src_address;
            // RSSI of this frame (dBm) from the ESP-NOW RX control info; used by
            // BENCH. Captured up front so each arm can record it if relevant.
            let rssi = recv.info.rx_control.rssi;
            let now = now_ms();

            match parse_frame(recv.data()) {
                Some(Frame::Snk(f)) => {
                    // An MMO-snake frame proves the peer is audible → counts
                    // toward the LED "detected" state exactly like HELLO/BEACON.
                    self.peers.last_hello_ms = now;
                    // Register the peer so any future unicast can reach it.
                    if !self.esp_now.peer_exists(&src) {
                        let _ = self.esp_now.add_peer(PeerInfo {
                            interface: EspNowWifiInterface::Sta,
                            peer_address: src,
                            lmk: None,
                            channel: None,
                            encrypt: false,
                        });
                    }
                    // Buffer for `main` to drain into the game PeerTable; do NOT
                    // set `label` — the MeshSnake screen owns its own render.
                    self.snk.push(f);
                }
                Some(Frame::Hello(peer_id)) => {
                    // We can hear a peer -> at least "detected".
                    self.peers.last_hello_ms = now;

                    // Register the broadcaster so the ACK below can be unicast.
                    if !self.esp_now.peer_exists(&src) {
                        let _ = self.esp_now.add_peer(PeerInfo {
                            interface: EspNowWifiInterface::Sta,
                            peer_address: src,
                            lmk: None,
                            channel: None,
                            encrypt: false,
                        });
                    }

                    // Reply "I heard you, <peer_id>" so the peer can confirm the
                    // link is two-way from its side.
                    let mut ack = [0u8; 16];
                    let len = encode_id_frame(ACK_PREFIX, peer_id, &mut ack);
                    self.send_to(&src, &ack[..len]);

                    // Show the PEER's magical noun, derived LOCALLY from the id in
                    // the frame (names never travel on the wire). Bare noun (no
                    // "peer " prefix) — ≤ 8 chars for fantasy, always fits the
                    // 72 px OLED line, and the blue LED already signals "a peer".
                    let noun = crate::net::names::name_for_id(peer_id).1;
                    label = Some(alloc::string::String::from(noun));
                }
                Some(Frame::Ack(acked_id)) => {
                    // An ACK addressed to US proves a peer received our HELLO ->
                    // the link is bidirectional -> "connected".
                    if acked_id == self.id {
                        self.peers.last_ack_for_us_ms = now;
                        label = Some(alloc::string::String::from("linked"));
                    }
                    // ACKs for other ids are peer-to-peer chatter; ignore.
                }
                Some(Frame::Beacon { seq, echo }) => {
                    // A peer BENCH beacon. Update RX count, RSSI, loss (seq
                    // gaps), and RTT (if the echo matches a seq we recently
                    // sent). A BEACON also proves we can hear the peer, so it
                    // counts toward the LED "detected" state like a HELLO.
                    self.peers.last_hello_ms = now;
                    self.bench.rx_count = self.bench.rx_count.wrapping_add(1);
                    self.bench.last_rssi = Some(rssi);

                    // Packet loss from gaps in the peer's seq stream: if the new
                    // seq is more than +1 beyond the last, the in-between seqs
                    // were lost. (Only counts forward jumps; reordering/wrap is
                    // treated as no loss to avoid huge spurious counts.)
                    if let Some(prev) = self.bench.peer_last_seq {
                        if seq > prev {
                            self.bench.lost_count = self
                                .bench
                                .lost_count
                                .wrapping_add(seq - prev - 1);
                        }
                    }
                    // Track the highest peer seq (what our own beacons echo).
                    if self.bench.peer_last_seq.is_none_or(|p| seq > p) {
                        self.bench.peer_last_seq = Some(seq);
                    }

                    // RTT: the peer echoed `echo` = a seq WE sent. If that slot
                    // still holds its send time, RTT = now − send_time.
                    let slot = (echo as usize) & (RTT_RING - 1);
                    let sent = self.bench.send_time[slot];
                    if sent != 0 && now >= sent {
                        self.bench.last_rtt_ms = Some((now - sent) as u32);
                    }

                    // Register the peer so future unicast (if any) can reach it.
                    if !self.esp_now.peer_exists(&src) {
                        let _ = self.esp_now.add_peer(PeerInfo {
                            interface: EspNowWifiInterface::Sta,
                            peer_address: src,
                            lmk: None,
                            channel: None,
                            encrypt: false,
                        });
                    }
                    label = Some(alloc::format!("bench seq {}", seq));
                }
                Some(Frame::Time { unix, synced_at }) => {
                    // Buffer the FRESHEST offer (highest synced_at) seen since
                    // `main` last took one, so a burst of frames collapses to the
                    // single best candidate. `main` owns the adopt decision + the
                    // clock re-anchor (see main::should_adopt); we only surface
                    // the offer here — this module never touches the clock.
                    if !self.time.have || synced_at > self.time.best_synced_at {
                        self.time.best_unix = unix;
                        self.time.best_synced_at = synced_at;
                        self.time.have = true;
                    }
                    // Hearing a TIME frame also proves the peer is audible, so it
                    // counts toward the LED "detected" state exactly like a HELLO
                    // or a BEACON does.
                    self.peers.last_hello_ms = now;
                    label = Some(alloc::format!("time {}", synced_at));
                }
                Some(Frame::Relay { src_id, msgid, frag, count, chunk }) => {
                    // A RELAY fragment proves we can hear the peer (LED detected).
                    self.peers.last_hello_ms = now;
                    // Only a GATEWAY reassembles + acks; a leaf ignores RELAY so
                    // work + memory stay with the role that needs them.
                    if self.relay.is_gateway {
                        let (bitmap, complete) =
                            self.relay.accept(src, (src_id, msgid, frag, count), chunk, now);
                        // Register the source so the RELAYACK can be unicast back
                        // (same pattern as the HELLO -> ACK reply).
                        if !self.esp_now.peer_exists(&src) {
                            let _ = self.esp_now.add_peer(PeerInfo {
                                interface: EspNowWifiInterface::Sta,
                                peer_address: src,
                                lmk: None,
                                channel: None,
                                encrypt: false,
                            });
                        }
                        let mut ack = [0u8; RELAYACK_FRAME_MAX];
                        let len = encode_relayack(msgid, bitmap, &mut ack);
                        self.send_to(&src, &ack[..len]);
                        label = Some(if complete {
                            alloc::format!("relay {:03} ok", src_id)
                        } else {
                            alloc::format!("relay {:03} {}/{}", src_id, bitmap.count_ones(), count)
                        });
                    }
                }
                Some(Frame::RelayAck { msgid, bitmap }) => {
                    // Leaf: the gateway confirmed these fragments — stop resending
                    // them. On a gateway (no outstanding tx) this is a no-op.
                    self.relay.apply_ack(msgid, bitmap);
                    label = Some(alloc::format!("ack {:05}", msgid));
                }
                None => {
                    // Unrecognised payload (other ESP-NOW traffic on-channel);
                    // surface it on the OLED but don't touch the handshake.
                    label = Some(alloc::string::String::from_utf8_lossy(recv.data()).into_owned());
                }
            }
        }
        label
    }

    /// Current peer-link state as an [`LedState`], evaluated at `now_ms`.
    /// One of `Idle` / `PeerDetected` / `Connected` (never `WifiSync` — that is
    /// owned by the boot-time WiFi burst, not the steady-state loop).
    pub fn peer_led_state(&self, now_ms: u64) -> LedState {
        self.peers.state(now_ms)
    }
}

/// Encode `"<prefix>NNN"` (3-digit zero-padded id) into `out`; returns length.
fn encode_id_frame(prefix: &[u8], id: u8, out: &mut [u8]) -> usize {
    out[..prefix.len()].copy_from_slice(prefix);
    out[prefix.len()] = b'0' + (id / 100) % 10;
    out[prefix.len() + 1] = b'0' + (id / 10) % 10;
    out[prefix.len() + 2] = b'0' + id % 10;
    prefix.len() + 3
}

/// Encode a BENCH beacon `"SMOLv1 BEACON NNN SSSSS EEEEE"` into `out`; returns
/// length. `seq`/`echo` are rendered as 5-digit zero-padded decimals (mod
/// 100000) — plenty of range for a link test and keeps the frame fixed-width.
fn encode_beacon(id: u8, seq: u32, echo: u32, out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..BEACON_PREFIX.len()].copy_from_slice(BEACON_PREFIX);
    n += BEACON_PREFIX.len();
    out[n] = b'0' + (id / 100) % 10;
    out[n + 1] = b'0' + (id / 10) % 10;
    out[n + 2] = b'0' + id % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    write_u5(seq, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    write_u5(echo, &mut out[n..]);
    n += 5;
    n
}

/// Write a 5-digit zero-padded decimal (value mod 100000) into `out[..5]`.
fn write_u5(v: u32, out: &mut [u8]) {
    let v = v % 100_000;
    out[0] = b'0' + ((v / 10_000) % 10) as u8;
    out[1] = b'0' + ((v / 1_000) % 10) as u8;
    out[2] = b'0' + ((v / 100) % 10) as u8;
    out[3] = b'0' + ((v / 10) % 10) as u8;
    out[4] = b'0' + (v % 10) as u8;
}

/// Encode a TIME frame `"SMOLv1 TIME NNN UUUUUUUUUU SSSSSSSSSS"` into `out`;
/// returns length (37). `unix`/`synced_at` are 10-digit zero-padded decimals —
/// 10 digits spans the whole u32 range, so (unlike [`write_u5`]) no modulo clamp
/// is needed. Pure + fixed-width so a serial sniffer and a host unit test can
/// both read it. Mirrors [`encode_beacon`].
fn encode_time(id: u8, unix: u32, synced_at: u32, out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..TIME_PREFIX.len()].copy_from_slice(TIME_PREFIX);
    n += TIME_PREFIX.len();
    out[n] = b'0' + (id / 100) % 10;
    out[n + 1] = b'0' + (id / 10) % 10;
    out[n + 2] = b'0' + id % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    write_u10(unix, &mut out[n..]);
    n += 10;
    out[n] = b' ';
    n += 1;
    write_u10(synced_at, &mut out[n..]);
    n += 10;
    n
}

/// Write a 10-digit zero-padded decimal into `out[..10]`. 10 digits holds every
/// u32 (max 4_294_967_295), so no clamp is needed — the full value round-trips
/// through [`parse_u10`]. Filled least-significant-digit first.
fn write_u10(mut v: u32, out: &mut [u8]) {
    for i in (0..10).rev() {
        out[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

/// Parse an inbound payload into a [`Frame`], or `None` if it isn't ours.
fn parse_frame(data: &[u8]) -> Option<Frame<'_>> {
    // MMO-snake frames are the hot path in the game; try them first. `parse_snk`
    // validates prefix/length/ver itself and degrades on unknown ver.
    if data.starts_with(snake_core::SNK_PREFIX) {
        return snake_core::parse_snk(data).map(Frame::Snk);
    }
    if let Some(rest) = data.strip_prefix(HELLO_PREFIX) {
        return parse_id(rest).map(Frame::Hello);
    }
    if let Some(rest) = data.strip_prefix(ACK_PREFIX) {
        return parse_id(rest).map(Frame::Ack);
    }
    if let Some(rest) = data.strip_prefix(BEACON_PREFIX) {
        // "NNN SSSSS EEEEE": id (3) space seq (5) space echo (5) = 15 bytes.
        if rest.len() >= 15 {
            let seq = parse_u5(&rest[4..9])?;
            let echo = parse_u5(&rest[10..15])?;
            return Some(Frame::Beacon { seq, echo });
        }
        return None;
    }
    if let Some(rest) = data.strip_prefix(TIME_PREFIX) {
        // "NNN UUUUUUUUUU SSSSSSSSSS": id (3) space unix (10) space
        // synced_at (10) = 25 bytes. The sender id isn't needed (freshness, not
        // identity, drives adoption), so we skip it just as the BEACON arm does.
        if rest.len() >= 25 {
            let unix = parse_u10(&rest[4..14])?;
            let synced_at = parse_u10(&rest[15..25])?;
            return Some(Frame::Time { unix, synced_at });
        }
        return None;
    }
    if let Some((src_id, msgid, frag, count, chunk)) = parse_relay(data) {
        return Some(Frame::Relay { src_id, msgid, frag, count, chunk });
    }
    if let Some((msgid, bitmap)) = parse_relayack(data) {
        return Some(Frame::RelayAck { msgid, bitmap });
    }
    None
}

/// Parse a 3-digit ASCII id (`b"007"` -> 7). Rejects non-digits / short input.
fn parse_id(rest: &[u8]) -> Option<u8> {
    if rest.len() < 3 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in &rest[..3] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u16;
    }
    (val <= 255).then_some(val as u8)
}

/// Parse exactly 5 ASCII digits into a u32. Rejects short/non-digit input.
fn parse_u5(rest: &[u8]) -> Option<u32> {
    if rest.len() < 5 {
        return None;
    }
    let mut val: u32 = 0;
    for &b in &rest[..5] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u32;
    }
    Some(val)
}

/// Parse exactly 10 ASCII digits into a u32. Accumulates in u64 and range-checks
/// on the way out, so a garbled/hostile 10-digit field that exceeds u32::MAX
/// (e.g. "9999999999") is rejected as `None` rather than silently wrapping —
/// stricter than [`parse_u5`], where 5 digits always fit in u32.
fn parse_u10(rest: &[u8]) -> Option<u32> {
    if rest.len() < 10 {
        return None;
    }
    let mut val: u64 = 0;
    for &b in &rest[..10] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u64;
    }
    u32::try_from(val).ok()
}

// --- Relay wire codec (pure + fixed-width; host-unit-testable) --------------

/// Encode a RELAY fragment `"SMOLv1 RELAY NNN MMMMM F C " + <chunk>` into `out`;
/// returns total length (27-byte header + chunk). `NNN` = src id, `MMMMM` = msgid
/// (5-digit u16), `F`/`C` = single-digit frag index / count (count <=
/// RELAY_MAX_FRAGS <= 9). `chunk` is truncated to `RELAY_CHUNK`. Mirrors
/// [`encode_beacon`]'s discipline.
fn encode_relay(src_id: u8, msgid: u16, frag: u8, count: u8, chunk: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..RELAY_PREFIX.len()].copy_from_slice(RELAY_PREFIX);
    n += RELAY_PREFIX.len();
    out[n] = b'0' + (src_id / 100) % 10;
    out[n + 1] = b'0' + (src_id / 10) % 10;
    out[n + 2] = b'0' + src_id % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    write_u5(msgid as u32, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + frag;
    n += 1;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + count;
    n += 1;
    out[n] = b' ';
    n += 1;
    let len = chunk.len().min(RELAY_CHUNK);
    out[n..n + len].copy_from_slice(&chunk[..len]);
    n + len
}

/// Parse a RELAY frame into `(src_id, msgid, frag, count, chunk)`, or `None` if it
/// isn't a well-formed RELAY. `chunk` borrows `data`. The caller
/// ([`Relay::accept`]) re-validates `count`/`frag`/`chunk.len()` against its caps.
fn parse_relay(data: &[u8]) -> Option<(u8, u16, u8, u8, &[u8])> {
    let rest = data.strip_prefix(RELAY_PREFIX)?;
    if rest.len() < 14 {
        return None;
    }
    let src_id = parse_id(&rest[0..3])?;
    let msgid = u16::try_from(parse_u5(&rest[4..9])?).ok()?;
    if !rest[10].is_ascii_digit() || !rest[12].is_ascii_digit() {
        return None;
    }
    let frag = rest[10] - b'0';
    let count = rest[12] - b'0';
    Some((src_id, msgid, frag, count, &rest[14..]))
}

/// Encode a `"SMOLv1 RELAYACK MMMMM BBB"` frame into `out`; returns length (25).
/// `MMMMM` = msgid (5-digit), `BBB` = the 3-digit received-fragment bitmap (0..255).
fn encode_relayack(msgid: u16, bitmap: u8, out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..RELAYACK_PREFIX.len()].copy_from_slice(RELAYACK_PREFIX);
    n += RELAYACK_PREFIX.len();
    write_u5(msgid as u32, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + (bitmap / 100) % 10;
    out[n + 1] = b'0' + (bitmap / 10) % 10;
    out[n + 2] = b'0' + bitmap % 10;
    n += 3;
    n
}

/// Parse a RELAYACK frame into `(msgid, bitmap)`, or `None`. The 3-digit bitmap
/// reuses [`parse_id`] (both are a 3-ASCII-digit `u8` in 0..=255).
fn parse_relayack(data: &[u8]) -> Option<(u16, u8)> {
    let rest = data.strip_prefix(RELAYACK_PREFIX)?;
    if rest.len() < 9 {
        return None;
    }
    let msgid = u16::try_from(parse_u5(&rest[0..5])?).ok()?;
    let bitmap = parse_id(&rest[6..9])?;
    Some((msgid, bitmap))
}

// -------------------------------------------------------------------------
// Public flow used by `main` under `--features espnow`.
// -------------------------------------------------------------------------

/// Bring the radio up, run a REAL WiFi -> DHCP -> SNTP burst (fast-blinking the
/// blue LED throughout), then TIME-SHARE-switch the single radio to ESP-NOW.
///
/// Returns the live `RadioManager` (now in ESP-NOW mode) and the synced Unix
/// time (or `None` if the burst failed — the clock then free-runs). The LED is
/// left in whatever physical state the last fast-blink tick set; `main`'s loop
/// immediately takes over and drives it from the peer state.
///
/// Credentials come from `crate::secrets` (git-ignored; repo is public). The
/// burst genuinely runs DHCP + SNTP against the STA device, which esp-wifi hands
/// out once alongside the ESP-NOW handle — we drive it here, then drop it before
/// pinning the ESP-NOW channel, so the single radio is never double-driven.
pub fn start(p: WifiPeripherals, id: u8, led: &mut Led) -> (Option<RadioManager>, Option<u32>) {
    let Some(mut radio) = RadioManager::new(p, id) else {
        return (None, None);
    };

    // --- WiFi burst for NTP, blue LED fast-blinking (~10 Hz) while it runs ---
    // The closure is called inside every busy-wait loop of the burst; it just
    // re-derives the fast-blink phase from the monotonic clock and pushes it to
    // the pin (non-blocking, no per-blink state).
    let synced = radio.burst_ntp(&mut || led.apply(LedState::WifiSync, now_ms()));

    // Relay role: a node that reached NTP is in WiFi range, so it can be the
    // internet GATEWAY that flushes out-of-range leaves' telemetry; a node that
    // did not is a LEAF (it only emits RELAY). See the relay-bridge section.
    radio.relay.is_gateway = synced.is_some();
    log::info!(
        "smol: relay role = {}",
        if synced.is_some() { "GATEWAY" } else { "leaf" }
    );

    // --- Hand the radio to ESP-NOW on a fixed channel (TIME-SHARE) -------
    let _ = radio.switch(Mode::EspNow);

    (Some(radio), synced)
}
