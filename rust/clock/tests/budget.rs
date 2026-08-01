//! #348 host tests for the per-chip memory budgets (`src/budget.rs`).
//!
//! Gated on `hostsim` like the bard/input suites, and run the same way:
//! `cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu
//!  --lib --test budget`
//!
//! ## Why a test as well as a compile-time assertion
//!
//! The const-assertions in `budget.rs` can only say *no* — a const panic message must be a
//! literal, so the shortfall the reader most wants ("by how much?") cannot appear in it.
//! These tests are where that number lives, checked rather than written in a comment.
//!
//! They also check the direction the assertions structurally cannot: that the predicate can
//! say **yes**. A guard that refuses everything is as useless as one that refuses nothing,
//! and it fails the same way — invisibly, by never being watched doing the other thing.

#![cfg(feature = "hostsim")]

use clock::budget::{cost, ChipBudget, FeatureCost, ESP32C3};

/// The published #335 shortfall. If this test starts failing, either a measurement in
/// `budget.rs` moved (fine — update it *with* its provenance) or someone rounded something.
const PUBLISHED_SHORTFALL: u32 = 6_720;

#[test]
fn c3_dram_headroom_is_the_measured_leftover() {
    // 106,560 B (the async-stack canonical tier, #233 spike) − 74,208 B (4/3 × the 55,656 B
    // measured peak, #302). Both numbers are measurements; this is only their subtraction.
    assert_eq!(ESP32C3.dram_headroom(), 32_352);
}

#[test]
fn c3_flash_headroom_is_the_slot_minus_the_baseline() {
    // 0x1F0000 (2,031,616 B OTA slot) − 1,155,648 B (canonical image, re-measured 2026-08-01
    // through repro_build_bin on three trees; see the provenance note in budget.rs).
    assert_eq!(ESP32C3.flash_headroom(), 875_968);
    assert_eq!(ESP32C3.app_slot_bytes, 2_031_616);
}

/// The whole point of #348: the C3 must refuse the Bard, and refuse it for the *right reason*.
#[test]
fn bard_does_not_fit_the_c3() {
    assert!(!ESP32C3.fits(&cost::BARD));
    assert!(!ESP32C3.fits_dram(&cost::BARD));
}

/// Reproduces #335's published number from declared data instead of a bench run. 39,072 B of
/// static DRAM against 32,352 B of headroom.
#[test]
fn bard_shortfall_matches_the_published_6720() {
    assert_eq!(ESP32C3.dram_shortfall(&cost::BARD), PUBLISHED_SHORTFALL);
    assert_eq!(
        cost::BARD.dram_bytes - ESP32C3.dram_headroom(),
        PUBLISHED_SHORTFALL
    );
}

/// The two axes are independent, and on the C3 they disagree — DRAM refuses, flash is
/// comfortable. Conflating them (WLED's single `WLED_DISABLE_*` namespace) would report the
/// wrong constraint and send someone off to shrink the model blob, which is not the problem.
#[test]
fn the_flash_axis_is_comfortable_and_says_so_separately() {
    assert!(ESP32C3.fits_flash(&cost::BARD));
    assert_eq!(ESP32C3.flash_shortfall(&cost::BARD), 0);
    // 287,392 B of model blob inside 875,968 B of slot headroom.
    assert_eq!(ESP32C3.flash_headroom() - cost::BARD.flash_bytes, 588_576);
}

/// The predicate must be able to say YES. This fixture is a *hypothetical* chip — the C6 has
/// 512 KB of SRAM and would clear this easily, but nobody has measured its baseline, and an
/// unmeasured row does not belong in `budget.rs` (that is the #348 anti-lesson: a capability
/// that is guessed is worse than one that is absent). So the yes-case is proven here, where
/// the numbers are explicitly a fixture and cannot be mistaken for a declaration.
#[test]
fn a_chip_with_room_accepts_the_bard() {
    let roomy = ChipBudget {
        chip: "fixture-not-a-real-chip",
        free_dram_bytes: 200_000,
        stack_floor_bytes: 74_208,
        app_slot_bytes: 0x001F_0000,
        baseline_image_bytes: 1_155_600,
    };
    assert!(roomy.fits(&cost::BARD));
    assert_eq!(roomy.dram_shortfall(&cost::BARD), 0);
}

/// One byte over is over. The boundary is where an off-by-one in `fits_dram` would hide, and
/// it is exactly the case the C3 sits 6,720 B on the wrong side of.
#[test]
fn the_boundary_is_exact() {
    let chip = ChipBudget {
        chip: "fixture-not-a-real-chip",
        free_dram_bytes: 100_000,
        stack_floor_bytes: 74_208,
        app_slot_bytes: 0x001F_0000,
        baseline_image_bytes: 1_155_600,
    };
    let headroom = chip.dram_headroom(); // 25,792
    let exact = FeatureCost {
        feature: "fixture",
        dram_bytes: headroom,
        flash_bytes: 0,
    };
    let one_over = FeatureCost {
        feature: "fixture",
        dram_bytes: headroom + 1,
        flash_bytes: 0,
    };
    assert!(chip.fits_dram(&exact), "exactly the headroom must fit");
    assert!(!chip.fits_dram(&one_over), "one byte over must not fit");
    assert_eq!(chip.dram_shortfall(&one_over), 1);
}

/// Headroom arithmetic must not wrap. A chip whose baseline already overruns its own slot is
/// a bug in the data, but it must report zero headroom rather than 4 GB of it — the
/// underflow would turn the guard into an unconditional pass, which is the one failure mode
/// a guard is not allowed to have.
#[test]
fn overrun_saturates_instead_of_wrapping() {
    let broken = ChipBudget {
        chip: "fixture-not-a-real-chip",
        free_dram_bytes: 10_000,
        stack_floor_bytes: 74_208,
        app_slot_bytes: 100_000,
        baseline_image_bytes: 1_155_600,
    };
    assert_eq!(broken.dram_headroom(), 0);
    assert_eq!(broken.flash_headroom(), 0);
    assert!(!broken.fits(&cost::BARD));
}
