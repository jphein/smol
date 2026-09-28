//! Four-byte identities carried in `L` (§4.1): the first 4 bytes of a SHA-256, little-endian.
use sha2::{Digest, Sha256};
use tapstone_rules::HouseRules;

fn first4(d: &[u8]) -> u32 {
    u32::from_le_bytes([d[0], d[1], d[2], d[3]])
}

/// The deck's name on the wire: a hash of the sorted design list, so the physical shuffle never
/// changes it and copy counts do.
pub fn deck_sigil(cards: &[u16]) -> u32 {
    let mut sorted = [0u16; 32];
    let n = cards.len().min(sorted.len());
    sorted[..n].copy_from_slice(&cards[..n]);
    sorted[..n].sort_unstable();
    let mut h = Sha256::new();
    h.update(b"tapstone:deck");
    for c in &sorted[..n] {
        h.update(c.to_le_bytes());
    }
    first4(&h.finalize())
}

pub fn rules_id_bytes(b: &[u8; 9]) -> u32 {
    let mut h = Sha256::new();
    h.update(b"tapstone:rules");
    h.update(b);
    first4(&h.finalize())
}

pub fn rules_id(r: &HouseRules) -> u32 {
    rules_id_bytes(&r.bytes())
}
