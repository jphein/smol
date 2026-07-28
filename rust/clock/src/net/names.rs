//! Deterministic magical node names — a faithful `no_std` port of realm-sigil's
//! `GenerateName` (github.com/jphein/realm-sigil). A node's `(adjective, noun)`
//! matches `sigil.generate_name(hex(seed), realm)` in Go/Python/JS for any `u32`
//! seed, so any node's name is reproducible off-device (see the parity snippet in
//! research §6).
//!
//! WHY derive on-device (not bake a `&str` at build time): every node must render
//! a *peer's* name too, and the only peer identity on the wire is the 3-digit id
//! already carried in HELLO/ACK/BEACON/TIME. Deriving names from that id keeps the
//! hardware-verified frame formats byte-identical — **names NEVER go on the wire**
//! — and costs zero airtime: both mesh ends compute the same name from the same
//! id. It is pure integer math over a static string table (no heap, no crypto, no
//! float), so it compiles into every build; our own name needs no radio at all.
//!
//! ⚠️ CORPUS-DRIFT WARNING — pinned deliberately (research
//! `scratch/smol/nebula-magical-names.md` §2 verified three *different* word lists
//! exist). This table is copied VERBATIM from sigil's GENERATED embeds
//! (`go/realms.go` == `python/realm_sigil/realms.py` == `js/realms.js`; all three
//! byte-identical, 20 adjectives / 20 nouns per realm). It is NOT sigil's
//! `words/realms.json` (stale: 28/25 for fantasy) and NOT lexicon's vocabularies
//! (the lexicon→sigil cutover is designed but unimplemented as of 2026-07). If
//! sigil re-runs its word-sync, or that cutover lands, this corpus — and therefore
//! every node's name — will change. Re-copy from sigil's generated source if you
//! ever want to track it; otherwise these names are frozen here on purpose.
//!
//! Only the `fantasy` realm is embedded (the locked realm for smol). The other six
//! (tarot / oracle / void / forge / signal / stellar, 20/20 each) are reproduced
//! verbatim in research §7 — paste a realm's table and repoint [`REALM`] to switch
//! the whole mesh at once. (The MAC-seed variant `seed_from_mac`, research B2, is
//! likewise there if zero-config per-chip naming is ever wanted; smol is locked to
//! id-seeding so it is omitted here to keep the module warning-free.)

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

/// smol byte-clips node nouns to 5..=8 characters on the 72x40 OLED (`bench.rs`, `finder.rs`,
/// `hunt.rs`, `watch.rs`). A truncation that collides two nouns would re-create exactly the
/// ambiguity the unique pair removes, and unlike the pair's uniqueness this depends on the LETTERS
/// rather than the counts — so it cannot be inferred and has to be asserted.
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

/// IDENTITY must not share a word with PROVENANCE. This module has claimed that separation since it
/// was written — "a build's identity reads in a DELIBERATELY different vocabulary from a node's
/// name" — and until now nothing checked it. A claim in a doc comment is not a guarantee; upstream's
/// post-cutover `forge` corpus in fact shares the noun `ember` with `fleet`, so the property is NOT
/// free.
///
/// It holds here because smol pins the 20-word forge table (see [`FORGE`]) — so this assertion is
/// simultaneously the guarantee and a second, independent reason the pin is load-bearing: syncing
/// FORGE from upstream would not merely rename past builds, it would make a build and a board share
/// a name. The build would fail rather than let that ship.
const _: () = assert!(
    sigil_names::realms_disjoint(REALM, &FORGE),
    "a node-identity word now collides with a firmware-VERSION word — a board and a build could \
     render the same name, which is the ambiguity the two-realm split exists to prevent. Do NOT \
     resolve this by relaxing the check; change a word."
);

/// The familiar's creature namespace overlaps node identity on 14 nouns, so this assertion is
/// deliberately the WEAKER `nouns_distinct_at`-style check it can currently pass... except it
/// cannot, so it is written as an explicit known-gap note rather than a disabled assertion:
///
/// ```text
/// assert!(realms_disjoint(REALM, &FANTASY))   // FAILS TODAY — 14 shared nouns
/// ```
///
/// A creature can therefore share a board's name. Pre-existing (creatures and nodes drew from the
/// same corpus before this change), owned by nebula-scribe, and fixed upstream by giving creatures
/// their own reserved-disjoint `creature` group — at which point this comment becomes the assertion
/// above it. Recorded here so the gap is a scheduled decision, not an oversight nobody wrote down.
const _: () = ();

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

/// The old `fantasy` corpus, kept ONLY for the familiar's creature names (`familiar/mod.rs`), which
/// seed from a per-creature `u32` rather than a node id. Pinned for the same reason as FORGE: a
/// creature's name is a thing JP has already seen, and this is not the namespace he authorised
/// renaming.
///
/// ⚠️ **Known taxonomy smell, deliberately NOT fixed here.** Creatures are a third namespace, and
/// this corpus overlaps `fleet` on 14 nouns (Aegis, Dominion, Ember, Grimoire, Insignia, Jewel,
/// Keystone, Lantern, Monolith, Nexus, Pinnacle, Quartz, Relic, Throne) — so a creature can share a
/// name with a node. That was already true before this change (creatures and nodes both drew from
/// `fantasy`), so repointing it would be a behaviour change smuggled into a refactor. Filed rather
/// than silently altered: creatures want their own reserved-disjoint group in lexicon, the same way
/// `fleet` got one.
pub static FANTASY: Realm = Realm {
    name: "fantasy@pinned-creatures",
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
