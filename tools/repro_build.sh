#!/usr/bin/env bash
# repro_build.sh — reproducible fleet-image build helpers (issue #44). SOURCE this file;
# it defines shell functions, runs nothing on its own.
#
# ── WHY ──────────────────────────────────────────────────────────────────────
# The smol release ELF was not hash-reproducible: rustc embeds ABSOLUTE build paths
# (panic `file!()` location strings) for every dependency and every build-std crate —
#     ~$CARGO_HOME/registry/src/…/<dep>/src/lib.rs      (deps; ~62 strings)
#     <rustc-sysroot>/lib/rustlib/src/rust/library/…    (core/alloc; ~3 strings)
# Those roots differ per build host / working-dir / user, so the SAME (commit, node-id)
# built on two machines produced different bytes → different sha256. That's why an OTA
# image couldn't be hash-verified against its source commit/board, which compounded the
# dup-NODE_ID outage (#42): the wrong image flashed to id8/id9 couldn't be caught by an
# image↔board hash check. (The git version stamp is NOT the cause — the release pipeline
# already pins it via SMOL_GIT_HASH/SMOL_BUILD_NUMBER, so it is deterministic per commit.)
#
# A SECOND source: esp-bootloader-esp-idf's build.rs stamps the esp_app_desc time/date from
# `Timestamp::now()` (wall clock) unless SOURCE_DATE_EPOCH is set, so two builds of the same
# commit differ by minutes even with paths remapped.
#
# ── FIX ──────────────────────────────────────────────────────────────────────
# (1) Canonicalise the two path roots with `--remap-path-prefix` so the embedded strings are
# identical on every machine (`/registry`, `/rust`). The SOURCE prefixes are machine-relative
# (computed here from $CARGO_HOME + `rustc --print sysroot`); the TARGET tokens are fixed.
# (2) Pin SOURCE_DATE_EPOCH to the COMMIT's Unix time so the app-descriptor timestamp is
# deterministic per commit. Result: byte-reproducible image for a fixed (commit, node-id) → a
# stable, verifiable sha256. Bonus: no `$HOME` path leaks into the public repo's binaries.
#
# ── DEFAULT-BUILD INVARIANT ───────────────────────────────────────────────────
# These flags are applied ONLY when a caller opts in (ota_publish.sh / verify_image.sh
# source this and splice REPRO_CARGO_ARGS into their `cargo build`). Nothing in
# .cargo/config.toml or any source file changes, so a plain `cargo build` is byte-for-byte
# whatever it was before — the default build is provably untouched (no cfg, no source edit).

# The bare-metal target the fleet builds for (matches .cargo/config.toml `build.target`).
#
# ⚠️ #413 PHASE 2B: THIS IS NOW A DEFAULT, NOT A CONSTANT. It is the CANONICAL chip's target, kept
# as a literal so every existing caller and every recorded C3 measurement is untouched — but it is
# no longer the only value it can hold. `repro_chip_spec <chip>` re-points it (plus the toolchain,
# build-std and opt-level that go with that chip) from `tools/build-matrix.toml`, which is the one
# place the (chip → target, toolchain, build_std, opt_level, features) tuple lives.
#
# It stays a scalar-with-a-default rather than becoming a required argument because six consumers
# read it and a required parameter would make every one of them a caller's problem at once. The
# per-chip paths call `repro_chip_spec` first; the C3 path does not have to.
REPRO_TARGET="riscv32imc-unknown-none-elf"
# The chip that target belongs to. Same reasoning: a default the C3 path never has to think about.
REPRO_CHIP="${REPRO_CHIP:-esp32c3}"
# Set by `repro_chip_spec` for non-default chips; empty means "the pinned stable toolchain, no
# build-std, the profile's own opt-level" — i.e. exactly what the C3 has always used.
REPRO_TOOLCHAIN="" REPRO_BUILD_STD="" REPRO_OPT_LEVEL=""

# repro_chip_spec <chip> — re-point REPRO_TARGET/CHIP/TOOLCHAIN/BUILD_STD/OPT_LEVEL at one chip.
#
# Reads `tools/build_matrix.py chip-checks`, which already emits the whole tuple and is what the
# xtensa CI spike builds from. Deliberately NOT a second roster here: a publish path that carried
# its own chip table would be the FIFTH copy of that roster in this repo, and the fourth
# (`ota_publish.sh`'s id→name dict) was already silently short the C5 when #413 found it.
#
# Fields are tab-separated with `-` for empty. A tab is IFS *whitespace*, so bash collapses runs of
# them and every field after an empty one shifts left — the sentinel is what survives that, and the
# reason this reads `-` back to empty rather than trusting position alone.
repro_chip_spec() {
  local chip="${1:-}" line
  case "$chip" in
    '') echo "repro_chip_spec: a chip is required" >&2; return 1 ;;
    *[!a-z0-9]*) echo "repro_chip_spec: implausible chip name '$chip'" >&2; return 1 ;;
  esac
  local matrix="$REPRO_ROOT/tools/build_matrix.py"
  [ -x "$matrix" ] || { echo "repro_chip_spec: no $matrix" >&2; return 1; }
  line="$("$matrix" chip-checks 2>/dev/null | awk -F'\t' -v c="$chip" '$1==c')"
  [ -n "$line" ] || { echo "repro_chip_spec: '$chip' is not a declared chip in build-matrix.toml" >&2; return 1; }
  local _c _t _e _tc _bs _ol _f
  IFS=$'\t' read -r _c _t _e _tc _bs _ol _f <<<"$line"
  [ "$_tc" = "-" ] && _tc=""
  [ "$_bs" = "-" ] && _bs=""
  [ "$_ol" = "-" ] && _ol=""
  [ -n "$_t" ] || { echo "repro_chip_spec: '$chip' declares no target" >&2; return 1; }
  REPRO_TARGET="$_t"; REPRO_CHIP="$_c"
  REPRO_TOOLCHAIN="$_tc"; REPRO_BUILD_STD="$_bs"; REPRO_OPT_LEVEL="$_ol"
}

# #338: the CANONICAL fleet feature set, named once. `repro_build_bin` builds it and `tools/gate.sh`
# gates it; before this was a variable the list lived only inside the build line below, so CI could
# have gated a DIFFERENT tier than the one that ships and nothing would have said so. Changing this
# forks the #44 reproducible-image sha lineage — see the note at the build call.
REPRO_FLEET_FEATURES="${REPRO_FLEET_FEATURES:-espnow,cast,io}"

# Resolve the rustc sysroot for the toolchain that will ACTUALLY build — rustup picks it from
# the crate's rust-toolchain.toml, so this MUST be evaluated inside the crate dir (from home it
# would return `stable`, not the pinned 1.96.1, and the remap prefix would miss the build-std
# paths). Arg $1 = crate dir (default ".").
# ⚠️ #413 2B: THE TOOLCHAIN MUST BE THE ONE THAT WILL ACTUALLY BUILD, and for a non-default chip
# that is NOT what rust-toolchain.toml resolves to. `rustc --print sysroot` in the crate dir picks
# up `channel = "stable"`; an S3 build runs on espup's `esp` fork, whose sysroot is a DIFFERENT
# path. Remapping stable's sysroot while build-std compiles `core` out of the esp fork's would
# leave the fork's absolute paths un-remapped in the image — reproducibility silently broken for
# the S3 while the C3 stayed perfect, which is the worst shape this file has.
#
# So the toolchain travels into the sysroot query. `$REPRO_TOOLCHAIN` is empty for the C3, and
# `rustc  --print sysroot` with an empty first word is exactly the old command.
repro_sysroot() {
  local crate_dir="${1:-.}" tc="${2:-$REPRO_TOOLCHAIN}" args=()
  [ -n "$tc" ] && args+=("+$tc")
  ( cd "$crate_dir" 2>/dev/null && "${RUSTC:-rustc}" "${args[@]}" --print sysroot 2>/dev/null ) || true
}

# Echo the machine-specific --remap-path-prefix flags (space-separated). Fixed targets
# (/registry, /rust) ⇒ identical embedded strings on any host. Fails loudly if the rustc
# sysroot can't be resolved (an un-remapped root would silently break reproducibility).
# Arg $1 = crate dir to resolve the toolchain sysroot from (default ".").
repro_remap_flags() {
  local reg sysroot
  reg="${CARGO_HOME:-$HOME/.cargo}/registry"
  sysroot="$(repro_sysroot "${1:-.}" "${2:-$REPRO_TOOLCHAIN}")"
  [ -n "$sysroot" ] || { echo "repro_build: could not resolve rustc sysroot — cannot remap build-std paths" >&2; return 1; }
  printf -- '--remap-path-prefix=%s=/registry --remap-path-prefix=%s=/rust' "$reg" "$sysroot"
}

# Populate the global array REPRO_CARGO_ARGS with the `--config` override that JOINS the
# remap flags onto the target's config-file rustflags (so the linker flags in
# .cargo/config.toml are preserved — an env RUSTFLAGS would REPLACE, not extend, them).
# Callers splice: `cargo build --release … "${REPRO_CARGO_ARGS[@]}"`.
# Arg $1 = crate dir to resolve the toolchain sysroot from (default ".").
repro_cargo_args() {
  local reg sysroot
  reg="${CARGO_HOME:-$HOME/.cargo}/registry"
  sysroot="$(repro_sysroot "${1:-.}" "${2:-$REPRO_TOOLCHAIN}")"
  [ -n "$sysroot" ] || { echo "repro_build: could not resolve rustc sysroot — cannot remap build-std paths" >&2; return 1; }
  REPRO_CARGO_ARGS=(
    --config "target.${REPRO_TARGET}.rustflags=[\"--remap-path-prefix=${reg}=/registry\",\"--remap-path-prefix=${sysroot}=/rust\"]"
  )
}

# #300 STACK FLOOR — the gate that was missing. `.stack` is placed in the DRAM left after `.bss`,
# and the linker silently shrinks it rather than failing, so a successful link is NOT evidence of a
# runnable image: with SEQ_CAP 192 the bard image linked with 2592 B of stack (82304 B without bard)
# and put `__stack_chk_guard` outside the stack region, where the canary cannot detect the overflow
# it exists to catch. Refuse to package an image that thin.
#
# The floor is DERIVED, not chosen — and since #348 it is derived in exactly ONE place.
#
# It used to be the literal 73,728 here: 4/3 x the T13 bench's 54,856 B high-water, rounded up to
# 72 KiB. That number was the LOWEST of the four peaks now on record, and #335 measured a higher
# one on hardware (55,656 B, id5 under crown duty, 10/10 byte-identical reports) — so the gate was
# knowably 480 B too low, while rust/clock/src/budget.rs already carried the re-derived 74,208.
# Two constants for one concept, disagreeing. `repro_stack_floor` (below) now PARSES the Rust
# declaration instead, the way #338 collapsed the fleet feature list to one variable.
#
# Direction of the dependency, deliberately: the Rust const is the definition and this script
# reads it, not the reverse. `budget.rs` is moving to `smol-core` (#347 Phase 2) to be shared by
# smol, the esp32c6-watch and the Bard device — a tools/ script in THIS repo cannot be the source
# of truth for a crate three firmwares depend on, and a build.rs that shelled out to read it would
# not move with the crate.
#
# The 4/3 is a third again on top of the worst path we have actually observed — enough to absorb a
# deeper interrupt nesting or a future radio change without being so generous that the gate stops
# biting. Re-measure with stack-paint if either the radio stack or the bard's buffers move; a floor
# copied forward untested is how the last one ended up at 12,288. Full derivation + the table of
# every peak on record lives on ESP32C3_STACK_FLOOR_BYTES in budget.rs.
#
# ⚠️ The floor bounds the linked REGION, which is all an ELF can show. It cannot see runtime
# high-water: a struct that lives in a stack-resident `RadioManager` costs real stack and moves this
# number by almost nothing (#181's LedgerLink = 1,760 B on target, but only −32 B of region). So a
# PASS here means "the region is not absurdly thin", NOT "the image has stack headroom". Re-derive
# 4/3 x measured-high-water whenever something grows the deep call paths.
#
# #338: extracted from `repro_build_bin` so CI measures with the SAME code the packaging path uses.
# A CI copy of this arithmetic would be a second definition free to drift from the one that actually
# gates shipping — which is the exact failure this issue exists to remove.
# Repo root, resolved from this script's own location so the functions below work from any cwd
# (gate.sh sources this from $ROOT, ota_publish.sh from the crate dir, agents from anywhere).
REPRO_ROOT="${REPRO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)}"

# #348: echo the C3 stack floor by PARSING its single definition in Rust. Returns non-zero and
# echoes nothing if the declaration cannot be read, so callers can fail closed — see the contract
# documented on ESP32C3_STACK_FLOOR_BYTES (one line, plain decimal, comments on their own line).
# The awk takes everything between `=` and `;` and strips spaces/underscores, so a trailing
# comment on the line cannot be swallowed into the number; the digits-only test is the backstop.
# repro_stack_floor <chip>  →  prints "<bytes> <provenance-token>"
#
# ── #413 PHASE 2: THIS USED TO BE CHIP-BLIND, AND ITS CORRECTNESS WAS COINCIDENTAL ────────────
# It read `ESP32C3_STACK_FLOOR_BYTES` by name and `repro_stack_check` applied that number to
# whatever ELF it was handed. The direction of the resulting error is worth stating exactly,
# because the obvious guess is wrong: the C3's floor (74,208) is the HIGHEST of the three declared
# floors (C6 71,680 · S3 72,004), so a chip-blind check was STRICTER than a non-C3 chip's own
# floor. It could not false-ACCEPT; it could false-REJECT a valid S3 image whose `.stack` landed in
# [72,004, 74,208) — a 2,204 B window — and it would only false-accept once some chip's legitimate
# floor exceeded the C3's. It happened to give the right answer for the S3 fleet image because that
# image measures 116,940 and clears both numbers. Right answer, unrelated reason.
#
# ── AND THE PART THAT MADE THE FIX MORE THAN A PARAMETER ──────────────────────────────────────
# The three floors are three different KINDS of number, so returning "whichever number the row
# holds" would produce a gate that LOOKS uniform and MEANS three different things:
#
#   derived              C3 — measured high-water × 4/3, compile-asserted. The strong form.
#   boot-assert          C6 — a firmware contract, sitting ~1,320 B BELOW the empirical line.
#   observed-sufficient  S3 — the smallest region PROVEN clean, because `stack-paint` is INVALID
#                             on xtensa (sentinel trampled by boot-era machinery; re-painting
#                             crashed into a 99-boot loop). No high-water exists for that chip.
#
# So the provenance travels WITH the number and `repro_stack_check` prints it. An operator reading
# `stack: 116940 B (floor 72004 B, observed-sufficient)` learns what the gate did and did not prove.
#
# ⚠️ THE HARDENING RATCHET (#413 ruling — the condition attached to this decision):
#
#     when a chip gains a working high-water instrument, this gate HARDENS for that chip —
#     derived floors become REQUIRED and observed-sufficient/boot-assert refuse; permissive only
#     while measurement is impossible AND documented at the instrument (#398 follow-up for xtensa).
#
# Today every provenance is accepted and merely reported. That is NOT a policy — it is a
# consequence of a named instrument defect, and it expires when the defect is fixed. What this gate
# protects against is silent `.stack` collapse from `.bss` growth, and an observed-sufficient floor
# catches that perfectly well (the S3 sits 44,936 B above its floor, so a ~40 KB regression trips
# it). What it does not do is certify an absolute margin.
repro_stack_floor() {
  local chip="${1:-}"
  case "$chip" in
    '') echo "repro_stack_floor: a chip is required (esp32c3|esp32c6|esp32s3)" >&2; return 1 ;;
    *[!a-z0-9]*) echo "repro_stack_floor: implausible chip name '$chip'" >&2; return 1 ;;
  esac
  local src="${REPRO_BUDGET_RS:-$REPRO_ROOT/rust/clock/src/budget.rs}" v p tok
  [ -f "$src" ] || return 1
  # Uppercase the chip to get the const prefix. `esp32c3` → `ESP32C3_STACK_FLOOR_BYTES`, which is
  # why #413 promoted the C6/S3 floors from inline literals to consts NAMED BY CHIP rather than by
  # board (the rows are ESP32C6_WATCH / ESP32S3_CYD — board names would not map).
  local pre; pre=$(printf '%s' "$chip" | tr 'a-z' 'A-Z')
  v=$(awk -v k="^pub const ${pre}_STACK_FLOOR_BYTES: u32 =" '$0 ~ k {
             split($0, a, "="); split(a[2], b, ";"); gsub(/[ _]/, "", b[1]); print b[1]; exit
           }' "$src" 2>/dev/null)
  case "$v" in ''|*[!0-9]*) return 1 ;; esac
  # The provenance variant, read from the sibling const.
  p=$(awk -v k="^pub const ${pre}_STACK_FLOOR_PROVENANCE: FloorProvenance =" '$0 ~ k {
             n = split($0, a, "::"); split(a[n], b, ";"); gsub(/[ \t]/, "", b[1]); print b[1]; exit
           }' "$src" 2>/dev/null)
  # EXPLICIT mapping, FAILING CLOSED on anything unrecognised. This is the deliberate friction the
  # enum's doc comment describes: adding a `FloorProvenance` variant in Rust does NOT teach this
  # function, so a new epistemic status cannot arrive by accident — it fails here until someone
  # decides, in writing, what the gate should do about it.
  case "$p" in
    Derived)            tok=derived ;;
    ObservedSufficient) tok=observed-sufficient ;;
    BootAssert)         tok=boot-assert ;;
    '') echo "repro_stack_floor: no ${pre}_STACK_FLOOR_PROVENANCE in $src" >&2; return 1 ;;
    *)  echo "repro_stack_floor: unknown FloorProvenance::$p — this gate must be taught what it means" >&2; return 1 ;;
  esac
  printf '%s %s' "$v" "$tok"
}

# repro_stack_check <elf>
# repro_stack_check <elf> <chip>
#
# #413: the chip is now REQUIRED and is not defaulted to the C3. A default would restore exactly
# the chip-blindness this change removes, and it would do so invisibly — the caller that forgot to
# pass a chip would get a plausible verdict rather than an error.
repro_stack_check() {
  local elf="$1" chip="${2:-}" stack_floor prov
  case "$chip" in
    '') echo "FATAL: repro_stack_check needs a chip — refusing to measure an image against an" >&2
        echo "       unspecified chip's floor. That was the #413 defect: the verdict was correct" >&2
        echo "       only by arithmetic coincidence because the C3's floor happens to be the" >&2
        echo "       highest of the three." >&2
        return 1 ;;
  esac
  if [ -n "${REPRO_STACK_FLOOR:-}" ]; then
    # Documented escape hatch, kept — but it is now LOUD. An operator-supplied number has no
    # provenance in `budget.rs`, and calling it `derived` would be a lie, so it gets its own token.
    stack_floor="$REPRO_STACK_FLOOR"; prov="operator-override"
  else
    local pair; pair="$(repro_stack_floor "$chip")" || pair=""
    stack_floor="${pair%% *}"; prov="${pair##* }"
  fi
  # FAIL CLOSED on an unreadable declaration, for the same reason as the unreadable-ELF branch
  # below: a gate that quietly falls back to a built-in default would measure against a number
  # nobody edited, which is precisely the drift #348 removed. Better to refuse and be fixed.
  case "$stack_floor" in
    ''|*[!0-9]*)
      echo "FATAL: could not read the ${chip} stack floor + provenance from ${REPRO_BUDGET_RS:-$REPRO_ROOT/rust/clock/src/budget.rs}" >&2
      echo "       — refusing to measure the stack against a guessed floor. Check that the" >&2
      echo "       declaration is one line of plain decimal (see its doc comment), or set" >&2
      echo "       REPRO_STACK_FLOOR explicitly if you are deliberately overriding it." >&2
      return 1
      ;;
  esac
  local ss se
  ss=$(readelf -sW "$elf" 2>/dev/null | awk '$8=="_stack_start"{print $2; exit}')
  se=$(readelf -sW "$elf" 2>/dev/null | awk '$8=="_stack_end"{print $2; exit}')
  if [ -n "$ss" ] && [ -n "$se" ]; then
    local stack_bytes=$(( 0x$ss - 0x$se ))
    if [ "$stack_bytes" -lt "$stack_floor" ]; then
      echo "FATAL: runtime stack is ${stack_bytes} B, below the ${chip} ${stack_floor} B floor (${prov})." >&2
      echo "       Something grew .bss. Shrink it (nano_llm::SEQ_CAP is the bard's lever) or" >&2
      echo "       reclaim DRAM elsewhere — do NOT ship this image." >&2
      return 1
    fi
    echo "  stack: ${stack_bytes} B (floor ${stack_floor} B, ${prov})"
  else
    # FAIL CLOSED. This gate exists precisely because "it links" was not evidence of a runnable
    # image; a gate that waves through an unreadable ELF restores that blind spot exactly, and
    # the one time it matters is the time something is wrong with the build.
    echo "FATAL: could not read _stack_start/_stack_end from $elf — refusing to package." >&2
    echo "       Check that the ELF exists and that readelf is available; do NOT ship an" >&2
    echo "       image whose stack was never measured." >&2
    return 1
  fi
}

# repro_build_bin <clock_dir> <out_bin> <hash> <build_number> [node_id]
# Reproducibly build the espnow release image for (commit-identity, [node-id]) and write
# the flashable .bin to <out_bin>. Pins the version stamp via the build.rs env contract,
# applies the path remap, and extracts the image with `espflash save-image`. Echoes nothing
# on success (the caller reads <out_bin>); returns non-zero on any step failure.
repro_build_bin() {
  local clock="$1" out="$2" hash="$3" number="$4" node_id="${5:-}"
  local espflash="${ESPFLASH:-$HOME/.cargo/bin/espflash}"
  # #348: `off-fleet` is the cargo feature that WAIVES the per-chip memory budget
  # (rust/clock/src/budget.rs). It exists so tools/gate.sh can keep compiling the Bard for the
  # bigger chips, and so a future Bard-only image can be built at all — neither of which is
  # the fleet image. This is where that stays true: a waived build must never become a
  # published OTA artifact, because the budget it waived is the fleet's. Refuse at the
  # PACKAGING boundary rather than trusting the caller's list, since this function is the one
  # thing every publish path goes through.
  case ",${REPRO_FLEET_FEATURES}," in
    *,off-fleet,*)
      echo "FATAL: REPRO_FLEET_FEATURES names 'off-fleet' — that feature waives the #348 chip" >&2
      echo "       memory budget and is for non-fleet builds (CI tiers, a Bard-only image)." >&2
      echo "       Refusing to package a fleet image that opted out of its own budget." >&2
      return 1
      ;;
  esac

  # #280 (lucid): refuse to build a chip whose `.cargo/config.toml` is STALE. The failure this
  # prevents is specific and was observed on familiar: a config byte-current EXCEPT for the S3
  # arm's `-C link-arg=-Tlinkall.x`, which `cargo check` cannot notice (check never links) and
  # which then fails the LINK with 129 undefined references — reading as a broken toolchain
  # rather than a stale file. The file is git-tracked but `.stignore` ignores `.cargo`, so a
  # synced tree's copy arrives out of band and nothing keeps it current.
  #
  # BOTH non-zero states are fatal HERE, and that is a packaging-boundary judgement rather than
  # the helper's default: `2` means "could not check" (unknown chip, or no `config_markers`
  # declared for it), and an image whose build configuration could not be VERIFIED has no
  # business becoming a published artifact — the same reasoning as the `off-fleet` refusal above.
  # A chip with no declared markers is a manifest gap to close before publishing that chip, and
  # failing here is what applies the pressure. Distinct messages so the operator knows which.
  if [ -r "$REPRO_ROOT/tools/assert_cargo_config.sh" ]; then
    # shellcheck source=tools/assert_cargo_config.sh
    . "$REPRO_ROOT/tools/assert_cargo_config.sh"
    assert_cargo_config "$REPRO_CHIP"
    case $? in
      0) : ;;
      1) echo "FATAL: refusing to package — see the stale-config refusal above (#280)." >&2
         return 1 ;;
      *) echo "FATAL: could not VERIFY .cargo/config.toml for '$REPRO_CHIP' (#280). Not a stale" >&2
         echo "       file — an unverifiable one, which at the packaging boundary is the same" >&2
         echo "       answer: declare the chip's config_markers in tools/build-matrix.toml." >&2
         return 1 ;;
    esac
  fi
  # #218: no explicit number ⇒ use the COMMITTED ratchet (version.txt), NOT git-count.
  # The caller sets SMOL_RELEASE=1 for a real release (clean `vN Word` stamp); otherwise
  # build.rs marks it dev (`vN+dev.<hash> Word`) so a canary can't masquerade as the release.
  [ -n "$number" ] || number="$(tr -d '[:space:]' < "$clock/version.txt" 2>/dev/null)"
  [ -n "$number" ] || number=0
  repro_cargo_args "$clock" || return 1   # resolve the sysroot with the crate's pinned toolchain
  # #44/#326: pin the esp-bootloader-esp-idf app-descriptor build time. Its build.rs fills
  # the esp_app_desc time/date from `Timestamp::now()` (wall clock) UNLESS SOURCE_DATE_EPOCH
  # is set — so without this, two builds of the same commit differ (even with paths remapped).
  #
  # PRECEDENCE FLIPPED 2026-07-31 (#326): the COMMIT time wins; an ambient/caller
  # SOURCE_DATE_EPOCH is only a FALLBACK for archive builds with no .git. The old
  # caller-first order meant a stale `export SOURCE_DATE_EPOCH` from an earlier experiment
  # in the operator's shell silently stamped the shipped image — staged 915 carries an
  # ambient epoch, not its commit time, which made its sha irreproducible from the recipe
  # "commit + flags". An image's identity must be a function of the commit, never of the
  # operator's shell history.
  # REPRO_SDE_OVERRIDE (nebula-triage's design): the EXPLICIT, named escape for forensic
  # reproduction of pre-fix stages — 915's stamp is operator-ambient, so its literal sha is
  # only reproducible by supplying that epoch. Deliberately NOT plain SOURCE_DATE_EPOCH:
  # ambient leakage of that var is the defect the flip above fixes, and an override you
  # must name cannot leak in by accident.
  local sde="${REPRO_SDE_OVERRIDE:-}"
  if [ -z "$sde" ]; then
    sde="$(git -C "$clock" show -s --format=%ct "$hash" 2>/dev/null || true)"
    [ -n "$sde" ] || sde="${SOURCE_DATE_EPOCH:-}"
    [ -n "$sde" ] || sde=1000000000
  fi
  # #326 upstream bug, two halves (esp-bootloader-esp-idf 0.2.0 build.rs): (1) it parses
  # SOURCE_DATE_EPOCH — a SECONDS value by spec — with Timestamp::from_microsecond(), so
  # every shipped image claims 1970-01-01 (epoch/10^6 ≈ 1785 s); cosmetic here since we
  # only need determinism, but documented so nobody "fixes" the date by unpinning. (2) it
  # declares NO rerun-if-env-changed=SOURCE_DATE_EPOCH (only rerun-if-changed=esp_config.yml),
  # so a warm target dir keeps the PREVIOUS build's stamp even when the epoch changes —
  # proven: two commits with different epochs produced the identical warm stamp. Force the
  # build script to re-run so the pinned epoch actually reaches the image.
  #
  # NOT `cargo clean -p esp-bootloader-esp-idf`: measured "Removed 0 files" against a warm
  # tree that then shipped a stale stamp — with a --target build, clean -p reaches neither
  # the HOST-side build-script dirs (target/release/build/<crate>-*) nor their fingerprints,
  # which is exactly where the frozen stamp lives. Delete those two surgically; the cost is
  # one build-script re-run + relink, and warm/cold builds of the same commit become
  # byte-identical (the --twice gate in verify_image.sh proves it).
  (
    cd "$clock" || exit 1
    # Pin identity (deterministic per commit); SMOL_NODE_ID only when a board is named
    # (empty ⇒ build.rs omits it ⇒ board.rs NODE_ID fallback — the fleet-shared image).
    export SMOL_GIT_HASH="$hash" SMOL_BUILD_NUMBER="$number" SOURCE_DATE_EPOCH="$sde"
    [ -n "$node_id" ] && export SMOL_NODE_ID="$node_id"
    # #326: see the upstream-bug note above — without this, the pinned epoch cannot reach
    # a warm build. MECHANISM (measured, nebula-triage review): the frozen stamp itself
    # lives TARGET-side, in <triple>/release/build/esp-bootloader-esp-idf-*/output — but
    # deleting the cached value is not what makes this work. Removing the HOST-side
    # build-script artifacts + fingerprint INVALIDATES THE PRODUCER, forcing the script to
    # re-run, which rewrites that output (proven: host-side-only purge flipped a warm
    # stamp). The target-side globs are included as defense in depth and so that a reader
    # who greps for where the stamp lives finds the glob that covers it — do not "optimise"
    # either pair away on the grounds the stamp isn't in it. `-f` makes an unmatched glob
    # (fresh dir) a no-op. cwd is the crate dir here, so the in-tree default is plain
    # `target` ($clock may be relative and would double up after the cd).
    local _t="${CARGO_TARGET_DIR:-target}"
    rm -rf "$_t"/release/build/esp-bootloader-esp-idf-* \
           "$_t"/release/.fingerprint/esp-bootloader-esp-idf-* \
           "$_t/${REPRO_TARGET}"/release/build/esp-bootloader-esp-idf-* \
           "$_t/${REPRO_TARGET}"/release/.fingerprint/esp-bootloader-esp-idf-*
    # #119: the canonical fleet image is espnow + cast (#26 WLED-cast + the #74 crown
    # display-mirror) + io (#72 registry — inert until a G config binds pins, and the
    # dollhouse's dashboard-only pin-binding depends on it being resident). Changing this
    # list changes the reproducible-image definition (#44): a new sha lineage per commit.
    # #300 added `bard`; #347 REMOVED it from this list. The Bard is radio-free and
    # self-contained, but on the C3 it costs +287,392 B of flash (the model blob in .rodata)
    # and +39,072 B of DRAM (.bss +37,832, .data +1,232) — and that DRAM comes straight out of
    # the RUNTIME STACK: `.stack` gets whatever is left over and the linker shrinks it SILENTLY,
    # so "it links" says nothing about whether the firmware can run. Measured on one commit,
    # one toolchain: canonical WITH bard = 67,488 B of stack, WITHOUT = 106,560 B, against a
    # re-derived floor of 74,208 B (4/3 x the 55,656 B measured high-water). With the bard in,
    # the #233 async re-platform does not fit at all — see #335.
    #
    # ⚠️ #348 attribution fix: those three figures are from `spike/233-stack-measure` @ 2b98fba
    # (the esp-radio 0.18 ASYNC stack — the one the fleet is migrating onto), NOT from this
    # branch. On main's blocking esp-wifi 0.15 the same tiers measure 75,568 / 114,648 against
    # the 73,728 B constant below, which is what docs/ROADMAP.md records. Both are true; they
    # are different radio stacks, and a reader who takes one for the other concludes the Bard
    # has 1,840 B of room when on the stack that matters it is 6,720 B short.
    # rust/clock/src/budget.rs declares the WORSE of the two and refuses `bard` at COMPILE
    # time, so this list is no longer the only thing standing between the Bard and the fleet.
    #
    # ⚠️ The `bard` FEATURE STAYS IN Cargo.toml AND IS STILL GATED (tools/gate.sh builds a
    # `bard` tier). It is out of the C3 fleet image, not out of the project: the S3 and C6 have
    # the DRAM to carry it as a normal smol feature, and bard.realm.watch is a standalone
    # DEVICE + public face, NOT a fork of this source. Do not delete the feature, and do not
    # copy nano_llm out of this tree — a second copy is the divergence #347 exists to avoid.
    #
    # ⚠️ FORKS THE #44 SHA LINEAGE: every image built from this commit forward differs from
    # the with-bard lineage by definition. That is the second fork of this lineage (#300 was
    # the first); an image sha only means something relative to the list in force when it was
    # built, so a hash compared across this boundary will disagree and is SUPPOSED to.
    # #413 2B: the chip's toolchain/build-std/opt-level, all PER-INVOCATION and never config keys.
    #
    # ⚠️ build-std as a `.cargo/config.toml` key is GLOBAL, and that is the 2026-07-20 regression
    # that leaked portable-atomic/unsafe-assume-single-core into the HOST build and broke every
    # cold C3 build. `opt_level` is the same shape for a different reason: the global release
    # profile is shared with the C3, and every C3 number on record (stack floors, #32 symbol
    # sizes, byte-reproducibility) was measured against it. Both stay in the environment of this
    # one command.
    #
    # ⚠️ THE OPT-LEVEL IS PART OF THE #44 REPRODUCIBILITY IDENTITY. A per-chip profile override
    # means the sha lineage is per (chip, PROFILE), not per chip: the S3 builds at opt-level 2 to
    # dodge an Xtensa LLVM scavenger crash, opt-level moves sections, and two S3 images built at
    # different opt-levels will differ and are SUPPOSED to. Compare shas only within one
    # (chip, profile) pair.
    local _cargo_env=() _toolchain_arg=()
    [ -n "$REPRO_BUILD_STD" ] && _cargo_env+=("CARGO_UNSTABLE_BUILD_STD=$REPRO_BUILD_STD")
    [ -n "$REPRO_OPT_LEVEL" ] && _cargo_env+=("CARGO_PROFILE_RELEASE_OPT_LEVEL=$REPRO_OPT_LEVEL")
    [ -n "$REPRO_TOOLCHAIN" ] && _toolchain_arg=("+$REPRO_TOOLCHAIN")
    # `env` with an empty array is a no-op prefix, so the C3 path runs the identical command it
    # always did — which is what makes the byte-identical claim checkable rather than argued.
    # ── THE FEATURE FORM IS BRANCHED, AND THE BRANCH IS FORCED BY MEASUREMENT ─────────────
    # The canonical chip keeps its EXACT historical invocation — `--features <tier>` with default
    # features ON, so `default`'s own chip feature supplies the chip. Non-canonical chips get the
    # explicit form, because `default` carries the CANONICAL chip and would otherwise hand esp-hal
    # two chips (or, before that, trip build.rs's #405 feature-vs-triple cross-check, which is
    # exactly how this was caught: an S3 build died with "chip DISAGREEMENT: feature `esp32c3` but
    # target triple xtensa-esp32s3-none-elf").
    #
    # ⚠️ DO NOT "SIMPLIFY" THIS INTO ONE PATH. The two forms are NOT byte-equivalent, measured:
    #     --features espnow,cast,io                          -> 072679408c8b4ba1…
    #     --no-default-features --features esp32c3,espnow,…   -> 2a3365610afea8ed…
    # Same resolved feature SET, different resolved feature NAME set (`default`/`hw` versus an
    # explicit `esp32c3`), and feature names feed the crate's `-Cmetadata`. The delta is almost
    # certainly metadata-only — but the C3 image's byte-identity is a constraint of record, and
    # "almost certainly" is not the standard. Branching keeps that identity BY CONSTRUCTION
    # instead of by an equivalence argument that has already failed once.
    local _canon _feat_args=()
    _canon="$("$REPRO_ROOT/tools/build_matrix.py" canonical-chip 2>/dev/null || true)"
    if [ -n "$_canon" ] && [ "$REPRO_CHIP" = "$_canon" ]; then
      _feat_args=(--features "$REPRO_FLEET_FEATURES")
    else
      _feat_args=(--no-default-features --features "$REPRO_CHIP,$REPRO_FLEET_FEATURES")
    fi
    env "${_cargo_env[@]}" cargo "${_toolchain_arg[@]}" build --release \
      --target "$REPRO_TARGET" "${_feat_args[@]}" "${REPRO_CARGO_ARGS[@]}"
  ) || return 1
  # Honor CARGO_TARGET_DIR (verify_image.sh --twice points each build at an isolated dir);
  # default to the in-tree target/ (ota_publish.sh's path) when unset.
  local tdir="${CARGO_TARGET_DIR:-$clock/target}"
  # #413: the chip travels with the check, and since 2B it is the SAME chip `repro_chip_spec`
  # resolved the target from — the two cannot disagree because one call sets both.
  repro_stack_check "$tdir/${REPRO_TARGET}/release/clock" "$REPRO_CHIP" || return 1
  # #413 2B: `--chip esp32c3` was HARDCODED here. It is the 7th single-chip site in this file and
  # the only one a `REPRO_TARGET` grep could never find, because it is a different literal in a
  # different flag — the [[literal-grep-proves-nothing-about-constructed-strings]] case, found by
  # reading the function rather than searching it.
  "$espflash" save-image --chip "$REPRO_CHIP" \
    "$tdir/${REPRO_TARGET}/release/clock" "$out" >/dev/null || return 1
}
