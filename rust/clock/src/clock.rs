//! CLOCK screen — big 12-hour HH:MM + an alternating sensor/label bottom line.
//!
//! Extracted from `main` into a [`Plugin`] (issue #7). The state is the single
//! `last_sec` dedup latch the old `main` kept as `last_clock_sec`; the render is
//! `draw_clock` moved out of `main` VERBATIM (it now builds its own text styles
//! instead of receiving them, since the render loop no longer threads styles —
//! same fonts, same positions, so the screen is byte-identical). Compiled into
//! every build (needs only the display + sensors + the Unix estimate in `Ctx`).

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;
use crate::sensors;

/// CLOCK state: the last seconds-of-day we painted, so the panel redraws exactly
/// once per second (or on a forced redraw). Was `main`'s `last_clock_sec`.
pub struct ClockState {
    last_sec: Option<u32>,
}

impl ClockState {
    pub fn new() -> Self {
        Self { last_sec: None }
    }
}

impl Plugin for ClockState {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // CLOCK ignores short taps (as it always has).
            Press::Short => Transition::Stay,
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Derive the current seconds-of-day from the mesh/anchor Unix estimate
        // `main` computed. Equivalence to the old `(base_unix + TZ + elapsed) mod
        // 86_400`: `ctx.unix_now == base_unix + elapsed` (u32, no wrap for years),
        // so `(unix_now + TZ) mod 86_400 == (base_unix + TZ + elapsed) mod 86_400`
        // for both the synced and free-run base — representation change, not
        // behaviour change. `i64 + rem_euclid` stays correct with a negative TZ.
        let sod = ((ctx.unix_now as i64) + crate::TZ_OFFSET_SECONDS).rem_euclid(86_400) as u32;

        // Redraw once per second (or right after a mode switch / adoption).
        if ctx.redraw || self.last_sec != Some(sod) {
            self.last_sec = Some(sod);
            ctx.display.clear(BinaryColor::Off).ok();
            draw_clock(
                ctx.display,
                sod,
                ctx.sensors,
                #[cfg(feature = "espnow")]
                ctx.label,
            );
            ctx.display.flush().ok();
        }
    }
}

/// Render the CLOCK: big HH:MM (FONT_10X20) with a blinking colon, plus a bottom
/// line that alternates every few seconds between the label (ESP-NOW peer
/// message under `espnow`, else our own noun) and the compact sensor readout.
/// Moved out of `main` verbatim; builds its own styles (the loop no longer
/// passes them). Generic over the draw target so it stays testable in principle.
fn draw_clock<D>(
    display: &mut D,
    sod: u32,
    sensors: &mut sensors::Sensors,
    #[cfg(feature = "espnow")] label: &str,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    let time_style = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(BinaryColor::On)
        .build();
    let label_style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    // Refresh the sensor sample (chip °C + battery V).
    let reading = sensors.read();
    let sensor_line = sensors::format_sensor_line(&reading);
    log::debug!(
        "smol: chip {}C, batt {:.2}V (~{}%)",
        reading.chip_c as i32,
        reading.batt_v,
        reading.batt_pct,
    );

    // Bottom line alternates every SENSOR_LINE_EVERY_S seconds.
    const SENSOR_LINE_EVERY_S: u32 = 4;
    let show_sensors = (sod / SENSOR_LINE_EVERY_S) % 2 == 1;

    // No radio -> no peer chatter, so the bottom line is simply our own name (the
    // node's noun, derived from its id). Matches the espnow build's idle label.
    #[cfg(not(feature = "espnow"))]
    let label: &str = crate::net::names::name_for_id(crate::node_id()).1;

    let bottom: &str = if show_sensors {
        sensor_line.as_str()
    } else {
        label
    };

    // 12-hour clock with AM/PM and a colon that blinks once per second.
    let h24 = (sod / 3600) % 24;
    let mm = (sod / 60) % 60;
    let pm = h24 >= 12;
    let h12 = {
        let h = h24 % 12;
        if h == 0 { 12 } else { h }
    };
    let colon = if sod % 2 == 1 { b' ' } else { b':' };
    // Build "H:MM" or "HH:MM" (no leading zero on the hour) — 4 or 5 chars.
    let mut tb = [0u8; 5];
    let mut ti = 0usize;
    if h12 >= 10 {
        tb[ti] = b'1';
        ti += 1;
    }
    tb[ti] = b'0' + (h12 % 10) as u8;
    ti += 1;
    tb[ti] = colon;
    ti += 1;
    tb[ti] = b'0' + (mm / 10) as u8;
    ti += 1;
    tb[ti] = b'0' + (mm % 10) as u8;
    ti += 1;
    let hm = core::str::from_utf8(&tb[..ti]).unwrap_or("--:--");
    // Center: "12:34" (5ch/50px) -> x=11; "1:34" (4ch/40px) -> x=16.
    let tx = if h12 >= 10 { 11 } else { 16 };

    // AM/PM small in the top-right (its own row above the big digits — no overlap).
    let ampm = if pm { "PM" } else { "AM" };
    Text::with_baseline(ampm, Point::new(59, 0), label_style, Baseline::Top)
        .draw(display)
        .ok();
    // Big 12-hour time, 20px tall from y=8 (AM/PM row sits above it).
    Text::with_baseline(hm, Point::new(tx, 8), time_style, Baseline::Top)
        .draw(display)
        .ok();
    // Bottom line at x=2 so longer peer messages fit.
    Text::with_baseline(bottom, Point::new(2, 31), label_style, Baseline::Top)
        .draw(display)
        .ok();
}
