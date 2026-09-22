#!/usr/bin/env bash
# test_lint_chips.sh — prove `check_chips.sh --lint` (#544) CAN FAIL, and can tell the three
# outcomes apart.
#
# ── WHY A STUB SUITE AND NOT A REAL PLANTED WARNING ───────────────────────────────────────────
# #544's acceptance asks for a demonstration with a planted warning rather than an assertion that
# the tree is clean, and it is right to: a green run shows nothing about whether an arm can see.
# That demonstration was done, end to end, with real clippy on real hardware toolchains — a
# `let _x = 1;` planted in `s3_oled.rs`, the arm going red, the plant removed. It is recorded in
# the commit rather than automated here, because automating it costs a full xtensa clippy run
# (~30 s warm, minutes cold) on a toolchain CI does not have, for a fact that does not change.
#
# What DOES change, and what this suite therefore covers, is the decision logic around the
# invocation: which chips it skips and why, which outcome an exit code maps to, and whether
# "nothing was linted" can masquerade as "everything is clean". That is where the bugs were — the
# missing-target case below was a live defect in the first draft of the mode, found by writing this
# file and not by running the arm.
#
# `cargo` and `rustup` are stubbed on PATH. No compile, no network, seconds to run. Same shape as
# tools/test_stack_floor.sh's readelf stub, and for the same stated reason: a suite that can only
# demonstrate the passing case is not evidence.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pass=0 fail=0

STUBS="$(mktemp -d)"
trap 'rm -rf "$STUBS"' EXIT

# ── the stubs ─────────────────────────────────────────────────────────────────────────────────
# cargo: exits with $STUB_CARGO_RC, or with $STUB_FAIL_RC for the chip named in $STUB_FAIL_CHIP
# (matched on the --features argument, which is where the chip name appears).
cat > "$STUBS/cargo" <<'EOS'
#!/usr/bin/env bash
args="$*"
if [ -n "${STUB_FAIL_CHIP:-}" ] && printf '%s' "$args" | grep -q "features $STUB_FAIL_CHIP,"; then
    echo "error: a planted lint"
    echo "error: could not compile \`clock\` (bin \"clock\") due to 1 previous error"
    exit "${STUB_FAIL_RC:-101}"
fi
exit "${STUB_CARGO_RC:-0}"
EOS

# rustup: reports the toolchains in $STUB_TOOLCHAINS and the targets in $STUB_TARGETS.
cat > "$STUBS/rustup" <<'EOS'
#!/usr/bin/env bash
case "$*" in
    *"toolchain list"*) printf '%s\n' ${STUB_TOOLCHAINS:-} ; exit 0 ;;
    *"target list --installed"*) printf '%s\n' ${STUB_TARGETS:-} ; exit 0 ;;
esac
exit 0
EOS
chmod +x "$STUBS/cargo" "$STUBS/rustup"

ALL_TARGETS="riscv32imc-unknown-none-elf riscv32imac-unknown-none-elf xtensa-esp32s3-none-elf"

# Strip ANSI so assertions can match across a colour reset. Without this, `SKIP +esp32s3` cannot
# match `\033[33mSKIP\033[0m esp32s3` — which cost three false failures on this suite's first run,
# and is the third time in this lane that the instrument was wrong rather than the thing measured.
plain() { sed -r 's/\x1B\[[0-9;]*[mK]//g'; }

# run <name> <expected-rc> <expected-grep-or-> [env assignments...]
run() {
    local name="$1" want_rc="$2" want_re="$3"; shift 3
    local out rc ok=1 why=""
    out=$(env PATH="$STUBS:$PATH" "$@" "$ROOT/tools/check_chips.sh" --lint 2>&1 | plain); rc=${PIPESTATUS[0]}
    [ "$rc" = "$want_rc" ] || { ok=0; why="exit $rc, wanted $want_rc"; }
    if [ "$want_re" != "-" ] && ! printf '%s' "$out" | grep -qE "$want_re"; then
        ok=0; why="${why}${why:+; }no match for /$want_re/"
    fi
    if [ "$ok" = 1 ]; then
        printf '  \033[32mok  \033[0m %s\n' "$name"; pass=$((pass + 1))
    else
        printf '  \033[31mFAIL\033[0m %s — %s\n' "$name" "$why"
        printf '%s\n' "$out" | sed 's/^/         /' | head -14
        fail=$((fail + 1))
    fi
}

echo "test_lint_chips: the arm's decision logic, with cargo and rustup stubbed"

# 1. THE ONE THAT MATTERS: a chip with lints must FAIL. If only this case is ever added, add this.
run "a chip with lints FAILS (exit 1)" 1 "FAIL +esp32s3 .*lint\\(s\\) under -D warnings" \
    STUB_TOOLCHAINS="stable esp" STUB_TARGETS="$ALL_TARGETS" STUB_FAIL_CHIP=esp32s3

# 2. and it must say WHY it might be a version divergence, since that is the confusing case.
run "a failure explains the clippy-version divergence" 1 "1\.95\.0\.0|WRITTEN REASON" \
    STUB_TOOLCHAINS="stable esp" STUB_TARGETS="$ALL_TARGETS" STUB_FAIL_CHIP=esp32s3

# 3. all clean → exit 0.
run "all chips clean (exit 0)" 0 "lints clean" \
    STUB_TOOLCHAINS="stable esp" STUB_TARGETS="$ALL_TARGETS"

# 4. the canonical chip is NOT re-linted here (gate.sh's tier loop already does it).
run "canonical chip is not re-run" 0 "covered by gate\.sh" \
    STUB_TOOLCHAINS="stable esp" STUB_TARGETS="$ALL_TARGETS"

# 5. a missing TOOLCHAIN is a loud skip, not a failure — the others still lint.
run "absent toolchain skips, others still run" 0 "SKIP +esp32s3.*toolchain" \
    STUB_TOOLCHAINS="stable" STUB_TARGETS="$ALL_TARGETS"

# 6. a missing TARGET is a loud skip too. THIS WAS A REAL DEFECT: without the guard, a fresh box
#    that has not run `rustup target add` fails with "can't find crate for core", which reads as a
#    code problem and is not one.
run "absent target skips, not fails" 0 "SKIP +esp32c6.*target not installed" \
    STUB_TOOLCHAINS="stable esp" STUB_TARGETS="riscv32imc-unknown-none-elf xtensa-esp32s3-none-elf"

# 6b. ⭐ A build-std chip must NOT be skipped for a "missing" target. THE DEFECT THIS SUITE MISSED:
#     the first guard checked unconditionally, so the S3 — a Tier-3 target with no prebuilt core,
#     which is why the manifest gives it `build_std` — was skipped as "target not installed" while
#     the arm still exited 0. The stub suite passed, because the stub said the target WAS installed.
#     A real planted lint in s3_oled.rs is what caught it. This case is that bug, frozen.
run "a build-std chip is linted even with no rustup target" 1 "FAIL +esp32s3" \
    STUB_TOOLCHAINS="stable esp" STUB_TARGETS="riscv32imc-unknown-none-elf riscv32imac-unknown-none-elf" \
    STUB_FAIL_CHIP=esp32s3

# 7. ⭐ NOTHING linted must NOT look like success. Exit 3, distinct from 1 and 2, so the caller can
#    print a loud skip. This is the arm reproducing #544's own gap at its own level: an arm that
#    silently measures nothing is exactly the condition that let S3 warnings go unseen.
run "nothing linted → exit 3, not 0" 3 "refusing to report success" \
    STUB_TOOLCHAINS="stable" STUB_TARGETS="riscv32imc-unknown-none-elf"

# 8. the default (check) mode must be untouched by any of the above.
out=$(env PATH="$STUBS:$PATH" STUB_TOOLCHAINS="stable esp" STUB_TARGETS="$ALL_TARGETS" \
        "$ROOT/tools/check_chips.sh" 2>&1 | plain); rc=${PIPESTATUS[0]}
if [ "$rc" = 0 ] && printf '%s' "$out" | grep -q "compiles clean" \
   && ! printf '%s' "$out" | grep -q "lints clean"; then
    printf '  \033[32mok  \033[0m %s\n' "default mode still says 'compiles clean', unchanged"
    pass=$((pass + 1))
else
    printf '  \033[31mFAIL\033[0m %s (exit %s)\n' "default mode changed" "$rc"
    printf '%s\n' "$out" | sed 's/^/         /' | head -10
    fail=$((fail + 1))
fi

# 9. an unknown flag is a usage error (2), not silently ignored as a chip name.
out=$(env PATH="$STUBS:$PATH" "$ROOT/tools/check_chips.sh" --lnit 2>&1 | plain); rc=${PIPESTATUS[0]}
if [ "$rc" = 2 ]; then
    printf '  \033[32mok  \033[0m %s\n' "a typo'd flag is exit 2, not a silent no-op"
    pass=$((pass + 1))
else
    printf '  \033[31mFAIL\033[0m typo flag gave exit %s, wanted 2\n' "$rc"; fail=$((fail + 1))
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
[ "$pass" -gt 0 ] || { echo "no assertions ran — refusing to report success" >&2; exit 1; }
exit 0
