//! Fleet channel-election host guard (#278). `#[path]`-includes the REAL `net/mesh_elect.rs` (no
//! drift) and runs the esp32c6-watch donor's own suite VERBATIM — the `#[test]` attributes are
//! stripped and every test fn is called from `main`, matching this repo's panic-on-failure
//! convention. Helper fns in those files are NOT called directly (they take arguments); only the
//! fns that carried `#[test]` upstream are entry points.
//!
//! These are the CROSS-REPO CONTRACT: both repos must compute the same winner from the same
//! observations and encode the same bytes, or the fleet partitions. `cargo run`.

#[path = "../../../rust/clock/src/net/mesh_elect.rs"]
mod mesh_elect;

mod consensus;
mod wire_tests;

// #278 stage 2, smol-only: the announce schedule / follow state / recovery ladder / send-path seal.
// Kept OUT of `consensus.rs` + `wire_tests.rs` so those two stay verbatim-diffable against the
// donor's own suite, the same split the source file itself carries.
mod follow_tests;

fn main() {
    consensus::weight_saturates_and_floors();
    consensus::partition_merge_is_total_order();
    wire_tests::encoding_is_byte_exact();
    wire_tests::round_trips();
    wire_tests::round_trips_at_field_extremes();
    wire_tests::tag_does_not_collide_with_existing_frames();
    wire_tests::rejects_malformed_frames();
    wire_tests::rejects_out_of_range_channel();
    wire_tests::encode_refuses_a_short_buffer();
    wire_tests::frame_is_human_readable_in_a_log();

    follow_tests::announces_before_and_after_the_move();
    follow_tests::a_redundant_decision_does_not_burn_an_epoch();
    follow_tests::refuses_an_out_of_range_channel();
    follow_tests::orders_by_epoch_and_ignores_replay();
    follow_tests::counts_would_be_moves_separately_from_hearings();
    follow_tests::an_observed_channel_must_settle_before_it_costs_an_epoch();
    follow_tests::beacons_between_migrations();
    follow_tests::probation_expires_on_a_dead_epoch();
    follow_tests::the_ladder_is_the_legacy_plan_while_following_is_off();
    follow_tests::the_ladder_ranks_and_dedupes();
    follow_tests::the_ladder_reaches_the_whole_band();
    follow_tests::sealing_preserves_the_cross_repo_bytes();
    follow_tests::sealing_refuses_a_frame_that_would_strand_a_leaf();

    println!("mesh_elect_verify: 23 checks passed (2 consensus + 8 wire + 13 follow/announce)");
}
