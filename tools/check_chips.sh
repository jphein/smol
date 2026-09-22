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
# `cargo check` only, in the DEFAULT mode. It proves smol's SOURCE compiles for a chip. It does NOT
# link, does not measure a section, does not honour a ChipBudget and does not produce anything
# flashable — those are the `builds` rung (CI) and the publish path (`ota_publish.sh`), and a green
# run here says nothing about either. The ladder is ships => builds => checks; this is the bottom
# rung.
#
# ── `--lint` (#544): A SECOND QUESTION, DELIBERATELY IN A SEPARATE INVOCATION ─────────────────
# Runs `clippy -- -D warnings` instead of `cargo check`, over the same manifest-derived chip list.
#
# WHY IT LIVES HERE rather than in a new script, given the scope paragraph above. #544 proposed a
# separate arm, and the concern it was avoiding was ONE EXIT CODE answering two questions. A
# separate MODE is a separate invocation with its own exit code and its own gate step, so that
# concern does not apply — while a separate FILE would have had to duplicate the four traps this
# script pays for in full: the directory-resolved toolchain, env-scoped `build-std`, the per-chip
# `opt_level`, and the #280 stale-config assertion. Two copies of that dance is a worse bargain
# than one file with two modes, and this repo's history is mostly about second copies of one fact.
#
# WHAT IT EXISTS TO CATCH: `clippy -D warnings` in gate.sh runs on the CANONICAL chip crossed with
# every tier. Nothing lints the others. So a warning in code only a non-canonical chip compiles —
# `s3_oled.rs`, `board_s3.rs`, an S3 arm of `main.rs` — was in nobody's field of view. Measured
# 2026-09-22 on a pristine provision: the S3 carried 4 such diagnostics, and the C3's arm had been
# green throughout.
#
# ⚠️ AND THE PART THAT IS NOT OBVIOUS: THIS IS ALSO A DIFFERENT CLIPPY, NOT ONLY A DIFFERENT CHIP.
# espup's xtensa fork is pinned at 1.95.0.0; every other chip lints on stable (1.97.1 today). Two
# of those four S3 diagnostics were in `sigil.rs` and `familiar/mod.rs` — files the C3 builds and
# lints clean — so they were version divergence, not chip-exclusive code. Attribution was settled by
# inspection (neither site has a chip `cfg`), because the obvious control — the esp toolchain
# targeting riscv — cannot run: that fork ships no prebuilt riscv `core`.
#
# The consequence for whoever hits a spurious-looking lint here: it is REAL on the toolchain that
# reported it. Fix it if the fix is good under both clippys (three of the four were). If the older
# clippy's suggestion is wrong or worse, `#[allow(...)]` it WITH A WRITTEN REASON — there is a
# worked example at `familiar/mod.rs`'s `FAM_CALL` arm, where the suggested match-guard would have
# made an explicit ignore depend on an unrelated `_ => {}` arm continuing to exist. Do NOT disable
# the arm: a lint nobody can see is what this mode was added to end.
#
# Exit 3, distinct from 1, means NOTHING WAS LINTED (no espup, no riscv targets) — a loud skip, not
# a pass. That distinction is the fix reproduced at its own level: an arm that silently measures
# nothing is the gap it was written to close.
#
# ⚠️ RUNNING `--lint` BY HAND WILL LIE TO YOU IF YOUR `board.rs` HAS LOCAL-ONLY SYMBOLS. Those are
# git-ignored and provisioned per tree, so a constant only YOUR copy declares is unused in every
# tier and clippy says so — three times per chip, convincingly, about nothing. Measured twice while
# building this mode: a local `board.rs` produced `WEATHER_LAT/LON/FALLBACK_IP is never used` on all
# three chips, and the second time it came back because an rsync carried the file along. `gate.sh`
# does not have this problem, because it passes `SMOL_CHIPS_CRATE` pointing at the #363 pristine
# mirror. By hand, either provision from the examples first —
#     rm -f rust/clock/src/{board,secrets}.rs && tools/ci_provision.sh rust/clock
# (⚠️ deletes YOUR provisioning) — or point `SMOL_CHIPS_CRATE` at a tree that already is pristine.
#
# USAGE:  tools/check_chips.sh [chip ...]           # default: every chip in the manifest
#         tools/check_chips.sh --lint [chip ...]    # clippy -D warnings, non-canonical chips
#         SMOL_CHIPS_CRATE=<dir> …                  # lint that crate copy instead of this tree's
#
# ⚠️ RUN IT ON familiar, NOT ON katana. Every cargo invocation in this repo is offloaded (JP's
# standing preference — katana's RAM is the constraint), and this script runs up to four of them:
#     ssh familiar 'cd ~/Projects/<worktree> && PATH=$HOME/.cargo/bin:$PATH \
#       CARGO_TARGET_DIR=/var/tmp/ftarget/<name> TMPDIR=/var/tmp/ftarget/tmp tools/check_chips.sh'
# They run STRICTLY ONE AT A TIME, deliberately: parallel cargo builds balloon the cgroup page
# cache and have twice taken out a whole agent scope with an oomd sweep.

set -uo pipefail   # NOT -e: a failing `cargo check` is DATA here, not an error to abort on.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The crate to operate on. Overridable because `tools/gate.sh` may be building the tiers from a
# PRISTINE MIRROR (#363) rather than from this tree, and an arm that lints the tree while the tiers
# lint the mirror is answering a different question about a different input — which is the exact
# measurement bug #363 exists to prevent. Not hypothetical: the first measurement taken for #544
# reported 3 phantom lints on the C6 and C5, all of them unused constants from a local-only
# `board.rs` that `ci_provision.sh` had (correctly) left alone.
#
# `MATRIX` and the #280 config assertion deliberately stay on $ROOT: the manifest is a property of
# the repo, not of whichever copy is being compiled, and the mirror's `.cargo/config.toml` is an
# rsync of this one.
CRATE="${SMOL_CHIPS_CRATE:-$ROOT/rust/clock}"
MATRIX="$ROOT/tools/build_matrix.py"
# Read from the manifest, never written here — the same discipline #413 applied to gate.sh's
# stack arm, so there is no second copy of "the canonical chip is esp32c3" to drift.
CANON="$("$ROOT/tools/build_matrix.py" canonical-chip 2>/dev/null || true)"
# #280 — the stale-config guard. Sourced, because the publish path needs the same function.
# shellcheck source=/dev/null
. "$ROOT/tools/assert_cargo_config.sh"

# Per-chip logs go in the REPO, never /tmp (JP directive 2026-08-25 — katana's /tmp is a 16 GB
# tmpfs, i.e. RAM). Git-ignored via tmp/.gitignore. Absolute, because this script `cd`s to $CRATE.
CHIPS_TMP="$ROOT/tmp"
mkdir -p "$CHIPS_TMP" || { echo "check_chips: cannot create $CHIPS_TMP" >&2; exit 2; }
# The four cargo invocations below inherit this. On familiar, the documented usage above overrides
# both this and CARGO_TARGET_DIR to /var/tmp/ftarget/* — that host's /tmp is a 512 MB tmpfs.
export TMPDIR="$CHIPS_TMP"

# MODE: `check` (the original job) or `lint` (#544). Parsed out of the positional args so the
# existing `check_chips.sh [chip ...]` usage is byte-identical.
MODE=check
want=()
for a in "$@"; do
  case "$a" in
    --lint)  MODE=lint ;;
    --check) MODE=check ;;
    -h|--help) sed -n '2,46p' "$0"; exit 0 ;;
    -*) echo "check_chips: unknown flag $a" >&2; exit 2 ;;
    *)  want+=("$a") ;;
  esac
done

if [ "$MODE" = lint ] && [ -z "$CANON" ]; then
    echo "check_chips: could not resolve meta.canonical_chip from tools/build-matrix.toml" >&2
    echo "             Lint mode needs it, to know which chip gate.sh already covers." >&2
    exit 2
fi

pass=0 fail=0 skip=0 verified=0

# ⚠️ `cd` into the CRATE, never the repo root. rust-toolchain.toml resolves by DIRECTORY, so an
# xtensa build launched from the root silently gets `stable` and fails deep inside xtensa_lx with
# an error that names neither the toolchain nor the directory (#347/1d22f14 documented this trap
# after paying for it).
cd "$CRATE" || { echo "no crate dir at $CRATE" >&2; exit 2; }

while IFS=$'\t' read -r chip target expect toolchain build_std opt_level features; do
    [ -n "$chip" ] || continue
    # `-` is the manifest's sentinel for "this optional field is empty". It exists because a tab is
    # an IFS *whitespace* character, so bash collapses consecutive tabs and every field after an
    # empty one shifts left — which on the first run made this script try `cargo +espnow,cast,io`.
    # Translate back to empty here, once, so the rest of the loop reads naturally.
    [ "$toolchain" = "-" ] && toolchain=""
    [ "$build_std" = "-" ] && build_std=""
    [ "$opt_level" = "-" ] && opt_level=""
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
    # #544 lint mode skips the CANONICAL chip, because gate.sh's tier loop already runs
    # `clippy -D warnings` across every tier on it. Re-running it here would add minutes to say
    # something already said, and would make this arm's verdict overlap another's — which is how
    # two arms come to disagree about one fact.
    if [ "$MODE" = lint ] && [ "$chip" = "$CANON" ]; then
        printf '  \033[2m....\033[0m %-9s covered by gate.sh'"'"'s per-tier clippy — not re-run here\n' "$chip"
        continue
    fi
    # A chip declared `checks = false` cannot be LINTED: it does not compile, so clippy has
    # nothing to say that `check` has not already said louder. Counted as a skip, not a pass —
    # "not applicable" must never read as "clean".
    if [ "$MODE" = lint ] && [ "$expect" != "check" ]; then
        printf '  \033[33mSKIP\033[0m %-9s declared `checks = %s` — cannot lint what does not compile\n' \
               "$chip" "$expect"
        skip=$((skip + 1)); continue
    fi

    # A missing rustup TARGET is an unavailable instrument, exactly like a missing toolchain, and
    # it must not be reported as a lint failure. Without this the arm goes red on any box that has
    # not run `rustup target add` — a fresh CI runner, most obviously — with `can't find crate for
    # \`core\`', which reads as a code defect and is not one. Found while writing this mode's own
    # regression suite; the failure mode was reachable and unhandled.
    #
    # Lint mode only, deliberately: the default `check` mode has the same latent issue, but its
    # pass/fail semantics are a DECLARATION in build-matrix.toml that other things read, and
    # quietly turning one of its FAILs into a SKIP would change what a green run there means.
    # Noted rather than fixed here.
    #
    # ⚠️ AND IT MUST NOT APPLY WHEN THE CHIP USES `build-std`. A Tier-3 target like
    # `xtensa-esp32s3-none-elf` is NOT a rustup-installed target — that is the whole reason
    # `build_std` exists in the manifest for it: there is no prebuilt `core`, so it is compiled
    # from source per invocation. `rustup +esp target list --installed` reports only the host.
    #
    # Getting this wrong was not theoretical: the first version of this guard checked
    # unconditionally, and so SKIPPED the S3 — the one chip #544 was filed about — while still
    # exiting 0. A planted lint in `s3_oled.rs` failed to turn the arm red, and that is the only
    # reason it was found. An arm that excludes its own subject and reports success is the gap
    # this mode exists to close, rebuilt inside the fix.
    if [ "$MODE" = lint ] && [ -z "$build_std" ]; then
        tc_sel=(); [ -n "$toolchain" ] && tc_sel=("+${toolchain}")
        if ! rustup "${tc_sel[@]}" target list --installed 2>/dev/null | grep -qx "$target"; then
            printf '  \033[33mSKIP\033[0m %-9s %s — target not installed (rustup target add %s)\n' \
                   "$chip" "$target" "$target"
            skip=$((skip + 1)); continue
        fi
    fi

    if [ "$MODE" = lint ]; then
        args=(clippy --no-default-features --features "${chip},${features}" --target "$target"
              -- -D warnings)
    else
        args=(check --no-default-features --features "${chip},${features}" --target "$target")
    fi
    [ -n "$toolchain" ] && args=("+${toolchain}" "${args[@]}")

    # #280 — BEFORE the build, not after. This script's own header sends operators to familiar,
    # and familiar is exactly where `.cargo/config.toml` arrives out of band and goes stale: on
    # 2026-08-25 its copy was current except for the S3 arm's `-Tlinkall.x`. `cargo check` would
    # not have noticed (check never links) — it is the LINK that emits 129 undefined references,
    # long after this harness has reported the chip green.
    #
    # Counted as a FAIL, deliberately, not a SKIP. A skip is for "the toolchain is absent", which
    # is not the chip's fault and not a defect; a stale config IS a defect and the tree is wrong
    # until it is fixed. Blurring the two would put a real problem in the column this script
    # already warns reads like a green tick.
    if ! assert_cargo_config "$chip"; then
        printf '  \033[31mFAIL\033[0m %-9s %s — .cargo/config.toml is stale for this chip (#280)\n' \
               "$chip" "$target"
        fail=$((fail + 1)); continue
    fi

    log="$CHIPS_TMP/check-chips-${MODE}-${chip}.log"
    printf '  .... %-9s %s (expect %s)\033[2K\r' "$chip" "$target" "$expect"

    # `SMOL_CHIP` is always passed: riscv32imac cannot tell a C5 from a C6, so build.rs maps that
    # triple to CHIP_UNKNOWN and the wifi-tier assert fails the build until a name is supplied
    # (#349). Harmless where the triple is already unambiguous.
    #
    # build-std goes in the ENV, per invocation, never into .cargo/config.toml — cargo's
    # `[unstable] build-std` is GLOBAL, and a config key would hand it to the riscv builds too.
    # That is the 2026-07-20 regression that leaked portable-atomic/unsafe-assume-single-core into
    # the HOST build and broke every cold C3 build. Env-scoped means a C3 check cannot inherit it.
    # `opt_level` (#398): a per-chip release-profile override — a TOOLCHAIN-BUG workaround seam,
    # env-scoped for the same reason build-std is: a config key would hand it to every chip, and
    # the global profile is what all the C3's recorded measurements were taken against. Inert for
    # `cargo check` (dev profile, no codegen); threaded so the invocation this script models is
    # the SAME one a future build rung will make, sourced from ONE manifest field.
    run_env=(SMOL_CHIP="$chip")
    [ -n "$opt_level" ] && run_env+=(CARGO_PROFILE_RELEASE_OPT_LEVEL="$opt_level")
    if [ -n "$build_std" ]; then
        # shellcheck disable=SC1090
        [ -f "$HOME/export-esp.sh" ] && . "$HOME/export-esp.sh" >/dev/null 2>&1
        run_env+=(CARGO_UNSTABLE_BUILD_STD="$build_std")
    fi
    env "${run_env[@]}" cargo "${args[@]}" >"$log" 2>&1
    rc=$?

    verified=$((verified + 1))
    # Count DIAGNOSTICS, not lines starting with "error". cargo ends a failed compile with its own
    # `error: could not compile \`clock\` (bin "clock") due to 6 previous errors`, which a naive
    # `grep -c '^error'` counts as a seventh — so the first version of this line reported the S3 at
    # 7 and the C5 at 3, one more than each really has. An off-by-one in a number that goes into a
    # commit message is how a measurement becomes folklore.
    errs=$(grep -E '^error' "$log" | grep -vc 'could not compile')
    if [ $rc -eq 0 ] && [ "$expect" = "check" ]; then
        if [ "$MODE" = lint ]; then
            printf '  \033[32mok  \033[0m %-9s %s — lints clean under -D warnings\n' "$chip" "$target"
        else
            printf '  \033[32mok  \033[0m %-9s %s — compiles clean\n' "$chip" "$target"
        fi
        pass=$((pass + 1))
    elif [ $rc -ne 0 ] && [ "$expect" = "fail" ]; then
        printf '  \033[32mok  \033[0m %-9s %s — fails as declared (%d errors, %s)\n' \
               "$chip" "$target" "$errs" "$log"
        pass=$((pass + 1))
    elif [ $rc -ne 0 ] && [ "$MODE" = lint ]; then
        printf '  \033[31mFAIL\033[0m %-9s %s — %d lint(s) under -D warnings: %s\n' \
               "$chip" "$target" "$errs" "$log"
        grep '^error' "$log" | grep -v 'could not compile' | head -5 | sed 's/^/         /'
        printf '         \033[2mNOTE: this chip may lint on a DIFFERENT clippy than the canonical one.\n'
        printf '         espup'"'"'s xtensa fork is pinned at 1.95.0.0; stable is newer. A lint here that\n'
        printf '         stable does not fire is a real divergence, not a false positive — fix it if the\n'
        printf '         fix is good on both, else `#[allow(...)]` it WITH A WRITTEN REASON.\033[0m\n'
        fail=$((fail + 1))
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

if [ "$MODE" = lint ]; then
    echo "  chip lints: $pass clean · $fail with lints · $skip skipped · $verified actually run"
else
    echo "  chips: $pass as declared · $fail wrong · $skip skipped (toolchain absent) · $verified actually run"
fi
[ $fail -eq 0 ] || exit 1
# Anti-vacuity, and in lint mode it is the WHOLE point: on a box with no espup and no riscv
# targets, every chip skips and this script would otherwise exit 0 having linted nothing — which
# is exactly the silent gap #544 was filed about, reproduced inside its own fix. Exit 3, distinct
# from 1 (real lints) and 2 (usage), so a caller can tell "nothing to measure" from "measured and
# bad" and report it as a loud skip rather than a pass.
if [ $verified -eq 0 ]; then
    echo "  nothing was verified — refusing to report success" >&2
    [ "$MODE" = lint ] && exit 3
    exit 1
fi
exit 0
