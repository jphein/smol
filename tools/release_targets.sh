#!/usr/bin/env bash
# release_targets.sh — build the per-target download artifacts from the targets/ manifests (#413).
#
# THE MANIFESTS ARE THE MATRIX. Each targets/<name>/target.toml declares a chip, a flavor and
# whether it produces an artifact; this script iterates them. Adding a target folder with
# `artifact = true` adds a download — no workflow edit. That is JP's rule for the targets/ layout.
#
# THE PRODUCTION PATH, NOT A PARALLEL ONE. Every artifact goes through the same calls the OTA
# publish path uses — `repro_chip_spec` + `repro_build_bin` from tools/repro_build.sh — from a
# `git archive` tree provisioned by tools/ci_provision.sh (placeholder credentials by
# construction; a published image must NEVER be built from a tree carrying a real secrets.rs).
#
# STAMP HONESTY (#420): a tree without the stage path's env injection stamps version.txt's stale
# number. We pass SMOL_BUILD_NUMBER=0 explicitly — 0 reads as "not a fleet ratchet number" — and
# the git hash rides both the ELF and the release notes. Downloads are for NEW hardware joining;
# fleet boards update over mesh OTA only (docs/RELEASES.md), so a download never needs to win a
# ratchet comparison.
#
# PROVENANCE ON THE ARTIFACT (#413 ruling of record): publishing is the stronger claim, which is
# exactly why the provenance must ride the artifact visibly rather than why the artifact must not
# exist. Each artifact gets a NOTES.md carrying its chip's stack-floor provenance in words a
# reader with no repo context can act on, plus the (chip, profile) sha-lineage rule.
#
# Usage: tools/release_targets.sh <output-dir> [target-name ...]
#   With no names: every manifest with artifact = true (aliases resolved, built once).
#   Exit: 0 all built · 1 a build/gate failed · 2 manifest/environment problem.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:?usage: release_targets.sh <output-dir> [target ...]}"; shift || true
mkdir -p "$OUT"

# Minimal TOML reader for the flat manifests (key = "value" / key = true|false). Deliberately not
# a TOML parser: the manifests are ours, flat by convention, and a parser dependency on the
# publish path is new failure surface (same argument as repro_build.sh's no-TOML rule).
tget() { sed -n "s/^${2} *= *\"\?\([^\"#]*\)\"\?.*/\1/p" "$1" | head -1 | sed 's/ *$//'; }

# Enumerate the requested manifests.
declare -a MANIFESTS=()
if [ "$#" -gt 0 ]; then
  for n in "$@"; do
    m="$ROOT/targets/$n/target.toml"
    [ -f "$m" ] || { echo "release_targets: no manifest for target '$n'" >&2; exit 2; }
    MANIFESTS+=("$m")
  done
else
  for m in "$ROOT"/targets/*/target.toml; do MANIFESTS+=("$m"); done
fi
[ "${#MANIFESTS[@]}" -gt 0 ] || { echo "release_targets: no manifests found" >&2; exit 2; }

GITHASH="$(git -C "$ROOT" rev-parse --short=12 HEAD)"
DATE_UTC="$(date -u +%Y-%m-%d)"

# One provisioned archive tree per run, shared by every target (provision ONCE, build many —
# the #327 confound: ci_provision substitutes a RANDOM key, so provisioning per-target would
# make the artifacts differ by key, not by chip).
#
# CANONICAL PATH, NOT $$ (#327's own mechanism): the tree carries PATH DEPENDENCIES
# (sigil-names, esp-wifi-sys-chip) whose absolute SourceId feeds -Cmetadata — a per-run
# random path makes every artifact unique. A CONSTANT path under one builder makes runs
# byte-identical; cross-BUILDER identity additionally requires the same canonical path
# (CI's is stable per workflow; humans verify via tools/repro_at_canonical.sh). Two cold
# runs at this path were measured identical; two at $$-suffixed paths were measured NOT.
WORK="${TMPDIR:-$ROOT/tmp}/release-targets-canon"
rm -rf "$WORK"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/tree"
git -C "$ROOT" archive HEAD | tar -x -C "$WORK/tree"
CI_PROVISION_FIXED_KEY=1 bash "$WORK/tree/tools/ci_provision.sh" "$WORK/tree/rust/clock" >/dev/null   # deterministic release placeholder (#44 repro + #394)
CLOCK="$WORK/tree/rust/clock"

# shellcheck source=repro_build.sh
. "$ROOT/tools/repro_build.sh"

built=0; failed=0
declare -A DONE=()   # chip+flavor already built (alias resolution)
for m in "${MANIFESTS[@]}"; do
  name="$(tget "$m" name)"; chip="$(tget "$m" chip)"; flavor="$(tget "$m" flavor)"
  artifact="$(tget "$m" artifact)"; alias_of="$(tget "$m" alias_of)"
  [ "$artifact" = "true" ] || { echo "· $name: artifact=false — skipped (reason in its manifest)"; continue; }
  if [ -n "$alias_of" ]; then
    echo "· $name: alias of $alias_of — same image, no second build"; continue
  fi
  [ "$flavor" = "fleet" ] || { echo "· $name: flavor '$flavor' — not a rust/clock target; see the GUI pass below"; continue; }
  key="$chip/$flavor"; [ -n "${DONE[$key]:-}" ] && continue

  echo "== $name ($chip, $flavor) =="
  repro_chip_spec "$chip"
  bin="$OUT/smol-$name-$DATE_UTC-g$GITHASH.bin"
  if ! SMOL_BUILD_NUMBER=0 SMOL_GIT_HASH="$GITHASH" \
       repro_build_bin "$CLOCK" "$bin" "$GITHASH" 0; then
    echo "FAIL: $name build/gate" >&2; failed=$((failed+1)); continue
  fi
  sha="$(sha256sum "$bin" | cut -c1-64)"
  floor_line="$(repro_stack_floor "$chip")"   # "<bytes> <provenance>"
  floor_b="${floor_line%% *}"; prov="${floor_line#* }"
  case "$prov" in
    derived) prov_txt="Its minimum-stack floor ($floor_b B) is DERIVED from a measured on-hardware high-water peak — the strongest provenance this project has." ;;
    observed-sufficient) prov_txt="⚠️ Its minimum-stack floor ($floor_b B) is OBSERVED-SUFFICIENT, not measured: the stack-measuring instrument is known-broken on this chip, so the floor is the largest stack region proven to run clean in bench operation — real protection, weaker provenance. A regression that overruns it may not be caught before it ships." ;;
    boot-assert) prov_txt="⚠️ Its minimum-stack floor ($floor_b B) is a BOOT-TIME DECLARATION from the firmware itself, sitting below the empirically-panicking line — the weakest provenance in the fleet." ;;
    *) prov_txt="Floor $floor_b B, provenance: $prov." ;;
  esac
  cat > "$bin.NOTES.md" <<NOTES
# $name — smol firmware image ($DATE_UTC, git $GITHASH)

**sha256** \`$sha\` · chip **$chip** · flavor **$flavor** · build stamp **0** (downloads are
not fleet-ratchet builds; identity is this git hash, not the on-screen number).

**Who this is for:** flashing NEW hardware to join a smol mesh. Boards already on the mesh
update over mesh OTA only — never by re-downloading this file.

**Stack-floor provenance:** $prov_txt

**⚠️ Re-key before trusting your mesh (#394):** this image carries the PUBLISHED placeholder
group key — deliberately, so the artifact is byte-reproducible. Placeholder-key boards can mesh
ONLY with other placeholder-key boards and can never join a re-keyed fleet. To own your mesh:
regenerate \`GROUP_KEY\` in \`rust/clock/src/secrets.rs\` (32 random bytes), rebuild, reflash.

**Reproducibility:** image shas are comparable only within one (chip, profile) pair — this
chip builds with its declared profile from \`tools/build-matrix.toml\`, and a different
opt-level legitimately produces a different (equally correct) image.
NOTES
  echo "   $bin"
  echo "   sha256 $sha · floor $floor_b ($prov)"
  DONE[$key]=1; built=$((built+1))
done

# ══ THE GUI PASS (#413 phase 3.1) ═════════════════════════════════════════════
#
# A SECOND FLAVOR, NOT A SECOND SCRIPT. The GUI firmware is targets/c6-watch — its own cargo
# workspace (subtree of the esp32c6-watch repo), its own toolchain pin, its own build.rs. It is
# NOT rust/clock, so none of repro_build.sh applies to it: no repro_chip_spec, no
# repro_build_bin, no budget.rs floor. Pretending otherwise is how a shared helper starts
# lying about one of its callers.
#
# ONE ARTIFACT PER *BOARD*, NOT PER FOLDER. One workspace builds three boards behind mutually
# exclusive features (board-waveshare-c6 / board-cyd-c5 / board-esp32s3-cyd), so a folder can
# now yield up to two downloads: its fleet image AND its GUI image. targets/s3-cyd is exactly
# that case — the only board with both. JP's rule survives intact: the folder is still the
# matrix, and a folder declares its GUI download with
#
#   gui_artifact = true
#   gui_board    = "board-<...>"      # the mutually-exclusive feature that selects it
#
# Adding a GUI board is a manifest edit, never a workflow edit.
#
# CREDENTIALS: ABSENT, NOT PLACEHOLDER — the inverse of the fleet flavor, on the watch lane's
# ruling of record. See tools/ci_provision_gui.sh, which is where that argument lives.
GUI_WS="$WORK/tree/targets/c6-watch"

# chip -> (triple, rustflags) for the GUI arms. The S3 is Xtensa and needs three things the
# riscv arms do not: the espup `esp` toolchain, a sourced export-esp.sh (the GCC linker
# xtensa-esp32s3-elf-gcc must be on PATH), and opt-level 2 — s/z crash the Xtensa LLVM
# scavenger under fat LTO. All three are the caller's job to have in the environment; this
# script asserts rather than guesses. Its rustflags are EMPTY because tools/build-s3.sh builds
# it with RUSTFLAGS='' and that is the only layout the Xtensa arm has ever been tested at.
gui_triple() {
  case "$1" in
    esp32c6|esp32c5) echo "riscv32imac-unknown-none-elf" ;;
    esp32s3)         echo "xtensa-esp32s3-none-elf" ;;
    *)               return 1 ;;
  esac
}
gui_rustflags() {
  case "$1" in
    esp32s3) echo '[]' ;;
    *)       echo '["-C", "force-frame-pointers"]' ;;
  esac
}
# The stack story, per board, in the words the source uses about itself. src/main.rs splits the
# boot assert by board and says the non-C6 value is "PROVISIONAL — the C6 value as a stand-in,
# not a fact (budgets are measured, never inherited)". So the C6 gets its measurement and the
# other two get told they are borrowing it. No margin range is printed for any of them: the
# per-feature margins on record range from +8,912 B down to story_api's 69,304 B, which is
# 2,376 B BELOW the floor — a range that averages a known below-floor case into a reassurance.
gui_stack_note() {
  case "$1" in
    esp32c6) cat <<'S'
Its 71,680 B stack floor is a boot assert the firmware enforces on every start, and on this
board the number is MEASURED (its #65 bracketed the failure). Note the direction of the known
error: smol's own `budget.rs` records the empirical clean line at ~73,000 B, so the declared
floor sits ~1,320 B BELOW it — permissive by a known, signed amount rather than unknown.
S
;;
    *) cat <<'S'
⚠️ This board has NO stack floor of its own. The 71,680 B boot assert it carries is the C6
watch's number standing in — `src/main.rs` labels it "PROVISIONAL … the C6 value as a
stand-in, not a fact (budgets are measured, never inherited)", because the WiFi blob lays its
globals out differently per chip and nothing has bracketed this one. The assert will still
stop an image whose stack gap collapses; it is simply not evidence that this board's real
floor is where the number says.
S
;;
  esac
}

for m in "${MANIFESTS[@]}"; do
  name="$(tget "$m" name)"; chip="$(tget "$m" chip)"
  gui_artifact="$(tget "$m" gui_artifact)"; gui_board="$(tget "$m" gui_board)"
  [ "$gui_artifact" = "true" ] || continue
  [ -n "$gui_board" ] || { echo "release_targets: $name declares gui_artifact but no gui_board" >&2; failed=$((failed+1)); continue; }
  [ -d "$GUI_WS" ] || { echo "release_targets: no GUI workspace at $GUI_WS" >&2; exit 2; }

  triple="$(gui_triple "$chip")" || { echo "release_targets: no GUI triple known for chip '$chip'" >&2; failed=$((failed+1)); continue; }
  aname="$(tget "$m" gui_artifact_name)"; [ -n "$aname" ] || aname="smol-$name-gui"

  echo "== $aname ($chip, gui, $gui_board) =="

  # Xtensa needs the esp toolchain present. Assert it, loudly, instead of emitting a confusing
  # "linker not found" thirty seconds into a build.
  cargo_tc=""
  if [ "$chip" = "esp32s3" ]; then
    if ! rustup toolchain list 2>/dev/null | grep -q '^esp'; then
      echo "FAIL: $aname needs the espup 'esp' toolchain (and a sourced export-esp.sh)" >&2
      failed=$((failed+1)); continue
    fi
    cargo_tc="+esp"
  fi

  # Provision a credentials-free build config for THIS board's triple, then remove it, so the
  # next board cannot inherit the previous one's target.
  rm -f "$GUI_WS/.cargo/config.toml"
  bash "$ROOT/tools/ci_provision_gui.sh" "$GUI_WS" "$triple" "$(gui_rustflags "$chip")" >/dev/null

  # SOURCE_DATE_EPOCH: esp-bootloader-esp-idf stamps the app descriptor from the wall clock
  # unless this is set, so two builds of one commit would differ by minutes. Pinned to the
  # COMMIT's time — it moves a timestamp FIELD, not code layout, so it is a determinism win
  # with no layout risk (unlike a path remap, which is why there isn't one).
  #
  # WATCH_BUILD_HASH: build.rs's own external-hash seam. It exists because fambuild rsyncs
  # without `/.git`, which is exactly our situation — a `git archive` tree. Unset, the sigil
  # stamp would read "no-git"/"unknown"; set, the image names the commit it came from.
  sde="$(git -C "$ROOT" show -s --format=%ct HEAD)"
  elf="$GUI_WS/target/$triple/release/esp32c6-watch"
  rm -f "$elf"
  if ! ( cd "$GUI_WS" && \
         SOURCE_DATE_EPOCH="$sde" WATCH_BUILD_HASH="$GITHASH" \
         CARGO_PROFILE_RELEASE_OPT_LEVEL="$([ "$chip" = esp32s3 ] && echo 2 || echo 3)" \
         cargo $cargo_tc build --release --no-default-features \
               --features "$gui_board" --target "$triple" --bin esp32c6-watch ); then
    echo "FAIL: $aname build" >&2; failed=$((failed+1)); continue
  fi
  [ -f "$elf" ] || { echo "FAIL: $aname produced no ELF at $elf" >&2; failed=$((failed+1)); continue; }

  # Merged flash-at-0x0 image, the watch's own proven packaging: bootloader + partition table +
  # app, against ITS partitions.csv (6 MiB A/B slots + the config partition provision.py
  # writes) at 16mb — all three boards are 16 MB parts, verified per board, and a wrong
  # flash-size would silently place ota_1/config outside the chip.
  bin="$OUT/$aname-$DATE_UTC-g$GITHASH.bin"
  if ! espflash save-image --merge --chip "$chip" \
        --partition-table "$GUI_WS/partitions.csv" --flash-size 16mb \
        "$elf" "$bin" >/dev/null; then
    echo "FAIL: $aname packaging" >&2; failed=$((failed+1)); continue
  fi
  cp "$elf" "${bin%.bin}.elf"      # for probe-rs / readelf / symbol work, as the fleet tier ships
  sha="$(sha256sum "$bin" | cut -c1-64)"

  cat > "$bin.NOTES.md" <<NOTES
# $aname — smol GUI firmware image ($DATE_UTC, git $GITHASH)

**sha256** \`$sha\` · chip **$chip** · flavor **gui** · board feature \`$gui_board\`

This is the **rich-GUI** firmware — the touch/Slint watch shell from \`targets/c6-watch\`, not
the \`rust/clock\` fleet image. Both speak the same SMOLv1 mesh; they are different programs.

**Who this is for:** flashing NEW hardware. Boards already on a smol mesh update over mesh OTA.

**Flash it at 0x0** — this is a merged image (bootloader + partition table + app) built against
the watch's own A/B table (two 6 MiB slots) for a 16 MB part. If this board has ever taken an
OTA, clear \`otadata\` first or the write lands in the slot the bootloader will not select:
\`espflash erase-region --port <port> 0xd000 0x2000\`.

**🔑 Credentials: there are none, and that is deliberate.** Unlike the fleet images, this
artifact carries **no placeholder credentials at all** — not an empty placeholder, genuinely
absent. The watch reads WiFi and broker settings with \`option_env!\` at compile time, and this
image was built from a tree with none set, so the firmware's own defaults apply: an **empty
SSID**, and the compiled-in broker literal **\`192.168.1.10:1883\`**, which will not be your
broker. Fake values were considered and rejected upstream on the grounds that a bogus default
baked into a public artifact is worse than an empty one.

So, to actually use it:

- **WiFi** — join from the on-glass Settings app (there is a scan-based picker and a QWERTY
  keyboard), or provision the config partition from a host with \`tools/provision.py\`.
- **Node id** — \`tools/provision.py --node-id <n>\`. Fresh boards read as the \`42\` "unset"
  sentinel and fall back to a MAC-derived id, so an *allocated* id must be written explicitly.
- **MQTT broker** — this one you cannot set on the device: it is a compile-time constant.
  Point it at your own broker by rebuilding with \`MQTT_BROKER\` in the workspace's
  \`.cargo/config.toml\` \`[env]\`.

**Stack floor.** $(gui_stack_note "$chip" | tr '\n' ' ')

**⚠️ Reproducibility — an honest limit, not a guarantee.** This image is **not** claimed
byte-reproducible. The fleet images are, because there the reproducible build IS the tested
build; here it is not. Byte-identity would need \`--remap-path-prefix\`, which changes
\`.rodata\` string lengths and therefore the memory layout — and on this chip layout is not
cosmetic (its #65 moved a crash from 0% to 100% with an 8-byte shift), so the release build
deliberately keeps the layout the bench builds were tested at instead. **This artifact's
identity is its git hash, \`$GITHASH\`, and the sha256 above for this exact file.**
NOTES
  echo "   $bin"
  echo "   sha256 $sha"
  built=$((built+1))
done

echo "release_targets: $built built · $failed failed"
[ "$failed" -eq 0 ]
