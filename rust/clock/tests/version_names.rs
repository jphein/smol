//! #218/#393 host tests for the FORGE version-name mapping (`net::names::version_name_for`).
//!
//! ## Why this file exists
//!
//! The sigil word is the release's public identity — it appears on the OLED, in the MQTT status
//! line, and in every OTA announce. Until now the mapping had **four documented controls
//! (341, 342, 345, 905) that lived only in prose**, in two docs, checked by whoever happened to
//! re-derive them by hand. #393 was a sigil-drift investigation; this is the same corpus with no
//! machine check on the arithmetic that names every build.
//!
//! A control that is a sentence is a claim. A control that is an assertion is a check. These are
//! the same five numbers, moved across that line.
//!
//! ## The fifth control, and what it cost to learn
//!
//! `v1446 Molten Gear` was added when the first versioned release was cut. The release had been
//! called "v346 Riveted Gear" in `RELEASES.md` prose since the fleet was at build 345 — but the
//! sanctioned release path (`tools/ota_publish.sh stage`) does not read `version.txt`. It computes
//! `choose_build() = max(commit-count, retained-staged + 1)`, passes it to `repro_build_bin`, which
//! exports `SMOL_BUILD_NUMBER`, which **wins** over `version.txt` in `build.rs`'s `env_or_file`.
//! The shipped number was 1446, so the shipped name was `Molten Gear`.
//!
//! ⚠️ **THE NEAR-MISS THAT MAKES THIS WORTH A TEST.** `346 % 20 == 1446 % 20 == 6`, so **both map
//! to the noun `Gear`** — only the adjective differs (`Riveted` vs `Molten`). The existing guard
//! ("always write the number and the word together, that pair self-checks") **could not catch it**:
//! `346 / Riveted Gear` self-checks perfectly. The pair was internally consistent and externally
//! wrong, because 346 was never the number being shipped. A half-matching name is far more
//! convincing than a wholly wrong one.
//!
//! **The corrected guard is therefore two steps, not one:** verify the NUMBER against
//! `choose_build`'s output first, *then* derive the word from it.

#![cfg(feature = "hostsim")]

use clock::net::names::version_name_for;

/// All five controls, exercising the real function rather than a re-derivation of the formula.
///
/// The first four are the historical ones that had lived in prose; the fifth is the first
/// versioned release. Kept as `(number, adjective, noun)` triples so a corpus edit — a reordered
/// or renamed word — fails here loudly, which is the drift #393 went looking for by hand.
#[test]
fn the_documented_version_name_controls_all_reproduce() {
    const CONTROLS: &[(u32, &str, &str)] = &[
        (341, "Riveted", "Bellows"),
        (342, "Riveted", "Crucible"),
        (345, "Riveted", "Furnace"),
        (905, "Flux", "Furnace"),
        // #335: the first versioned release. See the module note for why it is not 346.
        (1446, "Molten", "Gear"),
    ];
    for &(n, adj, noun) in CONTROLS {
        assert_eq!(
            version_name_for(n),
            (adj, noun),
            "version_name_for({n}) drifted — the FORGE corpus or the index arithmetic moved"
        );
    }
}

/// The near-miss, asserted directly so it cannot be re-learned the expensive way.
///
/// These two builds share a noun and differ only in adjective. Anyone checking a release name by
/// eye — or by the "number and word together" pair rule — can accept the wrong one. The test states
/// the collision so the next reader meets it here instead of on a dashboard.
#[test]
fn the_346_vs_1446_near_miss_shares_a_noun_and_that_is_the_trap() {
    let (adj_346, noun_346) = version_name_for(346);
    let (adj_1446, noun_1446) = version_name_for(1446);
    assert_eq!(noun_346, noun_1446, "both are ≡ 6 mod 20, so both are `Gear`");
    assert_ne!(
        adj_346, adj_1446,
        "and ONLY the adjective distinguishes them — which is why the pair rule could not catch it"
    );
    assert_eq!((adj_346, noun_346), ("Riveted", "Gear"));
    assert_eq!((adj_1446, noun_1446), ("Molten", "Gear"));
}

/// The adjective is a slow generation marker: it advances once per full noun cycle. Asserted
/// because the doc comment on `version_name_for` claims it, and a claim about arithmetic is
/// cheap to check and expensive to leave unchecked.
#[test]
fn consecutive_builds_differ_and_the_adjective_advances_once_per_noun_cycle() {
    // Consecutive builds get distinct words — the property that motivated direct modulo over
    // sigil's `>>8` seeding (256..=511 all collapse to one noun under the shift).
    for n in 340u32..=360 {
        assert_ne!(
            version_name_for(n),
            version_name_for(n + 1),
            "builds {n} and {} share a name",
            n + 1
        );
    }
    // The adjective is stable across a noun cycle and advances at the boundary.
    assert_eq!(version_name_for(340).0, version_name_for(359).0);
    assert_ne!(version_name_for(359).0, version_name_for(360).0);
}
