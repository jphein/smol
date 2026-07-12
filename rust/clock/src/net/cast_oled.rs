//! #26 smol Cast — the `DrawTarget` tee-wrapper that feeds the cast [`Mirror`].
//!
//! ## Why this exists
//! ssd1306 0.10's `BufferedGraphicsMode` keeps its 1-bit panel buffer PRIVATE with
//! no read accessor (only `set_pixel` write + `clear`), so there is no way to READ
//! back what was flushed to the glass. To "mirror the gateway's screen" we instead
//! TEE at draw time: [`CastOled`] wraps the real `Ssd1306` and, for every pixel an
//! app draws, writes it to BOTH the panel (via the crate's public `set_pixel`) and a
//! shadow [`Mirror`]. The shadow is then the exact image on the glass, ready to
//! down-sample + stream to a WLED matrix (see `net/cast.rs`).
//!
//! ## Isolation
//! `feature = "cast"` only. When cast is OFF, `app::Oled` stays the plain `Ssd1306`
//! (this file isn't compiled) so the default / wifi / espnow / wled builds are
//! byte-identical — the mirror machinery exists solely in a `--features cast` build.
//!
//! ## Fidelity
//! `draw_iter` here is the SAME two steps the stock `Ssd1306` `DrawTarget` does
//! (bounding-box filter → `set_pixel`), plus one extra `mirror.plot` — so the panel
//! sees byte-identical draws; the mirror is a faithful copy in logical (pre-rotation)
//! coordinates (the 180° mount is applied later, in the down-sampler's `flip180`).

#![cfg(feature = "cast")]

// `prelude::*` brings the traits (`DrawTarget`, `OriginDimensions`, `Dimensions`),
// `Size`, `Point`, and `Pixel`; only `BinaryColor` is outside the prelude.
use embedded_graphics::{pixelcolor::BinaryColor, prelude::*};
use ssd1306::mode::{BufferedGraphicsMode, DisplayConfig};
use ssd1306::prelude::I2CInterface;
use ssd1306::size::DisplaySize72x40;
use ssd1306::Ssd1306;

use crate::net::cast::{Mirror, SRC_H, SRC_W};

/// The concrete SSD1306 — byte-identical to the pre-cast `app::Oled` alias.
type RawOled = Ssd1306<
    I2CInterface<esp_hal::i2c::master::I2c<'static, esp_hal::Blocking>>,
    DisplaySize72x40,
    BufferedGraphicsMode<DisplaySize72x40>,
>;

/// The display's `DrawTarget`/flush error type (`display_interface::DisplayError`),
/// named through the trait so this file needs no direct `display_interface` dep.
type Err = <RawOled as DrawTarget>::Error;

/// Tee display: forwards every draw to the real OLED and mirrors it into `mirror`.
/// Drop-in for `RawOled` — `main` builds it around the freshly-constructed panel and
/// the plugins keep using `clear()` / `draw()` / `flush()` unchanged.
pub struct CastOled {
    inner: RawOled,
    mirror: Mirror,
}

impl CastOled {
    /// Wrap a just-constructed (buffered-graphics) SSD1306.
    pub fn new(inner: RawOled) -> Self {
        Self {
            inner,
            mirror: Mirror::new(),
        }
    }

    /// Initialise the panel (forwards `DisplayConfig::init`).
    pub fn init(&mut self) -> Result<(), Err> {
        self.inner.init()
    }

    /// Flush the dirty region to the panel (forwards the inherent `Ssd1306::flush`).
    pub fn flush(&mut self) -> Result<(), Err> {
        self.inner.flush()
    }

    /// The shadow copy of the current glass image (read by the cast send path).
    pub fn mirror(&self) -> &Mirror {
        &self.mirror
    }
}

impl OriginDimensions for CastOled {
    fn size(&self) -> Size {
        Size::new(SRC_W as u32, SRC_H as u32)
    }
}

impl DrawTarget for CastOled {
    type Color = BinaryColor;
    type Error = Err;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let bb = self.bounding_box();
        for Pixel(pos, color) in pixels {
            if bb.contains(pos) {
                let on = color.is_on();
                // Same write the stock Ssd1306 DrawTarget does…
                self.inner.set_pixel(pos.x as u32, pos.y as u32, on);
                // …plus the shadow copy for casting.
                self.mirror.plot(pos.x as usize, pos.y as usize, on);
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        self.mirror.fill(color.is_on());
        self.inner.clear(color)
    }
}
