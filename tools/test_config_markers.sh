#!/usr/bin/env bash
# test_config_markers.sh — proof that `tools/assert_cargo_config.sh`'s arms can actually FAIL. (#280)
#
# ── WHY THIS FILE EXISTS ──────────────────────────────────────────────────────────────────────
# The guard shipped in #280 with a both-directions demonstration run by hand on familiar. That is
# not the same as a test, and morpheus-depin3 named the exact reason while reviewing: familiar's
# `.cargo/config.toml` is **currently not stale**, so a green run there today demonstrates nothing
# about the guard catching anything. A check whose FAILING path depends on the world happening to
# be broken is a check nobody has watched fail.
#
# So the failing state is a FIXTURE, not a wait. Every arm below is driven from a config written
# here, and the healthy arm is asserted too — a guard that refuses everything would satisfy "it
# can fail" while being useless.
#
# ── THE ARM THAT MATTERS MOST IS #4 ───────────────────────────────────────────────────────────
# It asserts the SECTION SCOPE by proving the naive check would pass on the same bytes: a
# whole-file `grep -F -- -Tlinkall.x` succeeds on the stale fixture, because the two riscv arms
# carry the marker and only the xtensa arm lacks it. That is the real 2026-08-25 drift, and a
# file-wide check would have reported green on precisely the file whose staleness costs 129
# undefined references. If someone ever "simplifies" the guard to a file-wide grep, arm 4 is what
# fails.
#
# USAGE:  tools/test_config_markers.sh        # exit 0 = every arm behaved
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GUARD="$ROOT/tools/assert_cargo_config.sh"
CANON="$ROOT/rust/clock/.cargo/config.toml"
# Fixtures live in the repo's git-ignored tmp/, never /tmp (JP directive: katana's is a tmpfs).
WORK="$ROOT/tmp/test-config-markers"
pass=0; fail=0

ok()  { printf '   \033[32mok\033[0m   %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '   \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
rm -rf "$WORK"; mkdir -p "$WORK"

[ -f "$CANON" ] || { echo "no canonical config at $CANON" >&2; exit 2; }
[ -x "$GUARD" ] || { echo "no guard at $GUARD" >&2; exit 2; }

# ── fixtures ──────────────────────────────────────────────────────────────────────────────────
# healthy = the real file. Using the REAL config rather than a miniature is deliberate: a
# hand-written stand-in would drift from the thing being guarded, which is this issue's own defect.
cp "$CANON" "$WORK/healthy.toml"

# stale = the real 2026-08-25 drift, reconstructed: the xtensa arm loses its linker script and
# NOTHING ELSE changes.
python3 - "$CANON" "$WORK/stale.toml" <<'PY'
import sys
src = open(sys.argv[1]).read()
i = src.index('[target.xtensa-esp32s3-none-elf]')
head, tail = src[:i], src[i:]
tail = tail.replace('    "-C", "link-arg=-Tlinkall.x",\n', '', 1)
open(sys.argv[2], 'w').write(head + tail)
PY

# no_section = the xtensa arm removed entirely (a config predating the S3 target).
python3 - "$CANON" "$WORK/no_section.toml" <<'PY'
import sys, re
src = open(sys.argv[1]).read()
i = src.index('[target.xtensa-esp32s3-none-elf]')
j = src.find('\n[', i + 1)
open(sys.argv[2], 'w').write(src[:i] + (src[j+1:] if j != -1 else ''))
PY

# A manifest whose chip declares NO markers — the gap arm. Minimal but valid for `load()`.
cat > "$WORK/nomarkers.toml" <<'EOF'
[meta]
canonical_chip = "esp32c3"
canonical_tier = "fleet"
[chip.esp32c3]
target = "riscv32imc-unknown-none-elf"
builds = true
ships  = true
checks = true
[tier.fleet]
features = "espnow,cast,io"
EOF

echo "── assert_cargo_config arms (#280)"

# 1 — healthy config passes, every chip. Without this the suite would accept a guard that refuses
#     everything, which "can fail" and is worthless.
allok=1
for chip in esp32c3 esp32s3 esp32c6 esp32c5; do
    "$GUARD" "$chip" "$WORK/healthy.toml" >/dev/null 2>&1 || { allok=0; break; }
done
[ $allok -eq 1 ] && ok "healthy config: every chip passes" \
                 || bad "healthy config: a chip was refused"

# 2 — the stale fixture is REFUSED for the affected chip, exit 1.
out="$("$GUARD" esp32s3 "$WORK/stale.toml" 2>&1)"; rc=$?
if [ $rc -eq 1 ]; then ok "stale xtensa arm: REFUSED (exit 1)"
else bad "stale xtensa arm: expected exit 1, got $rc"; fi

# 3 — the message must NAME the chip and the triple. An operator who is told only "stale" has to
#     go find which of four arms broke, and the triple is the one fact that answers it.
case "$out" in
    *esp32s3*xtensa-esp32s3-none-elf*|*xtensa-esp32s3-none-elf*esp32s3*)
        ok "refusal names the chip AND the triple" ;;
    *)  bad "refusal message lacks chip/triple: $out" ;;
esac

# 4 — THE SECTION SCOPE. The naive check passes on the same bytes; the guard does not.
if grep -qF -- '-Tlinkall.x' "$WORK/stale.toml"; then
    ok "a whole-file grep PASSES on the stale file (so section scope is load-bearing)"
else
    bad "whole-file grep failed — fixture no longer reproduces the real drift"
fi

# 5 — the drift is chip-LOCAL: the C3 arm of that same stale file is intact and must still pass.
#     A guard that failed every chip on one arm's staleness would be unusable during a migration.
if "$GUARD" esp32c3 "$WORK/stale.toml" >/dev/null 2>&1; then
    ok "same stale file: esp32c3 still passes (drift is per-section)"
else
    bad "esp32c3 was refused by an xtensa-only staleness"
fi

# 6 — a config with no such [target.…] section at all is refused, not skipped.
"$GUARD" esp32s3 "$WORK/no_section.toml" >/dev/null 2>&1; rc=$?
[ $rc -eq 1 ] && ok "missing [target.<triple>] section: REFUSED" \
              || bad "missing section: expected exit 1, got $rc"

# 7 — a missing config file is refused (exit 1), not treated as nothing-to-check.
"$GUARD" esp32c3 "$WORK/does-not-exist.toml" >/dev/null 2>&1; rc=$?
[ $rc -eq 1 ] && ok "missing config file: REFUSED" \
              || bad "missing config: expected exit 1, got $rc"

# 8 — MANIFEST GAP is exit 2, not a pass. "Nobody declared markers yet" must never be
#     indistinguishable from "the config is correct" — the vacuous-pass shape.
SMOL_BUILD_MATRIX="$WORK/nomarkers.toml" "$GUARD" esp32c3 "$WORK/healthy.toml" >/dev/null 2>&1; rc=$?
[ $rc -eq 2 ] && ok "chip with no declared markers: exit 2 (gap, not pass)" \
              || bad "manifest gap: expected exit 2, got $rc"

# 9 — an unknown chip errors rather than finding nothing wrong.
"$GUARD" esp32z9 "$WORK/healthy.toml" >/dev/null 2>&1; rc=$?
[ $rc -eq 2 ] && ok "unknown chip: exit 2" \
              || bad "unknown chip: expected exit 2, got $rc"

# 10 — no chip at all is a usage error, not a pass.
"$GUARD" "" "$WORK/healthy.toml" >/dev/null 2>&1; rc=$?
[ $rc -eq 2 ] && ok "no chip argument: exit 2" \
              || bad "empty chip: expected exit 2, got $rc"

echo
echo "   $pass ok, $fail failed"
[ $fail -eq 0 ] || exit 1
