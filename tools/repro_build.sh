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
  # #44: pin the esp-bootloader-esp-idf app-descriptor build time. Its build.rs fills the
  # esp_app_desc time/date from `Timestamp::now()` (wall clock) UNLESS SOURCE_DATE_EPOCH is
  # set — so without this, two builds of the same commit differ (even with paths remapped).
  # Pin it to the COMMIT's own Unix time ⇒ deterministic per commit. Precedence: a caller/CI
  # SOURCE_DATE_EPOCH wins (archive builds with no .git); else the commit time; else a fixed
  # constant so the build is deterministic even with neither.
  local sde="${SOURCE_DATE_EPOCH:-}"
  if [ -z "$sde" ]; then
    sde="$(git -C "$clock" show -s --format=%ct "$hash" 2>/dev/null || true)"
    [ -n "$sde" ] || sde=1000000000
  fi
  (
    cd "$clock" || exit 1
    # Pin identity (deterministic per commit); SMOL_NODE_ID only when a board is named
    # (empty ⇒ build.rs omits it ⇒ board.rs NODE_ID fallback — the fleet-shared image).
    export SMOL_GIT_HASH="$hash" SMOL_BUILD_NUMBER="$number" SOURCE_DATE_EPOCH="$sde"
    [ -n "$node_id" ] && export SMOL_NODE_ID="$node_id"
    # #119: the canonical fleet image is espnow + cast (#26 WLED-cast + the #74 crown
    # display-mirror) + io (#72 registry — inert until a G config binds pins, and the
    # dollhouse's dashboard-only pin-binding depends on it being resident). Changing this
    # list changes the reproducible-image definition (#44): a new sha lineage per commit.
    cargo build --release --features espnow,cast,io "${REPRO_CARGO_ARGS[@]}"
  ) || return 1
  # Honor CARGO_TARGET_DIR (verify_image.sh --twice points each build at an isolated dir);
  # default to the in-tree target/ (ota_publish.sh's path) when unset.
  local tdir="${CARGO_TARGET_DIR:-$clock/target}"
  "$espflash" save-image --chip esp32c3 \
    "$tdir/${REPRO_TARGET}/release/clock" "$out" >/dev/null || return 1
}
