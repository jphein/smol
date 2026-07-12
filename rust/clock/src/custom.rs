//! #45 CUSTOM screen — per-node user-authored text/entities.
//!
//! A [`Plugin`] that renders the layout HA composes for this node (issue #45). The user authors
//! lines in a dashboard `input_text`; HA resolves any `{entity_id}` refs and publishes the
//! RESOLVED wire to retained `smol/<id>/config/custom` (CFG key `Y`), relayed to leaves over the
//! #21/#56 CFG mesh frame. This module NEVER resolves entities — it renders the plain bytes it
//! is handed (via [`Ctx::custom`]).
//!
//! ## Wire (luna's #81 compose contract)
//! `"<count>|<size><align>text;<size><align>text;…"` — `count` 1..4 (advisory; we parse the
//! actual `;`-separated segments), each segment is `<size><align>text` where `size ∈ {s,m,l}`
//! (fonts 5x8 / 6x10 / 10x20) and `align ∈ {l,c,r}` (left / centre / right within the 72 px row).
//! Empty payload = no custom set. Segments stack top→bottom; a segment that would overflow the
//! 40 px panel is dropped. TOTALLY panic-free — a malformed segment is skipped, never a crash.
//!
//! espnow-only: the config only ever arrives over the radio path (CFG relay / gateway-own MQTT),
//! so a default/wifi build has nothing to render and never compiles this screen.

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// The 72×40 OLED panel bounds used for layout (matches the other screens).
const PANEL_W: i32 = 72;
const PANEL_H: i32 = 40;

/// CUSTOM state: a content dedup so the panel repaints once per CHANGE (config update or a
/// re-resolved entity value) plus on any forced `redraw` — not every subtick. FNV-1a over the
/// wire bytes is enough to detect a change cheaply without storing the whole payload twice.
pub struct CustomState {
    last_hash: u32,
    drawn: bool,
}

impl CustomState {
    pub fn new() -> Self {
        Self { last_hash: 0, drawn: false }
    }
}

impl Plugin for CustomState {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu. Short taps do nothing (static screen).
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => Transition::Stay,
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let h = fnv1a(ctx.custom);
        if ctx.redraw || !self.drawn || h != self.last_hash {
            self.last_hash = h;
            self.drawn = true;
            ctx.display.clear(BinaryColor::Off).ok();
            draw_custom(ctx.display, ctx.custom);
            ctx.display.flush().ok();
        }
    }
}

/// FNV-1a over the wire bytes — a cheap change detector for the repaint dedup.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Render the composed layout. `wire` is `"<count>|seg;seg;…"` (see module docs). Generic over
/// the draw target so it stays testable in principle (mirrors `draw_clock`). Panic-free.
fn draw_custom<D>(display: &mut D, wire: &[u8])
where
    D: DrawTarget<Color = BinaryColor>,
{
    // Segments live after the FIRST '|' (the `<count>` prefix is advisory — we render whatever
    // segments are actually present). No '|' at all ⇒ treat the whole payload as absent.
    let body = match wire.iter().position(|&b| b == b'|') {
        Some(i) => &wire[i + 1..],
        None => &[][..],
    };

    let mut y: i32 = 0;
    let mut rendered = 0u8;
    for seg in body.split(|&b| b == b';') {
        // `<size><align>text` — need at least the 2 prefix bytes.
        if seg.len() < 2 {
            continue;
        }
        // (font, glyph width, line height) per size byte. Inference pins `font` to
        // `&'static MonoFont<'static>`; the `i32` literals match the layout arithmetic.
        let (font, char_w, line_h) = match seg[0] {
            b'l' => (&FONT_10X20, 10i32, 20i32),
            b'm' => (&FONT_6X10, 6i32, 10i32),
            _ => (&FONT_5X8, 5i32, 8i32), // 's' or anything unknown → smallest (safe default)
        };
        // Drop a segment that can't fit in the remaining panel height (clip, never overflow).
        if y + line_h > PANEL_H {
            break;
        }
        let text = core::str::from_utf8(&seg[2..]).unwrap_or("");
        // Width in glyphs (ASCII font → 1 glyph per char); align within the 72 px row.
        let tw = text.chars().count() as i32 * char_w;
        let x = match seg[1] {
            b'c' => ((PANEL_W - tw) / 2).max(0),
            b'r' => (PANEL_W - tw).max(0),
            _ => 2, // 'l' or unknown → left margin (matches clock/menu x=2)
        };
        let style = MonoTextStyleBuilder::new().font(font).text_color(BinaryColor::On).build();
        Text::with_baseline(text, Point::new(x, y), style, Baseline::Top).draw(display).ok();
        y += line_h + 1; // 1 px inter-line gap
        rendered = rendered.saturating_add(1);
    }

    // Nothing to show (empty / all-malformed): a placeholder so the screen is never a mystery
    // blank — the node's own noun, centred, tells the user which node + that no custom is set.
    if rendered == 0 {
        let noun = crate::net::names::name_for_id(crate::node_id()).1;
        let style = MonoTextStyleBuilder::new().font(&FONT_6X10).text_color(BinaryColor::On).build();
        let tw = noun.chars().count() as i32 * 6;
        let x = ((PANEL_W - tw) / 2).max(0);
        Text::with_baseline(noun, Point::new(x, 15), style, Baseline::Top).draw(display).ok();
    }
}
