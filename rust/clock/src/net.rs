//! Network module: WiFi + SNTP (Phase 2) and ESP-NOW + radio switching
//! (Phase 3). Everything here is feature-gated so the default Phase 1 build
//! pulls in none of the esp-wifi stack.
//!
//! Design note: `esp_hal::init()` may only run once and hands out the
//! peripheral singletons. `main` owns that call and passes the radio-related
//! peripherals into this module, so we never double-initialise the HAL.

#[cfg(feature = "wifi")]
mod wifi;

/// #141: clamp the radio's max TX power. Cheap C3-supermini boards distort their own TX at
/// full power (worse on marginal USB supplies) — the AP receives corrupted auth/ACK frames
/// (the "auth expired at strong signal" / silent-hostapd / mid-transfer-stall class). Units
/// are 0.25 dBm steps (the IDF `esp_wifi_set_max_tx_power` contract, valid range 8..=84);
/// 34 = 8.5 dBm, the sibling-project-proven value for this board class. Requires a STARTED
/// WiFi driver — called at radio init and re-asserted beside every #139 `PowerSaveMode::None`
/// assert (a driver stop/start resets it; connect() does not).
#[cfg(feature = "wifi")]
// #172: a wifi-layer TX-power clamp, but every caller today is on an espnow-gated path
// (run_mqtt_burst / run_ota_fetch / mode). Keep it available in wifi builds; silence
// dead-code in a wifi-without-espnow build so `clippy --features wifi -D warnings` passes.
#[cfg_attr(not(feature = "espnow"), allow(dead_code))]
pub(crate) fn assert_max_tx_power() {
    // 0c′ STUB (#198): esp-radio 0.18 keeps its `esp_wifi_sys` FFI in a `pub(crate)` module,
    // so the #141 8.5 dBm TX-power clamp can't be re-applied via a free fn here. WiFi-STA is
    // stubbed in 0c′; the bench canary runs at the driver-default TX power (higher, fine for a
    // brief bench run). TODO(#198 Phase 3): re-apply via the controller's safe set-power API
    // (esp_radio WifiController exposes esp_wifi_set_max_tx_power internally) from net::mode.
}

/// #204: the crown's CURRENT AP association — `(channel, RSSI dBm, BSSID)` — via the IDF
/// `esp_wifi_sta_get_ap_info` FFI. `None` if not associated or the call errors. Published in the
/// crown's DIAG so the #204 coexist-starvation hypothesis (crown deaf when its roam AP is NOT
/// co-channel with the mesh ch6) is TESTABLE from telemetry — the forensics gap that cost ~3h of
/// pcap tonight (the fw could not report which AP/channel/RSSI a deaf crown was on).
#[cfg(feature = "espnow")]
pub(crate) fn current_ap_info() -> Option<(u8, i8, [u8; 6])> {
    // 0c′ STUB (#198): the #204 crown-AP-association readback used esp-wifi's
    // `esp_wifi_sta_get_ap_info` FFI, now behind esp-radio 0.18's `pub(crate)` sys module.
    // WiFi-STA is stubbed in 0c′ so a node never associates → this always returned `None`
    // anyway; keep that. TODO(#198 Phase 3): read the AP info from the associated
    // embassy-net station (controller.ap_info() on the new stack).
    None
}

// Hand-rolled MQTT 3.1.1 (QoS0) codec for the HA batt/telemetry bridge (v2). Pure
// encode/decode; the socket poll-loop that drives it lives in `wifi.rs`.
#[cfg(feature = "wifi")]
mod mqtt;

#[cfg(feature = "espnow")]
pub mod mode;

// #164 per-peer link-quality (ETX) metric: the PURE reach-register + cost mapping, ported
// from babeld's neighbour.c (see docs/superpowers/research/althea-babel-study.md, #163).
// Host-testable (experiments/etx_verify), no HAL deps. espnow-gated to match its sole
// consumer `mode::Roster` (its `LinkQuality`-per-peer holder) — so the default/wifi Phase-1/2
// builds stay byte-free of it, exactly like `flood`/`wire`.
#[cfg(feature = "espnow")]
pub mod etx;

// #267 cross-burst OTA-fetch resume: the PURE resume-key logic (does a saved cursor match this
// staged image + slot → what offset to resume from). Host-testable (experiments/267_resume_verify),
// no HAL deps. Consumed by `crate::ota::ImageWriter` (the HW flash writer + the .bss cursor), which
// is espnow-gated — so gate it the same, byte-free of the default/wifi builds like `etx`/`flood`.
#[cfg(feature = "espnow")]
pub mod ota_resume;

// #13 routed multi-hop mesh: the PURE managed-flood decision core (SeenSet + forward
// decision + HopLatch escalation state machine), host-testable, no HAL deps. Driven by
// the relay path in `mode`, so espnow-gated.
#[cfg(feature = "espnow")]
pub mod flood;

// #13: the PURE SMOLv1 relay-family wire codec (RELAY/RELAYACK/RELAY2/RELAYACK2/BATT2/GRID2 +
// the fixed-width ASCII field helpers), extracted from `mode` so the frame formats are
// host-unit-testable off-target (see `experiments/relay_compat`) — the mixed-fleet / #124
// byte-compat guard. `mode` re-exports it via `use crate::net::wire::*`.
#[cfg(feature = "espnow")]
pub mod wire;

// #217 rung-3: co-channel-preferred crown AP selection + the never-crownless strand-guard state
// machine. PURE (no esp-hal/esp-wifi, no alloc) so it's host-tested verbatim by
// `experiments/ap_select_verify` (#[path]-include, like `wire`); `wifi`/`mode` build ApViews from
// scan results + drive the WiFi association + crown state from its decisions.
#[cfg(feature = "espnow")]
pub mod coexist;

// Configurable best-gateway election — PURE (no esp-hal/esp-wifi, no alloc), host-tested verbatim by
// `experiments/election_verify` (#[path]-include, like `coexist`). Seeded by `wifi`'s MeshElect
// resolver + `mode`'s flush/recovery paths; gated to `wifi` (where MeshElect lives — every election
// path is built with WiFi present).
#[cfg(feature = "wifi")]
pub mod election;

// Minimal HTTP/1.x response-head parsers for the OTA fetch leg — PURE, host-tested verbatim by
// `experiments/ota_http_verify` (#[path]-include). Extracted from `ota` so the #gateway-election
// byte-0 fetch bug (a coalesced header+binary-body segment failing UTF-8 → status None) has ONE
// definition with a regression test. `espnow`-gated (only `run_ota_fetch` uses it).
#[cfg(feature = "espnow")]
pub mod http;

// #25 WLED WiZmote-emit (smol as a WLED "linked remote"). `wled = ["espnow"]`, so
// this is present only in a wled build; the default/wifi/espnow builds are byte-free
// of it (the module is `#![cfg(feature = "wled")]`). Referenced by `app` (the
// WledRemote screen) + `mode` (broadcast_wled_button), so it is `pub`.
#[cfg(feature = "wled")]
pub mod wled;

// #26 smol Cast: stream the gateway's OLED image to a network WLED matrix as
// realtime UDP pixels. `cast = ["wifi"]`. `cast` is the PURE packer + shadow
// framebuffer (host-testable, no HAL deps); `cast_oled` is the DrawTarget tee that
// feeds it (needs ssd1306). Absent from every non-cast build → the default / wifi /
// espnow / wled profiles are byte-free of it.
#[cfg(feature = "cast")]
pub mod cast;
#[cfg(feature = "cast")]
pub mod cast_oled;

// Deterministic magical node names (realm-sigil port). Needs no radio — a node
// derives its OWN name and any peer's name from the logical id alone — so it is
// compiled in ALL builds (peer names are only *displayed* under espnow, but our
// own name is the idle bottom-line label everywhere).
pub mod names;

#[cfg(feature = "wifi")]
pub use wifi::WifiPeripherals;

// #56 keyed CFG: re-export the screen config-channel key so `main` (crate root, outside
// this module) can name it when pulling the screen offer from the keyed relay. `wifi` is
// private to `net`; `mode`/`wifi` reach the const directly, but `main` needs this bridge.
// espnow-gated: `main` consumes it ONLY on the leaf-apply path (`take_cfg_offer`), which is
// espnow-only — a wifi-only build reaches the const in-module (no re-export → no unused-import).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_SCREEN;
// #48 LED mode key — same `main`-bridge rationale as the screen key (espnow leaf-apply path).
// #55/#52 add their keys (P/R) here as each feature wires its apply.
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_LED;
// #43 display-units key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(U)). CFG_TARGET_ALL stays wifi-internal (only mode.rs/wifi.rs name it).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_UNITS;
// #55 plugin-mask key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(P)).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_PLUGINS;
// #52 remote-reboot key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(R), with a boot-debounce before software_reset()).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_REBOOT;
// #45 custom-screen key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(Y); the held layout feeds the Custom plugin render).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_CUSTOM;
// #100 network-switch key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(N); the apply writes the NVS net-record + reboots into the slot).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_NET;
// #100 Stage 2/3 broker + OTA-host override keys — same `main`-bridge rationale (espnow apply path
// via take_cfg_offer(B)/(O); B writes the NVS record + reboots, O writes it WITHOUT a reboot).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_BROKER;
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_OTA;
// #197 herald NOTIFY key — espnow leaf-apply path (take_cfg_offer(M) → crate::toast::set).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_NOTIFY;
// #gateway-election all-nodes-WiFi DEBUG key — espnow leaf/own apply path
// (take_cfg_offer(A) → RadioManager::set_debug_wifi_all).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_WIFI_ALL;
// #72 IO-registry key — the leaf/own apply path (take_cfg_offer(G) → io::apply_wire re-binds
// the free GPIOs). `io`-gated (⊃ espnow): only the io apply path names it here.
#[cfg(feature = "io")]
pub use wifi::CFG_KEY_IO;
// #72 IO output-control key — the leaf/own apply path (take_cfg_offer(g) → io::apply_set drives
// the bound OUTPUT slots). `io`-gated, same rationale.
#[cfg(feature = "io")]
pub use wifi::CFG_KEY_IO_SET;
// #45: `main` sizes its held Custom-layout buffer to the max keyed value — bridge the const out
// of the private `wifi` module (espnow-only: only the Custom apply path names it).
#[cfg(feature = "espnow")]
pub use wifi::CFG_VALUE_MAX;

// #71 on-demand WiFi-scan key — same `main`-bridge rationale (espnow apply path via
// take_cfg_offer(W) → run_scan).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_SCAN;

// `try_time_sync` is the Phase-2 entry point; under `espnow`, `main` calls
// `mode::start` instead, so only re-export it when espnow is NOT enabled.
#[cfg(all(feature = "wifi", not(feature = "espnow")))]
pub use wifi::try_time_sync;

// #192: the NTP re-sync staleness threshold — read by `main`'s flush-cadence re-sync trigger.
// `wifi` is a private submodule, so main reaches the const only via this re-export.
#[cfg(feature = "espnow")]
pub(crate) use wifi::NTP_RESYNC_AGE_S;

/// Install esp-wifi's heap ONCE. esp-alloc declares the `#[global_allocator]`
/// inside its own crate; this macro just adds an internal-RAM region to it.
/// Defined here so both the Phase 2 (`wifi`) and Phase 3 (`espnow`) code paths
/// share a single heap region rather than each reserving their own.
///
/// #140: grown 72 → 128 KiB. The gateway free-heap low-watermark bottomed at ~5.9 KB during crown
/// duty (esp-wifi's static RX pool ≈16 KB + dynamic RX churn draw from THIS region), leaving no room
/// to raise the RX buffers that fix the sustained-fetch stalls. The heap audit
/// (scratch/smol-ha-batt/140-heap-audit.md) showed ~120 KB of the C3's 313 KiB DRAM window unused —
/// so growing the heap is the safe unlock: it lifts the low-watermark to ~52 KB even after the
/// #140 static-RX bump (.cargo/config.toml [env]). The region is uninit `.bss` in DMA-capable
/// internal SRAM; 128 KiB keeps `.data`+`.bss` (~191 KB) well under the DRAM window before stack.
#[cfg(feature = "hw")] // #198: called unconditionally from main (D2 — default build allocates)
pub fn init_heap() {
    esp_alloc::heap_allocator!(size: 128 * 1024);
}

/// Phase-1 (default) placeholder used when no radio features are enabled: the
/// caller free-runs the clock from its compile-time start constant.
#[cfg(not(feature = "wifi"))]
pub fn try_time_sync() -> Option<u32> {
    None
}
