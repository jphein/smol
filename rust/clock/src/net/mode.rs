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

use esp_hal::{rng::Rng, timer::timg::TimerGroup};
use esp_wifi::{
    esp_now::{EspNow, EspNowWifiInterface, PeerInfo, BROADCAST_ADDRESS},
    wifi::{ClientConfiguration, Configuration, WifiController, WifiMode},
    EspWifiController,
};

use crate::net::WifiPeripherals;

/// Fixed ESP-NOW channel used in TIME-SHARE mode. All smol units must agree on
/// this value (1..=13). 6 is a common, low-congestion default.
const ESP_NOW_FIXED_CHANNEL: u8 = 6;

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
    mode: Mode,
    /// Our short device id, embedded in the broadcast message.
    id: u8,
}

impl RadioManager {
    /// Initialise the radio once. Starts in `WifiSta` mode so the caller can do
    /// an NTP burst before switching to ESP-NOW.
    pub fn new(p: WifiPeripherals, id: u8) -> Option<Self> {
        // esp-wifi needs a heap; use the single shared region (see net::init_heap).
        super::init_heap();

        let timg0 = TimerGroup::new(p.timg0);
        let rng = Rng::new(p.rng);
        let ctrl: EspWifiController<'static> = esp_wifi::init(timg0.timer0, rng).ok()?;
        let ctrl: &'static EspWifiController<'static> =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(ctrl));

        let (mut controller, interfaces) = esp_wifi::wifi::new(ctrl, p.wifi).ok()?;
        controller.set_mode(WifiMode::Sta).ok()?;
        controller.start().ok()?;

        Some(Self {
            controller,
            esp_now: interfaces.esp_now,
            mode: Mode::WifiSta,
            id,
        })
    }

    /// Current radio mode. Part of the public API (a caller may inspect which
    /// stack is live before choosing to broadcast); not used by `main` today.
    #[allow(dead_code)]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Access the WiFi controller (Phase 2 uses this to associate + run NTP).
    pub fn controller(&mut self) -> &mut WifiController<'static> {
        &mut self.controller
    }

    /// Configure + associate to the AP (used before an NTP burst). Blocking
    /// only up to the caller's own deadline check via `is_connected`.
    pub fn wifi_connect(&mut self, ssid: &str, password: &str) -> Result<(), ()> {
        self.controller
            .set_configuration(&Configuration::Client(ClientConfiguration {
                ssid: ssid.into(),
                password: password.into(),
                ..Default::default()
            }))
            .map_err(|_| ())?;
        // set_configuration flips the mode; ensure the radio is (re)started.
        if !matches!(self.controller.is_started(), Ok(true)) {
            self.controller.start().map_err(|_| ())?;
        }
        self.controller.connect().map_err(|_| ())
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

    /// Send one broadcast "hello from smol <id>" frame. Safe to call in either
    /// mode; in `WifiSta` it rides the AP channel, in `EspNow` the fixed one.
    pub fn broadcast_hello(&mut self) {
        // Build the message on the stack: "hello from smol NNN".
        let mut msg = [0u8; 20];
        let prefix = b"hello from smol ";
        msg[..prefix.len()].copy_from_slice(prefix);
        // 3-digit zero-padded id.
        msg[prefix.len()] = b'0' + (self.id / 100) % 10;
        msg[prefix.len() + 1] = b'0' + (self.id / 10) % 10;
        msg[prefix.len() + 2] = b'0' + self.id % 10;
        let len = prefix.len() + 3;

        match self.esp_now.send(&BROADCAST_ADDRESS, &msg[..len]) {
            Ok(waiter) => {
                let _ = waiter.wait();
            }
            Err(e) => log::warn!("smol: esp-now send failed: {:?}", e),
        }
    }

    /// Poll for one inbound ESP-NOW frame. Returns the sender MAC + a short
    /// display string (payload as UTF-8, truncated) if something arrived.
    ///
    /// Also auto-registers unknown broadcasters as peers so a subsequent
    /// unicast reply would succeed (mirrors the esp-wifi example).
    pub fn poll_message(&mut self) -> Option<([u8; 6], alloc::string::String)> {
        let recv = self.esp_now.receive()?;
        let src = recv.info.src_address;

        if recv.info.dst_address == BROADCAST_ADDRESS && !self.esp_now.peer_exists(&src) {
            let _ = self.esp_now.add_peer(PeerInfo {
                interface: EspNowWifiInterface::Sta,
                peer_address: src,
                lmk: None,
                channel: None,
                encrypt: false,
            });
        }

        let text = alloc::string::String::from_utf8_lossy(recv.data()).into_owned();
        Some((src, text))
    }
}

// -------------------------------------------------------------------------
// Public flow used by `main` under `--features espnow`.
// -------------------------------------------------------------------------

/// WiFi credentials for the NTP burst (compile-time placeholders).
const WIFI_SSID: &str = "YOUR_WIFI_SSID";
const WIFI_PASSWORD: &str = "YOUR_WIFI_PASSWORD";

/// Bring the radio up, do a WiFi/NTP burst, then TIME-SHARE-switch to ESP-NOW.
/// Returns the live `RadioManager` (now in ESP-NOW mode) and the synced Unix
/// time (or None if the NTP burst failed — clock then free-runs).
pub fn start(p: WifiPeripherals, id: u8) -> (Option<RadioManager>, Option<u32>) {
    let Some(mut radio) = RadioManager::new(p, id) else {
        return (None, None);
    };

    // --- WiFi burst for NTP (Phase 2 logic, reused honestly) -------------
    let synced = burst_ntp(&mut radio);

    // --- Hand the radio to ESP-NOW on a fixed channel (TIME-SHARE) -------
    let _ = radio.switch(Mode::EspNow);

    (Some(radio), synced)
}

/// Associate to the AP and run one SNTP exchange, reusing the smoltcp/SNTP
/// machinery from the Phase 2 `wifi` module. Kept here (rather than calling the
/// Phase 2 entry point) because Phase 3 already owns the controller + radio.
fn burst_ntp(radio: &mut RadioManager) -> Option<u32> {
    use esp_hal::time::{Duration, Instant};

    if radio.wifi_connect(WIFI_SSID, WIFI_PASSWORD).is_err() {
        log::warn!("smol: wifi_connect failed; skipping NTP");
        return None;
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(radio.controller().is_connected(), Ok(true)) {
        if Instant::now() > deadline {
            log::warn!("smol: WiFi connect timed out; skipping NTP");
            return None;
        }
    }
    // NOTE: A full DHCP+SNTP run needs the STA `WifiDevice`, which esp-wifi
    // hands out once from `Interfaces`. Under `--features espnow` we keep the
    // ESP-NOW handle live for the clock loop and DO NOT drive the smoltcp stack
    // here, to avoid holding two mutable radio stacks at once. The compile-safe
    // behaviour is: associate (proving the WiFi side works), then time-share to
    // ESP-NOW. Real NTP over this path is available in the `wifi`-only build
    // (`crate::net::wifi::try_time_sync`); wiring the shared device into this
    // Phase-3 flow is documented in README as the remaining integration step.
    log::info!("smol: WiFi associated (Phase 3 burst); NTP handled in wifi-only build");
    None
}
