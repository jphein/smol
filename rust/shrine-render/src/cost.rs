//! What a frame costs on the panel.
//!
//! # The assumption this whole model rests on
//!
//! **The shrine rasterises locally into a 320×240 RGB565 buffer and blits it over SPI.** Every
//! number below is a property of that path and of this panel — not of any particular firmware
//! seam — which is why the model survived the seam itself being called into question.
//!
//! Two things make that worth stating rather than leaving implicit:
//!
//! * Decision 0010 named the *fleet* flavor, whose `app::Oled` on the S3 is `s3_oled::S3Oled`, a
//!   logical **72×40 one-bit** surface (smol `rust/clock/src/s3_oled.rs`, `app.rs:46`). That is
//!   not a colour framebuffer and 48 px sprites are taller than its whole logical height. 0010's
//!   ruling that the shrine is a framebuffer app stands; the framebuffer it assumed does not.
//!   The colour path on the same board is `targets/s3-cyd/spike-scry`, driving the real
//!   320×240 RGB565 panel through mipidsi.
//! * `spike-scry` today **streams server-rendered frames over WiFi**. If the shrine ends up on
//!   that path rather than rasterising locally, **this model does not apply** and has to be redone
//!   with network latency and frame transfer in it instead of SPI time. Nothing here would
//!   transfer; the bottleneck would not even be the same component.
//!
//! So: the numbers hold for any seam that rasterises locally to this panel, and hold for none
//! that streams. That is the condition, written next to the conclusion.
//!
//! # Where the numbers come from
//!
//! Two measurements, both from smol's own `targets/s3-cyd` docs, both on this exact panel:
//!
//! * **Full 320×240 repaint ≈ 29 ms** — rasterise ≈ 1.6 ms in internal SRAM, wire ≈ 27 ms over
//!   SPI at 40 MHz (`DISPLAY-PACKAGE.md:224`, explore-ember measurement).
//! * **Per-cell SPI windows measured 2× slower than a full-screen repaint** (`BOARD.md:70`).
//!
//! The second one is easy to misread, and the UX doc does misread it: it prices a 56 px row as
//! "23 % of the frame ≈ 7 ms", which is the pixel fraction of a full repaint with no 2× applied.
//! Both cannot be true of the same operation. The primary source resolves it — the sentence the
//! 2× belongs to is about *how* you blit:
//!
//! > Rendering: rasterise in internal SRAM, blit as contiguous windowed writes
//! > (`fill_contiguous`); per-pixel `draw_iter` ≈ one SPI command per pixel. Measured: per-cell
//! > SPI windows are 2× slower than a full-screen repaint.
//!
//! So the penalty is **per window**, not per pixel. One contiguous window is priced by its pixel
//! count, and the doc's 7 ms row is right; scattering the same pixels across nine per-cell windows
//! is what costs 2×. That makes "redraw the row, not the two cells in it" correct advice rather
//! than a rule of thumb.
//!
//! # The honest limit of this model
//!
//! There is **one** measured point for window overhead — nine windows costing 2× a full repaint —
//! and one point cannot fix both the slope and the shape of a curve. `WINDOW_SETUP_MS` below is
//! derived from it by assuming the overhead is linear in window count, which is an assumption and
//! not a measurement. Worse, the value it yields (~4 ms) is implausibly large for the SPI
//! transaction alone: a `set_addr_window` is about 11 bytes, roughly 2 µs on a 40 MHz bus. So the
//! cost is driver, DMA-setup or CS-toggle overhead rather than wire time, and there is no reason
//! to expect it to scale linearly.
//!
//! Every verdict this module produces is therefore reported across a **range** of plausible
//! per-window costs, and any conclusion that flips inside that range is reported as needing a
//! device measurement rather than as an answer. See `Verdict::robust`.

use embedded_graphics::primitives::Rectangle;

use crate::geom;

/// Pixels in a full frame.
pub const FRAME_PX: f32 = (geom::W * geom::H) as f32;

/// Measured full-frame rasterise, milliseconds (`DISPLAY-PACKAGE.md:224`).
///
/// This is CPU time filling a buffer in SRAM, so it does **not** scale with the SPI clock.
pub const RASTER_FULL_MS: f32 = 1.6;

/// Panel SPI clock. `spike-scry/src/main.rs` configures SPI2 with
/// `.with_frequency(Rate::from_mhz(40))`, and `BOARD.md:41` records the same for LCD_CLK.
pub const SPI_HZ: f32 = 40_000_000.0;

/// Bytes in a full frame at RGB565.
pub const FRAME_BYTES: f32 = FRAME_PX * 2.0;

/// Wire time for a full frame, **derived from the clock rather than quoted**.
///
/// # Why this is computed and not a measurement
///
/// Every version of this model until now used `27 ms`, taken from
/// `DISPLAY-PACKAGE.md:224`. That figure is below the physical floor and cannot be right: one bit
/// per clock at 40 MHz puts 1,228,800 bits on the wire in **30.72 ms** before any command
/// overhead, so 27 ms implies 45.5 MHz. The document says where it came from — line 394,
/// *"Full-frame timing for Path A: extrapolated from explore-ember's 320×240 numbers, not
/// measured"*. It was never a full-frame measurement.
///
/// The figures in the same paragraph that **were** measured both sit at line rate and confirm the
/// clock: 107 KiB in ≈21 ms is 41.7 Mbit/s, and 26 KiB in ≈5 ms is 42.6 Mbit/s. So the clock is
/// right and the extrapolation was wrong.
///
/// Deriving it from `SPI_HZ` means a change to the clock moves the model instead of leaving a
/// typed number to disagree with it silently.
///
/// # The direction of the remaining error
///
/// This is a **floor**: real transfers carry command and window-setup overhead on top. So the
/// model now *underestimates* cost, which is the optimistic direction for a "does it fit"
/// verdict. A frame with little headroom needs a device measurement before it is believed; a
/// frame using a third of the budget does not.
pub const WIRE_FULL_MS: f32 = FRAME_BYTES * 8.0 / SPI_HZ * 1000.0;

/// A full repaint: rasterise plus wire.
pub const FULL_REPAINT_MS: f32 = RASTER_FULL_MS + WIRE_FULL_MS;

/// Rasterise cost per pixel, milliseconds.
pub const RASTER_PER_PX_MS: f32 = RASTER_FULL_MS / FRAME_PX;
/// Wire cost per pixel, milliseconds.
pub const WIRE_PER_PX_MS: f32 = WIRE_FULL_MS / FRAME_PX;

/// The nine cells the 2× measurement was taken over (3 lanes × 3 rows, the doc's own board).
pub const MEASURED_WINDOWS: f32 = 9.0;

/// Per-window overhead derived from the single 2× data point, milliseconds.
///
/// Nine windows covering the doc's nine cells cost twice a full repaint. Solving for the overhead
/// with everything else priced per pixel gives this. **It is one point extrapolated, not a
/// measurement** — see the module note.
pub const WINDOW_SETUP_MS: f32 = {
    // 2 * FULL = raster(covered) + wire(covered) + windows * setup
    let covered = (106 * 56 * 9) as f32;
    let per_px = RASTER_PER_PX_MS + WIRE_PER_PX_MS;
    (2.0 * FULL_REPAINT_MS - covered * per_px) / MEASURED_WINDOWS
};

/// The plausible span for per-window overhead, used to test whether a verdict survives being
/// wrong about it. The low end assumes the overhead is nearly free (pure wire time); the high end
/// is the linear extrapolation above.
pub const SETUP_RANGE_MS: (f32, f32) = (0.05, WINDOW_SETUP_MS);

/// The budget one animated frame has: 15 fps is the UX doc's cadence, and it deliberately leaves
/// half the interval for the engine, mesh and touch poll.
pub const FRAME_INTERVAL_MS: f32 = 1000.0 / 15.0;
/// The share of the interval drawing may take.
pub const DRAW_BUDGET_MS: f32 = FRAME_INTERVAL_MS / 2.0;

/// How a frame is pushed to the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Repaint all 320×240 in one window. Costs the same every frame, whatever moved.
    FullRepaint,
    /// One contiguous window per dirty region.
    Windows,
}

/// Cost of pushing `regions` under `strategy`, with a given per-window overhead.
pub fn cost_ms(regions: &[Rectangle], strategy: Strategy, setup_ms: f32) -> f32 {
    match strategy {
        Strategy::FullRepaint => FULL_REPAINT_MS,
        Strategy::Windows => {
            let px: f32 = regions
                .iter()
                .map(|r| (r.size.width * r.size.height) as f32)
                .sum();
            regions.len() as f32 * setup_ms + px * (RASTER_PER_PX_MS + WIRE_PER_PX_MS)
        }
    }
}

/// The pixel count at which one contiguous window stops being cheaper than a full repaint.
pub fn break_even_px(setup_ms: f32) -> f32 {
    ((FULL_REPAINT_MS - setup_ms) / (RASTER_PER_PX_MS + WIRE_PER_PX_MS)).max(0.0)
}

/// A costed frame, and whether the answer survives the uncertainty in `WINDOW_SETUP_MS`.
#[derive(Clone, Copy, Debug)]
pub struct Verdict {
    pub windows: usize,
    pub dirty_px: u32,
    /// Cost with the cheapest plausible per-window overhead.
    pub best_ms: f32,
    /// Cost with the most expensive plausible per-window overhead.
    pub worst_ms: f32,
    pub full_repaint_ms: f32,
}

impl Verdict {
    /// The cheapest way to push this frame, taking the better of the two strategies at each end.
    pub fn chosen_ms(&self) -> (f32, f32) {
        (
            self.best_ms.min(FULL_REPAINT_MS),
            self.worst_ms.min(FULL_REPAINT_MS),
        )
    }

    /// Does the frame fit the draw budget no matter where in the range the true overhead sits?
    ///
    /// This is the question worth answering. A frame that fits at one end and not the other is
    /// not an answer, it is a request for a device measurement.
    pub fn robust(&self) -> bool {
        let (_, worst) = self.chosen_ms();
        worst <= DRAW_BUDGET_MS
    }

    /// Fits only if the optimistic end of the range is right.
    pub fn marginal(&self) -> bool {
        let (best, worst) = self.chosen_ms();
        best <= DRAW_BUDGET_MS && worst > DRAW_BUDGET_MS
    }
}

/// The full lane column between the HUD bands: the largest region any single-lane animation can
/// need, and the candidate for "the natural unit of animation on this panel".
pub fn lane_column() -> Rectangle {
    use embedded_graphics::prelude::*;
    Rectangle::new(
        Point::new(0, geom::HUD),
        Size::new(geom::LANE_CROSS_V as u32, geom::BETWEEN_HUD as u32),
    )
}

/// The same lane column split into `N` separate windows — the naive "redraw each row" shape.
///
/// Returns a fixed array rather than a `Vec`: this crate has no allocator, and the row count is
/// a layout constant rather than runtime data.
pub fn lane_column_by_rows<const N: usize>() -> [Rectangle; N] {
    use embedded_graphics::prelude::*;
    let h = geom::BETWEEN_HUD / N as i32;
    core::array::from_fn(|i| {
        Rectangle::new(
            Point::new(0, geom::HUD + i as i32 * h),
            Size::new(geom::LANE_CROSS_V as u32, h as u32),
        )
    })
}

/// Cost a frame's dirty regions.
pub fn verdict(regions: &[Rectangle]) -> Verdict {
    let dirty_px: u32 = regions.iter().map(|r| r.size.width * r.size.height).sum();
    Verdict {
        windows: regions.len(),
        dirty_px,
        best_ms: cost_ms(regions, Strategy::Windows, SETUP_RANGE_MS.0),
        worst_ms: cost_ms(regions, Strategy::Windows, SETUP_RANGE_MS.1),
        full_repaint_ms: FULL_REPAINT_MS,
    }
}
