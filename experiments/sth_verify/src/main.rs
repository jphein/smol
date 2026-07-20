//! Host verification of the PURE #184 mesh-ledger L3 core: the ed25519 **Signed-Tree-Head**
//! over the L2 (#183) CT-Merkle anchor. Includes the real `net/sth.rs` + `net/treehead.rs`
//! verbatim (#[path], no drift). Run: `cargo run` — panics on failure.
//!
//! The point of L3: L2's anchor is *symmetric-unforgeable-free* (anyone can recompute it) — so a
//! node can't tell a genuine crown anchor from a forged one. Signing the anchor makes it
//! **non-repudiable**: only the crown's private key can author a valid STH. This test exercises
//! the real end-to-end: build an L2 head-set → its anchor → **sign it** → a peer **verifies the
//! STH signature AND an inclusion proof against the signed root** (the "prove my head is in a
//! crown-signed anchor" flow). Both the sha256 hasher and the ed25519 sign/verify are INJECTED
//! (the core is crypto-agnostic); here ed25519-compact (smol's OTA verifier) supplies real ones.

#[path = "../../../rust/clock/src/net/sth.rs"]
mod sth;
#[path = "../../../rust/clock/src/net/treehead.rs"]
mod treehead;

use ed25519_compact::{KeyPair, Seed, Signature};
use sha2::{Digest, Sha256};
use sth::{accept, sign_sth, verify_sth};
use treehead::{verify_inclusion, HeadSet};

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

fn head(seed: u8) -> [u8; 16] {
    let mut x = [0u8; 16];
    x[0] = seed;
    x[15] = seed ^ 0xa5;
    x
}

fn main() {
    // Fixed-seed crown keypair (deterministic — no RNG) + an attacker keypair.
    let crown = KeyPair::from_seed(Seed::new([7u8; 32]));
    let attacker = KeyPair::from_seed(Seed::new([9u8; 32]));
    let sign = |m: &[u8]| -> [u8; 64] {
        let s = crown.sk.sign(m, None);
        let mut b = [0u8; 64];
        b.copy_from_slice(&s[..]);
        b
    };
    let verify = |m: &[u8], sig: &[u8; 64]| -> bool {
        Signature::from_slice(sig)
            .map(|s| crown.pk.verify(m, &s).is_ok())
            .unwrap_or(false)
    };
    let verify_attacker = |m: &[u8], sig: &[u8; 64]| -> bool {
        Signature::from_slice(sig)
            .map(|s| attacker.pk.verify(m, &s).is_ok())
            .unwrap_or(false)
    };

    let root = [0x5au8; 16];

    // --- sign → verify round-trip -----------------------------------------
    let sth = sign_sth(root, 12, 5, sign);
    assert_eq!(sth.root(), root);
    assert_eq!(sth.size(), 12);
    assert_eq!(sth.epoch(), 5);
    assert!(verify_sth(&sth, verify), "an honestly-signed STH verifies");

    // --- tamper each authenticated field ⇒ verify fails -------------------
    {
        let mut t = sth;
        t.root[0] ^= 0xff;
        assert!(!verify_sth(&t, verify), "tampered root ⇒ STH verify fails");
    }
    {
        let mut t = sth;
        t.size ^= 0xff;
        assert!(!verify_sth(&t, verify), "tampered size ⇒ STH verify fails");
    }
    {
        let mut t = sth;
        t.epoch ^= 0xff; // epoch is authenticated ⇒ an old STH can't be replayed as a new one
        assert!(!verify_sth(&t, verify), "tampered epoch ⇒ STH verify fails");
    }
    {
        let mut t = sth;
        t.sig[0] ^= 0xff;
        assert!(
            !verify_sth(&t, verify),
            "tampered signature ⇒ STH verify fails"
        );
    }

    // --- wrong key (attacker) ⇒ verify fails ------------------------------
    assert!(
        !verify_sth(&sth, verify_attacker),
        "a different key ⇒ STH verify fails"
    );

    // --- epoch is authenticated: same root, different epoch → different sig -
    let sth5 = sign_sth(root, 12, 5, sign);
    let sth6 = sign_sth(root, 12, 6, sign);
    assert_ne!(
        sth5.sig(),
        sth6.sig(),
        "epoch is part of the signed message"
    );

    // --- THE REAL COMPOSITION: sign an L2 anchor, then prove a head is in it
    {
        let mut hs: HeadSet<16> = HeadSet::new();
        for i in 0..5u8 {
            hs.upsert(i, head(i + 1), i as u32 + 1).unwrap();
        }
        assert!(!hs.is_empty(), "head-set has entries before anchoring");
        let anchor = hs.root(sha256);
        let signed = sign_sth(anchor, hs.len() as u32, 7, sign); // the crown signs its anchor

        // A peer: verify the STH sig AND that node 3's head is included under the signed root.
        let proof = hs.inclusion_proof(3, sha256).unwrap();
        let incl_ok = verify_inclusion(signed.root(), 3, head(4), 4, &proof, sha256);
        assert!(
            accept(&signed, incl_ok, verify),
            "a crown-signed anchor + a valid inclusion proof ⇒ accept"
        );

        // Tamper the claimed head ⇒ inclusion fails ⇒ accept fails (even with a valid STH).
        let incl_bad = verify_inclusion(signed.root(), 3, head(0xff), 4, &proof, sha256);
        assert!(
            !accept(&signed, incl_bad, verify),
            "wrong head ⇒ not accepted"
        );

        // Tamper the STH signature ⇒ accept fails (even with a valid inclusion proof).
        let mut forged = signed;
        forged.sig[0] ^= 0xff;
        assert!(
            !accept(&forged, incl_ok, verify),
            "forged STH ⇒ not accepted"
        );

        // A valid STH but from the attacker's key ⇒ not accepted.
        assert!(
            !accept(&signed, incl_ok, verify_attacker),
            "wrong signer ⇒ not accepted"
        );
    }

    println!("sth_verify: all assertions passed");
}
