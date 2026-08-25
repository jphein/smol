//! Host verification of the PURE #352 board-variant mapping. Includes the real
//! `net/profile.rs` and `net/target.rs` verbatim (`#[path]`, no drift). Run: `cargo run` —
//! panics on failure.
//!
//! # Why this file exists, and why the S3 case is the point of it
//!
//! Before #352 the Home Assistant `model` label was chosen by `#[cfg(target_feature = "a")]` —
//! the RISC-V atomics extension — with `not(...)` standing in for "this is a C3". That is not a
//! chip discriminant, it is a negation that holds only while exactly two chips exist. **Xtensa
//! has no `a` feature**, so an `xtensa-esp32s3` build takes the `not(...)` arm and a board
//! announces itself as `smol ESP32-C3 OLED`.
//!
//! No test could have caught that, and not for want of trying: the S3 does not build from this
//! tree (esp-hal is pinned to `esp32c3`), and a `cfg` cannot be exercised for a target you
//! cannot compile. The fix was not a better test, it was making the mapping a **pure function
//! of `(chip, has_display)`** — at which point the S3 case is an ordinary host assertion,
//! today, years before the silicon arrives. That is `s3_gets_its_own_label` below, and it is
//! the assertion this whole harness was built to make possible.
//!
//! Everything else here follows the `target_guard_verify` house rule: assertions that would
//! still pass on the broken code are worthless, so the cases that matter are the ones that
//! DISTINGUISH — every chip gets a label that no other chip gets, and the C3's two variants are
//! required to differ from each other.

#[path = "../../../rust/clock/src/net/target.rs"]
mod target;

#[path = "../../../rust/clock/src/net/profile.rs"]
mod profile;

use profile::BoardProfile;
use target::{CHIP_ESP32C3, CHIP_ESP32C5, CHIP_ESP32C6, CHIP_ESP32S3, CHIP_UNKNOWN};

/// Pull the `model` value out of the JSON fragment, so the assertions below talk about the
/// field a dashboard shows rather than about a blob of escaped punctuation.
fn model_of(p: BoardProfile) -> String {
    let s = p.ha_device_extras();
    let head = "\",\"manufacturer\"";
    let start = s.find("\"model\":\"").expect("fragment has no model field") + 9;
    let end = s.find(head).expect("fragment has no manufacturer field");
    s[start..end].to_string()
}

fn main() {
    let mut n = 0usize;
    let mut check = |name: &str, cond: bool| {
        assert!(cond, "FAILED: {name}");
        n += 1;
        println!("  ok  {name}");
    };

    // ── the C3's two boards, which is the case that already worked ───────────────────────
    let c3_oled = BoardProfile::new(CHIP_ESP32C3, true);
    let c3_bare = BoardProfile::new(CHIP_ESP32C3, false);
    check("C3 with a panel is OLED", model_of(c3_oled) == "smol ESP32-C3 OLED");
    check("C3 without a panel is SuperMini", model_of(c3_bare) == "smol ESP32-C3 SuperMini");
    // Not redundant with the two above: it is the property that makes `has_display` load-bearing
    // at all. A mapping that ignored the flag would satisfy neither, but a mapping that returned
    // the same string for both would satisfy a weaker pair of tests written less carefully.
    check("the C3's two variants are distinguishable",
          c3_oled.ha_device_extras() != c3_bare.ha_device_extras());

    // ── the C6 watch ─────────────────────────────────────────────────────────────────────
    let c6 = BoardProfile::new(CHIP_ESP32C6, true);
    check("C6 is the Watch", model_of(c6) == "smol ESP32-C6 Watch");
    check("C6 is single-variant (the screen is part of the product)",
          BoardProfile::new(CHIP_ESP32C6, false).ha_device_extras() == c6.ha_device_extras());

    // ── THE ONE THIS HARNESS EXISTS FOR ──────────────────────────────────────────────────
    // Under the old `cfg(target_feature = "a")` form an S3 build would have produced the C3's
    // label, silently, on hardware nobody could compile for yet. This asserts the mapping
    // answers correctly for a chip the tree still cannot build.
    let s3 = BoardProfile::new(CHIP_ESP32S3, true);
    check("S3 gets its OWN label", model_of(s3) == "smol ESP32-S3 Ember");
    check("S3 does NOT inherit the C3's label (the pre-#352 failure)",
          s3.ha_device_extras() != c3_oled.ha_device_extras()
              && s3.ha_device_extras() != c3_bare.ha_device_extras());

    // ── the C5 CYD (#388) ────────────────────────────────────────────────────────────────
    // Same class of assertion as the S3's: the tree cannot build for this chip yet, and the
    // C5 shares the C6's target triple, so the mapping answering correctly here is the host
    // proof that a C5 image will not announce itself as the Watch (the 6f900a6 collision).
    let c5 = BoardProfile::new(CHIP_ESP32C5, true);
    check("C5 gets its OWN label", model_of(c5) == "smol ESP32-C5 CYD");
    check("C5 does NOT inherit the C6's label (the shared-triple hazard)",
          c5.ha_device_extras() != c6.ha_device_extras());
    check("C5 is single-variant (the screen is part of the product)",
          BoardProfile::new(CHIP_ESP32C5, false).ha_device_extras() == c5.ha_device_extras());

    // Every chip distinct from every other, so a future arm cannot be added as a copy-paste
    // that quietly duplicates a neighbour's label.
    let all = [c3_oled, c3_bare, c6, s3, c5];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert!(a.ha_device_extras() != b.ha_device_extras(),
                    "two profiles share a label: {a:?} and {b:?}");
        }
    }
    check("all five fleet targets have pairwise-distinct labels", true);

    // ── shape, because the fragment is spliced into JSON by hand ─────────────────────────
    for p in all {
        let s = p.ha_device_extras();
        assert!(s.starts_with(",\"model\":\""), "fragment must open with the model field: {s}");
        assert!(s.ends_with("\",\"manufacturer\":\"jphein\""), "fragment tail is wrong: {s}");
        assert!(!s.contains('\n') && !s.contains('\t'), "fragment must be one line: {s}");
        // A quote inside the label would break the enclosing document. Two escaped quotes open
        // the model value, two close it, four wrap `manufacturer` and its value = 8 total.
        assert_eq!(s.matches('"').count(), 8, "unbalanced quoting in {s}");
    }
    check("every fragment is well-formed and splices safely into the device block", true);

    // ── the unknown-chip arm is the SHORTEST, so it can never set the budget maximum ─────
    // `SELF_EXTRAS_MAX` is what `DISCOVERY_CFG_MAX_UPLINK` is const-asserted against, and
    // `encode_publish` returns None SILENTLY when a config will not fit. A fallback that was
    // the longest string would let an unrecognised chip push a real board out of Home
    // Assistant with no error anywhere.
    let unknown = BoardProfile::new(CHIP_UNKNOWN, true).ha_device_extras();
    for p in all {
        assert!(unknown.len() <= p.ha_device_extras().len(),
                "the unknown-chip fallback ({}) is longer than a real label ({})",
                unknown.len(), p.ha_device_extras().len());
    }
    check("the unknown-chip fallback cannot become the budget maximum", true);

    // ── the number the discovery budget is derived from ──────────────────────────────────
    // Printed, not asserted against a literal: a hardcoded expectation here would be a third
    // statement of a fact that already has two, and #352 is about removing exactly that. The
    // BINDING check is the `const _: () = assert!(...)` in wifi.rs, which fails the BUILD.
    let c3_max = c3_oled.ha_device_extras().len().max(c3_bare.ha_device_extras().len());
    println!("\n  C3 extras max = {c3_max} B  (OLED {} · SuperMini {})",
             c3_oled.ha_device_extras().len(), c3_bare.ha_device_extras().len());
    println!("  C6 extras     = {} B", c6.ha_device_extras().len());
    println!("  S3 extras     = {} B", s3.ha_device_extras().len());
    println!("  C5 extras     = {} B", c5.ha_device_extras().len());

    println!("\nprofile_verify: {n} checks passed");
}
