//! Text inks: a foreground **and the ground it is drawn on**, as one value.
//!
//! 0027's HUD finding was that a readout and a tint shared pixels and nobody had written the
//! ground down, so no check could see it. Here every piece of station text is drawn through
//! [`label`] with an [`Ink`], and `label` **paints the ink's ground under the glyph box before it
//! draws the glyphs**. The ground a contrast test measures is therefore the ground the panel
//! shows — the checked thing and the checking thing are one object (`docs/verification.md`).
//!
//! A side effect worth having: text drawn over the level-up light cuts its own well through the
//! light, which is exactly 0027's ruling (the readout moves into a dark well inside the tint).

use embedded_graphics::{
    mono_font::MonoFont, pixelcolor::Rgb565, prelude::*, primitives::Rectangle, text::Alignment,
};

use crate::battlefield::{fill, text};
use crate::palette as pal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    /// Plain text on the black ground.
    Text,
    Dim,
    /// Readouts in a `PANEL` well.
    WellText,
    WellDim,
    HealthOk,
    HealthMid,
    HealthLow,
    Warm,
    Warn,
    Good,
    /// Mana pips: available (filled) and spent this round (ring). Not text, but a readout on the
    /// same well, so it answers to the same floor.
    Mana,
    Spent,
    /// The owed-draws numeral, on the face-down card the figure holds (0036).
    OnCard,
}

impl Ink {
    pub const ALL: [Ink; 13] = [
        Ink::Text,
        Ink::Dim,
        Ink::WellText,
        Ink::WellDim,
        Ink::HealthOk,
        Ink::HealthMid,
        Ink::HealthLow,
        Ink::Warm,
        Ink::Warn,
        Ink::Good,
        Ink::Mana,
        Ink::Spent,
        Ink::OnCard,
    ];

    /// (foreground, ground). Exhaustive, so a new ink cannot be drawn until it has a ground.
    pub const fn pair(self) -> (Rgb565, Rgb565) {
        match self {
            Ink::Text => (pal::TEXT, pal::BG),
            Ink::Dim => (pal::TEXT_DIM, pal::BG),
            Ink::WellText => (pal::TEXT, pal::PANEL),
            Ink::WellDim => (pal::TEXT_DIM, pal::PANEL),
            Ink::HealthOk => (pal::HEALTH_OK, pal::PANEL),
            Ink::HealthMid => (pal::HEALTH_MID, pal::PANEL),
            Ink::HealthLow => (pal::HEALTH_LOW, pal::PANEL),
            Ink::Warm => (pal::WARM, pal::PANEL),
            Ink::Warn => (pal::WARN, pal::PANEL),
            Ink::Good => (pal::HEALTH_OK, pal::PANEL),
            Ink::Mana => (crate::station::MANA, pal::PANEL),
            Ink::Spent => (pal::TEXT_DIM, pal::PANEL),
            Ink::OnCard => (pal::TEXT, pal::DIMMED),
        }
    }

    /// The health ink for `remaining` of `max`, matching `palette::health`'s thresholds.
    pub fn health(remaining: u8, max: u8) -> Ink {
        let c = pal::health(remaining, max);
        if c == pal::HEALTH_OK {
            Ink::HealthOk
        } else if c == pal::HEALTH_MID {
            Ink::HealthMid
        } else {
            Ink::HealthLow
        }
    }
}

/// The glyph box `s` occupies when drawn at `at` with `align`, one pixel of padding each side.
pub fn label_box(s: &str, at: Point, font: &MonoFont, align: Alignment) -> Rectangle {
    let w = s.chars().count() as i32 * font.character_size.width as i32;
    let h = font.character_size.height as i32;
    let x = match align {
        Alignment::Left => at.x,
        Alignment::Center => at.x - w / 2,
        Alignment::Right => at.x - w,
    };
    Rectangle::new(
        Point::new(x - 1, at.y - 1),
        Size::new((w + 2) as u32, (h + 2) as u32),
    )
}

/// Draw `s` in `ink`, painting the ink's ground under it first. `at` is the glyph box's top-left
/// (or top-centre / top-right, per `align`), as `battlefield::text`.
pub fn label<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    at: Point,
    font: &'static MonoFont,
    ink: Ink,
    align: Alignment,
) {
    let (fg, ground) = ink.pair();
    if !s.is_empty() {
        fill(d, label_box(s, at, font, align), ground);
    }
    text(d, s, at, font, fg, align);
}
