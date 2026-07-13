//! Network module: WiFi + SNTP (Phase 2) and ESP-NOW + radio switching
//! (Phase 3). Everything here is feature-gated so the default Phase 1 build
//! pulls in none of the esp-wifi stack.
//!
//! Design note: `esp_hal::init()` may only run once and hands out the
//! peripheral singletons. `main` owns that call and passes the radio-related
//! peripherals into this module, so we never double-initialise the HAL.

#[cfg(feature = "wifi")]
mod wifi;

// Hand-rolled MQTT 3.1.1 (QoS0) codec for the HA batt/telemetry bridge (v2). Pure
// encode/decode; the socket poll-loop that drives it lives in `wifi.rs`.
#[cfg(feature = "wifi")]
mod mqtt;

#[cfg(feature = "espnow")]
pub mod mode;

// #13 routed multi-hop mesh: the PURE managed-flood decision core (SeenSet + forward
// decision + HopLatch escalation state machine), host-testable, no HAL deps. Driven by
// the relay path in `mode`, so espnow-gated. (wip: host-tested but not yet wired — see
// the module-level allow(dead_code) in flood.rs, dropped once mode.rs uses it.)
#[cfg(feature = "espnow")]
pub mod flood;

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

/// Install esp-wifi's heap ONCE. esp-alloc declares the `#[global_allocator]`
/// inside its own crate; this macro just adds a 72 KiB internal-RAM region to
/// it (the size the esp-wifi C3 examples use). Defined here so both the Phase 2
/// (`wifi`) and Phase 3 (`espnow`) code paths share a single heap region rather
/// than each reserving their own.
#[cfg(feature = "wifi")]
pub fn init_heap() {
    esp_alloc::heap_allocator!(size: 72 * 1024);
}

/// Phase-1 (default) placeholder used when no radio features are enabled: the
/// caller free-runs the clock from its compile-time start constant.
#[cfg(not(feature = "wifi"))]
pub fn try_time_sync() -> Option<u32> {
    None
}
