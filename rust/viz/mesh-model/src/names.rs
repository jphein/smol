//! Node names for the host tools — a thin re-export of the `sigil-names` crate.
//!
//! This file used to hold a SECOND full copy of the 20x20 corpus, hand-kept in sync with
//! `rust/clock/src/net/names.rs`. Two hand-synced copies of one table is precisely the drift the
//! sigil binding exists to remove, so the table is gone and only the accessors remain: meshscope and
//! the firmware now fold identical names through one implementation, and `tools/sigil_vendor.sh`
//! checks that implementation against upstream.
//!
//! Unlike the 72x40 OLED, host tools have room for the **full sigil** — and the full pair is unique
//! across all 256 ids, so nothing here needs the firmware's `noun+id` disambiguation.

/// `(adjective, noun)` for a node id — identical to what the board computes for itself.
pub fn name_for_id(id: u8) -> (&'static str, &'static str) {
    sigil_names::name_for_id(id, sigil_names::FLEET)
}

/// The noun alone.
///
/// ⚠️ **Not an identifier.** 32 nouns over 256 ids forces at least 8 ids (9 at worst) to share every
/// noun, so this can only be used where the id is printed alongside it. Prefer [`sigil_for_id`].
pub fn noun_for_id(id: u8) -> &'static str {
    name_for_id(id).1
}

/// The full sigil as one string — `"Obsidian Aegis"`. **Unique for every one of the 256 ids**
/// (proven at compile time in the crate), so this identifies a node on its own.
pub fn sigil_for_id(id: u8) -> String {
    let (a, n) = name_for_id(id);
    format!("{a} {n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the OLED cannot use but meshscope can: the pair alone identifies a node, with no
    /// id suffix needed. Exhaustive, matching the crate's own const-eval proof.
    #[test]
    fn full_sigil_is_unique_across_the_whole_id_space() {
        let mut seen = std::collections::BTreeSet::new();
        for id in 0..=u8::MAX {
            assert!(seen.insert(sigil_for_id(id)), "id {id} collides");
        }
        assert_eq!(seen.len(), 256);
    }

    /// And the reason `noun_for_id` must never stand alone.
    #[test]
    fn the_bare_noun_is_not_an_identifier() {
        let distinct: std::collections::BTreeSet<_> = (0..=u8::MAX).map(noun_for_id).collect();
        assert_eq!(distinct.len(), 32, "32 nouns over 256 ids — a noun cannot identify a node");
    }
}
