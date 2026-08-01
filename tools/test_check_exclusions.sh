#!/usr/bin/env bash
# test_check_exclusions.sh — #351. Prove `tools/check_exclusions.py` can FAIL, one arm at a time.
#
# ── WHY ───────────────────────────────────────────────────────────────────────
# The thing being replaced is a comment that says "BYTE-FREE" and is never tested. Replacing
# it with a CHECK that is never seen to refuse would be the same mistake with a shell script
# in front of it — and this tree has been bitten by exactly that: `repro_build.sh` is a
# sourced library, so running it bare exits 0 having done nothing, and it was cited as the
# stack gate for weeks.
#
# An ABSENCE check is the worst case of the genre, because its passing state and its broken
# state look identical: "I found no violations" and "I found nothing at all" print the same
# green. So every arm below is a tree or a file set crafted to violate exactly one rule, and
# the suite asserts BOTH that the checker fails AND that it fails with the RIGHT finding.
#
# ── WHAT IS AND IS NOT COVERED HERE ───────────────────────────────────────────
# Covered, with no cargo and no cross toolchain: the claim derivation from source, the
# feature resolution, the leak/subtree/unproven/stale-declaration decisions, and the ELF
# reader's refusal to read an ELF that has no DWARF.
#
# NOT covered here: that a real leak in a real firmware build is detected end to end. That
# is a cargo-scale demonstration; it is written up in the #351 PR with the two numbers
# (default tier: 11 crate files clean → 12 with a planted `net::wled` reference, gate red).
# The `--fileset` inputs below are the RECORDED form of exactly that output.
#
# Exit 0 all arms behaved; 1 otherwise.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CE="$HERE/check_exclusions.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
pass=0; fail=0

note() { printf '   \033[32mok\033[0m   %s\n' "$1"; pass=$((pass + 1)); }
oops() { printf '   \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }

# ── a synthetic crate with the same SHAPE as rust/clock ───────────────────────
# Deliberately tiny and deliberately NOT the real crate: a fixture that tracked the real
# source would go green whenever the real source changed, which is the failure mode of every
# test that asserts against its own subject.
mk_crate() {                                   # mk_crate <dir> [extra main.rs lines]
  local d="$1"; shift
  mkdir -p "$d/src/sub"
  cat > "$d/Cargo.toml" <<'TOML'
[package]
name = "fixture"
version = "0.0.0"

[features]
default = ["hw"]
hw = []
wifi = ["hw"]
espnow = ["wifi"]
wled = ["espnow"]
TOML
  cat > "$d/src/main.rs" <<'RS'
mod always;
// A paragraph of rationale between the cfg and the mod, because that is the house style
// in rust/clock/src/net.rs and a checker that cannot see past it would be blind there.
#[cfg(feature = "wifi")]
mod netz;
#[cfg(feature = "wled")]
mod wled;
#[cfg(feature = "espnow")]
mod sub;
RS
  [ $# -gt 0 ] && printf '%s\n' "$@" >> "$d/src/main.rs"
  : > "$d/src/always.rs"
  : > "$d/src/netz.rs"
  : > "$d/src/wled.rs"
  : > "$d/src/sub/mod.rs"
  : > "$d/src/sub/child.rs"
  echo 'mod child;' > "$d/src/sub/mod.rs"
}

fileset() { local f="$TMP/$1.set"; shift; printf '%s\n' "$@" > "$f"; echo "$f"; }

CRATE="$TMP/crate"; mk_crate "$CRATE"
EMPTY_MANIFEST="$TMP/empty.toml"; : > "$EMPTY_MANIFEST"

# The four tiers, as file sets. Together they observe every gated module at least once,
# which is what makes the GREEN case green rather than vacuous.
SET_DEFAULT=$(fileset default src/main.rs src/always.rs)
SET_WIFI=$(fileset wifi src/main.rs src/always.rs src/netz.rs)
SET_ESPNOW=$(fileset espnow src/main.rs src/always.rs src/netz.rs src/sub/mod.rs src/sub/child.rs)
SET_WLED=$(fileset wled src/main.rs src/always.rs src/netz.rs src/wled.rs src/sub/mod.rs src/sub/child.rs)
ALL=(--fileset "default==$SET_DEFAULT"
     --fileset "wifi=wifi=$SET_WIFI"
     --fileset "espnow=espnow=$SET_ESPNOW"
     --fileset "wled=wled,espnow=$SET_WLED")

# arm <name> <expected-exit> <expected-substring> -- <args…>
arm() {
  local name="$1" want_rc="$2" want="$3"; shift 4
  local out rc
  out="$("$CE" "$@" 2>&1)"; rc=$?
  if [ "$rc" != "$want_rc" ]; then
    oops "$name: expected exit $want_rc, got $rc — $(printf '%s' "$out" | tail -2 | head -1)"
  elif [ -n "$want" ] && ! printf '%s' "$out" | grep -qF -- "$want"; then
    oops "$name: exit $rc but not for '$want' — $(printf '%s' "$out" | tail -2 | head -1)"
  else
    note "$name"
  fi
}

echo "  #351 exclusion-checker arms"

# 1. The green case. Asserted FIRST and asserted to be exit 0 with every claim observed, so
#    that a later arm going red cannot be explained away as "the fixture was always broken".
arm "green — 4 tiers, every claim observed" 0 "4 claims · 4 observed" \
  -- check --crate "$CRATE" --manifest "$EMPTY_MANIFEST" "${ALL[@]}"

# 2. THE ARM THIS ISSUE EXISTS FOR: a module the tier claims to exclude contributed code.
LEAK=$(fileset leak src/main.rs src/always.rs src/wled.rs)
arm "leak — wled.rs present in the default tier" 1 "src/wled.rs contributed code, but wled is OFF" \
  -- check --crate "$CRATE" --manifest "$EMPTY_MANIFEST" \
     --fileset "default==$LEAK" --fileset "wifi=wifi=$SET_WIFI" \
     --fileset "espnow=espnow=$SET_ESPNOW" --fileset "wled=wled,espnow=$SET_WLED"

# 3. A directory module drags its subtree. Checking only the named file would miss the
#    submodule that got linked, which is the SHAPE of a real leak — nobody references
#    `mod.rs`, they reference the child.
SUBLEAK=$(fileset subleak src/main.rs src/always.rs src/sub/child.rs)
arm "leak — a SUBMODULE of an excluded directory module" 1 "src/sub/child.rs contributed code" \
  -- check --crate "$CRATE" --manifest "$EMPTY_MANIFEST" \
     --fileset "default==$SUBLEAK" --fileset "wifi=wifi=$SET_WIFI" \
     --fileset "espnow=espnow=$SET_ESPNOW" --fileset "wled=wled,espnow=$SET_WLED"

# 4. Anti-vacuity. Drop the one tier that builds `wled` and the remaining run "passes" every
#    absence check while having proved nothing about wled.rs. That must be RED.
arm "unproven — no checked tier ever enables wled" 1 "UNPROVEN: src/wled.rs" \
  -- check --crate "$CRATE" --manifest "$EMPTY_MANIFEST" \
     --fileset "default==$SET_DEFAULT" --fileset "wifi=wifi=$SET_WIFI" \
     --fileset "espnow=espnow=$SET_ESPNOW"

# 5. …and the escape hatch, used honestly: a module that genuinely emits no code is declared
#    with a reason, and the run goes green again.
cat > "$TMP/unobs.toml" <<'TOML'
[unobservable]
"src/wled.rs" = "fixture: consts only"
TOML
arm "unobservable — a declared reason makes it green" 0 "1 declared unobservable" \
  -- check --crate "$CRATE" --manifest "$TMP/unobs.toml" \
     --fileset "default==$SET_DEFAULT" --fileset "wifi=wifi=$SET_WIFI" \
     --fileset "espnow=espnow=$SET_ESPNOW"

# 6. …and dishonestly: the reason has stopped applying. #350 learned this on `[exempt]` —
#    a stale exemption is worse than none, because it reads as a considered decision.
arm "unobservable — stale entry, the module IS observed" 1 "but it WAS observed" \
  -- check --crate "$CRATE" --manifest "$TMP/unobs.toml" "${ALL[@]}"

# 7. …and for something that is not a gated module at all.
cat > "$TMP/ghost.toml" <<'TOML'
[unobservable]
"src/nope.rs" = "fixture: does not exist"
TOML
arm "unobservable — names a non-module" 1 "which is not a cfg-gated module" \
  -- check --crate "$CRATE" --manifest "$TMP/ghost.toml" "${ALL[@]}"

# 8. A cfg the checker does not model. Guessing at `any(...)` is how a checker starts quietly
#    passing; exit 2 says "the instrument cannot judge this tree", which is not the same
#    answer as "this tree is fine".
COMPOUND="$TMP/compound"; mk_crate "$COMPOUND"
cat >> "$COMPOUND/src/main.rs" <<'RS'
#[cfg(any(feature = "wifi", feature = "wled"))]
mod tricky;
RS
: > "$COMPOUND/src/tricky.rs"
arm "refuses a compound cfg rather than guess" 2 "compound cfg" \
  -- check --crate "$COMPOUND" --manifest "$EMPTY_MANIFEST" "${ALL[@]}"

# 9. A `mod` whose file is not there. Fail closed: a module the walker cannot find is a
#    module whose claims it cannot evaluate.
MISSING="$TMP/missing"; mk_crate "$MISSING" '#[cfg(feature = "wled")]' 'mod vanished;'
arm "refuses a mod whose file is missing" 2 "refusing to judge a module I cannot find" \
  -- check --crate "$MISSING" --manifest "$EMPTY_MANIFEST" "${ALL[@]}"

# 10. THE VACUOUS-GREEN TRAP, on the real instrument. An ELF with no DWARF yields an empty
#     file set, under which every absence check trivially holds. The release profile ships
#     `debug = false`, so this is the DEFAULT state of a smol build, not a corner case — it
#     is the single most likely way this gate would come to pass while proving nothing.
#     `readelf` itself is the fixture: it is guaranteed present (this checker shells out to
#     it) and it is a stripped distro binary, i.e. exactly a real ELF with no debug info.
NODWARF="$(command -v readelf)"
arm "refuses an ELF with no DWARF" 2 "no DWARF" \
  -- check --crate "$CRATE" --manifest "$EMPTY_MANIFEST" --elf "probe==$NODWARF"

# 11. Same refusal for the recorded form: a file set that does not even contain the crate
#     root is a truncated measurement, not a clean tier.
TRUNC=$(fileset trunc src/always.rs)
arm "refuses a file set with no crate root" 2 "omits src/main.rs" \
  -- check --crate "$CRATE" --manifest "$EMPTY_MANIFEST" --fileset "default==$TRUNC"

printf '  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
