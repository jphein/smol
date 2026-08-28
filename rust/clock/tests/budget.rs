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

use clock::budget::{
    cost, ChipBudget, FeatureCost, ESP32C3, ESP32C3_MEASURED_PEAK_BYTES,
    ESP32C3_STACK_FLOOR_BYTES, ESP32C6_WATCH, ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES,
    ESP32C6_WATCH_HEADROOM_OVERSTATEMENT_BYTES,
};

/// The #335 shortfall as published **pre-T**. Kept as a historical anchor rather than a live
/// expectation — see `bard_dram_verdict_flipped_and_this_is_NOT_a_clearance`.
const PUBLISHED_SHORTFALL_PRE_T: u32 = 6_720;

/// The floor is 4/3 of the highest measured peak, and the budget uses that same constant — the
/// one `tools/repro_build.sh` parses. Before #348's follow-up there were two floors for one
/// concept (73,728 in the shell, 74,208 here) and the shell's was derived from the *lowest* of
/// four recorded peaks. There is now one, and this pins it to its input.
#[test]
fn the_floor_is_four_thirds_of_the_highest_measured_peak() {
    assert_eq!(
        ESP32C3_MEASURED_PEAK_BYTES, 48_356,
        "#335 STEP T, id50, crown + relay OTA fetch + 4,471-chunk serve — pairs 4/5 EXERCISED, \
         so this is not a lower bound"
    );
    assert_eq!(ESP32C3_STACK_FLOOR_BYTES, 64_475);
    // ⚠️ Was `assert_eq!` while the peak divided evenly by 3. It no longer does:
    // 48,356 × 4 / 3 = 64,474.67, the floor is the CEILING 64,475, and integer arithmetic
    // truncates the right-hand side to 64,474. An equality here would now reject the
    // correctly-rounded floor — the relation is, and always was, `>=` with a ceiling.
    assert!(ESP32C3_STACK_FLOOR_BYTES >= ESP32C3_MEASURED_PEAK_BYTES * 4 / 3);
    assert_eq!(
        ESP32C3_STACK_FLOOR_BYTES,
        (ESP32C3_MEASURED_PEAK_BYTES * 4).div_ceil(3),
        "floor must be exactly ceil(peak × 4/3), not merely >= it"
    );
    // And the chip row must not carry a second, independent copy of it.
    assert_eq!(ESP32C3.stack_floor_bytes, ESP32C3_STACK_FLOOR_BYTES);
}

/// ⚠️ **Rewritten by #335 STEP T, and the rewrite is the point.** This test used to assert
/// `floor > 73_728` — i.e. that the floor had risen off T13's stale value. That is an assertion
/// about *one instance* of staleness, in *one direction*, and it broke the moment a legitimate
/// measurement moved the peak DOWN (T relocated per-burst socket buffers off the stack, so the
/// peak fell 67,400 → 48,356 and the floor with it). A test that fails on a correct change is a
/// test that gets deleted, taking its guard with it.
///
/// **The invariant was never "the floor is large". It is "the floor is derived from the CURRENT
/// peak and is never inherited from an earlier one" — in either direction.** Asserted as such,
/// with the two real historical staleness values kept as named regression anchors.
#[test]
fn the_floor_is_derived_from_the_current_peak_never_inherited() {
    assert_eq!(
        ESP32C3_STACK_FLOOR_BYTES,
        (ESP32C3_MEASURED_PEAK_BYTES * 4).div_ceil(3),
        "the floor must be exactly ceil(peak × 4/3) for the peak recorded beside it"
    );
    // Both of these were real, and both were stale in the LOW direction while a higher peak was
    // already known — which is the failure mode that ships a too-thin image.
    assert_ne!(
        ESP32C3_STACK_FLOOR_BYTES, 73_728,
        "#348: 4/3 × T13's 54,856, kept while two higher peaks were measured"
    );
    assert_ne!(
        ESP32C3_STACK_FLOOR_BYTES, 74_208,
        "#440: 4/3 × 55,656, kept while the measured peak was 67,400"
    );
}

#[test]
fn c3_dram_headroom_is_the_measured_leftover() {
    // 106,560 B (the async-stack canonical tier, #233 spike) − 64,475 B (ceil(4/3 × 48,356),
    // the #335 STEP T peak). Both numbers are measurements; this is only their subtraction.
    // Derived rather than restated, so it cannot drift from the constants it is a function of —
    // the previous literal 32,352 was a third copy of the floor in disguise.
    assert_eq!(
        ESP32C3.dram_headroom(),
        ESP32C3.free_dram_bytes - ESP32C3_STACK_FLOOR_BYTES
    );
    assert_eq!(ESP32C3.dram_headroom(), 42_085);
}

#[test]
fn c3_flash_headroom_is_the_slot_minus_the_baseline() {
    // 0x1F0000 (2,031,616 B OTA slot) − 1,155,648 B (canonical image, re-measured 2026-08-01
    // through repro_build_bin on three trees; see the provenance note in budget.rs).
    assert_eq!(ESP32C3.flash_headroom(), 875_968);
    assert_eq!(ESP32C3.app_slot_bytes, 2_031_616);
}

/// 🔴 **#335 STEP T FLIPPED THIS, AND THE FLIP IS NOT A CLEARANCE TO SHIP THE BARD.**
///
/// #348's conclusion was "the C3 must refuse the Bard". Post-T the *model* says otherwise: T moved
/// the per-burst socket buffers off the stack, the measured peak fell 67,400 → 48,356, the floor
/// fell 74,208 → 64,475, and the DRAM headroom rose 32,352 → 42,085 — which is **3,013 B more than
/// the Bard's 39,072 B of statics.** So `fits_dram` is now `true`.
///
/// ⚠️ **This test asserts what the MODEL computes. It is not a statement that the Bard can ship.**
/// The peak feeding that model was measured on a **Bard-FREE** image (`stack-paint-lite,espnow,
/// cast,io`). Applying it to a composition that *includes* the Bard is exactly the T-SCOPE §3.2
/// error — a number taken on one composition used to license a different one. #434 is the standing
/// counter-example: the Bard-inclusive composition needed **≥51,008 B just to reach `main`**, and
/// its linked `.stack` was 45,992 B, so it panicked at esp-hal's init guard before any of this
/// arithmetic applied.
///
/// **What would actually settle it:** a `stack-paint` (Bard-inclusive) peak on hardware, plus the
/// packaging gate run on that image. Until then the flip is a model result and the tier exclusions
/// are what keep the Bard off the fleet — not this number.
#[test]
fn bard_dram_verdict_flipped_and_this_is_NOT_a_clearance() {
    // The model result, asserted as such.
    assert!(
        ESP32C3.fits_dram(&cost::BARD),
        "post-T headroom 42,085 exceeds the Bard's 39,072 B — see the doc comment before acting"
    );
    assert_eq!(ESP32C3.dram_shortfall(&cost::BARD), 0);
    // The margin is THIN, and thin against a peak measured without the thing being budgeted for.
    assert_eq!(ESP32C3.dram_headroom() - cost::BARD.dram_bytes, 3_013);
}

/// Reproduces #335's **pre-T** published number from declared data. Kept because the arithmetic is
/// the audit trail for how the verdict moved: 39,072 B of static DRAM against the *old* 32,352 B of
/// headroom gave the 6,720 B shortfall. The headroom is now 42,085 B and the shortfall is zero;
/// this test pins the historical subtraction so the flip is legible rather than silent.
#[test]
fn the_pre_t_bard_shortfall_is_reproducible_from_the_old_floor() {
    const OLD_FLOOR: u32 = 74_208; // 4/3 × the superseded 55,656 peak
    let old_headroom = ESP32C3.free_dram_bytes - OLD_FLOOR;
    assert_eq!(old_headroom, 32_352);
    assert_eq!(cost::BARD.dram_bytes - old_headroom, PUBLISHED_SHORTFALL_PRE_T);
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

// ── #347: the esp32c6-watch row ─────────────────────────────────────────────────────────
//
// Measured by the esp32c6-watch session at watch repo `a4a86a3`, 2026-08-24. These are the
// tests the compile-time assertions cannot be, for two reasons: the C6 row is not reachable
// from any `CHIP` selection yet (the cfg ladder still fails closed for riscv32+atomics until
// the de-pin lands), and a const panic message cannot carry a computed number.
//
// ⚠️ ORIENTATION OF THE PREDICATE. It is `chip.fits_dram(&cost)`, NOT `cost.fits_dram(chip)`.
// The handoff note wrote it the second way. It does not compile, so it could only ever have
// been a typo — but the same inversion in prose ("does the feature fit the chip" read as
// "does the chip fit the feature") is exactly how a budget gets applied backwards, so the
// call is spelled out here rather than left to the reader.

/// The DRAM cost of `story` was derived two independent ways from the same baseline, and they
/// agree to the byte: the statics grew by what the stack region lost. This test is that
/// cross-check, kept as arithmetic rather than as a table in a doc comment — if someone
/// re-measures and updates only one of the two, this fails instead of quietly disagreeing.
#[test]
fn the_c6_story_cost_reconciles_from_both_directions() {
    // .bss + .data: 291,772 − 286,380
    assert_eq!(291_772u32 - 286_380, cost::STORY.dram_bytes);
    // .stack region: 80,272 − 74,880 — the same 5,392 B, arrived at from the other side.
    assert_eq!(
        ESP32C6_WATCH.free_dram_bytes - 74_880,
        cost::STORY.dram_bytes
    );
    // .text + .rodata: 4,595,074 − 4,559,532
    assert_eq!(4_595_074u32 - 4_559_532, cost::STORY.flash_bytes);
    // The IMAGE delta is 35,744, which is NOT the flash-section delta — 202 B of header and
    // padding. Both are right for what they measure; pinning the gap stops a future
    // "correction" of one to the other.
    assert_eq!((4_704_528u32 - 4_668_784) - cost::STORY.flash_bytes, 202);
}

#[test]
fn the_c6_headroom_is_the_measured_leftover() {
    // 80,272 − 71,680.
    assert_eq!(ESP32C6_WATCH.dram_headroom(), 8_592);
    // 0x600000 (6,291,456 B, and only because the watch's widen_rom_region build.rs hook
    // rewrites esp-hal's hardcoded 4 MiB ROM region) − 4,668,784 B.
    assert_eq!(ESP32C6_WATCH.app_slot_bytes, 6_291_456);
    assert_eq!(ESP32C6_WATCH.flash_headroom(), 1_622_672);
}

/// The yes-case, on real declared data rather than a fixture: `story` fits the C6 on both
/// axes. Note this is the direction the const-assertions structurally cannot demonstrate.
#[test]
fn story_fits_the_c6_on_both_axes() {
    assert!(ESP32C6_WATCH.fits(&cost::STORY));
    assert!(ESP32C6_WATCH.fits_dram(&cost::STORY));
    assert!(ESP32C6_WATCH.fits_flash(&cost::STORY));
    assert_eq!(ESP32C6_WATCH.dram_shortfall(&cost::STORY), 0);
    assert_eq!(ESP32C6_WATCH.flash_shortfall(&cost::STORY), 0);
}

/// **The caveat that must not get lost.** The declared floor (71,680) is the watch's boot
/// assert; the hardware bracket walked 61,000 B = 5/5 panics and 73,000 B = 0/5. So
/// `dram_headroom()` reports 1,320 B more room than has been proven to boot, and a feature
/// landing within ~2 KB of fitting must be judged against the empirical line instead.
///
/// `story` clears BOTH, which is why it was safe to ship — and checking both is the point:
/// a verdict that only cleared the optimistic bound would look identical here and differ on
/// the next feature.
#[test]
fn the_c6_declared_floor_is_optimistic_by_a_known_1320_bytes() {
    assert_eq!(ESP32C6_WATCH_HEADROOM_OVERSTATEMENT_BYTES, 1_320);
    assert_eq!(
        ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES - ESP32C6_WATCH.stack_floor_bytes,
        ESP32C6_WATCH_HEADROOM_OVERSTATEMENT_BYTES
    );

    // Judged against the EMPIRICAL line rather than the declared floor.
    let empirical = ChipBudget {
        stack_floor_bytes: ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES,
        ..ESP32C6_WATCH
    };
    assert_eq!(empirical.dram_headroom(), 7_272);
    assert!(
        empirical.fits_dram(&cost::STORY),
        "story must clear the proven-clean line, not merely the declared floor"
    );

    // And the shipped story image's own stack region (74,880) sits above BOTH.
    assert!(74_880 > ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES);
    assert!(74_880 > ESP32C6_WATCH.stack_floor_bytes);
}

/// The C6 and the C3 are scarce on OPPOSITE axes, and the model has to be able to say so.
/// The C3 refuses the Bard on DRAM while sitting comfortable on flash; the C6 has 1.6 MB of
/// flash headroom and only 8.6 KB of DRAM. A single conflated "does it fit" number would
/// describe neither chip, which is why `ChipBudget` carries two fields (OpenWrt's `low_mem`
/// vs `small_flash`) instead of one.
#[test]
fn the_two_chips_are_scarce_on_opposite_axes() {
    // The INVARIANT — opposite scarcity — and it is unaffected by #335 STEP T.
    assert!(ESP32C6_WATCH.flash_headroom() > ESP32C3.flash_headroom());
    assert!(ESP32C6_WATCH.dram_headroom() < ESP32C3.dram_headroom());
    // Concretely: the C6 has ~1.85x the flash room and ~0.20x the DRAM room (was ~0.27x before
    // STEP T lowered the C3's floor 74,208 → 64,475, which widened the C3's DRAM headroom).
    //
    // ⚠️ The C3 figure is DERIVED, not restated. It used to be the literal 32,352, which is a
    // third copy of the floor in disguise — and it went stale the moment the floor moved, failing
    // here rather than where the decision was made. The C6's 8,592 stays a literal because it is
    // independent measured data for that row, not a function of anything in this file.
    assert_eq!(
        ESP32C3.dram_headroom(),
        ESP32C3.free_dram_bytes - ESP32C3_STACK_FLOOR_BYTES
    );
    assert_eq!(ESP32C6_WATCH.dram_headroom(), 8_592);
}

/// The Bard does not fit the C6 watch **as the watch is configured today** — and this is the
/// test most likely to be misread, so it says what it means. `budget.rs` and #347 both note
/// that "the S3 and C6 have the DRAM to carry the Bard"; that is a claim about the CHIP
/// (512 KB SRAM), not about this ROW. The row is the watch's shipping image, which has already
/// spent its DRAM on a TTS stack and a display — leaving 8,592 B, against the Bard's 39,072 B.
///
/// So a Bard-on-C6 build is a real possibility and this is not evidence against it. It is
/// evidence that it needs its OWN measured baseline, taken from a Bard-shaped C6 image, and
/// that reaching for this row would refuse it for the wrong reason.
#[test]
fn the_bard_does_not_fit_the_watch_row_and_that_is_a_statement_about_the_image() {
    assert!(!ESP32C6_WATCH.fits_dram(&cost::BARD));
    assert_eq!(ESP32C6_WATCH.dram_shortfall(&cost::BARD), 30_480);
    // The flash axis, by contrast, is comfortable — 287,392 B inside 1,622,672 B.
    assert!(ESP32C6_WATCH.fits_flash(&cost::BARD));
}

/// The predicate must be able to say YES. This fixture is a *hypothetical* chip — the C6 has
/// 512 KB of SRAM and would clear this easily, but nobody has measured its baseline, and an
/// unmeasured row does not belong in `budget.rs` (that is the #348 anti-lesson: a capability
/// that is guessed is worse than one that is absent). So the yes-case is proven here, where
/// the numbers are explicitly a fixture and cannot be mistaken for a declaration.
///
/// ⚠️ HISTORICAL NOTE (#347, 2026-08-24): the C6 now HAS a measured row — [`ESP32C6_WATCH`] —
/// and `story_fits_the_c6_on_both_axes` proves the yes-case on real data. This fixture stays
/// because the *reasoning* above is still the rule, and because the C6 row happens to refuse
/// the Bard (see the test above), so it cannot replace this one.
#[test]
fn a_chip_with_room_accepts_the_bard() {
    let roomy = ChipBudget {
        chip: "fixture-not-a-real-chip",
        free_dram_bytes: 200_000,
        stack_floor_bytes: 64_475,
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
        stack_floor_bytes: 64_475,
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
        stack_floor_bytes: 64_475,
        app_slot_bytes: 100_000,
        baseline_image_bytes: 1_155_600,
    };
    assert_eq!(broken.dram_headroom(), 0);
    assert_eq!(broken.flash_headroom(), 0);
    assert!(!broken.fits(&cost::BARD));
}
