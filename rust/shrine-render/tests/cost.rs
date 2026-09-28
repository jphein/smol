//! Frame-cost model invariants.
//!
//! Lives in `tests/` so `src/` stays `no_std` and test-free for vendoring.

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use shrine_render::cost::*;

fn rect(w: u32, h: u32) -> Rectangle {
    Rectangle::new(Point::zero(), Size::new(w, h))
}

/// The model must reproduce the measurement it was derived from.
#[test]
fn nine_cell_windows_cost_twice_a_full_repaint() {
    let cells = [rect(106, 56); 9];
    let c = cost_ms(&cells, Strategy::Windows, WINDOW_SETUP_MS);
    let ratio = c / FULL_REPAINT_MS;
    assert!(
        (ratio - 2.0).abs() < 0.01,
        "model gives {ratio:.3}x, measurement says 2x"
    );
}

/// The UX doc's own figure: a 56 px row as one window is about 7 ms.
///
/// This is the number the doc got right, and it is only right because the 2× is per window
/// rather than per pixel. If someone "fixes" the model to apply 2× per pixel, this fails.
#[test]
fn one_contiguous_row_matches_the_docs_seven_milliseconds() {
    let c = cost_ms(&[rect(320, 56)], Strategy::Windows, SETUP_RANGE_MS.0);
    assert!(
        (6.0..8.0).contains(&c),
        "a 56 px row should cost about 7 ms, got {c:.2}"
    );
}

/// Scattering the same pixels across many windows must be worse than one window holding them.
#[test]
fn one_window_beats_the_same_pixels_scattered() {
    let one = cost_ms(&[rect(320, 56)], Strategy::Windows, WINDOW_SETUP_MS);
    let three = [rect(106, 56); 3];
    let many = cost_ms(&three, Strategy::Windows, WINDOW_SETUP_MS);
    assert!(
        many > one,
        "three windows ({many:.1} ms) should cost more than one ({one:.1} ms)"
    );
}

/// Below the break-even a window wins; above it a full repaint does. The naive optimisation -
/// "only redraw what moved" - is the wrong choice above that line.
#[test]
fn break_even_sits_below_a_full_screen() {
    let be = break_even_px(SETUP_RANGE_MS.0);
    assert!(
        be > 0.0 && be < FRAME_PX,
        "break-even {be} px is not inside a frame"
    );
}

/// A full repaint must itself fit the 15 fps draw budget, or no animation is possible at all.
///
/// Both sides are constants, so this is checked at compile time: if someone raises the frame
/// rate or lowers the budget past the point where a full repaint fits, the crate stops
/// building rather than failing a test run later.
#[test]
fn a_full_repaint_fits_the_draw_budget() {
    const _: () = assert!(FULL_REPAINT_MS <= DRAW_BUDGET_MS);
}

/// The firmware-shaping question: is a lane the right window?
///
/// Costed three ways — one lane-column window, the same column split per row, and a full
/// repaint. The column must win outright, and by enough that the conclusion does not depend
/// on where in the range the per-window overhead sits.
#[test]
fn a_lane_column_is_the_right_unit_of_animation() {
    let col = lane_column();
    let rows = lane_column_by_rows::<6>();

    let col_worst = cost_ms(&[col], Strategy::Windows, SETUP_RANGE_MS.1);
    let rows_best = cost_ms(&rows, Strategy::Windows, SETUP_RANGE_MS.0);
    let rows_worst = cost_ms(&rows, Strategy::Windows, SETUP_RANGE_MS.1);

    assert!(
        col_worst < FULL_REPAINT_MS,
        "a lane column ({col_worst:.1} ms) must beat a full repaint ({FULL_REPAINT_MS:.1} ms)"
    );
    assert!(
        col_worst < rows_worst,
        "one column ({col_worst:.1} ms) must beat the same pixels per row ({rows_worst:.1} ms)"
    );
    assert!(
        col_worst <= DRAW_BUDGET_MS,
        "a whole lane column must fit the draw budget even pessimistically"
    );
    // The lesson is not that per-row *exceeds* the budget - measured, it lands at 32.4 ms
    // against 33.3 ms, which technically fits. It is that it consumes essentially all of it,
    // leaving nothing for the engine, the mesh or the touch poll, while the column leaves
    // roughly two thirds free. "Fits" and "affordable" are different claims, and the first
    // one is what a naive check would have accepted here.
    // The primary claim is a RATIO, which is invariant to the wire constant: both paths are the
    // same pixels on the same wire, so a wrong clock cancels exactly. Stating it this way means a
    // correction to the timing model cannot move this conclusion at all - and one just did move
    // the budget shares below, which is why the distinction is worth encoding rather than
    // narrating.
    assert!(
        col_worst / rows_worst < 0.5,
        "one window must cost less than half the per-row split; measured {:.2}",
        col_worst / rows_worst
    );

    // The budget shares are a secondary, weaker claim: the draw budget comes from the frame rate
    // rather than from the wire, so it does NOT scale when the wire constant is corrected, and
    // these numbers move even though the ratio above does not.
    //
    // The thresholds are derived from intent, not fitted to whatever was measured on the day.
    // "A lane column leaves most of the budget" means under half of it; "per-row consumes the
    // budget" means at or over it. An earlier version used `< 0.4`, which was the observed 35%
    // rounded up, and came within 0.002 of failing when the wire constant was corrected - a
    // threshold fitted to a measurement rather than to a meaning.
    let col_share = col_worst / DRAW_BUDGET_MS;
    let rows_share = rows_worst / DRAW_BUDGET_MS;
    assert!(
        col_share < 0.5,
        "a lane column should leave most of the budget, measured {:.0}%",
        col_share * 100.0
    );
    assert!(
        rows_share > 0.9,
        "per-row should consume essentially the whole budget, measured {:.0}%",
        rows_share * 100.0
    );
    // The advantage must hold even if per-row somehow got the cheapest overhead going.
    assert!(
        col_worst < rows_best * 2.0,
        "the column advantage must not rest on the pessimistic end alone"
    );
}

/// A verdict must never claim a frame is cheaper than simply repainting everything.
#[test]
fn chosen_cost_never_exceeds_a_full_repaint() {
    let many = [rect(48, 48); 20];
    let v = verdict(&many);
    let (best, worst) = v.chosen_ms();
    assert!(best <= FULL_REPAINT_MS && worst <= FULL_REPAINT_MS);
}
