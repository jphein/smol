//! [`S3Oled`] — the 4th `app::Oled` backend: smol's logical 72×40 `BinaryColor` screens
//! on the ES3C28P's 320×240 ILI9341V, scaled 4× at flush time. (#398 phase 2, the
//! display arm. Board truth: [`crate::board_s3`]. Compiled only under `esp32s3`.)
//!
//! # The stack, top to bottom
//!
//! ```text
//! smol screens (menu.rs, clock.rs, snake.rs …)   171 BinaryColor sites, UNCHANGED
//!         │  DrawTarget<Color = BinaryColor>  +  inherent init/flush
//!         ▼
//!   S3Oled                ← this file: a 360-BYTE logical 1-bit framebuffer + dirty rect
//!         │  flush(): lazy 4× expansion INSIDE the fill_contiguous iterator
//!         ▼
//!   mipidsi Display<…, ILI9341Rgb565, NoResetPin>   ← ONE window per flush
//!         │  SPI2 @ 40 MHz, Mode 0
//!         ▼
//!   the glass (320×240; the 288×160 image letterboxed by (16, 40))
//! ```
//!
//! # ⚠️ Why ZERO-buffer, not the staged 92 KB `oled-scale` design
//!
//! The staging crate (`targets/s3-cyd/backend-staging`, type-proven) stores the scaled
//! 288×160 RGB565 image — 92,160 B, const-initialised into `.bss`. That was correct as a
//! type proof and is WRONG in this binary: on the fleet tier the S3's `.stack` leftover
//! measured 121,036 B (BUDGET-PREP §6.1), and esp-hal's stack is the gap under RAM top —
//! **a 92 KB static would shrink it to ~29 KB, far below any plausible radio floor**
//! (the C6 boot-looped at 61 KB). That is the `stack is not headroom` failure shape,
//! bought knowingly at design time instead of discovered as a WiFi-RX fault later.
//!
//! Storing the LOGICAL frame instead costs `72 × 40 / 8 = 360` bytes. The RGB565 pixels
//! never exist in RAM: `flush` feeds mipidsi's `fill_contiguous` an iterator that maps
//! each physical pixel back to its logical bit (`px/4`, `py/4`) on the fly. mipidsi sets
//! the address window once and streams — wire traffic is byte-identical to the buffered
//! design; the only added cost is a bit-lookup per pixel, and at 40 MHz the wire
//! dominates the rasterise ~17× (explore-ember.md §2), so the lookup is noise.
//!
//! The SPI/panel construction is transcribed from `emberburrito/burrito-fw` `main.rs`
//! (proven on glass on this board class) via the staging crate.

use embedded_graphics::{
    pixelcolor::{raw::RawU16, BinaryColor, Rgb565},
    prelude::*,
    primitives::Rectangle,
};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    spi::{
        master::{Config as SpiConfig, Spi},
        Mode,
    },
    time::Rate,
};
use mipidsi::{
    interface::SpiInterface,
    models::ILI9341Rgb565,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder, NoResetPin,
};

use crate::board_s3 as board;

// ===========================================================================
// Geometry
// ===========================================================================

/// smol's logical panel width — every screen in the registry is written for this.
pub const LOGICAL_W: u32 = 72;
/// smol's logical panel height.
pub const LOGICAL_H: u32 = 40;

/// Integer scale factor. 4× is the largest that fits: `72×4 = 288 ≤ 320` and
/// `40×4 = 160 ≤ 240`; 5× would be 360 wide.
pub const SCALE: u32 = 4;

/// Scaled image width — 288 px.
pub const IMAGE_W: u32 = LOGICAL_W * SCALE;
/// Scaled image height — 160 px.
pub const IMAGE_H: u32 = LOGICAL_H * SCALE;

/// Horizontal letterbox: `(320 − 288) / 2 = 16` px each side. The offset lives HERE —
/// the logical layer knows nothing of the panel, which is what keeps the scaling
/// invisible above the `app::Oled` seam.
pub const LETTERBOX_X: u32 = (board::LCD_WIDTH as u32 - IMAGE_W) / 2;
/// Vertical letterbox: `(240 − 160) / 2 = 40` px top and bottom.
pub const LETTERBOX_Y: u32 = (board::LCD_HEIGHT as u32 - IMAGE_H) / 2;

const _: () = {
    assert!(IMAGE_W <= board::LCD_WIDTH as u32, "scaled image wider than the panel");
    assert!(IMAGE_H <= board::LCD_HEIGHT as u32, "scaled image taller than the panel");
};

/// Foreground (lit) colour — white.
pub const FG: u16 = rgb565(31, 63, 31);
/// Background (unlit) colour — black.
pub const BG: u16 = rgb565(0, 0, 0);

/// Pack 5-6-5 channel values into a raw RGB565 word, in a `const` context.
///
/// Exists because `embedded-graphics-core` 0.4's `RgbColor` channel accessors are not
/// `const` (const traits aren't stable), so a const initialiser cannot decompose an
/// `Rgb565` — the intake note from `oled-scale`, carried here.
pub const fn rgb565(r: u16, g: u16, b: u16) -> u16 {
    ((r & 0x1f) << 11) | ((g & 0x3f) << 5) | (b & 0x1f)
}

// ===========================================================================
// Panel type + construction — lifted from backend-staging (type-proven), which
// transcribed it from burrito-fw (glass-proven)
// ===========================================================================

/// The concrete panel type, named once.
pub type Panel = mipidsi::Display<
    SpiInterface<
        'static,
        ExclusiveDevice<Spi<'static, esp_hal::Blocking>, Output<'static>, Delay>,
        Output<'static>,
    >,
    ILI9341Rgb565,
    NoResetPin,
>;

/// The panel's `DrawTarget` error, **projected rather than hand-spelled**: the true type
/// is three layers from three crates (`SpiError<DeviceError<esp_hal::spi::Error, _>, _>`)
/// and any literal spelling silently encodes today's bus-sharing choice. Found by
/// compiling, not by reading docs (backend-staging's headline finding).
pub type PanelError = <Panel as DrawTarget>::Error;

/// Landscape orientation — mipidsi's spelling of MADCTL `0x28` (`MV | BGR`),
/// human-verified on this unit 2026-08-25 ("readable landscape").
/// ⛔ Not `0x68`: that is `0x28` plus MX, a horizontal mirror — see
/// [`board::MADCTL_LANDSCAPE`] for the story it already shipped once.
pub const ORIENTATION: Orientation = Orientation::new().rotate(Rotation::Deg90).flip_vertical();

/// mipidsi's command/pixel batching scratch. `'static` because [`Panel`] is.
static mut DISPLAY_BUF: [u8; 4096] = [0; 4096];

/// Build the ILI9341V panel on SPI2 from [`crate::board_s3`]'s constants.
///
/// Concrete peripheral types on purpose — the signature IS the type proof.
///
/// ⚠️ No `.reset_pin()`: `LCD_RST` is bonded to `CHIP_PU`/`EN` on this board
/// ([`board::HAS_LCD_RESET_PIN`] is `false`); mipidsi's software reset in `init()` does
/// the work. Handing it an unrelated GPIO "because the type wants one" yields a panel
/// that is never reset — on glass, indistinguishable from a wiring fault.
///
/// ⚠️ The backlight ([`board::PIN_LCD_BACKLIGHT`]) is NOT touched here — the caller turns
/// it on **after** the first full paint, so the panel appears already-lit instead of
/// fading up out of vendor GRAM garbage.
///
/// # Safety contract
/// Takes `&'static mut DISPLAY_BUF` internally — call **once**, at boot, before any
/// concurrency (the same shape burrito-fw uses).
pub fn build_panel(
    spi2: esp_hal::peripherals::SPI2<'static>,
    sck: esp_hal::peripherals::GPIO12<'static>,
    mosi: esp_hal::peripherals::GPIO11<'static>,
    cs: esp_hal::peripherals::GPIO10<'static>,
    dc: esp_hal::peripherals::GPIO46<'static>,
    mut delay: Delay,
) -> Panel {
    let spi = Spi::new(
        spi2,
        SpiConfig::default()
            .with_frequency(Rate::from_hz(board::SPI_DISPLAY_HZ))
            .with_mode(Mode::_0),
    )
    .expect("spi2 config")
    .with_sck(sck)
    .with_mosi(mosi);

    let cs = Output::new(cs, Level::High, OutputConfig::default());
    let dc = Output::new(dc, Level::Low, OutputConfig::default());

    let spi_dev = ExclusiveDevice::new(spi, cs, delay).expect("spi device");

    // SAFETY: called once at boot, before concurrency; DISPLAY_BUF is touched nowhere else.
    let di = SpiInterface::new(spi_dev, dc, unsafe {
        &mut *core::ptr::addr_of_mut!(DISPLAY_BUF)
    });

    Builder::new(ILI9341Rgb565, di)
        .display_size(board::PANEL_NATIVE_W, board::PANEL_NATIVE_H)
        // No .reset_pin() — RST is bonded to EN on this board. See the fn doc.
        .orientation(ORIENTATION)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .expect("ili9341 init")
}

// ===========================================================================
// S3Oled — the app::Oled contract over a 360-byte logical framebuffer
// ===========================================================================

const FB_BYTES: usize = (LOGICAL_W as usize * LOGICAL_H as usize) / 8; // 360

/// The 4th `app::Oled` backend. Satisfies exactly the surface `app.rs` requires
/// (`DrawTarget<Color = BinaryColor>` + `OriginDimensions` + inherent `init()`/`flush()`,
/// with `clear()` arriving as `DrawTarget`'s provided method through `fill_solid`).
pub struct S3Oled {
    panel: Panel,
    /// Logical 72×40, one bit per pixel, row-major, bit `x % 8` of byte `(y*72 + x) / 8`.
    fb: [u8; FB_BYTES],
    /// Dirty logical rect, inclusive: (x0, y0, x1, y1). `None` = clean.
    dirty: Option<(u32, u32, u32, u32)>,
}

impl S3Oled {
    /// Wrap a built panel. Starts **fully dirty** on purpose: a first flush must push the
    /// whole (blank) image, or the panel shows power-on GRAM garbage inside the letterbox
    /// (the lesson `oled-scale`'s test suite pinned).
    pub fn new(panel: Panel) -> Self {
        Self {
            panel,
            fb: [0; FB_BYTES],
            dirty: Some((0, 0, LOGICAL_W - 1, LOGICAL_H - 1)),
        }
    }

    /// Present for `main`'s uniform boot flow; the panel was initialised in
    /// [`build_panel`]. Mirrors `CanvasOled::init`'s harmless-no-op shape. Typed
    /// `PanelError` (though it cannot fail) so the ONE error type serves draws, init
    /// and flush alike — which is what lets `cast_oled`'s tee keep its single `Err`
    /// alias and byte-identical body across all backends.
    pub fn init(&mut self) -> Result<(), PanelError> {
        Ok(())
    }

    /// Set one logical pixel — the inherent `Ssd1306::set_pixel` surface the cast tee
    /// (`net/cast_oled.rs`) writes through. Out-of-range is ignored, as there.
    pub fn set_pixel(&mut self, x: u32, y: u32, on: bool) {
        if x < LOGICAL_W && y < LOGICAL_H {
            self.set_bit(x, y, on);
        }
    }

    #[inline]
    fn bit(&self, x: u32, y: u32) -> bool {
        let idx = (y * LOGICAL_W + x) as usize;
        (self.fb[idx / 8] >> (idx % 8)) & 1 != 0
    }

    #[inline]
    fn set_bit(&mut self, x: u32, y: u32, on: bool) {
        let idx = (y * LOGICAL_W + x) as usize;
        let (byte, mask) = (idx / 8, 1u8 << (idx % 8));
        let old = self.fb[byte];
        let new = if on { old | mask } else { old & !mask };
        if new != old {
            self.fb[byte] = new;
            self.widen(x, y);
        }
    }

    #[inline]
    fn widen(&mut self, x: u32, y: u32) {
        self.dirty = Some(match self.dirty {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    }

    /// Push everything drawn since the last flush, as **one** windowed write.
    ///
    /// The dirty LOGICAL rect maps to a physical rect at 4×, offset by the letterbox;
    /// the RGB565 pixels are produced lazily by the iterator (`px/4, py/4` → bit → FG/BG)
    /// — they never exist in RAM. One `fill_contiguous` = one address window + one
    /// stream, mipidsi's fast path (per-pixel `draw_iter` is the measured anti-pattern).
    /// No traffic when clean.
    pub fn flush(&mut self) -> Result<(), PanelError> {
        let Some((x0, y0, x1, y1)) = self.dirty.take() else {
            return Ok(());
        };

        let (px0, py0) = (x0 * SCALE, y0 * SCALE);
        let (pw, ph) = ((x1 - x0 + 1) * SCALE, (y1 - y0 + 1) * SCALE);

        let area = Rectangle::new(
            Point::new((px0 + LETTERBOX_X) as i32, (py0 + LETTERBOX_Y) as i32),
            Size::new(pw, ph),
        );

        // Row-major over the physical rect; each pixel reads its logical bit on the fly.
        let fb = &self.fb;
        let colors = (py0..py0 + ph).flat_map(move |py| {
            let ly = py / SCALE;
            (px0..px0 + pw).map(move |px| {
                let lx = px / SCALE;
                let idx = (ly * LOGICAL_W + lx) as usize;
                let on = (fb[idx / 8] >> (idx % 8)) & 1 != 0;
                Rgb565::from(RawU16::new(if on { FG } else { BG }))
            })
        });

        self.panel.fill_contiguous(&area, colors)
    }

    /// Paint the letterbox margin once at boot — a deliberate frame instead of power-on
    /// GRAM garbage. Separate from [`flush`](Self::flush): it never needs to run again,
    /// and folding it in would repaint 62 % of the panel every frame.
    pub fn paint_letterbox(&mut self, color: Rgb565) -> Result<(), PanelError> {
        use embedded_graphics::primitives::PrimitiveStyle;
        Rectangle::new(
            Point::zero(),
            Size::new(board::LCD_WIDTH as u32, board::LCD_HEIGHT as u32),
        )
        .into_styled(PrimitiveStyle::with_fill(color))
        .draw(&mut self.panel)
    }
}

impl OriginDimensions for S3Oled {
    /// **Logical 72×40** — smol's screens see the panel they were written for; the
    /// scaling stays invisible above this seam.
    fn size(&self) -> Size {
        Size::new(LOGICAL_W, LOGICAL_H)
    }
}

impl DrawTarget for S3Oled {
    type Color = BinaryColor;
    /// `PanelError`, not `Infallible`, though buffer writes cannot fail: on the other
    /// backends the DrawTarget error and the flush error are the SAME type, and the
    /// cast tee's single `Err` projection depends on that. Draws always return `Ok`.
    type Error = PanelError;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            if (0..LOGICAL_W as i32).contains(&p.x) && (0..LOGICAL_H as i32).contains(&p.y) {
                self.set_bit(p.x as u32, p.y as u32, c.is_on());
            }
        }
        Ok(())
    }

    /// Fast path for solid fills (`DrawTarget::clear` routes here): byte-agnostic bit
    /// loop over ≤ 2,880 logical pixels — trivially cheap at this size, and one dirty
    /// widening per call instead of per pixel.
    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let Some(clipped) = area.intersection(&self.bounding_box()).size_nonzero() else {
            return Ok(());
        };
        let (x0, y0) = (clipped.top_left.x as u32, clipped.top_left.y as u32);
        let (x1, y1) = (x0 + clipped.size.width - 1, y0 + clipped.size.height - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let idx = (y * LOGICAL_W + x) as usize;
                let (byte, mask) = (idx / 8, 1u8 << (idx % 8));
                if color.is_on() {
                    self.fb[byte] |= mask;
                } else {
                    self.fb[byte] &= !mask;
                }
            }
        }
        // One widening for the whole rect — set_bit's change-detection is skipped here,
        // so a redundant solid fill re-dirties; correct (a flush pushes it) and simple.
        self.widen(x0, y0);
        self.widen(x1, y1);
        Ok(())
    }
}

/// `Rectangle::intersection` returns a possibly-zero-sized rect; this names the
/// empty-check so `fill_solid` reads as the two cases it has.
trait SizeNonZero {
    fn size_nonzero(self) -> Option<Rectangle>;
}
impl SizeNonZero for Rectangle {
    fn size_nonzero(self) -> Option<Rectangle> {
        (self.size.width > 0 && self.size.height > 0).then_some(self)
    }
}
