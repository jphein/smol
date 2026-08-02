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
    println!("mesh_elect_verify: 10 checks passed (2 consensus + 8 wire)");
}
