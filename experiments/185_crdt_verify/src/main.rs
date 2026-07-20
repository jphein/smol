//! Host verification of the PURE #185 mesh-ledger L4 core: the delta-state **OR-Set / G-Set**
//! CRDT for multi-writer shared RPG state (loot pickup, gravestones). Includes the real
//! `net/crdt.rs` verbatim (#[path], no drift). Run: `cargo run` — panics on failure.
//!
//! What L4 adds to the ledger substrate (L1 #182 chain · L2 #183 anchor · L3 #184 STH): those
//! give a *single-writer* tamper-evident history; RPG loot/gravestones are **multi-writer shared
//! state** edited concurrently over a lossy, order-scrambling, duplicating mesh with no
//! coordinator. A CRDT converges regardless: its merge is a **join-semilattice** — commutative,
//! idempotent, associative — so message order/loss/dup can't diverge two replicas. *Delta*-state
//! means a node gossips only the small change-set (fits the 250 B ESP-NOW MTU), and the join of a
//! delta is the same op as the join of full state. sha256 is INJECTED (the core is dep-free); here
//! it powers an order-independent live-set **digest** — the O(1) "are we converged?" check that
//! also feeds the L2/L3 anchor/signature.

#[path = "../../../rust/clock/src/net/crdt.rs"]
mod crdt;

use crdt::{ElemId, OrSet};
use sha2::{Digest, Sha256};

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// A deterministic element id (loot/gravestone content hashed to 16 B in real use).
fn eid(n: u8) -> ElemId {
    let mut x = [0u8; 16];
    x[0] = n;
    x[15] = n ^ 0x5a;
    x
}

const CAP: usize = 32;
type Set = OrSet<CAP>;

fn dg(s: &Set) -> [u8; 16] {
    s.digest(sha256)
}

fn main() {
    // ---- empty ------------------------------------------------------------
    let e0 = Set::new();
    assert!(e0.is_empty(), "fresh set is empty");
    assert_eq!(e0.len(), 0, "fresh set has len 0");
    assert!(!e0.contains(&eid(1)), "empty contains nothing");
    let e0b = Set::new();
    assert_eq!(dg(&e0), dg(&e0b), "two empty sets share a digest (deterministic)");

    // ---- add / contains / remove / re-add (add-wins re-add) --------------
    let mut s = Set::new();
    let t1 = s.add(eid(1), 1, 1).expect("add ok");
    assert!(s.contains(&eid(1)) && s.len() == 1 && !s.is_empty(), "added elem is present");
    assert_eq!(s.remove(&eid(1)), 1, "remove tombstones the 1 live tag");
    assert!(!s.contains(&eid(1)), "removed elem gone");
    assert_eq!(s.len(), 0, "removed elem drops the count");
    let t2 = s.add(eid(1), 1, 2).expect("re-add ok");
    assert!(s.contains(&eid(1)), "re-add with a fresh tag revives the elem (its tag isn't tombed)");
    assert_ne!(t1.seq, t2.seq, "each add mints a distinct tag");
    assert_eq!(s.remove(&eid(99)), 0, "removing an absent elem tombstones nothing");

    // ---- idempotent add of the exact same (elem,tag) ---------------------
    let mut d = Set::new();
    d.add(eid(7), 3, 10).unwrap();
    let before = d.len();
    // merging a set that already equals a subset must not double-count (same tags).
    let mut d2 = Set::new();
    d2.add(eid(7), 3, 10).unwrap();
    d.merge(&d2).unwrap();
    assert_eq!(d.len(), before, "merging an identical tag is idempotent (no duplication)");

    // ---- COMMUTATIVITY: merge(a,b) ≡ merge(b,a) --------------------------
    let mut a = Set::new();
    a.add(eid(1), 1, 1).unwrap();
    a.add(eid(2), 1, 2).unwrap();
    a.remove(&eid(2)); // a: {1}
    let mut b = Set::new();
    b.add(eid(3), 2, 1).unwrap();
    b.add(eid(2), 2, 2).unwrap(); // concurrent re-add of 2 by node 2 (add-wins vs a's remove of node1's tag)
    let mut ab = a.clone();
    ab.merge(&b).unwrap();
    let mut ba = b.clone();
    ba.merge(&a).unwrap();
    assert_eq!(dg(&ab), dg(&ba), "merge is commutative (same digest either order)");
    assert!(ab.contains(&eid(1)) && ab.contains(&eid(2)) && ab.contains(&eid(3)),
        "add-wins: 2 survives because node2's fresh tag was never tombed");

    // ---- IDEMPOTENCE: merge(x,x) ≡ x  and  re-merging a delta is a no-op -
    let mut x = ab.clone();
    let x_dg = dg(&x);
    x.merge(&ab).unwrap();
    assert_eq!(dg(&x), x_dg, "merge(x,x) leaves the state unchanged");
    x.merge(&b).unwrap();
    assert_eq!(dg(&x), x_dg, "re-merging an already-absorbed delta changes nothing");

    // ---- DELTA-MERGE ≡ STATE-MERGE ---------------------------------------
    let mut base = Set::new();
    base.add(eid(5), 4, 1).unwrap();
    let mut adv = base.clone();
    adv.add(eid(6), 4, 2).unwrap();
    adv.add(eid(7), 4, 3).unwrap();
    adv.remove(&eid(5));
    let delta = adv.delta_vs(&base); // only what `base` is missing
    let mut via_delta = base.clone();
    via_delta.merge(&delta).unwrap();
    let mut via_full = base.clone();
    via_full.merge(&adv).unwrap();
    assert_eq!(dg(&via_delta), dg(&via_full), "merging the delta ≡ merging full state (digest)");
    for n in [5u8, 6, 7] {
        assert_eq!(via_delta.contains(&eid(n)), via_full.contains(&eid(n)),
            "delta vs full agree on elem {n}");
    }
    assert!(delta.len() <= adv.len(), "a delta is no larger than the full state");

    // ---- CONVERGENCE: 3 replicas, local ops, arbitrary-order gossip ------
    let mut r1 = Set::new();
    let mut r2 = Set::new();
    let mut r3 = Set::new();
    r1.add(eid(10), 1, 1).unwrap();
    r1.add(eid(11), 1, 2).unwrap();
    r2.add(eid(20), 2, 1).unwrap();
    r2.add(eid(10), 2, 2).unwrap(); // 10 concurrently added by two nodes
    r3.add(eid(30), 3, 1).unwrap();
    r1.remove(&eid(11)); // r1 kills its own 11
    // gossip in a deliberately ugly order, with duplicates (mesh reality):
    let d1 = r1.clone();
    let d2 = r2.clone();
    let d3 = r3.clone();
    r2.merge(&d1).unwrap();
    r3.merge(&d2).unwrap();
    r1.merge(&d3).unwrap();
    r2.merge(&d3).unwrap();
    r1.merge(&r2.clone()).unwrap();
    r3.merge(&r1.clone()).unwrap();
    r2.merge(&r3.clone()).unwrap();
    r1.merge(&r2.clone()).unwrap(); // extra round + duplicate deliveries
    r3.merge(&r2.clone()).unwrap();
    assert_eq!(dg(&r1), dg(&r2), "r1 and r2 converge");
    assert_eq!(dg(&r2), dg(&r3), "r2 and r3 converge");
    for n in [10u8, 20, 30] {
        assert!(r1.contains(&eid(n)) && r2.contains(&eid(n)) && r3.contains(&eid(n)),
            "all replicas agree elem {n} is live");
    }
    assert!(!r1.contains(&eid(11)) && !r3.contains(&eid(11)), "the removed 11 stays gone everywhere");

    // ---- ADD-WINS (canonical concurrent add vs remove) -------------------
    // rA adds e; rB independently adds e; rB removes e (tombs only rB's own tag).
    let mut ra = Set::new();
    let ta = ra.add(eid(50), 1, 1).unwrap();
    let mut rb = Set::new();
    rb.add(eid(50), 2, 1).unwrap();
    rb.remove(&eid(50)); // tombs node2's tag only — never saw node1's tag ta
    let mut merged = ra.clone();
    merged.merge(&rb).unwrap();
    assert!(merged.contains(&eid(50)), "add-wins: rA's un-observed tag keeps elem 50 alive");
    let _ = ta;

    // ---- GROW-ONLY (G-Set) mode: never remove ⇒ pure union ---------------
    let mut g1 = Set::new();
    let mut g2 = Set::new();
    g1.add(eid(60), 1, 1).unwrap();
    g2.add(eid(61), 2, 1).unwrap();
    g1.merge(&g2).unwrap();
    g2.merge(&g1.clone()).unwrap();
    assert_eq!(dg(&g1), dg(&g2), "G-Set (no removes) converges");
    assert!(g1.contains(&eid(60)) && g1.contains(&eid(61)), "grow-only union has both");

    // ---- DIGEST order-independence ---------------------------------------
    let mut o1 = Set::new();
    o1.add(eid(1), 1, 1).unwrap();
    o1.add(eid(2), 1, 2).unwrap();
    o1.add(eid(3), 1, 3).unwrap();
    let mut o2 = Set::new();
    o2.add(eid(3), 1, 3).unwrap();
    o2.add(eid(1), 1, 1).unwrap();
    o2.add(eid(2), 1, 2).unwrap();
    assert_eq!(dg(&o1), dg(&o2), "live-set digest is independent of insertion order");
    // and the digest tracks the LIVE set, not the tag history:
    let mut o3 = o1.clone();
    o3.remove(&eid(2));
    assert_ne!(dg(&o1), dg(&o3), "removing a live elem changes the digest");

    // ---- BOUNDED capacity ------------------------------------------------
    let mut full: OrSet<4> = OrSet::new();
    for i in 0..4u8 {
        full.add(eid(i), 1, i as u32 + 1).unwrap();
    }
    assert!(full.add(eid(200), 1, 99).is_err(), "adding past CAP ⇒ Err (bounded)");

    // ---- const-constructible (a node holds one in .bss) ------------------
    const _C: OrSet<8> = OrSet::new();

    println!("crdt_verify: all assertions passed");
}
