//! The colour roles from `docs/design/shrine-ux.md` -> `## Sprite and type budget`.
//!
//! The doc names four faction hues. The shipped card set (`tapstone_rules::cards::TABLE`) has only
//! `Ember`, `Tide` and `Neutral`; `Grove` and `Grave` are defined here because the doc specifies
//! them and the palette must stay whole, but nothing in v0 can draw them yet.

use embedded_graphics::pixelcolor::Rgb565;
use tapstone_rules::cards::Faction;

/// Build an Rgb565 from the doc's 24-bit hex. Rgb565 quantises; the constants below are written
/// as the doc wrote them and the quantisation is left visible rather than pre-rounded.
const fn rgb(hex: u32) -> Rgb565 {
    let (r, g, b) = (
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    );
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

/// `bg #000000` — the dark ground, as on the watch.
pub const BG: Rgb565 = rgb(0x000000);
/// `panel #101728`.
pub const PANEL: Rgb565 = rgb(0x101728);

pub const EMBER: Rgb565 = rgb(0xff8a3d);
pub const TIDE: Rgb565 = rgb(0x35c8e0);
pub const GROVE: Rgb565 = rgb(0x7bd45a);
pub const GRAVE: Rgb565 = rgb(0xb06cff);
/// Neutral has no hue in the doc; a desaturated slate keeps it off the faction axis.
pub const NEUTRAL: Rgb565 = rgb(0x8b93a7);

/// `warn` red — lethal and errors.
pub const WARN: Rgb565 = rgb(0xff5566);
/// `warm` gold — sudden death.
pub const WARM: Rgb565 = rgb(0xffd166);

pub const TEXT: Rgb565 = rgb(0xe8edf7);
pub const TEXT_DIM: Rgb565 = rgb(0x7c869c);
/// One palette step down, for the dimming in the targeting prompt.
pub const DIMMED: Rgb565 = rgb(0x3a4256);

/// Health ramp: green -> amber -> red.
pub const HEALTH_OK: Rgb565 = rgb(0x5fd35f);
pub const HEALTH_MID: Rgb565 = rgb(0xffc043);
pub const HEALTH_LOW: Rgb565 = rgb(0xff5566);

pub fn faction(f: Faction) -> Rgb565 {
    match f {
        Faction::Ember => EMBER,
        Faction::Tide => TIDE,
        Faction::Neutral => NEUTRAL,
    }
}

/// Health colour for `remaining` of `max`.
pub fn health(remaining: u8, max: u8) -> Rgb565 {
    if max == 0 {
        return HEALTH_OK;
    }
    let pct = remaining as u32 * 100 / max as u32;
    if pct > 60 {
        HEALTH_OK
    } else if pct > 30 {
        HEALTH_MID
    } else {
        HEALTH_LOW
    }
}

// Contrast checking lives in `shrine-preview`, not here.
//
// It needs `powf`, which is std-only, and a firmware crate should not carry float math for a
// design-time assertion it never evaluates at runtime. The constants the check reads stay here,
// with the palette they describe; the check itself is an integration test on the host side
// (`shrine-preview/tests/contrast.rs`). The invariant is unchanged and still enforced.

/// The floor a readout must clear against its own ground to be legible on this panel.
pub const MIN_CONTRAST: f32 = 3.0;

/// Every ground a HUD band can have.
pub const BAND_TINTS: [Rgb565; 4] = [EMBER, TIDE, WARM, PANEL];
