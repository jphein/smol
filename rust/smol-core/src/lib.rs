//! # smol-core — the shared fleet layer (#347 Phase 2)
//!
//! Grown consumer-first: a module lands here when a SECOND consumer needs it,
//! adopted from whichever implementation is already measured and tested rather
//! than rewritten. The three intended consumers are `rust/clock` (the fleet
//! clock), the watch (`targets/c6-watch`), and the Bard firmware (Phase 3).
//!
//! ## `names` — intake item zero, executed
//!
//! The fleet naming layer IS the watch's `sigil-id`, re-exported. Not copied:
//! smol hand-copied `names.rs` twice and both copies drifted silently, and the
//! watch's crate exists specifically as the drift-proof form — pinned FANTASY
//! table (device identity: names + MQTT OTA topics derive from it), unpinned
//! FORGE table (build provenance), the MAC fold with the config-override
//! contract, and the dual-contract fleet tests that assert BOTH the derived ids
//! (122/236) and the allocated ones (176/162) so neither direction can drift.
//!
//! Consuming `smol_core::names::*` is therefore consuming the pinned tables
//! themselves. `rust/clock`'s adoption is a one-line dependency swap plus
//! deleting its local `names.rs` — deliberately left to the depin lane, which
//! owns that crate's manifest tonight.
//!
//! ## What lands next (in adoption order, each with its measured source)
//!
//! * `budget` — `rust/clock/src/budget.rs` is already written to move here
//!   without edits (its own header says so); the C6 row is measured and on
//!   #347's record.
//! * `wire` — the SMOLv1 frame format; the watch's `smol_mesh.rs` and clock's
//!   implementation must be reconciled BY TEST VECTORS, not by eye.
//! * `elect` — the watch's `crates/mesh-elect` (channel consensus), with the
//!   band-awareness caveat from the C5 port recorded before any dual-band
//!   consumer joins the vote.

#![no_std]
#![forbid(unsafe_code)]

/// Fleet naming: devices, builds, node ids. See the crate docs — this is the
/// watch's pinned `sigil-id`, adopted whole.
pub mod names {
    pub use sigil_id::*;
}
