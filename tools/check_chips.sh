#!/usr/bin/env bash
# check_chips.sh — #347 Part 2. Run the PER-CHIP `cargo check` that `tools/build-matrix.toml`'s
# `checks` field declares the outcome of, and assert the declaration in BOTH directions.
#
# ── WHY THIS EXISTS ───────────────────────────────────────────────────────────────────────────
# The de-pin's acceptance question is "does a target's own feature matrix pass from inside smol's
# tree?" — and until this script there was no way to ask it. `tools/gate.sh` crosses the CANONICAL
# CHIP with every tier; CI's job matrix contains only chips with `builds = true`. Both are correct
# and neither compiles a C5 or a C6. So the chip arms were verified the way they were added: by
# hand, once, by whoever was holding the context, with the result written into a commit message.
# That is exactly the shape #350 exists to end for tiers, applied one axis over.
#
# ── THE PART THAT IS NOT OBVIOUS: FAILURE IS ALSO AN EXPECTATION ──────────────────────────────
# A chip declared `checks = false` must still FAIL. If it starts compiling and nothing notices, the
# manifest now carries a pessimistic lie — `blocked_on` prose describing work already done, which
# this repo has been bitten by often enough to have a rule about it (#347 Part 2 rewrote two such
# `blocked_on` reasons whose stated causes had become false). A stale pessimistic declaration is
# not the safe direction; it is the direction that hides finished work. So `fail` is asserted as
# strictly as `check`, and a chip that unexpectedly compiles fails this gate with instructions to
# flip its row.
#
# ── SCOPE, STATED SO IT CANNOT BE MISREAD AS MORE ─────────────────────────────────────────────
# `cargo check` only. It proves smol's SOURCE compiles for a chip. It does NOT link, does not
# measure a section, does not honour a ChipBudget and does not produce anything flashable — those
# are the `builds` rung (CI) and the publish path (`ota_publish.sh`), and a green run here says
# nothing about either. The ladder is ships => builds => checks; this is the bottom rung.
#
# USAGE:  tools/check_chips.sh [chip ...]      # default: every chip in the manifest
#
# ⚠️ RUN IT ON familiar, NOT ON katana. Every cargo invocation in this repo is offloaded (JP's
# standing preference — katana's RAM is the constraint), and this script runs up to four of them:
#     ssh familiar 'cd ~/Projects/<worktree> && PATH=$HOME/.cargo/bin:$PATH \
#       CARGO_TARGET_DIR=/var/tmp/ftarget/<name> TMPDIR=/var/tmp/ftarget/tmp tools/check_chips.sh'
# They run STRICTLY ONE AT A TIME, deliberately: parallel cargo builds balloon the cgroup page
# cache and have twice taken out a whole agent scope with an oomd sweep.

set -uo pipefail   # NOT -e: a failing `cargo check` is DATA here, not an error to abort on.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE="$ROOT/rust/clock"
MATRIX="$ROOT/tools/build_matrix.py"

want=("$@")
pass=0 fail=0 skip=0 verified=0

# ⚠️ `cd` into the CRATE, never the repo root. rust-toolchain.toml resolves by DIRECTORY, so an
# xtensa build launched from the root silently gets `stable` and fails deep inside xtensa_lx with
# an error that names neither the toolchain nor the directory (#347/1d22f14 documented this trap
# after paying for it).
cd "$CRATE" || { echo "no crate dir at $CRATE" >&2; exit 2; }

while IFS=$'\t' read -r chip target expect toolchain build_std features; do
    [ -n "$chip" ] || continue
    # `-` is the manifest's sentinel for "this optional field is empty". It exists because a tab is
    # an IFS *whitespace* character, so bash collapses consecutive tabs and every field after an
    # empty one shifts left — which on the first run made this script try `cargo +espnow,cast,io`.
    # Translate back to empty here, once, so the rest of the loop reads naturally.
    [ "$toolchain" = "-" ] && toolchain=""
    [ "$build_std" = "-" ] && build_std=""
    if [ ${#want[@]} -gt 0 ]; then
        found=0
        for w in "${want[@]}"; do [ "$w" = "$chip" ] && found=1; done
        [ $found -eq 1 ] || continue
    fi

    # The `esp` channel is not installable from crates.io and is not on CI runners. A missing
    # toolchain is NOT a failure of the chip arm, and must not be reported as one — but it is also
    # not a pass. It is counted separately and printed loudly, because a skip that reads like a
    # green tick is the one outcome worse than a red one.
    # The pattern must allow END-OF-LINE: espup installs the xtensa fork as a bare `esp` with no
    # host-triple suffix, unlike `stable-x86_64-unknown-linux-gnu`. An earlier `^esp[ -]` required a
    # separator that is not there and reported the installed toolchain as absent — a false SKIP,
    # which is the failure mode this whole block is meant to distinguish from a real one.
    if [ -n "$toolchain" ] && ! rustup toolchain list 2>/dev/null | grep -qE "^${toolchain}( |-|\$)"; then
        printf '  \033[33mSKIP\033[0m %-9s %s — toolchain `+%s` not installed (espup)\n' \
               "$chip" "$target" "$toolchain"
        skip=$((skip + 1)); continue
    fi

    # Uniform invocation for EVERY chip, including the C3. The C3 would also be reachable as a
    # bare `cargo check` (its chip feature rides `default`), and using that shortcut here would
    # mean the canonical chip is the one chip this harness checks differently from the others —
    # so the default-features path would be the one never exercised against its explicit form.
    args=(check --no-default-features --features "${chip},${features}" --target "$target")
    [ -n "$toolchain" ] && args=("+${toolchain}" "${args[@]}")

    log="/tmp/check-chips-${chip}.log"
    printf '  .... %-9s %s (expect %s)\033[2K\r' "$chip" "$target" "$expect"

    # `SMOL_CHIP` is always passed: riscv32imac cannot tell a C5 from a C6, so build.rs maps that
    # triple to CHIP_UNKNOWN and the wifi-tier assert fails the build until a name is supplied
    # (#349). Harmless where the triple is already unambiguous.
    #
    # build-std goes in the ENV, per invocation, never into .cargo/config.toml — cargo's
    # `[unstable] build-std` is GLOBAL, and a config key would hand it to the riscv builds too.
    # That is the 2026-07-20 regression that leaked portable-atomic/unsafe-assume-single-core into
    # the HOST build and broke every cold C3 build. Env-scoped means a C3 check cannot inherit it.
    if [ -n "$build_std" ]; then
        # shellcheck disable=SC1090
        [ -f "$HOME/export-esp.sh" ] && . "$HOME/export-esp.sh" >/dev/null 2>&1
        CARGO_UNSTABLE_BUILD_STD="$build_std" SMOL_CHIP="$chip" cargo "${args[@]}" >"$log" 2>&1
    else
        SMOL_CHIP="$chip" cargo "${args[@]}" >"$log" 2>&1
    fi
    rc=$?

    verified=$((verified + 1))
    # Count DIAGNOSTICS, not lines starting with "error". cargo ends a failed compile with its own
    # `error: could not compile \`clock\` (bin "clock") due to 6 previous errors`, which a naive
    # `grep -c '^error'` counts as a seventh — so the first version of this line reported the S3 at
    # 7 and the C5 at 3, one more than each really has. An off-by-one in a number that goes into a
    # commit message is how a measurement becomes folklore.
    errs=$(grep -E '^error' "$log" | grep -vc 'could not compile')
    if [ $rc -eq 0 ] && [ "$expect" = "check" ]; then
        printf '  \033[32mok  \033[0m %-9s %s — compiles clean\n' "$chip" "$target"
        pass=$((pass + 1))
    elif [ $rc -ne 0 ] && [ "$expect" = "fail" ]; then
        printf '  \033[32mok  \033[0m %-9s %s — fails as declared (%d errors, %s)\n' \
               "$chip" "$target" "$errs" "$log"
        pass=$((pass + 1))
    elif [ $rc -ne 0 ]; then
        printf '  \033[31mFAIL\033[0m %-9s %s — declared `checks = true` but %d errors: %s\n' \
               "$chip" "$target" "$errs" "$log"
        grep '^error' "$log" | head -5 | sed 's/^/         /'
        fail=$((fail + 1))
    else
        # The direction that hides finished work.
        printf '  \033[31mFAIL\033[0m %-9s %s — declared `checks = false` but it COMPILES CLEAN.\n' \
               "$chip" "$target"
        printf '         Flip `checks = true` on [chip.%s] in tools/build-matrix.toml and rewrite\n' "$chip"
        printf '         its `blocked_on` — the stated cause is no longer true.\n'
        fail=$((fail + 1))
    fi
done < <("$MATRIX" chip-checks)

echo "  chips: $pass as declared · $fail wrong · $skip skipped (toolchain absent) · $verified actually run"
[ $fail -eq 0 ] || exit 1
[ $verified -gt 0 ] || { echo "  nothing was verified — refusing to report success" >&2; exit 1; }
exit 0
