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
//! (`FIRST_Y=11 + 3·ROW_H=9 = 38 ≤ 40`). Adding the Batt screen (cfg wifi) grows
//! the `wifi` menu to FOUR entries (Clock / Snake / Batt / About) and the `espnow`
//! menu to FIVE (Clock / Snake / Bench / Batt / About), so BOTH render a WINDOW of
//! ≤3 rows that follows the selection, with edge chevrons (`^`/`v`) marking
//! off-window items. Only the default build stays at 3 entries (Clock / Snake /
//! About) → the window is the whole list and never scrolls (no chevrons). The
//! window math is `VISIBLE`-relative, so it holds unchanged for 4 and 5 entries.

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
    fn draw(&self, display: &mut crate::app::Oled) {
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
        let noun = crate::net::names::name_for_id(crate::NODE_ID).1;
        Text::with_baseline(noun, Point::new(1, 0), title, Baseline::Top)
            .draw(display)
            .ok();

        // The window of rows [win .. win+VISIBLE), clamped to the registry length.
        let end = (self.win + VISIBLE).min(REGISTRY.len());
        for (i, desc) in REGISTRY.iter().enumerate().skip(self.win).take(VISIBLE) {
            let rel = (i - self.win) as i32;
            let y = FIRST_Y + rel * ROW_H;
            if i == self.selected {
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

        // Edge chevrons: mark items above/below the window. Draw in inverse when
        // the marked row is the highlighted (white-bar) one so it stays visible.
        if self.win > 0 {
            let style = if self.selected == self.win { item_sel } else { item };
            Text::with_baseline("^", Point::new(66, FIRST_Y), style, Baseline::Top)
                .draw(display)
                .ok();
        }
        if end < REGISTRY.len() {
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
        match press {
            // Short tap: advance the selection (wrapping) and follow the window.
            Press::Short => {
                self.selected = (self.selected + 1) % REGISTRY.len();
                if self.selected < self.win {
                    self.win = self.selected;
                } else if self.selected >= self.win + VISIBLE {
                    self.win = self.selected + 1 - VISIBLE;
                }
                ctx.redraw = true;
                Transition::Stay
            }
            // Long press: enter the highlighted app (the SNAKE_KIND merge means
            // "Snake" launches MeshSnake under espnow — see the registry).
            Press::Long => Transition::Switch(REGISTRY[self.selected].kind),
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // The menu is static between selections, so it repaints on `redraw` only
        // (mode entry or a tap) — same cadence as the old Menu render arm.
        if ctx.redraw {
            ctx.display.clear(BinaryColor::Off).ok();
            self.draw(ctx.display);
            ctx.display.flush().ok();
        }
    }
}
