//! BENCH mode — live ESP-NOW link statistics (the mesh test).
//!
//! Compiled only under `--features espnow` (it is the one mode that exercises
//! the ESP-NOW mesh). It renders a compact multi-line `FONT_5X8` readout of the
//! link stats gathered by [`crate::net::mode::RadioManager`]:
//!
//!   * **FPS**  — render rate, measured by `main`'s loop and passed in.
//!   * **TX/s** — BENCH BEACONs we broadcast per second.
//!   * **RX/s** — peer BEACONs received per second.
//!   * **RTT**  — round-trip time (ms) from an echoed seq (`--` until measured).
//!   * **LOSS** — packet-loss percent inferred from peer seq gaps.
//!   * **RSSI** — last peer BEACON RSSI in dBm (`--` until a peer is heard).
//!   * **LINK** — peer link state (IDLE / SEEN / LINK) from the LED handshake.
//!
//! The stats themselves live in `net::mode` (piggybacked on the ESP-NOW path);
//! this module is purely the on-OLED presentation. See that module for how RTT /
//! loss / RSSI are actually measured.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::led::LedState;
use crate::net::mode::BenchStats;

/// Fixed-capacity heap-free line builder for one 72 px OLED row (~14 chars in
/// `FONT_5X8`). Overflow is silently dropped — every line we build is bounded
/// and short by construction, and a clipped stat is cosmetic, never fatal.
struct Line {
    buf: [u8; 24],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            buf: [0; 24],
            len: 0,
        }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Short label for the current peer link state (fits the compact readout).
fn link_str(link: LedState) -> &'static str {
    match link {
        LedState::Connected => "LINK",
        LedState::PeerDetected => "SEEN",
        // WifiSync only occurs during the boot burst, before BENCH runs; treat
        // anything else as idle for display purposes.
        _ => "IDLE",
    }
}

/// Draw the BENCH readout. `fps` is the render loop's measured frames-per-second
/// (the one stat this module's data source can't see); `s` is everything else.
/// Five `FONT_5X8` rows fit the 40 px height (8 px each). Individual draw errors
/// are ignored (a dropped pixel must never panic the firmware).
pub fn draw<D>(display: &mut D, s: &BenchStats, fps: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    // Build the five lines into stack buffers (no heap).
    let mut l0 = Line::new();
    let _ = write!(l0, "FPS{} TX{}", fps, s.tx_per_s);

    let mut l1 = Line::new();
    let _ = write!(l1, "RX{} LOS{}%", s.rx_per_s, s.loss_pct);

    let mut l2 = Line::new();
    match s.rtt_ms {
        Some(rtt) => {
            let _ = write!(l2, "RTT{}ms", rtt);
        }
        None => {
            let _ = write!(l2, "RTT--");
        }
    }

    let mut l3 = Line::new();
    match s.rssi {
        Some(rssi) => {
            let _ = write!(l3, "RSSI{}", rssi);
        }
        None => {
            let _ = write!(l3, "RSSI--");
        }
    }

    let mut l4 = Line::new();
    let _ = write!(l4, "LINK {}", link_str(s.link));

    // Lay the rows out at 8 px pitch from the top.
    let lines = [l0.as_str(), l1.as_str(), l2.as_str(), l3.as_str(), l4.as_str()];
    for (i, text) in lines.iter().enumerate() {
        Text::with_baseline(
            text,
            Point::new(0, i as i32 * 8),
            style,
            Baseline::Top,
        )
        .draw(display)
        .ok();
    }
}
