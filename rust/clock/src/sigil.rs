//! #282 SIGIL screen — the node's identity nameplate.
//!
//! A deliberate, navigable identity screen with its OWN [`Plugin`] + Home-menu row (issue
//! #282) — NOT crammed into the Clock (the #276/#277 header was reverted) and NOT hijacking
//! the Custom screen. It shows the node's full magical name (the "sigil"): the fantasy
//! ADJECTIVE as a flourish over the NOUN handle (the identity every other screen uses), with
//! the firmware's FORGE version sigil as a secondary provenance line — a real nameplate for
//! telling boards apart at a glance.
//!
//! Compiled into EVERY build: identity needs no radio (like About/Clock). The content is
//! fully static at runtime, so it paints ONCE (redraw-latched) and then idles — no per-tick
//! cost. Heap-free so it runs in the alloc-free default build. Panic-free — every string is
//! bounded ASCII and the version line is built into a fixed buffer.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoFont, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle},
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// The 72×40 OLED panel width used for centring (matches the other screens).
const PANEL_W: i32 = 72;

/// SIGIL state: a one-shot paint latch. Identity + firmware version never change at runtime,
/// so we paint on the first `update` (or a forced `redraw` after a mode switch) and then idle
/// — the same dedup shape as the Custom screen, minus the content hash (nothing changes).
pub struct SigilState {
    drawn: bool,
}

impl SigilState {
    pub fn new() -> Self {
        Self { drawn: false }
    }
}

impl Plugin for SigilState {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu. A static nameplate ignores taps.
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => Transition::Stay,
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Static content: repaint only on the first entry or a forced redraw (menu switch).
        if !(ctx.redraw || !self.drawn) {
            return;
        }
        self.drawn = true;
        ctx.display.clear(BinaryColor::Off).ok();
        draw_sigil(ctx.display);
        ctx.display.flush().ok();
    }
}

/// Render the identity nameplate. Generic over the draw target (mirrors `draw_clock`) so it
/// stays host-testable in principle. Panic-free.
fn draw_sigil<D>(display: &mut D)
where
    D: DrawTarget<Color = BinaryColor>,
{
    // The node's full magical name: `.0` = adjective (the flourish), `.1` = noun (the handle
    // every other screen shows). FONT_6X10 is the largest font that fits ALL fantasy nouns at
    // 72 px — FONT_10X20 clips an 8-char noun (the boot-splash rationale) — so the noun uses it.
    let (adj, noun) = crate::net::names::name_for_id(crate::node_id());

    // Adjective — small, centred, top: a subtle flourish above the identity.
    draw_centered(display, adj, &FONT_5X8, 2);
    // Noun — the identity, prominent + centred.
    draw_centered(display, noun, &FONT_6X10, 12);

    // A hairline divider separating identity (WHO) from provenance (WHICH firmware).
    Line::new(Point::new(8, 25), Point::new(PANEL_W - 9, 25))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(display)
        .ok();

    // Version sigil — "v<build> <forge-noun>" (e.g. "v903 Bellows"), matching the boot splash's
    // compact form (no dev-hash, so it never overflows the row). Built heap-free into a fixed
    // buffer since this screen compiles into the alloc-free default build.
    let mut vbuf = Buf::new();
    let _ = write!(
        vbuf,
        "v{} {}",
        crate::net::names::build_number(),
        crate::net::names::version_name().1
    );
    draw_centered(display, vbuf.as_str(), &FONT_5X8, 29);
}

/// Draw `text` horizontally centred in the 72 px row at baseline-top `y`, in `font`. The
/// centre offset uses the font's own glyph width (ASCII mono → one glyph per char), so it
/// stays correct if the font changes. Never negative (clamped), so it can't panic or wrap.
fn draw_centered<D>(display: &mut D, text: &str, font: &MonoFont, y: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyleBuilder::new().font(font).text_color(BinaryColor::On).build();
    let cw = font.character_size.width as i32;
    let tw = text.chars().count() as i32 * cw;
    let x = ((PANEL_W - tw) / 2).max(0);
    Text::with_baseline(text, Point::new(x, y), style, Baseline::Top).draw(display).ok();
}

/// Tiny heap-free line builder (Sigil compiles into the alloc-free default build, so no
/// `String`). 24 bytes comfortably holds "v<5-digit> <forge-noun≤8>" (≤15 chars).
struct Buf {
    buf: [u8; 24],
    len: usize,
}

impl Buf {
    fn new() -> Self {
        Self { buf: [0; 24], len: 0 }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for Buf {
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
