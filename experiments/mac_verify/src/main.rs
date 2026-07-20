//! #190 host verification of the PURE group-HMAC-SHA256 transport-auth codec (Fork B / B1).
//! Includes the real `net/wire.rs` verbatim (`#[path]`, no drift) and exercises the MAC codec
//! end-to-end. Run: `cargo run` — panics on any failure.
//!
//! Coverage:
//!   1. RFC 4231 KNOWN-ANSWER vectors (TC1, TC2) for `hmac_sha256` — the correctness proof for the
//!      hand-rolled HMAC-SHA256 (a wrong HMAC would silently split the fleet in two).
//!   2. append → verify ROUND-TRIP: the payload survives, the length grows by exactly the trailer.
//!   3. REJECTION: a flipped payload byte, a flipped tag byte, a flipped epoch, and the wrong key
//!      all fail (constant-time-compared).
//!   4. EPOCH ROTATION: a two-epoch accepted set verifies frames MAC'd under either epoch.
//!   5. SHORT frame (< trailer) fails cleanly (a legacy un-MAC'd tiny frame).
//!   6. MTU / trailer BUDGET invariants: `MAC_TRAILER_LEN == 1 + MAC_TAG_LEN`, and the UP2 inner
//!      budget reserves the trailer so a full UP2 frame + trailer is exactly `ESP_NOW_MTU`.

#[path = "../../../rust/clock/src/net/wire.rs"]
mod wire;

use wire::{
    append_group_mac, hmac_sha256, verify_group_mac, MacVerdict, ESP_NOW_MTU, MAC_TAG_LEN,
    MAC_TRAILER_LEN, UP2_INNER_MAX, UP2_OVERHEAD,
};

/// Decode a lowercase hex string into a `[u8; 32]` (panics on wrong length / non-hex).
fn hex32(s: &str) -> [u8; 32] {
    assert_eq!(s.len(), 64, "hex32 wants 64 hex chars");
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("valid hex");
    }
    out
}

fn main() {
    // --- 1. RFC 4231 KATs — prove the hand-rolled HMAC-SHA256 matches the standard --------------
    // TC1: key = 20 × 0x0b, data = "Hi There".
    let tc1_key = [0x0bu8; 20];
    let tc1 = hmac_sha256(&tc1_key, b"Hi There");
    assert_eq!(
        tc1,
        hex32("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"),
        "RFC 4231 test case 1"
    );
    // TC2: key = "Jefe", data = "what do ya want for nothing?".
    let tc2 = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        tc2,
        hex32("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
        "RFC 4231 test case 2"
    );

    // --- 2. append → verify round-trip ----------------------------------------------------------
    let key = [0x11u8; 32];
    let epoch = 1u8;
    let frame = b"SMOLv1 HELLO 007"; // a representative fixed-width control frame
    let mut buf = [0u8; 64];
    buf[..frame.len()].copy_from_slice(frame);
    let n = append_group_mac(&mut buf, frame.len(), &key, epoch);
    assert_eq!(n, frame.len() + MAC_TRAILER_LEN, "trailer grows the frame by MAC_TRAILER_LEN");
    match verify_group_mac(&buf[..n], &[(epoch, &key)]) {
        MacVerdict::Ok { payload_len } => {
            assert_eq!(payload_len, frame.len(), "verify recovers the original payload length");
            assert_eq!(&buf[..payload_len], frame, "verify recovers the original payload bytes");
        }
        MacVerdict::Fail => panic!("a freshly-MAC'd frame MUST verify"),
    }

    // --- 3. rejection: any tamper or a wrong key fails ------------------------------------------
    // (a) flipped payload byte.
    let mut t = buf;
    t[3] ^= 0x01;
    assert_eq!(verify_group_mac(&t[..n], &[(epoch, &key)]), MacVerdict::Fail, "flipped payload rejected");
    // (b) flipped tag byte (the very last byte).
    let mut t = buf;
    t[n - 1] ^= 0x01;
    assert_eq!(verify_group_mac(&t[..n], &[(epoch, &key)]), MacVerdict::Fail, "flipped tag rejected");
    // (c) flipped epoch byte (now points at an epoch not in the accepted set / breaks the covered MAC).
    let mut t = buf;
    t[frame.len()] ^= 0x01;
    assert_eq!(verify_group_mac(&t[..n], &[(epoch, &key)]), MacVerdict::Fail, "flipped epoch rejected");
    // (d) wrong key.
    let other = [0x22u8; 32];
    assert_eq!(verify_group_mac(&buf[..n], &[(epoch, &other)]), MacVerdict::Fail, "wrong key rejected");
    // (e) right key but an epoch that isn't the frame's epoch → no candidate key → fail.
    assert_eq!(verify_group_mac(&buf[..n], &[(epoch + 5, &key)]), MacVerdict::Fail, "unknown epoch rejected");

    // --- 4. epoch rotation: a two-epoch accepted set verifies EITHER epoch ----------------------
    let key_a = [0xa1u8; 32];
    let key_b = [0xb2u8; 32];
    let payload = b"SMOLv1 TIME 007 1700000000 1700000000";
    // Frame MAC'd under epoch N (key_a).
    let mut fa = [0u8; 64];
    fa[..payload.len()].copy_from_slice(payload);
    let na = append_group_mac(&mut fa, payload.len(), &key_a, 7);
    // Frame MAC'd under epoch N+1 (key_b).
    let mut fb = [0u8; 64];
    fb[..payload.len()].copy_from_slice(payload);
    let nb = append_group_mac(&mut fb, payload.len(), &key_b, 8);
    let accepted: &[(u8, &[u8; 32])] = &[(7, &key_a), (8, &key_b)];
    assert!(matches!(verify_group_mac(&fa[..na], accepted), MacVerdict::Ok { .. }), "overlap accepts epoch N");
    assert!(matches!(verify_group_mac(&fb[..nb], accepted), MacVerdict::Ok { .. }), "overlap accepts epoch N+1");
    // The old key alone must NOT verify the new-epoch frame (proves epoch actually selects the key).
    assert_eq!(verify_group_mac(&fb[..nb], &[(7, &key_a)]), MacVerdict::Fail, "epoch N key rejects an epoch N+1 frame");

    // --- 5. a frame shorter than the trailer is a clean Fail (legacy un-MAC'd tiny frame) -------
    assert_eq!(verify_group_mac(&[0u8; 4], &[(epoch, &key)]), MacVerdict::Fail, "sub-trailer frame fails");
    assert_eq!(verify_group_mac(&[], &[(epoch, &key)]), MacVerdict::Fail, "empty frame fails");

    // --- 6. MTU / trailer budget invariants -----------------------------------------------------
    assert_eq!(MAC_TRAILER_LEN, 1 + MAC_TAG_LEN, "trailer = epoch(1) + tag");
    // The UP2 inner budget reserves the trailer, so a MAXED UP2 frame + trailer is exactly the MTU.
    assert_eq!(
        UP2_INNER_MAX + UP2_OVERHEAD + MAC_TRAILER_LEN,
        ESP_NOW_MTU,
        "UP2 inner budget reserves the MAC trailer (full UP2 + trailer == MTU)"
    );
    // Append to a frame at the reserved ceiling: the on-wire result is exactly the MTU, never over.
    let ceiling = ESP_NOW_MTU - MAC_TRAILER_LEN;
    let mut big = [0u8; ESP_NOW_MTU];
    for (i, b) in big.iter_mut().enumerate().take(ceiling) {
        *b = b"SMOLv1 "[i % 7]; // any bytes; a real frame would start with the SMOLv1 prefix
    }
    let bn = append_group_mac(&mut big, ceiling, &key, epoch);
    assert_eq!(bn, ESP_NOW_MTU, "a ceiling-sized frame + trailer is exactly ESP_NOW_MTU");
    assert!(matches!(verify_group_mac(&big[..bn], &[(epoch, &key)]), MacVerdict::Ok { .. }), "ceiling frame verifies");

    println!("mac_verify: ALL CHECKS PASSED (RFC 4231 KATs + round-trip + tamper/key/epoch rejection + epoch-rotation dual-accept + MTU budget)");
}
