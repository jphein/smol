//! Deterministic magical node names — a host copy of the firmware's `net/names.rs`
//! (itself a faithful port of realm-sigil's `GenerateName`). A node's noun is
//! computed from its 3-digit id, identically on-device and here, so meshscope shows
//! the same fantasy handle the OLED does with ZERO dependency on HA discovery being
//! present. Node id is the only identity on the wire.
//!
//! CORPUS is pinned VERBATIM to sigil's generated `fantasy` embed (20 adj / 20 noun)
//! — matching `rust/clock/src/net/names.rs`. If that table ever changes, re-copy it.

struct Realm {
    adjectives: &'static [&'static str],
    nouns: &'static [&'static str],
}

static FANTASY: Realm = Realm {
    adjectives: &[
        "Arcane", "Blazing", "Celestial", "Draconic", "Eldritch", "Fabled", "Gilded",
        "Hallowed", "Infernal", "Jade", "Kindled", "Luminous", "Mythic", "Noble", "Obsidian",
        "Primal", "Radiant", "Spectral", "Twilight", "Valiant",
    ],
    nouns: &[
        "Aegis", "Beacon", "Crown", "Dominion", "Ember", "Forge", "Grimoire", "Herald",
        "Insignia", "Jewel", "Keystone", "Lantern", "Monolith", "Nexus", "Oracle", "Pinnacle",
        "Quartz", "Relic", "Sigil", "Throne",
    ],
};

/// Knuth multiplicative-hash constant (2^32 / phi, rounded to odd).
const GOLDEN_U32: u32 = 2_654_435_761;

fn seed_from_id(id: u8) -> u32 {
    (id as u32).wrapping_mul(GOLDEN_U32)
}

/// `(adjective, noun)` for a node id — same math as sigil / the firmware.
pub fn name_for_id(id: u8) -> (&'static str, &'static str) {
    let seed = seed_from_id(id);
    let adj = FANTASY.adjectives[(seed as usize) % FANTASY.adjectives.len()];
    let noun = FANTASY.nouns[((seed >> 8) as usize) % FANTASY.nouns.len()];
    (adj, noun)
}

/// The short handle (noun) meshscope labels a node with — matches the OLED.
pub fn noun_for_id(id: u8) -> &'static str {
    name_for_id(id).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nouns_vary_between_adjacent_ids() {
        // The golden-ratio spread must give adjacent ids different nouns (the whole
        // reason seed_from_id exists — without it every id < 256 shares noun[0]).
        let a = noun_for_id(7);
        let b = noun_for_id(8);
        let c = noun_for_id(9);
        assert!(a != b || b != c, "adjacent ids collapsed to one noun: {a} {b} {c}");
    }

    #[test]
    fn deterministic() {
        assert_eq!(name_for_id(42), name_for_id(42));
    }
}
