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
  [ "$flavor" = "fleet" ] || { echo "· $name: flavor '$flavor' not buildable here yet (phase 3.1)"; continue; }
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

echo "release_targets: $built built · $failed failed"
[ "$failed" -eq 0 ]
