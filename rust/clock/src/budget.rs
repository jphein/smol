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

/// The budget in force for the target being compiled.
///
/// **Fail-closed by construction.** A bare-metal target with no declared budget is a
/// compile error, not an unbudgeted pass — the whole point of #348 is that an undeclared
/// capability cannot be queried, so silently answering "sure, it fits" for a chip nobody has
/// measured would reproduce the bug in a new place. When the S3 or the C6 arrives (#331),
/// the compiler will demand its row here before it will build, which is the intended
/// friction.
///
/// The selection is `target_arch`-shaped only because this crate pins exactly one bare-metal
/// target today. **#349 owns chip identity**; when its `TargetId` lands, replace this cfg
/// ladder with a lookup on that — it is one function and the data above does not move.
#[cfg(all(target_os = "none", target_arch = "riscv32"))]
pub const CHIP: ChipBudget = ESP32C3;

#[cfg(all(target_os = "none", not(target_arch = "riscv32")))]
compile_error!(
    "no ChipBudget is declared for this bare-metal target. Add a `ChipBudget` const in \
     src/budget.rs with MEASURED numbers (build the canonical tier for the chip and read \
     `.stack` / image size from the artifact) and extend the CHIP cfg ladder. Do not copy \
     the C3's row: an undeclared capability that is guessed is worse than one that is absent."
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
     Options: build it for a chip whose row has the room (#331 — the S3 and C6 do); shrink \
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
