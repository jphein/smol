//! bard (#300): per-node protagonist — every board tells its own kind of stories (spec §8).
//!
//! Words must be TinyStories-FREQUENT. The model has a 512-token vocabulary, so the realm's own
//! names ("Draconic Dominion", "Eldritch Nexus") would shred into `<0xXX>` byte fallbacks and
//! poison the prompt (spec §8) — hence plain nursery nouns, each of which the tokenizer holds
//! as whole words. The node's identity survives in WHICH protagonist it gets, not in spelling
//! its realm name at the model.
//!
//! Pure: no `hw`, no radio, no alloc. Exported to the host lib beside the other cores so the
//! prompt lengths are testable off-device.

/// The 16 protagonists, indexed by node id (see [`protagonist`]).
pub const PROTAGONISTS: [&str; 16] = [
    "a little dragon",
    "a little owl",
    "a little bird",
    "a brave cat",
    "a tiny robot",
    "a little fish",
    "a small dog",
    "a little bunny",
    "a happy bear",
    "a little star",
    "a small mouse",
    "a little duck",
    "a small frog",
    "a kind girl",
    "a brave boy",
    "a little pony",
];

/// This node's protagonist.
///
/// id7 Draconic Dominion → dragon, id8 Eldritch Nexus → owl, id9 Jade Herald → bird: the three
/// live boards get a creature that matches their realm persona. Every other id falls back to
/// `id % 16`, so an unprovisioned board still tells a coherent story instead of nothing.
pub fn protagonist(node_id: u8) -> &'static str {
    PROTAGONISTS[match node_id {
        7 => 0,
        8 => 1,
        9 => 2,
        n => (n as usize) % 16,
    }]
}

/// Fill `buf` with this node's full prompt; returns the used length.
///
/// Takes a caller-owned buffer rather than returning a `&str` because the firmware builds this
/// into a static — no alloc anywhere in the bard. The longest possible prompt is 28 + 15 = 43
/// bytes, so the 64-byte buffer cannot overflow (asserted by the host test over all 16).
pub fn prompt(node_id: u8, buf: &mut [u8; 64]) -> usize {
    let mut n = 0;
    for part in ["Once upon a time, there was ", protagonist(node_id)] {
        buf[n..n + part.len()].copy_from_slice(part.as_bytes());
        n += part.len();
    }
    n
}
