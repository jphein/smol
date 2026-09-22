//! smol's own budget assertions for the vendored engine. Not an upstream test — see
//! `tests/vendor_golden_replay.rs`'s header for the smol-own / vendored split.
//!
//! ── WHY THESE ARE TESTS AND NOT NUMBERS IN A REPORT ──────────────────────────────────────────
//! smol issue 10's fourth criterion is "`Game` stays **≤ 350 B**". Note the verb: *stays*. That is
//! a standing guarantee, and a standing guarantee measured once by hand and written into a PR
//! description is the exact shape this repo keeps finding and killing — #348's "8/8 host tests"
//! reported from a manual run, the OTA suite nothing invoked for three months, the byte-free
//! claims that were prose until #351. Upstream asserts none of these sizes (checked: `grep -rn
//! size_of` over tapstone-rules finds nothing), so if smol does not, nobody does, and the number
//! is free to drift by 40 bytes in a commit that looks like a rules tweak.
//!
//! They are cheap, they are exact, and they turn four sentences of a GitHub issue into something
//! `tools/gate.sh` can fail on.

use tapstone_rules::hash::canonical_len;
use tapstone_rules::state::{DECK_MAX, HAND_MAX, LANES, SEATS};
use tapstone_rules::{Game, Record};

/// Issue 10: `Game` ≤ 350 B.
///
/// `Game` is what the shrine app owns and what `crate::app::App`'s tagged union is sized against,
/// so this is a RAM budget, not a curiosity: the union is as large as its biggest screen and every
/// screen pays for the largest. The bound is an inequality on purpose — shrinking it is fine and
/// should not fail a test — but if it is ever exceeded, the fix is upstream in tapstone and then a
/// re-vendor, never a bump of the number here.
#[test]
fn game_fits_its_ram_budget() {
    let n = core::mem::size_of::<Game>();
    assert!(
        n <= 350,
        "size_of::<Game>() is {n} B, over issue 10's 350 B budget. This is the state every \
         shrine holds and the `App` union is sized to, so it is a firmware-wide RAM cost. Fix it \
         in tapstone and re-vendor; do not raise this bound without saying what the extra bytes \
         buy and re-checking `App`'s largest variant (MeshSnake, ~0.5 KB)."
    );
}

/// Issue 10 / protocol draft §3: a record is 24 bytes on the wire.
///
/// `Record::LEN`, not `size_of::<Record>()`: the in-memory struct carries padding (21 B of fields
/// at align 4) and its size is nobody's contract. The WIRE size is the contract — it is what
/// `encode`/`decode` move, what the MATCH frame's 221 B payload budget is divided by (issue 1),
/// and what the chain hashes. Asserting the wrong one of those two would look like a check and
/// measure a number no protocol depends on.
#[test]
fn a_record_is_twenty_four_bytes_on_the_wire() {
    assert_eq!(Record::LEN, 24);
    let r = Record::decode(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect("a ClaimSeat record decodes");
    assert_eq!(r.encode().len(), 24);
}

/// The canonical state image is 134 B, and it is 134 B *for a derivable reason*.
///
/// This one is not a size check, it is a **coupling** check. The chain hashes this image, so its
/// length and layout are the protocol: two shrines that disagree about it compute different heads
/// from identical taps. `hash.rs` derives `CANON` from `SEATS`, `LANES`, `CELLS` and `HAND_MAX`,
/// so re-asserting the arithmetic here means a future change to any of those constants cannot
/// quietly move the image — it fails with the two numbers side by side. A bare
/// `assert_eq!(canonical_len(), 134)` would catch the same drift while explaining none of it.
#[test]
fn the_canonical_state_image_is_134_bytes_by_construction() {
    const CELLS: usize = 3;
    let derived = 6 + SEATS * (2 + 5 + LANES * CELLS * 4 + 1 + HAND_MAX * 2);
    assert_eq!(
        canonical_len(),
        derived,
        "canonical_len() no longer matches the arithmetic in hash.rs's CANON comment"
    );
    assert_eq!(
        canonical_len(),
        134,
        "the canonical state image changed size. This is a PROTOCOL BREAK, not a refactor: every \
         recorded match hashes differently, and a shrine on the old image and one on the new \
         compute different chain heads from the same taps without either noticing. It needs a \
         version bump in `Chain::genesis`'s domain tag (b\"tapstone:v0\"), decided in tapstone."
    );
}

/// The fixed-capacity arrays the no-alloc design rests on, pinned so a "small" bump to one of them
/// cannot silently multiply through `Game` and past the budget above. `DECK_MAX` at 30 is also
/// tapstone's ≤30-card deck rule (CLAUDE.md "keep the game small") expressed in a type.
#[test]
fn the_fixed_capacities_are_what_the_budget_assumes() {
    assert_eq!(SEATS, 2, "a Duel is two seats");
    assert_eq!(LANES, 3, "three lanes (decision 0010's battlefield)");
    assert_eq!(DECK_MAX, 30, "the >=30-card deck cap");
    assert_eq!(HAND_MAX, 10);
}
