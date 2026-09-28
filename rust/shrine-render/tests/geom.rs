//! Geometry invariants for the shrine panel.
//!
//! Lives in `tests/` rather than inline: `src/` stays `no_std` and test-free so it can be
//! vendored into smol byte-identical, exactly as `tapstone-rules` is.

use shrine_render::geom::*;
use tapstone_rules::state::LANES;

/// The bands the doc names must tile the panel exactly.
#[test]
fn vertical_bands_tile_240() {
    assert_eq!(HUD + WALL + CELL_BAND + WALL + HUD, H);
    assert_eq!(BETWEEN_HUD, 192);
    assert_eq!(CELL_BAND, 168);
}

/// Three 106 px lanes plus two 1 px separators fill 320 exactly.
#[test]
fn lane_columns_fill_320() {
    let total = LANES as i32 * LANE_CROSS_V + (LANES as i32 - 1) * LANE_SEP;
    assert_eq!(total, W);
    assert_eq!(lane_x(0), 0);
    assert_eq!(lane_x(2) + LANE_CROSS_V, W);
}

/// The engine tracks six cells per lane. This is the premise of the whole module.
#[test]
fn engine_lane_holds_six_cells() {
    assert_eq!(CELLS_PER_LANE, 6);
}

/// The doc's headline option cannot show the board, and that is the finding.
#[test]
fn doc_layout_cannot_show_the_whole_board() {
    let d = doc_three_row();
    assert!(!d.shows_whole_board());
}

/// Six honest rows break both floors the doc sets for itself.
#[test]
fn vertical_full_breaks_both_floors() {
    let v = vertical_full();
    assert_eq!(v.cell_adv, 28);
    assert!(
        !v.touchable(),
        "28 px row must fail the 44 px touch minimum"
    );
    assert!(!v.sprite_fits(), "28 px row cannot hold a 48 px sprite");
    assert!(v.shows_whole_board());
}

/// The asymmetric layout must spend the 168 px budget exactly, with nothing left over.
#[test]
fn asymmetric_tiles_the_band_exactly() {
    let a = vertical_asymmetric();
    assert_eq!(a.cell_adv * 3 + a.far_adv * 3, CELL_BAND);
    assert_eq!(slot_offset(&a, a.per_seat * 2), CELL_BAND);
    assert!(a.shows_whole_board());
    assert!(
        a.sprite_base_ok(),
        "36 px rows must hold decision 0014's 32 px base sprite"
    );
}

/// Every layout's drawn slots must fit the band they are given.
#[test]
fn every_layout_fits_its_band() {
    for l in all() {
        let span = advance_span(&l);
        let budget = match l.orientation {
            Orientation::Vertical => CELL_BAND,
            Orientation::Horizontal => W - 2 * WALL,
        };
        assert!(span <= budget, "{} spans {span} px of {budget} px", l.name);
    }
}

/// Only the asymmetric layout clears the 32 px base sprite while showing the whole board.
#[test]
fn asymmetric_is_the_only_full_board_layout_over_the_base_sprite() {
    let winners: Vec<_> = all()
        .into_iter()
        .filter(|l| l.shows_whole_board() && l.sprite_base_ok())
        .map(|l| l.name)
        .collect();
    assert!(winners.contains(&"v6-asym"));
    assert!(
        !winners.contains(&"v6-full"),
        "28 px rows cannot hold a 32 px sprite"
    );
}

/// The horizontal layout is the one that satisfies every stated constraint.
#[test]
fn horizontal_full_satisfies_every_constraint() {
    let h = horizontal_full();
    assert_eq!(h.cell_adv, 49);
    assert_eq!(h.cell_cross, 64);
    assert!(h.touchable(), "49x64 clears the 44 px minimum on both axes");
    assert!(h.sprite_fits(), "48 px sprite fits in a 49x64 cell");
    assert!(h.shows_whole_board());
}
