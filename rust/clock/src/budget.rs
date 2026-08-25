//! #348 — per-chip memory budgets, **declared as data**, with heavy features predicated on them.
//!
//! ## Why this exists
//!
//! #347 took the Bard off the C3 fleet image by deleting one word from
//! `REPRO_FLEET_FEATURES` in `tools/repro_build.sh`. Nothing stops someone typing it back.
//! The only mechanism that would notice is `repro_stack_check`, which runs at *package*
//! time — the latest possible moment, after every build in CI has already gone green.
//! Worse, the relationship it is checking ("the Bard's `.bss` comes out of the runtime
//! stack") existed nowhere but prose: a Cargo.toml comment, a ROADMAP block, three issue
//! bodies. A capability that is not declared cannot be queried — OpenWrt learned that the
//! expensive way when its low-RAM deprecation sweep had to hand-audit hundreds of devices
//! against a wiki table because RAM size was never a field
//! ([openwrt#12672](https://github.com/openwrt/openwrt/pull/12672)).
//!
//! So: declare it, and let the compiler refuse.
//!
//! ## The model (OpenWrt's, not WLED's)
//!
//! OpenWrt predicates a feature on a **declared capability**, never on a chip name —
//! `include/target.mk:94` drops `procd-ujail` from any subtarget that declared
//! `small_flash`, and nothing in that rule names a SoC. It also keeps **flash and RAM as
//! two independent axes** (`FEATURES += low_mem small_flash`), because they are two
//! different scarcities. The Bard is short on one and comfortable on the other, and a
//! model that conflated them would report the wrong reason.
//!
//! The alternatives, both rejected:
//!   * **WLED** — a commented-out line with the reason in prose
//!     (`;; pushed program flash size over the limits`). Nothing fails if someone
//!     uncomments it. That is the status quo #347 left behind, written down.
//!   * **Tasmota** — a separate named build (`tasmota-lite`). Costs a whole artifact, an
//!     OTA URL, and a taxonomy the user has to navigate.
//!
//! Full study: `scratch/parity/multitarget-prior-art.md`.
//!
//! ## ⚠️ PLACEMENT — this module moves to `smol-core` (#347 Phase 2)
//!
//! It belongs in the core crate, not in `clock`. If the memory math lives here, the Bard
//! device firmware and the esp32c6-watch each re-derive their own copy — which is exactly
//! the divergence the core extraction exists to prevent (they have already hand-copied
//! `names.rs` and `sigil.rs` between them). Core is not extracted yet, so this module is
//! written to **move without edits**:
//!
//!   * no `use crate::…`, no `super::`, no esp-hal, no `alloc`, no `std`;
//!   * every item `pub`;
//!   * the only crate-local coupling is the `#[cfg(feature = "…")]` on the assertions at
//!     the bottom, which is the one part that stays behind in whichever crate owns the
//!     feature flags.
//!
//! Moving it is `git mv` + `pub use smol_core::budget::*;`. Keep it that way.
//!
//! ## Relationship to the other guards
//!
//! | guard | when it fires | what it proves |
//! |---|---|---|
//! | **this module** | compile | the declared cost of a feature fits the declared budget |
//! | `repro_stack_check` | package | the *linked* `.stack` region clears the floor |
//! | `--features stack-paint` | runtime, on a board | the *actual* high-water under live radio |
//!
//! They are not redundant and none replaces another. This one is a **declaration checked
//! against a declaration** — it is only as honest as the numbers below, which is why every
//! one of them carries its provenance. `repro_stack_check` measures the real link and
//! stays the authority; if the two ever disagree, the ELF is right and this file is stale.

// This module is DATA. Its consumers are (a) the cfg-gated const-assertions at the bottom,
// (b) `tests/budget.rs`, and (c) — after #347 Phase 2 — other crates. Under a firmware
// build with no predicated feature enabled, nothing in it is referenced, and `clippy -D
// warnings` runs on every tier since #343.
#![allow(dead_code)]

/// What a chip can afford. One `const` per chip; see [`ESP32C3`].
///
/// **Flash and DRAM are separate fields on purpose** (OpenWrt's `small_flash` vs `low_mem`).
/// A feature can be comfortable on one axis and impossible on the other — the Bard is
/// exactly that case — and a verdict that cannot say which one is not actionable.
pub struct ChipBudget {
    /// Chip identifier, for messages and for whoever cross-references the artifact.
    ///
    /// Deliberately a `&'static str` and **not** a new enum: #349 is defining a structured
    /// `TargetId` for the OTA manifest, and this field should become that type rather than
    /// compete with it.
    pub chip: &'static str,

    /// DRAM available to a predicated feature's statics **plus** the runtime stack, measured
    /// with the canonical fleet tier linked and no predicated feature in it.
    ///
    /// This is the linked `.stack` region of the baseline image, because on this platform
    /// `.stack` *is* the leftover: the linker hands `.stack` whatever DRAM remains after
    /// `.bss`/`.data` and **silently shrinks it** rather than failing, which is why a
    /// successful link has never been evidence of a runnable image (#300).
    pub free_dram_bytes: u32,

    /// The minimum linked `.stack` region an image may ship with — the floor
    /// `repro_stack_check` enforces at package time.
    pub stack_floor_bytes: u32,

    /// The OTA app partition a single image must fit in (`partitions-ota.csv`).
    pub app_slot_bytes: u32,

    /// Image size of the canonical fleet tier with no predicated feature — the flash-axis
    /// counterpart to [`Self::free_dram_bytes`], and measured from the same baseline.
    pub baseline_image_bytes: u32,
}

/// What a predicated feature costs, as an ELF-section delta against the same baseline the
/// [`ChipBudget`] was measured from. Both fields come from `readelf -SW`, never an estimate.
pub struct FeatureCost {
    pub feature: &'static str,
    /// `.bss` + `.data` (+ alignment) delta — DRAM that comes straight out of the stack.
    pub dram_bytes: u32,
    /// `.rodata` + `.text` delta — flash, which on this platform is XIP and costs no DRAM.
    pub flash_bytes: u32,
}

impl ChipBudget {
    /// DRAM a predicated feature may spend before the stack region breaks the floor.
    pub const fn dram_headroom(&self) -> u32 {
        self.free_dram_bytes.saturating_sub(self.stack_floor_bytes)
    }

    /// Flash a predicated feature may spend before the image overruns the OTA slot.
    pub const fn flash_headroom(&self) -> u32 {
        self.app_slot_bytes.saturating_sub(self.baseline_image_bytes)
    }

    pub const fn fits_dram(&self, cost: &FeatureCost) -> bool {
        cost.dram_bytes <= self.dram_headroom()
    }

    pub const fn fits_flash(&self, cost: &FeatureCost) -> bool {
        cost.flash_bytes <= self.flash_headroom()
    }

    /// Both axes. Kept separate from the two above so a failing assertion can name the axis.
    pub const fn fits(&self, cost: &FeatureCost) -> bool {
        self.fits_dram(cost) && self.fits_flash(cost)
    }

    /// Bytes by which `cost` overruns the DRAM headroom; `0` when it fits.
    pub const fn dram_shortfall(&self, cost: &FeatureCost) -> u32 {
        cost.dram_bytes.saturating_sub(self.dram_headroom())
    }

    /// Bytes by which `cost` overruns the flash headroom; `0` when it fits.
    pub const fn flash_shortfall(&self, cost: &FeatureCost) -> u32 {
        cost.flash_bytes.saturating_sub(self.flash_headroom())
    }
}

// ── The declared budgets ────────────────────────────────────────────────────────────────
//
// PROVENANCE. Every number below was measured, and each says where. Re-measure rather than
// trusting the comment; a number copied forward untested is how the stack floor once ended
// up at 12,288 B.

/// **How a chip's declared stack floor was arrived at — the number's epistemic status, as DATA.**
///
/// #413 phase 2. The three floors on this fleet are three different KINDS of number, and until
/// this enum existed that difference lived only in prose:
///
/// | chip | floor | how |
/// |---|---|---|
/// | C3 | 74,208 | [`Derived`](Self::Derived) — measured high-water × 4/3, compile-asserted |
/// | C6 | 71,680 | [`BootAssert`](Self::BootAssert) — a firmware contract, sitting BELOW the empirical line |
/// | S3 | 72,004 | [`ObservedSufficient`](Self::ObservedSufficient) — the smallest region proven clean, because the high-water instrument does not work on that chip |
///
/// ## Why this is an enum and not a doc comment
///
/// `tools/repro_build.sh` prints the provenance beside its verdict, so an operator reading
/// `stack: 116940 B (floor 72004 B, observed-sufficient)` learns what the gate did and did not
/// prove. A doc comment cannot reach the shell; a free-text string would rot back into prose. And
/// an enum makes a fourth kind a **deliberate act in two places**: adding a variant here does not
/// teach the shell, whose mapping is explicit and FAILS CLOSED on an unknown one. That is the
/// intended friction — a new epistemic status should not be able to arrive by accident.
///
/// ## ⚠️ THE HARDENING RATCHET — the condition attached to this whole design
///
/// > when a chip gains a working high-water instrument, this gate HARDENS for that chip —
/// > derived floors become REQUIRED and observed-sufficient/boot-assert refuse; permissive only
/// > while measurement is impossible AND documented at the instrument (#398 follow-up for xtensa).
///
/// Today the packaging gate accepts every variant and merely reports it (#413 ruling). That
/// permissiveness is **not a policy** — it is a consequence of `stack-paint` being invalid on
/// xtensa, and it expires when that is fixed. What the gate protects against is silent `.stack`
/// collapse from `.bss` growth, and an [`ObservedSufficient`](Self::ObservedSufficient) floor
/// catches that perfectly well (the S3 sits at 116,940 against 72,004, so a ~40 KB regression
/// trips it). What it does NOT do is certify an absolute safety margin — hence the ratchet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FloorProvenance {
    /// Derived from the highest stack high-water ever MEASURED on hardware for this chip, with a
    /// safety multiplier, and coupled to that measurement by a compile-time assertion. The strong
    /// form: the floor cannot drift from its own derivation without the build failing.
    Derived,
    /// The smallest `.stack` region PROVEN sufficient by observation — a tier that ran clean — with
    /// **no high-water number available**, because the measuring instrument does not work on this
    /// chip. Weaker than [`Derived`](Self::Derived) in a specific way: it says "this much was
    /// enough for what we ran", not "this much is enough".
    ObservedSufficient,
    /// A floor the FIRMWARE asserts at boot — a contract rather than a measurement. Note this one
    /// can sit BELOW the empirically-established line (the C6 does, by ~1,320 B, and `budget.rs`
    /// asserts that inversion as a fact to preserve), so it is permissive in a KNOWN direction.
    BootAssert,
}

/// **The C3 stack floor — ONE definition, two languages.**
///
/// The minimum linked `.stack` region an image may ship with. Both consumers read *this*:
///
///   * the compile-time budget, via [`ESP32C3`]`.stack_floor_bytes`;
///   * the packaging gate, because **`tools/repro_build.sh` parses the line below**
///     (`repro_stack_floor`) instead of carrying its own copy.
///
/// Until #348-followup they were two constants — 73,728 in the gate, 74,208 here — for one
/// concept, and the gate's was the stale one. Two numbers for one quantity is the drift this
/// project keeps getting bitten by; #338 collapsed the feature list the same way.
///
/// ## ⚠️ Contract with the shell parser — do not reformat this declaration
///
/// `repro_stack_floor` matches a line beginning `pub const ESP32C3_STACK_FLOOR_BYTES: u32 =`
/// and reads the integer up to the `;`, stripping `_`. So: keep it on ONE line, keep the
/// literal a plain decimal, and put any comment on its own line above. The parser **fails
/// closed** — if it cannot read a number, `repro_stack_check` refuses to measure rather than
/// falling back to a default, because a gate that silently substitutes its own number for the
/// one you edited is worse than no gate. (`REPRO_STACK_FLOOR` still overrides, for experiments.)
///
/// ## Derivation — 4/3 × the highest peak on record
///
/// **74,208 = 4/3 × 55,656 B**, and it is exact, not rounded.
///
/// | peak | where | note |
/// |---:|---|---|
/// | 54,856 B | T13 bench, #300 (id8) | the origin of the old 73,728 (4/3, rounded up to 72 KiB) |
/// | 54,960 B | T13 final-geometry, #300 | same bench, marginally higher |
/// | 55,440 B | #302 (id8), 5 byte-identical reports | endless-narration window; does not creep |
/// | **55,656 B** | **#335 (id5), crown duty, 10/10 byte-identical** | **highest on record — this one** |
///
/// The #335 run (2026-08-01) is the strongest evidence in the set: a different board under
/// live crown duty, ten byte-identical reports, and both instrument-falsification checks
/// passed. It came in **+216 B above #302 and +800 B above T13** — the old floor was derived
/// from the *lowest* of the four and was knowably 480 B too low.
///
/// **Re-derive this when the peak moves**, with `--features stack-paint` under live radio;
/// idle numbers are meaningless. A floor copied forward untested is how the last one ended up
/// at 12,288 B. And note every peak on record was measured **with the Bard narrating**, so for
/// a Bard-free image this floor is an upper bound on what is required, not a target.
pub const ESP32C3_STACK_FLOOR_BYTES: u32 = 74_208;

/// The C3's floor is [`FloorProvenance::Derived`] — the strong form, and the only one on the fleet.
///
/// It is `ESP32C3_MEASURED_PEAK_BYTES * 4 / 3` with the assertion below enforcing the coupling, so
/// the number cannot drift from the measurement it came from without the build failing. **This is
/// the shape the other two chips are missing**, and #413 phase 2 makes that absence visible rather
/// than leaving it in prose: grep for `_MEASURED_PEAK_BYTES` and there is exactly one.
///
/// ⚠️ Same one-line shell-parser contract as the floor above — `tools/repro_build.sh` matches
/// `pub const ESP32C3_STACK_FLOOR_PROVENANCE: FloorProvenance = FloorProvenance::` and reads the
/// variant up to the `;`. Keep it on ONE line.
pub const ESP32C3_STACK_FLOOR_PROVENANCE: FloorProvenance = FloorProvenance::Derived;

/// The input to that derivation: the highest stack high-water ever **measured on hardware** for
/// this chip — #335, 2026-08-01, id5 under crown duty, 10/10 byte-identical reports, both
/// instrument-falsification checks passed.
///
/// It is a const rather than a sentence so the floor can be checked against it (below) instead
/// of merely described by it. When a stack-paint run measures a higher peak, change **this**
/// first; the assertion will then tell you the floor is stale rather than leaving you to notice.
pub const ESP32C3_MEASURED_PEAK_BYTES: u32 = 55_656;

/// The floor must be at least 4/3 of the highest measured peak. `>=` rather than `==` because a
/// peak that is not divisible by 3 must round **up** — today it is exact (55,656 × 4 / 3 =
/// 74,208). This is what stops the two constants drifting apart the way the floor drifted from
/// its own derivation before #348: the old 73,728 was 4/3 × 54,856, and stayed put through two
/// higher measurements.
const _: () = assert!(
    ESP32C3_STACK_FLOOR_BYTES >= ESP32C3_MEASURED_PEAK_BYTES * 4 / 3,
    "the C3 stack floor is below 4/3 of the highest measured stack peak (src/budget.rs). \
     Either a higher peak was recorded without re-deriving the floor — raise \
     ESP32C3_STACK_FLOOR_BYTES to at least peak * 4/3, rounding UP — or the floor was lowered \
     without a measurement to justify it, which is how it once ended up at 12,288 B."
);

/// ESP32-C3 — the pinned fleet chip (RV32IMC, 4 MB flash, 400 KB SRAM).
///
/// ## `free_dram_bytes` is the WORST supported radio stack, deliberately
///
/// The C3 fleet image is mid-migration between two radio stacks and the number differs by
/// 8 KB between them:
///
/// | radio stack | canonical tier, no bard | source |
/// |---|---:|---|
/// | esp-wifi 0.15 (blocking; `main` today) | 114,648 B | measured 2026-08-01 on `a5b1312`+`1efb8b5`, `repro_build_bin` |
/// | esp-radio 0.18 (async; #233/#335) | **106,560 B** | measured 2026-08-01 on `spike/233-stack-measure` @ `2b98fba` |
///
/// Both figures carry a ~40 B per-tree term from the git-ignored provisioning files — see the
/// note on `baseline_image_bytes` below. It is four orders of magnitude below the margins here.
///
/// The declared budget takes the **minimum**, for the same reason `stack_floor_bytes` is a
/// floor over runtime paths rather than the number one run happened to produce: a capability
/// that is only true on the configuration you happen to be building today stops being a
/// guard the moment the migration lands. It is a conservative declaration — OpenWrt's
/// `small_flash` is a declared *class*, not a per-build measurement — and the cost of that
/// choice is that a feature needing between 32,352 B and 40,920 B is refused here while
/// still fitting on `main`. That is the direction to be wrong in, and #233 closes the gap.
///
/// ## `stack_floor_bytes` — see [`ESP32C3_STACK_FLOOR_BYTES`], which the shell gate parses
///
/// ⚠️ **Slack is not headroom.** The floor is 4/3 × a *measured runtime peak*, and the peak
/// on record was measured with the Bard narrating. A Bard-free peak can only be lower, but
/// nobody has taken it, so the headroom below is a **lower bound**, not a margin to spend.
pub const ESP32C3: ChipBudget = ChipBudget {
    chip: "esp32c3",
    free_dram_bytes: 106_560,
    stack_floor_bytes: ESP32C3_STACK_FLOOR_BYTES,
    // partitions-ota.csv: ota_0/ota_1 are 0x1F0000 each. `ota_publish.sh` hard-gates on this.
    app_slot_bytes: 0x001F_0000,
    // Canonical `espnow,cast,io` image (56.9% of slot), from `repro_build_bin` — the packaging
    // path, not a plain cargo build.
    //
    // ⚠️ THIS NUMBER IS NOT BYTE-STABLE ACROSS TREES, and neither is `free_dram_bytes`. Both
    // depend on `src/board.rs` and `src/secrets.rs`, which are GIT-IGNORED and provisioned
    // per-tree (`tools/ci_provision.sh` generates them from the `.example` templates). Their
    // string literals and constants land in `.rodata`/`.data`, and `.stack` is whatever DRAM is
    // left over, so both move. Measured 2026-08-01, same commit, same packaging path:
    //
    //   | provisioning                      | image     | .stack  |
    //   |-----------------------------------|-----------|---------|
    //   | this workstation's board/secrets  | 1,155,648 | 114,648 |
    //   | ci_provision.sh templates, local  | 1,155,776 | 114,640 |
    //   | GitHub Actions runner             |         — | 114,608 |
    //
    // So the spread is ~128 B of image and ~40 B of stack. docs/ota.md's 1,155,600 is inside
    // that band and is not an error — it is a different tree, which is worth knowing before
    // someone "corrects" one of these figures to match another (this file nearly did).
    //
    // It changes nothing here: the verdicts below turn on 6,720 B and 588,576 B, three to four
    // orders of magnitude above the noise. But a byte-exact constant that is not byte-stable is
    // false precision, so treat this as a REFERENCE measurement +/- ~200 B, and never as a
    // reproducibility check — `verify_image.sh` is what proves an image reproducible, and it
    // compares two builds of the SAME tree.
    baseline_image_bytes: 1_155_648,
};

// ── ESP32-C6 (the esp32c6-watch) ────────────────────────────────────────────────────────
//
// Delivered by the esp32c6-watch session 2026-08-24, measured at watch repo `a4a86a3`
// (clean tree, built via fambuild on familiar; sections from `readelf -SW`, image from
// `espflash save-image --partition-table partitions.csv`).
//
// It is declared here but NOT yet selectable by the `CHIP` ladder below — `target_feature
// = "a"` cannot tell a C5 from a C6, so the ladder still fails closed for riscv32+atomics
// until #347's chip de-pin gives it per-chip features to switch on. A row that exists but
// cannot be selected is the intended intermediate state: the measurement is banked and
// host-checked (`tests/budget.rs`) without any build silently inheriting it.

/// The empirical boot line for the watch, which sits **ABOVE** the declared floor.
///
/// `ESP32C6_WATCH.stack_floor_bytes` (71,680) is the watch's *boot assert* — the contract its
/// firmware enforces. The bracket actually walked on hardware was **61,000 B = 5/5 boot
/// panics, 73,000 B = 0/5**, so the true line lies in `(61_000, 73_000]` and this constant is
/// the conservative upper end: the lowest stack region PROVEN clean, not the lowest that
/// works.
///
/// Recorded as a const, not a sentence, because it makes [`ESP32C6_WATCH`]'s `dram_headroom()`
/// **optimistic by a known amount** — see [`ESP32C6_WATCH_HEADROOM_OVERSTATEMENT_BYTES`]. A
/// feature that lands within ~2 KB of fitting must be judged against this number, not against
/// the declared floor.
pub const ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES: u32 = 73_000;

/// By how much [`ChipBudget::dram_headroom`] overstates real safety on the watch: 1,320 B.
///
/// This is the C6's version of the trap the C3 row spent two issues learning — a floor that
/// is a *declaration* rather than a *measurement* drifts from the hardware, and the drift is
/// invisible until an image links and then dies at boot (#300). Here the drift is known and
/// signed: the declared floor is the LOW one, so the guard is the permissive direction.
pub const ESP32C6_WATCH_HEADROOM_OVERSTATEMENT_BYTES: u32 =
    ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES - ESP32C6_WATCH.stack_floor_bytes;

/// The inversion above is a FACT to preserve, not a bug to silence. If someone raises the
/// declared floor to meet the empirical line (the correct fix, once a fresh bracket justifies
/// a specific number), this assertion fires and points at the two constants that must move
/// together — the same coupling `ESP32C3_STACK_FLOOR_BYTES` has with its measured peak.
const _: () = assert!(
    ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES > ESP32C6_WATCH.stack_floor_bytes,
    "the C6 watch's empirical boot line is no longer above its declared stack floor \
     (src/budget.rs). If the floor was RAISED to meet the line, that is the intended fix — \
     delete this assertion and the overstatement const with it, and say in the commit which \
     bracket run justified the new floor. If the LINE was lowered, a fresh 0/5 bracket must \
     back it; a boot line moved without one is how a floor once ended up at 12,288 B."
);

/// ESP32-C6 as it ships on the **esp32c6-watch** (RV32IMAC, 512 KB SRAM, 6 MB OTA slots).
///
/// ## The baseline is the watch's SHIPPING default, not a stripped image
///
/// `free_dram_bytes` is `_stack_start - _bss_end` of the DEFAULT feature build — the same
/// semantic as the C3 row's "the linked `.stack` region is the leftover DRAM". The watch's
/// default features **include `tts`** (on by default since its repo's `7cfa270`), so this is
/// what the board actually runs, not a minimum.
///
/// ## ⚠️ The floor is BELOW the observed clean line
///
/// 71,680 is the boot assert; the hardware bracket says ~73,000 (see
/// [`ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES`]). So `dram_headroom()` returns 8,592 B while
/// only ~7,272 B is proven safe. Judge anything within 2 KB of fitting against the line.
///
/// ## ⚠️ `app_slot_bytes` is 6 MB only because a build.rs hook makes it so
///
/// The watch's `partitions.csv` gives `ota_0`/`ota_1` 0x600000 each, but esp-hal's generated
/// `memory.x` hardcodes a 4 MiB ROM region. The watch's `build.rs` (`widen_rom_region`, its
/// #67) rewrites it. **Without that hook an image this size does not LINK** — so convergence
/// must carry the hook or inherit the 4 MiB ceiling, and this row would then be wrong by
/// 2 MB on the flash axis. Carried as a note here because the number cannot defend itself.
///
/// ## ⚠️ Not byte-stable across trees — same class as the C3 row
///
/// The watch's `.cargo/config.toml` is git-ignored and holds per-tree WiFi/MQTT literals that
/// land in `.rodata`/`.data`. Reference measurement ± a few hundred B. The verdicts below turn
/// on thousands, so it changes nothing — but a byte-exact constant that is not byte-stable is
/// false precision, and saying so is what stops someone "reconciling" it against another doc.
///
/// ## Scarcity axis: DRAM only
///
/// Flash headroom after `widen_rom_region` is 1,622,672 B. The C6 is the mirror image of the
/// Bard's problem on the C3 (flash-comfortable, DRAM-tight) — which is why the two axes are
/// separate fields and why a verdict that could not name the axis would be useless here.
/// The C6's floor, promoted from an inline literal to a named const (#413 phase 2) so the shell
/// gate can read it the same way it reads the C3's, and so the provenance below has somewhere to
/// live that is not a paragraph.
///
/// 71,680 B is the watch firmware's **boot assert** — see `ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES`
/// and the assertion that pins the inversion: the declared floor sits ~1,320 B BELOW the line the
/// hardware bracket actually established. Permissive in a known, signed direction.
///
/// ⚠️ One-line shell-parser contract, as the C3's.
pub const ESP32C6_STACK_FLOOR_BYTES: u32 = 71_680;

/// [`FloorProvenance::BootAssert`] — a contract the firmware enforces, not a measurement.
///
/// ⚠️ There is deliberately **no** `ESP32C6_MEASURED_PEAK_BYTES` and therefore no `4/3` assertion:
/// the C6's number did not come from a high-water run. The missing assert is the point.
pub const ESP32C6_STACK_FLOOR_PROVENANCE: FloorProvenance = FloorProvenance::BootAssert;

pub const ESP32C6_WATCH: ChipBudget = ChipBudget {
    chip: "esp32c6",
    free_dram_bytes: 80_272,
    stack_floor_bytes: ESP32C6_STACK_FLOOR_BYTES,
    // partitions.csv (the watch's, NOT smol's partitions-ota.csv): ota_0/ota_1 = 0x600000.
    // Requires the `widen_rom_region` build.rs hook — see the doc note above.
    app_slot_bytes: 0x0060_0000,
    baseline_image_bytes: 4_668_784,
};

/// The budget in force for the target being compiled.
///
/// **Fail-closed by construction.** A bare-metal target with no declared budget is a
/// compile error, not an unbudgeted pass — the whole point of #348 is that an undeclared
/// capability cannot be queried, so silently answering "sure, it fits" for a chip nobody has
/// measured would reproduce the bug in a new place. When the S3 or the C6 arrives (#331),
/// the compiler will demand its row here before it will build, which is the intended
/// friction.
///
/// # The selection is now keyed on the CHIP FEATURE (#347 Part 2)
///
/// It used to be keyed on `target_arch` + `target_feature = "a"`, which was the best the tree
/// could do while the chip was hardcoded in eight dependency declarations. That ladder could
/// name the C3 (riscv32 without atomics) and could prove a build was "a C5 or a C6" — but not
/// which, because the A extension is the only thing the target cfgs expose and both chips have
/// it. So `riscv32imac` had to be a `compile_error!`, and the measured C6 row landed in Part 1
/// unreachable by construction.
///
/// The chip feature discriminates exactly, so the ladder is a lookup and the C6 row is live.
/// The data above did not move; only the selection did — which is what the old note promised
/// and is worth confirming, because "replace the selection" is the kind of change that quietly
/// becomes "adjust the numbers so it passes".
///
/// # Fail-closed, but at the RIGHT granularity — this is the part that changed shape
///
/// **Fail-closed by construction** still holds: no chip silently inherits another's measured
/// numbers. But the old ladder enforced it by refusing to compile the CRATE, and that was
/// stricter than the facts require and it blocked the de-pin's own acceptance.
///
/// `CHIP` has exactly two consumers, both `bard` predicates at the bottom of this file. So a
/// chip with no measured row does not break the crate — it breaks only the features whose
/// verdicts need a budget. Refusing the whole crate meant a C5 radio image, which asks nothing
/// of this module, could not be compiled or even type-checked until someone produced a
/// hardware measurement it had no use for. That is not caution, it is a measurement gate on the
/// wrong build, and it is the reason "port smol to the C5" could not begin.
///
/// So an unmeasured chip now selects [`UNMEASURED`] — a poison row that fits nothing — and the
/// refusal moved to a dedicated predicate beside the `bard` asserts, where it can say what is
/// actually missing. Strictly stronger, not weaker: the old ladder could only refuse chips it
/// could NAME, and would have handed a fifth riscv32imc chip the C3's row without a murmur.
/// [`CHIP_MEASURED`] is the machine-checkable form of the distinction.
#[cfg(all(target_os = "none", feature = "esp32c3"))]
pub const CHIP: ChipBudget = ESP32C3;

/// The C6 row measured in Part 1, finally reachable. See [`ESP32C6_WATCH`] — and note it is the
/// esp32c6-**watch's** shipping image, whose DRAM is already spent on a TTS stack and a display.
/// A smol C6 build is a different image and will want its own row; until it has one, this is a
/// conservative stand-in on the DRAM axis and an OPTIMISTIC one on flash (6 MB slots exist only
/// because the watch's `widen_rom_region` build.rs hook rewrites esp-hal's hardcoded 4 MiB ROM
/// region — smol does not carry that hook yet, so its C6 ceiling is 4 MiB until it does).
#[cfg(all(target_os = "none", feature = "esp32c6"))]
pub const CHIP: ChipBudget = ESP32C6_WATCH;

/// The S3 (ES3C28P, #398) — measured on unit `14:C1:9F:D1:C8:10` ("eldritch-insignia",
/// node 162), 2026-08-25, the day smol first ran on Xtensa silicon.
///
/// **`stack_floor_bytes` is an OBSERVED-SUFFICIENT line, not a high-water derivation,
/// and the difference is a measured fact about the instrument:** `stack-paint` is
/// INVALID on this chip as written. Its sentinel is trampled by boot-era machinery that
/// shares the `.stack` region (59,504 B read "used" one statement after painting, at
/// boot, before anything deep ran), and re-painting after init crashed the box into a
/// 99-boot exception loop. The `_stack_*_cpu0` symbols alias the whole-region ones
/// (readelf-verified), so a CPU-slice port is NOT the fix — understanding what actually
/// writes there is, and that is follow-up work on #398. Until then, the floor is the
/// C6's own semantics (`ESP32C6_WATCH_EMPIRICAL_BOOT_LINE_BYTES`): **72,004 B is the
/// `.stack` region of the heaviest-stack tier ever run on this unit** (`stack-paint` =
/// bard + canonical: narration + mesh relay + MQTT + the 4× display), which ran clean —
/// the smallest region PROVEN sufficient, not the smallest that works.
///
/// `free_dram_bytes` = the fleet image's `.stack` section (readelf), **re-measured
/// post-#391**: the Embassy executor costs ~20.3 KB of `.bss`, which the linker takes
/// straight out of `.stack` — the original pre-executor measurement (116,940, sha
/// `5fd23661…`) was correct for its base and stale one merge later, caught by depin3's
/// reconciliation and re-derived independently (their 96,668 vs this 96,676 = per-tree
/// provisioning variance). Provenance: main `4cf6841`, ELF sha256 `d5edc686…`,
/// opt-level 2 per the [chip.esp32s3] toolchain-bug workaround — opt-level AND the
/// executor era are both part of this number's conditions.
/// `app_slot_bytes`/`baseline_image_bytes`: `targets/s3-cyd/partitions-ota-s3.csv`,
/// espflash save-image against that CSV = 1,028,656 B (16.35% of the slot).
/// The S3's floor, promoted from an inline literal to a named const (#413 phase 2).
///
/// 72,004 B is the `.stack` region of the heaviest-stack tier ever run on this unit
/// (`stack-paint` = bard + canonical: narration + mesh relay + MQTT + the 4× display), which ran
/// clean — **the smallest region PROVEN sufficient, not the smallest that works.**
///
/// ⚠️ One-line shell-parser contract, as the C3's.
pub const ESP32S3_STACK_FLOOR_BYTES: u32 = 72_004;

/// [`FloorProvenance::ObservedSufficient`] — and this is the chip the whole enum exists for.
///
/// **There is no measured high-water for the S3, because `stack-paint` is INVALID on this chip as
/// written**: its sentinel is trampled by boot-era machinery sharing the `.stack` region (59,504 B
/// read "used" one statement after painting, at boot, before anything deep ran), and re-painting
/// after init crashed the box into a 99-boot exception loop. The `_stack_*_cpu0` symbols alias the
/// whole-region ones (readelf-verified), so a CPU-slice port is NOT the fix.
///
/// ⚠️ **THIS CONST IS THE RATCHET'S TRIGGER.** When the xtensa high-water instrument is fixed
/// (#398 follow-up), the correct change is: measure the peak, add `ESP32S3_MEASURED_PEAK_BYTES`
/// with the `4/3` assertion the C3 has, and flip this to [`FloorProvenance::Derived`] — at which
/// point the packaging gate stops merely reporting the status and starts requiring it. Do NOT flip
/// this constant to `Derived` on the strength of a clean run; `ObservedSufficient` already means
/// "it ran clean". `Derived` means the instrument measured how close it came.
pub const ESP32S3_STACK_FLOOR_PROVENANCE: FloorProvenance = FloorProvenance::ObservedSufficient;

pub const ESP32S3_CYD: ChipBudget = ChipBudget {
    chip: "esp32s3",
    free_dram_bytes: 96_676,
    stack_floor_bytes: ESP32S3_STACK_FLOOR_BYTES,
    app_slot_bytes: 0x0060_0000,
    baseline_image_bytes: 1_028_656,
};

#[cfg(all(target_os = "none", feature = "esp32s3"))]
pub const CHIP: ChipBudget = ESP32S3_CYD;

/// A chip whose budget has never been measured, on a bare-metal target.
///
/// **A poison row, not a permissive default.** Every field is chosen so that any question asked
/// of it answers "no" and any number read off it is obviously wrong rather than plausibly
/// right: zero free DRAM and a zero-byte app slot mean `fits_dram`/`fits_flash` are false for
/// every cost, and `dram_headroom()`/`flash_headroom()` are 0.
///
/// The name is the other half of the design. `chip: "unmeasured"` reaching a log line or a
/// dashboard reads as a bug immediately, which is the same discipline as
/// `net::profile`'s deliberately-implausible fallback arm — a fallback that looks like a real
/// device is how a fallback survives.
///
/// ⚠️ `tools/build_matrix.py::budget_chips()` skips this row by name when it cross-checks the
/// chip roster against `tools/build-matrix.toml`. It is not a fleet target; it is the absence
/// of one, given a shape.
#[cfg(all(target_os = "none", not(any(feature = "esp32c3", feature = "esp32c6", feature = "esp32s3"))))]
pub const UNMEASURED: ChipBudget = ChipBudget {
    chip: "unmeasured",
    free_dram_bytes: 0,
    stack_floor_bytes: 0,
    app_slot_bytes: 0,
    baseline_image_bytes: 0,
};

#[cfg(all(target_os = "none", not(any(feature = "esp32c3", feature = "esp32c6", feature = "esp32s3"))))]
pub const CHIP: ChipBudget = UNMEASURED;

/// Whether [`CHIP`] carries MEASURED numbers, as a value rather than as a cfg incantation.
///
/// The point of exposing it is that "this chip has no budget" is a fact other code may need to
/// branch on WITHOUT restating the roster — a second copy of `any(feature = "esp32c3", …)`
/// somewhere else is exactly the two-statements-of-one-fact rot that `build_matrix.py` exists to
/// catch between this file and the build matrix.
#[cfg(target_os = "none")]
pub const CHIP_MEASURED: bool = cfg!(any(feature = "esp32c3", feature = "esp32c6", feature = "esp32s3"));

/// A bare-metal build must name its chip. Distinct from "named it and it has no row" — that is
/// [`UNMEASURED`] and it only bites the budget-predicated features. This is the build not saying
/// which silicon it is for AT ALL, which nothing downstream can recover from: `build.rs` cannot
/// stamp `SMOL_CHIP_ID`, `net::target` cannot refuse a cross-chip OTA image, and `BoardProfile`
/// cannot label the device.
#[cfg(all(
    target_os = "none",
    not(any(
        feature = "esp32c3",
        feature = "esp32c5",
        feature = "esp32c6",
        feature = "esp32s3"
    ))
))]
compile_error!(
    "no chip feature is enabled, so this firmware build does not say what silicon it is for. \
     Enable exactly one of `esp32c3` / `esp32c5` / `esp32c6` / `esp32s3` (rust/clock/Cargo.toml). \
     `default` carries `esp32c3`, so reaching this means `--no-default-features` was used with a \
     tier but no chip — the per-chip invocations in tools/build-matrix.toml show the full form. \
     NOTE this is NOT the 'chip has no measured budget' case: that one is UNMEASURED, it compiles \
     fine, and it refuses only the features whose verdicts need a budget."
);

/// Two chip features at once. **This must be an error and not a precedence rule**, which is why
/// there is no `else` arm anywhere above: with a silent winner, `--features esp32c5` — the
/// natural thing to type, and wrong, because `default` already carries `esp32c3` — would build
/// C3 numbers, C3 HAL bindings and a C3-stamped descriptor while the operator believed they had
/// a C5. #349 removed that exact shape from `build.rs` (an ambiguous triple resolved to a
/// plausible-looking C6 id that `decide()` then trusted); re-introducing it here would be the
/// same bug one layer up.
///
/// esp-hal's own build script would also refuse two chips, eventually. It would do so without
/// mentioning smol, the feature that caused it, or the `--no-default-features` that fixes it.
#[cfg(any(
    all(feature = "esp32c3", feature = "esp32c5"),
    all(feature = "esp32c3", feature = "esp32c6"),
    all(feature = "esp32c3", feature = "esp32s3"),
    all(feature = "esp32c5", feature = "esp32c6"),
    all(feature = "esp32c5", feature = "esp32s3"),
    all(feature = "esp32c6", feature = "esp32s3")
))]
compile_error!(
    "TWO OR MORE chip features are enabled, and a build is one chip. Almost always the cause is \
     `--features esp32c5` (or c6/s3) WITHOUT `--no-default-features`: `default` carries \
     `esp32c3`, so that adds a second chip rather than choosing one. The full form is \
     `--no-default-features --features esp32c5,<tier>` — see tools/build-matrix.toml, which \
     spells out the per-chip invocation for every declared chip."
);

/// Host builds (`hostsim`, the `tests/` suites, `web-emu`) link no firmware, so no device
/// budget applies and the predicates below are inert. The numbers are still reachable as
/// [`ESP32C3`] etc., which is how `tests/budget.rs` checks the arithmetic without needing a
/// cross-compile.
#[cfg(not(target_os = "none"))]
pub const CHIP: ChipBudget = ChipBudget {
    chip: "host",
    free_dram_bytes: u32::MAX,
    stack_floor_bytes: 0,
    app_slot_bytes: u32::MAX,
    baseline_image_bytes: 0,
};

/// Measured costs of the predicated features.
pub mod cost {
    use super::FeatureCost;

    /// The Bard (#300) — a 260K-param transformer, `SEQ_CAP` 80.
    ///
    /// ELF-section delta, bard on → off, same commit and toolchain
    /// (`scratch/347/morpheus-task-a.md`, 2026-08-01):
    ///
    /// | section | delta |
    /// |---|---:|
    /// | `.bss` | +37,832 |
    /// | `.data` | +1,232 |
    /// | alignment (`.rwtext`/`.rwdata_dummy`) | +8 |
    /// | **DRAM total** | **+39,072** |
    /// | `.rodata` (the model blob) | **+287,392** |
    ///
    /// The DRAM delta and the `.stack` loss matched to the byte in that measurement, which is
    /// the evidence that `.stack` really is "whatever DRAM is left".
    ///
    /// ⚠️ `.bss` is **37,832 B**, not the ~67 KB quoted in #347's body — that figure predates
    /// the SEQ_CAP 80 / 96 KB-heap shape the Bard actually shipped with.
    pub const BARD: FeatureCost = FeatureCost {
        feature: "bard",
        dram_bytes: 39_072,
        flash_bytes: 287_392,
    };

    /// `story` — the esp32c6-watch's one predicated feature, and the **only cost row in this
    /// file measured on a chip other than the C3**. Not a smol `[features]` entry today; it is
    /// here as data because [`super::ESP32C6_WATCH`] is, and a budget with no cost to judge is
    /// a guard that has never been watched saying yes.
    ///
    /// ELF-section deltas against the same watch baseline the chip row was measured from
    /// (watch repo `a4a86a3`, 2026-08-24):
    ///
    /// | quantity | baseline | with `story` | delta |
    /// |---|---:|---:|---:|
    /// | `.bss` + `.data` | 286,380 | 291,772 | **+5,392** |
    /// | `.stack` region | 80,272 | 74,880 | **−5,392** |
    /// | `.text` + `.rodata` | 4,559,532 | 4,595,074 | **+35,542** |
    /// | image | 4,668,784 | 4,704,528 | +35,744 |
    ///
    /// **Two independent derivations of the DRAM cost agree to the byte** — the statics grew by
    /// exactly what the stack region lost. That is the same cross-check that made the Bard's
    /// row trustworthy, and it is the evidence that `.stack` really is "whatever DRAM is left".
    ///
    /// ⚠️ `flash_bytes` is the **section** delta (35,542), not the **image** delta (35,744).
    /// The 202 B difference is image header + padding, and the field is defined as
    /// `.rodata + .text`. Both numbers are correct for what they measure; do not reconcile one
    /// to the other.
    pub const STORY: FeatureCost = FeatureCost {
        feature: "story",
        dram_bytes: 5_392,
        flash_bytes: 35_542,
    };
}

// ── The predicates ──────────────────────────────────────────────────────────────────────
//
// A const-eval assertion, not a build-script check: build.rs cannot read these consts, so a
// copy of this arithmetic over there would be a second definition free to drift from the one
// that is documented — the exact failure `repro_build.sh` calls out when it explains why the
// stack check was extracted into a shared function.
//
// The cost is that a const panic message must be a literal, so it cannot print the computed
// shortfall. The shortfall is available two ways instead: `ChipBudget::dram_shortfall()` at
// any call site, and `tests/budget.rs`, which asserts the exact byte count.
//
// ⚠️ These are the only items in this file that do not move to `smol-core` verbatim — the
// feature flags belong to whichever crate declares them.

/// **The unmeasured-chip refusal (#347 Part 2).** Must come FIRST, because it is the one case
/// where the two asserts below would fire with true arithmetic and a misleading explanation.
///
/// A budget-predicated feature on a chip with no [`ChipBudget`] row hits [`UNMEASURED`], whose
/// every field is zero, so `fits_dram` is false and the DRAM assert fires — telling the reader to
/// shrink `SEQ_CAP` and re-measure, against a chip whose budget does not exist. They would go
/// tune a model to fit a number that was never measured. The fix is a measurement, and only this
/// assert can say so.
///
/// This is the granularity change from the old cfg ladder made concrete: the refusal is here, on
/// the feature that needs a budget, instead of on the crate. A C5 radio build asks nothing of this
/// module and compiles; a C5 **Bard** build is refused, by name, with the measurement to take.
#[cfg(all(target_os = "none", feature = "bard", not(feature = "off-fleet")))]
const _: () = assert!(
    CHIP_MEASURED,
    "`bard` is budget-predicated, and THIS CHIP HAS NO MEASURED ChipBudget row (src/budget.rs). \
     The verdict is refused for want of data, NOT because the feature is too big — do not read \
     the DRAM/FLASH messages below as applying here, and do not shrink the model to satisfy a \
     budget nobody has measured. \
     To take the measurement: build the canonical tier for this chip, read `.stack` and the image \
     size off the artifact (`readelf -SW`), and add a `ChipBudget` const with those numbers plus a \
     `feature = \"<chip>\"` arm at the CHIP lookup above. Add the chip to tools/build-matrix.toml \
     in the same commit — `build_matrix.py check` asserts the two rosters agree in both \
     directions. \
     Or, if this build is deliberately not a fleet image, add `off-fleet` (#348), which \
     tools/repro_build.sh then refuses to package."
);

/// DRAM axis. Fires for `--features bard` on the C3: the Bard's 39,072 B against a
/// 32,352 B headroom, **short by 6,720 B** — which reproduces #335's published shortfall
/// exactly, from data rather than from a bench run.
#[cfg(all(feature = "bard", not(feature = "off-fleet")))]
const _: () = assert!(
    CHIP.fits_dram(&cost::BARD),
    "`bard` does not fit this chip's declared DRAM budget (src/budget.rs). Its static DRAM \
     (.bss + .data) is larger than free_dram_bytes - stack_floor_bytes, so the linker would \
     take the difference out of the runtime stack and shrink it below the floor SILENTLY — \
     the image would link and then die on hardware (#300, #335, #347). \
     Options: build it for a chip with a declared row that has the room (the C6's is measured; \
     the S3 has NO row yet — #398 — despite what an older revision of this message claimed); shrink \
     the cost (`SEQ_CAP` in src/bard/nano_llm.rs is the lever) and re-measure BOTH the cost \
     and the budget; or, if this build is deliberately not the C3 fleet image, add the \
     `off-fleet` feature — which tools/repro_build.sh then refuses to package."
);

/// Flash axis. Does **not** fire today — the Bard's 287,392 B sits inside an 876,016 B
/// headroom. It is here because the axes are independent: the day a chip has a smaller OTA
/// slot, this is the one that catches it, and the message says so instead of blaming DRAM.
#[cfg(all(feature = "bard", not(feature = "off-fleet")))]
const _: () = assert!(
    CHIP.fits_flash(&cost::BARD),
    "`bard` does not fit this chip's declared FLASH budget (src/budget.rs): the canonical \
     image plus the model blob overruns the OTA app slot, so ota_publish.sh would refuse the \
     image at package time. This is the flash axis, NOT the DRAM one — the two are separate \
     fields and separate verdicts. Shrink the model, use a chip with a larger slot, or \
     re-cut the partition table (which is an OTA-compatibility change, not a build change)."
);

// ── #367: scan heap floor ──────────────────────────────────────────────────────────────────────

/// Size of one `esp_radio::wifi::ap::AccessPointInfo`, **measured on this target** (compile-time
/// probe: `const _: [(); 0] = [(); size_of::<AccessPointInfo>()]`, whose error reports the size).
/// A const rather than a sentence so the derivation below can be checked, not merely believed.
pub const SCAN_AP_INFO_BYTES: u32 = 47;

/// Largest AP count the scan guard is sized to absorb.
///
/// **This is bounded by the FLEET, not chosen from the environment** — and that inversion is the
/// whole correction. The first version of this file picked 150 (a guess at residential density)
/// and derived a 36,096 B floor from it. Retained DIAG then showed the fleet's *steady* free heap:
///
/// ```text
/// id8  heap=42,104  hmin=36,976   (crown)
/// id5  heap=27,732  hmin=24,296
/// id50 heap=29,332  hmin=29,332
/// id51 heap=42,056  hmin= 3,732   <- min-ever, and the reason this guard exists
/// ```
/// *(retained DIAG, 2026-08-02 ~01:35 PDT, C3 fleet of 4, flush-time snapshots. The two C6
/// watches report `hmin=0` — the field is unimplemented there — and are excluded.)*
///
/// A floor of 36,096 B **exceeds the steady free heap of id5 and id50**, so those boards would
/// have skipped every scan forever: defer → skip → defer, a permanent scan-disabler dressed as a
/// safety guard. So the floor must sit BELOW the fleet's minimum steady free heap (27,732 B), and
/// that ceiling is what bounds the density we can cover — not the other way round.
///
/// 128 is the largest power-of-two capacity whose peak leaves a 1.5x margin under such a floor.
pub const SCAN_REF_BSSIDS: u32 = 128;

/// Peak heap the scan itself can occupy, derived — see #367.
///
/// `scan_async` collects internally and `ScanResults` implements **only** `next()` (no
/// `size_hint`, no `ExactSizeIterator`), so `collect()` cannot pre-allocate and the `Vec` grows by
/// DOUBLING. Two consequences, and the second is the one that gets rescaled wrongly later:
///
/// 1. Final capacity is the power of two >= N — for 128 BSSIDs that is exactly 128.
/// 2. Across the final realloc the OLD and NEW buffers are both live, so the transient is
///    `(cap/2 + cap) x 47 B` = `1.5 x cap x 47 B`.
///
/// => `(64 + 128) x 47` = **9,024 B**.
///
/// ⚠️ **STEP FUNCTION, not a curve. Do not rescale it linearly.** Every N in 65..=128 costs the
/// same. The next cliff is **129** BSSIDs → capacity 256 → `(128 + 256) x 47` = 18,048 B, double
/// in one step — which is ABOVE the floor below, so a genuinely denser environment will start
/// deferring scans rather than risking the heap. That is the intended failure direction, and
/// `diag.scan_skip` is how it becomes visible instead of silent.
pub const SCAN_PEAK_BYTES: u32 = (64 + 128) * SCAN_AP_INFO_BYTES; // 9,024

/// Free heap required before starting a scan (#367). `= 1.5 x SCAN_PEAK_BYTES`.
///
/// The 0.5x margin is an **honest safety factor, NOT an enumeration**, and the distinction is what
/// a re-deriver needs:
///
/// * The main superloop contributes **nothing**, and that half IS enumerated: the scan is awaited
///   under `embassy_futures::block_on` with **no executor** (0 embassy-executor in the lockfile;
///   `esp-rtos` without its `embassy` feature), so MQTT, DIAG, OTA and relay are parked throughout.
/// * What could **not** be bounded from source is esp-radio's demand-driven RX pool
///   (`dynamic_rx 40` / `static_rx 16`, set in `net::radio_controller_config()`). The margin covers
///   that gap and nothing else.
///
/// Sanity against the live fleet above: **13,536 B sits 14,196 B below id5's steady free heap**, so
/// every healthy board still scans — and **3.6x above id51's 3,732 B min-ever**, so the guard does
/// engage under the real pressure that made it necessary. A floor that never fires and a floor that
/// always fires are the same bug; this one discriminates.
pub const SCAN_HEAP_FLOOR_BYTES: u32 = SCAN_PEAK_BYTES + SCAN_PEAK_BYTES / 2; // 13,536

const _: () = assert!(
    SCAN_HEAP_FLOOR_BYTES > SCAN_PEAK_BYTES,
    "the scan heap floor must exceed the scan's own peak, or the guard cannot protect anything \
     (src/budget.rs, #367)."
);

/// The fleet's minimum observed STEADY free heap (id5, see [`SCAN_REF_BSSIDS`]). Recorded as a
/// const so the constraint below is machine-checked rather than trusted: a floor above this is a
/// permanent scan-disabler, which is exactly the defect this file shipped once already.
///
/// ⚠️ Timestamped + scoped, per the rule a negative result taught us: retained DIAG, 2026-08-02,
/// C3 fleet of 4. It is NOT a standing property. If the fleet grows, or a feature lands that eats
/// heap, re-measure — and note this asserts against the value observed THEN, not against live heap.
pub const FLEET_MIN_STEADY_FREE_HEAP_BYTES: u32 = 27_732;

const _: () = assert!(
    SCAN_HEAP_FLOOR_BYTES < FLEET_MIN_STEADY_FREE_HEAP_BYTES,
    "the scan heap floor is at or above the lowest steady free heap ever observed on the fleet \
     (src/budget.rs, #367) — boards at that level would defer EVERY scan forever, which is a \
     scan-disabler wearing a guard's clothes. Lower the floor, or re-measure the fleet and update \
     FLEET_MIN_STEADY_FREE_HEAP_BYTES with a fresh timestamp."
);
