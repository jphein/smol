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
expect_floor esp32c3 74208 derived
expect_floor esp32c6 71680 boot-assert
expect_floor esp32s3 72004 observed-sufficient

# ── ARM 2: THE ACCEPTANCE TEST — the false-REJECT window closes ───────────────────────────────
# A .stack of 73,000 B sits INSIDE [72,004, 74,208): above the S3's floor, below the C3's. Before
# this change both verdicts came from the C3's number, so the S3 case was a FATAL on a valid image.
echo
echo "═══ the 2,204 B false-REJECT window — the defect this change exists to close ═══"
export STACK_BYTES=73000
out="$(repro_stack_check /nonexistent-elf esp32s3 2>&1)"; rc=$?
if [ $rc -eq 0 ]; then ok "S3 image with .stack 73,000 PASSES (its floor is 72,004)"
else bad "S3 image with .stack 73,000 was REJECTED — the window did not close: $out"; fi
case "$out" in *observed-sufficient*) ok "and the verdict names the provenance" ;;
                *) bad "verdict omits the provenance: $out" ;; esac

out="$(repro_stack_check /nonexistent-elf esp32c3 2>&1)"; rc=$?
if [ $rc -ne 0 ]; then ok "the SAME .stack 73,000 correctly FATALs for the C3 (floor 74,208)"
else bad "C3 accepted 73,000 B against a 74,208 B floor"; fi
case "$out" in *esp32c3*74208*) ok "and the FATAL names the chip and its floor" ;;
                *) bad "FATAL does not name chip+floor: $out" ;; esac

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
