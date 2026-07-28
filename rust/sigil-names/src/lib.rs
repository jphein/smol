//! # realm-sigil — the Rust binding
//!
//! Deterministic magical names, matching the Go, Python and JS implementations for the same
//! `(hash, realm)`. `no_std`, no `alloc`, no dependencies: the corpus is a static table in
//! `.rodata` and the whole algorithm is two modulo operations, so this compiles into bare-metal
//! firmware without costing heap, stack or a dependency tree.
//!
//! ## Why this binding exists
//!
//! It is the answer to a specific failure. smol (an ESP32-C3 mesh, `no_std` Rust) needed
//! deterministic node names and there was no Rust binding, so it **hand-copied** the generated
//! corpus into its own source — twice, in two crates. Both copies then drifted from
//! `words/realms.json` without anything noticing, and the drift warning they carried had the
//! direction backwards. A hand-copied table cannot be checked; a generated one can.
//!
//! ## The algorithm (identical in all four languages)
//!
//! ```text
//! seed      = parse_hex(hash)
//! adjective = realm.adjectives[seed % len(adjectives)]
//! noun      = realm.nouns[(seed >> 8) % len(nouns)]
//! ```
//!
//! ⚠️ **`js/index.js` diverges for seeds ≥ 2³¹** — it applies `>>` to a `parseInt` result, which
//! coerces to int32, so the shift goes negative and the lookup yields `undefined`. Go, Python and
//! this crate agree. Sigil's own tests only feed 7-hex-char git hashes (< 2²⁸), which is why it has
//! never surfaced there; it bites any consumer using a full `u32` seed. This crate is the
//! reference-correct implementation until that is fixed upstream.
//!
//! ## Node identity vs. provenance — two namespaces, deliberately
//!
//! [`FLEET`] names **things** (a board, a node — an identity). The themed realms name **builds**
//! (a version — provenance). A name should never leave you wondering whether you are looking at
//! *which board* or *which build*. The [`reserved`] set extends the same separation one level
//! further out: a node must never be named after a role, a frame, a feature or a tool. `crown` IS
//! the gateway role — a board permanently called Crown is a permanent ambiguity, and it cost real
//! debugging time on 2026-07-28.
//!
//! ⚠️ **That separation is enforced where it matters and still imperfect elsewhere — check, don't
//! assume.** I first wrote here that the realms "draw from disjoint vocabularies on purpose", and my
//! own test refuted it within the hour. That is exactly the failure the reserved set exists to
//! prevent: a separation asserted in prose and enforced nowhere. (The same sentence had been sitting
//! in smol's `familiar/mod.rs` for months — "distinct from any node's name", while both namespaces
//! drew from the *identical* corpus.)
//!
//! So it is now machine-checked, as a **shrinking list** that can only improve:
//! - `forge` shared the noun `ember` with `fleet` — one word naming both a build and a board, the
//!   worst case, since those two appear in the same sentence ("board X is running build Y").
//!   **FIXED** in lexicon `e690207` (`ember` → `quench`).
//! - `signal` still shares `keystone` and `pulsar` with `fleet`. Tolerated deliberately: a *project*
//!   name never appears in that sentence, so the overlap costs nothing. That triage — an overlap is
//!   dangerous when both namespaces name things that co-occur in one operational sentence — is what
//!   makes the remaining entries a judgement someone can check rather than a backlog.
//!
//! [`realms_disjoint`] / [`nouns_disjoint`] are the check to use when you need the property to hold
//! for a specific pair. [`CREATURE`] vs [`FLEET`] is asserted at compile time here; smol additionally
//! asserts `FLEET` against its pinned forge table, so a future re-sync cannot quietly reintroduce a
//! build/board name clash.
//!
//! ## What is guaranteed, and how
//!
//! For [`FLEET`], **every one of the 256 `u8` ids maps to a distinct name**. That is not a
//! probability and not a sampled test — [`is_injective_over_u8`] enumerates the *complete* space at
//! **compile time**, and the `const` assertions below make a colliding namespace unrepresentable:
//! the crate does not build if the property is lost. Same for the reserved-set exclusion and the
//! 32×32 size lock.
//!
//! The size lock is load-bearing, not tidiness. `A = 32 = 2⁵` makes `seed % A` the low 5 bits and
//! `(seed >> 8) % N` bits 8–12 — **disjoint bit fields**. A non-power-of-two modulus mixes bits and
//! correlates the two indices: 24 × 25 = 600 combinations collides **201** times over 256 ids,
//! while 32 × 16 = 512 collides **zero**. Vocabulary size is not the lever; divisor arithmetic is.
//! Adding one word to `fleet` would silently break uniqueness — hence the equality assertion.
//!
//! Injectivity depends only on the *counts* and the multiplier, never on which words are in the
//! lists. Curate freely; keep the counts.

#![no_std]
#![forbid(unsafe_code)]

// `no_std` is unconditional for the library — firmware consumers must never accidentally get `std`
// linked because a test needed it. The test harness alone opts back in, so the collections used to
// make the guarantees legible in test output cost consumers nothing.
#[cfg(test)]
extern crate std;

mod realms;
mod reserved;

pub use realms::REALMS;
pub use reserved::RESERVED;

/// Name-based realm lookup — **only with the `divergent-themed-realms` feature**.
///
/// Gated because a name lookup is precisely how a mixed-language project would silently acquire a
/// divergent version name: it asks for `"fantasy"` in two languages and gets two different answers.
/// See the feature's documentation in `Cargo.toml`. For node identity use [`FLEET`] directly, which
/// is always available and cannot diverge.
#[cfg(feature = "divergent-themed-realms")]
pub use realms::realm_by_name;

/// Every themed realm the other bindings expose, re-exported for the gated case only.
#[cfg(feature = "divergent-themed-realms")]
pub use realms::{FANTASY, FORGE, ORACLE, SIGNAL, STELLAR, TAROT, VOID};

/// A realm's word corpus. `name = "{adjectives[seed % |A|]} {nouns[(seed >> 8) % |N|]}"`.
pub struct Realm {
    pub name: &'static str,
    pub adjectives: &'static [&'static str],
    pub nouns: &'static [&'static str],
}

/// Knuth multiplicative hash constant (2³² / φ, rounded to odd). Spreads a small integer across
/// all 32 seed bits. **Oddness is required**, not decorative: it makes `id ↦ (id · G) mod 2ᵏ` a
/// bijection for any `k`, which is what lets the adjective index alone separate every id class
/// mod 32.
pub const GOLDEN_U32: u32 = 2_654_435_761;

/// Spread an 8-bit id across 32 bits. WITHOUT this every id < 256 has `(seed >> 8) == 0` and
/// shares noun index 0 — every node would get the same noun.
#[inline]
pub const fn seed_from_id(id: u8) -> u32 {
    (id as u32).wrapping_mul(GOLDEN_U32)
}

/// The `(adjective, noun)` INDEX pair for a seed. Separated from [`name_for_seed`] so the
/// compile-time proofs can reason about indices without materialising strings.
#[inline]
pub const fn indices(seed: u32, realm: &Realm) -> (usize, usize) {
    (
        (seed as usize) % realm.adjectives.len(),
        ((seed >> 8) as usize) % realm.nouns.len(),
    )
}

/// `(adjective, noun)` for a seed — the shared algorithm, matching Go and Python exactly.
#[inline]
pub const fn name_for_seed(seed: u32, realm: &Realm) -> (&'static str, &'static str) {
    let (a, n) = indices(seed, realm);
    (realm.adjectives[a], realm.nouns[n])
}

/// `(adjective, noun)` for a node id. Both ends of a link compute the same name from the id
/// carried in the frame, so **names never need to go on the wire**.
#[inline]
pub const fn name_for_id(id: u8, realm: &Realm) -> (&'static str, &'static str) {
    name_for_seed(seed_from_id(id), realm)
}

/// `(adjective, noun)` from a hex string — parity with Go's `GenerateName(hash, realm)` and
/// Python's `generate_name`. Non-hex characters are skipped, matching Go's tolerant `parseHex`;
/// `"dev"` therefore yields seed 0, as it does in Python.
pub const fn name_for_hex(hash: &str, realm: &Realm) -> (&'static str, &'static str) {
    name_for_seed(parse_hex(hash) as u32, realm)
}

/// Tolerant hex parse, mirroring Go's `parseHex`: accumulate hex digits, ignore anything else.
/// Widening to `u64` matches Go/Python; the `u32` truncation happens at the call site so the
/// modulo arithmetic is identical to the other bindings for the 8-hex-char seeds smol uses.
pub const fn parse_hex(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut acc: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let d = match bytes[i] {
            b'0'..=b'9' => Some(bytes[i] - b'0'),
            b'a'..=b'f' => Some(bytes[i] - b'a' + 10),
            b'A'..=b'F' => Some(bytes[i] - b'A' + 10),
            _ => None,
        };
        if let Some(d) = d {
            acc = acc.wrapping_mul(16).wrapping_add(d as u64);
        }
        i += 1;
    }
    acc
}

// ---------------------------------------------------------------------------------------------
// Compile-time proofs. These are `const fn` so a consumer can assert them too — and so that a
// vocabulary edit that breaks a guarantee fails the BUILD rather than a test someone might not run.
// ---------------------------------------------------------------------------------------------

/// True iff every one of the 256 `u8` ids maps to a distinct `(adjective, noun)` pair in `realm`.
///
/// This enumerates the **complete** identifier space, which is why it is a proof rather than a
/// sample. It is deliberately not an algebraic argument: `id ↦ (id·G) mod 32` is a clean bijection,
/// but carries out of the low bits of `id·G` can propagate into bits 8–12, so there is no tidy
/// theorem for the second half. The enumeration does real work.
pub const fn is_injective_over_u8(realm: &Realm) -> bool {
    let mut i: u16 = 0;
    while i < 256 {
        let (ai, ni) = indices(seed_from_id(i as u8), realm);
        let mut j: u16 = 0;
        while j < i {
            let (aj, nj) = indices(seed_from_id(j as u8), realm);
            if ai == aj && ni == nj {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// True iff no word in `realm` appears in [`RESERVED`] (ASCII case-insensitive).
///
/// **Must hold before uniqueness means anything.** Distinctness over a raw list passes happily
/// while the live namespace still collides with a role name.
pub const fn has_no_reserved_word(realm: &Realm) -> bool {
    let mut i = 0;
    while i < realm.adjectives.len() {
        if is_reserved(realm.adjectives[i]) {
            return false;
        }
        i += 1;
    }
    let mut i = 0;
    while i < realm.nouns.len() {
        if is_reserved(realm.nouns[i]) {
            return false;
        }
        i += 1;
    }
    true
}

/// True iff `word` is in [`RESERVED`], ASCII case-insensitively.
pub const fn is_reserved(word: &str) -> bool {
    let mut i = 0;
    while i < RESERVED.len() {
        if str_eq_ascii_ci(word, RESERVED[i]) {
            return true;
        }
        i += 1;
    }
    false
}

/// True iff `a` and `b` share no word at all — no adjective, no noun, in any position.
///
/// The reserved set stops an identity colliding with a *role*, *frame*, *feature* or *tool*. It
/// does not stop **two naming namespaces colliding with each other**, and that gap is not
/// hypothetical: smol's familiar draws creature names from one corpus while nodes draw from
/// another, and `familiar/mod.rs` has carried the comment *"distinct from any node's name"* since
/// it was written — while both namespaces drew from the **identical** corpus. The property was
/// documented and never held.
///
/// So: assert it. Any two namespaces that a person might see side by side — identity vs. version,
/// identity vs. creature — should be provably disjoint, not disjoint by intention.
pub const fn realms_disjoint(a: &Realm, b: &Realm) -> bool {
    nouns_disjoint(a, b) && adjectives_disjoint(a, b)
}

/// True iff `a` and `b` share no NOUN. The sharper half of [`realms_disjoint`]: the noun is what a
/// cramped display shows, so a shared noun is what actually renders ambiguously.
pub const fn nouns_disjoint(a: &Realm, b: &Realm) -> bool {
    let mut i = 0;
    while i < a.nouns.len() {
        let mut j = 0;
        while j < b.nouns.len() {
            if str_eq_ascii_ci(a.nouns[i], b.nouns[j]) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// True iff `a` and `b` share no ADJECTIVE.
pub const fn adjectives_disjoint(a: &Realm, b: &Realm) -> bool {
    let mut i = 0;
    while i < a.adjectives.len() {
        let mut j = 0;
        while j < b.adjectives.len() {
            if str_eq_ascii_ci(a.adjectives[i], b.adjectives[j]) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// True iff every noun in `realm` is still distinct when truncated to `prefix` characters.
///
/// For consumers with a hard display budget. smol's OLED is 72×40 and clips node names to 5–8
/// characters, so a truncation that collides two nouns re-creates the ambiguity the full name
/// removed. Assert this at the width you actually clip to. ASCII-only, like the corpus.
pub const fn nouns_distinct_at(realm: &Realm, prefix: usize) -> bool {
    let mut i = 0;
    while i < realm.nouns.len() {
        let mut j = 0;
        while j < i {
            if prefix_eq_ascii_ci(realm.nouns[i], realm.nouns[j], prefix) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// Compare the first `n` bytes case-insensitively. Compares BYTES rather than slicing to a `&str`
/// because that is what the consumer's clip actually does: smol's `clip()` helpers in `bench.rs`
/// and `rssi.rs` are `&s[..n]`, i.e. byte-slicing, which is only safe while the corpus is ASCII.
/// Modelling the real operation keeps this check honest.
const fn prefix_eq_ascii_ci(a: &str, b: &str, n: usize) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let la = if n < a.len() { n } else { a.len() };
    let lb = if n < b.len() { n } else { b.len() };
    if la != lb {
        return false;
    }
    let mut i = 0;
    while i < la {
        if ascii_lower(a[i]) != ascii_lower(b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

const fn str_eq_ascii_ci(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if ascii_lower(a[i]) != ascii_lower(b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

const fn ascii_lower(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

// ---------------------------------------------------------------------------------------------
// The guarantees, enforced. A vocabulary edit that breaks one of these does not compile.
// ---------------------------------------------------------------------------------------------

/// The node-identity realm. Every consumer naming a *thing* should use this one; the themed realms
/// name *builds*. Keeping those apart is the whole point of the split.
pub const FLEET: &Realm = &realms::FLEET;

/// The creature realm — a THIRD namespace, for entities that are neither boards nor builds
/// (smol's familiar). Ungated for the same reason as [`FLEET`]: it exists in no other binding, so
/// nothing can contradict a name it produces.
///
/// **Deliberately 24×24, and deliberately NOT a power of two.** The 32-lock on `FLEET` exists only
/// to make a *256-element* space injective; a creature seeds from an arbitrary `u32`, so its domain
/// is ~2³² and **injectivity is impossible by pigeonhole, not merely unnecessary**. Copying the lock
/// here would be cargo-culting a rule past the premise that justifies it. What matters instead is a
/// birthday bound over concurrently-visible creatures (576 combos ≈ 7.5% across 10) and, critically,
/// **disjointness from `FLEET`** — a creature that shares a board's name is exactly the ambiguity the
/// taxonomy exists to prevent, and it is asserted below.
pub const CREATURE: &Realm = &realms::CREATURE;

const _: () = assert!(
    has_no_reserved_word(CREATURE),
    "a creature vocabulary word is in the RESERVED set — see the FLEET assertion."
);

const _: () = assert!(
    realms_disjoint(CREATURE, FLEET),
    "a creature word collides with a node-identity word, so a familiar could render the same name \
     as a board. This is the gap that let `familiar/mod.rs` claim creature names were \"distinct \
     from any node's name\" for months while both drew from the identical corpus — now it cannot \
     be claimed without being true."
);

/// The familiar renders on the same 72×40 panel as node names, so it inherits the clip budget.
const _: () = assert!(
    nouns_distinct_at(CREATURE, 4) && nouns_distinct_at(CREATURE, 5),
    "two creature nouns become identical when clipped to 4 or 5 chars — the familiar's display \
     budget. Rename a word in lexicon's creature group."
);

const _: () = assert!(
    FLEET.adjectives.len() == 32,
    "fleet adjectives must be EXACTLY 32. This is not a floor: 32 = 2^5 makes `seed % 32` the low \
     5 bits, disjoint from the noun's bits 8-12. Any other count mixes bits and re-introduces \
     collisions (24 x 25 = 600 combinations collides 201 times over 256 ids). Adding a word here \
     silently breaks uniqueness AND renames every board."
);

const _: () = assert!(
    FLEET.nouns.len() == 32,
    "fleet nouns must be EXACTLY 32 — see the adjectives assertion. Indices are `% len`, so a \
     single added or removed noun re-maps every id."
);

const _: () = assert!(
    has_no_reserved_word(FLEET),
    "a fleet vocabulary word is in the RESERVED set — a node would be named after a role, a wire \
     frame, a feature or a sibling project (`crown` IS the gateway role). Remove the word from \
     lexicon's fleet group; do NOT remove it from reserved.yaml to make this pass."
);

const _: () = assert!(
    is_injective_over_u8(FLEET),
    "the fleet namespace is NO LONGER UNIQUE over the u8 id space — two node ids would render the \
     same name, which is the exact defect this realm was created to eliminate. Check the 32x32 \
     size lock first; that is almost always the cause."
);

#[cfg(test)]
mod tests {
    use super::*;

    /// The `const` assertions above already prove this at compile time — if they failed, this file
    /// would not build. This test exists to make the guarantee *legible* in test output, and to
    /// print the actual count rather than only failing.
    #[test]
    fn fleet_is_injective_over_the_whole_u8_space() {
        let mut seen = std::collections::BTreeSet::new();
        for id in 0..=u8::MAX {
            let (a, n) = name_for_id(id, FLEET);
            assert!(
                seen.insert((a, n)),
                "id {id} collides on {a} {n} — 256 ids must yield 256 distinct names"
            );
        }
        assert_eq!(seen.len(), 256);
    }

    #[test]
    fn fleet_excludes_every_reserved_word() {
        for w in FLEET.adjectives.iter().chain(FLEET.nouns.iter()) {
            assert!(!is_reserved(w), "{w} is a reserved project term");
        }
    }

    #[test]
    fn fleet_is_size_locked_at_32x32() {
        assert_eq!(FLEET.adjectives.len(), 32);
        assert_eq!(FLEET.nouns.len(), 32);
    }

    /// Guards the claim in the module docs, so the counter-example cannot rot into folklore.
    #[test]
    fn a_bigger_vocabulary_is_not_a_safer_one() {
        fn distinct(a: usize, n: usize) -> usize {
            let mut s = std::collections::BTreeSet::new();
            for id in 0..=u8::MAX {
                let seed = seed_from_id(id);
                s.insert(((seed as usize) % a, ((seed >> 8) as usize) % n));
            }
            s.len()
        }
        assert_eq!(distinct(32, 32), 256, "32x32 must be collision-free");
        assert_eq!(distinct(32, 16), 256, "32x16 = 512 combos, also collision-free");
        assert_eq!(distinct(24, 25), 55, "24x25 = 600 combos yet collides 201 times");
        assert_eq!(distinct(40, 40), 249, "40x40 = 1600 combos and STILL collides");
    }

    /// The disjointness helpers must detect an overlap in EITHER position, and must not
    /// false-positive on genuinely disjoint sets. Tested with fixtures rather than live realms so
    /// the assertion cannot silently become vacuous if a corpus changes.
    #[test]
    fn disjointness_detects_overlap_in_either_position() {
        const A: Realm = Realm { name: "a", adjectives: &["Ashen"], nouns: &["Vigil"] };
        const SHARED_NOUN: Realm = Realm { name: "b", adjectives: &["Molten"], nouns: &["Vigil"] };
        const SHARED_ADJ: Realm = Realm { name: "c", adjectives: &["ashen"], nouns: &["Anvil"] };
        const CLEAN: Realm = Realm { name: "d", adjectives: &["Molten"], nouns: &["Anvil"] };

        assert!(!nouns_disjoint(&A, &SHARED_NOUN), "a shared noun must be caught");
        assert!(!realms_disjoint(&A, &SHARED_NOUN));
        // Case-insensitive: "ashen" must collide with "Ashen".
        assert!(!adjectives_disjoint(&A, &SHARED_ADJ), "case must not hide an overlap");
        assert!(!realms_disjoint(&A, &SHARED_ADJ));
        assert!(nouns_disjoint(&A, &SHARED_ADJ), "only the adjective overlaps here");
        assert!(realms_disjoint(&A, &CLEAN));
    }

    /// Identity should not share a noun with provenance: `fleet` names boards, the themed realms
    /// name builds, and a reader glancing at a screen must never have to wonder which they are
    /// looking at.
    ///
    /// **It does not currently hold, and this test says so out loud rather than being weakened to
    /// green.** The overlaps below are a DEFECT LIST awaiting curation, not an accepted state — the
    /// test asserts that no *new* overlap appears and that every listed one still exists, so the
    /// list can only shrink. Deleting a line as a word is fixed is the intended workflow; if a
    /// listed overlap disappears the test fails and tells you to remove the line.
    ///
    /// The `forge` entry is the one that matters: **forge is the VERSION realm**, so `ember` can
    /// name a build AND a board simultaneously — exactly the ambiguity the split exists to prevent,
    /// and exactly what `smol/rust/clock/src/net/names.rs` has claimed to provide since it was
    /// written. (smol is insulated today only because it pins the OLD 20-word forge table, which is
    /// disjoint from `fleet`; upstream's post-cutover 14-word forge is not.)
    ///
    /// `fantasy` overlapping heavily is different in kind and arguably fine: `fleet` was curated
    /// largely FROM the fantasy pool. But it means "identity vs fantasy" is not a separation at all,
    /// which is worth knowing before anyone relies on it.
    #[cfg(feature = "divergent-themed-realms")]
    #[test]
    fn identity_vs_provenance_overlaps_are_a_shrinking_list() {
        // (realm, nouns known to be shared with `fleet`). Shrink me.
        let known: &[(&Realm, &[&str])] = &[
            (&FORGE, &[]),                        // was ["ember"] — FIXED in lexicon e690207
            (&SIGNAL, &["keystone", "pulsar"]),
            (&ORACLE, &[]),
            (&STELLAR, &[]),
            (&TAROT, &[]),
            (&VOID, &[]),
        ];
        for (themed, expected) in known {
            let actual: std::vec::Vec<&str> = FLEET
                .nouns
                .iter()
                .filter(|n| themed.nouns.iter().any(|t| t.eq_ignore_ascii_case(n)))
                .map(|n| *n)
                .collect();
            let actual_lower: std::vec::Vec<std::string::String> =
                actual.iter().map(|s| s.to_ascii_lowercase()).collect();
            let mut expect_sorted: std::vec::Vec<std::string::String> =
                expected.iter().map(|s| s.to_ascii_lowercase()).collect();
            let mut got_sorted = actual_lower.clone();
            expect_sorted.sort();
            got_sorted.sort();
            assert_eq!(
                got_sorted, expect_sorted,
                "identity/provenance noun overlap with `{}` CHANGED. If you fixed one, delete it \
                 from the list. If a new one appeared, a board can now read as a build — fix the \
                 vocabulary, do not extend this list.",
                themed.name
            );
        }
    }

    /// smol clips node names to 5-8 chars on a 72x40 OLED.
    #[test]
    fn fleet_nouns_survive_display_truncation() {
        for w in 4..=8 {
            assert!(nouns_distinct_at(FLEET, w), "nouns collide when clipped to {w} chars");
        }
    }

    /// Cross-language parity: these are the values Go and Python produce. `parse_hex` is tolerant
    /// of non-hex bytes exactly as Go's is, so `"dev"` degrades to seed 0 rather than erroring.
    #[test]
    fn hex_seeding_matches_the_other_bindings() {
        assert_eq!(parse_hex("9e3779b1"), 0x9e3779b1);
        assert_eq!(parse_hex("abc1234"), 0x0abc1234);
        assert_eq!(parse_hex("dev"), 0xde); // 'd', 'e' are hex digits; 'v' is skipped
        assert_eq!(parse_hex(""), 0);
    }

    /// ALGORITHM parity with Go, isolated from corpus drift.
    ///
    /// This matters because a plain four-way comparison of `generate_name("9e3779b1", "fantasy")`
    /// currently DISAGREES: Go, Python and JS all say `Blazing Jewel`, this crate says
    /// `Draconic Monolith`. That is **not** an algorithm difference — it is corpus staleness.
    /// go/python/js ship generated embeds frozen on 2026-04-05 (20 adj / 20 nouns), while
    /// `words/realms.json` was cut over to lexicon on 2026-05-07 (28 / 25) and this crate is
    /// generated from the current file. Same arithmetic, different table.
    ///
    /// So the parity worth testing is: *given the same words, do we compute the same index?* This
    /// pins the 2026-04-05 fantasy corpus as a fixture and asserts we reproduce Go's live output
    /// on it — including the u32 edges where JS used to return `undefined`.
    ///
    /// ⚠️ `FLEET` is unaffected by the drift and cannot diverge: it does not exist in the stale
    /// embeds at all, so no other binding can produce a conflicting node name. Node identity is
    /// safe; only the themed (version-name) realms differ across bindings today.
    #[test]
    fn algorithm_matches_go_given_the_same_corpus() {
        const STALE_FANTASY: Realm = Realm {
            name: "fantasy@2026-04-05",
            adjectives: &[
                "Arcane", "Blazing", "Celestial", "Draconic", "Eldritch", "Fabled", "Gilded",
                "Hallowed", "Infernal", "Jade", "Kindled", "Luminous", "Mythic", "Noble",
                "Obsidian", "Primal", "Radiant", "Spectral", "Twilight", "Valiant",
            ],
            nouns: &[
                "Aegis", "Beacon", "Crown", "Dominion", "Ember", "Forge", "Grimoire", "Herald",
                "Insignia", "Jewel", "Keystone", "Lantern", "Monolith", "Nexus", "Oracle",
                "Pinnacle", "Quartz", "Relic", "Sigil", "Throne",
            ],
        };
        // Captured from `go run` against realm-sigil/go on 2026-07-28.
        let go_says = [
            ("9e3779b1", "Blazing", "Jewel"),   // JS returned "Blazing undefined" before the fix
            ("f1bbcd88", "Eldritch", "Nexus"),  // ditto
            ("7dc219", "Jade", "Oracle"),
            ("abc1234", "Infernal", "Grimoire"),
            ("0000001", "Blazing", "Aegis"),
            ("fffffff", "Primal", "Pinnacle"),
            ("ffffffff", "Primal", "Pinnacle"), // u32 max
            ("80000000", "Infernal", "Insignia"), // sign bit set — the int32 trap
            ("7fffffff", "Hallowed", "Herald"),
        ];
        for (hash, adj, noun) in go_says {
            assert_eq!(
                name_for_hex(hash, &STALE_FANTASY),
                (adj, noun),
                "algorithm diverged from Go on hash {hash}"
            );
        }
    }

    /// A realm lookup must never panic on an unknown name — Go falls back to fantasy.
    #[cfg(feature = "divergent-themed-realms")]
    #[test]
    fn unknown_realm_falls_back() {
        assert_eq!(realm_by_name("no-such-realm").name, "fantasy");
        assert_eq!(realm_by_name("fleet").name, "fleet");
    }

    /// The DEFAULT build must expose exactly one realm — `fleet` — so a consumer cannot reach a
    /// corpus-divergent themed realm without naming the hazard in its Cargo.toml. This is the
    /// guard, and it is the reason the divergence is unrepresentable rather than documented.
    #[cfg(not(feature = "divergent-themed-realms"))]
    #[test]
    fn default_build_exposes_only_the_non_divergent_realm() {
        let names: std::vec::Vec<&str> = REALMS.iter().map(|r| r.name).collect();
        assert_eq!(names, ["creature", "fleet"], "only non-divergent realms may be reachable by default");
    }
}
