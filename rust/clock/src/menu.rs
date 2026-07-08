//! Home menu + top-level mode state machine.
//!
//! The unified firmware is a small state machine over [`AppMode`]. `main`'s
//! render loop dispatches on the *current* mode each frame; this module owns the
//! **Menu** state itself (the list of modes, the current selection, and how the
//! menu reacts to button presses) plus the shared [`AppMode`] enum that ties the
//! whole dispatcher together.
//!
//! ## Controls (single BOOT button — see `src/input.rs`)
//!
//! | Where            | Short tap                      | Long press (~700 ms)     |
//! |------------------|--------------------------------|--------------------------|
//! | **Home menu**    | move selection (wraps)         | **enter** highlighted mode |
//! | **Clock / Bench**| (mode-specific / none)         | **back** to Home         |
//! | **Snake (alive)**| turn clockwise                 | **back** to Home         |
//! | **Snake (dead)** | restart                        | **back** to Home         |
//!
//! Keeping the "long press = enter from menu / back from a mode" rule uniform
//! means the one button is always predictable: hold to change *level*, tap to
//! act *within* a level.
//!
//! ## Feature gating
//!
//! BENCH exercises the ESP-NOW mesh, so the `Bench` menu entry (and the
//! [`AppMode::Bench`] variant's use) only exists under `--features espnow`. The
//! default and `wifi` builds show a two-item menu (Clock / Snake); the `espnow`
//! build shows three. The [`AppMode`] enum itself always declares all variants
//! (so `main`'s `match` is identical across builds) — the non-espnow builds just
//! never construct or select `Bench`.

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyle, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

/// The top-level application mode dispatched by `main` each frame.
///
/// All variants are always declared so the dispatcher `match` in `main` is the
/// same across every feature build; `Bench` is simply never *entered* unless the
/// `espnow` feature compiled it into the menu (see [`MENU_ITEMS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppMode {
    /// The Home menu (this module drives it).
    Menu,
    /// Live clock (big HH:MM + alternating sensor line); NTP-synced at boot.
    Clock,
    /// Single-player Snake (see `src/snake.rs`). Under `espnow`, MeshSnake
    /// replaces it in the menu, so it is not entered there.
    #[allow(dead_code)] // not selected in espnow builds (MeshSnake takes its slot)
    Snake,
    /// MMO Mesh Snake over ESP-NOW (issue #5; see `src/mesh_snake`). Only
    /// entered under `espnow`.
    #[allow(dead_code)] // never constructed in non-espnow builds
    MeshSnake,
    /// ESP-NOW link statistics (see `src/bench.rs`). Only entered under `espnow`.
    #[allow(dead_code)] // never constructed in non-espnow builds
    Bench,
}

/// A selectable Home-menu entry: its label + the mode entering it launches.
struct MenuItem {
    label: &'static str,
    mode: AppMode,
}

/// The menu entries, in display order. BENCH is compiled in only under `espnow`
/// (it is the ESP-NOW mesh test); smaller builds get Clock + Snake.
const MENU_ITEMS: &[MenuItem] = &[
    MenuItem {
        label: "Clock",
        mode: AppMode::Clock,
    },
    // Non-espnow builds get the single-player Snake; espnow builds replace it
    // with MMO Mesh Snake (design §6) so the menu stays ≤ 3 items (4 would
    // overflow the 40 px panel).
    #[cfg(not(feature = "espnow"))]
    MenuItem {
        label: "Snake",
        mode: AppMode::Snake,
    },
    #[cfg(feature = "espnow")]
    MenuItem {
        label: "MeshSnake",
        mode: AppMode::MeshSnake,
    },
    #[cfg(feature = "espnow")]
    MenuItem {
        label: "Bench",
        mode: AppMode::Bench,
    },
];

/// Home-menu state: just which item is currently highlighted.
pub struct Menu {
    selected: usize,
}

impl Menu {
    /// Start with the first entry (Clock) highlighted.
    pub fn new() -> Self {
        Self { selected: 0 }
    }

    /// Short tap in the menu: move the highlight to the next item, wrapping.
    pub fn on_tap(&mut self) {
        self.selected = (self.selected + 1) % MENU_ITEMS.len();
    }

    /// Long press in the menu: enter the highlighted mode. Returns the
    /// [`AppMode`] `main` should switch to.
    pub fn on_enter(&self) -> AppMode {
        MENU_ITEMS[self.selected].mode
    }

    /// Render the menu: a title bar plus the item list, the selection drawn in
    /// inverse video (filled bar + off-colour text) so it's obvious on the tiny
    /// mono OLED. Heap-free; all labels are `'static`.
    pub fn draw<D>(&self, display: &mut D)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Title in the 6x10 font at the top; items below in 5x8.
        let title: MonoTextStyle<BinaryColor> = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(BinaryColor::On)
            .build();
        let item: MonoTextStyle<BinaryColor> = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::On)
            .build();
        // Inverse text for the highlighted row (drawn over a filled bar).
        let item_sel: MonoTextStyle<BinaryColor> = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::Off)
            .build();

        // Title row = this node's magical NOUN (its handle), derived from
        // NODE_ID — the same identity the Clock bottom line and peer labels use,
        // now on Home too. The noun (<= 8 chars for fantasy) fits the 6x10 title
        // within 72 px; the full "Adjective Noun" (up to 17 chars) would not fit
        // one line in any font here, so we show the noun consistently across the
        // whole UI. It is static per boot, so it rides the normal `redraw` with no
        // extra invalidation, and it needs no radio — works in EVERY build.
        let noun = crate::net::names::name_for_id(crate::NODE_ID).1;
        Text::with_baseline(noun, Point::new(1, 0), title, Baseline::Top)
            .draw(display)
            .ok();

        // Items start below the title (10 px) and step 9 px each (~8 px glyph +
        // 1 px gap). With <=3 items this fits comfortably inside 40 px height.
        const FIRST_Y: i32 = 11;
        const ROW_H: i32 = 9;
        for (i, it) in MENU_ITEMS.iter().enumerate() {
            let y = FIRST_Y + i as i32 * ROW_H;
            if i == self.selected {
                // Highlight bar spanning the 72 px width, then inverse text.
                Rectangle::new(Point::new(0, y - 1), Size::new(72, ROW_H as u32))
                    .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                    .draw(display)
                    .ok();
                Text::with_baseline(it.label, Point::new(2, y), item_sel, Baseline::Top)
                    .draw(display)
                    .ok();
            } else {
                Text::with_baseline(it.label, Point::new(2, y), item, Baseline::Top)
                    .draw(display)
                    .ok();
            }
        }
    }
}
