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

/// A realm's word corpus. `name = "{adjectives[seed % |A|]} {nouns[(seed>>8) % |N|]}"`.
pub struct Realm {
    pub adjectives: &'static [&'static str],
    pub nouns: &'static [&'static str],
}

/// The `fantasy` realm — verbatim from sigil's generated corpus (20 adj / 20 noun).
pub static FANTASY: Realm = Realm {
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

/// The realm every smol unit agrees on (sigil's `realm` string). LOCKED to fantasy
/// for smol — this const is the single switch-point: repoint it (and paste the
/// realm's table from research §7) to re-theme every node's name at once.
pub const REALM: &Realm = &FANTASY;

/// Knuth multiplicative-hash constant (2^32 / φ, rounded to odd). Spreads an 8-bit
/// id across all 32 seed bits — see [`seed_from_id`].
const GOLDEN_U32: u32 = 2_654_435_761;

/// Faithful port of sigil's index math: `adj = A[seed % |A|]`,
/// `noun = N[(seed >> 8) % |N|]`. Uses the list LENGTH (not a hard-coded 20),
/// exactly like sigil's Go/Python source — the README's "% 20" only happens to be
/// right because the generated lists are 20 long. `(seed >> 8)` still leaves 24
/// bits for the noun. Matches sigil for any `u32` seed.
#[inline]
pub fn name_for_seed(seed: u32, realm: &'static Realm) -> (&'static str, &'static str) {
    let adj = realm.adjectives[(seed as usize) % realm.adjectives.len()];
    let noun = realm.nouns[((seed >> 8) as usize) % realm.nouns.len()];
    (adj, noun)
}

/// Spread an 8-bit id across 32 bits so BOTH the adjective (`% |A|`) and the noun
/// (`(>>8) % |N|`) vary between adjacent ids. WITHOUT this every id < 256 has
/// `(seed >> 8) == 0` and shares noun index 0 — all nodes would get the same noun.
/// This is the documented off-device parity function (research §6):
/// `(id * 2654435761) & 0xFFFFFFFF`, which on-device is exactly `wrapping_mul`.
#[inline]
pub fn seed_from_id(id: u8) -> u32 {
    (id as u32).wrapping_mul(GOLDEN_U32)
}

/// A node's `(adjective, noun)` from its logical id. Both mesh ends call this with
/// the id carried in the frame to get an identical name. `.1` is the noun (the
/// OLED handle, always short for fantasy); `.0` is the adjective (logs only).
#[inline]
pub fn name_for_id(id: u8) -> (&'static str, &'static str) {
    name_for_seed(seed_from_id(id), REALM)
}

/// The `forge` realm — verbatim from sigil's generated corpus (20 adj / 20 noun),
/// same source as FANTASY (research `nebula-magical-names.md` §7). Used for the
/// FIRMWARE VERSION name so a build's identity (e.g. "Molten Crucible") reads in a
/// DELIBERATELY different vocabulary from a node's fantasy name — provenance is
/// never confused with identity at a glance (ota-ux-design.md §1).
pub static FORGE: Realm = Realm {
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

/// Parse leading hex digits of `s` into a `u32` seed (sigil's native version path:
/// the git short hash IS the seed). Non-hex terminates parsing, so a non-hex build
/// tag like `"dev"` seeds deterministically off its leading hex (`"dev"` → `0xDE`)
/// — every dev build then shares one forge name, which is the honest signal.
fn parse_hex_seed(s: &str) -> u32 {
    let mut seed: u32 = 0;
    for &b in s.as_bytes() {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => break, // non-hex char ends the seed (e.g. the 'v' in "dev")
        };
        seed = seed.wrapping_mul(16).wrapping_add(d as u32);
    }
    seed
}

/// The firmware's magical VERSION name — sigil's native path: seed from the git
/// short hash (`BUILD_HASH`, from `build.rs`), rendered in the FORGE realm. Both
/// the seed and the corpus are compile-time, so this const-folds to two
/// `&'static str` exactly like `name_for_id(NODE_ID)` (the seed here is an `env!`
/// literal). `.0` = adjective, `.1` = noun. Full name in the boot log; the OLED
/// shows the noun handle, matching the rest of the UI.
pub fn version_name() -> (&'static str, &'static str) {
    name_for_seed(parse_hex_seed(env!("BUILD_HASH")), &FORGE)
}
