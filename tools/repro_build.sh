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
REPRO_TARGET="riscv32imc-unknown-none-elf"

# #338: the CANONICAL fleet feature set, named once. `repro_build_bin` builds it and `tools/gate.sh`
# gates it; before this was a variable the list lived only inside the build line below, so CI could
# have gated a DIFFERENT tier than the one that ships and nothing would have said so. Changing this
# forks the #44 reproducible-image sha lineage — see the note at the build call.
REPRO_FLEET_FEATURES="${REPRO_FLEET_FEATURES:-espnow,cast,io,bard}"

# Resolve the rustc sysroot for the toolchain that will ACTUALLY build — rustup picks it from
# the crate's rust-toolchain.toml, so this MUST be evaluated inside the crate dir (from home it
# would return `stable`, not the pinned 1.96.1, and the remap prefix would miss the build-std
# paths). Arg $1 = crate dir (default ".").
repro_sysroot() {
  local crate_dir="${1:-.}"
  ( cd "$crate_dir" 2>/dev/null && "${RUSTC:-rustc}" --print sysroot 2>/dev/null ) || true
}

# Echo the machine-specific --remap-path-prefix flags (space-separated). Fixed targets
# (/registry, /rust) ⇒ identical embedded strings on any host. Fails loudly if the rustc
# sysroot can't be resolved (an un-remapped root would silently break reproducibility).
# Arg $1 = crate dir to resolve the toolchain sysroot from (default ".").
repro_remap_flags() {
  local reg sysroot
  reg="${CARGO_HOME:-$HOME/.cargo}/registry"
  sysroot="$(repro_sysroot "${1:-.}")"
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
  sysroot="$(repro_sysroot "${1:-.}")"
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
# The floor is DERIVED, not chosen: the T13 bench measured a 54,856 B high-water with the stack-paint
# build (WiFi burst + crown duty + three stories), and 54,856 x 4/3 = 73,141, rounded up to 72 KiB =
# 73,728. The 4/3 is a third again on top of the worst path we have actually observed — enough to
# absorb a deeper interrupt nesting or a future radio change without being so generous that the gate
# stops biting. Re-measure with stack-paint if either the radio stack or the bard's buffers move; a
# floor copied forward untested is how the last one ended up at 12,288.
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
# repro_stack_check <elf>
repro_stack_check() {
  local elf="$1" stack_floor="${REPRO_STACK_FLOOR:-73728}"
  local ss se
  ss=$(readelf -sW "$elf" 2>/dev/null | awk '$8=="_stack_start"{print $2; exit}')
  se=$(readelf -sW "$elf" 2>/dev/null | awk '$8=="_stack_end"{print $2; exit}')
  if [ -n "$ss" ] && [ -n "$se" ]; then
    local stack_bytes=$(( 0x$ss - 0x$se ))
    if [ "$stack_bytes" -lt "$stack_floor" ]; then
      echo "FATAL: runtime stack is ${stack_bytes} B, below the ${stack_floor} B floor." >&2
      echo "       Something grew .bss. Shrink it (nano_llm::SEQ_CAP is the bard's lever) or" >&2
      echo "       reclaim DRAM elsewhere — do NOT ship this image." >&2
      return 1
    fi
    echo "  stack: ${stack_bytes} B (floor ${stack_floor} B)"
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
    # #300: + bard (The Bard storyteller). It is radio-free and self-contained, but it costs
    # ~285 KB of flash (the model blob in .rodata) and ~67 KB of .bss. That .bss comes straight
    # out of the RUNTIME STACK: `.stack` gets whatever DRAM is left over and the linker shrinks
    # it SILENTLY, so "it links" says nothing about whether the firmware can run — see the
    # stack-floor gate below. ⚠️ FORKS THE #44 SHA LINEAGE: every image built from this commit
    # forward differs from the pre-bard lineage by definition.
    cargo build --release --features "$REPRO_FLEET_FEATURES" "${REPRO_CARGO_ARGS[@]}"
  ) || return 1
  # Honor CARGO_TARGET_DIR (verify_image.sh --twice points each build at an isolated dir);
  # default to the in-tree target/ (ota_publish.sh's path) when unset.
  local tdir="${CARGO_TARGET_DIR:-$clock/target}"
  repro_stack_check "$tdir/${REPRO_TARGET}/release/clock" || return 1
  "$espflash" save-image --chip esp32c3 \
    "$tdir/${REPRO_TARGET}/release/clock" "$out" >/dev/null || return 1
}
