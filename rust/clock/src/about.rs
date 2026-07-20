//! ABOUT screen — identity + provenance, and the home of the (stubbed) OTA
//! action (issue #7 / ota-ux-design.md §1).
//!
//! Shows WHO this node is (its magical noun, from `NODE_ID`), WHICH firmware it
//! runs (the sigil FORGE version name + build number, from `build.rs`), its MAC
//! (read once from eFuse), and its uptime. Compiled into EVERY build — identity
//! needs no radio.
//!
//! OTA: the spec folds "Update" into About as its ACTION rather than a separate
//! menu entry (which would overflow the 40 px menu). Under the uniform
//! single-button grammar a **long press is reserved for "back to Menu"**, so the
//! OTA trigger is the **short tap** (the ota-ux doc's "hold to check" reconciled
//! to the framework's gesture contract). The flow itself is a DOCUMENTED STUB
//! this pass — a tap logs intent; the WiFi burst / streamed image / rollback are
//! deferred to the OTA implementation slot (see ota-ux-design.md §2–§4).
//!
//! About sits at the tail of every menu; with the Batt screen (cfg wifi) added
//! ahead of it the `wifi` menu is FOUR entries (Clock / Snake / Batt / About) and
//! the `espnow` menu FIVE (… / Bench / Batt / About) — both exercise the scrolling
//! menu window in `menu.rs` (default stays at 3 and never scrolls).

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// ABOUT state: the MAC (read once at entry — it never changes) + the last
/// uptime-second painted, so the screen re-renders once per second (live uptime)
/// or on a forced redraw, mirroring the CLOCK dedup.
pub struct About {
    mac: [u8; 6],
    last_s: Option<u32>,
}

impl About {
    /// Read the MAC once from eFuse (the ota-ux `on_enter` one-time setup). Takes
    /// the keystone's `now_ms` for signature parity with `App::enter`; uptime is
    /// derived from `ctx.now_ms` (true since-boot uptime — `millis()`'s epoch is
    /// boot), so the entry stamp is reserved, not needed.
    pub fn new(_now_ms: u64) -> Self {
        Self {
            // #233: esp-hal 1.1 replaced `Efuse::read_base_mac_address() -> [u8;6]` with the
            // free fn `efuse::base_mac_address() -> MacAddress` (newtype; `.as_bytes()` → &[u8;6]).
            mac: esp_hal::efuse::base_mac_address()
                .as_bytes()
                .try_into()
                .unwrap_or([0u8; 6]),
            last_s: None,
        }
    }
}

impl Plugin for About {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // Short tap = the OTA action. DOCUMENTED STUB: the check/download/
            // verify/rollback flow (ota-ux §2–§4) is deferred; log the intent so
            // the affordance is real and testable without pretending to update.
            Press::Short => {
                log::info!(
                    "smol: About — OTA update requested (stub; not yet implemented — see ota-ux-design.md)"
                );
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // `millis()` (== ctx.now_ms) is time-since-boot, so uptime is just it in
        // seconds — no separate boot stamp needed. Repaint once/second for a live
        // uptime, or on a forced redraw (menu entry).
        let up_s = (ctx.now_ms / 1000) as u32;
        if !(ctx.redraw || self.last_s != Some(up_s)) {
            return;
        }
        self.last_s = Some(up_s);

        let title = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(BinaryColor::On)
            .build();
        let small = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::On)
            .build();

        ctx.display.clear(BinaryColor::Off).ok();

        // WHO — the node noun (identity), matching the menu title + boot splash.
        let noun = crate::net::names::name_for_id(crate::node_id()).1;
        Text::with_baseline(noun, Point::new(2, 0), title, Baseline::Top)
            .draw(ctx.display)
            .ok();

        // WHICH — "v<build> <forge-noun>" (provenance).
        let vnoun = crate::net::names::version_name().1;
        let mut l1 = Line::new();
        let _ = write!(l1, "v{} {}", env!("BUILD_NUMBER"), vnoun);
        Text::with_baseline(l1.as_str(), Point::new(2, 12), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        // MAC — the last 3 bytes (the chip-unique tail; the full 6 don't fit).
        let mut l2 = Line::new();
        let _ = write!(
            l2,
            "M {:02X}{:02X}{:02X}",
            self.mac[3], self.mac[4], self.mac[5]
        );
        Text::with_baseline(l2.as_str(), Point::new(2, 21), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        // Uptime (live) + a tap hint that the OTA action lives here.
        let mut l3 = Line::new();
        write_uptime(&mut l3, up_s);
        Text::with_baseline(l3.as_str(), Point::new(2, 30), small, Baseline::Top)
            .draw(ctx.display)
            .ok();
        // The OTA affordance is only meaningful where a WiFi burst is possible;
        // gate the hint (the tap stub itself is harmless in any build).
        #[cfg(feature = "wifi")]
        Text::with_baseline("OTA?", Point::new(48, 30), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        ctx.display.flush().ok();
    }
}

/// Write a compact uptime (`up 45s` / `up 12m` / `up 3h04m`) into `out`.
fn write_uptime(out: &mut Line, up_s: u32) {
    let h = up_s / 3600;
    let m = (up_s / 60) % 60;
    let s = up_s % 60;
    if h > 0 {
        let _ = write!(out, "up {}h{:02}m", h, m);
    } else if m > 0 {
        let _ = write!(out, "up {}m", m);
    } else {
        let _ = write!(out, "up {}s", s);
    }
}

use core::fmt::Write;

/// Tiny heap-free line builder (About is in the alloc-free default build too).
struct Line {
    buf: [u8; 20],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self { buf: [0; 20], len: 0 }
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
