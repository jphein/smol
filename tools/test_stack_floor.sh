#!/usr/bin/env bash
# test_stack_floor.sh — #413 phase 2. Prove the per-chip stack-floor lookup works AND can FAIL.
#
# ── WHY THIS EXISTS, AND WHY A GREEN GATE RUN IS NOT IT ───────────────────────────────────────
# `repro_stack_floor` used to read the C3's constant by name and `repro_stack_check` applied it to
# whatever ELF it was handed. That gate was GREEN throughout, and its verdict happened to be right
# because the C3's floor (74,208) is the HIGHEST of the three declared floors — so a chip-blind
# check was stricter than a non-C3 chip's own, and the S3's fleet image (116,940) cleared both.
# Right answer, unrelated reason. Nothing in a passing run could have told anyone.
#
# So each case below is a chip/ELF/manifest combination crafted to exercise one arm, and the suite
# asserts the failures as strictly as the passes — `test_build_matrix.sh`'s discipline, and #350's
# lesson that a gate demonstrated only in its passing state is not evidence.
#
# NO cargo, NO hardware, NO ELF. `repro_stack_check` shells out to `readelf`, so a stub earlier on
# PATH lets every stack size be chosen exactly. That is what makes the false-REJECT window (a 2,204
# B band no real image happens to sit in) testable at all.
#
# Exit 0 = every case behaved; 1 = otherwise.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
pass=0; fail=0
ok()   { printf '   \033[32mok\033[0m   %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '   \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }

WORK="$(mktemp -d --tmpdir stack-floor-test-XXXX)"
trap 'rm -rf "$WORK"' EXIT

# ── the readelf stub ─────────────────────────────────────────────────────────────────────────
# Emits `-sW` symbol rows in the real column layout (field 2 = Value, field 8 = Name), so the
# awk in `repro_stack_check` parses it unchanged. STACK_BYTES picks the region size.
mkdir -p "$WORK/bin"
cat > "$WORK/bin/readelf" <<'STUB'
#!/usr/bin/env bash
# stub: only the -sW symbol query is used by repro_stack_check
end=0x40000000
start=$(printf '0x%x' $(( end + ${STACK_BYTES:-100000} )))
echo "Symbol table '.symtab' contains 2 entries:"
echo "   Num:    Value          Size Type    Bind   Vis      Ndx Name"
echo "     1: ${start#0x}     0 NOTYPE  GLOBAL DEFAULT  ABS _stack_start"
echo "     2: ${end#0x}     0 NOTYPE  GLOBAL DEFAULT  ABS _stack_end"
STUB
chmod +x "$WORK/bin/readelf"
export PATH="$WORK/bin:$PATH"

# shellcheck source=tools/repro_build.sh
. "$ROOT/tools/repro_build.sh"

# ── ARM 1: each chip's floor + provenance parses, from the REAL budget.rs ─────────────────────
echo
echo "═══ the three floors are three different KINDS of number ═══"
expect_floor() { # <chip> <bytes> <provenance>
  local got; got="$(repro_stack_floor "$1" 2>&1)"
  if [ "$got" = "$2 $3" ]; then ok "$1 → $2 $3"; else bad "$1 → wanted '$2 $3', got '$got'"; fi
}
expect_floor esp32c3 64475 derived
expect_floor esp32c6 71680 boot-assert
expect_floor esp32s3 72004 observed-sufficient

# ── ARM 2: THE ACCEPTANCE TEST — per-chip discrimination, and the DANGER DIRECTION HAS INVERTED ─
#
# ⚠️ THIS IS A FIXTURE WHOSE **MEANING** MOVED WHILE ITS **VALUE** STOOD STILL — the most
# expensive kind to miss, because nothing goes red. The probe stayed 73,000 B and stayed a
# perfectly valid number; what changed underneath it was which side of every floor it landed on.
# A stale VALUE fails loudly. A stale MEANING passes, and the arm reports success while covering
# nothing at all.
#
# This arm was reworked, not renumbered, when #335 STEP T moved the C3 floor 74,208 → 64,475.
# **The C3 used to be the HIGHEST floor in the tree and is now the LOWEST** (C3 64,475 · C6 71,680
# · S3 72,004), and the old probe value silently stopped discriminating: 73,000 B is now ABOVE
# every floor, so both chips accept it and the arm would have passed while testing nothing.
#
# The inversion also flips what chip-blindness COSTS, which is the part worth reading:
#   * BEFORE — everything used the C3's 74,208, the highest. A valid S3 image in [72,004, 74,208)
#     was FALSELY REJECTED. Annoying, loud, and safe: nothing shipped.
#   * NOW — the C3's 64,475 is the lowest. Chip-blindness would FALSELY ACCEPT a C6/S3 image in
#     [64,475, 72,004). **That ships an image too thin for its own chip**, which is the failure
#     direction that reaches hardware instead of a console.
# So this arm guards something strictly more dangerous than it did when it was written, and its
# probe must sit in the new gap rather than the old one.
echo
echo "═══ per-chip discrimination — and post-T the blind failure would be a false ACCEPT ═══"
export STACK_BYTES=68000
out="$(repro_stack_check /nonexistent-elf esp32c3 2>&1)"; rc=$?
if [ $rc -eq 0 ]; then ok "C3 image with .stack 68,000 PASSES (its floor is 64,475)"
else bad "C3 image with .stack 68,000 was REJECTED against a 64,475 B floor: $out"; fi
case "$out" in *derived*) ok "and the verdict names the provenance" ;;
                *) bad "verdict omits the provenance: $out" ;; esac

out="$(repro_stack_check /nonexistent-elf esp32s3 2>&1)"; rc=$?
if [ $rc -ne 0 ]; then ok "the SAME .stack 68,000 correctly FATALs for the S3 (floor 72,004)"
else bad "S3 ACCEPTED 68,000 B against a 72,004 B floor — a chip-blind FALSE ACCEPT, which ships"; fi
case "$out" in *esp32s3*72004*) ok "and the FATAL names the chip and its floor" ;;
                *) bad "FATAL does not name chip+floor: $out" ;; esac

out="$(repro_stack_check /nonexistent-elf esp32c6 2>&1)"; rc=$?
if [ $rc -ne 0 ]; then ok "and for the C6 too (floor 71,680) — both non-C3 chips reject it"
else bad "C6 ACCEPTED 68,000 B against a 71,680 B floor — chip-blind false accept"; fi

# ── ARM 3: a genuinely thin stack still fails, for every chip ────────────────────────────────
echo
echo "═══ the regression it actually guards: .bss growth collapsing .stack ═══"
export STACK_BYTES=2592   # the #300 bard image's real linked stack
for c in esp32c3 esp32c6 esp32s3; do
  out="$(repro_stack_check /nonexistent-elf "$c" 2>&1)"
  if [ $? -ne 0 ]; then ok "$c refuses a 2,592 B stack (the #300 case)"
  else bad "$c ACCEPTED a 2,592 B stack"; fi
done
unset STACK_BYTES

# ── ARM 4: fail-closed arms ──────────────────────────────────────────────────────────────────
echo
echo "═══ fail-closed: every unknown must refuse, never default ═══"
out="$(repro_stack_check /nonexistent-elf 2>&1)"; [ $? -ne 0 ] \
  && case "$out" in *"needs a chip"*) ok "no chip → refuses (no silent C3 default)" ;;
                    *) bad "wrong refusal for a missing chip: $out" ;; esac \
  || bad "a missing chip was ACCEPTED"

out="$(repro_stack_floor esp32c9 2>&1)"; [ $? -ne 0 ] \
  && ok "an undeclared chip → refuses" || bad "undeclared chip returned '$out'"

out="$(repro_stack_floor 'esp32c3; rm -rf /' 2>&1)"; [ $? -ne 0 ] \
  && ok "an implausible chip name → refuses before it reaches awk" \
  || bad "implausible chip name accepted"

# A budget.rs carrying a FloorProvenance variant the shell has not been taught. This is the
# deliberate friction: adding a Rust variant must NOT silently acquire a shell meaning.
sed -e 's/FloorProvenance::Derived;/FloorProvenance::VibesBased;/' \
    "$ROOT/rust/clock/src/budget.rs" > "$WORK/budget-unknown-prov.rs"
out="$(REPRO_BUDGET_RS="$WORK/budget-unknown-prov.rs" repro_stack_floor esp32c3 2>&1)"; [ $? -ne 0 ] \
  && case "$out" in *"must be taught"*) ok "an UNKNOWN FloorProvenance variant → refuses" ;;
                    *) bad "wrong refusal for an unknown variant: $out" ;; esac \
  || bad "an unknown FloorProvenance variant was silently accepted as '$out'"

# A budget.rs with the floor const deleted — the #348 drift case.
grep -v '^pub const ESP32S3_STACK_FLOOR_BYTES' "$ROOT/rust/clock/src/budget.rs" > "$WORK/budget-no-floor.rs"
out="$(REPRO_BUDGET_RS="$WORK/budget-no-floor.rs" repro_stack_floor esp32s3 2>&1)"; [ $? -ne 0 ] \
  && ok "a missing floor const → refuses" || bad "missing floor const returned '$out'"

# ── ARM 5: the operator override is honest about having no provenance ────────────────────────
echo
echo "═══ the escape hatch does not get to claim a provenance it does not have ═══"
export STACK_BYTES=100000
out="$(REPRO_STACK_FLOOR=50000 repro_stack_check /nonexistent-elf esp32s3 2>&1)"
case "$out" in *operator-override*) ok "REPRO_STACK_FLOOR prints 'operator-override', not 'derived'" ;;
                *) bad "override did not label itself: $out" ;; esac
unset STACK_BYTES

echo
echo "════════════════════════════════════════════"
printf '  %d passed · %d failed\n' "$pass" "$fail"
echo "════════════════════════════════════════════"
[ "$fail" -eq 0 ]
