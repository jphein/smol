//! #161 OTA screen — the dedicated "an update is crossing the mesh" display.
//!
//! Beyond #153's minimal 1-px bottom edge: when an OTA is in flight this takes the WHOLE
//! glass with a purpose-built screen so you can watch a board *become* its next image.
//! Three surfaces, one layout (72×40, 1-bit):
//!   * **Receiving** — a leaf pulling an image over ESP-NOW: `from <gateway> · N hop`.
//!   * **Feeding** — the crown relaying to a leaf: `feeding <leaf>`.
//!   * **SelfFetch** — a gateway fetching its own image over WiFi/HTTP: `from <host>`.
//!
//! Layout (top→bottom):
//! ```text
//!   y=0   FONT_6X10   incoming build's FORGE codename  ("Molten Engine")   ← the hero
//!   y=11  FONT_5X8    source / path                    ("from Forge id8")
//!   y=20  FONT_5X8    incoming build + count           ("v339  42/128")
//!   y=29  h=11        big dithered BLOCK bar: fill + segment dividers + leading cap,
//!                     with "NN%" (left) and "~ETA" (right) boxed-overlaid ON the bar
//! ```
//!
//! Pure PRESENTATION — no radio traffic, no flash, no wire change. `main` paints it over the
//! frozen app frame while an OTA is live (auto-activated by [`crate::net::mode::RadioManager::ota_rx_view`]
//! for the receive path, or the fetch/relay burst closures for the gateway paths).
//!
//! ## The codename caveat (parity)
//! The firmware's true FORGE version name (About/splash) is seeded from the git short HASH
//! (`BUILD_HASH`), which a receiving board can't know mid-transfer — the signed manifest
//! carries only the monotonic build *number*. So this codename is seeded from the build
//! NUMBER: deterministic + delightful + identical on every on-board surface, but it will NOT
//! match the hash-seeded name the board shows after it reboots. Threading the true target
//! name through the announce/OTAM is a future (viz/HA-parity) enhancement.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

use crate::app::Oled;
use crate::net::names::{name_for_seed, FORGE};
use crate::rssi::{clip, Line};

/// Panel geometry (matches the other screens).
const PANEL_W: i32 = 72;
const PANEL_H: i32 = 40;
/// Conservative per-glyph advance used for right-alignment + the boxed-label backgrounds.
/// FONT_6X10 is exactly 6; FONT_5X8 is 5 — using 6 for both over-reserves ≤1 px/char, which
/// only ever nudges a right-aligned label LEFT (never off the right edge) → always safe.
const CHAR_W: i32 = 6;
/// How many block dividers segment the progress bar (the "block k/n" texture).
const BAR_SEGMENTS: i32 = 8;

/// What kind of OTA this board is part of — picks the source/direction line.
pub enum OtaKind<'a> {
    /// This board is RECEIVING an image over the mesh from `source_id` (`None` = the feeding
    /// MAC isn't in the roster yet), `hop` ESP-NOW hops away.
    Receiving { source_id: Option<u8>, hop: u8 },
    /// This gateway (crown) is FEEDING the image to leaf `leaf_id` over ESP-NOW.
    Feeding { leaf_id: u8 },
    /// This gateway is fetching its OWN image directly over WiFi/HTTP from `host`.
    SelfFetch { host: &'a str },
}

/// The transfer's counting unit — drives the `k/n` readout (`Blocks`) vs a `KB` readout
/// (`Bytes`). ETA is unit-agnostic (computed from the ratio over time).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OtaUnit {
    Blocks,
    Bytes,
}

/// A fully-specified on-board OTA screen. `build` = the incoming image's build number;
/// `done`/`total` = the transfer counts in `unit`; `eta_s` = the display-computed ETA in
/// seconds (`None` until a rate is measurable).
pub struct OtaView<'a> {
    pub kind: OtaKind<'a>,
    pub build: u32,
    pub done: u32,
    pub total: u32,
    pub eta_s: Option<u32>,
    pub unit: OtaUnit,
}

/// Display-side ETA estimator (no float — the C3 is soft-float). Averages the transfer rate
/// since the moment real progress began (re-anchoring through any pre-progress wait, e.g. the
/// gateway's off-channel fetch), so the ETA is stable and never depressed by the armed idle.
/// One per in-flight transfer, reset when it ends.
pub struct OtaEta {
    /// `(done, now_ms)` anchor — moved forward while `done` is unchanged, then frozen once
    /// blocks start flowing so the average rate is measured over the ACTIVE window only.
    anchor: Option<(u32, u64)>,
}

impl Default for OtaEta {
    fn default() -> Self {
        Self::new()
    }
}

impl OtaEta {
    pub const fn new() -> Self {
        Self { anchor: None }
    }

    /// Drop the anchor (call when a transfer ends) so the next one starts fresh.
    pub fn reset(&mut self) {
        self.anchor = None;
    }

    /// Feed the latest `(done, total, now_ms)`; returns the ETA in seconds to reach `total`,
    /// or `None` until there is measurable progress. Integer-only, overflow-safe (`u64` math).
    pub fn sample(&mut self, done: u32, total: u32, now_ms: u64) -> Option<u32> {
        match self.anchor {
            // Real progress since the anchor → average rate = Δdone / Δt.
            Some((d0, t0)) if done > d0 && total > done => {
                let dd = (done - d0) as u64;
                let dt = now_ms.saturating_sub(t0).max(1);
                let remaining = (total - done) as u64;
                Some((remaining.saturating_mul(dt) / dd / 1000) as u32)
            }
            // No progress yet (first call, or still awaiting the first block) → (re)anchor at
            // NOW so the pre-progress wait never enters the average.
            _ => {
                self.anchor = Some((done, now_ms));
                None
            }
        }
    }
}

fn style_5x8() -> MonoTextStyle<'static, BinaryColor> {
    MonoTextStyleBuilder::new().font(&FONT_5X8).text_color(BinaryColor::On).build()
}
fn style_6x10() -> MonoTextStyle<'static, BinaryColor> {
    MonoTextStyleBuilder::new().font(&FONT_6X10).text_color(BinaryColor::On).build()
}

/// Draw `text` at `(x, y)` on a cleared (Off) background box so it stays crisp when overlaid on
/// the dithered bar. The box is sized from the (already-clipped) text and kept 9 px tall so it
/// never nicks the bar's 1-px top/bottom outline. Bounded to the panel — panic-free.
fn boxed_label<D>(display: &mut D, text: &str, x: i32, y: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let w = text.chars().count() as i32 * CHAR_W;
    if w <= 0 {
        return;
    }
    let bx = (x - 1).max(0);
    let bw = (w + 2).min(PANEL_W - bx);
    Rectangle::new(Point::new(bx, y - 1), Size::new(bw.max(0) as u32, 9))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
        .draw(display)
        .ok();
    Text::with_baseline(text, Point::new(x, y), style_5x8(), Baseline::Top)
        .draw(display)
        .ok();
}

/// The big BLOCK progress bar: full-width outlined track, checkerboard-dithered fill
/// proportional to `done/total`, `BAR_SEGMENTS` divider ticks (the "blocks"), and a crisp
/// 1-px leading-edge cap. Mirrors the Finder's dither aesthetic so the fleet reads as one UI.
/// Bounded (≤ track) + panic-free.
fn draw_block_bar<D>(display: &mut D, y: i32, h: i32, done: u32, total: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    // Outlined full-width track.
    Rectangle::new(Point::new(0, y), Size::new(PANEL_W as u32, h as u32))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(display)
        .ok();

    let inner_w = PANEL_W - 2; // inside the 1-px border
    let fill_w = if total > 0 {
        ((done.min(total) as u64 * inner_w as u64) / total as u64) as i32
    } else {
        0
    };

    // Checkerboard dither of the filled region (inside the border).
    let x_end = 1 + fill_w.clamp(0, inner_w);
    for yy in (y + 1)..(y + h - 1) {
        for xx in 1..x_end {
            if (xx + yy) & 1 == 0 {
                Pixel(Point::new(xx, yy), BinaryColor::On).draw(display).ok();
            }
        }
    }

    // Segment dividers: 1-px vertical ticks at each block boundary. On in the UNFILLED part
    // (faint ruler) and Off in the FILLED part (a clean gap in the dither) → reads as blocks.
    for k in 1..BAR_SEGMENTS {
        let xx = 1 + (k * inner_w) / BAR_SEGMENTS;
        if xx <= 0 || xx >= PANEL_W - 1 {
            continue;
        }
        let color = if xx < x_end { BinaryColor::Off } else { BinaryColor::On };
        // Faint: only the middle rows, so the outline stays intact.
        for yy in (y + 2)..(y + h - 2) {
            Pixel(Point::new(xx, yy), color).draw(display).ok();
        }
    }

    // Crisp leading-edge cap so the moving front is unambiguous.
    if fill_w > 0 {
        let cap_x = x_end.clamp(1, PANEL_W - 2);
        Rectangle::new(Point::new(cap_x, y + 1), Size::new(1, (h - 2).max(1) as u32))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(display)
            .ok();
    }
}

/// Format the ETA compactly (≤5 chars): `~45s` under 100 s, else `~Nm` minutes.
fn fmt_eta(line: &mut Line, eta_s: u32) {
    if eta_s < 100 {
        let _ = write!(line, "~{}s", eta_s);
    } else {
        let _ = write!(line, "~{}m", eta_s.div_ceil(60));
    }
}

/// Paint the full OTA screen. `display` is the concrete panel (we own the clear + flush, like
/// the other full-screen draws). Alloc-free; every string is clipped to the 1-bit glass.
pub fn draw(display: &mut Oled, v: &OtaView) {
    display.clear(BinaryColor::Off).ok();

    // --- y=0: incoming build's FORGE codename (the hero — "what you're becoming"). --------
    // Build-NUMBER-seeded (see the module caveat), so it is identical on every on-board surface.
    let (adj, noun) = name_for_seed(v.build, &FORGE);
    let mut hero = Line::new();
    let _ = write!(hero, "{} {}", adj, noun);
    Text::with_baseline(clip(hero.as_str(), 12), Point::new(0, 0), style_6x10(), Baseline::Top)
        .draw(display)
        .ok();

    // --- y=11: source / path line (kind-specific). ----------------------------------------
    let mut src = Line::new();
    match &v.kind {
        OtaKind::Receiving { source_id, hop } => {
            match source_id {
                Some(id) => {
                    let _ = write!(src, "from {} i{}", name_for_seed_id_noun(*id), id);
                }
                None => {
                    let _ = write!(src, "from mesh");
                }
            }
            // Single-hop today (gateway→leaf); surface the distance only if a future multi-hop
            // relay ever feeds from farther out.
            if *hop > 1 {
                let _ = write!(src, " {}h", hop);
            }
        }
        OtaKind::Feeding { leaf_id } => {
            let _ = write!(src, "feed {} i{}", name_for_seed_id_noun(*leaf_id), leaf_id);
        }
        OtaKind::SelfFetch { host } => {
            let _ = write!(src, "from {}", host);
        }
    }
    Text::with_baseline(clip(src.as_str(), 14), Point::new(0, 11), style_5x8(), Baseline::Top)
        .draw(display)
        .ok();

    // --- y=20: incoming build + count. ----------------------------------------------------
    let mut stat = Line::new();
    match v.unit {
        OtaUnit::Blocks => {
            let _ = write!(stat, "v{} {}/{}", v.build, v.done, v.total);
        }
        OtaUnit::Bytes => {
            let _ = write!(stat, "v{} {}/{}K", v.build, v.done / 1024, v.total / 1024);
        }
    }
    Text::with_baseline(clip(stat.as_str(), 14), Point::new(0, 20), style_5x8(), Baseline::Top)
        .draw(display)
        .ok();

    // --- y=29: the big block bar + boxed % (left) and ETA (right) overlays. ----------------
    let bar_y = 29;
    let bar_h = PANEL_H - bar_y; // 11 → fills to the bottom row
    draw_block_bar(display, bar_y, bar_h, v.done, v.total);

    let pct = if v.total > 0 {
        (v.done.min(v.total) as u64 * 100 / v.total as u64) as u32
    } else {
        0
    };
    let mut pl = Line::new();
    let _ = write!(pl, "{}%", pct);
    boxed_label(display, pl.as_str(), 2, bar_y + 2);

    if let Some(eta) = v.eta_s {
        let mut el = Line::new();
        fmt_eta(&mut el, eta);
        let w = el.as_str().chars().count() as i32 * CHAR_W;
        boxed_label(display, el.as_str(), (PANEL_W - w - 2).max(0), bar_y + 2);
    }

    display.flush().ok();
}

/// The magical NOUN for a node id (the on-screen handle used across the UI). A thin wrapper so
/// the source-line formatter reads cleanly.
fn name_for_seed_id_noun(id: u8) -> &'static str {
    crate::net::names::name_for_id(id).1
}
