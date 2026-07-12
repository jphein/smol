//! Treasure-hunt (#60) — an RSSI warmer/colder game: pick a target node, walk
//! toward it guided by signal strength.
//!
//! `espnow`-only. A pure consumer of [`crate::net::mode::RadioManager::roster`] —
//! **no new frames, no radio traffic** (the target just broadcasts its ordinary
//! HELLO/BEACON, which every node already does). Shares the smoothing + proximity
//! mapping with the Watch ([`crate::rssi`]).
//!
//! One-button UX: **short tap** cycles the target to the next peer, **long press**
//! returns to the Menu. The big warmer/colder trend (smoothed RSSI now vs ~1.5 s
//! ago, with a ±2 dB deadband) is the hero — readable while walking. Cheat-/jitter-
//! resistance: EWMA smoothing + the deadband + a hold-to-confirm FOUND so a single
//! lucky spike can't declare victory.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;
use crate::net::mode::RosterView;
use crate::net::names::name_for_id;
use crate::rssi::{bar_px, clip, label, proximity, Line, RssiSmoother};

/// Compare the smoothed RSSI against its value this many ms ago for the trend.
const TREND_LAG_MS: u64 = 1500;
/// Trend deadband (dB): |delta| below this reads as SAME, not warmer/colder — below
/// the smoothed reading's noise floor, so standing still never flickers.
const TREND_DEADBAND: i32 = 2;
/// Smoothed RSSI (dBm) at/above which the target counts as "found"…
const FOUND_RSSI: i32 = -40;
/// …once held for this long (a lucky spike can't declare victory).
const FOUND_HOLD_MS: u64 = 1000;

pub struct HuntState {
    smoother: RssiSmoother,
    /// Current target node id (None until a peer exists to hunt).
    target: Option<u8>,
    /// Smoothed RSSI captured ~`TREND_LAG_MS` ago (the trend reference).
    trend_ref: i32,
    trend_ref_ms: u64,
    /// When the smoothed RSSI first crossed `FOUND_RSSI` (None = not currently over).
    found_since_ms: Option<u64>,
}

impl Default for HuntState {
    fn default() -> Self {
        Self::new()
    }
}

impl HuntState {
    pub fn new() -> Self {
        Self {
            smoother: RssiSmoother::new(),
            target: None,
            trend_ref: -100,
            trend_ref_ms: 0,
            found_since_ms: None,
        }
    }

    /// Reset the trend + found state to the current reading — called when the target
    /// changes so a fresh hunt never inherits the previous target's warmth.
    fn arm(&mut self, rssi: i32, now: u64) {
        self.trend_ref = rssi;
        self.trend_ref_ms = now;
        self.found_since_ms = None;
    }

    /// The id-bearing peer ids in the roster, in order (RSSI-desc). Returns the
    /// filled prefix + count (heap-free).
    fn known_ids(roster: &RosterView) -> ([u8; 16], usize) {
        let mut ids = [0u8; 16];
        let mut n = 0;
        for node in &roster.nodes[..roster.count] {
            if node.id_known && n < ids.len() {
                ids[n] = node.id;
                n += 1;
            }
        }
        (ids, n)
    }
}

fn style_5x8() -> embedded_graphics::mono_font::MonoTextStyle<'static, BinaryColor> {
    MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build()
}
fn style_6x10() -> embedded_graphics::mono_font::MonoTextStyle<'static, BinaryColor> {
    MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build()
}

impl Plugin for HuntState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                // Cycle the target to the next id-bearing peer (wraps).
                let roster = match ctx.radio.as_deref() {
                    Some(r) => r.roster(ctx.now_ms),
                    None => return Transition::Stay,
                };
                let (ids, n) = Self::known_ids(&roster);
                if n > 0 {
                    let next = match self.target.and_then(|t| ids[..n].iter().position(|&i| i == t))
                    {
                        Some(pos) => ids[(pos + 1) % n],
                        None => ids[0],
                    };
                    self.target = Some(next);
                    // Re-arm the trend from the new target's known/last reading.
                    let seed = self.smoother.get(next).unwrap_or(-100);
                    self.arm(seed, ctx.now_ms);
                }
                ctx.redraw = true;
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let roster = match ctx.radio.as_deref_mut() {
            Some(r) => r.roster(ctx.now_ms),
            None => return,
        };
        let now = ctx.now_ms;

        // Acquire a target on first sight (strongest id-bearing peer = nodes[0..]).
        if self.target.is_none() {
            let (ids, n) = Self::known_ids(&roster);
            if n > 0 {
                self.target = Some(ids[0]);
                self.arm(self.smoother.get(ids[0]).unwrap_or(-100), now);
            }
        }

        // Locate the target in the current roster; update its EWMA if present.
        let (present, smoothed) = match self.target {
            Some(t) => match roster.nodes[..roster.count].iter().find(|n| n.id_known && n.id == t) {
                Some(node) => (true, self.smoother.update(t, node.rssi)),
                None => (false, self.smoother.get(t).unwrap_or(-100)),
            },
            None => (false, -100),
        };

        // Trend: compare to the ~TREND_LAG_MS-ago reference, then roll the reference.
        let delta = smoothed - self.trend_ref;
        if now.saturating_sub(self.trend_ref_ms) >= TREND_LAG_MS {
            self.trend_ref = smoothed;
            self.trend_ref_ms = now;
        }

        // FOUND: hold-to-confirm above the threshold (only while present).
        let found = if present && smoothed >= FOUND_RSSI {
            let since = *self.found_since_ms.get_or_insert(now);
            now.saturating_sub(since) >= FOUND_HOLD_MS
        } else {
            self.found_since_ms = None;
            false
        };

        // ---- render ----------------------------------------------------------
        ctx.display.clear(BinaryColor::Off).ok();

        // Row 0: "SEEK <Noun>".
        let mut hdr = Line::new();
        match self.target {
            Some(t) => {
                let _ = write!(hdr, "SEEK {}", clip(name_for_id(t).1, 7));
            }
            None => {
                let _ = write!(hdr, "SEEK --");
            }
        }
        Text::with_baseline(hdr.as_str(), Point::new(0, 0), style_5x8(), Baseline::Top)
            .draw(ctx.display)
            .ok();

        if self.target.is_none() {
            Text::with_baseline("no peers", Point::new(0, 16), style_6x10(), Baseline::Top)
                .draw(ctx.display)
                .ok();
            ctx.display.flush().ok();
            return;
        }

        // Hero line (y≈11): FOUND! / LOST / the warmer-colder trend.
        let hero: &str = if found {
            "FOUND!"
        } else if !present {
            "LOST"
        } else if delta > TREND_DEADBAND {
            "WARMER"
        } else if delta < -TREND_DEADBAND {
            "COLDER"
        } else {
            "SAME"
        };
        Text::with_baseline(hero, Point::new(0, 11), style_6x10(), Baseline::Top)
            .draw(ctx.display)
            .ok();
        // A trailing arrow glyph for the trend (skip for FOUND/LOST/SAME).
        // ASCII arrows only — the embedded-graphics `ascii::FONT_6X10` has no
        // Unicode glyphs, so `^`/`v` are what actually render.
        let arrow = if found || !present {
            ""
        } else if delta > TREND_DEADBAND {
            "^"
        } else if delta < -TREND_DEADBAND {
            "v"
        } else {
            ""
        };
        if !arrow.is_empty() {
            Text::with_baseline(arrow, Point::new(60, 11), style_6x10(), Baseline::Top)
                .draw(ctx.display)
                .ok();
        }

        // Proximity bar (y=24): smoothed RSSI, full width (70 px), outlined + filled.
        let bw = bar_px(smoothed, 70);
        Rectangle::new(Point::new(0, 24), Size::new(70, 8))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(ctx.display)
            .ok();
        if bw > 0 {
            Rectangle::new(Point::new(0, 24), Size::new(bw as u32, 8))
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(ctx.display)
                .ok();
        }

        // Bottom (y=32): "<rssi>dBm  <BUCKET>" (raw smoothed dBm + tier word). When
        // LOST, show the reacquiring hint instead of a stale dBm.
        let mut bot = Line::new();
        if present {
            let _ = write!(bot, "{}dBm {}", smoothed, label(proximity(smoothed)));
        } else {
            let _ = write!(bot, "reacquiring");
        }
        Text::with_baseline(bot.as_str(), Point::new(0, 32), style_5x8(), Baseline::Top)
            .draw(ctx.display)
            .ok();

        ctx.display.flush().ok();
    }
}
