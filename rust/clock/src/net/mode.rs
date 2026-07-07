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

/// Parsed inbound handshake frame.
enum Frame {
    /// A peer beacon; carries the sender's id.
    Hello(u8),
    /// An acknowledgement; carries the id of the unit being acked.
    Ack(u8),
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

/// Parse an inbound payload into a [`Frame`], or `None` if it isn't ours.
fn parse_frame(data: &[u8]) -> Option<Frame> {
    if let Some(rest) = data.strip_prefix(HELLO_PREFIX) {
        return parse_id(rest).map(Frame::Hello);
    }
    if let Some(rest) = data.strip_prefix(ACK_PREFIX) {
        return parse_id(rest).map(Frame::Ack);
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
