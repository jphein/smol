//! **smol#398 Path A** — the `BinaryColor` → RGB565 integer-scaling display backend.
//!
//! smol's UI is 1-bit and 72×40, and it is not resolution-independent: 171 `BinaryColor`
//! sites across 20 files, with `72`/`40` appearing as bare magic numbers inside layout
//! arithmetic (`batt.rs:306` computes `((72 - w) / 2)`). Rewriting that for a 320×240
//! colour panel is a UI project. **Scaling it is not.**
//!
//! [`ScaledOled`] presents a **logical 72×40 `DrawTarget<Color = BinaryColor>`** — byte-for-byte
//! the surface smol's screens already draw into — and rasterises every logical pixel as an
//! `N×N` block of RGB565 in an internal buffer. smol's menu, clock, snake and bench render
//! through it **unchanged**.
//!
//! # Where this sits in the seam
//!
//! `rust/clock/src/app.rs` defines ONE concrete display type behind a cargo feature. There are
//! three arms today — `Ssd1306` (`app.rs:34-38`), `hostsim::CanvasOled` (`app.rs:44`), and
//! `cast_oled::CastOled` (`app.rs:53`). This type is shaped to be the fourth. `app.rs:39-43`
//! states the bar the hostsim arm had to clear, and it is the same bar here:
//!
//! > the one concrete `Oled` becomes a canvas-backed 72×40 framebuffer that impls the SAME
//! > `DrawTarget<Color = BinaryColor>` + inherent `clear()`/`flush()`/`init()` the plugins
//! > already call — so `snake.rs` / `clock.rs` draw through it UNCHANGED (zero forked render
//! > code, the #152 gate).
//!
//! So the inherent surface here mirrors `CanvasOled` (`rust/clock/src/lib.rs:119-180`):
//! [`new`](ScaledOled::new) · [`init`](ScaledOled::init) · [`flush`](ScaledOled::flush) ·
//! a buffer accessor. `clear()` is **not** inherent on either type — it is `DrawTarget`'s
//! provided method, which routes through [`fill_solid`](ScaledOled::fill_solid).
//!
//! # Blit discipline
//!
//! `emberburrito/burrito-fw/src/canvas.rs:1-8` records the measured rule for this panel family:
//!
//! > mipidsi overrides `fill_contiguous`/`fill_solid`, but its `draw_iter` is **one SPI command
//! > per pixel**. […] So everything rasterises into RAM here and goes out as a single windowed
//! > write.
//!
//! Hence: [`fill_solid`](ScaledOled::fill_solid) and [`fill_contiguous`](ScaledOled::fill_contiguous)
//! are both overridden with real fast paths (row-wise `slice::fill`, not per-pixel), and
//! [`take_dirty`](ScaledOled::take_dirty) hands the backend **one** rectangle to push per flush.
//!
//! # This crate is placement-agnostic — deliberately
//!
//! It knows its own scaled image (`288×160` at 4×) and **nothing about the 320×240 panel**.
//! The letterbox offset (16 px each side, 40 px top and bottom) belongs to the S3 backend, not
//! here — which is what keeps this crate chip-agnostic and host-testable on plain stable.
//! Dirty rectangles are therefore in **this image's own coordinates**, origin at its top-left;
//! the backend adds its offset before calling `set_addr_window`.
//!
//! # ⚠️ Buffer placement is the caller's problem, and it is not a small one
//!
//! At 4× the buffer is `288 × 160 × 2 B = 92,160 B` — see [`CAP_4X`]. That must **not** land on
//! a task stack. [`new`](ScaledOled::new) is a `const fn` precisely so the S3 backend can place
//! it in a `static`/`StaticCell` rather than construct it in a local. Compare smol's own
//! `stack-is-not-headroom` lesson: esp-hal shrinks `.stack` silently as `.bss` grows, so "it
//! links" proves nothing. This crate allocates nothing and chooses nothing; it only makes the
//! const-placement possible.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

use embedded_graphics::{
    pixelcolor::{raw::RawU16, BinaryColor, Rgb565},
    prelude::*,
    primitives::Rectangle,
};

/// smol's logical panel width — the 0.42" SSD1306 every screen's layout assumes.
/// Mirrors `rust/clock/src/lib.rs:115`.
pub const LOGICAL_W: u32 = 72;
/// smol's logical panel height. Mirrors `rust/clock/src/lib.rs:117`.
pub const LOGICAL_H: u32 = 40;

/// Buffer capacity in **pixels** for the 4× scale, the recommended factor for a 320×240 panel.
///
/// `72 × 4 = 288` and `40 × 4 = 160`, so `288 × 160 = 46,080` pixels = **92,160 bytes** of
/// RGB565. 5× would be `360 × 200` — wider than 320, so it does not fit; 4× is the largest
/// integer scale that does, and integer scaling means nearest-neighbour with no interpolation.
pub const CAP_4X: usize = (LOGICAL_W as usize * 4) * (LOGICAL_H as usize * 4);

/// The 4× display, ready to alias into `app::Oled`.
pub type ScaledOled4x = ScaledOled<CAP_4X>;

/// A dirty rectangle in **physical (scaled-image) coordinates**, inclusive on both corners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dirty {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Dirty {
    const fn point(x: u32, y: u32) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        }
    }

    fn union_block(&mut self, x: u32, y: u32, w: u32, h: u32) {
        if x < self.x0 {
            self.x0 = x;
        }
        if y < self.y0 {
            self.y0 = y;
        }
        let (xe, ye) = (x + w - 1, y + h - 1);
        if xe > self.x1 {
            self.x1 = xe;
        }
        if ye > self.y1 {
            self.y1 = ye;
        }
    }
}

/// Logical-72×40, physical-`N×`-scaled RGB565 draw target with dirty-rect tracking.
///
/// # Why `CAP` is the const parameter and `scale` is a runtime field
///
/// The natural spelling would be `ScaledOled<const SCALE: usize>` with a
/// `[u16; LOGICAL_W * SCALE * LOGICAL_H * SCALE]` buffer — but a const-generic **expression**
/// in an array length needs `generic_const_exprs`, which is unstable. This crate's entire value
/// is that it compiles and tests on **plain stable** for the host, so that spelling is
/// unavailable, not merely unfashionable.
///
/// The workaround is the one the sibling crate on this very board already uses —
/// `emberburrito/burrito-fw/src/canvas.rs:8-9`:
///
/// > Capacity is a const parameter but width/height are runtime, so one buffer type serves the
/// > boot flame and the grid cards without const-generic arithmetic.
///
/// Matching it keeps two buffers on one board shaped the same way. `scale` is validated against
/// `CAP` in [`new`](Self::new); an over-large scale is **clamped, not panicked**, for the reason
/// `canvas.rs:21-23` gives: *"a panic on the board is a black screen and a reboot loop."*
pub struct ScaledOled<const CAP: usize> {
    /// RGB565 stored **raw**, not as [`Rgb565`].
    ///
    /// The panel contract this feeds — `esp32c6-watch/src/drivers/panel.rs:76-88` — takes
    /// `push_pixels(&[u16])`, deliberately, so that byte order stays the driver's problem and
    /// callers stay panel-agnostic. Storing `Rgb565` here would force a conversion pass on every
    /// blit to satisfy that contract; storing raw makes [`raw_pixels`](Self::raw_pixels) free.
    px: [u16; CAP],
    scale: u32,
    fg: u16,
    bg: u16,
    dirty: Option<Dirty>,
}

impl<const CAP: usize> ScaledOled<CAP> {
    /// Build a target from **raw** RGB565 colours. `const fn`, and that is load-bearing: the S3
    /// backend must place 92 KB in a `static`/`StaticCell`, not build it on a task stack (see the
    /// module note on buffer placement).
    ///
    /// # Why raw `u16` and not [`Rgb565`]
    ///
    /// `RgbColor::r()/g()/b()` are **not** `const` in `embedded-graphics-core` 0.4.1
    /// (`pixelcolor/rgb_color.rs:8` — the trait itself is not const, and const traits are not on
    /// stable), so a `const fn` cannot decompose an `Rgb565` into channels. Taking the packed
    /// value keeps the const path open. Use [`rgb565`] to build one in a `const` context, or
    /// [`new`](Self::new) when you already have an `Rgb565` and are not in one.
    ///
    /// If `scale` would need more than `CAP` pixels it is **clamped** to the largest factor that
    /// fits (minimum 1) rather than panicking — `canvas.rs:21-23`'s rule: *"a panic on the board
    /// is a black screen and a reboot loop."* The buffer starts filled with `bg` and **fully
    /// dirty**, so the first flush paints the whole image instead of leaving the panel showing
    /// whatever its GRAM held at power-on.
    pub const fn new_raw(scale: u32, fg: u16, bg: u16) -> Self {
        let mut scale = if scale == 0 { 1 } else { scale };
        // Clamp instead of panicking. `const fn` forbids most of std, so this is a plain loop.
        while (LOGICAL_W as usize * scale as usize) * (LOGICAL_H as usize * scale as usize) > CAP
            && scale > 1
        {
            scale -= 1;
        }
        Self {
            px: [bg; CAP],
            scale,
            fg,
            bg,
            dirty: Some(Dirty {
                x0: 0,
                y0: 0,
                x1: LOGICAL_W * scale - 1,
                y1: LOGICAL_H * scale - 1,
            }),
        }
    }

    /// Build a target from [`Rgb565`] colours. Convenience wrapper over
    /// [`new_raw`](Self::new_raw); **not** `const` for the reason documented there.
    pub fn new(scale: u32, fg: Rgb565, bg: Rgb565) -> Self {
        Self::new_raw(scale, rgb565_raw(fg), rgb565_raw(bg))
    }

    /// Initialise. No-op — mirrors `CanvasOled::init` (`rust/clock/src/lib.rs:137-139`) so a boot
    /// call from `main` is harmless. Panel bring-up belongs to the driver below this type.
    pub fn init(&mut self) -> Result<(), core::convert::Infallible> {
        Ok(())
    }

    /// Flush. No-op **here** — this type owns no bus. Present so plugins can call
    /// `display.flush()` unchanged, exactly as `CanvasOled::flush` is
    /// (`rust/clock/src/lib.rs:142-145`).
    ///
    /// The S3 backend's own `flush` wraps this: [`take_dirty`](Self::take_dirty), then one
    /// windowed write of that rectangle. Returning `Ok` here is not a claim that pixels reached
    /// glass — it is the same no-op contract the host emulator already ships.
    pub fn flush(&mut self) -> Result<(), core::convert::Infallible> {
        Ok(())
    }

    /// The scale factor actually in use (post-clamp).
    pub const fn scale(&self) -> u32 {
        self.scale
    }

    /// Physical size of the scaled image. **Not** the panel's size — see the module note on
    /// placement-agnosticism.
    pub const fn physical_size(&self) -> Size {
        Size::new(LOGICAL_W * self.scale, LOGICAL_H * self.scale)
    }

    /// The scaled RGB565 image, row-major, `physical_size().width` pixels per row.
    ///
    /// Raw `u16` so it feeds `PanelDriver::push_pixels(&[u16])` with no conversion.
    pub fn raw_pixels(&self) -> &[u16] {
        &self.px[..(LOGICAL_W * self.scale * LOGICAL_H * self.scale) as usize]
    }

    /// One physical row of the scaled image — what a row-band blit pushes.
    pub fn raw_row(&self, y: u32) -> &[u16] {
        let w = (LOGICAL_W * self.scale) as usize;
        let start = y as usize * w;
        &self.px[start..start + w]
    }

    /// Recolour without touching the framebuffer's logical content.
    ///
    /// The cheap extension the survey flagged: per-screen colours (a colour clock, a red DIAG
    /// toast) without a single smol screen learning about `Rgb565`. Repaints every pixel that
    /// currently holds the old colours and marks the image fully dirty.
    pub fn set_colors(&mut self, fg: Rgb565, bg: Rgb565) {
        let (nfg, nbg) = (rgb565_raw(fg), rgb565_raw(bg));
        if nfg == self.fg && nbg == self.bg {
            return;
        }
        let (ofg, obg) = (self.fg, self.bg);
        let used = (LOGICAL_W * self.scale * LOGICAL_H * self.scale) as usize;
        for p in self.px[..used].iter_mut() {
            if *p == ofg {
                *p = nfg;
            } else if *p == obg {
                *p = nbg;
            }
        }
        self.fg = nfg;
        self.bg = nbg;
        self.mark_all();
    }

    /// Take the minimal rectangle covering everything drawn since the last take, in **physical
    /// (scaled-image) coordinates**, and reset the tracker.
    ///
    /// `None` means nothing changed — the backend should skip the blit entirely rather than
    /// push an empty window.
    pub fn take_dirty(&mut self) -> Option<Rectangle> {
        let d = self.dirty.take()?;
        Some(Rectangle::new(
            Point::new(d.x0 as i32, d.y0 as i32),
            Size::new(d.x1 - d.x0 + 1, d.y1 - d.y0 + 1),
        ))
    }

    /// Peek at the dirty rectangle without clearing it.
    pub fn peek_dirty(&self) -> Option<Rectangle> {
        let d = self.dirty?;
        Some(Rectangle::new(
            Point::new(d.x0 as i32, d.y0 as i32),
            Size::new(d.x1 - d.x0 + 1, d.y1 - d.y0 + 1),
        ))
    }

    /// Read back one logical pixel — `true` where the scaled block holds `fg`. Test seam and
    /// the read accessor `ssd1306`'s `BufferedGraphicsMode` notoriously lacks
    /// (`rust/clock/src/net/cast_oled.rs:4-7` had to build a whole tee-wrapper for want of it).
    pub fn logical_pixel(&self, x: u32, y: u32) -> Option<bool> {
        if x >= LOGICAL_W || y >= LOGICAL_H {
            return None;
        }
        let w = LOGICAL_W * self.scale;
        Some(self.px[(y * self.scale * w + x * self.scale) as usize] == self.fg)
    }

    fn mark_all(&mut self) {
        self.dirty = Some(Dirty {
            x0: 0,
            y0: 0,
            x1: LOGICAL_W * self.scale - 1,
            y1: LOGICAL_H * self.scale - 1,
        });
    }

    /// Paint one logical pixel's `scale × scale` block and widen the dirty rect.
    /// Out-of-bounds logical coordinates are dropped, mirroring `CanvasOled::draw_iter`'s bounds
    /// check (`rust/clock/src/lib.rs:167-175`).
    fn set_logical(&mut self, x: i32, y: i32, color: BinaryColor) {
        if x < 0 || y < 0 || x as u32 >= LOGICAL_W || y as u32 >= LOGICAL_H {
            return;
        }
        let raw = if matches!(color, BinaryColor::On) {
            self.fg
        } else {
            self.bg
        };
        let (s, w) = (self.scale, LOGICAL_W * self.scale);
        let (px0, py0) = (x as u32 * s, y as u32 * s);
        for row in 0..s {
            let start = ((py0 + row) * w + px0) as usize;
            self.px[start..start + s as usize].fill(raw);
        }
        match &mut self.dirty {
            Some(d) => d.union_block(px0, py0, s, s),
            None => {
                let mut d = Dirty::point(px0, py0);
                d.union_block(px0, py0, s, s);
                self.dirty = Some(d);
            }
        }
    }

    /// Clip a logical rectangle to the panel, returning inclusive `(x0, y0, x1, y1)`.
    fn clip(area: &Rectangle) -> Option<(u32, u32, u32, u32)> {
        let bounds = Rectangle::new(Point::zero(), Size::new(LOGICAL_W, LOGICAL_H));
        let a = area.intersection(&bounds);
        if a.size.width == 0 || a.size.height == 0 {
            return None;
        }
        Some((
            a.top_left.x as u32,
            a.top_left.y as u32,
            a.top_left.x as u32 + a.size.width - 1,
            a.top_left.y as u32 + a.size.height - 1,
        ))
    }
}

/// Pack 5-6-5 channel values into a raw RGB565 `u16`. **`const`** — this is the constructor to
/// use when building a [`ScaledOled`] in a `static`, since `Rgb565`'s own accessors are not
/// `const` (see [`ScaledOled::new_raw`]).
///
/// Channels are used as-is: `r`/`b` carry 5 significant bits (0..=31), `g` carries 6 (0..=63),
/// matching `Rgb565::new`'s contract.
pub const fn rgb565(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16) << 11) | ((g as u16) << 5) | (b as u16)
}

/// [`Rgb565`] \u2192 raw `u16`, via the crate's own raw conversion.
fn rgb565_raw(c: Rgb565) -> u16 {
    RawU16::from(c).into_inner()
}

impl<const CAP: usize> OriginDimensions for ScaledOled<CAP> {
    /// **Logical** 72×40 — the whole point. smol's screens must see the panel they were written
    /// for; the scaling is invisible above this seam.
    fn size(&self) -> Size {
        Size::new(LOGICAL_W, LOGICAL_H)
    }
}

impl<const CAP: usize> DrawTarget for ScaledOled<CAP> {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            self.set_logical(coord.x, coord.y, color);
        }
        Ok(())
    }

    /// Row-major fill from a colour iterator.
    ///
    /// The iterator is advanced for **every** pixel of `area`, including ones clipped away, so a
    /// partially off-screen area stays in phase with its colour stream — getting this wrong
    /// shears the image rather than clipping it.
    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let mut colors = colors.into_iter();
        for y in area.rows() {
            for x in area.columns() {
                match colors.next() {
                    Some(c) => self.set_logical(x, y, c),
                    None => return Ok(()),
                }
            }
        }
        Ok(())
    }

    /// Solid fill — the fast path, and the one `DrawTarget::clear` routes through.
    ///
    /// Fills whole physical rows with `slice::fill` (one memset per row) instead of walking
    /// `scale²` pixels per logical cell, and widens the dirty rect **once** for the entire
    /// block rather than once per pixel.
    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let Some((lx0, ly0, lx1, ly1)) = Self::clip(area) else {
            return Ok(());
        };
        let raw = if matches!(color, BinaryColor::On) {
            self.fg
        } else {
            self.bg
        };
        let (s, w) = (self.scale, LOGICAL_W * self.scale);
        let (px0, py0) = (lx0 * s, ly0 * s);
        let (pw, ph) = ((lx1 - lx0 + 1) * s, (ly1 - ly0 + 1) * s);
        for row in 0..ph {
            let start = ((py0 + row) * w + px0) as usize;
            self.px[start..start + pw as usize].fill(raw);
        }
        match &mut self.dirty {
            Some(d) => d.union_block(px0, py0, pw, ph),
            None => {
                let mut d = Dirty::point(px0, py0);
                d.union_block(px0, py0, pw, ph);
                self.dirty = Some(d);
            }
        }
        Ok(())
    }
}
