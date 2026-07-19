//! #151 Finder — a hand-held placement / range meter: the live link quality to the
//! NEAREST mesh peer, hands-free.
//!
//! `espnow`-only. A pure consumer of [`crate::net::mode::RadioManager::roster`] —
//! **no new frames, no radio traffic** (every node already broadcasts HELLO/BEACON;
//! we only READ the RSSI the roster captured, F6-aged). Shares the smoothing +
//! proximity mapping with the Watch (#58) and treasure-hunt (#60) via [`crate::rssi`].
//!
//! ## What it adds over Watch/Hunt (the #151 gap analysis, in code)
//! - **Auto-nearest hero**: the strongest FRESH peer is re-selected EVERY tick, so the
//!   hero retargets hands-free as you carry the board between peers. Hunt's target is
//!   short-tap-cycled + sticky (a *game*); Watch has no single hero. The roster is
//!   already RSSI-descending, so "nearest" is just the first id-bearing node — zero
//!   selection state, nothing to fiddle while walking.
//! - **Fused layout on one screen**: the nearest peer's name + live dBm + a big moving
//!   signal bar (the hero) TOGETHER WITH a compact ranked list of the other peers.
//!   Watch has the list, Hunt has the bar; neither has both.
//! - **dBm promoted**: the raw smoothed dBm is the primary placement number, up top by
//!   the name (Watch shows none; Hunt buries it small at the bottom of a game screen).
//!
//! Cadence: fold every sample into the per-peer EWMA each tick, but only REPAINT when
//! the hero signature (id / smoothed dBm / peer count) changes OR a 500 ms floor
//! elapses — live in the hand, without burning a flush per subtick when held still.

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
use crate::net::names::name_for_id;
use crate::rssi::{bar_px, clip, label, proximity, tier_bars, Line, RssiSmoother};

/// The 72×40 OLED panel width used for layout (matches the other screens).
const PANEL_W: i32 = 72;
/// Repaint at least this often even with no signal change, so a held-still board
/// still refreshes (the issue's "500 ms tick, whichever first").
const REDRAW_FLOOR_MS: u64 = 500;
/// Compact ranked list: how many OTHER peers (after the hero) to show. Two 8 px rows
/// fit under the hero + bar in the 40 px panel; the fleet is tiny and the hero is the
/// point, so the weakest overflow peers age off the bottom (roster is RSSI-desc).
const OTHERS_SHOWN: usize = 2;

/// Finder state: the shared RSSI smoother + a repaint change-detector.
pub struct FinderState {
    smoother: RssiSmoother,
    /// Signature of the last painted hero (id, clamped smoothed dBm, peer count) — a
    /// cheap "did anything the user can see change?" gate for the repaint dedup.
    last_sig: u32,
    last_draw_ms: u64,
    drawn: bool,
}

impl Default for FinderState {
    fn default() -> Self {
        Self::new()
    }
}

impl FinderState {
    pub fn new() -> Self {
        Self {
            smoother: RssiSmoother::new(),
            last_sig: 0,
            last_draw_ms: 0,
            drawn: false,
        }
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

/// The hero signal bar: an outlined full-width track with a **dither-fill** proportional
/// to the nearest peer's smoothed RSSI (`bar_px`), plus a solid 1 px cap at the leading
/// edge so the moving front stays crisp as you walk. The 50 % ordered stipple reads as a
/// distinct "signal-energy" texture on the 1-bit panel (vs Hunt's solid block) and is the
/// #151 "dither-gradient fill". Bounded (≤ track area) + panic-free.
fn draw_hero_bar<D>(display: &mut D, y: i32, h: i32, fill_w: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    // Full-width outlined track.
    Rectangle::new(Point::new(0, y), Size::new(PANEL_W as u32, h as u32))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(display)
        .ok();
    if fill_w <= 0 {
        return;
    }
    // Dither the filled region (inside the 1 px border): checkerboard stipple.
    let x_end = fill_w.min(PANEL_W);
    for yy in (y + 1)..(y + h - 1) {
        for xx in 1..x_end {
            if (xx + yy) & 1 == 0 {
                Pixel(Point::new(xx, yy), BinaryColor::On).draw(display).ok();
            }
        }
    }
    // Crisp leading-edge cap so the moving front is unambiguous.
    let cap_x = (x_end - 1).clamp(1, PANEL_W - 1);
    Rectangle::new(Point::new(cap_x, y + 1), Size::new(1, (h - 2).max(1) as u32))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(display)
        .ok();
}

/// A compact 4-bar phone-style glyph for an "other peer" row, right-aligned. Mirrors the
/// Watch glyph but smaller (2 px bars, tight pitch) so a whole peer row fits at 8 px.
fn draw_mini_bars<D>(display: &mut D, y: i32, bars: u8)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);
    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    for i in 0..4u8 {
        let bh = 2 + i as i32 * 2; // 2,4,6,8 px
        let x = 52 + i as i32 * 5; // 52,57,62,67
        let top = y + 8 - bh;
        let style = if i < bars { fill } else { outline };
        Rectangle::new(Point::new(x, top), Size::new(3, bh as u32))
            .into_styled(style)
            .draw(display)
            .ok();
    }
}

impl Plugin for FinderState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu. Short tap is a no-op —
            // Finder is hands-free (auto-nearest), there is nothing to select; just
            // force a repaint so a tap gives immediate feedback.
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                ctx.redraw = true;
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Snapshot the roster (Copy) and release the radio borrow before drawing — the
        // exact pattern Watch/Hunt/Bench use.
        let roster = match ctx.radio.as_deref_mut() {
            Some(r) => r.roster(ctx.now_ms),
            None => return, // radio bring-up failed: render nothing (like Watch/Hunt)
        };
        let now = ctx.now_ms;

        // Fold every fresh id-bearing peer's RSSI into its per-id EWMA (an unknown id
        // can't be a stable smoother key). The roster is already RSSI-descending, so the
        // FIRST id-bearing node is the nearest (strongest) — re-picked every tick, which
        // is the hands-free auto-nearest the #151 hero needs.
        let mut hero: Option<(u8, i32)> = None; // (id, smoothed dBm)
        let mut others: [(u8, i32); OTHERS_SHOWN] = [(0, 0); OTHERS_SHOWN];
        let mut n_others = 0usize;
        for node in &roster.nodes[..roster.count] {
            if !node.id_known {
                continue;
            }
            let sm = self.smoother.update(node.id, node.rssi);
            if hero.is_none() {
                hero = Some((node.id, sm));
            } else if n_others < OTHERS_SHOWN {
                others[n_others] = (node.id, sm);
                n_others += 1;
            }
        }

        // Repaint gate: on a visible change (hero id / smoothed dBm / peer count) OR the
        // 500 ms floor OR a forced redraw. Signature is cheap + panic-free.
        let sig = match hero {
            Some((id, sm)) => {
                ((id as u32) << 24) ^ (((sm & 0xffff) as u32) << 8) ^ (roster.count as u32)
            }
            None => 0xffff_ffff, // "no peers" is its own distinct state
        };
        let due = ctx.redraw
            || !self.drawn
            || sig != self.last_sig
            || now.saturating_sub(self.last_draw_ms) >= REDRAW_FLOOR_MS;
        if !due {
            return;
        }
        self.last_sig = sig;
        self.last_draw_ms = now;
        self.drawn = true;

        // ---- render ----------------------------------------------------------
        ctx.display.clear(BinaryColor::Off).ok();

        let Some((hid, hrssi)) = hero else {
            // No id-bearing peer in range: a clear placeholder, never a mystery blank.
            Text::with_baseline("FINDER", Point::new(0, 0), style_5x8(), Baseline::Top)
                .draw(ctx.display)
                .ok();
            Text::with_baseline("no peers", Point::new(0, 16), style_6x10(), Baseline::Top)
                .draw(ctx.display)
                .ok();
            ctx.display.flush().ok();
            return;
        };

        // Hero row (y=0): nearest peer's noun (left) + live smoothed dBm (right-aligned),
        // the primary placement readout.
        Text::with_baseline(clip(name_for_id(hid).1, 6), Point::new(0, 0), style_6x10(), Baseline::Top)
            .draw(ctx.display)
            .ok();
        let mut dbm = Line::new();
        let _ = write!(dbm, "{}dBm", hrssi);
        let dbm_w = dbm.as_str().chars().count() as i32 * 6; // FONT_6X10 glyph width
        Text::with_baseline(
            dbm.as_str(),
            Point::new((PANEL_W - dbm_w).max(0), 0),
            style_6x10(),
            Baseline::Top,
        )
        .draw(ctx.display)
        .ok();

        // Hero bar (y=12, h=9): the big moving signal bar for the nearest peer.
        draw_hero_bar(ctx.display, 12, 9, bar_px(hrssi, PANEL_W));

        // Compact ranked list of the OTHER peers (y=22, y=31): noun + a mini strength
        // glyph. Stale peers have already aged out of the roster (F6), so this is always
        // the live ranking.
        for (row, &(oid, orssi)) in others[..n_others].iter().enumerate() {
            let y = 22 + row as i32 * 9;
            Text::with_baseline(clip(name_for_id(oid).1, 8), Point::new(0, y), style_5x8(), Baseline::Top)
                .draw(ctx.display)
                .ok();
            draw_mini_bars(ctx.display, y, tier_bars(proximity(orssi)));
        }
        // If there are no other peers, show the hero's tier word where the list would be
        // (keeps the bottom informative on a two-node walk-test).
        if n_others == 0 {
            let mut tl = Line::new();
            let _ = write!(tl, "{}", label(proximity(hrssi)));
            Text::with_baseline(tl.as_str(), Point::new(0, 24), style_6x10(), Baseline::Top)
                .draw(ctx.display)
                .ok();
        }

        ctx.display.flush().ok();
    }
}
