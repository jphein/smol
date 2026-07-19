//! Host verification of the PURE #183 L2 core: the crown-ordered tree-head — a CT-style
//! (RFC 6962 lineage) Merkle **anchor** over the per-node #182 chain heads. Includes the real
//! `net/treehead.rs` verbatim (#[path], no drift). Run: `cargo run` — panics on failure.
//!
//! The load-bearing invariant is **order-independence**: every node must compute the *identical*
//! anchor from the same set of heads regardless of the order it learned them — that's the
//! "ordered" in crown-ordered, and what lets a node compare its anchor to the crown's in O(1).
//! Inclusion proofs let a node prove its head is under the anchor in O(log n) hashes instead of
//! shipping every head. sha256 is injected (the core is dependency-free).

#[path = "../../../rust/clock/src/net/treehead.rs"]
mod treehead;

use sha2::{Digest, Sha256};
use treehead::{verify_inclusion, HeadSet, EMPTY_ROOT, HASH_LEN};

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// A stand-in per-node chain head (a #182 tip) — arbitrary 16 bytes.
fn head(seed: u8) -> [u8; 16] {
    let mut x = [0u8; 16];
    x[0] = seed;
    x[15] = seed ^ 0xa5;
    x
}

fn main() {
    // --- empty set → EMPTY_ROOT --------------------------------------------
    let hs: HeadSet<16> = HeadSet::new();
    assert_eq!(
        hs.root(sha256),
        EMPTY_ROOT,
        "empty head-set anchors to EMPTY_ROOT"
    );
    assert!(hs.is_empty());
    assert_eq!(hs.len(), 0);

    // --- single head → non-empty deterministic root ------------------------
    let mut a: HeadSet<16> = HeadSet::new();
    a.upsert(7, head(0x77), 3).unwrap();
    assert_eq!(a.len(), 1);
    assert_ne!(a.root(sha256), EMPTY_ROOT);

    // --- ORDER-INDEPENDENCE: same heads, different insert order → same root -
    let mut x: HeadSet<16> = HeadSet::new();
    x.upsert(7, head(7), 1).unwrap();
    x.upsert(8, head(8), 1).unwrap();
    x.upsert(9, head(9), 1).unwrap();
    let mut y: HeadSet<16> = HeadSet::new();
    y.upsert(9, head(9), 1).unwrap();
    y.upsert(7, head(7), 1).unwrap();
    y.upsert(8, head(8), 1).unwrap();
    assert_eq!(
        x.root(sha256),
        y.root(sha256),
        "canonical order ⇒ identical anchor regardless of insert order"
    );

    // --- divergence: one changed head ⇒ a different anchor ------------------
    let mut z: HeadSet<16> = HeadSet::new();
    z.upsert(7, head(7), 1).unwrap();
    z.upsert(8, head(0x88), 1).unwrap(); // different head for node 8
    z.upsert(9, head(9), 1).unwrap();
    assert_ne!(
        z.root(sha256),
        x.root(sha256),
        "a changed head ⇒ a different anchor"
    );

    // --- upsert = insert-or-update (same node twice updates, not adds) ------
    let mut u: HeadSet<16> = HeadSet::new();
    u.upsert(5, head(1), 1).unwrap();
    u.upsert(5, head(2), 2).unwrap();
    assert_eq!(u.len(), 1, "re-upserting a node updates it, does not add");

    // --- inclusion proofs round-trip for every node, across odd counts -----
    for n in [1usize, 2, 3, 5] {
        let mut s: HeadSet<16> = HeadSet::new();
        for i in 0..n {
            s.upsert(i as u8, head(i as u8 + 1), i as u32 + 1).unwrap();
        }
        let root = s.root(sha256);
        for i in 0..n {
            let p = s
                .inclusion_proof(i as u8, sha256)
                .expect("proof for a present node");
            assert!(
                verify_inclusion(root, i as u8, head(i as u8 + 1), i as u32 + 1, &p, sha256),
                "node {i} of {n} proves inclusion under the anchor"
            );
        }
    }

    // --- tampered leaf (wrong head) ⇒ verify fails -------------------------
    {
        let mut s: HeadSet<16> = HeadSet::new();
        for i in 0..5 {
            s.upsert(i, head(i + 1), 1).unwrap();
        }
        let root = s.root(sha256);
        let p = s.inclusion_proof(2, sha256).unwrap();
        assert!(
            verify_inclusion(root, 2, head(3), 1, &p, sha256),
            "honest proof verifies"
        );
        assert!(
            !verify_inclusion(root, 2, head(0xff), 1, &p, sha256),
            "a wrong head ⇒ inclusion verify fails"
        );
    }

    // --- tampered proof (flipped sibling) ⇒ verify fails -------------------
    {
        let mut s: HeadSet<16> = HeadSet::new();
        for i in 0..5 {
            s.upsert(i, head(i + 1), 1).unwrap();
        }
        let root = s.root(sha256);
        let mut p = s.inclusion_proof(2, sha256).unwrap();
        if p.len > 0 {
            p.siblings[0][0] ^= 0xff;
            assert!(
                !verify_inclusion(root, 2, head(3), 1, &p, sha256),
                "a tampered proof sibling ⇒ verify fails"
            );
        }
    }

    // --- bounded capacity: upsert past N ⇒ Full; update-when-full ok -------
    {
        let mut s: HeadSet<2> = HeadSet::new();
        s.upsert(1, head(1), 1).unwrap();
        s.upsert(2, head(2), 1).unwrap();
        assert!(s.upsert(3, head(3), 1).is_err(), "a new node past N ⇒ Full");
        assert_eq!(s.len(), 2);
        assert!(
            s.upsert(1, head(9), 2).is_ok(),
            "updating an existing node when full is ok"
        );
    }

    // --- absent node ⇒ no inclusion proof ----------------------------------
    {
        let mut s: HeadSet<16> = HeadSet::new();
        s.upsert(1, head(1), 1).unwrap();
        assert!(
            s.inclusion_proof(42, sha256).is_none(),
            "absent node has no proof"
        );
    }

    // --- const-constructible (a crown holds one in .bss) -------------------
    const _C: HeadSet<8> = HeadSet::new();
    assert_eq!(HASH_LEN, 16);

    println!("treehead_verify: all assertions passed");
}
