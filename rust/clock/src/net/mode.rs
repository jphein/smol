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

/// Parsed inbound frame.
enum Frame {
    /// A peer HELLO beacon (LED handshake); carries the sender's id.
    Hello(u8),
    /// An acknowledgement (LED handshake); carries the id of the unit acked.
    Ack(u8),
    /// A BENCH beacon: (sender_id, sender_seq, echo_seq). `echo_seq` is the last
    /// of OUR seqs the sender had heard when it sent this — an echo_seq that
    /// matches a seq we recently sent lets us compute round-trip time.
    Beacon { seq: u32, echo: u32 },
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
    /// an `Option` so the NTP burst can drive it, then `take()` + drop it to
    /// free the smoltcp stack before we time-share the radio to ESP-NOW.
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
        })
    }

    /// Run the real WiFi -> DHCP -> SNTP burst using the STA device, driving the
    /// caller's `tick` closure throughout (the `espnow` build fast-blinks the
    /// blue LED so "WiFi/NTP in progress" is visible). Returns the synced Unix
    /// time, or `None` on any timeout. Consumes the STA device (drops it after)
    /// so the radio is free to time-share to ESP-NOW next.
    pub fn burst_ntp(&mut self, tick: &mut dyn FnMut()) -> Option<u32> {
        let mut sta = self.sta.take()?;
        let synced =
            crate::net::wifi::run_ntp_burst(&mut self.controller, &mut sta, self.rng, tick);
        // `sta` (the STA device + its smoltcp stack) falls out of scope here,
        // releasing the WiFi datapath; the ESP-NOW handle stays live for the
        // clock loop. We took it out of `self.sta` so it can't be reused.
        synced
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
        // Bound the drain: ESP-NOW's RX queue can hold several frames; process a
        // handful per call so a burst can't stall the 1 Hz clock tick.
        let mut label: Option<alloc::string::String> = None;
        for _ in 0..8 {
            let Some(recv) = self.esp_now.receive() else {
                break;
            };
            let src = recv.info.src_address;
            // RSSI of this frame (dBm) from the ESP-NOW RX control info; used by
            // BENCH. Captured up front so each arm can record it if relevant.
            let rssi = recv.info.rx_control.rssi;
            let now = now_ms();

            match parse_frame(recv.data()) {
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

                    label = Some(alloc::format!("peer {:03}", peer_id));
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

/// Parse an inbound payload into a [`Frame`], or `None` if it isn't ours.
fn parse_frame(data: &[u8]) -> Option<Frame> {
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

    // --- Hand the radio to ESP-NOW on a fixed channel (TIME-SHARE) -------
    let _ = radio.switch(Mode::EspNow);

    (Some(radio), synced)
}
