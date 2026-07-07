//! smol — ESP32-C3 SuperMini + 0.42" SSD1306 (72x40) OLED clock.
//!
//! Wiring (I2C):
//!   SDA = GPIO5, SCL = GPIO6, address 0x3C, 0.42" 72x40 panel.
//!
//! Build phases (see README.md):
//!   * Phase 1 (default features): free-running clock rendered on the OLED,
//!     counting up from a compile-time start constant using a blocking Delay.
//!   * Phase 2 (`--features wifi`):   WiFi STA + SNTP real-time sync.
//!   * Phase 3 (`--features espnow`): ESP-NOW peer messaging + radio switching.
//!
//! Phases 2 and 3 live in the `net` module and are compiled only when their
//! feature is enabled, so the default build is the guaranteed-green baseline.

#![no_std]
#![no_main]

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    i2c::master::{Config as I2cConfig, I2c},
    main,
    time::Rate,
};
use ssd1306::{
    mode::DisplayConfig, prelude::*, size::DisplaySize72x40, I2CDisplayInterface, Ssd1306,
};

// Phase 2/3 code (WiFi, SNTP, ESP-NOW, radio switching). Compiled only under
// the `wifi` / `espnow` features; a no-op placeholder otherwise so the rest of
// this file can reference it unconditionally without cfg noise.
mod net;

/// Compile-time clock start, encoded as seconds-since-midnight.
/// Phase 1 has no real-time source, so the clock free-runs from here.
/// (12:34:56 -> 12*3600 + 34*60 + 56 = 45296.) Phase 2 overwrites this at
/// runtime once SNTP returns a real Unix time.
const START_SECONDS_OF_DAY: u32 = 12 * 3600 + 34 * 60 + 56;

#[main]
fn main() -> ! {
    // --- Clocks & peripherals ------------------------------------------------
    // Run the single RISC-V core at its maximum frequency (160 MHz on the C3).
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    esp_println::logger::init_logger_from_env();
    log::info!("smol booting: Phase 1 clock");

    // --- I2C bus to the OLED -------------------------------------------------
    // SSD1306 is happy at 400 kHz fast-mode I2C.
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("I2C init")
    .with_sda(peripherals.GPIO5)
    .with_scl(peripherals.GPIO6);

    // --- SSD1306 display -----------------------------------------------------
    // The 0.42" glass exposes a 72x40 window inside the controller's 128x64
    // RAM; `DisplaySize72x40` applies the correct column/row offset internally.
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize72x40, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().expect("display init");

    // Text styles: 6x10 for the big HH:MM:SS line, 5x8 for the little label.
    let time_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let label_style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    let delay = Delay::new();

    // Give WiFi/SNTP (Phase 2+) a chance to correct the clock. Under the
    // default (Phase 1) build this is a no-op returning `None`; under
    // `--features wifi` it brings the radio up for a burst SNTP query.
    #[cfg(not(feature = "wifi"))]
    let synced = net::try_time_sync();

    #[cfg(feature = "wifi")]
    let synced = net::try_time_sync(net::WifiPeripherals {
        timg0: peripherals.TIMG0,
        rng: peripherals.RNG,
        wifi: peripherals.WIFI,
    });

    let mut seconds_of_day: u32 = match synced {
        Some(unix) => unix % 86_400,
        None => START_SECONDS_OF_DAY,
    };

    // --- Render loop ---------------------------------------------------------
    // One tick per second: clear the framebuffer, draw HH:MM:SS + label, flush.
    let mut buf = [0u8; 8]; // "HH:MM:SS"
    loop {
        format_hms(seconds_of_day, &mut buf);
        let hms = core::str::from_utf8(&buf).unwrap_or("--:--:--");

        display.clear(BinaryColor::Off).ok();

        // Center "HH:MM:SS": 8 chars * 6px = 48px wide on a 72px panel ->
        // left margin (72-48)/2 = 12. Vertically place the big line ~row 14.
        Text::with_baseline(hms, Point::new(12, 14), time_style, Baseline::Top)
            .draw(&mut display)
            .ok();

        // Small "smol" label centered on the bottom: 4 chars * 5px = 20px ->
        // left margin (72-20)/2 = 26.
        Text::with_baseline("smol", Point::new(26, 30), label_style, Baseline::Top)
            .draw(&mut display)
            .ok();

        display.flush().ok();

        delay.delay_millis(1000);
        seconds_of_day = (seconds_of_day + 1) % 86_400;
    }
}

/// Format seconds-of-day into an 8-byte `HH:MM:SS` ASCII buffer (no heap).
fn format_hms(sod: u32, out: &mut [u8; 8]) {
    let h = (sod / 3600) % 24;
    let m = (sod / 60) % 60;
    let s = sod % 60;
    out[0] = b'0' + (h / 10) as u8;
    out[1] = b'0' + (h % 10) as u8;
    out[2] = b':';
    out[3] = b'0' + (m / 10) as u8;
    out[4] = b'0' + (m % 10) as u8;
    out[5] = b':';
    out[6] = b'0' + (s / 10) as u8;
    out[7] = b'0' + (s % 10) as u8;
}
