#!/usr/bin/env bash
# #338 THE GATE. One script, run identically by CI (.github/workflows/fw-gate.yml) and by a human
# before pushing. It exists because the multi-tier gate everyone cited was PROSE in ONBOARDING.md —
# and on 2026-08-01 main was found red on its own `clippy -D warnings` rule with nobody aware, a new
# feature (`ledger-provision`) was added with no matrix to add it to, and two agents independently
# hand-built baselines to compute a clippy delta. A gate that lives in prose has already stopped
# running.
#
# ONBOARDING.md points AT this file rather than restating the commands, so the two cannot drift.
#
# WHAT IT COVERS
#   0. #350 the build-matrix declarations — the canonical tier matches REPRO_FLEET_FEATURES, every
#      buildable chip has a #348 ChipBudget (and vice versa), nothing ships that nothing builds,
#      and the matrix is one-axis-at-a-time rather than a cross product
#   1. cargo check --release across the build tiers — DERIVED from tools/build-matrix.toml, not
#      listed here (#350). Adding a chip or a tier is a data change in one file.
#   2. cargo clippy --release -D warnings on EVERY tier (#343 — was canonical-only), from the
#      same manifest, so the two lists cannot disagree the way they had already started to
#   3. the host experiments/*_verify suites
#   4. the #300 stack floor — the canonical ELF's .stack region vs the floor declared in
#      rust/clock/src/budget.rs (#348: one definition, parsed by repro_stack_floor). The number
#      is deliberately NOT repeated in this file, in build-matrix.toml, or anywhere else.
#   5. the crate's own tests/*.rs suites via cargo test (#350) — bard / budget / input. Nothing
#      ran these before: 57 tests, including the Bard's bit-for-bit golden, that no gate executed
#   6. tools/test_build_matrix.sh — proof that (0)'s arms can actually fail
#   7. #351 the BYTE-FREE tier claims, in two arms. (a) build-matrix.toml's [tier_exclusive]
#      must still agree with the `#[cfg(feature = …)]` in the source, both directions — this
#      catches the regression (b) structurally cannot, where deleting a gate deletes the claim
#      along with it. (b) each tier is LINKED and its DWARF line table is asked which source
#      files contributed code, so "the default build is BYTE-FREE of it" is a measurement
#      instead of a comment.
#   8. tools/test_check_exclusions.sh — proof that (7)'s arms can actually fail, including the
#      vacuous-green one (an ELF with no DWARF has an empty file set and every absence "holds")
#
# WHAT IT DOES NOT COVER — read this before trusting a green run:
#   * `mesh-test`: cannot RUN — it needs a per-board `DEAF_MACS` that only a real board.rs has.
#     It does now COMPILE, as a tier (#350). "Cannot run" had been silently widened to "cannot
#     build", which is how it went uncompiled for months.
#   * espflash `save-image` packaging: not run (no espflash in CI). The stack number does not depend
#     on it — it is read from the ELF — but image PACKAGING is therefore unproven here.
#   * Anything requiring hardware: OTA, radio, election, the stack-paint HIGH-WATER measurement.
#     ⚠️ The stack check bounds the linked REGION, not runtime high-water. A struct living in a
#     stack-resident RadioManager can cost ~1.8 KB of real stack and move the region by ~32 B. A
#     green stack line is NOT evidence of runtime headroom (see repro_stack_check's comment).
#   * #351 proves no CODE from an excluded module survived. A module of pure `const`/`static`
#     items lands in .rodata with no line-table rows and is invisible to it — `src/secrets.rs`
#     is exactly that, and is declared in build-matrix.toml's [unobservable] for the reason.
#     "BYTE-FREE" is therefore verified for executable bytes, not literally every byte.
#
# Usage:  tools/gate.sh              # everything
#         tools/gate.sh host         # host suites only (no riscv toolchain needed)
#         tools/gate.sh fw           # tiers + clippy + stack only
#         tools/gate.sh excl         # #351 tier exclusions only
# Env:    CARGO_TARGET_DIR honoured; SMOL_GATE_JOBS caps cargo parallelism.
#
# `excl` is its own mode because it is the only arm that LINKS every tier, and it needs a
# different cargo PROFILE (`CARGO_PROFILE_RELEASE_DEBUG`) in a different target dir — mixing
# that into `fw` would thrash the fingerprints the stack-floor build depends on and poison one
# rust-cache with two profiles' artifacts. CI runs the two as parallel jobs. A human running
# `tools/gate.sh` still gets all of it.
#
# MEASURED, and not what was first assumed: 179 s for all ten links on a cold GitHub runner
# (job "tier exclusions (#351)", 2m59s). The first estimate here said ~700 s, from a local
# experiment that gave each tier its OWN target dir and so paid for ten cold DEPENDENCY builds.
# The shipped path shares one dir, where only the crate and the LTO link are per-tier. Cost is
# therefore NOT the reason for the split — cache and profile hygiene is.
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
CLOCK="rust/clock"
WHAT="${1:-all}"
FAILED=()
run_fw=1; run_host=1; run_excl=1
case "$WHAT" in
  host) run_fw=0; run_excl=0 ;;
  fw)   run_host=0; run_excl=0 ;;
  excl) run_host=0; run_fw=0 ;;
  all)  ;;
  *) echo "usage: tools/gate.sh [all|host|fw|excl]" >&2; exit 2 ;;
esac

# shellcheck source=/dev/null
. "$ROOT/tools/repro_build.sh"   # REPRO_TARGET, REPRO_FLEET_FEATURES, repro_cargo_args, repro_stack_check

JOBS=()
[ -n "${SMOL_GATE_JOBS:-}" ] && JOBS=(-j "$SMOL_GATE_JOBS")

# Where the stack verdict is left for a caller (CI lifts it into the PR job summary).
STACK_REPORT="${SMOL_GATE_STACK_REPORT:-/tmp/gate-stack-report}"
rm -f "$STACK_REPORT"

step() { printf '\n\033[1m── %s\033[0m\n' "$1"; }
ok()   { printf '   \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { printf '   \033[31mFAIL\033[0m %s\n' "$1"; FAILED+=("$1"); }

# Both firmware halves need `board.rs`/`secrets.rs`, so this runs for either — `excl` on its
# own in CI would otherwise fail on the missing files rather than on anything it measures.
if [ "$run_fw" = 1 ] || [ "$run_excl" = 1 ]; then
  step "provisioning (git-ignored; existing files untouched)"
  "$ROOT/tools/ci_provision.sh" "$CLOCK" || { echo "provisioning failed" >&2; exit 1; }
fi

if [ "$run_fw" = 1 ]; then
  # Cheap and first, so a source-consistency error is not buried behind ten minutes of LTO.
  # #339: the prose describing the DIAG shed order had drifted from the appends it described, in a
  # direction that would send an operator to the wrong fields. Prose cannot be tested; this can.
  step "DIAG shed order matches its declaration (#339)"
  if out=$("$ROOT/tools/check_shed_order.py" "$CLOCK/src/net/mode.rs" 2>&1); then
    printf '%s\n' "$out"; ok "shed order"
  else
    printf '%s\n' "$out"; bad "shed order"
  fi

  # #350: cheap and before any compile, because everything below DERIVES from this manifest —
  # a gate that builds the wrong tier list confidently is worse than one that refuses to start.
  # The arms: the canonical tier matches REPRO_FLEET_FEATURES (the packaging path's own
  # definition); every buildable chip has a #348 ChipBudget and every ChipBudget has a chip row;
  # nothing ships that nothing builds; the emitted matrix is one-axis-at-a-time, not a cross
  # product. `tools/test_build_matrix.sh` (host half) proves each of those can fail.
  step "build matrix declarations (#350)"
  if out=$("$ROOT/tools/build_matrix.py" check 2>&1); then
    printf '%s\n' "$out"; ok "build matrix"
  else
    printf '%s\n' "$out"; bad "build matrix"
  fi

  # The tiers. `default` is the always-green baseline; `wifi`/`espnow` are the documented rungs;
  # the canonical fleet tier is what actually ships. A feature added to Cargo.toml and NOT added
  # here is a code path nothing compiles — the `ledger-provision` case that motivated this issue.
  # Any feature listed in Cargo.toml's [features] that is not covered below should be added.
  # #347: `bard` left the canonical fleet list (it starves the C3's runtime stack), so WITHOUT an
  # explicit tier here nothing would compile it — precisely the "code path nothing compiles" case
  # this gate exists to prevent, and it would rot exactly while it is being kept alive for the S3
  # and C6. The tier is `bard` PLUS the fleet list rather than `bard` alone, because the build that
  # has to keep working on a bigger chip is the COMBINED one. `bard` is named first so the
  # feature-not-on-this-branch SKIP below tests for `bard` and not for `espnow`.
  # #348: the bard tier now also carries `off-fleet`. That feature is the DECLARATION that this
  # build is not the C3 fleet image — which is precisely what this tier is — and without it the
  # #348 budget predicate refuses to compile the Bard on a C3, killing the tier that exists to
  # keep it alive. Drop `off-fleet` here and you will see the guard fire; that is the check
  # working, not the tier breaking. `repro_build_bin` refuses to PACKAGE anything naming it, so
  # this cannot leak into a shipped image.
  # #350: the tier list is DERIVED from tools/build-matrix.toml, not written here. It used to
  # be a literal in this loop and a second literal in the clippy loop below, and the two had
  # already drifted — `ledger-provision` was in one and not the other, so that tier compiled
  # but was never linted. One declaration, two consumers, no third place to forget.
  step "cargo check — build tiers"
  while IFS=$'\t' read -r name feats; do
    # A tier naming a feature this branch does not have (e.g. ledger-provision before #181 lands)
    # is SKIPPED, not failed — the gate must work on both sides of that merge.
    if [ -n "$feats" ] && ! grep -qE "^${feats%%,*} *=" "$CLOCK/Cargo.toml"; then
      printf '   \033[33mSKIP\033[0m %-16s (feature not in Cargo.toml on this branch)\n' "$name"; continue
    fi
    args=(--release "${JOBS[@]}"); [ -n "$feats" ] && args+=(--features "$feats")
    if (cd "$CLOCK" && cargo check "${args[@]}") >/tmp/gate-$name.log 2>&1; then
      ok "check $name"
    else
      bad "check $name"; tail -15 /tmp/gate-$name.log | sed 's/^/        /'
    fi
  done < <("$ROOT/tools/build_matrix.py" emit --for check)

  # `-D warnings` on EVERY tier, not just the canonical one. Until #343 this ran on the canonical
  # tier alone because `default`/`wifi` carried dead-code findings (symbols whose only callers are
  # espnow-gated); those now carry item-scoped `#[allow(dead_code)]` with a cited reason, so the
  # gate can cover what ONBOARDING has claimed all along. A tier that only gets `cargo check` has
  # its new warnings invisible — which is how the findings accumulated unnoticed in the first place.
  # #350: derived from the same manifest as the check loop, so "every tier" is now literally
  # true instead of "every tier someone remembered to add here". The fleet tier is named
  # `fleet` in both loops now — it was `canonical` here and `fleet` above, which is why the
  # two lists could disagree without looking like they did. Log path moves with the name:
  # /tmp/gate-clippy-fleet.log, was /tmp/gate-clippy-canonical.log.
  step "cargo clippy -D warnings — every tier"
  while IFS=$'\t' read -r name feats; do
    if [ -n "$feats" ] && ! grep -qE "^${feats%%,*} *=" "$CLOCK/Cargo.toml"; then
      printf '   \033[33mSKIP\033[0m %-16s (feature not in Cargo.toml on this branch)\n' "$name"; continue
    fi
    args=(--release "${JOBS[@]}"); [ -n "$feats" ] && args+=(--features "$feats")
    if (cd "$CLOCK" && cargo clippy "${args[@]}" -- -D warnings) >/tmp/gate-clippy-$name.log 2>&1; then
      ok "clippy $name"
    else
      bad "clippy $name"
      grep -E "^(error|warning)" /tmp/gate-clippy-$name.log | grep -v "could not compile" \
        | sort -u | sed 's/^/        /' | head -12
    fi
  done < <("$ROOT/tools/build_matrix.py" emit --for clippy)
fi

if [ "$run_excl" = 1 ]; then
  # #351 the BYTE-FREE claims. Cargo.toml, net.rs and main.rs all assert that a lower tier
  # contains none of a higher tier's code ("the default build is BYTE-FREE of it (symbol-
  # absence provable)"), and until now nothing checked a single one of them — the same
  # species of comment as this file's old "any feature not covered below should be added",
  # which #350 found to be false for three features at once.
  #
  # The claims are DERIVED from the `#[cfg(feature = …)]` on each `mod` declaration, walked
  # transitively from src/main.rs, so there is no second hand-written list to drift. The
  # tiers come from the same manifest as the two loops above.
  #
  # This arm LINKS each tier, which the check/clippy loops do not, because the property is
  # about what survives into the binary. Two consequences worth knowing before reading a
  # green line:
  #   * it needs DWARF, and `[profile.release]` sets `debug = false`, so the builds carry
  #     CARGO_PROFILE_RELEASE_DEBUG=line-tables-only. Debug info is not an optimisation
  #     input — MEASURED on this tree, .text/.rodata/.bss/.stack come out the same SIZE with
  #     and without it — and it lands only in non-ALLOC sections, so no shipped byte moves.
  #     The checker REFUSES an ELF with no DWARF rather than reading an empty file set and
  #     calling it clean.
  #   * a separate CARGO_TARGET_DIR, so it cannot invalidate the fingerprints the stack-floor
  #     build below depends on. The ELF is copied out after each tier because the next tier
  #     overwrites it.
  step "tier exclusions — the BYTE-FREE claims (#351)"
  EXCL="${SMOL_GATE_EXCL_DIR:-/tmp/gate-exclusions}"
  mkdir -p "$EXCL/elf"
  EXCL_ARGS=()
  while IFS=$'\t' read -r name feats; do
    if [ -n "$feats" ] && ! grep -qE "^${feats%%,*} *=" "$CLOCK/Cargo.toml"; then
      printf '   \033[33mSKIP\033[0m %-16s (feature not in Cargo.toml on this branch)\n' "$name"; continue
    fi
    args=(--release --bin clock "${JOBS[@]}"); [ -n "$feats" ] && args+=(--features "$feats")
    if (cd "$CLOCK" && CARGO_TARGET_DIR="$EXCL/target" \
          CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build "${args[@]}") \
          >/tmp/gate-excl-$name.log 2>&1; then
      cp "$EXCL/target/$REPRO_TARGET/release/clock" "$EXCL/elf/$name"
      EXCL_ARGS+=(--elf "$name=$feats=$EXCL/elf/$name")
    else
      bad "exclusions build $name"; tail -15 /tmp/gate-excl-$name.log | sed 's/^/        /'
    fi
  done < <("$ROOT/tools/build_matrix.py" emit --for check)
  if [ ${#EXCL_ARGS[@]} -eq 0 ]; then
    bad "tier exclusions (nothing built — no tier could be measured)"
  elif out=$("$ROOT/tools/check_exclusions.py" check "${EXCL_ARGS[@]}" 2>&1); then
    printf '%s\n' "$out"; ok "tier exclusions"
  else
    printf '%s\n' "$out"; bad "tier exclusions"
  fi
fi

if [ "$run_fw" = 1 ]; then
  # #300 stack floor, measured with the SAME function the packaging path uses (repro_stack_check).
  # Built with repro_cargo_args so the ELF matches the shipped one's geometry.
  # #348: the title reads the floor from the same single definition repro_stack_check will use —
  # it was a third hardcoded copy of 73728, which would have gone on announcing the old number
  # while the gate enforced a new one. "unreadable" here is not a failure; repro_stack_check
  # fails closed on it a few lines down, with the diagnostic.
  step "stack floor — canonical ELF vs ${REPRO_STACK_FLOOR:-$(repro_stack_floor || echo unreadable)} B"
  if repro_cargo_args "$CLOCK" 2>/dev/null && \
     (cd "$CLOCK" && cargo build --release "${JOBS[@]}" --features "$REPRO_FLEET_FEATURES" "${REPRO_CARGO_ARGS[@]}") \
       >/tmp/gate-stack.log 2>&1; then
    ELF="${CARGO_TARGET_DIR:-$ROOT/$CLOCK/target}/${REPRO_TARGET}/release/clock"
    # Capture the verdict to a file as well as the console: CI lifts it into the job summary so the
    # number is visible without opening logs. Written on FAILURE too — "the stack broke the floor"
    # is the case a reader most needs surfaced, and a report that only exists when green would hide
    # exactly that.
    if out=$(repro_stack_check "$ELF" 2>&1); then
      printf '%s\n' "$out"; printf '%s\n' "$out" > "$STACK_REPORT"; ok "stack floor"
    else
      printf '%s\n' "$out"; printf '%s\n' "$out" > "$STACK_REPORT"; bad "stack floor"
    fi
  else
    bad "stack floor (canonical build failed)"
    echo "canonical build failed before the stack could be measured" > "$STACK_REPORT"
    tail -15 /tmp/gate-stack.log | sed 's/^/        /'
  fi
fi

if [ "$run_host" = 1 ]; then
  # Every experiments/*_verify is a standalone host crate that panics on failure. Discovered by
  # GLOB, not by a hardcoded list: a new verifier is picked up automatically, which is the whole
  # point — the last list-shaped gate silently stopped covering what was added after it.
  step "host verifier suites"
  for d in "$ROOT"/experiments/*/; do
    [ -f "$d/Cargo.toml" ] || continue
    n=$(basename "$d")
    case "$n" in *_verify|relay_compat) ;; *) continue ;; esac
    if (cd "$d" && cargo run --release -q "${JOBS[@]}") >/tmp/gate-$n.log 2>&1; then
      ok "$n"
    else
      bad "$n"; tail -10 /tmp/gate-$n.log | sed 's/^/        /'
    fi
  done

  # #350: the crate's OWN test suites — `rust/clock/tests/*.rs`. Found while wiring the matrix:
  # NOTHING in tools/ or .github/ ran `cargo test`, so `tests/bard.rs` (the Bard's bit-for-bit
  # golden against an independent reference), `tests/input.rs`, and #348's brand-new
  # `tests/budget.rs` were three suites no gate executed. #348's PR reported "8/8 host tests"
  # from a manual run — which is exactly how a suite stops being evidence.
  #
  # `--no-default-features` is LOAD-BEARING, not tidiness: the default features pull the
  # bare-metal stack, and `portable-atomic`'s `unsafe-assume-single-core` then leaks into the
  # HOST build and hard-errors ("not supported yet on this architecture"). Drop the flag and
  # this arm fails on a healthy tree.
  #
  # Discovered by GLOB for the same reason the loop above is: a list would stop covering what
  # is added after it.
  step "crate host test suites (cargo test)"
  HOST_TRIPLE="${SMOL_HOST_TRIPLE:-$(rustc -vV | sed -n 's/^host: //p')}"
  for t in "$ROOT/$CLOCK"/tests/*.rs; do
    [ -f "$t" ] || continue
    n=$(basename "$t" .rs)
    if (cd "$ROOT/$CLOCK" && cargo test --no-default-features --features hostsim \
          --target "$HOST_TRIPLE" --test "$n" "${JOBS[@]}") >/tmp/gate-test-$n.log 2>&1; then
      # Print the count rather than a bare PASS: "8 passed" is checkable, "ok" is not, and a
      # suite that silently starts running ZERO tests still says ok.
      ok "test $n — $(grep -Eo '[0-9]+ passed' /tmp/gate-test-$n.log | tail -1)"
    else
      bad "test $n"; tail -12 /tmp/gate-test-$n.log | sed 's/^/        /'
    fi
  done

  # #350: prove the matrix checker's arms can fail. Pure text, no cargo — see the file header
  # for why a green-only demonstration is not evidence.
  step "build-matrix checker regression suite (#350)"
  if out=$("$ROOT/tools/test_build_matrix.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_build_matrix"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_build_matrix"
  fi

  # #351: the same discipline for the exclusion checker, and it matters more here. An ABSENCE
  # check's passing state and its broken state print the same green — "no violations found"
  # and "nothing found at all" are indistinguishable from the outside. Pure text, no cargo,
  # so it runs in the host half where a cross toolchain is not available.
  step "exclusion checker regression suite (#351)"
  if out=$("$ROOT/tools/test_check_exclusions.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_check_exclusions"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_check_exclusions"
  fi
fi

step "summary"
if [ ${#FAILED[@]} -eq 0 ]; then
  printf '   \033[32mGATE GREEN\033[0m\n'; exit 0
fi
printf '   \033[31mGATE RED — %d failure(s):\033[0m\n' "${#FAILED[@]}"
printf '     • %s\n' "${FAILED[@]}"
exit 1
