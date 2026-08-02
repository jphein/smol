//! Consensus-layer host guard, REDUCED for #269.
//!
//! The donor's 31 consensus tests exercised its `Elector` (the channel VOTE). That machinery was
//! removed deliberately — under #269 the mesh channel is DERIVED from the elected gateway's AP, so
//! there is nothing to vote on. These two survive because they test the surface smol KEPT:
//! `weight()` (still fills the frame's `w[13]` honestly for the watch) and `Decision::supersedes`
//! (epoch ordering, which is how a leaf rejects a stale or replayed announcement).
//!
//! The other 29 are not lost — they live upstream in `esp32c6-watch:crates/mesh-elect`, which
//! still owns the election. If smol ever needs a vote again, re-port from there rather than
//! reconstructing them here.

use crate::mesh_elect::*;

pub fn weight_saturates_and_floors() {
    assert_eq!(weight(-30), weight(WEIGHT_CEIL_DBM), "clamped at the top");
    assert_eq!(weight(-35), weight(-30));
    assert_eq!(weight(-83), 0, "below the usable floor is not a vote");
    assert_eq!(weight(USABLE_MIN_DBM), 1, "bare usable visibility still counts");
    assert!(weight(-50) > weight(-70), "monotone in between");
}

/// A channel the fleet can only barely hear must not win on headcount, or the
/// election would happily march everyone onto a channel nobody can associate on.

pub fn partition_merge_is_total_order() {
    let a = Decision { channel: 6, epoch: 5, gateway: 0 };
    let b = Decision { channel: 1, epoch: 4, gateway: 0 };
    assert!(a.supersedes(2, &b, 9), "higher epoch beats bigger partition");

    let c = Decision { channel: 11, epoch: 5, gateway: 0 };
    assert!(c.supersedes(6, &a, 3), "equal epoch → more members wins");
    assert!(!a.supersedes(3, &c, 6));

    let d = Decision { channel: 1, epoch: 5, gateway: 0 };
    assert!(d.supersedes(3, &a, 3), "equal epoch+members → lower channel wins");
    assert!(!a.supersedes(3, &d, 3), "and the order is antisymmetric");
}

// ===========================================================================
// Hysteresis: converge, then STOP
// ===========================================================================
