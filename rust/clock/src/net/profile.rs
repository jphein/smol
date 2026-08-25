//! #352 board VARIANT identity — what this board **is**, discovered at runtime.
//!
//! ## Orthogonal to [`super::target::TargetId`], and it must stay that way
//!
//! | | answers | decided | on the wire |
//! |---|---|---|---|
//! | [`TargetId`](super::target::TargetId) (#349) | what an image is **FOR** | build time | **yes** |
//! | [`BoardProfile`] (#352) | what this board **IS** | runtime, I2C probe | **never** |
//!
//! `target.rs` already states the rule this module has to respect: *"Board VARIANT is
//! deliberately absent. OLED vs SuperMini is detected at runtime."* Folding the two together
//! breaks #349 outright — a `TargetId` must be decidable **from an image alone**, because that
//! is the whole OTA suitability guard, and a board variant cannot be read out of a binary. So:
//! compose, never merge. They share exactly one field, `chip`, and this module **borrows** it
//! rather than re-deriving it — see below, because re-deriving it is the bug this issue fixes.
//!
//! ## The defect: the chip axis was stated twice, by two different mechanisms
//!
//! Before #352, `wifi.rs` chose its Home Assistant `model` label with
//! `#[cfg(target_feature = "a")]` — the RISC-V atomics extension, true on the C6's
//! `riscv32imac` and false on the C3's `riscv32imc`. Meanwhile `target.rs` already carried
//! [`SELF_CHIP`](super::target::SELF_CHIP), a positive three-way value parsed by `build.rs`
//! from the target triple, with a const-assert that it is not `CHIP_UNKNOWN`.
//!
//! Two derivations of one fact is the shape that rots — but this one was worse than untidy,
//! because the cfg pair is not a discriminant, it is **a negation that only means "C3" while
//! exactly two chips exist**. `target_feature = "a"` is a RISC-V feature; an Xtensa target does
//! not have it, so `xtensa-esp32s3` takes the `not(...)` arm and an S3 would announce itself as
//! `smol ESP32-C3 OLED`. The old comment anticipated the S3 but predicted the wrong failure —
//! it says *"no cfg arm here can even compile for it yet"*, and in fact the default arm compiles
//! perfectly and quietly picks the wrong silicon's label. Compile error vs silent lie is the
//! entire difference, and `model` is the first field anyone checks: that same comment records
//! two C6 watches being misidentified as C3 fleet nodes, twice, for exactly this reason.
//!
//! So the chip axis here is a **value** ([`super::target::SELF_CHIP`]), never a cfg. Two things
//! follow that the cfg form could not give: the S3 arm exists and is correct *before* the chip
//! is buildable, and the whole mapping is a pure function of `(chip, has_display)` — so
//! `experiments/profile_verify` tests every chip on the host, including chips this tree cannot
//! yet compile for. A label that cannot be exercised until the hardware lands is a label that
//! will be wrong when it does.
//!
//! ## The const-evaluability constraint — read before changing a signature
//!
//! [`SELF_EXTRAS_MAX`] feeds `DISCOVERY_CFG_MAX_UPLINK`, which is `const`-asserted against
//! `DISCOVERY_BUDGET`. `encode_publish` returns `None` **silently** when a config will not fit,
//! so that assert is the only thing between a long label and a node that boots, joins, publishes
//! telemetry, and simply never appears in Home Assistant. Everything on the path from a label to
//! that number is therefore `const fn` over `&'static str`. If a future edit makes the fragment
//! runtime-built, the fit proof does not get weaker — it **ceases to exist**.

use super::target;

/// Which physical board this firmware woke up on.
///
/// `chip` is one of `target::CHIP_*`; `has_display` is the boot-time I2C fact. Deliberately a
/// plain value type with a `const fn` constructor so the whole mapping stays const-evaluable
/// and host-testable for chips that are not yet buildable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BoardProfile {
    pub chip: u8,
    /// Did an OLED answer on I2C at boot? False on a SuperMini **or** a dead panel — the
    /// firmware cannot tell those apart and does not claim to.
    pub has_display: bool,
}

impl BoardProfile {
    pub const fn new(chip: u8, has_display: bool) -> Self {
        Self { chip, has_display }
    }

    /// The Home Assistant device-block fragment for this board: `model` + `manufacturer`.
    ///
    /// ONE table, exhaustive over the fleet taxonomy, replacing the three cfg-gated
    /// `DEVICE_EXTRAS_*` constants. `const fn` is load-bearing — see the module note on
    /// [`SELF_EXTRAS_MAX`].
    ///
    /// `has_display` is consulted only for the C3, and that is a statement about the hardware
    /// rather than an oversight: the C3 image runs on TWO boards ($2.76 OLED, $1.00 screenless
    /// SuperMini) and the panel is the only thing that distinguishes them, whereas the C6 watch
    /// and the S3 Ember are single-variant products whose screen is part of the product. If a
    /// screenless C6 ever exists, it gets an arm here and a case in `profile_verify`.
    pub const fn ha_device_extras(&self) -> &'static str {
        match (self.chip, self.has_display) {
            (target::CHIP_ESP32C3, true) => {
                ",\"model\":\"smol ESP32-C3 OLED\",\"manufacturer\":\"jphein\""
            }
            (target::CHIP_ESP32C3, false) => {
                ",\"model\":\"smol ESP32-C3 SuperMini\",\"manufacturer\":\"jphein\""
            }
            (target::CHIP_ESP32C6, _) => {
                ",\"model\":\"smol ESP32-C6 Watch\",\"manufacturer\":\"jphein\""
            }
            // #331: the Ember satellite (ember.realm.watch, 2.8" touchscreen). Not buildable
            // from this tree yet — esp-hal is pinned to esp32c3 — which is precisely why the
            // arm is written now and covered by a host case now. Under the old cfg form this
            // chip silently inherited the C3's label.
            (target::CHIP_ESP32S3, _) => {
                ",\"model\":\"smol ESP32-S3 Ember\",\"manufacturer\":\"jphein\""
            }
            // #388: the NM-CYD-C5 (2.8" CYD, ESP32-C5). Single-variant like the C6/S3 — the
            // screen is part of the product. 52 B, below every existing target's fragment, so
            // it can never set SELF_EXTRAS_MAX. First heard on the mesh as peer 176 on
            // 2026-08-24 (from its own spike firmware); this arm is what a SMOL image on that
            // silicon will announce, and what the phase-1 spike's hand-published discovery
            // block must match byte-for-byte.
            (target::CHIP_ESP32C5, _) => {
                ",\"model\":\"smol ESP32-C5 CYD\",\"manufacturer\":\"jphein\""
            }
            // Unreachable in a firmware build — the assert below refuses it — but a `const fn`
            // match must be total. Deliberately the SHORTEST arm so it can never be the one
            // that sets the budget maximum, and deliberately not a plausible-looking label:
            // if this ever reaches a dashboard it should read as a bug, not as a device.
            _ => ",\"model\":\"smol\",\"manufacturer\":\"jphein\"",
        }
    }
}

// ── the firmware-only half ────────────────────────────────────────────────────────────────
// Everything above is PURE and compiles off-target; everything below reaches for
// `target::SELF_CHIP`, which is itself `wifi`-gated because it is built from `build.rs`'s
// `env!("SMOL_CHIP_ID")` stamp. Same split, for the same reason, as `net/target.rs`: it lets
// `experiments/profile_verify` `#[path]`-include this exact file WITHOUT the feature and test
// the mapping for every chip — including the S3, which this tree cannot build at all. A label
// that cannot be exercised until the hardware lands is a label that will be wrong when it does.

/// This build's profile for a given runtime display fact. The chip half is compile-time.
#[cfg(feature = "wifi")]
pub const fn for_self(has_display: bool) -> BoardProfile {
    BoardProfile::new(target::SELF_CHIP, has_display)
}

/// FAIL CLOSED on a chip with no label of its own.
///
/// `target.rs` already asserts `SELF_CHIP != CHIP_UNKNOWN`, which catches a triple `build.rs`
/// could not parse. This catches the other direction: a chip id that IS recognised upstream but
/// has no arm here yet, which would otherwise fall to the `_` arm and ship a device advertising
/// itself as plain `smol`. Adding a chip to `build.rs` now breaks the build here until its label
/// is written — which is the only moment anyone is thinking about it.
#[cfg(feature = "wifi")]
const _: () = assert!(
    target::SELF_CHIP == target::CHIP_ESP32C3
        || target::SELF_CHIP == target::CHIP_ESP32C6
        || target::SELF_CHIP == target::CHIP_ESP32S3
        || target::SELF_CHIP == target::CHIP_ESP32C5,
    "this chip has no BoardProfile label — add an arm to ha_device_extras() and a case to \
     experiments/profile_verify before building for it, or the board will announce itself to \
     Home Assistant as plain \"smol\""
);

/// The longest fragment THIS build can emit at runtime, over the profile set rather than over a
/// list of named constants — so a variant added to the match is covered without anyone
/// remembering to extend a second expression.
///
/// Derived from the strings, never hand-maxed: the property `wifi.rs` had before #352 and the
/// reason a reworded label re-derives its own fit proof instead of silently outgrowing a copied
/// literal.
#[cfg(feature = "wifi")]
pub const SELF_EXTRAS_MAX: usize = {
    let with = for_self(true).ha_device_extras().len();
    let without = for_self(false).ha_device_extras().len();
    if with > without {
        with
    } else {
        without
    }
};
