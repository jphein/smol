//! Host verification of the PURE #182 mesh-ledger L1 core. Includes the real
//! `net/ledger.rs` verbatim (#[path], no drift) and exercises the hash-chain:
//! append→chain-onto-prev, verify→detect-tamper, bounded-ring→prune-with-checkpoint.
//! Run: `cargo run` — panics on failure.
//!
//! The ledger core is **hasher-agnostic** (dependency-free): every append/verify takes an
//! injected `sha256` closure. Here we inject the real sha2 crate so the tamper tests are
//! meaningful. Tamper is simulated by mutating the ledger's `pub(crate)` ring directly —
//! this file is compiled *as part of* the ledger_verify crate via #[path], so no test-only
//! production method is needed (an attacker flipping a stored byte is exactly this).

#[path = "../../../rust/clock/src/net/ledger.rs"]
mod ledger;

use ledger::{Ledger, VerifyError, GENESIS, HASH_LEN, MAX_PAYLOAD};
use sha2::{Digest, Sha256};

/// The injected hasher — a full sha256 (the ledger core never imports sha2 itself).
fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

fn main() {
    // --- new(): empty ledger verifies clean; tip is GENESIS ----------------
    let mut l: Ledger<16, 32> = Ledger::new();
    assert_eq!(l.verify(sha256), Ok(()), "empty ledger verifies");
    assert_eq!(l.tip(), GENESIS, "fresh tip is GENESIS");
    assert_eq!(l.len(), 0);
    assert!(l.is_empty(), "fresh ledger is empty");
    assert_eq!(l.pruned(), 0);

    // --- MAX_PAYLOAD boundary: a record at the hashable cap verifies ------
    {
        let mut capl: Ledger<2, MAX_PAYLOAD> = Ledger::new();
        capl.append(&[0x5a; MAX_PAYLOAD], sha256);
        assert_eq!(
            capl.verify(sha256),
            Ok(()),
            "a max-payload (cap) record verifies"
        );
    }

    // --- append chains onto prev; tip advances; seqs are 1,2,3 -------------
    let h1 = l.append(b"elect id7 ch6", sha256);
    let h2 = l.append(b"ota install b45", sha256);
    let h3 = l.append(b"cfg id7 default=batt", sha256);
    assert_ne!(h1, GENESIS, "first record's hash != genesis");
    assert_ne!(h1, h2);
    assert_ne!(h2, h3);
    assert_eq!(l.tip(), h3, "tip is the newest record's hash");
    assert_eq!(l.len(), 3);
    assert_eq!(
        l.verify(sha256),
        Ok(()),
        "an untampered 3-record chain verifies"
    );

    // --- determinism: identical (seq, payload) sequence ⇒ identical tip ----
    let mut m: Ledger<16, 32> = Ledger::new();
    m.append(b"elect id7 ch6", sha256);
    m.append(b"ota install b45", sha256);
    m.append(b"cfg id7 default=batt", sha256);
    assert_eq!(
        m.tip(),
        l.tip(),
        "same records ⇒ same chain tip (deterministic)"
    );

    // --- tamper a PAYLOAD byte ⇒ verify fails at that record ---------------
    {
        let mut t: Ledger<16, 32> = Ledger::new();
        t.append(b"aaaa", sha256);
        t.append(b"bbbb", sha256);
        t.append(b"cccc", sha256);
        t.ring[1].payload[0] ^= 0xff; // flip a byte of record index 1 (the "bbbb")
        assert_eq!(
            t.verify(sha256),
            Err(VerifyError::BadHash { index: 1 }),
            "a flipped payload byte is detected at its record"
        );
    }

    // --- tamper a stored HASH ⇒ verify fails -------------------------------
    {
        let mut t: Ledger<16, 32> = Ledger::new();
        t.append(b"one", sha256);
        t.append(b"two", sha256);
        t.ring[0].hash[0] ^= 0xff; // corrupt record 0's stored hash
        assert!(
            t.verify(sha256).is_err(),
            "a corrupted stored hash is detected"
        );
    }

    // --- tamper SEQ (non-contiguous) ⇒ BadSeq ------------------------------
    {
        let mut t: Ledger<16, 32> = Ledger::new();
        t.append(b"one", sha256);
        t.append(b"two", sha256);
        t.ring[1].seq = 99; // break seq contiguity at index 1
        assert_eq!(
            t.verify(sha256),
            Err(VerifyError::BadSeq { index: 1 }),
            "non-contiguous seq is detected"
        );
    }

    // --- genesis: the first record chains onto GENESIS (verify from base) --
    // (implicitly covered by the Ok() above; assert the base is GENESIS pre-prune)
    {
        let mut t: Ledger<16, 32> = Ledger::new();
        t.append(b"x", sha256);
        assert_eq!(
            t.base(),
            GENESIS,
            "before any prune, the chain base is GENESIS"
        );
        assert_eq!(t.verify(sha256), Ok(()));
    }

    // --- bounded ring: overflow prunes oldest, keeps a checkpoint ----------
    {
        let mut t: Ledger<4, 16> = Ledger::new(); // CAP=4
        for i in 0..7u32 {
            let mut p = [0u8; 8];
            p[..4].copy_from_slice(&i.to_le_bytes());
            t.append(&p, sha256);
        }
        assert_eq!(t.len(), 4, "ring is bounded at CAP");
        assert_eq!(t.pruned(), 3, "3 oldest records pruned (7 appended, CAP 4)");
        assert_ne!(
            t.base(),
            GENESIS,
            "base is now the pruned checkpoint, not GENESIS"
        );
        assert_eq!(
            t.verify(sha256),
            Ok(()),
            "the retained suffix still verifies against the pruned checkpoint"
        );
        // tamper a retained record post-prune ⇒ still detected
        t.ring[t.head].payload[0] ^= 0xff; // corrupt the oldest RETAINED record
        assert!(
            t.verify(sha256).is_err(),
            "tamper after prune is still detected"
        );
    }

    // --- const-constructible (must live in a .bss static, no heap) ---------
    const _CONST_OK: Ledger<8, 16> = Ledger::new();

    // --- HASH_LEN is the truncated width documented in the study -----------
    assert_eq!(HASH_LEN, 16, "16-B truncated chain hash");

    println!("ledger_verify: all assertions passed");
}
