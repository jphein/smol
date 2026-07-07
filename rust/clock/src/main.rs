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

// Phase 3 stores inbound ESP-NOW messages as owned Strings for display.
#[cfg(feature = "espnow")]
extern crate alloc;

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_5X8, MonoTextStyleBuilder},
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

// Blue status LED on GPIO8 (Phase 3 drives it; the module itself only needs
// esp-hal GPIO, so it is always available for reuse).
#[cfg(feature = "espnow")]
mod led;

// LOCAL git-ignored WiFi credentials, used by the `wifi`/`espnow` radio bring-up.
// Fresh clones copy `src/secrets.rs.example` -> `src/secrets.rs` (see README).
#[cfg(feature = "wifi")]
mod secrets;

/// Compile-time clock start, encoded as seconds-since-midnight.
/// Phase 1 has no real-time source, so the clock free-runs from here.
/// (12:34:56 -> 12*3600 + 34*60 + 56 = 45296.) Phase 2 overwrites this at
/// runtime once SNTP returns a real Unix time.
const START_SECONDS_OF_DAY: u32 = 12 * 3600 + 34 * 60 + 56;
/// Local timezone offset from UTC, in seconds. Pacific is -7h (PDT, summer /
/// daylight time — correct for July); switch to `-8 * 3600` for PST in winter.
const TZ_OFFSET_SECONDS: i64 = -7 * 3600;

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
        .font(&FONT_10X20)
        .text_color(BinaryColor::On)
        .build();
    let label_style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    let delay = Delay::new();

    // Radio bring-up varies by build phase. Each branch produces `synced`
    // (Option<u32> Unix time) and, for Phase 3, a live ESP-NOW `radio`.
    //
    //  * Phase 1 (default):        no radio; `synced = None`.
    //  * Phase 2 (`wifi`):         WiFi burst -> SNTP -> Unix time.
    //  * Phase 3 (`espnow`):       WiFi burst for NTP, then TIME-SHARE the
    //                              single radio over to ESP-NOW for messaging.

    #[cfg(not(feature = "wifi"))]
    let synced = net::try_time_sync();

    #[cfg(all(feature = "wifi", not(feature = "espnow")))]
    let synced = net::try_time_sync(net::WifiPeripherals {
        timg0: peripherals.TIMG0,
        rng: peripherals.RNG,
        wifi: peripherals.WIFI,
    });

    // Phase 3: blue status LED on GPIO8. Create it FIRST (initialised to the
    // logical-OFF physical level so we never hold this strapping pin low through
    // a reset), then pass it into `mode::start` so it can fast-blink (~10 Hz)
    // during the WiFi/NTP burst. After start() returns, the render loop drives
    // it from the ESP-NOW peer state.
    #[cfg(feature = "espnow")]
    let mut led = led::Led::new(esp_hal::gpio::Output::new(
        peripherals.GPIO8,
        led::Led::off_level(),
        esp_hal::gpio::OutputConfig::default(),
    ));

    #[cfg(feature = "espnow")]
    let (mut radio, synced) = net::mode::start(
        net::WifiPeripherals {
            timg0: peripherals.TIMG0,
            rng: peripherals.RNG,
            wifi: peripherals.WIFI,
        },
        // This unit's short id, embedded in HELLO beacons ("SMOLv1 HELLO NNN").
        // Give each physical board a distinct value (we flashed 7 / 8 / 9).
        7,
        &mut led,
    );

    let mut seconds_of_day: u32 = match synced {
        Some(unix) => {
            // NTP gives UTC; shift to local (Pacific) for display.
            let local = ((unix as i64 + TZ_OFFSET_SECONDS).rem_euclid(86_400)) as u32;
            log::info!("smol: NTP {} UTC s-of-day -> local {} (Pacific)", unix % 86_400, local);
            local
        }
        None => {
            log::info!("smol: no NTP; clock free-runs from compile-time start");
            START_SECONDS_OF_DAY
        }
    };

    // Bottom-line label. In Phase 3 it is replaced by the last ESP-NOW peer
    // message (an owned String); in Phase 1/2 it stays the static "smol".
    #[cfg(feature = "espnow")]
    let mut bottom_line = alloc::string::String::from("smol");

    // --- Render loop ---------------------------------------------------------
    // The loop runs at a fast SUB-TICK (50 ms) so the blue LED can blink smoothly
    // (a ~10 Hz blink needs a 50 ms toggle; ~2 Hz needs 250 ms). The clock digits
    // and the OLED are only advanced/redrawn once per accumulated 1000 ms, so the
    // display still ticks exactly once per second and we don't hammer the I2C bus.
    //
    // Each 50 ms sub-tick (Phase 3): service ESP-NOW (drain RX, run the HELLO/ACK
    // handshake), periodically broadcast our HELLO beacon, then recompute the
    // peer state and push it to the LED at the current time so blinking is smooth
    // and non-blocking. Phase 1/2 have no LED and simply redraw the clock.
    const SUBTICK_MS: u32 = 50;
    #[cfg(feature = "espnow")]
    const SUBTICKS_PER_SEC: u32 = 1000 / SUBTICK_MS; // 20
    #[cfg(feature = "espnow")]
    const HELLO_EVERY_SUBTICKS: u32 = SUBTICKS_PER_SEC * 2; // broadcast HELLO ~every 2 s

    let mut buf = [0u8; 8]; // "HH:MM:SS"
    let mut ms_accum: u32 = 0; // time since last 1 s clock advance
    let mut first_frame = true; // draw once immediately at startup
    #[cfg(feature = "espnow")]
    let mut subtick: u32 = 0;
    // Last LED peer-state we logged, so we print only on transitions (gives a
    // serial-observable trace of the state machine without spamming every tick).
    #[cfg(feature = "espnow")]
    let mut last_led_state: Option<led::LedState> = None;
    loop {
        // --- Phase 3: ESP-NOW servicing + LED, every sub-tick ---------------
        #[cfg(feature = "espnow")]
        if let Some(r) = radio.as_mut() {
            // Drain inbound frames + advance the handshake; surface last activity.
            if let Some(text) = r.service() {
                bottom_line = text;
            }
            // Periodically advertise ourselves so peers can detect + ACK us.
            if subtick.is_multiple_of(HELLO_EVERY_SUBTICKS) {
                r.broadcast_hello();
            }
            // Reflect the current peer link state on the blue LED (off / slow
            // blink / solid), phased off the monotonic clock for smooth blink.
            let now = net::mode::now_ms();
            let state = r.peer_led_state(now);
            if last_led_state != Some(state) {
                log::info!("smol: LED -> {:?}", state);
                last_led_state = Some(state);
            }
            led.apply(state, now);
            subtick = subtick.wrapping_add(1);
        }

        // --- Advance + redraw the clock once per second ---------------------
        if first_frame || ms_accum >= 1000 {
            if !first_frame {
                seconds_of_day = (seconds_of_day + 1) % 86_400;
                ms_accum -= 1000;
            }
            first_frame = false;

            // Resolve the bottom-line &str for this frame (String view or static).
            #[cfg(feature = "espnow")]
            let bottom: &str = bottom_line.as_str();
            #[cfg(not(feature = "espnow"))]
            let bottom: &str = "smol";

            format_hms(seconds_of_day, &mut buf);
            // Blink the colon once per second so the big clock reads as "live".
            if seconds_of_day % 2 == 1 {
                buf[2] = b' ';
            }
            // BIG time line: "HH:MM" (5 chars * 10px = 50px) in the 10x20 font.
            let hm = core::str::from_utf8(&buf[0..5]).unwrap_or("--:--");

            display.clear(BinaryColor::Off).ok();

            // Center "HH:MM": (72-50)/2 = 11px left margin; 20px tall from y=2.
            Text::with_baseline(hm, Point::new(11, 2), time_style, Baseline::Top)
                .draw(&mut display)
                .ok();

            // Bottom line (5x8 font). Draw at x=2 so longer peer messages fit.
            Text::with_baseline(bottom, Point::new(2, 30), label_style, Baseline::Top)
                .draw(&mut display)
                .ok();

            display.flush().ok();
        }

        delay.delay_millis(SUBTICK_MS);
        ms_accum += SUBTICK_MS;
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
