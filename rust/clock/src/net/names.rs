//! Deterministic magical names for the three things smol names: **nodes**, **builds**, and the
//! familiar's **creatures**. The corpus and the arithmetic come from the `sigil-names` crate
//! (vendored realm-sigil) — this module no longer holds a word list of its own.
//!
//! WHY derive on-device rather than bake a `&str` at build time: every node must render a *peer's*
//! name too, and the only peer identity on the wire is the 3-digit id already carried in
//! HELLO/ACK/BEACON/TIME. Deriving from that id keeps the hardware-verified frame formats
//! byte-identical — **names never go on the MESH wire** — and costs zero airtime: both ends compute
//! the same name from the same id. Pure integer math over a static `.rodata` table: no heap, no
//! stack, no crypto, no float. (Names *do* go out as strings over WiFi in HA discovery, which is a
//! budgeted 512 B packet — see `DISCOVERY_BUDGET` in `net/wifi.rs`. The old wording here said "never
//! on the wire" full stop, and that looseness is part of why the discovery budget went unwatched.)
//!
//! ## What this module guarantees, and how
//!
//! **Every one of the 256 `u8` node ids maps to a distinct name.** Enumerated over the complete
//! space during const evaluation in the crate, so a colliding namespace does not compile. This
//! replaced a hand-copied 20×20 table under which only 20 of 256 ids had a distinct noun — id9, id42
//! and id236 all rendered as bare "Herald" — and where even the full adjective+noun pair collided
//! (163 distinct of 256), so there was no guarantee to restore. There had never been one.
//!
//! ## Three namespaces, kept apart by assertion rather than intention
//!
//! | namespace | names | source | churns? |
//! |---|---|---|---|
//! | [`REALM`] (`fleet`) | a board | crate, 32×32 size-locked | renamed once, authorised |
//! | [`FORGE`] | a build | **PINNED here, never synced** | never — version names are history |
//! | [`CREATURE`] | a familiar | crate, 24×24 unlocked | renamed once |
//!
//! The `const` assertions below prove they are pairwise disjoint. That matters because this module
//! *claimed* the identity/provenance split for months with nothing checking it, and
//! `familiar/mod.rs` claimed creature names were "distinct from any node's name" while both drew
//! from the identical corpus. Three properties documented and never true — all found by writing the
//! check, none by re-reading the prose.
//!
//! ## Drift
//!
//! There is no CORPUS-DRIFT WARNING here any more because there is no copy here to drift. The
//! previous warning was also **backwards**: it named `words/realms.json` as stale when that file is
//! the current source (lexicon cut over 2026-05-07) and the generated embeds it copied from are the
//! frozen ones. `tools/sigil_vendor.sh --check` now enforces what the comment used to ask for.

/// The corpus type and the index arithmetic now come from the sigil crate rather than being
/// re-implemented here. This is the whole point of the change: there is one algorithm.
///
/// `seed_from_id` has no in-crate caller — `name_for_id` is the useful entry point — but it is
/// re-exported anyway because it is the documented off-device parity function: `docs/BUILDING.md`
/// tells a reader they can reproduce any node's name with `id * 2654435761`, and that claim should
/// resolve to a symbol in the module the docs point at, not to a private detail of a dependency.
#[allow(unused_imports)]
pub use sigil_names::{name_for_seed, seed_from_id, Realm};

/// The realm every smol unit agrees on: sigil's **`fleet`** group — node IDENTITY, 32x32,
/// size-locked, reserved words excluded, and **proven collision-free over the entire `u8` id
/// space at compile time**.
///
/// This replaced a hand-copied 20x20 `fantasy` table under which only 20 of 256 ids had a distinct
/// noun — id9, id42 and id236 all published as bare "Herald" and were indistinguishable in every
/// UI. The full adjective+noun pair collided too (163 distinct of 256), so propagating the
/// adjective alone would not have been a guarantee, merely an improvement.
pub const REALM: &Realm = sigil_names::FLEET;

/// A node's `(adjective, noun)` from its logical id — **unique for every one of the 256 ids**.
///
/// Both mesh ends call this with the id carried in the frame to get an identical name, so names
/// still never go on the mesh wire.
///
/// ⚠️ `.1` alone is NOT an identifier. 32 nouns over 256 ids forces at least 8 ids to share every
/// noun (9 in the worst case) — that is pigeonhole, not a tuning problem, and no corpus size fixes
/// it: noun-uniqueness would need 256 nouns. **Any surface too cramped for the pair must show a
/// disambiguated short form** (`noun`+id, or an adjective initial), never the bare noun. The
/// nameplate in `sigil.rs` is the only screen with room for the full pair, and it renders it.
#[inline]
pub fn name_for_id(id: u8) -> (&'static str, &'static str) {
    sigil_names::name_for_id(id, REALM)
}

/// The number of decimal digits in `id` — 1, 2 or 3.
#[inline]
const fn id_digits(id: u8) -> usize {
    if id >= 100 {
        3
    } else if id >= 10 {
        2
    } else {
        1
    }
}

/// Write a **disambiguated** short label for `id` into `w`, fitting `budget` characters:
/// the noun truncated as needed, then the id — `Vigil122`, `Aegis5`, `Ci42`.
///
/// This is the answer to the only ambiguity the unique-pair guarantee does NOT remove. 32 nouns over
/// 256 ids forces at least 8 ids (9 at worst) to share every noun; that is pigeonhole, so **no corpus
/// size fixes it** — noun-uniqueness would need 256 nouns. A 72x40 panel cannot render "Obsidian
/// Aegis", so the cramped surfaces used to render the bare noun, which is the HALF THAT IS NOT
/// UNIQUE. Three boards showing "Herald" was that choice, not a hash defect.
///
/// **The id suffix makes the label injective by construction, and — importantly — independently of
/// how brutally the noun is clipped.** Uniqueness rides entirely on the id, so the noun is there to
/// be recognisable, not to identify. `Ci42` and `Ci7` stay distinguishable even when the budget
/// leaves two characters of noun. Nouns contain no digits, so the split is unambiguous to a reader.
///
/// Budget accounting: the id takes 1-3 chars and the noun gets the remainder. A budget too small for
/// the digits alone yields the id alone, which is still correct — never a bare noun.
pub fn write_short(w: &mut impl core::fmt::Write, id: u8, budget: usize) {
    let noun = name_for_id(id).1;
    let room = budget.saturating_sub(id_digits(id));
    // char_indices, not byte slicing: the `clip()` helpers in bench.rs/rssi.rs slice bytes and are
    // safe only while the corpus stays ASCII. This one cannot be made to panic by a word change.
    let cut = match noun.char_indices().nth(room) {
        Some((i, _)) => i,
        None => noun.len(),
    };
    let _ = write!(w, "{}{}", &noun[..cut], id);
}

/// A stack-allocated disambiguated short label — [`write_short`] for the many call sites that want a
/// `&str` rather than a formatter. No heap, no static: 16 bytes on the caller's frame, which is why
/// this is a returned value rather than a `&'static str` (there is no static to point at — the label
/// depends on the id).
pub struct ShortName {
    buf: [u8; Self::CAP],
    len: usize,
}

impl ShortName {
    /// Longest possible label: 8-char noun (the corpus max) + 3 id digits, rounded up.
    const CAP: usize = 12;

    pub fn as_str(&self) -> &str {
        // Only ever written through `write_short`, which emits ASCII noun bytes + ASCII digits.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

impl core::fmt::Write for ShortName {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < Self::CAP {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// The disambiguated short label for `id`, fitting `budget` characters. See [`write_short`] for why
/// the id suffix — not the noun — is what makes this unique.
pub fn short_name(id: u8, budget: usize) -> ShortName {
    let mut s = ShortName { buf: [0; ShortName::CAP], len: 0 };
    write_short(&mut s, id, budget.min(ShortName::CAP));
    s
}

/// smol byte-clips node nouns on the 72x40 OLED. Uniqueness no longer depends on this — see
/// [`write_short`], where the id carries it — but a truncation that collides two nouns still makes
/// the label harder to READ, so it stays asserted. Unlike the pair's uniqueness this depends on the
/// LETTERS rather than the counts, so it cannot be inferred and has to be checked.
const _: () = assert!(
    sigil_names::nouns_distinct_at(REALM, 5),
    "two fleet nouns become identical when clipped to 5 chars — the narrowest display budget in \
     smol (bench.rs time-source column). Rename a word in lexicon's fleet group."
);
const _: () = assert!(
    sigil_names::nouns_distinct_at(REALM, 6),
    "two fleet nouns collide when clipped to 6 chars (finder.rs hero row, bench.rs own-status)."
);
const _: () = assert!(
    sigil_names::nouns_distinct_at(REALM, 8),
    "two fleet nouns collide when clipped to 8 chars (bench.rs peer row, finder.rs peer rows)."
);

/// IDENTITY must not share a word with PROVENANCE. This module claimed that separation from the day
/// it was written — "a build's identity reads in a DELIBERATELY different vocabulary from a node's
/// name" — and nothing checked it.
///
/// The property was not free, either: upstream's post-cutover `forge` shared the noun `ember` with
/// `fleet`, so one word could name a build *and* a board. Writing this assertion is what found it
/// (fixed in lexicon `e690207`, `ember` → `quench`). smol was insulated only by accident, because it
/// pins the 20-word forge table — which makes this a second, independent reason the pin is
/// load-bearing: a future FORGE sync would not merely rename past builds, it could collapse the
/// build/board distinction. Now the build fails instead.
const _: () = assert!(
    sigil_names::realms_disjoint(REALM, &FORGE),
    "a node-identity word now collides with a firmware-VERSION word — a board and a build could \
     render the same name, which is the ambiguity the two-realm split exists to prevent. Do NOT \
     resolve this by relaxing the check; change a word."
);

/// The familiar's creature namespace — a THIRD namespace, neither board nor build.
///
/// Was the pinned `fantasy` corpus, which overlapped node identity on **14 nouns**, so a familiar
/// could render a board's name. The known-gap note that stood here is now the assertion below:
/// lexicon `e690207` gave creatures their own reserved-disjoint 24×24 group.
///
/// Deliberately NOT size-locked. A creature seeds from an arbitrary `u32`, so its domain is ~2³² and
/// injectivity is impossible by pigeonhole — the 32-lock on node identity exists only to make a
/// 256-element space injective, and copying it here would be applying a rule past the premise that
/// justifies it. Sized for sound instead (576 combos ≈ 7.5% chance any two of ten concurrent
/// creatures share a noun, and a collision is cosmetic because a creature is never an addressing
/// key).
pub const CREATURE: &Realm = sigil_names::CREATURE;

/// A familiar must never wear a board's name. Asserted rather than intended — `familiar/mod.rs`
/// carried the comment "distinct from any node's name" for months while both namespaces drew from
/// the *identical* corpus, so this is the third documented-but-never-true property found today, and
/// the only durable fix is to make the claim unrepresentable when false.
const _: () = assert!(
    sigil_names::realms_disjoint(REALM, CREATURE),
    "a creature word collides with a node-identity word — a familiar could render a board's name. \
     Fix the vocabulary in lexicon's creature group; do not relax this."
);

/// The `forge` realm for FIRMWARE VERSION names — **deliberately PINNED, never synced.**
///
/// A build's name (e.g. "Molten Crucible") reads in a different vocabulary from a node's name so
/// provenance is never confused with identity at a glance (ota-ux-design.md §1). That separation is
/// now enforced one level further out: `fleet` and `forge` share no words, and the reserved set
/// (which contains `forge`) stops a node ever being named after the version namespace.
///
/// ⚠️ **Do not source this from upstream.** `words/realms.json`'s `forge` took a different cutover
/// path and is a **non-superset 14/14** corpus, so adopting it changes the modulus 20 -> 14 and
/// **renames every past build** — v345 stops being "Furnace". Version names are historical record:
/// they appear in commits, in memories, and in JP's speech ("Bellows" is build 341). Renaming them
/// is data loss, not a refresh. Node identity was authorised to churn once; provenance was not.
///
/// This is also why the vendored `sigil-names` crate must never enable
/// `divergent-themed-realms` — that feature exists precisely to make reaching a moved themed
/// corpus impossible by accident.
pub static FORGE: Realm = Realm {
    name: "forge@pinned-20x20",
    adjectives: &[
        "Annealed", "Bolted", "Carbonized", "Dense", "Electric", "Flux", "Galvanized",
        "Hardened", "Ignited", "Joined", "Keen", "Laminated", "Molten", "Nitrided", "Oxidized",
        "Pressed", "Quenched", "Riveted", "Sintered", "Tempered",
    ],
    nouns: &[
        "Anvil", "Bellows", "Crucible", "Die", "Engine", "Furnace", "Gear", "Hammer", "Ingot",
        "Jig", "Kiln", "Lathe", "Mandrel", "Nozzle", "Oven", "Piston", "Quench", "Rivet", "Spark",
        "Tongs",
    ],
};


/// #218: the FORGE version name for a build NUMBER — the sigil word for `n`. Uses direct
/// modulo (NOT sigil's `name_for_seed` `>>8` formula): build numbers are small + sequential
/// (256..=511 all shift to one noun under `>>8`), so we index the corpus directly so every
/// build gets its OWN word and consecutive builds differ — 341 = Bellows, 342 = Crucible.
/// The adjective advances once per full noun cycle (a slow "generation" marker). `.0` =
/// adjective, `.1` = noun. Names ANY build (e.g. an OTA announce target), not just our own.
// Fed by the wifi/espnow ota-state title path; dead in a no-radio default build (same
// rationale as the toast producer API + the CFG_KEY_* consts).
#[allow(dead_code)]
pub fn version_name_for(n: u32) -> (&'static str, &'static str) {
    let nouns = FORGE.nouns;
    let adjs = FORGE.adjectives;
    let noun = nouns[(n as usize) % nouns.len()];
    let adj = adjs[((n / nouns.len() as u32) as usize) % adjs.len()];
    (adj, noun)
}

/// The RUNNING firmware's build number — the committed ratchet (`BUILD_NUMBER`, build.rs).
pub fn build_number() -> u32 {
    env!("BUILD_NUMBER").parse().unwrap_or(0)
}

/// True for a dev/canary build (build.rs sets `BUILD_DEV=1` unless the ship pipeline
/// declared a release via `SMOL_RELEASE=1`).
#[allow(dead_code)] // consumed by `write_version` (wifi/espnow ota-title path)
pub fn version_is_dev() -> bool {
    env!("BUILD_DEV") == "1"
}

/// The RUNNING firmware's magical VERSION name (FORGE realm), sigil-mapped from the build
/// NUMBER. `.0` = adjective, `.1` = noun; the OLED/UI shows the noun handle.
pub fn version_name() -> (&'static str, &'static str) {
    version_name_for(build_number())
}

/// Write the running version DISPLAY into `w`: `v342 Bellows` (release) or
/// `v342+dev.25f756a Bellows` (dev/canary — honest, can't masquerade as the release).
/// Display only — version *comparisons* stay numeric via [`build_number`].
// Fed by the wifi/espnow ota-state title path; dead in a no-radio default build.
#[allow(dead_code)]
pub fn write_version(w: &mut impl core::fmt::Write) {
    let _ = write!(w, "v{}", env!("BUILD_NUMBER"));
    if version_is_dev() {
        let _ = write!(w, "+dev.{}", env!("BUILD_HASH"));
    }
    let _ = write!(w, " {}", version_name().1);
}
