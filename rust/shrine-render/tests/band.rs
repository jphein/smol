//! The band memory arithmetic, and the contract that makes banding safe.
//!
//! The byte-equality oracle that proves band-invariance lives in `shrine-preview/tests/banded.rs`,
//! because it needs the simulator. This covers the arithmetic firmware uses to choose a band
//! height, and the reason bands are necessary at all.

use shrine_render::band::*;

/// The premise: a full frame does not fit in internal memory, which is why strips exist.
#[test]
fn a_whole_frame_does_not_fit_internal_memory() {
    assert!(!frame_fits_internal());
    assert_eq!(FRAME_BYTES, 153_600);
    // Both sides are constants, so this is checked at compile time: if someone revises the free
    // memory figure upward past a full frame, the crate stops building and banding becomes a
    // choice rather than a requirement - which is a decision, not a silent test pass.
    const _: () = assert!(FRAME_BYTES > FREE_INTERNAL_BYTES);
}

/// The two figures the firmware survey quoted, reproduced from the arithmetic rather than copied.
#[test]
fn the_surveyed_band_budgets_reproduce() {
    // Four bands is ~38 KB, about 40% of free internal memory.
    assert_eq!(band_bytes(rows_for_bands(4)), 38_400);
    assert_eq!(budget_percent(4), 39);
    // Sixteen bands is ~9.6 KB, about 10%.
    assert_eq!(band_bytes(rows_for_bands(16)), 9_600);
    assert_eq!(budget_percent(16), 9);
}

/// More bands must never cost more memory.
#[test]
fn more_bands_cost_less_memory() {
    let mut last = usize::MAX;
    for n in [2, 4, 8, 12, 16, 24] {
        let b = band_bytes(rows_for_bands(n));
        assert!(b < last, "{n} bands cost {b} B, not less than {last} B");
        last = b;
    }
}

/// `n` bands must always cover the panel, including when `n` does not divide 240.
#[test]
fn bands_always_cover_the_panel() {
    for n in 1..=48usize {
        let rows = rows_for_bands(n);
        assert!(
            rows * n as i32 >= 240,
            "{n} bands of {rows} rows leave the bottom of the panel undrawn"
        );
        // And must not be wasteful: one fewer row would fail to cover.
        assert!(
            (rows - 1) * (n as i32) < 240,
            "{n} bands of {rows} rows is taller than necessary"
        );
    }
}

/// Choosing a band height from a byte budget must land inside that budget.
#[test]
fn a_budget_choice_fits_its_budget() {
    for budget in [4_096usize, 9_600, 16_384, 38_400, 65_536] {
        let n = bands_for_budget(budget);
        assert!(n > 0, "{budget} B should afford at least one band");
        let actual = band_bytes(rows_for_bands(n));
        assert!(
            actual <= budget,
            "{budget} B budget chose {n} bands costing {actual} B"
        );
    }
}

/// A budget too small for a single row must say so rather than dividing by zero.
#[test]
fn an_impossible_budget_is_reported_not_guessed() {
    assert_eq!(rows_for_budget(0), 0);
    assert_eq!(bands_for_budget(0), 0);
    assert_eq!(bands_for_budget(639), 0, "one row is 640 B");
    assert_eq!(bands_for_budget(640), 240, "640 B affords exactly one row");
}
