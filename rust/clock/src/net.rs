//! Network module: WiFi + SNTP (Phase 2) and ESP-NOW + radio switching
//! (Phase 3). Everything here is feature-gated so the default Phase 1 build
//! pulls in none of the esp-wifi stack.
//!
//! Design note: `esp_hal::init()` may only run once and hands out the
//! peripheral singletons. `main` owns that call and passes the radio-related
//! peripherals into this module, so we never double-initialise the HAL.

#[cfg(feature = "wifi")]
mod wifi;

#[cfg(feature = "espnow")]
pub mod mode;

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
