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

use crate::led::LedState;
use crate::net::WifiPeripherals;

/// Fixed ESP-NOW channel used in TIME-SHARE mode. All smol units must agree on
/// this value (1..=13). 6 is a common, low-congestion default.
#[cfg(not(feature = "coexist-soak"))]
const ESP_NOW_FIXED_CHANNEL: u8 = 6;
/// COEXIST SOAK (#23 PART 1): pin the mesh to the test gateway's AP channel
/// (north-bedroom = ch1) so the leaf (`set_channel`) and the gateway (rides the AP
/// channel via association) agree — the coexist precondition (mesh ch == AP ch).
#[cfg(feature = "coexist-soak")]
const ESP_NOW_FIXED_CHANNEL: u8 = 1;

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
/// Battery-downlink tag: the 12-B `"SMOLv1 BATT "` (trailing space, mirroring TIME)
/// then the VERBATIM `smol/display/batt` payload INCLUDING its `BATT|` marker
/// (e.g. `SMOLv1 BATT BATT|48V 52.8V|HV 391.9V|d 43mV`). NO length byte — the
/// payload is the rest of the frame (len − 12), so frame payload and `BattCache`
/// contents are byte-identical (one memcpy each way; see the pinned byte-layout).
///
/// Only a GATEWAY broadcasts this (the single source, fresh from HA via MQTT);
/// leaves are receive-only and never re-broadcast — unlike TIME, which every node
/// re-broadcasts. BATT carries no freshness field, so leaf re-broadcast could
/// propagate a stale payload or loop; single-hop gateway → neighbour leaves is
/// the safe, intended shape. Same threat model as every SMOLv1 frame (unauthed).
const BATT_PREFIX: &[u8] = b"SMOLv1 BATT "; // + verbatim "BATT|l1|l2|l3"
/// Max BATT payload retained/echoed — matches `BattCache` (LOCKED ≤ 96 B).
const BATT_PAYLOAD_MAX: usize = 96;

/// Grid-downlink tag (issue #16): the exact TWIN of [`BATT_PREFIX`] — the 12-B
/// `"SMOLv1 GRID "` (trailing space) then the VERBATIM `smol/display/grid` payload
/// INCLUDING its `GRID|` marker (e.g. `SMOLv1 GRID GRID|963W|L1 177W|L2 786W`). NO
/// length byte; same gateway-only single-hop rule as BATT (no freshness field, so
/// leaves never re-broadcast). `"SMOLv1 GRID "` diverges from `"SMOLv1 BATT "` at
/// byte 7 (0-indexed), so `strip_prefix` never confuses the two.
const GRID_PREFIX: &[u8] = b"SMOLv1 GRID "; // + verbatim "GRID|l1|l2|l3"
/// Max GRID payload retained/echoed — matches `GridCache` (LOCKED ≤ 96 B).
const GRID_PAYLOAD_MAX: usize = 96;

/// #21/#56 leaf-relay config-downlink tag: `"SMOLv1 CFG "` (11 B, trailing space) then
/// `"NNN"` (3-ASCII zero-padded TARGET leaf id), then a 1-byte config `KEY` (#56), then
/// the VERBATIM value for that channel (empty = clear). Diverges from HELLO/BATT/GRID/
/// TIME/OTA at byte 7 (`'C'`), so `strip_prefix` never confuses them. Single-hop gateway
/// → leaf (leaves never re-broadcast).
///
/// **#56 keyed channel:** ONE relay carries N per-node config types — the KEY dispatches:
/// `S` = default screen (#21; the ONLY channel #56 ships) · `L` = blue-LED mode (#48) ·
/// `U` = display units (#43) · `P` = plugin-enable mask (#55). A leaf applies only the keys
/// in [`CFG_APPLY_KEYS`]; a key its firmware predates is DROPPED (forward-compat, the #46
/// clamp discipline — never mis-applied). Back-compat: a key-less frame (id only, empty
/// tail) is read as an empty-value clear on the SCREEN key, matching the pre-#56 wire.
///
/// **HARD BOUNDARY (oracle R-P3, §0 of the #21 design):** the SCREEN key carries screen
/// config ONLY — never OTA/url/install. It's a low-blast, reversible, escapable command
/// (long-press → Menu is universal), so a forged frame's worst case is a valid screen or an
/// ignore — never code exec, never a brick. Each key's value is bounded by `CFG_VALUE_MAX`;
/// the leaf validates the screen value with the strict/panic-free `parse_default_screen`.
const CFG_PREFIX: &[u8] = b"SMOLv1 CFG "; // + "NNN" + KEY + verbatim "<value>"
/// #56 keyed CFG: the config keys THIS build knows how to APPLY on a leaf. #56 ships only
/// the screen key; #48/#43/#55 each add their letter here alongside a `main` dispatch arm
/// (a `take_cfg_offer(key)` + apply) and a gateway fill site in `mqtt_session`. Sized `[_; N]`
/// (not `&[u8]`) so [`CfgTracker`] can allocate exactly one `.bss` buffer slot per key.
/// An inbound key not listed is dropped at [`CfgTracker::set`] (never buffered/applied).
const CFG_APPLY_KEYS: [u8; 7] = [
    crate::net::wifi::CFG_KEY_SCREEN,
    crate::net::wifi::CFG_KEY_LED,
    crate::net::wifi::CFG_KEY_UNITS,
    crate::net::wifi::CFG_KEY_PLUGINS,
    crate::net::wifi::CFG_KEY_CUSTOM, // #45 custom-screen layout (cached + relayed like S/L/U/P)
    // #52 remote reboot: R IS a buffered/applied key (a leaf takes it via take_cfg_offer(R)) but
    // is NEVER put in cfg_cache/broadcast_cached_configs — the one-shot relay uses broadcast_config
    // directly. The anti-reboot-loop invariant: R rides the CFG wire + apply path, not the cache.
    crate::net::wifi::CFG_KEY_REBOOT,
    // #71 on-demand WiFi scan: W — same COMMAND discipline as R (buffered/applied via take_cfg_offer(W),
    // NEVER cached — a cached scan = periodic off-channel excursion, the coexist hazard). One-shot relay.
    crate::net::wifi::CFG_KEY_SCAN,
];
/// #50b leaf-status UPLINK tag: `"SMOLv1 STAT "` (12 B, trailing space) then `"NNN"`
/// (3-ASCII zero-padded SENDER leaf id) then the verbatim live `<AppKind>:<page>` value
/// (empty = none). Mirror of `CFG` but UPLINK (leaf → gateway): a leaf with no MQTT
/// broadcasts its live render-state; the GATEWAY caches it (`stat_cache`) and republishes
/// it as retained `smol/<id>/status`. Diverges from `SNK` at byte 8 (`'T'` vs `'N'`) so
/// `strip_prefix` never confuses them. Single-hop (the gateway never re-broadcasts a leaf
/// STAT → no flood/loop). Same unauthed, low-blast threat model as CFG: worst case a stray
/// frame yields a bad screen STRING on a status topic — never code exec, self-corrected
/// next cadence. Value bounded by `CFG_VALUE_MAX`.
const STAT_PREFIX: &[u8] = b"SMOLv1 STAT "; // + "NNN" + verbatim "<AppKind>:<page>|<build>"

/// #70/#49 observability UPLINK tag: `"SMOLv1 DIAG "` (12 B, trailing space) then `"NNN"`
/// (3-ASCII zero-padded SENDER id) then the verbatim key=val DIAG record. Exact mirror of
/// `STAT` but a SEPARATE frame (the DIAG value is ~130 B, far past STAT's 16 B `CFG_VALUE_MAX`)
/// with its own gateway cache (`diag_cache`) → retained `smol/<id>/diag`. Diverges from STAT at
/// byte 7 (`'D'` vs `'S'`), so `strip_prefix` never confuses them. Same unauthed, low-blast
/// threat model: worst case a stray frame yields a bad diag string, self-corrected next cadence.
/// Value bounded by `RELAY_VALUE_MAX`.
const DIAG_PREFIX: &[u8] = b"SMOLv1 DIAG "; // + "NNN" + verbatim "up=.. bt=.. rr=.. …"

/// #71 observability UPLINK tag: `"SMOLv1 SCAN "` (12 B, trailing space) then `"NNN"` (SENDER id)
/// then the verbatim one-shot WiFi-scan record. Exact twin of `DIAG` (own gateway cache
/// `scan_cache` → retained `smol/<id>/scan`) but produced ON-DEMAND (a `W` command), never
/// periodically. Diverges from DIAG at byte 9 (`'C'` vs `'I'`) + STAT at byte 8, so `strip_prefix`
/// never confuses them. Value bounded by `RELAY_VALUE_MAX`.
const SCAN_PREFIX: &[u8] = b"SMOLv1 SCAN "; // + "NNN" + verbatim "<ssid>,<bssid3>,<ch>,<rssi>|…"

/// #71: max APs in one scan record (strongest-RSSI first) — bounds the record under
/// `RELAY_VALUE_MAX` and keeps the mesh frame small.
const SCAN_MAX_APS: usize = 5;
/// #71: max SSID chars kept per AP (SSIDs are free-form up to 32 B; truncate for the record).
const SCAN_SSID_MAX: usize = 12;

/// #71: format the strongest scanned APs as a `SCAN`-record value for `smol/<id>/scan`:
/// literal `SCAN` first field, then up to `SCAN_MAX_APS` `|`-separated groups
/// `<ssid>,<bssid-3oct-hex>,<ch>,<rssi>`. SSIDs are truncated to `SCAN_SSID_MAX` and have `|`/`,`
/// stripped (they're free-form → keep the record parseable); BSSIDs are truncated to 3 octets
/// (PUBLIC-repo topic — a full BSSID is a privacy leak). `|none` when the scan found nothing.
/// Panic-free (heap `String`); the caller bounds it to `RELAY_VALUE_MAX` at broadcast/publish.
fn format_scan_record(aps: &[esp_wifi::wifi::AccessPointInfo]) -> alloc::string::String {
    use core::fmt::Write;
    let mut s = alloc::string::String::from("SCAN");
    for ap in aps.iter().take(SCAN_MAX_APS) {
        let mut ssid = alloc::string::String::new();
        for c in ap.ssid.chars().take(SCAN_SSID_MAX) {
            ssid.push(if c == '|' || c == ',' { '_' } else { c });
        }
        let b = ap.bssid;
        let _ = write!(
            s,
            "|{},{:02x}{:02x}{:02x},{},{}",
            ssid, b[0], b[1], b[2], ap.channel, ap.signal_strength
        );
    }
    if aps.is_empty() {
        s.push_str("|none");
    }
    s
}

// #40 leaf-mesh-OTA GATEWAY relay tuning (blocking maintenance op; hardware-tunable — see
// lucid-40-build-handoff.md). The relay monopolizes the radio for its duration by design.
/// Wait for a leaf OTAN after (re)sending a window before resending it (ms).
const GW_OTAN_WAIT_MS: u64 = 800;
/// Max retransmit rounds per window before aborting the relay (R2 spoof/DoS bound).
const GW_WINDOW_ROUNDS_MAX: u32 = 16;
/// Tier-2 confirm window (ms) — MUST exceed the leaf self-test window (`LEAF_SELFTEST_
/// WINDOW_MS` ≈ 60 s) + a STAT cadence (~10 s) so a late self-test rollback is observed
/// before the gateway declares Confirmed.
const GW_CONFIRM_TIMEOUT_MS: u64 = 120_000;
/// #40: max consecutive TRANSIENT (mac-unknown / fetch / relay / timeout) relay attempts
/// for one retained install before the gateway gives up + clears it — bounds the auto-retry
/// so a persistently-failing leaf can't loop the (mesh-deaf) relay every flush forever.
const LEAF_OTA_MAX_RETRIES: u8 = 3;
/// One-window relay readback buffer (off the stack, in `.bss`). Alias-safe: a gateway
/// relays to ONE leaf at a time (serial canary), and never runs a leaf receive session.
static mut GW_OTA_WINDOW: [u8; crate::ota_mesh::WINDOW_BYTES] =
    [0u8; crate::ota_mesh::WINDOW_BYTES];

use crate::mesh_snake::snake_core::{self, SnkFrame};

// #57 Mesh Familiar: the FAM frame codec + the always-on holder/arbitration/
// migration state machine live in `crate::familiar`; RadioManager owns a `FamState`
// beside the roster it elects from (wisp §7 "holder/election = infra"). This module
// only buffers inbound FAM frames + broadcasts, exactly as it does for SNK.
use crate::familiar::{encode_fam, parse_fam, FamFrame, FamState, FAM_FRAME_LEN, FAM_PREFIX};

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

/// #57: depth of the decoded FAM RX ring. FAM is a LOW-rate frame (one holder
/// beats ~1.5 s), so a handful of slots absorbs any realistic burst; like the SNK
/// inbox it DROPS OLDEST on overflow — the familiar's absolute-state + seq
/// arbitration tolerate a dropped beat by design (the next beat re-converges).
const FAM_RX_RING: usize = 4;

/// #57: a tiny FIFO of decoded [`FamFrame`]s (+ their RX RSSI, needed by the
/// orphan-takeover weighting) buffered by `service()` for [`RadioManager::fam_tick`]
/// to drain into the [`FamState`]. Mirrors [`SnkInbox`]: all `Copy`, fixed size →
/// `.bss`, no heap, drop-oldest on overflow.
struct FamInbox {
    buf: [Option<(FamFrame, i32)>; FAM_RX_RING],
    head: usize,
    tail: usize,
    len: usize,
}

impl FamInbox {
    const fn new() -> Self {
        Self { buf: [None; FAM_RX_RING], head: 0, tail: 0, len: 0 }
    }

    /// Push a frame (+ its RSSI); drop the OLDEST if full.
    fn push(&mut self, f: FamFrame, rssi: i32) {
        self.buf[self.head] = Some((f, rssi));
        self.head = (self.head + 1) % FAM_RX_RING;
        if self.len < FAM_RX_RING {
            self.len += 1;
        } else {
            self.tail = (self.tail + 1) % FAM_RX_RING;
        }
    }

    /// Pop the oldest buffered `(frame, rssi)`, or `None` if empty.
    fn pop(&mut self) -> Option<(FamFrame, i32)> {
        if self.len == 0 {
            return None;
        }
        let f = self.buf[self.tail].take();
        self.tail = (self.tail + 1) % FAM_RX_RING;
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
    /// A mesh time offer: the sender's logical `id` (for adoption provenance +
    /// the roster; already on the wire, now retained), its current Unix-time
    /// estimate, plus the `synced_at` that time descends from (the Unix instant
    /// of the peer's last authoritative NTP sync; 0 = never synced). `main`
    /// adopts it iff `synced_at` is strictly newer than ours — see
    /// `main::should_adopt`.
    Time { id: u8, unix: u32, synced_at: u32 },
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
    /// A battery-downlink payload from a gateway: the verbatim `BATT|…` bytes
    /// (`payload` borrows the RX buffer; copied into `BattTracker` in `service`).
    Batt(&'a [u8]),
    /// A grid-downlink payload from a gateway (issue #16): the verbatim `GRID|…`
    /// bytes (twin of `Batt`; copied into `GridTracker` in `service`).
    Grid(&'a [u8]),
    /// #21/#56 leaf-relay: a gateway's keyed CONFIG downlink — `target` leaf id, the config
    /// channel `key` (`S`=screen/…), + the verbatim `value` bytes (`value` borrows the RX
    /// buffer; the leaf buffers it per-key in `CfgTracker` in `service` IFF `target ==
    /// self.id` AND the key is one it applies). Screen config ONLY on the `S` key.
    Cfg { target: u8, key: u8, value: &'a [u8] },
    /// #50b leaf-status uplink: a leaf's live screen readback — `src` sender leaf id +
    /// the verbatim `<AppKind>:<page>` `value` bytes (`value` borrows the RX buffer; the
    /// GATEWAY caches it in `stat_cache` in `service`, keyed by `src`). Twin of `Cfg` but
    /// UPLINK — republished as retained `smol/<src>/status`.
    Stat { src: u8, value: &'a [u8] },
    /// #70/#49 observability uplink: a node's compact key=val DIAG record — `src` sender id +
    /// the verbatim record bytes (borrow the RX buffer; the GATEWAY caches it in `diag_cache`,
    /// keyed by `src`). Twin of `Stat` but a bigger value → retained `smol/<src>/diag`.
    Diag { src: u8, value: &'a [u8] },
    /// #71 observability uplink: a node's one-shot WiFi-scan record. Twin of `Diag` (own cache
    /// `scan_cache`) → retained `smol/<src>/scan`.
    Scan { src: u8, value: &'a [u8] },
    /// #57 Mesh Familiar: a decoded FAM frame (heartbeat / handoff / call). Scalar-only
    /// (no borrow) — buffered into the [`FamInbox`] for [`RadioManager::fam_tick`] to ingest.
    Fam(FamFrame),
}

/// #70/#49 observability: a node's own live diag counters, folded into the retained DIAG record
/// each cadence. `Copy` (lives inline in `RadioManager`). All monotonic-since-boot except the
/// min-heap watermark. Cheap to maintain; no heap, no flash.
#[derive(Clone, Copy)]
struct DiagCounters {
    /// Lowest free-heap reading observed since boot (leak/pressure watermark). `u32::MAX` until
    /// the first sample so `min()` latches the true low-water on the first `diag_sample_heap`.
    heap_min: u32,
    /// BOOT-button SHORT-press count (monotonic, wraps). HA fires a `press` event on each change.
    btn: u16,
    /// BOOT-button LONG-press count (monotonic, wraps). HA fires a `long-press` event on change.
    btnl: u16,
    /// #49 flush ok/fail (GATEWAY): MQTT bursts that reached CONNACK vs failed/timed-out. Proves
    /// the #9 flush-win on hardware (was UART0-only). Leaf-side these stay 0 (a leaf never flushes).
    /// (OTA verify ok/fail is read live from `OtaLeafSession`, not mirrored here.)
    flush_ok: u32,
    flush_fail: u32,
}

impl DiagCounters {
    const fn new() -> Self {
        Self {
            heap_min: u32::MAX,
            btn: 0,
            btnl: 0,
            flush_ok: 0,
            flush_fail: 0,
        }
    }
}

/// #74 obs wave-2: node state that lives in `main` (the LED mode + the clock), pushed into the
/// RadioManager each cadence via `set_diag_extra` so BOTH diag builders (the leaf broadcast + the
/// gateway flush) read one stored copy — `main` owns these, RadioManager only mirrors them for the
/// DIAG record. `Copy`. Folds `led`/`tage`/`tsrc` onto the record (rtt/rx/tx come from `bench`).
#[derive(Clone, Copy)]
struct DiagExtra {
    /// #74 item 6: commanded LED mode wire token ("status"/"on"/"off").
    led_mode: &'static str,
    /// #74 item 6: the LED's actual lit state right now (mode `Status` resolves via link state).
    led_on: bool,
    /// #74 item 8: seconds since the last time-sync (`(now - anchor)/1000`).
    tage_s: u32,
    /// #74 item 8: time source token ("ntp"/"mesh"/"none") — maps `TimeSource`.
    tsrc: &'static str,
}

impl DiagExtra {
    const fn new() -> Self {
        Self { led_mode: "status", led_on: false, tage_s: 0, tsrc: "none" }
    }
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
    /// The logical id of the peer that sent the best offer (for adoption
    /// provenance: the `Adopted(source_id)` shown on Bench's own-status line).
    /// The id is already on the TIME wire; the parser now retains it (no format
    /// change). 0 until an offer is buffered.
    best_id: u8,
    /// Whether an un-taken offer is currently buffered.
    have: bool,
}

impl TimeTracker {
    const fn new() -> Self {
        Self {
            best_unix: 0,
            best_synced_at: 0,
            best_id: 0,
            have: false,
        }
    }
}

/// Buffers the most-recent inbound SMOLv1 BATT payload (verbatim `BATT|…` bytes)
/// until `main` takes it via [`RadioManager::take_batt_offer`] and stores it into
/// its `BattCache`. Mirrors [`TimeTracker`]: `service()` only BUFFERS what arrives;
/// `main` owns the cache write (this module never touches `BattCache`, keeping the
/// clean radio/plugin split). Fixed `.bss` buffer, no heap.
struct BattTracker {
    buf: [u8; BATT_PAYLOAD_MAX],
    len: usize,
    have: bool,
}

impl BattTracker {
    const fn new() -> Self {
        Self { buf: [0; BATT_PAYLOAD_MAX], len: 0, have: false }
    }

    /// Buffer a freshly-received payload (truncated to capacity; ≤ 96 B by spec).
    fn set(&mut self, payload: &[u8]) {
        let n = payload.len().min(BATT_PAYLOAD_MAX);
        self.buf[..n].copy_from_slice(&payload[..n]);
        self.len = n;
        self.have = true;
    }
}

/// A `Copy` snapshot of a buffered BATT payload handed to `main` by
/// [`RadioManager::take_batt_offer`]. `buf[..len]` is the verbatim `BATT|…` bytes.
#[derive(Clone, Copy)]
pub struct BattOffer {
    pub buf: [u8; BATT_PAYLOAD_MAX],
    pub len: usize,
}

/// Buffers the most-recent inbound SMOLv1 GRID payload — the exact TWIN of
/// [`BattTracker`] (issue #16). `service()` only BUFFERS; `main` takes it via
/// [`RadioManager::take_grid_offer`] and writes its `GridCache`. Fixed `.bss`
/// buffer, no heap.
struct GridTracker {
    buf: [u8; GRID_PAYLOAD_MAX],
    len: usize,
    have: bool,
}

impl GridTracker {
    const fn new() -> Self {
        Self { buf: [0; GRID_PAYLOAD_MAX], len: 0, have: false }
    }

    /// Buffer a freshly-received payload (truncated to capacity; ≤ 96 B by spec).
    fn set(&mut self, payload: &[u8]) {
        let n = payload.len().min(GRID_PAYLOAD_MAX);
        self.buf[..n].copy_from_slice(&payload[..n]);
        self.len = n;
        self.have = true;
    }
}

/// A `Copy` snapshot of a buffered GRID payload handed to `main` by
/// [`RadioManager::take_grid_offer`]. `buf[..len]` is the verbatim `GRID|…` bytes.
#[derive(Clone, Copy)]
pub struct GridOffer {
    pub buf: [u8; GRID_PAYLOAD_MAX],
    pub len: usize,
}

/// #21/#56 leaf-relay: buffers the most-recent inbound `SMOLv1 CFG` value that targeted
/// THIS leaf, held in ONE `.bss` slot PER config key (#56) so keyed frames arriving in the
/// same relay burst (the gateway broadcasts every cached key back-to-back) don't clobber
/// each other. The `service()` arm target-filters on `self.id` before buffering (a config
/// for another leaf never lands here) and `set` key-filters on [`CFG_APPLY_KEYS`] (an
/// unapplied key is dropped). `main` takes a channel via [`RadioManager::take_cfg_offer`]
/// and runs the bytes through that channel's validator (screen → `parse_default_screen`).
/// Fixed `.bss`, no heap. Twin of [`BattTracker`], generalised to N keyed slots.
struct CfgTracker {
    vals: [[u8; crate::net::wifi::CFG_VALUE_MAX]; CFG_APPLY_KEYS.len()],
    lens: [u8; CFG_APPLY_KEYS.len()],
    have: [bool; CFG_APPLY_KEYS.len()],
}

impl CfgTracker {
    const fn new() -> Self {
        Self {
            vals: [[0; crate::net::wifi::CFG_VALUE_MAX]; CFG_APPLY_KEYS.len()],
            lens: [0; CFG_APPLY_KEYS.len()],
            have: [false; CFG_APPLY_KEYS.len()],
        }
    }

    /// The `.bss` slot for `key`, or `None` if this build doesn't apply that key.
    fn slot(key: u8) -> Option<usize> {
        CFG_APPLY_KEYS.iter().position(|&k| k == key)
    }

    /// Buffer a freshly-received value under its `key` (truncated to capacity); returns
    /// `true` if buffered. A `key` not in [`CFG_APPLY_KEYS`] is DROPPED and returns `false`
    /// (#56 forward-compat — a newer gateway may relay a config channel this firmware
    /// predates; ignore it rather than mis-apply, per the #46 clamp discipline).
    fn set(&mut self, key: u8, value: &[u8]) -> bool {
        match Self::slot(key) {
            Some(i) => {
                let n = value.len().min(crate::net::wifi::CFG_VALUE_MAX);
                self.vals[i][..n].copy_from_slice(&value[..n]);
                self.lens[i] = n as u8;
                self.have[i] = true;
                true
            }
            None => false,
        }
    }

    /// Take (clear) the buffered value for `key`, or `None` if none pending / unapplied key.
    fn take(&mut self, key: u8) -> Option<CfgOffer> {
        let i = Self::slot(key)?;
        if self.have[i] {
            self.have[i] = false;
            Some(CfgOffer { buf: self.vals[i], len: self.lens[i] as usize })
        } else {
            None
        }
    }
}

/// A `Copy` snapshot of one keyed CFG channel's buffered value, handed to `main` by
/// [`RadioManager::take_cfg_offer`]. For the screen key, `buf[..len]` is the verbatim
/// `<AppKind>:<page>` value (empty = clear → board default) that `main` parses via
/// `parse_default_screen`; other keys (#48/#43/#55) interpret their own value grammar.
#[derive(Clone, Copy)]
pub struct CfgOffer {
    pub buf: [u8; crate::net::wifi::CFG_VALUE_MAX],
    pub len: usize,
}

// =========================================================================
// Per-peer Roster (Bench mesh-view — scratch/smol/bench-mesh-view-spec.md).
// =========================================================================
//
// A MAC-keyed link/identity table populated ADDITIVELY beside the aggregate
// `PeerTracker`, from the SAME `service()` arms. It RETAINS per-peer what already
// flows through `service()` (id, MAC, rssi, synced_at, last-heard/ack) so Bench
// can show "who is on the mesh." It NEVER feeds the blue LED — `peer_led_state`
// still reads only `PeerTracker`, so the hardware-verified handshake→LED path is
// byte-identical. Zero new wire frames: pure retention of data already arriving.
//
// Keyed on the MAC because it is the ONLY id on EVERY frame — an ACK carries the
// *acked* id (ours), not the sender's, so only `src_address` attributes "this
// peer acked us." The logical id is learned from id-bearing frames (HELLO / SNK /
// TIME) and drives the displayed noun.

/// Per-peer table capacity. Matches the snake `PEER_CAP` and the realistic mesh N.
const ROSTER_CAP: usize = 16;
/// Bench "recently seen" window — deliberately longer than the LED's
/// `PEER_STALE_MS` (3 s) so a node lingers on the list ~30 s after going quiet.
const ROSTER_STALE_MS: u64 = 30_000;
/// #28: the ESP-NOW *hardware* peer-table cap. `ESP_NOW_MAX_TOTAL_PEER_NUM` = 20 on the
/// ESP32-C3 (esp-wifi-sys), and it INCLUDES the broadcast peer. esp-wifi never auto-evicts and
/// `add_peer` silently `Err`s once full — so without our own bound the table grows monotonically
/// (every heard MAC is added, never removed) and the mesh ceilings at ~20 nodes: the 20th+ node's
/// unicast (ACK / RELAYACK / OTAN) can never register. We LRU-evict before `add_peer` (see
/// `ensure_peer`). Kept as `i32` to match `PeerCount::total_count` from esp-wifi.
const ESP_NOW_PEER_CAP: i32 = 20;

/// One tracked peer, keyed by MAC. `Copy` so the table lives in `.bss` (no heap).
#[derive(Clone, Copy)]
struct Node {
    used: bool,
    mac: [u8; 6],
    id: u8,
    id_known: bool,
    last_heard_ms: u64,
    last_ack_ms: u64,
    rssi: i32,
    synced_at: u32,
}

impl Node {
    const EMPTY: Self = Self {
        used: false,
        mac: [0; 6],
        id: 0,
        id_known: false,
        last_heard_ms: 0,
        last_ack_ms: 0,
        rssi: 0,
        synced_at: 0,
    };
}

/// Fixed-cap MAC-keyed peer table (~34 B × 16 ≈ 0.5 KB, fixed, no heap). Fed
/// additively from `service()`; read by Bench via [`RadioManager::roster`].
struct Roster {
    nodes: [Node; ROSTER_CAP],
}

impl Roster {
    const fn new() -> Self {
        Self { nodes: [Node::EMPTY; ROSTER_CAP] }
    }

    /// #40: the MAC last heard for a KNOWN leaf `id` (learned from its HELLOs), for the
    /// unicast OTA relay. `None` if that id hasn't been heard yet.
    fn mac_for_id(&self, id: u8) -> Option<[u8; 6]> {
        for n in &self.nodes {
            if n.used && n.id_known && n.id == id {
                return Some(n.mac);
            }
        }
        None
    }

    /// #28: eviction value-key for a MAC — LOWER is LESS valuable (evicted first). Orders by
    /// usability (`id_known` — a MAC we can't place in HA / can't relay to is worthless), then
    /// liveness (`connected` = a fresh ACK within `PEER_STALE_MS`), then signal (`rssi`), then
    /// age (`last_heard_ms`). `None` when the MAC isn't in the roster at all — i.e. a HW peer the
    /// roster has already dropped (a ghost), which `ensure_peer` treats as the first to evict.
    fn value_key_for_mac(&self, mac: [u8; 6], now: u64) -> Option<(bool, bool, i32, u64)> {
        for n in &self.nodes {
            if n.used && n.mac == mac {
                return Some((
                    n.id_known,
                    PeerTracker::fresh(n.last_ack_ms, now),
                    n.rssi,
                    n.last_heard_ms,
                ));
            }
        }
        None
    }

    /// Find the slot for `mac`: existing match, else a free slot, else evict the
    /// oldest-heard (bounded — a new MAC past capacity drops the stalest).
    fn slot(&mut self, mac: [u8; 6]) -> usize {
        for i in 0..ROSTER_CAP {
            if self.nodes[i].used && self.nodes[i].mac == mac {
                return i;
            }
        }
        for i in 0..ROSTER_CAP {
            if !self.nodes[i].used {
                self.nodes[i] = Node::EMPTY;
                self.nodes[i].used = true;
                self.nodes[i].mac = mac;
                return i;
            }
        }
        let mut victim = 0;
        let mut oldest = u64::MAX;
        for i in 0..ROSTER_CAP {
            if self.nodes[i].last_heard_ms < oldest {
                oldest = self.nodes[i].last_heard_ms;
                victim = i;
            }
        }
        log::warn!("smol: roster full ({}); evicting oldest peer", ROSTER_CAP);
        self.nodes[victim] = Node::EMPTY;
        self.nodes[victim].used = true;
        self.nodes[victim].mac = mac;
        victim
    }

    /// Record any frame heard from `mac` (freshens last-heard + rssi; learns the
    /// logical id when the frame carried one).
    fn heard(&mut self, mac: [u8; 6], id: Option<u8>, rssi: i32, now: u64) {
        let i = self.slot(mac);
        let n = &mut self.nodes[i];
        n.last_heard_ms = now;
        n.rssi = rssi;
        if let Some(id) = id {
            n.id = id;
            n.id_known = true;
        }
    }

    /// Record an ACK addressed to US from `mac` (per-peer "connected").
    fn acked(&mut self, mac: [u8; 6], now: u64) {
        let i = self.slot(mac);
        self.nodes[i].last_ack_ms = now;
    }

    /// Record a TIME frame from `mac`: its `synced_at` + freshen heard/rssi/id.
    fn synced(&mut self, mac: [u8; 6], id: Option<u8>, synced_at: u32, rssi: i32, now: u64) {
        let i = self.slot(mac);
        let n = &mut self.nodes[i];
        n.last_heard_ms = now;
        n.rssi = rssi;
        n.synced_at = synced_at;
        if let Some(id) = id {
            n.id = id;
            n.id_known = true;
        }
    }

    /// Snapshot the fresh peers (heard within `ROSTER_STALE_MS`), strongest-RSSI
    /// first, into a `Copy` [`RosterView`] Bench renders with no live radio borrow.
    fn view(&self, now: u64) -> RosterView {
        let mut out = [NodeView::EMPTY; ROSTER_CAP];
        let mut count = 0;
        for n in self.nodes.iter() {
            if !n.used || now.saturating_sub(n.last_heard_ms) > ROSTER_STALE_MS {
                continue;
            }
            out[count] = NodeView {
                id: n.id,
                id_known: n.id_known,
                rssi: n.rssi,
                age_s: (now.saturating_sub(n.last_heard_ms) / 1000) as u32,
                has_mesh_time: n.synced_at != 0,
                connected: PeerTracker::fresh(n.last_ack_ms, now),
            };
            count += 1;
        }
        // Insertion sort the populated prefix by RSSI descending (nearest first).
        // ≤16 elements, no_std-safe, no alloc.
        for i in 1..count {
            let mut j = i;
            while j > 0 && out[j].rssi > out[j - 1].rssi {
                out.swap(j, j - 1);
                j -= 1;
            }
        }
        RosterView { nodes: out, count }
    }
}

/// A `Copy` per-peer snapshot for the Bench UI (no live borrow of the radio).
#[derive(Clone, Copy)]
pub struct NodeView {
    /// Logical id (drives the noun); meaningful only when `id_known`.
    pub id: u8,
    /// Whether an id-bearing frame has been heard from this peer yet.
    pub id_known: bool,
    /// Most-recent frame RSSI (dBm).
    pub rssi: i32,
    /// Seconds since we last heard this peer.
    pub age_s: u32,
    /// True once a TIME frame with a real `synced_at` has been heard (the `*`).
    pub has_mesh_time: bool,
    /// True if a fresh ACK addressed to us has been heard from this peer (the
    /// same `PEER_STALE_MS` freshness the LED uses, but per-peer).
    pub connected: bool,
}

impl NodeView {
    const EMPTY: Self = Self {
        id: 0,
        id_known: false,
        rssi: 0,
        age_s: 0,
        has_mesh_time: false,
        connected: false,
    };
}

/// A `Copy` roster snapshot: `nodes[..count]` are the fresh peers, RSSI desc.
#[derive(Clone, Copy)]
pub struct RosterView {
    pub nodes: [NodeView; ROSTER_CAP],
    pub count: usize,
}

/// #57: the `RosterView.nodes` array length, exposed so the familiar's wander
/// candidate buffer can be sized to match without hard-coding 16.
pub const ROSTER_VIEW_CAP: usize = ROSTER_CAP;

/// #27: a bounded `core::fmt::Write` sink over a caller-owned `&mut [u8]`. Writes
/// TRUNCATE (never panic) once the buffer is full — so any serializer built on it
/// is total. `len` = committed bytes (always ≤ `buf.len()`); `overflow` latches
/// true if a `write_str` couldn't fit in full, letting the caller roll `len` back
/// to a clean boundary and drop the offending item rather than emit a half-record.
struct SliceWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
    overflow: bool,
}

impl core::fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = self.buf.len().saturating_sub(self.len);
        if s.len() > room {
            self.overflow = true;
        }
        let n = s.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
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
// — an MQTT session to the HA broker (`run_mqtt_burst`: publish telemetry +
// retained discovery, receive the retained downlink) — then returns to ESP-NOW ch6.
//
// SINGLE-RADIO AIRTIME COST: a flush burst tunes the one PHY to the AP's channel,
// so the mesh is DEAF on ch6 for the ~seconds it lasts (the documented one-radio
// trade-off — see the module header). Flushes are tens of seconds apart and
// telemetry is loss-tolerant, so this is acceptable; retransmit rides over it.
//
// HONESTY (compile-verified only): the flush uses the PROVEN TIME-SHARE burst
// (disconnect -> associate -> MQTT -> re-pin ch6), the same pattern boot NTP uses,
// NOT true concurrent COEXIST. Per Nebula, STA-associated + ESP-NOW RX
// reliability / DTIM latency under real COEXIST is UNVERIFIED on this board, so we
// deliberately pause RX during the burst rather than depend on it.
//
// SECURITY: like every SMOLv1 frame, RELAY is unauthenticated + unencrypted — any
// on-channel device can inject telemetry attributed to any src id, or spoof a
// RELAYACK. Fine for a hobby mesh; sign or LMK-encrypt if it ever matters.
//
// OUT OF SCOPE this run (documented stubs, NOT implemented):
//   * PER-LEAF DOWNLINK (broker -> one leaf): the v2 BATT downlink covers the
//     broadcast display case (retained `smol/display/batt` -> gateway-only
//     `SMOLv1 BATT` broadcast, single-hop). Addressed unicast payloads back to a
//     specific leaf would still need a poll/queue + unicast fragmentation; the
//     hooks exist (`service` has the leaf MAC, `send_to` unicasts) but the
//     queue/protocol is unspecified, so it stays deferred.
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
/// After this many CONSECUTIVE failed flushes, shed the OLDEST queued message on
/// every further failure. Bounds queue staleness AND lets a gateway stuck against
/// a dead AP drain to empty → `relay_ready_to_flush` goes false → the blocking
/// bursts STOP (finding 1c). A couple of transient failures are tolerated first.
const FLUSH_FAILS_BEFORE_DROP: u8 = 2;
/// R-DEMOTE (oracle audit-#1): after this many CONSECUTIVE failed flushes the AP is
/// GENUINELY gone (R-CONNECT would have recovered a mere roam within seconds), so the
/// gateway relinquishes ownership — `is_gateway=false` + drop to leaf-scan — and (with the
/// #51 A1 silence gate) stops HELLOing, letting a reachable board take over. Set above
/// `FLUSH_FAILS_BEFORE_DROP` so a transient broker blip never flap-demotes.
/// #51 speed-up: 3 (≈`FLUSH_FAILS_BEFORE_DEMOTE` × RELAY_FLUSH_INTERVAL_MS ≈ 90 s of
/// sustained no-AP) — still safely past a transient blip / roam (R-CONNECT recovers those in
/// seconds), but ~60 s snappier than the prior 5 for the powered-uplink-loss (R4) failover.
const FLUSH_FAILS_BEFORE_DEMOTE: u8 = 3;
/// Recently-completed `(src_mac, msgid)` memory. A lost RELAYACK makes a leaf
/// retransmit an ALREADY-complete message; we must re-ACK it but NOT re-enqueue
/// (else duplicate UDP delivery — finding 3). Ring of the last few completions.
const DONE_RING: usize = 4;

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
    enq_ms: u64, // enqueue time, so the queue can shed its OLDEST (finding 1c)
    buf: [u8; RELAY_MAX_MSG],
}

impl GwMsg {
    const fn new() -> Self {
        Self {
            used: false,
            src_id: 0,
            len: 0,
            enq_ms: 0,
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
    /// Consecutive failed flushes; past `FLUSH_FAILS_BEFORE_DROP` we shed the
    /// queue's oldest each failure so a dead-AP gateway drains + stops re-bursting.
    flush_fails: u8,
    /// Recently-completed `(src_mac, msgid)` ring — dedup post-completion
    /// retransmits so a message is never enqueued (UDP-delivered) twice (finding 3).
    done: [Option<([u8; 6], u16)>; DONE_RING],
    done_cursor: usize,
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
            flush_fails: 0,
            done: [None; DONE_RING],
            done_cursor: 0,
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
        // Finding 3: if we ALREADY completed this (src_mac, msgid), the leaf is
        // retransmitting because its RELAYACK was lost. Re-ACK it as complete (so
        // it stops) but do NOT re-reassemble/enqueue — that would UDP-deliver the
        // same telemetry twice and burn a queue slot.
        if self.recently_done(&src_mac, msgid) {
            return (frag_mask(count), true);
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
            self.enqueue(idx, total_len, now);
            self.reasm[idx].used = false; // free the slot
            self.mark_done(src_mac, msgid); // remember it → dedup late retransmits
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
    fn enqueue(&mut self, reasm_idx: usize, total_len: usize, now: u64) {
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
        q.enq_ms = now;
        q.buf[..len].copy_from_slice(&src_buf[..len]);
    }

    /// Finding 3: is `(src_mac, msgid)` in the recently-completed ring?
    fn recently_done(&self, src_mac: &[u8; 6], msgid: u16) -> bool {
        self.done.contains(&Some((*src_mac, msgid)))
    }

    /// Record a completed `(src_mac, msgid)` in the ring (evicting the oldest entry).
    fn mark_done(&mut self, src_mac: [u8; 6], msgid: u16) {
        self.done[self.done_cursor] = Some((src_mac, msgid));
        self.done_cursor = (self.done_cursor + 1) % DONE_RING;
    }

    /// Finding 1c: shed the OLDEST buffered message so a gateway stuck against a
    /// dead AP bounds queue staleness and eventually drains to empty. No-op if
    /// the queue is already empty.
    fn drop_oldest(&mut self) {
        let mut victim: Option<usize> = None;
        let mut oldest = u64::MAX;
        for (i, q) in self.queue.iter().enumerate() {
            if q.used && q.enq_ms <= oldest {
                oldest = q.enq_ms;
                victim = Some(i);
            }
        }
        if let Some(i) = victim {
            self.queue[i].used = false;
        }
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
    /// #68/#76 SELF-MAC: our own STA/ESP-NOW MAC, captured once at `new()`. `service()` drops
    /// inbound frames whose `src` equals this — the esp-wifi RX path can loop our own broadcasts
    /// back, which otherwise self-rosters us (roster anomaly #1) and pollutes PEERS.
    self_mac: [u8; 6],
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
    /// Per-peer link/identity table for the Bench mesh-view (issue #8). Fed
    /// additively from `service()` beside `peers`; never feeds the LED.
    roster: Roster,
    /// Most-recent inbound SMOLv1 BATT payload, buffered for `main` to store into
    /// its `BattCache` (leaves receive the gateway's HA battery downlink here).
    batt: BattTracker,
    /// Twin of `batt` (issue #16): most-recent inbound SMOLv1 GRID payload, buffered
    /// for `main` to store into its `GridCache`.
    grid: GridTracker,
    /// #21/#56 leaf-relay (LEAF side): most-recent inbound `SMOLv1 CFG` value PER config
    /// key that targeted THIS leaf, buffered for `main` to apply via `take_cfg_offer(key)`.
    cfg: CfgTracker,
    /// #21/#56 leaf-relay (GATEWAY side): per-(leaf, key) config cache, filled from the
    /// MQTT wildcard during a flush and re-broadcast as keyed `SMOLv1 CFG` frames on the
    /// ~10 s cadence. Lives here so it persists across bursts (a (re)joined leaf converges
    /// without HA re-publishing). #56 fills only the screen key. Unused on a leaf.
    cfg_cache: crate::net::wifi::CfgCache,
    /// #50b leaf-status uplink (GATEWAY side): per-leaf live `<screen>:<page>` cache,
    /// filled by the `SMOLv1 STAT` service arm from leaf uplinks (leaves have no MQTT) and
    /// republished as retained `smol/<leaf>/status` on each gateway flush. Twin of
    /// `cfg_cache` but UPLINK-sourced. Unused on a leaf (never flushes/publishes). REUSES
    /// `CfgCache` as a single-channel map — pinned to one fixed key so its (id, key) upsert
    /// behaves id-keyed (`Batt:3` fits `CFG_VALUE_MAX`); the key column is inert here.
    stat_cache: crate::net::wifi::CfgCache,
    /// #70/#49 observability (GATEWAY side): per-node relayed DIAG record cache, filled by the
    /// `SMOLv1 DIAG` service arm from node uplinks and republished as retained `smol/<id>/diag`
    /// on each flush (F6 freshness-gated). Twin of `stat_cache` but a bigger value. Unused leaf-side.
    diag_cache: crate::net::wifi::RelayCache,
    /// #71 observability (GATEWAY side): per-node relayed one-shot WiFi-scan cache, twin of
    /// `diag_cache` → retained `smol/<id>/scan`. Unused leaf-side.
    scan_cache: crate::net::wifi::RelayCache,
    /// #71: this GATEWAY's OWN pending scan record — set by `run_scan` when the gateway self-scans
    /// (its own `W`), consumed by the next flush (published to `smol/<id>/scan`, then cleared —
    /// one-shot; retained MQTT holds it). `None` when no gateway self-scan is pending. Leaf-side a
    /// scan is relayed via `broadcast_scan` instead, so this stays `None`.
    own_scan: Option<alloc::string::String>,
    /// #70/#49 observability: this node's own live diag counters (min-heap watermark + BOOT-button
    /// press counters; #49 adds flush/verify counters). Folded into the DIAG record each cadence.
    diag: DiagCounters,
    /// #74 obs wave-2: node state mirrored from `main` (LED mode + time-sync) for the DIAG record.
    diag_extra: DiagExtra,
    /// #40 leaf-mesh-OTA: the LEAF receive state machine (verify-sig → signed-bounds →
    /// window reassembly → readback verify → activate). Idle except during a transfer.
    /// Unused on a gateway (which drives the RELAY side via `run_leaf_ota_relay`).
    ota_leaf: crate::ota_mesh::OtaLeafSession,
    /// #25 WLED: monotonic WiZmote sequence, ++ per emitted button. Wraps safely
    /// (reboot-to-0 is panic-safe; WLED's per-remote dedup may drop the first few
    /// post-reboot presses until it re-exceeds the pre-reboot value — accepted MVP).
    #[cfg(feature = "wled")]
    wled_seq: u32,
    /// #23: the elected mesh owner's id (the single coexist gateway). Set at boot from
    /// the broker election; a leaf scans for THIS id's HELLO to find the mesh channel.
    elected_owner: u8,
    /// #23 leaf scan-discovery state (stage 2): locked onto the owner's channel, the
    /// current 1/6/11 scan index, last channel-hop time, last time the owner was heard.
    scan_locked: bool,
    scan_idx: usize,
    last_scan_hop_ms: u64,
    last_owner_heard_ms: u64,
    /// #29: the gateway's operating ESP-NOW channel, LEARNED from the `rx_control` of received
    /// frames (in `service`) — 0 until the first frame is heard. Published in the retained
    /// `MC|owner|<ch>|seq` election record (via `current_channel`) so a re-electing / roaming leaf
    /// can pre-tune to it instead of scanning 1/6/11 (issue #29, producer half). ADVISORY — a leaf
    /// still HELLO-scans when it is 0/stale/absent (the proven fallback). Sourced from `rx_control`,
    /// NOT the deliberately-sidestepped `esp_wifi_get_channel`.
    learned_channel: u8,
    /// #23 fix (oracle #1/#2): persistent staleness observation of the retained `MC`
    /// record — the last owner id + seq seen and when that pair was FIRST seen. A seq
    /// frozen past `MC_STALE_MS` marks the owner DEAD (takeover-able). Seeded into the
    /// per-burst `MeshElect` and read back so staleness accrues ACROSS bursts.
    mc_seen_owner: u8,
    mc_seen_seq: u32,
    mc_seen_ms: u64,
    /// #23 fix (oracle #1): last time a LEAF opened a recovery re-election burst — the
    /// retry throttle so a partitioned leaf can't thrash the radio re-associating.
    last_reelect_ms: u64,
    /// #6 OTA: a gated retained announce surfaced by a burst (boot or gateway flush),
    /// pending `main`'s `take_ota_offer` → fetch. `None` when nothing is pending.
    ota_offer: Option<crate::ota::Announce>,
    /// #40: the LAST raw (ungated) `smol/ota/staged` announce this gateway drained,
    /// PERSISTED across flushes. A leaf-OTA install pairs with THIS (not a session-local
    /// capture), so the pair is independent of whether the staged retained was re-drained
    /// in the SAME flush that consumed the install — closing the "install consumed but
    /// relay never armed" race (the staged and install are separate retained topics with
    /// independent drain timing). Persists the current staged image for every leaf relay.
    staged_raw: Option<crate::ota::Announce>,
    /// #40 headless observability + clear/retry: the LAST leaf-OTA attempt's `(leaf_id,
    /// phase, clear_install)`, set by `main` after `run_leaf_ota_relay`, PUBLISHED to
    /// `smol/<leaf>/ota/diag` on the gateway's next burst and used to drive the install
    /// clear (terminal/exhausted) vs retry (transient). `None` when nothing is pending.
    /// The phase is stored as the rendered `&'static str` (not the espnow-only enum) so the
    /// shared `wifi::mqtt_session` signature stays buildable in the wifi-only profile.
    leaf_ota_diag: Option<(u8, &'static str, bool)>,
    /// #40 #1 DECOUPLE: true while a leaf-OTA relay is pending/in-flight (armed until its
    /// outcome is terminal or the retry cap is hit). While set, the gateway SUPPRESSES its own
    /// self-OTA (`do_install`) so a relay is never interrupted by the gateway rebooting into a
    /// fresh build mid-session (and the two OTAs never collide/thrash the fleet).
    leaf_ota_pending: bool,
    /// #3 (self-OTA-first, multi-leaf gap): true while ANY leaf still holds a retained OTA install,
    /// as observed by the last TRUSTED gateway flush. Distinct from the per-session
    /// `leaf_ota_pending` (which a terminal `record_leaf_ota` clears the instant ONE leaf resolves):
    /// this stays latched across the terminal→next-flush gap until a completed flush sees ZERO
    /// installs. `main` gates the gateway's self-OTA on `!leaf_installs_outstanding()` TOO, so the
    /// gateway can't self-OTA (rebooting) in that gap — starving a second leaf + inverting the order.
    /// The gateway updates itself LAST, only once every leaf's install has cleared.
    leaf_installs_outstanding: bool,
    /// #40 #3 STICKY MAC: `(leaf_id, mac)` captured the moment an armed leaf became addressable,
    /// held for the whole install session (cleared on a terminal `record_leaf_ota`). The roster
    /// is a bounded 16-slot LRU that EVICTS this leaf while the mesh-deaf relay stops hearing it
    /// → `mac_for_id` reverts to None (canary `mac-unknown` churn); this cache survives eviction.
    leaf_ota_mac: Option<(u8, [u8; 6])>,
    /// #40 #3 RELAY RX-DIAG (instrumentation): from the last `run_leaf_ota_relay` —
    /// `(leaf_id, rx_frames_from_leaf, valid_otan, last_window_reached, total_windows)`.
    /// Published to retained `smol/<leaf>/ota/relaydiag` on the next flush so a headless
    /// `relay-failed` says WHETHER the leaf NAK'd at all (valid_otan>0) and HOW FAR it got
    /// (last_wb/total): rx=0 → gateway heard nothing from the leaf (leaf offline / OTAD not
    /// landing); rx>0,otan=0 → leaf alive but never NAK'd this session; otan>0,last_wb<total →
    /// chunk-loss stall. `last_wb` is in chunk units (matches `total = om::total_chunks`).
    /// Carries the leaf's `LDBG` self-report too (captured during the relay) — see `RelayDiag`.
    leaf_relay_rx: Option<crate::net::wifi::RelayDiag>,
    /// #40: consecutive non-terminal (transient) relay attempts for the current install —
    /// caps the auto-retry so a persistently-failing leaf can't loop the mesh-deaf relay.
    leaf_ota_retries: u8,
    /// #21 node-manager: the parsed default-screen command surfaced by a burst,
    /// pending `main`'s `take_config_offer` → apply. `None` when nothing is pending.
    config_offer: Option<crate::app::DefaultScreen>,
    /// #33 HA Update entity: a retained `install` command was seen on a burst, pending
    /// `main`'s `take_install_request` → AND-gate the fetch. One-shot (cleared on take).
    install_requested: bool,
    /// #51 A1 — SILENT-UNTIL-RELOCK: set true when this board ABDICATES (R-DEMOTE: its
    /// uplink died) so `broadcast_hello` goes quiet. Leaves lock strictly on the OWNER's
    /// HELLO (`peer_id == elected_owner`), so a demoted ex-owner that kept HELLOing would
    /// pin them forever and block re-election (issue #51/#31). Cleared when we re-lock a
    /// new owner's HELLO or win a fresh election — then normal 2 Hz HELLO resumes.
    silent_until_relock: bool,
    /// #51 B — last RSSI-to-AP (dBm, signed) captured after a successful association.
    /// Seeded into the recovery `MeshElect` so the strongest-uplink survivor wins the
    /// re-election. Weak default until the first burst measures it.
    my_rssi_to_ap: i8,
    /// #57 Mesh Familiar: the ALWAYS-ON living-creature state machine (holder /
    /// heartbeat / seq-arbitration / handoff / RSSI-weighted orphan-takeover). Lives
    /// here beside the `roster` it elects from; ticked every main loop by `fam_tick`
    /// so the pet stays alive even when its screen isn't the active plugin (wisp §7).
    fam: FamState,
    /// #57: decoded inbound FAM frames (+ RSSI) buffered by `service()` for `fam_tick`
    /// to drain into `fam`. Bounded, drop-oldest — mirror of `snk`.
    fam_inbox: FamInbox,
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

        // #68/#76 SELF-MAC: capture our STA MAC (ESP-NOW rides the STA interface) so service()
        // can DROP frames from our own address. The esp-wifi RX queue can deliver our own
        // broadcasts back to us; with no self-filter the gateway rosters ITSELF (constant-RSSI,
        // age-0, flags-3 self-entry — roster anomaly #1) and wastes an eviction-immune slot.
        let self_mac = interfaces.sta.mac_address();

        Some(Self {
            controller,
            self_mac,
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
            roster: Roster::new(),
            batt: BattTracker::new(),
            grid: GridTracker::new(),
            cfg: CfgTracker::new(),
            cfg_cache: crate::net::wifi::CfgCache::new(),
            stat_cache: crate::net::wifi::CfgCache::new(),
            diag_cache: crate::net::wifi::RelayCache::new(),
            scan_cache: crate::net::wifi::RelayCache::new(),
            own_scan: None,
            diag: DiagCounters::new(),
            diag_extra: DiagExtra::new(),
            ota_leaf: crate::ota_mesh::OtaLeafSession::new(),
            #[cfg(feature = "wled")]
            wled_seq: 0,
            elected_owner: id,
            scan_locked: false,
            scan_idx: 0,
            learned_channel: 0, // #29: unknown until the first frame is heard

            last_scan_hop_ms: 0,
            last_owner_heard_ms: 0,
            mc_seen_owner: 0,
            mc_seen_seq: 0,
            mc_seen_ms: 0,
            last_reelect_ms: 0,
            ota_offer: None,
            staged_raw: None,
            leaf_ota_diag: None,
            leaf_ota_retries: 0,
            leaf_ota_pending: false,
            leaf_installs_outstanding: false,
            leaf_ota_mac: None,
            leaf_relay_rx: None,
            config_offer: None,
            install_requested: false,
            silent_until_relock: false,
            my_rssi_to_ap: -99,
            // #57: seed the familiar with our id (heartbeat phase + arbitration id).
            // No creature exists until one is heard or first-birthed on a cold mesh.
            fam: FamState::new(id),
            fam_inbox: FamInbox::new(),
        })
    }

    /// #23 stage 2 leaf channel-discovery. Call every subtick from `main`. A LEAF (not
    /// the elected gateway) that hasn't LOCKED onto the owner's HELLO hops the ESP-NOW
    /// channel across 1/6/11 on a dwell; once the owner is heard (see the HELLO arm in
    /// `service`) it locks. If the owner then goes silent past `SCAN_SILENCE_MS` (a
    /// roam), it unlocks + resumes scanning. Gateways never scan (they ride their AP
    /// channel via the live association).
    pub fn leaf_scan_tick(&mut self, now: u64) {
        const CANDIDATES: [u8; 3] = [1, 6, 11]; // JP's roam plan
        const DWELL_MS: u64 = 1500; // listen this long per candidate before hopping
        const SCAN_SILENCE_MS: u64 = 6000; // owner silence → assume roam → re-scan
        if self.relay.is_gateway {
            return; // gateway owns its channel via association; never scans
        }
        // #3b (regression fix): while ACTIVELY receiving a mesh-OTA, HOLD the channel — do NOT
        // unlock/hop. The transfer runs on ESP_NOW_FIXED_CHANNEL and hopping mid-transfer drops
        // chunks. NARROW + bounded: true only during a live session (`is_active`); with no OTA in
        // flight this is a no-op and normal membership behaves EXACTLY as before. (60ff390 made
        // the hold UNCONDITIONAL and broke basic mesh join — id8 couldn't re-acquire the gateway
        // after routine beacon loss the way id9/pre-#40 does. This restores that path and scopes
        // the hold to only when it's actually needed.)
        if self.ota_leaf.is_active() {
            let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL);
            return;
        }
        // RESTORED old behavior: unlock + re-scan on owner-silence. A leaf MUST re-acquire the
        // gateway after routine beacon loss to keep mesh membership. The fetch-drift is handled
        // narrowly instead: (i) receiving an OTA frame locks the leaf to ch6 (`handle_ota_frame`),
        // (ii) the gateway's OTAM wake-burst lets a hopping leaf catch the first announce.
        if self.scan_locked && now.saturating_sub(self.last_owner_heard_ms) > SCAN_SILENCE_MS {
            self.scan_locked = false;
            log::info!("smol: leaf lost gateway id{} — re-scanning", self.elected_owner);
        }
        if self.scan_locked {
            return;
        }
        if now.saturating_sub(self.last_scan_hop_ms) >= DWELL_MS {
            self.scan_idx = (self.scan_idx + 1) % CANDIDATES.len();
            let ch = CANDIDATES[self.scan_idx];
            let _ = self.esp_now.set_channel(ch);
            self.last_scan_hop_ms = now;
        }
    }

    /// #23 fix (oracle #1 dead-owner wedge): a LEAF that has lost its owner's HELLO for
    /// a PROLONGED period re-opens the broker election. This is the ONLY runtime path
    /// that lets a dead lowest-id owner be taken over — leaves never flush, so without
    /// it a fleet whose lowest-id owner dies can never self-heal (it just ghost-scans
    /// the dead id forever). Call every subtick from `main` (cheap: it early-returns
    /// unless a leaf has been owner-silent past `REELECT_SILENCE_MS`, throttled by
    /// `REELECT_RETRY_MS`). Re-associates, runs an election-only MQTT burst (no
    /// telemetry), applies the result to the LIVE role:
    ///   * WON (a stale/dead owner taken over, or the topic empty) → become GATEWAY,
    ///     stay WiFi-associated (mesh rides the AP channel);
    ///   * adopted a LIVE owner (possibly a NEW lower id) → stay leaf, drop back to
    ///     ESP-NOW, re-scan for it. A live owner the leaf simply can't hear is NEVER
    ///     taken over — the takeover fires only on a FROZEN `MC` seq (owner truly dead).
    ///
    /// Returns true iff a re-election burst actually ran.
    pub fn maybe_leaf_reelect(
        &mut self,
        batt: &mut crate::batt::BattCache,
        grid: &mut crate::grid::GridCache,
        now: u64,
        tick: &mut dyn FnMut() -> bool,
    ) -> bool {
        // #51 speed-up: owner HELLOs every 2 s, so 15 s silence ≈ 7 missed HELLOs = gone
        // (a live owner that merely roamed is re-locked by leaf_scan_tick's 6 s re-scan long
        // before this). Retry every 10 s — MUST be < RSSI_BUCKET_STEP_MS (15 s) so a weaker
        // board gets a burst (reads the winner's retained MC → adopts) before its claim window.
        const REELECT_SILENCE_MS: u64 = 15_000; // owner HELLO gone this long → recover
        const REELECT_RETRY_MS: u64 = 10_000; // min gap between recovery bursts
        if self.relay.is_gateway {
            return false; // a gateway re-decides on its own flush; only leaves recover here
        }
        // #3b: a leaf with a LIVE mesh-OTA session must NOT re-elect — re-election re-associates
        // to WiFi (off-ch6) and took id8 OFFLINE mid-relay. While armed, the gateway is merely
        // fetching (off-ch6, transiently silent), NOT dead; the leaf holds ch6 and waits for the
        // OTAD. The session's own deadline/stall (ota_mesh) bounds a genuinely-abandoned relay.
        if self.ota_leaf.is_active() {
            return false;
        }
        let silent = now.saturating_sub(self.last_owner_heard_ms) >= REELECT_SILENCE_MS;
        let retry_ok = now.saturating_sub(self.last_reelect_ms) >= REELECT_RETRY_MS;
        if !(silent && retry_ok) {
            return false;
        }
        self.last_reelect_ms = now;
        log::info!(
            "smol: leaf owner id{} silent {}ms — re-opening broker election",
            self.elected_owner,
            now.saturating_sub(self.last_owner_heard_ms)
        );
        // Re-associate + run an election-ONLY burst (empty telemetry list).
        let _ = self.switch(Mode::WifiSta);
        let id = self.id;
        let mut elect = crate::net::wifi::MeshElect::new(id);
        elect.now_ms = now;
        // #51: this is a LEAF RECOVERY election → select the WiFi-strength rule (sticky live
        // owner + RSSI-weighted dead-owner takeover on the shorter RECOVERY_STALE_MS window).
        // Seed our last-measured RSSI-to-AP so the strongest-uplink survivor wins; node-id
        // only breaks exact-signal ties.
        elect.recovery = true;
        elect.my_rssi = self.my_rssi_to_ap;
        elect.my_channel = self.learned_channel; // #29: seed the MC record's <ch> (0 until learned)
        elect.seen_owner = self.mc_seen_owner;
        elect.seen_seq = self.mc_seen_seq;
        elect.seen_ms = self.mc_seen_ms;
        // #6 OTA / #21 config / #33 install: a leaf's recovery burst can also surface these.
        let mut ota_offer: Option<crate::ota::Announce> = None;
        let mut config_offer: Option<crate::app::DefaultScreen> = None;
        let mut _gw_own = crate::net::wifi::GwOwnCfg::new(); // #48: recovery burst never captures gateway-own cfg
        let mut _reset_req = crate::net::wifi::ResetReq::new(); // #52: recovery burst issues no reboots
        let mut _scan_req = crate::net::wifi::ScanReq::new(); // #71: recovery burst issues no scans
        let mut install_requested = false;
        let mut _leaf_install_seen = false; // #40 #1: a leaf's recovery burst is not a gateway relay
        let reached = match self.sta.as_mut() {
            None => false,
            Some(sta) => {
                let empty: [(u8, &[u8]); 0] = [];
                crate::net::wifi::run_mqtt_burst(
                    &mut self.controller,
                    sta,
                    self.rng,
                    id,
                    &empty,
                    batt,
                    grid,
                    &mut elect,
                    &mut ota_offer,
                    &mut config_offer,
                    &mut _gw_own,
                    &mut _reset_req,
                    &mut install_requested,
                    &mut _leaf_install_seen, // #40 #1: leaf recovery burst — never a gateway relay
                    &[], // #27: election-only recovery burst publishes no peers (leaf/v1)
            &[], // #50: recovery burst publishes no live-screen status
                    None, // #21: a leaf's recovery burst is not a gateway relay
                    None, // #50b: recovery burst republishes no cached leaf status
                    &[], // #70/#49: recovery burst publishes no own diag
                    None, // #70/#49: recovery burst republishes no cached diag
                    &[], // #71: recovery burst publishes no own scan
                    None, // #71: recovery burst republishes no cached scan
                    &mut _scan_req, // #71: recovery burst subscribes no cmd/scan (cfg_cache=None)
                    &mut None, // #40: a leaf's recovery burst never relays a leaf OTA
                    &mut None, // #40: recovery burst carries no persistent staged
                    &mut None, // #40: recovery burst publishes no relay diag
                    &mut None, // #3: recovery burst publishes no relay RX-diag
                    tick,
                )
            }
        };
        if ota_offer.is_some() {
            self.ota_offer = ota_offer;
        }
        if config_offer.is_some() {
            self.config_offer = config_offer;
        }
        if install_requested {
            self.install_requested = true;
        }
        if !reached {
            // Broker unreachable (AP down / off our scan channel): do NOT claim on a
            // failed read — fall back to ESP-NOW + keep scanning; retry after the gap.
            let _ = self.switch(Mode::EspNow);
            log::info!("smol: leaf re-election — broker unreachable, still scanning");
            return true;
        }
        // #51 B: we're still associated here (pre-EspNow-switch) → capture a fresh
        // RSSI-to-AP for the NEXT recovery election's strength comparison.
        if let Ok(r) = self.controller.rssi() {
            self.my_rssi_to_ap = r.clamp(-127, 0) as i8;
        }
        // Persist the refreshed staleness observation (so takeover accrues across bursts).
        self.mc_seen_owner = elect.seen_owner;
        self.mc_seen_seq = elect.seen_seq;
        self.mc_seen_ms = elect.seen_ms;
        self.elected_owner = elect.owner_id;
        self.scan_locked = false;
        if elect.i_am_owner {
            // Took over a dead/stale owner (or empty topic): become the coexist GATEWAY.
            self.relay.is_gateway = true;
            // #51 A1: we own the mesh now → resume HELLO (leaves lock on the owner's HELLO).
            self.silent_until_relock = false;
            // #51 A2: freshly-elected grace — clear the fail counter so a single transient
            // flush miss can't immediately re-demote us (the mandatory anti-flap window).
            self.relay.flush_fails = 0;
            log::info!("smol: leaf re-election WON — now GATEWAY (owner id{})", id);
            // switch() already left us in WifiSta — stay associated (coexist gateway).
        } else {
            // A LIVE owner holds the mesh (possibly a new lower id): stay leaf, re-scan.
            self.relay.is_gateway = false;
            // #51 speed-up: grace-reset the owner-silence clock ONLY for a GENUINELY LIVE
            // owner (give the scan time to re-lock it). A dead-but-inside-our-backoff owner
            // (owner_alive == false) gets NO reset, so the next recovery burst fires on
            // cadence and the deferred takeover isn't pushed out an extra window.
            if elect.owner_alive {
                self.last_owner_heard_ms = now;
            }
            let _ = self.switch(Mode::EspNow);
            log::info!(
                "smol: leaf re-election → owner id{} ({})",
                self.elected_owner,
                if elect.owner_alive { "live — re-scanning" } else { "dead — deferring takeover" }
            );
        }
        true
    }

    /// Run the real WiFi -> DHCP -> SNTP burst using the STA device, driving the
    /// caller's `tick` closure throughout (the `espnow` build fast-blinks the blue
    /// LED so "WiFi/NTP in progress" is visible).
    ///
    /// Returns `(reached_dhcp, synced)`: `reached_dhcp` is true once the burst
    /// ASSOCIATED + got a DHCP lease (proven before SNTP runs) — this decides the
    /// relay GATEWAY role (N3c: decoupled from NTP, so an SNTP outage can't demote
    /// a node with a working LAN uplink); `synced` is the SNTP Unix time or `None`.
    ///
    /// We BORROW the STA device (not `take()`+drop) so a gateway can re-associate
    /// for periodic relay flushes (`flush_telemetry`); the smoltcp interface is
    /// built + dropped INSIDE `run_ntp_burst`, so no live stack contends with
    /// ESP-NOW between bursts.
    #[allow(clippy::too_many_arguments)] // +ota/config/install offer out-params (#6/#21/#33)
    pub fn burst_ntp(
        &mut self,
        batt: &mut crate::batt::BattCache,
        grid: &mut crate::grid::GridCache,
        elect: &mut crate::net::wifi::MeshElect,
        ota_offer: &mut Option<crate::ota::Announce>,
        config_offer: &mut Option<crate::app::DefaultScreen>,
        install_requested: &mut bool,
        tick: &mut dyn FnMut() -> bool,
    ) -> (bool, Option<u32>) {
        // Disjoint field borrows: &mut self.controller, &mut *sta, Copy of rng/id.
        // `batt` is a caller-owned &mut (main's cache), disjoint from every self
        // field — the boot burst's MQTT downlink fills it (see wifi::run_ntp_burst).
        let id = self.id;
        let Some(sta) = self.sta.as_mut() else {
            return (false, None);
        };
        let mut reached_dhcp = false;
        let synced = crate::net::wifi::run_ntp_burst(
            &mut self.controller,
            sta,
            self.rng,
            tick,
            &mut reached_dhcp,
            id,
            batt,
            grid,
            elect,
            ota_offer,
            config_offer,
            install_requested,
        );
        (reached_dhcp, synced)
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
        // #51 A1: an abdicated board (uplink died → R-DEMOTE) stays HELLO-silent until it
        // re-locks a new owner. Leaves pin on the OWNER's HELLO, so a demoted ex-owner that
        // kept HELLOing would block them from ever re-electing (the #51/#31 wedge). TIME and
        // other frames are unaffected — only HELLO drives the owner-silence clock.
        if self.silent_until_relock {
            return;
        }
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

    /// Coexist-soak (#23 PART 1) loss snapshot `(tx, rx, lost, loss_pct)` — `main`
    /// reads it around each flush to bucket during-flush vs idle ESP-NOW RX loss.
    #[cfg(feature = "coexist-soak")]
    pub fn soak_counts(&self) -> (u32, u32, u32, u8) {
        (
            self.bench.tx_count,
            self.bench.rx_count,
            self.bench.lost_count,
            self.bench.loss_pct(),
        )
    }

    /// Coexist-soak periodic report to serial (the measurer reads this off the log):
    /// cumulative BEACON tx/rx, inferred seq-gap losses, loss %, last RTT + RSSI.
    #[cfg(feature = "coexist-soak")]
    pub fn soak_report(&self) {
        log::info!(
            "smol: SOAK role={} tx={} rx={} lost={} loss={}% rtt={:?}ms rssi={:?}",
            if self.relay.is_gateway { "GW" } else { "leaf" },
            self.bench.tx_count,
            self.bench.rx_count,
            self.bench.lost_count,
            self.bench.loss_pct(),
            self.bench.last_rtt_ms,
            self.bench.last_rssi,
        );
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

    // --- Battery downlink relay (see the BATT_PREFIX + BattTracker sections) ---

    /// Broadcast one SMOLv1 BATT frame: the 12-B tag + the verbatim `BATT|…`
    /// `payload` (byte-for-byte the gateway's `BattCache`). GATEWAY-ONLY by
    /// convention — `main` calls this on a slow cadence while `is_gateway` and the
    /// cache is non-empty, so neighbour leaves fill their own cache. No length byte
    /// (payload is the rest of the frame); safe in either radio mode.
    pub fn broadcast_batt(&mut self, payload: &[u8]) {
        // 12-B tag + ≤ 96-B payload = ≤ 108 B, well under the 250-B ESP-NOW limit.
        let mut msg = [0u8; BATT_PREFIX.len() + BATT_PAYLOAD_MAX];
        msg[..BATT_PREFIX.len()].copy_from_slice(BATT_PREFIX);
        let n = payload.len().min(BATT_PAYLOAD_MAX);
        msg[BATT_PREFIX.len()..BATT_PREFIX.len() + n].copy_from_slice(&payload[..n]);
        self.send_to(&BROADCAST_ADDRESS, &msg[..BATT_PREFIX.len() + n]);
    }

    /// Take the buffered inbound BATT payload (if any), clearing it. `main` stores
    /// the bytes into its `BattCache`; mirrors [`take_time_offer`]'s pull pattern.
    ///
    /// [`take_time_offer`]: RadioManager::take_time_offer
    pub fn take_batt_offer(&mut self) -> Option<BattOffer> {
        if self.batt.have {
            self.batt.have = false;
            Some(BattOffer { buf: self.batt.buf, len: self.batt.len })
        } else {
            None
        }
    }

    /// Broadcast one SMOLv1 GRID frame — the exact TWIN of [`broadcast_batt`]
    /// (issue #16). GATEWAY-ONLY by convention (`main` gates on `is_gateway` + a
    /// non-empty cache); 12-B tag + verbatim `GRID|…` payload, no length byte.
    ///
    /// [`broadcast_batt`]: RadioManager::broadcast_batt
    pub fn broadcast_grid(&mut self, payload: &[u8]) {
        let mut msg = [0u8; GRID_PREFIX.len() + GRID_PAYLOAD_MAX];
        msg[..GRID_PREFIX.len()].copy_from_slice(GRID_PREFIX);
        let n = payload.len().min(GRID_PAYLOAD_MAX);
        msg[GRID_PREFIX.len()..GRID_PREFIX.len() + n].copy_from_slice(&payload[..n]);
        self.send_to(&BROADCAST_ADDRESS, &msg[..GRID_PREFIX.len() + n]);
    }

    /// Take the buffered inbound GRID payload (if any), clearing it. Twin of
    /// [`take_batt_offer`]; `main` stores the bytes into its `GridCache`.
    ///
    /// [`take_batt_offer`]: RadioManager::take_batt_offer
    pub fn take_grid_offer(&mut self) -> Option<GridOffer> {
        if self.grid.have {
            self.grid.have = false;
            Some(GridOffer { buf: self.grid.buf, len: self.grid.len })
        } else {
            None
        }
    }

    /// #21/#56 leaf-relay: broadcast one targeted keyed `SMOLv1 CFG` frame — tag + 3-ASCII
    /// zero-padded `target_id` + the 1-byte config `key` (#56) + the verbatim `value`.
    /// Mirror of [`broadcast_batt`]: fixed frame → `send_to(&BROADCAST_ADDRESS, ..)`
    /// (fire-and-forget). Every leaf hears it; only the `target` acts, and only if it
    /// applies `key` (leaf-side target + key filter). Value truncated to `CFG_VALUE_MAX`;
    /// empty value = clear that key.
    ///
    /// [`broadcast_batt`]: RadioManager::broadcast_batt
    pub fn broadcast_config(&mut self, target_id: u8, key: u8, value: &[u8]) {
        let base = CFG_PREFIX.len();
        let mut msg = [0u8; CFG_PREFIX.len() + 3 + 1 + crate::net::wifi::CFG_VALUE_MAX];
        msg[..base].copy_from_slice(CFG_PREFIX);
        // 3-ASCII zero-padded target id (matches `parse_id`; u8 ⇒ each digit 0–9).
        msg[base] = b'0' + target_id / 100;
        msg[base + 1] = b'0' + (target_id / 10) % 10;
        msg[base + 2] = b'0' + target_id % 10;
        msg[base + 3] = key; // #56: config channel key (S=screen / L=led / U=units / P=plugins)
        let n = value.len().min(crate::net::wifi::CFG_VALUE_MAX);
        msg[base + 4..base + 4 + n].copy_from_slice(&value[..n]);
        self.send_to(&BROADCAST_ADDRESS, &msg[..base + 4 + n]);
    }

    /// #50b leaf-status uplink: a LEAF broadcasts its OWN live `<screen>:<page>` as a
    /// `SMOLv1 STAT` frame (own id + verbatim value). Mirror of [`broadcast_config`] but
    /// UPLINK — the id is OURS (the sender), not a target. Fire-and-forget broadcast; the
    /// gateway caches it and republishes retained `smol/<id>/status`. Value truncated to
    /// `CFG_VALUE_MAX`. `main` gates the call on `!is_gateway` + the ~10 s cadence.
    ///
    /// [`broadcast_config`]: RadioManager::broadcast_config
    pub fn broadcast_stat(&mut self, value: &[u8]) {
        let base = STAT_PREFIX.len();
        let mut msg = [0u8; STAT_PREFIX.len() + 3 + crate::net::wifi::CFG_VALUE_MAX];
        msg[..base].copy_from_slice(STAT_PREFIX);
        // 3-ASCII zero-padded OWN id (matches `parse_id`; u8 ⇒ each digit 0–9).
        let own = self.id;
        msg[base] = b'0' + own / 100;
        msg[base + 1] = b'0' + (own / 10) % 10;
        msg[base + 2] = b'0' + own % 10;
        let n = value.len().min(crate::net::wifi::CFG_VALUE_MAX);
        msg[base + 3..base + 3 + n].copy_from_slice(&value[..n]);
        self.send_to(&BROADCAST_ADDRESS, &msg[..base + 3 + n]);
    }

    /// #70/#49: broadcast this node's compact key=val DIAG record as a `SMOLv1 DIAG` frame —
    /// mirror of [`broadcast_stat`] but a bigger value (`RELAY_VALUE_MAX`). Fire-and-forget; the
    /// gateway caches it (`diag_cache`) and republishes retained `smol/<id>/diag`. `main` gates on
    /// the ~60 s diag cadence (a BOOT-button press expedites one). Value truncated to the cap.
    ///
    /// [`broadcast_stat`]: RadioManager::broadcast_stat
    pub fn broadcast_diag(&mut self, value: &[u8]) {
        let base = DIAG_PREFIX.len();
        let mut msg = [0u8; DIAG_PREFIX.len() + 3 + crate::net::wifi::RELAY_VALUE_MAX];
        msg[..base].copy_from_slice(DIAG_PREFIX);
        let own = self.id;
        msg[base] = b'0' + own / 100;
        msg[base + 1] = b'0' + (own / 10) % 10;
        msg[base + 2] = b'0' + own % 10;
        let n = value.len().min(crate::net::wifi::RELAY_VALUE_MAX);
        msg[base + 3..base + 3 + n].copy_from_slice(&value[..n]);
        self.send_to(&BROADCAST_ADDRESS, &msg[..base + 3 + n]);
    }

    /// #71: broadcast this node's one-shot WiFi-scan record as a `SMOLv1 SCAN` frame — twin of
    /// [`broadcast_diag`]. Leaf path: the gateway caches it (`scan_cache`) + republishes retained
    /// `smol/<id>/scan`. Value truncated to `RELAY_VALUE_MAX`.
    ///
    /// [`broadcast_diag`]: RadioManager::broadcast_diag
    pub fn broadcast_scan(&mut self, value: &[u8]) {
        let base = SCAN_PREFIX.len();
        let mut msg = [0u8; SCAN_PREFIX.len() + 3 + crate::net::wifi::RELAY_VALUE_MAX];
        msg[..base].copy_from_slice(SCAN_PREFIX);
        let own = self.id;
        msg[base] = b'0' + own / 100;
        msg[base + 1] = b'0' + (own / 10) % 10;
        msg[base + 2] = b'0' + own % 10;
        let n = value.len().min(crate::net::wifi::RELAY_VALUE_MAX);
        msg[base + 3..base + 3 + n].copy_from_slice(&value[..n]);
        self.send_to(&BROADCAST_ADDRESS, &msg[..base + 3 + n]);
    }

    /// #71: run ONE on-demand WiFi AP scan (triggered by applying the `W` command) → publish the
    /// strongest APs to `smol/<id>/scan`.
    ///
    /// ⚠️ COEXIST (the #71 caveat, the 2026-07-11 lesson): a scan takes the SINGLE radio OFF the
    /// mesh channel for its duration — a mesh-deaf blip. So it is ON-DEMAND ONLY, NEVER periodic,
    /// and HARD-SKIPPED while a mesh-OTA transfer is live. After the scan the ESP-NOW channel is
    /// re-pinned (the scan hopped the PHY across 1/6/11). Record = up to `SCAN_MAX_APS`
    /// `<ssid>,<bssid-3oct>,<ch>,<rssi>` groups `|`-joined, strongest-RSSI first; BSSIDs are
    /// truncated to 3 octets (this is a PUBLIC-repo-documented topic — a full BSSID is a privacy
    /// leak, per nebula's #71 note). Role: a GATEWAY stashes it in `own_scan` (its next flush
    /// publishes it — the gateway has MQTT); a LEAF broadcasts it (the gateway republishes).
    ///
    /// ⚠️ HW-CANARY-GATED: the scan radio-path + channel-restore cannot be verified without
    /// hardware — canary ONE node before any fleet-wide use (same discipline as OTA).
    pub fn run_scan(&mut self) {
        // COEXIST hard gate: never leave the mesh channel while a mesh-OTA transfer is live.
        if self.ota_leaf.is_active() {
            log::info!("smol #71: scan skipped — mesh-OTA session active (coexist)");
            return;
        }
        let ch_before = self.current_channel();
        // scan_n = scan_with_config_sync_max(Default, N): a synchronous full-band scan capped at N
        // results. We cap generously then keep the strongest few, so a busy band still yields the
        // most-relevant APs.
        let record = match self.controller.scan_n(16) {
            Ok(mut aps) => {
                // Strongest RSSI first (descending → Reverse of the ascending key).
                aps.sort_by_key(|a| core::cmp::Reverse(a.signal_strength));
                format_scan_record(&aps)
            }
            Err(_) => alloc::string::String::from("SCAN|err"),
        };
        // Re-pin the mesh channel (the scan hopped the PHY off it). Prefer the pre-scan locked
        // channel; fall back to the fixed ESP-NOW channel if we were unlocked (0 = scanning).
        let restore = if ch_before != 0 { ch_before } else { ESP_NOW_FIXED_CHANNEL };
        let _ = self.esp_now.set_channel(restore);
        if self.relay.is_gateway {
            self.own_scan = Some(record); // the next flush publishes smol/<id>/scan (then clears it)
        } else {
            self.broadcast_scan(record.as_bytes()); // relay → gateway caches + republishes
        }
    }

    /// #70: sample the live free-heap and lower the min-heap watermark. Cheap (one `HEAP.free()`);
    /// `main` calls it on the ~10 s tick so the watermark tracks leak/pressure at finer resolution
    /// than the slow diag publish cadence.
    pub fn diag_sample_heap(&mut self) {
        let free = esp_alloc::HEAP.free() as u32;
        if free < self.diag.heap_min {
            self.diag.heap_min = free;
        }
    }

    /// #70: record a BOOT-button press (`long` = long-press). Bumps the monotonic press counter
    /// that rides the DIAG record — HA fires a distinct press / long-press event on each increment.
    pub fn note_button(&mut self, long: bool) {
        if long {
            self.diag.btnl = self.diag.btnl.wrapping_add(1);
        } else {
            self.diag.btn = self.diag.btn.wrapping_add(1);
        }
    }

    /// #49: record an MQTT flush outcome (`ok` = reached CONNACK). The flush-success rate proves
    /// the #9 flush-win on hardware (was UART0-only). Gateway-only in practice (leaves never flush).
    pub fn note_flush(&mut self, ok: bool) {
        if ok {
            self.diag.flush_ok = self.diag.flush_ok.saturating_add(1);
        } else {
            self.diag.flush_fail = self.diag.flush_fail.saturating_add(1);
        }
    }

    /// #74: mirror `main`-owned node state (LED mode + its lit state, time-sync age + source) into
    /// the RadioManager so the DIAG record can fold in `led`/`tage`/`tsrc`. `main` calls this on the
    /// ~10 s diag-sample tick; both diag builders (leaf broadcast + gateway flush) read the copy.
    pub fn set_diag_extra(&mut self, led_mode: &'static str, led_on: bool, tage_s: u32, tsrc: &'static str) {
        self.diag_extra = DiagExtra { led_mode, led_on, tage_s, tsrc };
    }

    /// #70/#49: build this node's compact DIAG record for retained `smol/<id>/diag`. Format matches
    /// luna's DEPLOYED HA parser (PR #81): literal `DIAG` first field, then `key=value` pairs
    /// PIPE-separated, order-independent, forward-compatible (HA picks by key, unknown keys ignored,
    /// missing → safe default). ONE record carries the whole signal set — HA parses it package-side,
    /// NO per-signal MQTT discovery (no entity doubling).
    ///
    /// Keys: `slot`=running OTA slot (0/1, 255=unknown), `rst`=reset reason (luna's enum + `panic`),
    /// `boot`=boot count, `ota`=last OTA OUTCOME (confirmed/rolled-back/none — read LIVE, set by
    /// `boot_confirm`; drives luna's rollback alert), `up`=uptime_s, `heap`=free bytes, `hmin`=min-
    /// free watermark, `btn`/`btnl`=BOOT short/long press counts. #49 extras (folded in, ignored by
    /// HA until it adds sensors): `fok`/`ffl`=flush ok/fail (#9 proof), `vok`/`vfl`=OTA verify ok/fail
    /// (#32 proof), `loss`=mesh loss % (luna's #74-reserved `loss` key, same semantic). Samples heap
    /// first so `hmin <= heap` holds. Heap `String` (like the STAT/telemetry builders) — panic-free.
    pub fn diag_record(&mut self) -> alloc::string::String {
        self.diag_sample_heap();
        let up_s = now_ms() / 1000;
        let d = crate::ota::boot_diag();
        let ota = crate::ota::ota_outcome_token(); // live — boot_confirm sets it after capture
        let heap = esp_alloc::HEAP.free();
        let (vok, vfl) = self.ota_leaf.verify_counts();
        let loss = self.bench.loss_pct();
        // #74 wave-2 fold-ins: mesh RTT + cumulative rx/tx (from `bench`), LED mode:state + time
        // age/source (mirrored from `main` via `set_diag_extra`). `loss`/`rtt`/`rx`/`tx` = the #49
        // link-quality set; `led`/`tage`/`tsrc` = items 6/8. (`toff`/`cfg` deferred — see luna Q.)
        let rtt = self.bench.last_rtt_ms.unwrap_or(0);
        let rx = self.bench.rx_count;
        let tx = self.bench.tx_count;
        let e = self.diag_extra;
        let led_state = if e.led_on { "on" } else { "off" };
        alloc::format!(
            "DIAG|slot={}|rst={}|boot={}|ota={}|up={}|heap={}|hmin={}|btn={}|btnl={}|fok={}|ffl={}|vok={}|vfl={}|loss={}|rtt={}|rx={}|tx={}|led={}:{}|tage={}|tsrc={}",
            d.boot_slot,
            d.reset_reason,
            d.boot_count,
            ota,
            up_s,
            heap,
            self.diag.heap_min,
            self.diag.btn,
            self.diag.btnl,
            self.diag.flush_ok,
            self.diag.flush_fail,
            vok,
            vfl,
            loss,
            rtt,
            rx,
            tx,
            e.led_mode,
            led_state,
            e.tage_s,
            e.tsrc,
        )
    }

    /// #40 #3: broadcast this LEAF's OTA RX-diag beacon (`LDBG` = id + heard/verdict/sent) so a
    /// relaying gateway captures `on_meta`'s verdict LIVE (folded into `…/ota/relaydiag`). `main`
    /// gates the call on `!is_gateway` + the ~2 s HELLO cadence. Payload is fixed-width binary.
    /// `espnow`-scoped: the `LDBG` frame + `ota_leaf` self-report only exist in the mesh build.
    #[cfg(feature = "espnow")]
    pub fn broadcast_ldbg(&mut self, heard: u16, verdict: u8, sent: u16) {
        let ch = self.current_channel(); // #3b: 0 = scanning/unlocked, else the locked channel
        let mut msg = [0u8; crate::ota_mesh::LDBG_FRAME_LEN];
        let base = encode_id_frame(crate::ota_mesh::LDBG_PREFIX, self.id, &mut msg);
        msg[base..base + 2].copy_from_slice(&heard.to_le_bytes());
        msg[base + 2] = verdict;
        msg[base + 3..base + 5].copy_from_slice(&sent.to_le_bytes());
        msg[base + 5] = ch;
        self.send_to(&BROADCAST_ADDRESS, &msg[..base + 6]);
    }

    /// #21 leaf-relay: GATEWAY-only. Broadcast one `SMOLv1 CFG` per CACHED leaf
    /// config (skipping the gateway's OWN id — it self-applies via the credentialed
    /// MQTT path). Single-hop (leaves never re-broadcast → no flood/loop). No-op on a
    /// leaf or an empty cache. `main` gates the call on `is_gateway` + the ~10 s tick,
    /// mirroring the BATT/GRID re-broadcast cadence; edge-trigger on the leaf side
    /// makes the periodic resend idempotent (never yanks a user off their screen).
    pub fn broadcast_cached_configs(&mut self) {
        if !self.relay.is_gateway {
            return;
        }
        let own = self.id;
        let count = self.cfg_cache.count();
        for i in 0..count {
            // Copy the entry out to release the `cfg_cache` borrow before the
            // `&mut self` broadcast_config call (disjoint-borrow discipline).
            let mut vbuf = [0u8; crate::net::wifi::CFG_VALUE_MAX];
            let (id, key, vlen) = match self.cfg_cache.entry(i) {
                Some((id, key, v)) => {
                    let l = v.len();
                    vbuf[..l].copy_from_slice(v);
                    (id, key, l)
                }
                None => continue,
            };
            if id == own {
                continue;
            }
            self.broadcast_config(id, key, &vbuf[..vlen]);
        }
    }

    /// #21/#56 leaf-relay: take the buffered inbound value for config channel `key` that
    /// targeted us (if any), clearing it. `None` if nothing pending or this build doesn't
    /// apply `key`. Twin of [`take_batt_offer`]; for the screen key `main` runs the bytes
    /// through `parse_default_screen` and edge-trigger-applies the result.
    ///
    /// [`take_batt_offer`]: RadioManager::take_batt_offer
    pub fn take_cfg_offer(&mut self, key: u8) -> Option<CfgOffer> {
        self.cfg.take(key)
    }

    /// #25 WLED: broadcast one WiZmote button over ESP-NOW on the CURRENT channel.
    /// Mirror of [`broadcast_batt`]: build the fixed 13-B frame → `send_to(&
    /// BROADCAST_ADDRESS, ..)` (fire-and-forget, exactly like BATT/TIME; the
    /// underlying `esp_now.send` result is already discarded). `wled_seq` wraps
    /// (reboot-to-0 panic-safe). The WLED controller acts only on frames from its
    /// linked-remote MAC, so this broadcast is harmless to other boards.
    #[cfg(feature = "wled")]
    pub fn broadcast_wled_button(&mut self, btn: crate::net::wled::WledButton, bat_level: u8) {
        let frame = crate::net::wled::encode_wizmote(btn, self.wled_seq, bat_level);
        self.wled_seq = self.wled_seq.wrapping_add(1);
        self.send_to(&BROADCAST_ADDRESS, &frame);
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

    // =====================================================================
    // #57 Mesh Familiar — the always-on creature, ticked from main's background
    // block. This module buffers/broadcasts; the LIVING happens in `FamState`.
    // =====================================================================

    /// Broadcast a FAM frame (heartbeat / handoff / call) — mirror of
    /// [`broadcast_snk`]: encode into a stack buffer, fire-and-forget to the
    /// broadcast address. `pub` so the Familiar plugin's CALL path can send too.
    pub fn broadcast_fam(&mut self, f: &FamFrame) {
        let mut msg = [0u8; FAM_FRAME_LEN];
        if let Some(len) = encode_fam(f, &mut msg) {
            self.send_to(&BROADCAST_ADDRESS, &msg[..len]);
        }
    }

    /// #57 the ALWAYS-ON familiar tick (called every main loop, all screens): drain
    /// inbound FAM frames into the state machine, then run the holder / heartbeat /
    /// arbitration / migration / orphan-takeover step and broadcast any resulting
    /// frame. The pet keeps living regardless of which screen is on top.
    pub fn fam_tick(&mut self, now_ms: u64, unix_now: u32) {
        // Ingest everything `service()` buffered this subtick (+ its RX RSSI).
        while let Some((f, rssi)) = self.fam_inbox.pop() {
            self.fam.ingest(&f, rssi, now_ms, unix_now);
        }
        // Elect / beat / migrate from the LIVE roster (RSSI-desc). `view` returns an
        // owned `Copy` snapshot, so `fam` + `broadcast_fam` borrow cleanly after it.
        let roster = self.roster.view(now_ms);
        if let Some(frame) = self.fam.tick(&roster, now_ms, unix_now) {
            self.broadcast_fam(&frame);
        }
    }

    /// #57 a `Copy` render snapshot for the Familiar plugin (no live borrow).
    pub fn fam_view(&self, now_ms: u64) -> crate::familiar::FamView {
        self.fam.view(now_ms)
    }

    /// #57 are WE the current holder? (holder → FEED on tap; else → CALL.)
    pub fn fam_is_holder(&self) -> bool {
        self.fam.is_holder()
    }

    /// #57 FEED the creature (holder BOOT tap) — resets hunger + a happy wiggle
    /// propagates on the next heartbeat.
    pub fn fam_feed(&mut self, unix_now: u32) {
        self.fam.feed(unix_now);
    }

    /// #57 build a CALL frame (non-holder BOOT tap) — the plugin broadcasts it to
    /// bias the holder's wander toward this node. `None` if no familiar is known.
    pub fn fam_call_frame(&self) -> Option<FamFrame> {
        self.fam.call_frame()
    }

    /// Take the freshest buffered peer TIME offer, clearing it so a later call
    /// only sees offers that arrive afterward. Returns `(peer_unix,
    /// peer_synced_at, peer_id)`; `main` decides via `should_adopt` whether to
    /// re-anchor, and records `peer_id` as the adoption source (Bench own-status).
    pub fn take_time_offer(&mut self) -> Option<(u32, u32, u8)> {
        if self.time.have {
            self.time.have = false;
            Some((self.time.best_unix, self.time.best_synced_at, self.time.best_id))
        } else {
            None
        }
    }

    /// Snapshot the per-peer roster for the Bench mesh-view (issue #8): fresh
    /// peers, strongest-RSSI first, as a `Copy` view (no live borrow). Read-only
    /// w.r.t. the LED (which still reads only `PeerTracker`).
    pub fn roster(&self, now: u64) -> RosterView {
        self.roster.view(now)
    }

    /// Our own relay role, decided at boot (associated to an AP → gateway, else
    /// leaf). Bench shows this as `GATE`/`LEAF` on its own-status line — own role
    /// only, since a peer's role is never on the wire (bench-mesh-view-spec §3).
    pub fn is_gateway(&self) -> bool {
        self.relay.is_gateway
    }

    /// #27/#29: this node's current ESP-NOW channel for the peers + MC publish. Leaf = its
    /// locked scan channel (`CANDIDATES[scan_idx]` while `scan_locked`), else 0 (scanning);
    /// gateway = its `learned_channel` (#29 — the channel `rx_control` last saw a frame on;
    /// 0 until the first frame). `0` ⇒ advisory-only: HA/leaves treat the `<ch>` field as absent
    /// and fall back (no roam-flag / HELLO-scan), so this never ships a false positive.
    fn current_channel(&self) -> u8 {
        const CANDIDATES: [u8; 3] = [1, 6, 11]; // must match the scan plan above
        if self.relay.is_gateway {
            self.learned_channel // #29: real gateway channel from rx_control (0 until learned)
        } else if !self.scan_locked {
            0
        } else {
            CANDIDATES[self.scan_idx % CANDIDATES.len()]
        }
    }

    /// #27: serialize the #8 roster into `out` as the retained `smol/<id>/peers`
    /// payload — `PEERS|<role>|<ch>|id,rssi,age,ch,flags;…` — and return the byte
    /// length. GATEWAY-primary (the hub hears every leaf + flushes ~30 s); a leaf
    /// passes an empty slice in v1. Total/panic-free: a bounded [`SliceWriter`]
    /// (writes truncate, never panic) + integer formatting only — no
    /// `unwrap`/index-on-external/alloc. Peers emit in the roster's RSSI-desc order,
    /// so a full buffer keeps the STRONGEST; a peer that wouldn't fit is dropped at a
    /// clean `;` boundary and the dropped count is logged (no silent truncation —
    /// per the no-silent-cap rule). Only `id_known` peers emit (HA needs an id to
    /// place a node). `flags` = bit0 `connected` | bit1 `has_mesh_time`.
    pub fn serialize_peers(&self, now: u64, out: &mut [u8]) -> usize {
        use core::fmt::Write as _;
        let view = self.roster(now);
        let ch = self.current_channel();
        let role = if self.relay.is_gateway { 'G' } else { 'L' };
        let mut w = SliceWriter { buf: out, len: 0, overflow: false };
        let _ = write!(w, "PEERS|{}|{}|", role, ch);
        let mut dropped = 0u32;
        let mut emitted = 0usize;
        for i in 0..view.count {
            let p = view.nodes[i];
            if !p.id_known {
                continue; // no id ⇒ HA can't place it; skip (never emit an unknown id)
            }
            let flags: u8 = (p.connected as u8) | ((p.has_mesh_time as u8) << 1);
            let sep = if emitted == 0 { "" } else { ";" };
            let start = w.len;
            let _ = write!(w, "{}{},{},{},{},{}", sep, p.id, p.rssi, p.age_s, ch, flags);
            if w.overflow {
                // Roll the partial peer back → clean boundary; a later (differently
                // sized) peer may still fit, so keep scanning rather than break.
                w.len = start;
                w.overflow = false;
                dropped += 1;
            } else {
                emitted += 1;
            }
        }
        if dropped > 0 {
            log::info!("smol: #27 peers publish truncated — dropped {} weakest peer(s)", dropped);
        }
        w.len
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
        // M-1 (oracle MEDIUM): an EMPTY queue STILL flushes on the interval so the
        // gateway's retained MC record keeps refreshing (seq++). Without this a
        // quiet-but-alive lower-id gateway (no leaf traffic) stops bumping seq → looks
        // STALE to a re-electing leaf → gets falsely taken over (reintroduced
        // split-brain). HELLO (~2 s) stays the primary liveness beacon; this only stops
        // the MC record from FALSELY freezing. In coexist the gateway is already
        // associated, so an idle flush is a sub-second MQTT session (soak-proven, no
        // mesh loss) and also heartbeats the gateway's own telemetry to HA. Interval
        // 30 s vs MC_STALE_MS 90 s = a 3x liveness margin.
        if pending == 0 {
            return self.relay.last_flush_ms == 0
                || now.saturating_sub(self.relay.last_flush_ms) >= RELAY_FLUSH_INTERVAL_MS;
        }
        // "Queue full → flush now" applies only when NOT in a failing streak
        // (finding 1a): once flushes are failing, the interval backoff must govern
        // even a full queue, or a dead AP causes back-to-back bursts again.
        // `last_flush_ms == 0` is the "never flushed yet" fast path.
        (self.relay.flush_fails == 0 && pending >= GATEWAY_QUEUE)
            || self.relay.last_flush_ms == 0
            || now.saturating_sub(self.relay.last_flush_ms) >= RELAY_FLUSH_INTERVAL_MS
    }

    /// Gateway only: run an **MQTT burst** — PUBLISH each buffered leaf message +
    /// the gateway's own telemetry to `smol/<id>/telemetry` (+ retained discovery),
    /// and receive the retained `smol/display/batt` downlink into `batt` — then
    /// return to ESP-NOW ch6. This EXERCISES the `switch(Mode::WifiSta)` arm. (v2:
    /// replaces the retired UDP-to-collector egress; see `wifi::run_mqtt_burst`.)
    ///
    /// SINGLE-RADIO / SINGLE-THREAD COST (honest): this BLOCKS the main loop for
    /// the whole burst — so the **display and button are frozen and the mesh is
    /// deaf on ch6**, not merely "mesh deaf". `tick` fast-blinks the LED throughout.
    /// The burst is hard-bounded by `RELAY_FLUSH_BUDGET` (~15 s — hardware-tuned in
    /// 652155b), so a FAILED flush (AP down) can no longer block for the 30 s NTP
    /// budget. A future non-blocking (cooperative/async) flush would remove the
    /// stall entirely; that redesign is deliberately deferred.
    ///
    /// On success the queue is cleared. On FAILURE it backs off a full flush
    /// interval (see the unconditional `last_flush_ms` below) and, past
    /// `FLUSH_FAILS_BEFORE_DROP`, sheds its oldest message so a dead-AP gateway
    /// drains and stops re-bursting. Returns whether the flush succeeded.
    /// #6 OTA: take a pending gated announce (surfaced by a boot/flush burst) for
    /// `main` to act on — the pull pattern mirroring `take_time_offer`.
    pub fn take_ota_offer(&mut self) -> Option<crate::ota::Announce> {
        self.ota_offer.take()
    }

    /// #21 node-manager: take a pending default-screen command (surfaced by a
    /// boot/flush burst) for `main` to apply — the pull pattern mirroring the others.
    pub fn take_config_offer(&mut self) -> Option<crate::app::DefaultScreen> {
        self.config_offer.take()
    }

    /// #33 HA Update entity: take (and clear) the one-shot install-command flag. `main`
    /// AND-gates the OTA fetch on this, so the native Install button is the sole trigger
    /// (unless `ota::OTA_AUTO_INSTALL` is set).
    pub fn take_install_request(&mut self) -> bool {
        core::mem::take(&mut self.install_requested)
    }

    /// #6 OTA: run the update burst for a gated announce — re-associate, stream the
    /// image to the inactive slot, verify, activate + reboot. Returns only on a
    /// non-activating outcome (a SUCCESS reboots inside the fetch). Mesh-deaf for the
    /// whole download (spec §6-R4) — driven by the caller's responsive/abortable tick.
    pub fn run_ota_update(
        &mut self,
        announce: &crate::ota::Announce,
        tick: &mut dyn FnMut() -> bool,
    ) -> bool {
        log::info!("smol OTA: opening update burst (mesh deaf for the whole download)");
        let _ = self.switch(Mode::WifiSta);
        let rng = self.rng;
        match self.sta.as_mut() {
            None => false,
            Some(sta) => {
                // Self-OTA: activate-on-success (relay_mode = false; the slot out-param is
                // unused since a successful self-fetch reboots inside `activate`).
                crate::net::wifi::run_ota_fetch(
                    &mut self.controller, sta, rng, announce, tick, false, &mut None,
                )
            }
        }
    }

    /// #40: record a leaf-OTA attempt's outcome (called by `main` after `run_leaf_ota_relay`,
    /// or with `MacUnknown` when the MAC wasn't in the roster). Decides whether to CLEAR the
    /// retained install (terminal — the leaf installed or rolled back — or the transient-retry
    /// cap is hit) vs LEAVE it retained to retry (mac-unknown / fetch / relay / timeout). The
    /// phase is published to `smol/<leaf>/ota/diag` on the next burst (headless observability).
    pub fn record_leaf_ota(&mut self, leaf_id: u8, outcome: crate::ota_mesh::LeafOtaOutcome) {
        let clear = if outcome.is_terminal() {
            self.leaf_ota_retries = 0;
            true
        } else {
            self.leaf_ota_retries = self.leaf_ota_retries.saturating_add(1);
            let exhausted = self.leaf_ota_retries >= LEAF_OTA_MAX_RETRIES;
            if exhausted {
                self.leaf_ota_retries = 0;
            }
            exhausted
        };
        log::info!(
            "smol #40: leaf id{} OTA phase={} (clear_install={}, retries={})",
            leaf_id, outcome.as_str(), clear, self.leaf_ota_retries
        );
        self.leaf_ota_diag = Some((leaf_id, outcome.as_str(), clear));
        // #1 DECOUPLE: the relay session is done for this install iff we're clearing it
        // (terminal outcome or retry cap). While NOT cleared (transient retry) it stays
        // pending so the gateway keeps suppressing its own self-OTA until the leaf resolves.
        // #3: the sticky MAC is likewise session-scoped — drop it on the same terminal edge so
        // a FUTURE install re-learns the (possibly re-homed) leaf from a live roster hit.
        if clear {
            self.leaf_ota_pending = false;
            self.leaf_ota_mac = None;
        }
    }

    /// #40 #1: mark that a leaf-OTA relay has been armed (called by `main` when it takes an
    /// armed `leaf_ota`). Suppresses the gateway's own self-OTA until the relay resolves.
    pub fn note_leaf_ota_armed(&mut self) {
        self.leaf_ota_pending = true;
    }

    /// #40 #1: is a leaf-OTA relay pending/in-flight? `main` gates the gateway's self-OTA
    /// (`do_install`) on `!leaf_ota_pending()` so a relay is never interrupted.
    pub fn leaf_ota_pending(&self) -> bool {
        self.leaf_ota_pending
    }

    /// #3: is ANY leaf still holding a retained OTA install, as of the last TRUSTED gateway flush?
    /// `main` AND-gates the gateway's self-OTA on `!leaf_installs_outstanding()` too — not just the
    /// per-session `!leaf_ota_pending()` — so the gateway can't self-OTA in the gap between one
    /// leaf's terminal `record_leaf_ota` (which clears `leaf_ota_pending`) and the next flush
    /// surfacing the next leaf. Goes false only when a completed flush sees zero installs, i.e.
    /// every leaf is done → the gateway updates itself LAST.
    pub fn leaf_installs_outstanding(&self) -> bool {
        self.leaf_installs_outstanding
    }

    /// #40 GATEWAY leaf-mesh-OTA orchestration (§B4). Fetch the staged image (WiFi) into
    /// THIS gateway's inactive slot → relay it over ESP-NOW to ONE leaf (windowed-NAK) →
    /// watch for the leaf's Tier-2 STAT reappearance at the NEW build. CANARY-ONE-LEAF:
    /// targets exactly `leaf_mac`; there is NO broadcast image push. Blocking + mesh-
    /// degrading for its duration (WiFi fetch then ESP-NOW relay — never concurrent, §D#1);
    /// the UI-alive `tick` runs throughout and latches a long-press ABORT. Returns the
    /// outcome for the HA `smol/<leaf>/ota/state` publish. No-op on a non-gateway.
    pub fn run_leaf_ota_relay(
        &mut self,
        leaf_id: u8,
        leaf_mac: [u8; 6],
        announce: &crate::ota::Announce,
        tick: &mut dyn FnMut() -> bool,
    ) -> crate::ota_mesh::LeafOtaOutcome {
        use crate::ota_mesh::{
            self as om, LeafOtaOutcome, CHUNK_PAYLOAD, WINDOW_BYTES, WINDOW_CHUNKS,
        };
        use esp_bootloader_esp_idf::ota::Slot;
        if !self.relay.is_gateway {
            return LeafOtaOutcome::RelayFailed;
        }
        let size = announce.size;
        let total = om::total_chunks(size);
        // Session discriminator (retry/concurrency): low 16 bits of the monotonic build.
        let session: u16 = (announce.build & 0xFFFF) as u16;
        log::info!(
            "smol #40: RELAY start → leaf id{} build {} ({} B, {} chunks)",
            leaf_id, announce.build, size, total
        );

        // --- #3b PRE-FETCH ARM (the AP-independent fix) ---------------------------------------
        // Broadcast the OTAM to the leaf WHILE IT'S STILL RECEPTIVE ON ch6 — BEFORE the WiFi fetch
        // takes the radio off-channel for minutes. At this point the leaf has only been
        // gateway-silent for the just-finished flush (~seconds), so it's hopping [1,6,11] but
        // still ONLINE (not yet the prolonged-re-election OFFLINE we saw post-fetch). The instant
        // it catches an OTAM on a ch6 dwell it (a) locks ch6 via `handle_ota_frame` and (b) arms
        // its session → `ota_leaf.is_active()` then PINS it on ch6 through the whole fetch (no
        // scan, no re-election — see leaf_scan_tick + maybe_leaf_reelect gates). Post-fetch OTAD
        // lands on the still-locked leaf. (The post-fetch wake-burst failed because it chased an
        // ALREADY-scanning/offline leaf — canary leaf_ch=0. Arming pre-fetch stops the scan before
        // it starts.) Bounded; breaks early once the leaf's LDBG shows armed (verdict=1) or it NAKs.
        {
            let _ = self.switch(Mode::EspNow); // ch6 (the flush left us in WifiSta)
            let mut s = 0u16;
            while s < 40 {
                if !matches!(self.controller.is_connected(), Ok(true)) {
                    break;
                }
                let _ = self.controller.disconnect();
                s = s.saturating_add(1);
                if tick() {
                    return LeafOtaOutcome::Aborted;
                }
            }
            let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL);
            if !self.esp_now.peer_exists(&BROADCAST_ADDRESS) {
                let _ = self.esp_now.add_peer(PeerInfo {
                    interface: EspNowWifiInterface::Sta,
                    peer_address: BROADCAST_ADDRESS,
                    lmk: None,
                    channel: None,
                    encrypt: false,
                });
            }
            let mut otam_pre = [0u8; om::OTAM_FRAME_MAX];
            if let Some(pre_len) = om::encode_otam(
                leaf_id, session, announce.signed_msg(), announce.sig(), &mut otam_pre,
            ) {
                const PREARM_MS: u64 = 15_000;
                const PREARM_GAP_MS: u64 = 120;
                let deadline = now_ms() + PREARM_MS;
                let mut last = 0u64;
                while now_ms() < deadline {
                    if tick() {
                        return LeafOtaOutcome::Aborted;
                    }
                    let t = now_ms();
                    if t.saturating_sub(last) >= PREARM_GAP_MS {
                        let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL);
                        let _ = self.esp_now.send(&BROADCAST_ADDRESS, &otam_pre[..pre_len]);
                        last = t;
                    }
                    if let Some(recv) = self.esp_now.receive() {
                        if recv.info.src_address == leaf_mac {
                            let armed = matches!(
                                om::parse_ldbg(recv.data(), leaf_id),
                                Some((_, 1, _, _))
                            ) || matches!(
                                om::parse_ota_frame(recv.data()),
                                Some(om::OtaFrame::Nak { .. })
                            );
                            if armed {
                                break; // leaf armed pre-fetch → its is_active hold now carries ch6
                            }
                        }
                    }
                }
                log::info!("smol #3b: pre-fetch arm burst done (leaf id{})", leaf_id);
            }
        }

        // --- FETCH (WiFi) → stage+verify into THIS gateway's inactive slot (no activate).
        // CRITICAL: `run_leaf_ota_relay` is called IMMEDIATELY after `flush_telemetry`, which
        // leaves the radio in WifiSta. `switch()` is a NO-OP when already in-mode → it would
        // NOT issue a fresh `connect()`, but `run_ota_fetch` ASSUMES the caller's switch just
        // connect()'d (its own contract). If the flush's association has gone stale, the fetch
        // then spins its whole 5-min budget waiting for `is_connected` (no SYN, mesh-deaf) —
        // exactly the observed "no fetch + long offline". Self-OTA avoids this because it runs
        // from EspNow mode (its switch DOES connect). So force a fresh association here:
        // EspNow (drop the stale link) → WifiSta (issue a real connect), mirroring self-OTA.
        let _ = self.switch(Mode::EspNow);
        let _ = self.switch(Mode::WifiSta);
        let rng = self.rng;
        let mut staged: Option<Slot> = None;
        let fetched = match self.sta.as_mut() {
            Some(sta) => crate::net::wifi::run_ota_fetch(
                &mut self.controller, sta, rng, announce, tick, true, &mut staged,
            ),
            None => false,
        };
        let staged = if fetched { staged } else { None };
        let Some(slot) = staged else {
            log::error!("smol #40: relay FETCH/stage failed — aborting (leaf untouched)");
            let _ = self.switch(Mode::EspNow);
            return LeafOtaOutcome::FetchFailed;
        };
        let Some(reader) = crate::ota::SlotReader::open(slot) else {
            log::error!("smol #40: relay slot-open failed — aborting");
            let _ = self.switch(Mode::EspNow);
            return LeafOtaOutcome::FetchFailed;
        };

        // --- RELAY (ESP-NOW) — windowed-NAK to the ONE leaf MAC. ---
        let _ = self.switch(Mode::EspNow);
        // #28: ensure the relay target is registered even when the HW peer table is full — evict
        // the least-valuable peer to make room (never this target: it is both `adding` and the
        // protected `leaf_ota_mac` sticky, nor the broadcast peer).
        self.ensure_peer(leaf_mac, now_ms());
        // #3b: DEFENSIVE — (re)add the broadcast peer in the relay ESP-NOW context. Normal HELLOs
        // egress without an explicit broadcast peer, but this send follows a WiFi fetch
        // (WifiSta→switch(EspNow)) which may have perturbed the peer table / TX state; a missing
        // broadcast peer makes `esp_now_send(ff:ff:…)` return NOT_FOUND → the OTAM never egresses
        // (candidate cause of leaf H0 with the gateway on-channel). The `otam_tx/otam_ok` diag
        // below proves whether the send now succeeds.
        if !self.esp_now.peer_exists(&BROADCAST_ADDRESS) {
            let _ = self.esp_now.add_peer(PeerInfo {
                interface: EspNowWifiInterface::Sta,
                peer_address: BROADCAST_ADDRESS,
                lmk: None,
                channel: None,
                encrypt: false,
            });
        }
        // #3b CHANNEL FIX (the real one — canary 7708b20 proved otam_tx=17/17 egress OK, yet leaf
        // H0 with rx collapsed 10→2): the OTAM fires right after the WiFi FETCH. `switch(EspNow)`
        // above issued an ASYNC `disconnect()` + `set_channel(ch6)`, but while the STA is still
        // tearing down it HOLDS the AP's channel → the OTAM broadcast egresses on the AP channel,
        // not ch6, so the ch6 leaf never hears it. SPIN until the STA truly releases the PHY, THEN
        // pin ch6 (and re-pin right before each OTAM send below). `settle` (published) = how long
        // the STA held on — settle>0 confirms this WAS the off-channel egress.
        let mut settle: u16 = 0;
        while settle < 40 {
            if !matches!(self.controller.is_connected(), Ok(true)) {
                break;
            }
            let _ = self.controller.disconnect();
            settle = settle.saturating_add(1);
            if tick() {
                return LeafOtaOutcome::Aborted;
            }
        }
        let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL);
        let gwbuf = unsafe { &mut *core::ptr::addr_of_mut!(GW_OTA_WINDOW) };
        let mut otam = [0u8; om::OTAM_FRAME_MAX];
        let Some(otam_len) =
            om::encode_otam(leaf_id, session, announce.signed_msg(), announce.sig(), &mut otam)
        else {
            return LeafOtaOutcome::RelayFailed;
        };
        let mut otad = [0u8; om::OTAD_FRAME_MAX];

        // #3 RX-DIAG counters: rx_any = frames heard FROM this leaf during the relay windows;
        // otan_valid = valid advancing/NAK OTANs for the live session. Plus the leaf's own LDBG
        // self-report (heard/verdict/sent), captured live from its beacon; verdict 255 = none seen.
        // All stored into `leaf_relay_rx` at each terminal exit → published next flush (relaydiag).
        let mut rx_any: u16 = 0;
        let mut otan_valid: u16 = 0;
        let mut ldbg_heard: u16 = 0;
        let mut ldbg_verdict: u8 = 255;
        let mut ldbg_sent: u16 = 0;
        let mut ldbg_ch: u8 = 0; // #3b: leaf's channel at beacon time (0=scanning, 6=locked ch6)
        // #3b OTAM TX-diag: broadcast sends attempted vs returned-Ok (queued + TX-callback ok).
        let mut otam_tx: u16 = 0;
        let mut otam_ok: u16 = 0;

        // #3b WAKE-BURST: the leaf lost the gateway during the off-ch6 fetch and is now hopping
        // [1,6,11] (on ch6 only ~⅓ of the time) and/or re-electing over WiFi — so a single OTAM
        // per window-0 round (spaced by the 800 ms OTAN wait) rarely coincides with the leaf's
        // brief ch6 dwell (canary: leaf H0). OPEN the relay with a DENSE OTAM flood: rebroadcast
        // the announce every ~120 ms for up to WAKE_MS, maximizing overlap with the leaf's sparse
        // ch6 windows. The first OTAM the leaf catches locks it to ch6 (`handle_ota_frame`) and
        // arms its session → it then holds ch6 (`leaf_scan_tick` is_active) for the transfer. Stop
        // early the instant the leaf NAKs (it's armed + ready for the windowed transfer).
        // #3b: this post-fetch burst is now a short FALLBACK + diag capture — the PRIMARY arming
        // happens PRE-fetch (above), so a leaf that pre-armed is already locked+active here and
        // this just backstops the case where pre-arm missed. Kept short (was 90s) since a leaf
        // that didn't pre-arm has drifted off-ch6 for the whole fetch and a long post-fetch flood
        // was proven dead (canary leaf_ch=0). Self-stops on first NAK. Also captures the leaf's
        // LDBG (leaf_ch/verdict) for relaydiag. Real coexist cure stays infra (ch6 fetch AP, JP).
        const WAKE_MS: u64 = 15_000;
        const WAKE_GAP_MS: u64 = 120;
        let wake_deadline = now_ms() + WAKE_MS;
        let mut last_wake_ms = 0u64;
        while now_ms() < wake_deadline {
            if tick() {
                return LeafOtaOutcome::Aborted;
            }
            let t = now_ms();
            if t.saturating_sub(last_wake_ms) >= WAKE_GAP_MS {
                let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL);
                otam_tx = otam_tx.saturating_add(1);
                if let Ok(waiter) = self.esp_now.send(&BROADCAST_ADDRESS, &otam[..otam_len]) {
                    if waiter.wait().is_ok() {
                        otam_ok = otam_ok.saturating_add(1);
                    }
                }
                last_wake_ms = t;
            }
            if let Some(recv) = self.esp_now.receive() {
                if recv.info.src_address == leaf_mac {
                    rx_any = rx_any.saturating_add(1);
                    if let Some((h, v, n, c)) = om::parse_ldbg(recv.data(), leaf_id) {
                        ldbg_heard = h;
                        ldbg_verdict = v;
                        ldbg_sent = n;
                        ldbg_ch = c;
                    }
                    if matches!(
                        om::parse_ota_frame(recv.data()),
                        Some(om::OtaFrame::Nak { .. })
                    ) {
                        break; // leaf armed + NAKing → proceed to the windowed transfer
                    }
                }
            }
        }

        let mut wb: u32 = 0;
        'windows: while wb < total {
            if tick() {
                return LeafOtaOutcome::Aborted;
            }
            let wlen_chunks = core::cmp::min(WINDOW_CHUNKS as u32, total - wb);
            let window_off = wb * CHUNK_PAYLOAD as u32;
            let window_bytes = core::cmp::min(WINDOW_BYTES as u32, size - window_off) as usize;
            // Read this window from the staged slot (word-aligned length; extra padding
            // bytes past `window_bytes` are read but never sent).
            let read_len = ((window_bytes as u32).div_ceil(4) * 4) as usize;
            if !reader.read(window_off, &mut gwbuf[..read_len]) {
                log::error!("smol #40: slot readback failed at window {} — aborting", wb);
                self.leaf_relay_rx = Some(crate::net::wifi::RelayDiag {
                    leaf_id, rx_any, otan_valid, last_wb: wb as u16, total: total as u16,
                    leaf_heard: ldbg_heard, leaf_verdict: ldbg_verdict, leaf_sent: ldbg_sent,
                    otam_tx, otam_ok,
                    settle,
                    leaf_ch: ldbg_ch,
                });
                let _ = self.switch(Mode::EspNow);
                return LeafOtaOutcome::RelayFailed;
            }
            let full = om::window_full_mask(wlen_chunks);
            let mut missing = full; // first pass: send every chunk in the window
            let mut rounds: u32 = 0;
            loop {
                if tick() {
                    return LeafOtaOutcome::Aborted;
                }
                // (Re)send OTAM during window 0 so a leaf that missed it can still arm.
                // #3b: BROADCAST the announce (not unicast to leaf_mac). Root cause of the
                // a949574 canary's `leaf=H0V0N0` (leaf hears ZERO OTAMs while rx=10 proves the
                // reverse path works): ESP-NOW unicast RX requires the SENDER be a registered
                // peer on the receiver, but the leaf only adds the gateway as a peer WHEN it
                // receives an OTA frame (handle_ota_frame) or a gateway HELLO — and the gateway
                // is HELLO-silent during the mesh-deaf relay → bootstrap deadlock. A BROADCAST
                // needs no peer (that's why HELLOs land), so the leaf receives it, arms, and
                // adds the gateway peer → the subsequent UNICAST OTAD/OTAN then flow to the
                // (correct, roster-learned) MACs. Design §B: only the small ANNOUNCE broadcasts;
                // the image (OTAD) stays unicast — "no broadcast image push" preserved. The
                // `target` id in the frame keeps canary-one-leaf semantics (only leaf_id arms).
                if wb == 0 {
                    // #3b: re-pin ch6 right before the announce (self-healing if the post-fetch
                    // STA teardown released the channel late), then capture the send Result
                    // (send_to swallows it) to prove egress.
                    let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL);
                    otam_tx = otam_tx.saturating_add(1);
                    if let Ok(waiter) = self.esp_now.send(&BROADCAST_ADDRESS, &otam[..otam_len]) {
                        if waiter.wait().is_ok() {
                            otam_ok = otam_ok.saturating_add(1);
                        }
                    }
                }
                for i in 0..wlen_chunks as usize {
                    if (missing >> i) & 1 == 1 {
                        let seq = wb + i as u32;
                        let coff = i * CHUNK_PAYLOAD;
                        let clen = core::cmp::min(CHUNK_PAYLOAD, window_bytes - coff);
                        let n = om::encode_otad(
                            leaf_id, session, seq as u16, &gwbuf[coff..coff + clen], &mut otad,
                        );
                        self.send_to(&leaf_mac, &otad[..n]);
                    }
                }
                // Wait for this window's OTAN (all-zero = advance; else the missing set).
                let deadline = now_ms() + GW_OTAN_WAIT_MS;
                let mut advanced = false;
                let mut got_nak = false;
                while now_ms() < deadline {
                    if let Some(recv) = self.esp_now.receive() {
                        if recv.info.src_address == leaf_mac {
                            rx_any = rx_any.saturating_add(1); // #3: heard SOMETHING from the leaf
                            // #3: capture the leaf's LDBG self-report (latest wins) — names WHY
                            // otan=0 without needing the leaf back online post-relay.
                            if let Some((h, v, n, c)) = om::parse_ldbg(recv.data(), leaf_id) {
                                ldbg_heard = h;
                                ldbg_verdict = v;
                                ldbg_sent = n;
                                ldbg_ch = c;
                            }
                            if let Some(om::OtaFrame::Nak { origin, session: s, window_base, bitmap }) =
                                om::parse_ota_frame(recv.data())
                            {
                                if origin == leaf_id && s == session && window_base as u32 == wb {
                                    otan_valid = otan_valid.saturating_add(1); // #3: valid OTAN
                                    let miss = om::bitmap_to_u64(bitmap) & full;
                                    if miss == 0 {
                                        advanced = true;
                                    } else {
                                        missing = miss;
                                        got_nak = true;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
                if advanced {
                    wb += WINDOW_CHUNKS as u32;
                    break;
                }
                if !got_nak {
                    missing = full; // no NAK in the wait → resend the whole window
                }
                rounds += 1;
                if rounds > GW_WINDOW_ROUNDS_MAX {
                    // #3b TAIL FIX: the LAST window never gets an advance-ack — the leaf
                    // finalizes+Completes it (ota_mesh on_data sends the all-zero ack ONLY for
                    // non-last windows, then reboots into the new image). So the gateway CANNOT
                    // hear an advance for the last window; "exhaustion" here is EXPECTED, not a
                    // failure. The blind full-window resends across these rounds delivered the
                    // last chunks (98.8%→100% at the tail); the leaf's real completion signal is
                    // its REBOOT (build flip), which the CONFIRM phase below detects. So fall
                    // THROUGH to confirm — returning RelayFailed here failed even a perfect
                    // transfer (canary 7ee3982: 4288/4341, last window, leaf never confirmed).
                    if wb + WINDOW_CHUNKS as u32 >= total {
                        log::info!(
                            "smol #40: last window {} sent {} rounds (rx_any={} otan={}) — leaf finalizes w/o advance-ack → CONFIRM",
                            wb, GW_WINDOW_ROUNDS_MAX, rx_any, otan_valid
                        );
                        break 'windows;
                    }
                    log::error!(
                        "smol #40: window {} exceeded {} retransmit rounds (rx_any={} otan={}) — aborting (R2 bound)",
                        wb, GW_WINDOW_ROUNDS_MAX, rx_any, otan_valid
                    );
                    self.leaf_relay_rx = Some(crate::net::wifi::RelayDiag {
                        leaf_id, rx_any, otan_valid, last_wb: wb as u16, total: total as u16,
                        leaf_heard: ldbg_heard, leaf_verdict: ldbg_verdict, leaf_sent: ldbg_sent,
                        otam_tx, otam_ok, settle, leaf_ch: ldbg_ch,
                    });
                    return LeafOtaOutcome::RelayFailed;
                }
            }
        }
        // #3 RX-DIAG: relay TX phase COMPLETE — all chunks delivered + acked (wb==total). Record
        // the full-progress RX evidence for the relaydiag; the confirm outcome (below) is the
        // separate Tier-2 verdict published to `…/ota/diag`.
        self.leaf_relay_rx = Some(crate::net::wifi::RelayDiag {
            leaf_id, rx_any, otan_valid, last_wb: total as u16, total: total as u16,
            leaf_heard: ldbg_heard, leaf_verdict: ldbg_verdict, leaf_sent: ldbg_sent,
            otam_tx, otam_ok, settle,
            leaf_ch: ldbg_ch,
        });

        // --- CONFIRMING (Tier-2, build-matched): sample the leaf's SETTLED build over the
        // full confirm window. The leaf runs the NEW image immediately post-activate, so it
        // STATs the new build even mid-self-test; a FAILED self-test then rolls it back to an
        // OLDER build (and STATs that). We must NOT early-out:
        //   • an early-out on a NEW-build sighting would miss a later rollback;
        //   • an early-out on an OLD-build sighting would false-fail on a STALE STAT the leaf
        //     broadcast at its old build DURING the (minutes-long) relay, still buffered in
        //     the 10-deep ESP-NOW RX queue.
        // So: DRAIN the stale queue first, then track the LAST-seen build across the whole
        // window (> the leaf self-test window + a STAT cadence) → the settled verdict.
        // A stale old-build STAT is overwritten by the post-reboot new-build STAT; a genuine
        // rollback's old-build STAT is the last one seen. No STAT at all → possible brick.
        // This is the PROGRESSION gate, not the safety gate (the leaf's local Tier-1 protects
        // it); a wrong verdict at worst mis-labels the HA card, never bricks/compromises.
        log::info!(
            "smol #40: all {} chunks relayed — awaiting leaf id{} Tier-2 confirm at build {}",
            total, leaf_id, announce.build
        );
        // Discard any frames buffered during the relay (incl. the leaf's stale old-build STAT).
        for _ in 0..64 {
            if self.esp_now.receive().is_none() {
                break;
            }
        }
        let cdeadline = now_ms() + GW_CONFIRM_TIMEOUT_MS;
        let mut last_build: Option<u32> = None;
        // #40 IDENTITY GUARD: a logical id STATed by the TARGET MAC that isn't `leaf_id`.
        // With the runtime-NVS identity fix this stays None; if it fires, the image booted
        // with a stolen/wrong id → an explicit `id-mismatch` beats a silent leaf-timeout.
        let mut mac_seen_id: Option<u8> = None;
        let mut last_hello_ms = 0u64;
        while now_ms() < cdeadline {
            if tick() {
                return LeafOtaOutcome::Aborted;
            }
            // #3b REJOIN: HELLO on ch6 (~1 Hz) throughout the confirm window. The leaf just
            // activated + REBOOTED into the new image and must HEAR A VALID FRAME to pass its
            // hear-a-frame self-test (else it rolls back off the new build) AND lock onto us to
            // STAT its new build (the confirm signal). The old confirm loop was SILENT — the
            // rebooted leaf heard nothing → self-test never passed → it rolled back / never
            // rejoined, so a SUCCESSFUL delivery (image activated + booted) looked like a failure
            // (canary: leaf booted 126 from ota_1 but HA never saw a 126 STAT). Stay pinned ch6.
            let t = now_ms();
            if t.saturating_sub(last_hello_ms) >= 1000 {
                let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL);
                self.broadcast_hello();
                last_hello_ms = t;
            }
            if let Some(recv) = self.esp_now.receive() {
                // Copy the source MAC out FIRST (Copy) so the `recv.data()` borrow in the
                // parse below doesn't collide with the identity-guard MAC compare.
                let src_mac = recv.info.src_address;
                if let Some(Frame::Stat { src, value }) = parse_frame(recv.data()) {
                    if src == leaf_id {
                        if let Some(b) = om::stat_build(value) {
                            last_build = Some(b); // keep the latest — the settled state wins
                        }
                    } else if src_mac == leaf_mac {
                        // Our target board (matched by sticky MAC) is STATing a DIFFERENT id.
                        mac_seen_id = Some(src);
                    }
                }
            }
        }
        match last_build {
            Some(b) if b >= announce.build => {
                log::info!("smol #40: leaf id{} CONFIRMED at build {} — update stuck", leaf_id, b);
                LeafOtaOutcome::Confirmed
            }
            Some(b) => {
                log::warn!(
                    "smol #40: leaf id{} settled at OLD build {} — self-test rolled it back (HA re-offers)",
                    leaf_id, b
                );
                LeafOtaOutcome::RolledBack
            }
            None => {
                if let Some(wrong) = mac_seen_id {
                    // The physical board booted + STATs, but under the WRONG id — a stolen
                    // baked-default identity (NVS not seeded / shared image on a fresh board).
                    // Explicit `id-mismatch` (terminal) instead of a mystery leaf-timeout.
                    log::error!(
                        "smol #40: leaf id{} target MAC now STATs as id{} — IDENTITY MISMATCH (stolen baked id / NVS unseeded), not a brick",
                        leaf_id, wrong
                    );
                    LeafOtaOutcome::IdMismatch
                } else {
                    log::error!(
                        "smol #40: leaf id{} did not reappear at build {} within the confirm window — possible brick (USB recovery)",
                        leaf_id, announce.build
                    );
                    LeafOtaOutcome::Timeout
                }
            }
        }
    }

    pub fn flush_telemetry(
        &mut self,
        own_telemetry: &[u8],
        // #50: the gateway's live `STAT|<screen>:<page>` (main passes it from
        // `App::live_screen`) → forwarded to run_mqtt_burst → retained `smol/<id>/status`.
        status: &[u8],
        batt: &mut crate::batt::BattCache,
        grid: &mut crate::grid::GridCache,
        // #40: filled with `(leaf_id, staged announce)` if this flush surfaced a leaf OTA
        // install → `main` then calls `run_leaf_ota_relay` for that leaf. `None` otherwise.
        leaf_ota: &mut Option<(u8, crate::ota::Announce)>,
        tick: &mut dyn FnMut() -> bool,
    ) -> bool {
        if !self.relay.is_gateway {
            return false;
        }
        log::info!("smol: relay flush -> MQTT burst (mesh deaf on ch6 during it)");
        // Resurrected COEXIST arm: re-associate to the AP (retunes the PHY off
        // ch6). run_mqtt_burst waits for the association, so we only trigger it.
        let _ = self.switch(Mode::WifiSta);
        let id = self.id;
        let now = now_ms();
        // #27: serialize the roster into a stack buffer BEFORE the mutable-borrow burst
        // — the resulting slice is disjoint from `self` (same disjoint-borrow discipline
        // as `own_telemetry`), so it threads through with no borrow conflict. Worst case
        // (ROSTER_CAP=16 peers) ≈ 298 B; 320 matches the MqttScratch budget from #33.
        let mut peers_buf = [0u8; 320];
        let peers_len = self.serialize_peers(now, &mut peers_buf);
        let peers = &peers_buf[..peers_len];
        // #23 fix: seed the election from the live role + persistent staleness so THIS
        // flush RE-DECIDES ownership (demote a duplicate gateway that now sees a LIVE
        // lower-id owner — oracle #2). Read back below and applied to the live role.
        let mut elect = crate::net::wifi::MeshElect::new(id);
        elect.now_ms = now;
        elect.seen_owner = self.mc_seen_owner;
        elect.seen_seq = self.mc_seen_seq;
        elect.seen_ms = self.mc_seen_ms;
        // #64: carry the last-good RSSI-to-AP into the burst so the gateway PUBLISHES its
        // WiFi-uplink signal. Captured post-flush below (#51 B) → this is the previous
        // good flush's reading (~one heartbeat of lag, fine for a display value); -99
        // until the first good flush, which mqtt_session's publish guard skips.
        elect.my_rssi = self.my_rssi_to_ap;
        elect.my_channel = self.learned_channel; // #29: seed the MC record's <ch> (0 until learned)
        // #6 OTA: capture any gated retained announce this flush surfaces.
        let mut ota_offer: Option<crate::ota::Announce> = None;
        // #21: capture any default-screen config this flush surfaces.
        let mut config_offer: Option<crate::app::DefaultScreen> = None;
        // #48: capture the GATEWAY's OWN keyed configs (led/…) this flush surfaces, then inject
        // them into our own CfgTracker below so `main`'s take_cfg_offer(key) self-applies them.
        let mut gw_own = crate::net::wifi::GwOwnCfg::new();
        // #52: capture any remote-reboot COMMANDS this flush surfaces (transient cmd/reset). Drained
        // below into a ONE-SHOT relay per leaf target (NEVER cached) + a self-reboot inject if own.
        let mut reset_req = crate::net::wifi::ResetReq::new();
        // #71: capture any on-demand WiFi-scan COMMANDS this flush surfaces (transient cmd/scan).
        // Drained below like reset_req: ONE-SHOT `<id>W` relay per leaf target + a self-scan inject.
        let mut scan_req = crate::net::wifi::ScanReq::new();
        // #33: capture any OTA install command this flush surfaces.
        let mut install_requested = false;
        // #40 #1: set iff this flush SEES a retained leaf install (pre-arm) → latch pending below.
        let mut leaf_install_seen = false;
        // #70/#49: build the gateway's OWN DIAG record BEFORE the burst borrow (it takes `&mut self`
        // — the run_mqtt_burst call below already holds disjoint field borrows). Published retained
        // as `smol/<id>/diag`, and cached leaf diags are republished alongside via `diag_cache`.
        let diag_rec = self.diag_record();
        // #71: take the gateway's pending own-scan record (set by `run_scan` when the gateway
        // self-scanned) — one-shot: publish it THIS flush (retained holds it), then it's cleared.
        let own_scan = self.own_scan.take();
        let scan_bytes: &[u8] = own_scan.as_deref().map(|s| s.as_bytes()).unwrap_or(&[]);
        let sta = self.sta.as_mut();
        let ok = match sta {
            None => false,
            Some(sta) => {
                // (src_id, &payload) list to PUBLISH: the gateway's OWN telemetry
                // first (spec: "also PUBLISH its own telemetry"), then each queued
                // leaf message. Disjoint borrows: `own_telemetry` is the caller's;
                // the queue slices are `&self.relay`; `&mut self.controller`/`*sta`
                // are other fields. `+ 1` slot holds the gateway's own line.
                let empty: &[u8] = &[];
                let mut items: [(u8, &[u8]); GATEWAY_QUEUE + 1] =
                    [(0u8, empty); GATEWAY_QUEUE + 1];
                let mut n = 0;
                if !own_telemetry.is_empty() {
                    items[n] = (id, own_telemetry);
                    n += 1;
                }
                for q in self.relay.queue.iter() {
                    if q.used {
                        items[n] = (q.src_id, &q.buf[..q.len]);
                        n += 1;
                    }
                }
                crate::net::wifi::run_mqtt_burst(
                    &mut self.controller,
                    sta,
                    self.rng,
                    id,
                    &items[..n],
                    batt,
                    grid,
                    &mut elect,
                    &mut ota_offer,
                    &mut config_offer,
                    &mut gw_own, // #48: capture the gateway's own keyed configs
                    &mut reset_req, // #52: capture remote-reboot commands (one-shot relay below)
                    &mut install_requested,
                    &mut leaf_install_seen, // #40 #1: latch pending on install-SEEN (below)
                    peers, // #27: gateway publishes its roster as retained smol/<id>/peers
                    status, // #50: gateway publishes its live screen as smol/<id>/status
                    Some(&mut self.cfg_cache), // #21: gateway caches leaf configs to relay
                    Some(&self.stat_cache), // #50b: gateway republishes cached leaf statuses
                    diag_rec.as_bytes(), // #70/#49: gateway publishes its own smol/<id>/diag
                    Some(&self.diag_cache), // #70/#49: republish cached relayed-node diags
                    scan_bytes, // #71: gateway publishes its own smol/<id>/scan (empty unless it self-scanned)
                    Some(&self.scan_cache), // #71: republish cached relayed-node scans
                    &mut scan_req, // #71: capture on-demand scan commands (one-shot relay below)
                    leaf_ota, // #40: surface a pending leaf-OTA install for main to relay
                    &mut self.staged_raw, // #40: persist the staged across flushes (pair-safe)
                    &mut self.leaf_ota_diag, // #40: publish smol/<leaf>/ota/diag + clear/retry
                    &mut self.leaf_relay_rx, // #3: publish smol/<leaf>/ota/relaydiag (RX evidence)
                    tick,
                )
            }
        };
        // #49: record the flush outcome (`ok` = reached CONNACK) into the diag counters — the
        // flush-success rate is the on-device #9 flush-win proof (was UART0-only). Bumped here so
        // it rides the NEXT diag record.
        self.note_flush(ok);
        // #6 OTA: stash any announce this flush surfaced for `main` to act on.
        if ota_offer.is_some() {
            self.ota_offer = ota_offer;
        }
        // #21: stash any default-screen config this flush surfaced for `main`.
        if config_offer.is_some() {
            self.config_offer = config_offer;
        }
        // #48 GwOwnCfg: inject the gateway's OWN keyed configs into our (gateway-idle) CfgTracker
        // so `main`'s take_cfg_offer(key) self-applies them — same path as a leaf's relayed config.
        // `CfgTracker::set` key-filters (only CFG_APPLY_KEYS land), so a stray key is a no-op.
        if let Some((buf, len)) = gw_own.led {
            self.cfg.set(crate::net::wifi::CFG_KEY_LED, &buf[..len]);
        }
        // #43: the gateway's OWN global display units — same self-apply path (take_cfg_offer(U)).
        if let Some((buf, len)) = gw_own.units {
            self.cfg.set(crate::net::wifi::CFG_KEY_UNITS, &buf[..len]);
        }
        // #55: the gateway's OWN plugin-visibility mask — same self-apply path (take_cfg_offer(P)).
        if let Some((buf, len)) = gw_own.plugins {
            self.cfg.set(crate::net::wifi::CFG_KEY_PLUGINS, &buf[..len]);
        }
        // #45: the gateway's OWN custom-screen layout — same self-apply path (take_cfg_offer(Y)).
        if let Some((buf, len)) = gw_own.custom {
            self.cfg.set(crate::net::wifi::CFG_KEY_CUSTOM, &buf[..len]);
        }
        // #52 remote reboot — a COMMAND, not a config. Fire a ONE-SHOT `<id>R` frame per queued
        // leaf target via `broadcast_config` DIRECTLY (NOT `cfg_cache` / `broadcast_cached_configs`):
        // a cached/rebroadcast reboot = a permanent ~10 s reboot-loop soft-brick — the anti-loop
        // invariant. Own id → inject R into our OWN CfgTracker so `main`'s take_cfg_offer(R) reboots
        // us on the SAME boot-debounced path as a leaf (never a raw reset here). Empty value: the
        // key IS the command. `reset_req` is a local (not `self`) → the borrow is disjoint.
        for &leaf in reset_req.targets() {
            self.broadcast_config(leaf, crate::net::wifi::CFG_KEY_REBOOT, b"");
        }
        if reset_req.own() {
            self.cfg.set(crate::net::wifi::CFG_KEY_REBOOT, b"");
        }
        // #71 on-demand scan — twin of the reset drain: ONE-SHOT `<id>W` frame per queued leaf
        // target (direct `broadcast_config`, NEVER cached — a cached scan = a periodic off-channel
        // excursion, the coexist hazard). Own id → inject W into our OWN CfgTracker so `main`'s
        // take_cfg_offer(W) runs the scan on the SAME path as a leaf. Empty value: the key IS the command.
        for &leaf in scan_req.targets() {
            self.broadcast_config(leaf, crate::net::wifi::CFG_KEY_SCAN, b"");
        }
        if scan_req.own() {
            self.cfg.set(crate::net::wifi::CFG_KEY_SCAN, b"");
        }
        // #33: OR-in an install command (one-shot; `main`'s take clears it).
        if install_requested {
            self.install_requested = true;
        }
        // #40 #1 (self-OTA-first fix): if this flush SAW a retained leaf install, latch
        // `leaf_ota_pending` NOW — on install-SEEN, before the arm (which needs the cached staged
        // image). Otherwise the gateway's own `do_install` (gated on !leaf_ota_pending) could fire
        // in the window between seeing its own install and the leaf arming → self-OTA first,
        // inverting the demo order + rebooting the gateway early. Cleared on the terminal
        // `record_leaf_ota` (retained install goes away → not seen next flush).
        if leaf_install_seen {
            self.leaf_ota_pending = true;
        }
        // #3 (self-OTA-first, multi-leaf gap): the flush is the AUTHORITY on "any leaf still has a
        // retained install." Latch it separately from the per-session `leaf_ota_pending` so the
        // gateway's self-OTA stays suppressed across the terminal→next-flush gap — after one leaf's
        // `record_leaf_ota` clears `leaf_ota_pending`, this stays true until a completed flush sees
        // NO installs (every leaf done). Only a TRUSTED flush (`ok`) updates the view: an aborted /
        // disconnected flush read no install topics, so it must NOT be taken as "no installs left."
        if ok {
            self.leaf_installs_outstanding = leaf_install_seen;
        }
        // #23 fix (oracle #2): APPLY the re-election result to the LIVE role. On a
        // connected flush, persist the refreshed staleness observation; if a LIVE
        // lower-id owner now holds the mesh, DEMOTE to a scanning leaf (drop to ESP-NOW)
        // — the runtime convergence the boot-only election never had. A NON-connected
        // flush (transient broker outage) is NOT trusted → keep the current role.
        if ok {
            // #64 fix: the RSSI-to-AP is now captured DURING the burst (run_mqtt_burst, while
            // is_connected()==true) into elect.my_rssi — persist it here across bursts for the
            // leaf-reelect path. The old post-burst self.controller.rssi() ran when the STA
            // state was unreliable → returned Err → my_rssi_to_ap stuck at -99 (dead #51
            // tiebreak + no #64 uplink publish).
            if elect.my_rssi > -99 {
                self.my_rssi_to_ap = elect.my_rssi;
            }
            self.mc_seen_owner = elect.seen_owner;
            self.mc_seen_seq = elect.seen_seq;
            self.mc_seen_ms = elect.seen_ms;
            self.elected_owner = elect.owner_id;
            if !elect.i_am_owner {
                self.relay.is_gateway = false;
                self.scan_locked = false;
                self.last_owner_heard_ms = now;
                let _ = self.switch(Mode::EspNow);
                log::info!(
                    "smol: gateway DEMOTED — live owner id{} holds the mesh; leaf-scanning",
                    self.elected_owner
                );
            }
        }
        // #23 stage 1 — PRODUCTION COEXIST: STAY WiFi-associated after the flush (NO
        // ESP-NOW teardown). The gateway keeps riding its AP channel, so the flush is
        // a sub-second DHCP+MQTT on the live link with no channel-deaf window — only
        // the CPU-blocking poll loop remains (soak-proven: 0/60 flush-window RX loss).
        // FINDING 1a: stamp the backoff UNCONDITIONALLY. Previously `last_flush_ms`
        // was set only on success, so a FAILED flush left it stale →
        // `relay_ready_to_flush` re-fired instantly → back-to-back multi-second
        // blocking bursts FOREVER (display + input + mesh all frozen). Now every
        // attempt resets the clock, so a failure backs off a full
        // RELAY_FLUSH_INTERVAL_MS instead of spinning.
        self.relay.last_flush_ms = now_ms();
        if ok {
            for q in self.relay.queue.iter_mut() {
                q.used = false;
            }
            self.relay.flush_fails = 0;
            log::info!("smol: relay flush done");
        } else {
            self.relay.flush_fails = self.relay.flush_fails.saturating_add(1);
            // FINDING 1c: once failures pile up, shed the OLDEST message each time
            // so the queue drains to empty (→ relay_ready_to_flush false → the
            // bursts stop) and never holds arbitrarily-stale telemetry.
            if self.relay.flush_fails >= FLUSH_FAILS_BEFORE_DROP {
                self.relay.drop_oldest();
            }
            // R-DEMOTE (oracle audit-#1): sustained failures mean the AP is truly gone
            // (a mere roam would have self-recovered via R-CONNECT within seconds).
            // Relinquish ownership so this board's HELLO stops pinning leaves to a
            // HA-unreachable owner — drop to leaf-scan; the broker election re-promotes
            // a reachable board (or this one, once an AP returns). Guarded on the demote
            // threshold (≈2.5 min) so a transient broker blip can't flap the role.
            if self.relay.is_gateway && self.relay.flush_fails >= FLUSH_FAILS_BEFORE_DEMOTE {
                self.relay.is_gateway = false;
                self.scan_locked = false;
                self.last_owner_heard_ms = now;
                // #51 A1: this is an ABDICATION (our uplink is genuinely dead). Go
                // HELLO-silent until we re-lock a new owner — otherwise our stale HELLO
                // keeps the leaves pinned to us and no successor can ever be elected.
                self.silent_until_relock = true;
                let _ = self.switch(Mode::EspNow);
                log::warn!(
                    "smol: gateway DEMOTED after {} failed flushes — AP unreachable, HELLO-silent + leaf-scanning",
                    self.relay.flush_fails
                );
            }
            log::warn!(
                "smol: relay flush FAILED ({} consecutive) — backing off {} ms",
                self.relay.flush_fails,
                RELAY_FLUSH_INTERVAL_MS
            );
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
        // per-call drain to 24 so it absorbs TYPICAL bursts, while staying BOUNDED
        // so a pathological flood can't stall the 1 Hz clock tick or the LED.
        // (Note: the decoded-SNK inbox is SNK_RX_RING=8 vs the 10-deep HW queue, so
        // a worst-case all-SNK window can drop ≤2 SNK frames/subtick — tolerated by
        // design: per-id upsert + absolute state + 200 ms resend recover it.)
        // Each parse is a cheap prefix match.
        let mut label: Option<alloc::string::String> = None;
        for _ in 0..24 {
            let Some(recv) = self.esp_now.receive() else {
                break;
            };
            let src = recv.info.src_address;
            // #68/#76 SELF-MAC FILTER: the esp-wifi RX path can deliver our OWN broadcasts back
            // to us (HELLO/TIME/PEERS). With no guard the gateway rosters ITSELF (roster anomaly
            // #1: a constant-RSSI, age-0, flags-3 self-entry that, being always freshest, is
            // eviction-immune and permanently burns a 16-slot LRU slot — the #28 ceiling too).
            // Drop our own frames before ANY handler (OTA dispatch + generic Frame parse below).
            if src == self.self_mac {
                continue;
            }
            // RSSI of this frame (dBm) from the ESP-NOW RX control info; used by
            // BENCH. Captured up front so each arm can record it if relevant.
            let rssi = recv.info.rx_control.rssi;
            // #29: learn the channel this frame arrived on = this board's live ESP-NOW channel.
            // Advisory; the gateway publishes it in the MC record so leaves can pre-tune on roam.
            self.learned_channel = recv.info.rx_control.channel as u8;
            let now = now_ms();

            // #40 leaf-mesh-OTA transport (OTAM/OTAD/OTAN). Dispatched BEFORE the generic
            // Frame parse — the 64-B signed manifest / raw image chunk don't fit the Frame
            // enum. A LEAF drives its receive session here; a GATEWAY's relay is the
            // dedicated blocking `run_leaf_ota_relay`, so it no-ops these in this drain.
            if let Some(otaf) = crate::ota_mesh::parse_ota_frame(recv.data()) {
                self.handle_ota_frame(otaf, src, rssi, now);
                continue;
            }

            match parse_frame(recv.data()) {
                Some(Frame::Snk(f)) => {
                    // An MMO-snake frame proves the peer is audible → counts
                    // toward the LED "detected" state exactly like HELLO/BEACON.
                    self.peers.last_hello_ms = now;
                    // Roster (additive; never touches the LED): learn this peer's
                    // id (SNK frames carry it) + freshen heard/rssi.
                    self.roster.heard(src, Some(f.id), rssi, now);
                    // Register the peer so any future unicast can reach it (#28: bounded LRU).
                    self.ensure_peer(src, now);
                    // Buffer for `main` to drain into the game PeerTable; do NOT
                    // set `label` — the MeshSnake screen owns its own render.
                    self.snk.push(f);
                }
                Some(Frame::Fam(f)) => {
                    // #57 Mesh Familiar: a FAM frame proves the peer is audible → counts
                    // toward the LED "detected" state like HELLO/SNK.
                    self.peers.last_hello_ms = now;
                    // Roster (additive): learn the SENDER's logical id — the holder for
                    // a heartbeat/handoff (`f.holder`), the caller for a call (`f.target`).
                    let sender_id = if f.kind == crate::familiar::FAM_CALL { f.target } else { f.holder };
                    self.roster.heard(src, Some(sender_id), rssi, now);
                    // Register the peer so a future unicast can reach it (#28: bounded LRU).
                    self.ensure_peer(src, now);
                    // Buffer (+ RSSI, for the orphan-takeover weighting) for `fam_tick` to
                    // ingest; do NOT set `label` — the Familiar screen owns its own render.
                    self.fam_inbox.push(f, rssi);
                }
                Some(Frame::Hello(peer_id)) => {
                    // We can hear a peer -> at least "detected".
                    self.peers.last_hello_ms = now;
                    // Roster (additive): HELLO carries the sender's id — the
                    // primary place peer ids are learned (every node HELLOs 2 Hz).
                    self.roster.heard(src, Some(peer_id), rssi, now);

                    // #23 leaf channel-lock: a leaf that hears the ELECTED OWNER's
                    // HELLO has found the mesh channel — lock (stop scanning) + stamp
                    // the time (silence re-scan is driven by `leaf_scan_tick`).
                    if !self.relay.is_gateway && peer_id == self.elected_owner {
                        self.scan_locked = true;
                        self.last_owner_heard_ms = now;
                        // #51 A1: re-locked a valid owner's HELLO → resume normal HELLO
                        // (we are a healthy leaf again, no longer an abdicated ghost).
                        self.silent_until_relock = false;
                    }

                    // Register the broadcaster so the ACK below can be unicast (#28: bounded LRU).
                    self.ensure_peer(src, now);

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
                        // Roster (additive): attribute per-peer "connected" to the
                        // MAC that acked US (an ACK's payload id is OURS, so only
                        // `src` identifies the acker — the whole reason for MAC keying).
                        self.roster.acked(src, now);
                        label = Some(alloc::string::String::from("linked"));
                    }
                    // We heard *a* frame from this MAC regardless of whom it acked;
                    // record it as audible (id unknown from an ACK → learned via HELLO).
                    self.roster.heard(src, None, rssi, now);
                    // ACKs for other ids are peer-to-peer chatter; ignore (LED-wise).
                }
                Some(Frame::Beacon { seq, echo }) => {
                    // A peer BENCH beacon. Update RX count, RSSI, loss (seq
                    // gaps), and RTT (if the echo matches a seq we recently
                    // sent). A BEACON also proves we can hear the peer, so it
                    // counts toward the LED "detected" state like a HELLO.
                    self.peers.last_hello_ms = now;
                    // Roster (additive): a BEACON's sender id is not parsed (the
                    // frame drops it), so pass None — the id is learned from this
                    // MAC's HELLO, always present at 2 Hz alongside the BEACON.
                    self.roster.heard(src, None, rssi, now);
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

                    // Register the peer so future unicast (if any) can reach it (#28: bounded LRU).
                    self.ensure_peer(src, now);
                    label = Some(alloc::format!("bench seq {}", seq));
                }
                Some(Frame::Time { id, unix, synced_at }) => {
                    // Buffer the FRESHEST offer (highest synced_at) seen since
                    // `main` last took one, so a burst of frames collapses to the
                    // single best candidate. `main` owns the adopt decision + the
                    // clock re-anchor (see main::should_adopt); we only surface
                    // the offer here — this module never touches the clock.
                    if !self.time.have || synced_at > self.time.best_synced_at {
                        self.time.best_unix = unix;
                        self.time.best_synced_at = synced_at;
                        self.time.best_id = id;
                        self.time.have = true;
                    }
                    // Hearing a TIME frame also proves the peer is audible, so it
                    // counts toward the LED "detected" state exactly like a HELLO
                    // or a BEACON does.
                    self.peers.last_hello_ms = now;
                    // Roster (additive): record this peer's synced_at (drives the
                    // `*` mesh-time marker) + learn its id (TIME carries it now).
                    self.roster.synced(src, Some(id), synced_at, rssi, now);
                    label = Some(alloc::format!("time {}", synced_at));
                }
                Some(Frame::Relay { src_id, msgid, frag, count, chunk }) => {
                    // A RELAY fragment proves we can hear the peer (LED detected).
                    self.peers.last_hello_ms = now;
                    // Roster (additive): RELAY carries the leaf's src_id — learn it.
                    self.roster.heard(src, Some(src_id), rssi, now);
                    // Only a GATEWAY reassembles + acks; a leaf ignores RELAY so
                    // work + memory stay with the role that needs them.
                    if self.relay.is_gateway {
                        let (bitmap, complete) =
                            self.relay.accept(src, (src_id, msgid, frag, count), chunk, now);
                        // Register the source so the RELAYACK can be unicast back (same pattern
                        // as the HELLO -> ACK reply; #28: bounded LRU eviction when the table fills).
                        self.ensure_peer(src, now);
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
                Some(Frame::Batt(payload)) => {
                    // A gateway's battery downlink. Buffer the verbatim payload for
                    // `main` to store into its `BattCache` (this module never touches
                    // the cache — mirror of the TIME/`take_time_offer` split). Only a
                    // well-formed `BATT|…` payload is kept, so a stray frame can't wipe
                    // a good reading. Also proves the peer is audible (LED detected).
                    if payload.starts_with(b"BATT|") {
                        self.batt.set(payload);
                        // Issue #15b: log receipt so leaf-side downlink adoption is
                        // observable in serial (mirrors the "adopted mesh time" line
                        // for TIME frames — a BATT frame is the display's downlink).
                        log::info!("smol: BATT frame received ({} B) — cached", payload.len());
                    }
                    self.peers.last_hello_ms = now;
                    self.roster.heard(src, None, rssi, now);
                    label = Some(alloc::string::String::from("batt"));
                }
                Some(Frame::Grid(payload)) => {
                    // Twin of the BATT arm (issue #16): a gateway's grid downlink.
                    // Buffer the verbatim `GRID|…` payload for `main` to store into
                    // its `GridCache`; a stray/foreign frame can't wipe a good reading.
                    if payload.starts_with(b"GRID|") {
                        self.grid.set(payload);
                        log::info!("smol: GRID frame received ({} B) — cached", payload.len());
                    }
                    self.peers.last_hello_ms = now;
                    self.roster.heard(src, None, rssi, now);
                    label = Some(alloc::string::String::from("grid"));
                }
                Some(Frame::Cfg { target, key, value }) => {
                    // #21/#56 leaf-relay: a gateway's keyed CONFIG downlink. Target-filter
                    // FIRST — buffer if addressed to US (`self.id`) OR to the #43 broadcast
                    // sentinel CFG_TARGET_ALL (255, a fleet-global config like display units);
                    // a config for any OTHER specific leaf is ignored here. `CfgTracker::set`
                    // then per-KEY-filters:
                    // a key this build doesn't apply (a future channel from a newer gateway)
                    // is DROPPED (#56 forward-compat, #46 clamp). The value is opaque here;
                    // the matching `main` dispatch validates it (screen → strict/panic-free
                    // `parse_default_screen`: unknown/wrong-tier/bad-page → keep current;
                    // empty → clear). HARD BOUNDARY: screen config ONLY on `S` — never OTA.
                    // Buffering the raw bytes (not parsing here) keeps this arm total.
                    if target == self.id || target == crate::net::wifi::CFG_TARGET_ALL {
                        if self.cfg.set(key, value) {
                            log::info!(
                                "smol #56: CFG frame for us (key '{}', {} B value) — buffered",
                                key as char,
                                value.len()
                            );
                        } else {
                            // Forward-compat: a key this firmware predates (newer gateway
                            // mid rolling-OTA). Dropped, not mis-applied (#46 clamp).
                            log::info!("smol #56: CFG frame for us (unknown key '{}') — ignored", key as char);
                        }
                    }
                    self.peers.last_hello_ms = now;
                    self.roster.heard(src, None, rssi, now);
                    label = Some(alloc::string::String::from("cfg"));
                }
                Some(Frame::Stat { src: leaf_id, value }) => {
                    // #50b leaf-status uplink: a leaf's LIVE screen:page. GATEWAY-only
                    // cache — a leaf hearing another leaf's STAT ignores it (no MQTT to
                    // publish from). Buffer the raw `<screen>:<page>` value keyed by the
                    // SENDER leaf id; the MQTT flush republishes it as retained
                    // `smol/<leaf_id>/status`. Opaque bytes (mirror CFG) — a stray/foreign
                    // frame can at worst set a bad screen string, self-corrected next
                    // cadence; never a brick.
                    //
                    // #40 FIX (was `None`): LEARN the leaf's id↔MAC from its STAT. The STAT
                    // frame CARRIES the sender's id (`leaf_id`), and the relay-eligible set is
                    // exactly "leaves the gateway heard STAT from" (stat_cache / §B2). The old
                    // `None` assumed the id↔MAC was already learned from the leaf's 2 s HELLOs
                    // — but a leaf can be STAT-audible yet HELLO-silent to the gateway (e.g.
                    // #51 silent-until-relock, or its HELLO simply not landing), leaving
                    // `id_known=false` → `mac_for_id(leaf)` returned None → the relay armed but
                    // bailed at the MAC lookup with no fetch (RUNTIME-confirmed via ota/diag=
                    // mac-unknown). Recording `Some(leaf_id)` here makes the relay-set and the
                    // MAC lookup consistent. Threat model unchanged: id↔MAC from an unauth STAT
                    // is no weaker than from an unauth HELLO (both already trusted), and the
                    // relayed image is ed25519-signed so a spoofed id→MAC only wastes a session.
                    if self.relay.is_gateway {
                        // #68 F6/roster-robustness: stamp the STAT with the sender's MAC + time so
                        // a stale leaf stops ghosting its status and stays mac-resolvable if the
                        // LRU roster later evicts it. #56: pin the single-channel stat cache to one
                        // fixed key (screen key) so its (id, key) upsert behaves id-keyed.
                        self.stat_cache.set(leaf_id, crate::net::wifi::CFG_KEY_SCREEN, value, src, now);
                    }
                    self.peers.last_hello_ms = now;
                    self.roster.heard(src, Some(leaf_id), rssi, now);
                    label = Some(alloc::string::String::from("stat"));
                }
                Some(Frame::Diag { src: node_id, value }) => {
                    // #70/#49 observability uplink: a node's DIAG record. GATEWAY-only cache (a
                    // leaf hearing another's DIAG ignores it — no MQTT). Buffer the raw record keyed
                    // by SENDER id; the flush republishes it retained `smol/<id>/diag` (F6-gated).
                    // Opaque bytes (mirror STAT) — a stray frame at worst yields a bad diag string,
                    // self-corrected next cadence; never a brick.
                    if self.relay.is_gateway {
                        self.diag_cache.set(node_id, value, now);
                    }
                    self.peers.last_hello_ms = now;
                    self.roster.heard(src, Some(node_id), rssi, now);
                    label = Some(alloc::string::String::from("diag"));
                }
                Some(Frame::Scan { src: node_id, value }) => {
                    // #71 observability uplink: a leaf's one-shot WiFi-scan record. GATEWAY-only
                    // cache (twin of the DIAG arm) → republished retained `smol/<id>/scan`.
                    if self.relay.is_gateway {
                        self.scan_cache.set(node_id, value, now);
                    }
                    self.peers.last_hello_ms = now;
                    self.roster.heard(src, Some(node_id), rssi, now);
                    label = Some(alloc::string::String::from("scan"));
                }
                None => {
                    // Unrecognised payload (other ESP-NOW traffic on-channel);
                    // surface it on the OLED but don't touch the handshake.
                    label = Some(alloc::string::String::from_utf8_lossy(recv.data()).into_owned());
                }
            }
        }
        // #40: nudge the leaf OTA session once per service pass (idle-NAK a stalled
        // window; abort on a progress/hard-cap timeout). No-op unless a transfer is live.
        {
            let mut out = [0u8; crate::ota_mesh::OTAN_FRAME_MAX];
            if let crate::ota_mesh::LeafAction::Nak(n) = self.ota_leaf.tick(self.id, now_ms(), &mut out) {
                let gw = self.ota_leaf.gateway_mac();
                self.send_to(&gw, &out[..n]);
            }
        }
        label
    }

    /// #40: dispatch one decoded leaf-mesh-OTA frame. Updates peer liveness (an OTA frame
    /// proves the gateway is audible — it also satisfies the leaf self-test's "heard a
    /// valid SMOLv1 frame"), registers the sender for the unicast NAK back-channel, then
    /// drives the receive session. On `Complete` [`crate::ota::activate`] reboots into the
    /// new slot; on `Nak` we unicast the OTAN to the gateway; `Abort`/`None` do nothing.
    fn handle_ota_frame(&mut self, f: crate::ota_mesh::OtaFrame<'_>, src: [u8; 6], rssi: i32, now: u64) {
        use crate::ota_mesh::{LeafAction, OtaFrame};
        // Benign "heard a frame" liveness — safe to record for ANY decoded OTA-shaped frame:
        // both only feed roster freshness + the self-test hear-a-frame proof, and neither gates
        // trust. The VOLATILE mutations (owner-silence reset, channel lock, peer insert) are
        // deferred to the `authed` block below — see F2.
        self.peers.last_hello_ms = now;
        self.roster.heard(src, None, rssi, now);
        let mut out = [0u8; crate::ota_mesh::OTAN_FRAME_MAX];
        let id = self.id;
        let action = match f {
            OtaFrame::Meta { target, session, m, sig } => {
                self.ota_leaf.on_meta(target, session, m, sig, src, id, now)
            }
            OtaFrame::Data { target, session, seq, payload } => {
                self.ota_leaf.on_data(target, session, seq, payload, src, id, now, &mut out)
            }
            // A NAK is leaf→gateway; the gateway consumes it inside its blocking relay
            // loop, never here. A leaf ignores it.
            OtaFrame::Nak { .. } => LeafAction::None,
        };
        // F2 (oracle LOW): only a frame the leaf-OTA state machine ACCEPTED may mutate volatile
        // mesh state. A valid Meta arms the session (on_meta sets `active` + `gateway_mac = src`
        // ONLY after the ed25519 + freshness gates pass); a valid Data/Complete belongs to that
        // armed session (on_data rejects any other src/session). A spoofed or replayed OTA-shaped
        // frame fails those gates → never arms → NOT authed → it cannot pin the leaf's channel,
        // reset its owner-silence (suppressing re-election), or squat a peer-table slot pre-auth.
        // (The relayed image is itself ed25519-signed, so this only closes the pre-session spoof
        // window — hence LOW — but it keeps unauthenticated frames off the volatile mesh state.)
        let authed = matches!(action, LeafAction::Nak(_) | LeafAction::Complete(_, _))
            || (self.ota_leaf.is_active() && self.ota_leaf.gateway_mac() == src);
        if authed {
            // #3b: an ACCEPTED OTA frame proves the gateway is audible on THIS channel (the relay
            // runs on ESP_NOW_FIXED_CHANNEL). Treat it like the owner's HELLO — reset the
            // owner-silence timer AND lock the channel — so the leaf STOPS the scan hop and STAYS
            // on ch6 for the rest of the transfer. This is what lets the FIRST caught OTAM (caught
            // while the leaf was mid-hop) pin the leaf so it hears the OTAD chunks + OTAM resends.
            self.last_owner_heard_ms = now;
            if !self.relay.is_gateway {
                self.scan_locked = true;
            }
            // Register the gateway for the unicast OTAN back-channel (needed before the Nak send;
            // #28: bounded LRU eviction when the table fills).
            self.ensure_peer(src, now);
        }
        match action {
            LeafAction::Nak(n) => {
                let gw = self.ota_leaf.gateway_mac();
                self.send_to(&gw, &out[..n]);
            }
            LeafAction::Complete(slot, build) => {
                // Leaf mesh-OTA → is_leaf_ota=true → confirm via hear-a-frame on next boot.
                crate::ota::activate(slot, build, true); // reboots on success
            }
            LeafAction::None | LeafAction::Abort => {}
        }
    }

    /// #40 self-test (HOLE-1): has this node decoded ≥1 valid inbound `SMOLv1` frame since
    /// boot? This is the leaf's mesh-terms health proof (radio+parse+RX work on the new
    /// image) — the leaf analog of `reached_dhcp`, which a credential-less leaf never hits.
    pub fn heard_valid_frame(&self) -> bool {
        self.peers.last_hello_ms != 0
    }

    /// #40: look up a leaf's MAC (from the #27 roster, learned via its HELLOs) so the
    /// gateway can unicast an OTA relay to it. `None` until the leaf has been heard.
    pub fn mac_for_id(&self, id: u8) -> Option<[u8; 6]> {
        self.roster.mac_for_id(id)
    }

    /// #40 #3: eviction-proof MAC lookup for the armed leaf-OTA install. Prefers the LIVE
    /// roster (refreshing the sticky cache whenever the leaf is addressable), and falls back
    /// to the cached `(leaf_id, mac)` when the LRU roster has EVICTED the leaf during the
    /// mesh-deaf relay. The cache is set here on first sight and cleared by `record_leaf_ota`
    /// on a terminal/exhausted outcome — so it holds for exactly one install session. This is
    /// the fix for the a5d9b33 canary's `mac-unknown` dominance (relay could rarely even start).
    pub fn mac_for_id_sticky(&mut self, id: u8) -> Option<[u8; 6]> {
        if let Some(mac) = self.mac_for_id(id) {
            self.leaf_ota_mac = Some((id, mac)); // freshest wins; refresh the session cache
            return Some(mac);
        }
        if let Some((cid, mac)) = self.leaf_ota_mac {
            if cid == id {
                return Some(mac); // roster evicted it mid-session → use the session cache
            }
        }
        // #68 roster-admission robustness: the roster (a 16-slot LRU with no staleness reaping)
        // can EVICT a leaf that HELLOs sparsely while a chatty peer stays resident — so a leaf the
        // gateway currently RELAYS/STATs (id8, live) can be absent from `mac_for_id`, silently
        // no-arming its retained install. The stat_cache holds the id↔MAC of every recently-heard
        // leaf (freshness-gated), so resolve from there as a last resort → any STAT-heard leaf
        // stays armable. If it's genuinely off-air, the entry ages past STAT_FRESH_MS → None (no
        // arm to a gone leaf; a stale ghost can't fake reachability).
        let mac =
            self.stat_cache
                .mac_for(id, now_ms(), crate::net::wifi::STAT_FRESH_MS)?;
        self.leaf_ota_mac = Some((id, mac));
        Some(mac)
    }

    /// #28: register `mac` as an ESP-NOW unicast peer, evicting the least-valuable existing peer
    /// first when the HW table is full (`ESP_NOW_PEER_CAP`, which includes the broadcast peer).
    /// This is the bound that stops the mesh ceilinging at ~20 nodes: esp-wifi has no auto-eviction
    /// and `add_peer` silently `Err`s once full, so a large fleet's later joiners could otherwise
    /// never be unicast (HELLO→ACK never latches `Connected`; a gateway's RELAYACK/OTAN never
    /// lands). No-op if the peer already exists. Routed through by every unicast-reply add-peer site
    /// (SNK / HELLO / BEACON / RELAY frames, the OTA back-channel, and the relay target).
    fn ensure_peer(&self, mac: [u8; 6], now: u64) {
        if self.esp_now.peer_exists(&mac) {
            return;
        }
        // Evict the least-valuable peer(s) until under cap. Bounded: a `None` victim (everything
        // left is protected) or a `remove_peer` error breaks out rather than spinning — leaving
        // the final `add_peer` to `Err` exactly as it did before the fix (never worse than today).
        while matches!(self.esp_now.peer_count(), Ok(c) if c.total_count >= ESP_NOW_PEER_CAP) {
            let Some(victim) = self.peer_evict_victim(mac, now) else {
                break;
            };
            if self.esp_now.remove_peer(&victim).is_err() {
                break;
            }
        }
        let _ = self.esp_now.add_peer(PeerInfo {
            interface: EspNowWifiInterface::Sta,
            peer_address: mac,
            lmk: None,
            channel: None,
            encrypt: false,
        });
    }

    /// #28: pick the least-valuable HW peer to evict. Walks the ESP-NOW peer list (`fetch_peer`
    /// SKIPS broadcast/multicast, so only unicast peers are candidates) and ranks each by the
    /// roster value-key, with a peer no longer in the roster (a ghost the roster already dropped)
    /// sorted BELOW every live peer so it goes first. NEVER evicts the MAC we're about to add, the
    /// BROADCAST peer, or the active leaf-OTA relay target (`leaf_ota_mac` — its sticky MAC must
    /// survive the mesh-deaf relay). `None` when nothing is evictable (all remaining peers protected).
    ///
    /// Broadcast belt-and-suspenders (nebula finding): `fetch_peer` is documented to already skip
    /// broadcast/multicast (so it can't be a candidate), and evicting broadcast would be
    /// catastrophic — `esp_now_send(ff:ff…)` would return NOT_FOUND and this node's HELLOs would
    /// silently stop egressing, dropping it off the mesh. We therefore ALSO guard it explicitly, so
    /// the code is self-evidently safe without depending on that external `fetch_peer` contract.
    fn peer_evict_victim(&self, adding: [u8; 6], now: u64) -> Option<[u8; 6]> {
        let protect = self.leaf_ota_mac.map(|(_, m)| m);
        let mut victim: Option<[u8; 6]> = None;
        let mut victim_rank: (bool, bool, bool, i32, u64) = (true, true, true, i32::MAX, u64::MAX);
        let mut from_head = true;
        while let Ok(p) = self.esp_now.fetch_peer(from_head) {
            from_head = false;
            let mac = p.peer_address;
            if mac == adding || mac == BROADCAST_ADDRESS || Some(mac) == protect {
                continue;
            }
            // Rank = (is_live, id_known, connected, rssi, last_heard); LOWER = evict first. A ghost
            // (not in the roster) is `is_live=false` → below any live peer regardless of the rest.
            let rank = match self.roster.value_key_for_mac(mac, now) {
                Some((idk, conn, rssi, heard)) => (true, idk, conn, rssi, heard),
                None => (false, false, false, i32::MIN, 0),
            };
            if victim.is_none() || rank < victim_rank {
                victim = Some(mac);
                victim_rank = rank;
            }
        }
        victim
    }

    /// #40 #3: the leaf's OTA RX-diag `(otam_heard, on_meta_verdict, otan_sent)` — `main`
    /// beacons it via `broadcast_ldbg` so a headless canary names (a)/(b)/(c) for a `relay-failed`.
    /// `espnow`-scoped: `ota_leaf` (the receive session) only exists in the mesh build.
    #[cfg(feature = "espnow")]
    pub fn ota_leaf_dbg(&self) -> (u16, u8, u16) {
        self.ota_leaf.dbg()
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
    // #57 Mesh Familiar: FAM diverges from SNK at prefix byte 7 ('F' vs 'S'), so this
    // prefix check never collides. `parse_fam` validates length/kind/seed itself.
    if data.starts_with(FAM_PREFIX.as_slice()) {
        return parse_fam(data).map(Frame::Fam);
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
        // synced_at (10) = 25 bytes. Freshness (not identity) drives adoption, but
        // the sender id IS retained now — for adoption provenance (Bench's
        // `adopt<Noun>` own-status) and to learn ids into the roster. This reads a
        // field already on the wire; the TIME frame format is unchanged.
        if rest.len() >= 25 {
            let id = parse_id(&rest[0..3])?;
            let unix = parse_u10(&rest[4..14])?;
            let synced_at = parse_u10(&rest[15..25])?;
            return Some(Frame::Time { id, unix, synced_at });
        }
        return None;
    }
    if let Some((src_id, msgid, frag, count, chunk)) = parse_relay(data) {
        return Some(Frame::Relay { src_id, msgid, frag, count, chunk });
    }
    if let Some((msgid, bitmap)) = parse_relayack(data) {
        return Some(Frame::RelayAck { msgid, bitmap });
    }
    if let Some(rest) = data.strip_prefix(BATT_PREFIX) {
        // The rest is the verbatim `BATT|…` payload (no length byte).
        return Some(Frame::Batt(rest));
    }
    if let Some(rest) = data.strip_prefix(GRID_PREFIX) {
        // Twin of BATT (issue #16): the rest is the verbatim `GRID|…` payload.
        return Some(Frame::Grid(rest));
    }
    if let Some(rest) = data.strip_prefix(CFG_PREFIX) {
        // #21/#56 leaf-relay: "NNN<KEY><value>" — 3-ASCII target id, a 1-byte config KEY,
        // then the verbatim value (to end-of-frame; empty = clear that key). `parse_id`
        // rejects short/non-digit and guarantees `rest.len() >= 3`. The byte after the id
        // is the KEY; `&rest[4..]` is its value (an empty slice for a keyed clear like
        // "007S"). A key-less frame (id only, `rest.len() == 3`) is read as an empty-value
        // clear on the SCREEN key — #56 back-compat with the pre-key `default_screen` wire.
        // Unknown keys parse fine here and are dropped at the leaf's per-key dispatch.
        let target = parse_id(rest)?;
        let (key, value): (u8, &[u8]) = match rest.get(3) {
            Some(&k) => (k, &rest[4..]),
            None => (crate::net::wifi::CFG_KEY_SCREEN, &rest[3..]),
        };
        return Some(Frame::Cfg { target, key, value });
    }
    if let Some(rest) = data.strip_prefix(STAT_PREFIX) {
        // #50b leaf-status uplink: "NNN<value>" — 3-ASCII SENDER id then the verbatim
        // `<screen>:<page>` value (to end-of-frame; empty = none). `parse_id` rejects
        // short/non-digit and guarantees `rest.len() >= 3`, so `&rest[3..]` never panics.
        let src = parse_id(rest)?;
        return Some(Frame::Stat { src, value: &rest[3..] });
    }
    if let Some(rest) = data.strip_prefix(DIAG_PREFIX) {
        // #70/#49 observability uplink: "NNN<record>" — 3-ASCII SENDER id then the verbatim
        // key=val DIAG record (to end-of-frame; empty = none). Same `parse_id` guard as STAT.
        let src = parse_id(rest)?;
        return Some(Frame::Diag { src, value: &rest[3..] });
    }
    if let Some(rest) = data.strip_prefix(SCAN_PREFIX) {
        // #71 observability uplink: "NNN<record>" — 3-ASCII SENDER id then the verbatim WiFi-scan
        // record (to end-of-frame; empty = none). Twin of the DIAG arm.
        let src = parse_id(rest)?;
        return Some(Frame::Scan { src, value: &rest[3..] });
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
pub fn start(
    p: WifiPeripherals,
    id: u8,
    // #20: `main` passes the responsive tick (poll button + "Syncing…" redraw +
    // LED fast-blink); returns true to ABORT the boot burst (a long-press → boot
    // ends early and proceeds straight to the Menu). Replaces the old internal
    // LED-only closure, keeping this radio module UI-agnostic — it just calls
    // `tick() -> bool`.
    tick: &mut dyn FnMut() -> bool,
    batt: &mut crate::batt::BattCache,
    grid: &mut crate::grid::GridCache,
) -> (Option<RadioManager>, Option<u32>) {
    let Some(mut radio) = RadioManager::new(p, id) else {
        return (None, None);
    };

    // --- WiFi burst for NTP; `main`'s responsive `tick` runs inside every busy-
    // wait loop (LED fast-blink + "Syncing…" redraw + long-press abort). `batt`/
    // `grid` ride along: the same burst runs the MQTT downlink at its tail to seed
    // the Batt/Grid screens at boot. If `tick` returns true (abort), the burst
    // returns early (no sync) and boot proceeds straight to the Menu.
    // #23 stage 3-4: the boot MQTT session (inside the burst) runs the single-gateway
    // election over the retained `smol/mesh/channel`; `elect` returns whether I own the
    // mesh (coexist gateway) or must demote to a scanning leaf.
    let mut elect = crate::net::wifi::MeshElect::new(id);
    elect.now_ms = now_ms(); // seed the ONE clock the stale-owner timeout runs on
    elect.boot = true; // #51 return-flap: never displace a different owner already in the MC
    let mut ota_offer: Option<crate::ota::Announce> = None;
    let mut config_offer: Option<crate::app::DefaultScreen> = None;
    let mut install_requested = false;
    let (reached_dhcp, synced) = radio.burst_ntp(
        batt,
        grid,
        &mut elect,
        &mut ota_offer,
        &mut config_offer,
        &mut install_requested,
        tick,
    );
    // #6 OTA: stash any gated boot-time announce for `main` to fetch after boot.
    radio.ota_offer = ota_offer;
    // #21: stash the boot-time default-screen config so `main` seeds the boot screen
    // from it (the reconciled per-node config), falling back to DEFAULT_APP if absent.
    radio.config_offer = config_offer;
    // #33: stash a boot-time install command (retained) so `main` fetches after boot.
    radio.install_requested = install_requested;

    // OTA MF-1 / #40 HOLE-1 — ROLE-AWARE first-boot self-test.
    // A node that reached DHCP has a working uplink → confirm NOW (DHCP is the gateway's
    // health proof; a broken-WiFi image can't reach it, so a healthy image won't
    // false-rollback). A node that did NOT reach DHCP is a LEAF (it can never win the
    // gateway election): confirming on `reached_dhcp=false` here would roll back EVERY
    // mesh-OTA (the credential-less leaf never does DHCP). So a leaf DEFERS its self-test
    // to the main loop, which confirms on the mesh predicate (heard ≥1 valid SMOLv1 frame
    // within N s) instead. `boot_confirm(true)` may still reboot-on-nothing (no-op if the
    // image is already Valid). See main.rs `leaf_selftest_pending`.
    if reached_dhcp {
        crate::ota::boot_confirm(true);
    }

    // #23: GATEWAY iff we reached DHCP AND won the election (lowest-id owner). A board
    // that reached DHCP but LOST (a lower-id owner already holds it) demotes to leaf.
    // `synced` stays best-effort for TIME (mesh-time adoption handles an unsynced GW).
    radio.relay.is_gateway = reached_dhcp && elect.i_am_owner;
    radio.elected_owner = elect.owner_id;
    // #51 B: if we associated, seed the initial RSSI-to-AP so the first recovery
    // election already has a real signal-strength reading to compare on.
    if reached_dhcp {
        if let Ok(r) = radio.controller.rssi() {
            radio.my_rssi_to_ap = r.clamp(-127, 0) as i8;
        }
    }
    // #23 fix: persist the boot election's staleness observation → a leaf's later
    // recovery re-election measures the owner's seq freshness FROM this first sample.
    radio.mc_seen_owner = elect.seen_owner;
    radio.mc_seen_seq = elect.seen_seq;
    radio.mc_seen_ms = elect.seen_ms;
    log::info!(
        "smol: relay role = {} (assoc+dhcp {}, elected-owner id{}, ntp {})",
        if radio.relay.is_gateway { "GATEWAY" } else { "leaf" },
        reached_dhcp,
        elect.owner_id,
        if synced.is_some() { "ok" } else { "miss" }
    );

    // --- #23 stage 1: PRODUCTION COEXIST (soak-proven, retire the burst) ---------
    // A GATEWAY (reached DHCP) STAYS WiFi-associated — the mesh rides its AP channel,
    // no ESP-NOW time-share teardown, so the flush is sub-second on the live link
    // (0/60 flush-window RX loss in the 30-min soak). A LEAF (didn't reach DHCP)
    // switches to ESP-NOW and discovers the mesh channel (stages 2-4). Under the
    // `coexist-soak` measurement feature the gateway additionally rides the pinned
    // ch1 (ESP_NOW_FIXED_CHANNEL) via the ClientConfiguration channel pin.
    if radio.relay.is_gateway {
        // Coexist gateway (elected owner): stay WiFi-STA, mesh rides my AP channel.
        log::info!("smol: coexist gateway — staying WiFi STA on AP channel (no teardown)");
    } else {
        // Leaf (no DHCP, or lost the election): drop to ESP-NOW and scan 1/6/11 for the
        // elected owner's HELLO — channel-agnostic discovery (stage 2).
        let _ = radio.switch(Mode::EspNow);
        log::info!("smol: leaf — scanning for gateway id{}", radio.elected_owner);
    }

    (Some(radio), synced)
}
