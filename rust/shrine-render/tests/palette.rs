//! Palette invariants.
//!
//! Lives in `tests/` so `src/` stays `no_std` and test-free for vendoring.

use embedded_graphics::prelude::RgbColor;
use shrine_render::palette::*;

/// The doc's hexes must survive the 565 quantisation as recognisably the same hue.
#[test]
fn faction_hues_stay_distinct() {
    let all = [EMBER, TIDE, GROVE, GRAVE, NEUTRAL];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(
                (a.r(), a.g(), a.b()),
                (b.r(), b.g(), b.b()),
                "two faction colours collapsed to the same Rgb565"
            );
        }
    }
}

#[test]
fn health_ramp_walks_green_to_red() {
    assert_eq!(health(10, 10), HEALTH_OK);
    assert_eq!(health(5, 10), HEALTH_MID);
    assert_eq!(health(1, 10), HEALTH_LOW);
    // A dead or unset castle must not panic.
    assert_eq!(health(0, 0), HEALTH_OK);
}
