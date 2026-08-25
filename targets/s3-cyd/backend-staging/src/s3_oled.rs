//! [`S3Oled`] — the concrete display type: smol's logical 72×40 `BinaryColor` surface,
//! scaled 4× into RGB565 and blitted to the ES3C28P's 320×240 ILI9341V.
//!
//! # The stack, top to bottom
//!
//! ```text
//! smol screens (menu.rs, clock.rs, snake.rs …)   171 BinaryColor sites, UNCHANGED
//!         │  DrawTarget<Color = BinaryColor>  +  inherent init/flush/clear
//!         ▼
//!   S3Oled                        ← this file: the app::Oled contract, and the blit
//!         │  delegates every draw to …
//!         ▼
//!   oled_scale::ScaledOled<CAP_4X>   ← 72×40 logical → 288×160 RGB565, dirty-rect tracked
//!         │  take_dirty() → one Rectangle
//!         ▼
//!   mipidsi Display<…, ILI9341Rgb565, NoResetPin>   ← fill_contiguous, one window
//!         │  SPI2 @ 40 MHz, Mode 0
//!         ▼
//!   the glass (320×240, 288×160 image letterboxed by (16, 40))
//! ```
//!
//! The SPI/panel construction below is transcribed from `emberburrito/burrito-fw`'s
//! `src/main.rs:397-427`, which drives this exact panel on this exact board class and is
//! **proven on glass**. Every value comes from [`crate::board`].

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
use oled_scale::{rgb565, ScaledOled, CAP_4X, LOGICAL_H, LOGICAL_W};

use crate::board;

// ===========================================================================
// Geometry — the letterbox this crate owns and oled-scale deliberately does not
// ===========================================================================

/// Integer scale factor. 4× is the largest that fits: `72×4 = 288 ≤ 320` and
/// `40×4 = 160 ≤ 240`, while 5× would be `360×200` — too wide.
pub const SCALE: u32 = 4;

/// Scaled image width — 288 px.
pub const IMAGE_W: u32 = LOGICAL_W * SCALE;
/// Scaled image height — 160 px.
pub const IMAGE_H: u32 = LOGICAL_H * SCALE;

/// Horizontal letterbox: `(320 − 288) / 2 = 16` px each side.
///
/// **This offset lives here, not in `oled-scale`**, and that split is deliberate:
/// `oled-scale` knows its own 288×160 image and nothing about any panel, which is exactly
/// what keeps it chip-agnostic and host-testable on plain stable. Dirty rectangles arrive
/// in the image's own coordinates and this file translates them.
pub const LETTERBOX_X: u32 = (board::LCD_WIDTH as u32 - IMAGE_W) / 2;

/// Vertical letterbox: `(240 − 160) / 2 = 40` px top and bottom.
pub const LETTERBOX_Y: u32 = (board::LCD_HEIGHT as u32 - IMAGE_H) / 2;

const _: () = {
    assert!(IMAGE_W <= board::LCD_WIDTH as u32, "scaled image is wider than the panel");
    assert!(IMAGE_H <= board::LCD_HEIGHT as u32, "scaled image is taller than the panel");
};

// ===========================================================================
// Panel type + construction
// ===========================================================================

/// The concrete panel type, named once so it can cross a task boundary.
/// Shape transcribed from `burrito-fw/src/main.rs:61-69`.
pub type Panel = mipidsi::Display<
    SpiInterface<
        'static,
        ExclusiveDevice<Spi<'static, esp_hal::Blocking>, Output<'static>, Delay>,
        Output<'static>,
    >,
    ILI9341Rgb565,
    NoResetPin,
>;

/// The panel's `DrawTarget` error, **projected rather than hand-spelled**.
///
/// ⚠️ **A real finding from getting this crate to compile**, and worth carrying into the
/// intake PR. The naive spelling is `SpiError<esp_hal::spi::Error, Infallible>` — and it is
/// WRONG. The true type is
///
/// ```text
/// SpiError<DeviceError<esp_hal::spi::Error, Infallible>, Infallible>
/// ```
///
/// because `embedded_hal_bus::spi::ExclusiveDevice` wraps bus and CS failures in its own
/// `DeviceError` before mipidsi ever sees them. The error therefore carries **three** layers
/// from three different crates, and any hand-written spelling silently encodes today's
/// bus-sharing choice: swapping `ExclusiveDevice` for a `RefCellDevice` (which an S3 that
/// ever shares SPI2 would need) changes the type and breaks every signature naming it
/// literally.
///
/// Projecting through the trait costs nothing and is immune to all of it.
pub type PanelError = <Panel as DrawTarget>::Error;

/// Landscape orientation — mipidsi's spelling of MADCTL [`board::MADCTL_LANDSCAPE`]
/// (`0x28` = `MV | BGR`).
///
/// ⛔ Not `0x68`. See [`board::MADCTL_LANDSCAPE`] for the retro-go story; that value is
/// `0x28` **plus MX**, a horizontal mirror, and copying it shipped mirror-writing once.
pub const ORIENTATION: Orientation = Orientation::new().rotate(Rotation::Deg90).flip_vertical();

/// mipidsi's command/pixel batching scratch. `'static` because [`Panel`] is.
static mut DISPLAY_BUF: [u8; 4096] = [0; 4096];

/// Build the ILI9341V panel on SPI2 from [`crate::board`]'s constants.
///
/// Transcribed from `burrito-fw/src/main.rs:397-427`. Peripherals are taken by concrete
/// type rather than generically, because that is what makes this function a **type proof**:
/// if esp-hal's SPI2 pin types stopped matching mipidsi's `SpiInterface`, this would fail
/// to compile — which is the entire reason this crate exists.
///
/// # ⚠️ No reset pin, and that is a board fact
///
/// `.reset_pin()` is **not** called: `LCD_RST` is bonded to `CHIP_PU`/`EN`
/// ([`board::HAS_LCD_RESET_PIN`] `== false`), so the panel is already out of reset when
/// firmware runs. mipidsi's [`NoResetPin`] relies on the software reset inside `init()`.
/// Handing it an unrelated GPIO "because the type wants one" drives an unconnected pad and
/// yields a panel that is never reset — on glass, indistinguishable from a wiring fault.
///
/// # ⚠️ The backlight is NOT touched here
///
/// [`board::PIN_LCD_BACKLIGHT`] stays the caller's. Turn it on **after** the first full
/// paint, so the panel appears already-lit instead of fading up out of whatever the vendor
/// firmware left in GRAM (burrito-fw's `main.rs:437-440` does exactly this). Doing it here
/// would take that ordering decision away from the one place that knows when the first
/// frame is ready.
///
/// # Safety
///
/// Takes `&'static mut DISPLAY_BUF`. Call **once**, before any concurrency starts — the
/// same contract and the same `addr_of_mut!` shape burrito-fw uses at `main.rs:413-416`.
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
// The scaling buffer — const-initialised, never stack-built
// ===========================================================================

/// Foreground (lit) colour — white.
pub const FG: u16 = rgb565(31, 63, 31);
/// Background (unlit) colour — black.
pub const BG: u16 = rgb565(0, 0, 0);

/// The 92,160-byte scaled framebuffer, **const-initialised in `.bss`**.
///
/// `288 × 160 × 2 B = 92,160 B`. This is the concrete payoff of `oled-scale`'s
/// `const fn new_raw` (intake note (a)): the buffer is constructed *in place* by the const
/// evaluator, so it never exists as a 92 KB temporary on a task stack.
///
/// A `StaticCell` would also work and is more idiomatic, but `StaticCell::init(v)` takes
/// its value **by move** — the temporary is built on the caller's stack first and only then
/// copied. On this chip that is the difference between working and a stack overflow that
/// presents, per smol's `stack is not headroom` lesson, as a fault somewhere entirely else.
static mut SCALED: ScaledOled<CAP_4X> = ScaledOled::new_raw(SCALE, FG, BG);

/// Hand out the static scaling buffer.
///
/// # Safety
///
/// Call **once**. Same contract as [`build_panel`]'s `DISPLAY_BUF`.
#[allow(clippy::missing_safety_doc)]
pub fn scaled_buffer() -> &'static mut ScaledOled<CAP_4X> {
    // SAFETY: called once at boot, before concurrency starts.
    unsafe { &mut *core::ptr::addr_of_mut!(SCALED) }
}

// ===========================================================================
// S3Oled — the app::Oled contract
// ===========================================================================

/// The 4th `app::Oled` backend.
///
/// Satisfies exactly the surface `rust/clock/src/app.rs:39-43` requires of a backend, and
/// which `hostsim::CanvasOled` (`rust/clock/src/lib.rs:119-180`) already demonstrates:
/// `DrawTarget<Color = BinaryColor>` + `OriginDimensions` + inherent `init()` / `flush()`.
///
/// `clear()` is **not** inherent — it is `DrawTarget`'s provided method, routed through
/// `ScaledOled`'s `fill_solid` fast path. (Flagged at intake: `app.rs` lists
/// "clear()/flush()/init()" together as if all three were alike; only the latter two are
/// inherent on any backend.)
pub struct S3Oled<'a> {
    panel: Panel,
    scaled: &'a mut ScaledOled<CAP_4X>,
}

impl<'a> S3Oled<'a> {
    /// Wrap a built panel and the scaling buffer.
    pub fn new(panel: Panel, scaled: &'a mut ScaledOled<CAP_4X>) -> Self {
        Self { panel, scaled }
    }

    /// Initialise. The panel is already initialised by [`build_panel`], so this only
    /// forwards to the scaling layer — present so a boot-time `display.init()` call from
    /// `main` is harmless, exactly as `CanvasOled::init` is
    /// (`rust/clock/src/lib.rs:137-139`).
    pub fn init(&mut self) -> Result<(), core::convert::Infallible> {
        self.scaled.init()
    }

    /// Push everything drawn since the last flush to the glass, as **one** windowed write.
    ///
    /// # Why one `fill_contiguous` over a row-chained iterator
    ///
    /// mipidsi overrides `fill_contiguous` to set the address window **once** and then
    /// stream pixels, so a single call is already one window plus one pixel run — which is
    /// the shape `burrito-fw/src/canvas.rs:1-8` measured as correct for this panel:
    ///
    /// > mipidsi […] `draw_iter` is one SPI command *per pixel*. […] everything rasterises
    /// > into RAM here and goes out as a single windowed write.
    ///
    /// A full-width rectangle could instead hand over one contiguous slice, avoiding the
    /// per-row `chain`. I deliberately did **not** special-case it:
    ///
    /// * the win is an iterator-adapter optimisation, not fewer SPI transactions — the wire
    ///   traffic is byte-identical either way;
    /// * `explore-ember.md` §2's measurements put a full-screen repaint at ~1.6 ms
    ///   rasterise + ~27 ms wire, so the wire dominates by ~17× and the iterator is noise;
    /// * a second code path that only runs for full-width rectangles is a path that is
    ///   rarely exercised and silently diverges. **If a profile ever shows this iterator on
    ///   a hot path, that is the trade to reopen — with numbers**, which is the same rule
    ///   `esp32c6-watch/src/drivers/panel.rs:83-88` applies to its own `&[u16]` decision.
    ///
    /// Returns `Ok(())` with no traffic when nothing changed — [`ScaledOled::take_dirty`]
    /// yields `None`, and pushing an empty window is worse than skipping it.
    ///
    /// # ⚠️ Backlight ordering
    ///
    /// If this is the **first** paint, the caller should turn
    /// [`board::PIN_LCD_BACKLIGHT`] on *after* this returns — see [`build_panel`].
    pub fn flush(&mut self) -> Result<(), PanelError> {
        let Some(dirty) = self.scaled.take_dirty() else {
            return Ok(());
        };

        let x0 = dirty.top_left.x as u32;
        let y0 = dirty.top_left.y as u32;
        let w = dirty.size.width;
        let h = dirty.size.height;

        // Image coordinates -> panel coordinates. This is the only place the letterbox
        // exists; `oled-scale` is deliberately unaware of it.
        let area = Rectangle::new(
            Point::new((x0 + LETTERBOX_X) as i32, (y0 + LETTERBOX_Y) as i32),
            Size::new(w, h),
        );

        // Row-major RGB565 over the dirty sub-rect, borrowed from the scaled buffer.
        let scaled = &*self.scaled;
        let colors = (y0..y0 + h).flat_map(move |y| {
            let row = scaled.raw_row(y);
            row[x0 as usize..(x0 + w) as usize]
                .iter()
                .map(|&raw| Rgb565::from(RawU16::new(raw)))
        });

        self.panel.fill_contiguous(&area, colors)
    }

    /// Paint the letterbox borders once, so the unused 320×240 margin is a deliberate
    /// frame rather than power-on GRAM garbage.
    ///
    /// Separate from [`flush`](Self::flush) because it only ever needs to run once, and
    /// folding it in would repaint 62 % of the panel on every frame.
    pub fn paint_letterbox(&mut self, color: Rgb565) -> Result<(), PanelError> {
        use embedded_graphics::primitives::PrimitiveStyle;
        let full = Rectangle::new(
            Point::zero(),
            Size::new(board::LCD_WIDTH as u32, board::LCD_HEIGHT as u32),
        );
        full.into_styled(PrimitiveStyle::with_fill(color))
            .draw(&mut self.panel)
    }

    /// Borrow the scaling layer — test seam, and the read-back accessor `ssd1306`'s
    /// `BufferedGraphicsMode` notoriously lacks (`rust/clock/src/net/cast_oled.rs:4-7` had
    /// to build a whole tee-wrapper for want of it).
    pub fn scaled(&self) -> &ScaledOled<CAP_4X> {
        self.scaled
    }
}

impl OriginDimensions for S3Oled<'_> {
    /// **Logical 72×40** — delegated, so smol's screens see the panel they were written
    /// for and the scaling stays invisible above this seam.
    fn size(&self) -> Size {
        self.scaled.size()
    }
}

impl DrawTarget for S3Oled<'_> {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        self.scaled.draw_iter(pixels)
    }

    /// Delegated so `ScaledOled`'s fast path is used rather than this trait's per-pixel
    /// default — the anti-pattern `canvas.rs:1-8` names.
    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        self.scaled.fill_contiguous(area, colors)
    }

    /// Delegated — see [`fill_contiguous`](Self::fill_contiguous). `DrawTarget::clear`
    /// routes here.
    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        self.scaled.fill_solid(area, color)
    }
}
