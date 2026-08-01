#!/usr/bin/env bash
# test_build_matrix.sh — #350. Prove `tools/build_matrix.py check` can FAIL, one arm at a time.
#
# ── WHY THIS EXISTS AND NOT JUST A GREEN RUN ──────────────────────────────────
# A gate demonstrated only in its passing state is the #335 failure: both Embassy branches
# produced "all tiers green" evidence from a configuration that contained no stack-floor
# gate at all, and nothing about a green run said so. The same trap applies here — every
# cross-check in `build_matrix.py` would report success on a tree where it had been
# silently disabled, and the day it matters is the day someone adds a chip.
#
# So each case below is a manifest crafted to violate exactly one rule, and the suite
# asserts BOTH that the checker fails AND that it fails with the right finding — a check
# that goes red for the wrong reason is a check that will be "fixed" by deleting it.
#
# No cargo, no network, no hardware: this is a python script reading text files.
#
# Case format — directives in the fixture's own comments, so a case is one file:
#   # EXPECT: <substring>          the failure text that must appear (exit 1)
#   # EXPECT-MALFORMED: <substring> the manifest is broken, not merely wrong (exit 2)
#   # EXPECT-OK                    must pass
#   # EXPECT-JOBS: <n>             the emitted matrix must have exactly n jobs
#   # BUDGET: <file>               use this budget fixture instead of _budget_ok.rs
#   # CARGO: <file>                use this Cargo.toml fixture instead of _cargo_empty.toml
#
# Exit 0 all cases behaved; 1 otherwise.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CASES="$HERE/test_build_matrix_cases"
BM="$HERE/build_matrix.py"
REPRO="$CASES/_repro_ok.sh"
pass=0; fail=0

note() { printf '   \033[32mok\033[0m   %s\n' "$1"; pass=$((pass + 1)); }
oops() { printf '   \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }

for case_file in "$CASES"/*.toml; do
  name="$(basename "$case_file" .toml)"
  # `_`-prefixed files are SHARED FIXTURES (a stub Cargo.toml, a stub budget.rs), not cases.
  # They live here rather than in a subdirectory so a case and the thing it points at are
  # visible together; the prefix is what keeps them out of the case glob.
  case "$name" in _*) continue ;; esac

  budget="$CASES/$(sed -n 's/^# BUDGET: *//p' "$case_file" | head -1)"
  [ -f "$budget" ] || budget="$CASES/_budget_ok.rs"
  cargo="$CASES/$(sed -n 's/^# CARGO: *//p' "$case_file" | head -1)"
  [ -f "$cargo" ] || cargo="$CASES/_cargo_empty.toml"

  want_fail="$(sed -n 's/^# EXPECT: *//p' "$case_file" | head -1)"
  want_bad="$(sed -n 's/^# EXPECT-MALFORMED: *//p' "$case_file" | head -1)"
  want_jobs="$(sed -n 's/^# EXPECT-JOBS: *//p' "$case_file" | head -1)"
  # A directive must be on its OWN line — these patterns are anchored at `^# `. The first
  # version of this suite had `# EXPECT-OK, EXPECT-JOBS: 8` on one line and the job-count
  # assertion silently never ran, which is the same silent-skip class of bug the whole file
  # exists to prevent. So: a case with no recognised directive is an ERROR, never a pass.
  if [ -z "$want_fail$want_bad$want_jobs" ] && ! grep -q '^# EXPECT-OK$' "$case_file"; then
    oops "$name: no recognised EXPECT directive (typo? directive not on its own line?)"
    continue
  fi

  out="$("$BM" check --manifest "$case_file" --repro "$REPRO" --budget "$budget" --cargo "$cargo" 2>&1)"
  rc=$?

  if [ -n "$want_bad" ]; then
    if [ "$rc" != 2 ]; then
      oops "$name: expected exit 2 (malformed), got $rc"
    elif ! printf '%s' "$out" | grep -qF -- "$want_bad"; then
      oops "$name: exit 2 but not for '$want_bad' — got: $(printf '%s' "$out" | head -1)"
    else
      note "$name — malformed, correctly refused"
    fi
  elif [ -n "$want_fail" ]; then
    # rc 2 here would mean the fixture is broken rather than violating the rule it targets,
    # which would make the case pass for the wrong reason — the one outcome worse than a
    # missing test. Insist on exit 1.
    if [ "$rc" != 1 ]; then
      oops "$name: expected exit 1 (check failed), got $rc — fixture may be malformed"
    elif ! printf '%s' "$out" | grep -qF -- "$want_fail"; then
      oops "$name: failed, but not with '$want_fail' — got: $(printf '%s' "$out" | tail -2 | head -1)"
    else
      note "$name — caught: $want_fail"
    fi
  else
    if [ "$rc" != 0 ]; then
      oops "$name: expected pass, got $rc — $(printf '%s' "$out" | tail -1)"
      continue
    fi
    note "$name — passes"
  fi

  if [ -n "$want_jobs" ]; then
    got=$("$BM" ci-matrix --manifest "$case_file" | python3 -c \
      'import json,sys; print(len(json.load(sys.stdin)["include"]))')
    if [ "$got" != "$want_jobs" ]; then
      oops "$name: expected $want_jobs jobs, got $got — the axes are being crossed"
    else
      note "$name — $got jobs (one axis at a time; the cross product would be more)"
    fi
  fi
done

printf '\n   %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
