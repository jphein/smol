//! Marauder's Watch (#58) — a presence screen: every OTHER node by name with a
//! phone-style signal-strength glyph, from the ESP-NOW roster RSSI the mesh already
//! tracks.
//!
//! `espnow`-only. A pure consumer of [`crate::net::mode::RadioManager::roster`] —
//! **no new frames, no radio traffic**. RSSI is smoothed per peer (shared
//! [`crate::rssi::RssiSmoother`]) so the bars don't strobe on multipath jitter.
//! Peers render strongest-first (the roster is already RSSI-desc), so the nearest
//! node is always on top. Paginates like Bench when there are more peers than rows.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;
use crate::net::names::name_for_id;
use crate::rssi::{clip, proximity, tier_bars, Line, RssiSmoother};

/// Peer rows per page: a title row + 4 rows fills the 40 px height at 8 px pitch.
const PEERS_PER_PAGE: usize = 4;

/// Marauder's Watch state: the shared RSSI smoother + which page is showing.
pub struct WatchState {
    smoother: RssiSmoother,
    page: u8,
}

impl Default for WatchState {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchState {
    pub fn new() -> Self {
        Self {
            smoother: RssiSmoother::new(),
            page: 0,
        }
    }

    /// Total pages = ⌈count / PEERS_PER_PAGE⌉, at least 1 (an empty roster still
    /// shows the "no peers" page).
    fn page_count(count: usize) -> usize {
        count.div_ceil(PEERS_PER_PAGE).max(1)
    }
}

fn text_style() -> embedded_graphics::mono_font::MonoTextStyle<'static, BinaryColor> {
    MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build()
}

/// Draw the 4-bar phone-style strength glyph at the right of a peer row: bar `i`
/// (0..4) is filled iff `i < bars`, else outlined — increasing height, bottom-
/// aligned to the row so it reads as signal strength at a glance.
fn draw_signal<D>(display: &mut D, y: i32, bars: u8)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);
    let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
    for i in 0..4u8 {
        let h = 2 + i as i32 * 2; // 2,4,6,8 px
        let x = 46 + i as i32 * 6; // 46,52,58,64
        let top = y + 8 - h;
        let style = if i < bars { fill } else { outline };
        Rectangle::new(Point::new(x, top), Size::new(4, h as u32))
            .into_styled(style)
            .draw(display)
            .ok();
    }
}

/// One peer row: noun (≤7, from the id) at x=0, then the signal glyph. Unknown-id
/// peers show `?` (mirrors Bench).
fn draw_peer_row<D>(display: &mut D, y: i32, id: u8, id_known: bool, bars: u8)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let noun = if id_known { name_for_id(id).1 } else { "?" };
    Text::with_baseline(clip(noun, 7), Point::new(0, y), text_style(), Baseline::Top)
        .draw(display)
        .ok();
    draw_signal(display, y, bars);
}

/// The `p/N` page indicator, top-right (1-based). Shared shape with Bench.
fn draw_page_tag<D>(display: &mut D, page: usize, n_pages: usize)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let mut t = Line::new();
    let _ = write!(t, "{}/{}", page, n_pages);
    Text::with_baseline(t.as_str(), Point::new(57, 0), text_style(), Baseline::Top)
        .draw(display)
        .ok();
}

impl Plugin for WatchState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                let count = match ctx.radio.as_deref() {
                    Some(r) => r.roster(ctx.now_ms).count,
                    None => 0,
                };
                let n_pages = Self::page_count(count);
                self.page = ((self.page as usize + 1) % n_pages) as u8;
                ctx.redraw = true;
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Snapshot the roster (Copy) and release the radio borrow before drawing —
        // the exact pattern Bench uses.
        let roster = match ctx.radio.as_deref_mut() {
            Some(r) => r.roster(ctx.now_ms),
            None => return, // radio bring-up failed: render nothing (like Bench)
        };

        // Fold each fresh peer's RSSI into its per-id EWMA (only id-bearing peers —
        // an unknown id can't be a stable smoother key).
        for n in &roster.nodes[..roster.count] {
            if n.id_known {
                self.smoother.update(n.id, n.rssi);
            }
        }

        let n_pages = Self::page_count(roster.count);
        // The roster can shrink between ticks — clamp the page.
        if self.page as usize >= n_pages {
            self.page = 0;
        }

        ctx.display.clear(BinaryColor::Off).ok();

        // Title: "WATCH <count>".
        let mut hdr = Line::new();
        let _ = write!(hdr, "WATCH {}", roster.count);
        Text::with_baseline(hdr.as_str(), Point::new(0, 0), text_style(), Baseline::Top)
            .draw(ctx.display)
            .ok();
        draw_page_tag(ctx.display, self.page as usize + 1, n_pages);

        // Peer rows for this page.
        let start = self.page as usize * PEERS_PER_PAGE;
        for row in 0..PEERS_PER_PAGE {
            let idx = start + row;
            if idx >= roster.count {
                break;
            }
            let n = &roster.nodes[idx];
            // Prefer the smoothed value; fall back to the raw sample first-sight.
            let rssi = self.smoother.get(n.id).unwrap_or(n.rssi);
            let bars = tier_bars(proximity(rssi));
            draw_peer_row(ctx.display, 8 + row as i32 * 8, n.id, n.id_known, bars);
        }

        ctx.display.flush().ok();
    }
}
