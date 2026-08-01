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
#   1. cargo check --release across the build tiers (default / wifi / espnow / canonical fleet /
#      canonical+bard, which is off the fleet image since #347 but still a supported build)
#   2. cargo clippy --release -D warnings on EVERY tier (#343 — was canonical-only)
#   3. the host experiments/*_verify suites
#   4. the #300 stack floor — the canonical ELF's .stack region vs the floor declared in
#      rust/clock/src/budget.rs (#348: one definition, parsed by repro_stack_floor)
#
# WHAT IT DOES NOT COVER — read this before trusting a green run:
#   * `mesh-test`: needs a per-board `DEAF_MACS` that only a real board.rs has.
#   * espflash `save-image` packaging: not run (no espflash in CI). The stack number does not depend
#     on it — it is read from the ELF — but image PACKAGING is therefore unproven here.
#   * Anything requiring hardware: OTA, radio, election, the stack-paint HIGH-WATER measurement.
#     ⚠️ The stack check bounds the linked REGION, not runtime high-water. A struct living in a
#     stack-resident RadioManager can cost ~1.8 KB of real stack and move the region by ~32 B. A
#     green stack line is NOT evidence of runtime headroom (see repro_stack_check's comment).
#
# Usage:  tools/gate.sh              # everything
#         tools/gate.sh host         # host suites only (no riscv toolchain needed)
#         tools/gate.sh fw           # tiers + clippy + stack only
# Env:    CARGO_TARGET_DIR honoured; SMOL_GATE_JOBS caps cargo parallelism.
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
CLOCK="rust/clock"
WHAT="${1:-all}"
FAILED=()
run_fw=1; run_host=1
case "$WHAT" in
  host) run_fw=0 ;;
  fw)   run_host=0 ;;
  all)  ;;
  *) echo "usage: tools/gate.sh [all|host|fw]" >&2; exit 2 ;;
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

if [ "$run_fw" = 1 ]; then
  step "provisioning (git-ignored; existing files untouched)"
  "$ROOT/tools/ci_provision.sh" "$CLOCK" || { echo "provisioning failed" >&2; exit 1; }

  # Cheap and first, so a source-consistency error is not buried behind ten minutes of LTO.
  # #339: the prose describing the DIAG shed order had drifted from the appends it described, in a
  # direction that would send an operator to the wrong fields. Prose cannot be tested; this can.
  step "DIAG shed order matches its declaration (#339)"
  if out=$("$ROOT/tools/check_shed_order.py" "$CLOCK/src/net/mode.rs" 2>&1); then
    printf '%s\n' "$out"; ok "shed order"
  else
    printf '%s\n' "$out"; bad "shed order"
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
  step "cargo check — build tiers"
  for tier in "default:" "wifi:wifi" "espnow:espnow" "fleet:$REPRO_FLEET_FEATURES" "bard:bard,off-fleet,$REPRO_FLEET_FEATURES" "ledger-provision:ledger-provision"; do
    name="${tier%%:*}"; feats="${tier#*:}"
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
  done

  # `-D warnings` on EVERY tier, not just the canonical one. Until #343 this ran on the canonical
  # tier alone because `default`/`wifi` carried dead-code findings (symbols whose only callers are
  # espnow-gated); those now carry item-scoped `#[allow(dead_code)]` with a cited reason, so the
  # gate can cover what ONBOARDING has claimed all along. A tier that only gets `cargo check` has
  # its new warnings invisible — which is how the findings accumulated unnoticed in the first place.
  step "cargo clippy -D warnings — every tier"
  for tier in "default:" "wifi:wifi" "espnow:espnow" "canonical:$REPRO_FLEET_FEATURES" "bard:bard,off-fleet,$REPRO_FLEET_FEATURES"; do
    name="${tier%%:*}"; feats="${tier#*:}"
    args=(--release "${JOBS[@]}"); [ -n "$feats" ] && args+=(--features "$feats")
    if (cd "$CLOCK" && cargo clippy "${args[@]}" -- -D warnings) >/tmp/gate-clippy-$name.log 2>&1; then
      ok "clippy $name"
    else
      bad "clippy $name"
      grep -E "^(error|warning)" /tmp/gate-clippy-$name.log | grep -v "could not compile" \
        | sort -u | sed 's/^/        /' | head -12
    fi
  done

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
fi

step "summary"
if [ ${#FAILED[@]} -eq 0 ]; then
  printf '   \033[32mGATE GREEN\033[0m\n'; exit 0
fi
printf '   \033[31mGATE RED — %d failure(s):\033[0m\n' "${#FAILED[@]}"
printf '     • %s\n' "${FAILED[@]}"
exit 1
