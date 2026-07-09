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

// #25 WLED WiZmote-emit (smol as a WLED "linked remote"). `wled = ["espnow"]`, so
// this is present only in a wled build; the default/wifi/espnow builds are byte-free
// of it (the module is `#![cfg(feature = "wled")]`). Referenced by `app` (the
// WledRemote screen) + `mode` (broadcast_wled_button), so it is `pub`.
#[cfg(feature = "wled")]
pub mod wled;

// Deterministic magical node names (realm-sigil port). Needs no radio — a node
// derives its OWN name and any peer's name from the logical id alone — so it is
// compiled in ALL builds (peer names are only *displayed* under espnow, but our
// own name is the idle bottom-line label everywhere).
pub mod names;

#[cfg(feature = "wifi")]
pub use wifi::WifiPeripherals;

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
