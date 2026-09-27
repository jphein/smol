//! Banded rendering: the contract, and the memory arithmetic behind it.
//!
//! # The contract
//!
//! **Every entry point in this crate positions from panel coordinates and never asks the target
//! where it is.** No call to `bounding_box()`, `size()` or `dimensions()` appears anywhere in the
//! drawing code. That is what makes the renderer band-invariant: drawing the whole screen into a
//! strip, with the panel's coordinate space translated so absolute positions land in that strip,
//! produces exactly the pixels a full-panel render would have put there.
//!
//! So firmware may rasterise a horizontal strip at a time into a small buffer and blit it:
//!
//! ```ignore
//! for band in 0..n {
//!     let y0 = band * rows;
//!     strip.clear(bg)?;
//!     battlefield::draw(&mut strip.translated(Point::new(0, -y0)), &game, view, &layout, &opts);
//!     panel.blit(0, y0, &strip)?;   // one contiguous windowed write
//! }
//! ```
//!
//! **Band count is not constrained by this crate.** It is verified byte-identical against a
//! one-pass render for 2, 4, 7, 8, 9, 12, 16 and 24 bands — including 7 and 9, which do not
//! divide 240, so the last strip is short. Choose the band height from the memory budget; the
//! renderer does not care.
//!
//! The one rule that **is** a requirement: anything added to this crate must keep positioning
//! absolutely. A single `bounding_box()` would silently make each band draw its own copy of
//! whatever was positioned from it, and each band would still look plausible in isolation, which
//! is why it is checked by a byte-equality oracle rather than by review.
//!
//! # Why bands at all
//!
//! `BOARD.md`'s house rule for this panel is strip-rasterise plus windowed contiguous writes, and
//! `spike-scry` never buffers a whole frame by explicit design. The arithmetic forces it: a full
//! 320×240 RGB565 frame is 153,600 B, against 96,676 B of free internal memory. A frame does not
//! fit; a band does.
//!
//! That also happens to agree with what `cost` concludes from timing alone — one contiguous
//! window per frame, a lane-width band being the natural unit. Two independent routes to the same
//! shape is a better reason to believe it than either on its own.

use crate::geom;

/// Bytes per pixel on this panel (RGB565).
pub const BYTES_PER_PX: usize = 2;

/// A full frame, for comparison: 320 × 240 × 2 = 153,600 B.
pub const FRAME_BYTES: usize = (geom::W * geom::H) as usize * BYTES_PER_PX;

/// Free internal memory measured on the S3 with the app slot wired.
///
/// The figure comes from the firmware survey, not from this crate; it is here so the arithmetic
/// below has its source attached rather than being a bare constant.
pub const FREE_INTERNAL_BYTES: usize = 96_676;

/// Bytes a strip of `rows` rows occupies: 640 B per row at 320 px × 2 B.
pub const fn band_bytes(rows: i32) -> usize {
    geom::W as usize * BYTES_PER_PX * rows as usize
}

/// Rows per band when the panel is split into `n` bands, rounding up so `n` bands always cover it.
pub const fn rows_for_bands(n: usize) -> i32 {
    (geom::H + n as i32 - 1) / n as i32
}

/// The tallest band that fits in `budget` bytes, or 0 if not even one row does.
pub const fn rows_for_budget(budget: usize) -> i32 {
    let per_row = geom::W as usize * BYTES_PER_PX;
    (budget / per_row) as i32
}

/// The fewest bands whose strip fits in `budget` bytes.
pub const fn bands_for_budget(budget: usize) -> usize {
    let rows = rows_for_budget(budget);
    if rows <= 0 {
        return 0;
    }
    ((geom::H + rows - 1) / rows) as usize
}

/// Share of free internal memory a given band count costs, in percent.
pub const fn budget_percent(n: usize) -> usize {
    band_bytes(rows_for_bands(n)) * 100 / FREE_INTERNAL_BYTES
}

/// Does a whole frame fit in internal memory? It does not, which is why bands exist.
pub const fn frame_fits_internal() -> bool {
    FRAME_BYTES <= FREE_INTERNAL_BYTES
}
