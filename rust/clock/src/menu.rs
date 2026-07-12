//! Home menu — a [`Plugin`] that renders the app [`REGISTRY`] and launches the
//! highlighted app.
//!
//! The menu is itself an app (state = the current selection + the scroll window).
//! It reads the single source of truth — `crate::app::REGISTRY` — so adding an
//! app never touches this file: a new registry row appears here automatically.
//! The old `AppMode` enum + `MENU_ITEMS` table are gone (replaced by
//! `crate::app::AppKind` + `REGISTRY`).
//!
//! ## Controls (single BOOT button — see `src/input.rs`)
//!
//! | Where            | Short tap                      | Long press (~700 ms)     |
//! |------------------|--------------------------------|--------------------------|
//! | **Home menu**    | move selection (wraps)         | **enter** highlighted app |
//! | **an app**       | app-specific (or none)         | **back** to Home         |
//!
//! ## Scrolling window (REQUIRED under wifi and espnow)
//!
//! The 40 px panel fits a title row + [`VISIBLE`] = 3 item rows
//! (`FIRST_Y=11 + 3·ROW_H=9 = 38 ≤ 40`). The Batt + Grid screens (both cfg wifi;
//! issue #16 added Grid) grow the `wifi` menu to FIVE entries (Clock / Snake / Batt
//! / Grid / About) and the `espnow` menu to SIX (Clock / Snake / Bench / Batt /
//! Grid / About), so BOTH render a WINDOW of ≤3 rows that follows the selection,
//! with edge chevrons (`^`/`v`) marking off-window items. Only the default build
//! stays at 3 entries (Clock / Snake / About) → the window is the whole list and
//! never scrolls (no chevrons). The window math is `VISIBLE`-relative, so it holds
//! unchanged for 5 and 6 entries.

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

use crate::app::{Ctx, Plugin, Transition, REGISTRY};
use crate::input::Press;

/// Item rows visible at once (the panel fits a title + 3 rows within 40 px).
const VISIBLE: usize = 3;
/// First item row's top Y (below the title). Items step [`ROW_H`] px each.
const FIRST_Y: i32 = 11;
const ROW_H: i32 = 9;

/// #55: parse a `smol/<id>/config/plugins` payload — up to 4 ASCII-hex chars → a `u16` bit mask
/// (e.g. `"007F"`, `"7f"`, `"5"`). Case-insensitive, panic-free. Empty / too long / any non-hex
/// char → `None`, so the caller KEEPS its current mask (untrusted retained/relayed value — the
/// #46 clamp). Bit `i` (see [`crate::app::plugin_bit`]) set = that app is shown in the Home menu.
///
/// espnow-only: the config apply path (`take_cfg_offer(P)`) is radio-only (like the screen/LED/
/// units channels) — a non-espnow build never receives a mask (stays `0` = all shown).
#[cfg(feature = "espnow")]
pub fn parse_plugin_mask(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.is_empty() || s.len() > 4 {
        return None;
    }
    let mut v: u16 = 0;
    for b in s.bytes() {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        };
        v = (v << 4) | d as u16;
    }
    Some(v)
}

/// #55: is `kind` shown under `mask`? A ZERO mask = keep all (the #55 safety: never blank the
/// menu — also the non-espnow default, where the mask is never set). A kind with no plugin bit
/// (`Menu`) is always shown. `mask` bit `plugin_bit(kind)` set = shown.
pub fn kind_enabled(kind: crate::app::AppKind, mask: u16) -> bool {
    mask == 0 || crate::app::plugin_bit(kind).is_none_or(|b| mask & (1 << b) != 0)
}

/// #55: the REGISTRY indices the mask ENABLES, in registry order (a fixed `.bss` buffer — at most
/// `REGISTRY.len()` entries). SAFETY: if the mask enables NOTHING compiled, fall back to the whole
/// registry — the menu is never blank (#55 safety / #46 clamp). Returns `(indices, count)`.
fn enabled_indices(mask: u16) -> ([usize; REGISTRY.len()], usize) {
    let mut buf = [0usize; REGISTRY.len()];
    let mut n = 0;
    for (i, desc) in REGISTRY.iter().enumerate() {
        if kind_enabled(desc.kind, mask) {
            buf[n] = i;
            n += 1;
        }
    }
    if n == 0 {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = i;
        }
        n = REGISTRY.len();
    }
    (buf, n)
}

/// Home-menu state: which item is highlighted, and the top of the scroll window.
pub struct Menu {
    selected: usize,
    /// Index of the first item currently drawn (`[win .. win+VISIBLE)`).
    win: usize,
}

impl Menu {
    /// Start with the first entry (Clock) highlighted, window at the top.
    pub fn new() -> Self {
        Self { selected: 0, win: 0 }
    }

    /// Render the title (node noun) + the visible window of [`REGISTRY`] rows,
    /// the selection in inverse video, plus edge chevrons when items sit off the
    /// window. Heap-free; all labels are `'static`.
    fn draw(&self, display: &mut crate::app::Oled, mask: u16) {
        let title: MonoTextStyle<BinaryColor> = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(BinaryColor::On)
            .build();
        let item: MonoTextStyle<BinaryColor> = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::On)
            .build();
        let item_sel: MonoTextStyle<BinaryColor> = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::Off)
            .build();

        // Title row = this node's magical NOUN (its handle), derived from
        // NODE_ID — the same identity the Clock line, splash and peer labels use.
        // The noun (<= 8 chars for fantasy) fits the 6x10 title within 72 px.
        // Static per boot, so it rides the normal `redraw` with no extra
        // invalidation, and it needs no radio — works in EVERY build.
        let noun = crate::net::names::name_for_id(crate::node_id()).1;
        Text::with_baseline(noun, Point::new(1, 0), title, Baseline::Top)
            .draw(display)
            .ok();

        // #55: `selected`/`win` index the ENABLED subset (the mask may hide rows). The window
        // [win .. win+VISIBLE) walks that subset; each position maps to a REGISTRY row via `idx`.
        // A zero mask → all rows enabled → identical to the pre-#55 behavior.
        let (idx, n) = enabled_indices(mask);
        let end = (self.win + VISIBLE).min(n);
        for pos in self.win..end {
            let desc = &REGISTRY[idx[pos]];
            let rel = (pos - self.win) as i32;
            let y = FIRST_Y + rel * ROW_H;
            if pos == self.selected {
                // Highlight bar spanning the 72 px width, then inverse text.
                Rectangle::new(Point::new(0, y - 1), Size::new(72, ROW_H as u32))
                    .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                    .draw(display)
                    .ok();
                Text::with_baseline(desc.title, Point::new(2, y), item_sel, Baseline::Top)
                    .draw(display)
                    .ok();
            } else {
                Text::with_baseline(desc.title, Point::new(2, y), item, Baseline::Top)
                    .draw(display)
                    .ok();
            }
        }

        // Edge chevrons: mark enabled items above/below the window. Draw in inverse when
        // the marked row is the highlighted (white-bar) one so it stays visible.
        if self.win > 0 {
            let style = if self.selected == self.win { item_sel } else { item };
            Text::with_baseline("^", Point::new(66, FIRST_Y), style, Baseline::Top)
                .draw(display)
                .ok();
        }
        if end < n {
            let bottom = end - 1;
            let style = if self.selected == bottom { item_sel } else { item };
            let y = FIRST_Y + (VISIBLE as i32 - 1) * ROW_H;
            Text::with_baseline("v", Point::new(66, y), style, Baseline::Top)
                .draw(display)
                .ok();
        }
    }
}

impl Plugin for Menu {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        // #55: `selected`/`win` index the ENABLED subset. Resolve it (a zero mask → the full
        // list) and clamp first — the mask may have shrunk the subset since the last press.
        // `n >= 1` always (enabled_indices falls back to the whole registry if nothing is on).
        let (idx, n) = enabled_indices(ctx.plugin_mask);
        if self.selected >= n {
            self.selected = n - 1;
        }
        match press {
            // Short tap: advance the selection (wrapping over the enabled subset) + follow window.
            Press::Short => {
                self.selected = (self.selected + 1) % n;
                if self.selected < self.win {
                    self.win = self.selected;
                } else if self.selected >= self.win + VISIBLE {
                    self.win = self.selected + 1 - VISIBLE;
                }
                ctx.redraw = true;
                Transition::Stay
            }
            // Long press: enter the highlighted app (the SNAKE_KIND merge means "Snake" launches
            // MeshSnake under espnow). `idx[selected]` maps the subset position to its REGISTRY row.
            Press::Long => Transition::Switch(REGISTRY[idx[self.selected]].kind),
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // The menu is static between selections, so it repaints on `redraw` only
        // (mode entry or a tap) — same cadence as the old Menu render arm.
        if ctx.redraw {
            // #55: clamp the selection/window to the enabled-row count before drawing — the mask
            // may have shrunk the subset out from under us since the last paint.
            let (_, n) = enabled_indices(ctx.plugin_mask);
            if self.selected >= n {
                self.selected = n - 1;
            }
            if self.selected < self.win {
                self.win = self.selected;
            } else if self.selected >= self.win + VISIBLE {
                self.win = self.selected + 1 - VISIBLE;
            }
            ctx.display.clear(BinaryColor::Off).ok();
            self.draw(ctx.display, ctx.plugin_mask);
            ctx.display.flush().ok();
        }
    }
}
