#!/usr/bin/env bash
# test_cargo_locked.sh — #526. Proof that the rust/clock BUILD paths ENFORCE the lock (`--locked`), not
# merely that tools/check_lock_fresh.sh can DETECT a stale one (#460). A gate shown green proves nothing
# until it has been seen going red for the right reason, so every arm below is observed:
#   1. repro_cargo_args puts --locked on rust/clock (and on a mirror of it, e.g. gate.sh's pristine copy)
#      and NOT on a tree that has not been adopted (#526: per tree, never globally).
#   2. CONTROL — without --locked, cargo silently REWRITES a stale lock. This is the failure mode #460
#      named; if it stopped happening the arm below would be proving nothing.
#   3. FAIL-CLOSED — the same stale lock with REPRO_CARGO_ARGS: the build REFUSES (exit != 0, cargo's
#      "--locked" refusal) and the lock is left byte-for-byte as it was.
#   4. every cargo check/clippy/build/test gate.sh runs in $CLOCK carries --locked, directly
#      ("${LOCKED[@]}") or through REPRO_CARGO_ARGS.
# Scratch: a copy of rust/ WITHOUT target/ (the clock crate has path deps into rust/, see
# check_lock_fresh.sh's self-test), under $TMPDIR or /var/tmp — never /tmp. Needs cargo + network/cache
# for the registry index, like test_lock_fresh.sh. Arm 3 fails at dependency resolution, before any
# compilation, so it costs seconds.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pass=0; fail=0
ok()  { printf '   PASS %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '   FAIL %s\n' "$1"; fail=$((fail+1)); }
WORK="$(mktemp -d "${TMPDIR:-/var/tmp}/cargo-locked.XXXXXX")"
trap 'rm -rf "${WORK:-}"' EXIT INT TERM
mkdir -p "$WORK/rust" "$WORK/other"
tar -cf - --exclude=target -C "$ROOT/rust" . | tar -xf - -C "$WORK/rust"
C="$WORK/rust/clock"
. "$ROOT/tools/repro_build.sh"

# 1. per-tree adoption
if repro_cargo_args "$C" 2>/dev/null && printf '%s\n' "${REPRO_CARGO_ARGS[@]}" | grep -qx -- --locked; then
  ok "repro_cargo_args: a mirror of rust/clock gets --locked"
else bad "repro_cargo_args: a mirror of rust/clock has NO --locked (${REPRO_CARGO_ARGS[*]:-unresolved})"; fi
if repro_tree_locked "$ROOT/rust/clock"; then ok "repro_tree_locked: the checkout's rust/clock is adopted"
else bad "repro_tree_locked: the checkout's rust/clock is not adopted"; fi
if ! repro_tree_locked "$WORK/other" && ! repro_tree_locked "$ROOT/targets/c6-watch"; then
  ok "repro_tree_locked: an un-adopted tree (targets/c6-watch) is NOT locked"
else bad "repro_tree_locked: an un-adopted tree was treated as locked (#526: per tree only)"; fi

# a stale lock: drop one [[package]] block cargo must resolve (the real #460 drift shape)
stale() {
  python3 - "$C/Cargo.lock" <<'PY'
import re, sys, pathlib
p = pathlib.Path(sys.argv[1]); blocks = p.read_text().split('\n[[package]]\n')
keep = [blocks[0]] + [b for b in blocks[1:] if not re.match(r'name = "(mipidsi|embedded-hal-bus)"', b)]
p.write_text('\n[[package]]\n'.join(keep))
PY
}
cp "$C/Cargo.lock" "$WORK/lock.healthy"

# 2. control: unlocked, cargo rewrites the stale lock without a word
stale; before="$(md5sum < "$C/Cargo.lock")"
( cd "$C" && cargo metadata --format-version 1 >/dev/null 2>"$WORK/ctl.err" )
if [ "$(md5sum < "$C/Cargo.lock")" != "$before" ]; then ok "control: without --locked a stale lock is silently REWRITTEN"
else bad "control: cargo did not rewrite the stale lock — arm 3 would prove nothing"; sed 's/^/        /' "$WORK/ctl.err" | tail -3; fi

# 3. fail-closed through the real build args
cp "$WORK/lock.healthy" "$C/Cargo.lock"; stale; before="$(md5sum < "$C/Cargo.lock")"
repro_cargo_args "$C" 2>/dev/null
if ( cd "$C" && cargo build --release "${REPRO_CARGO_ARGS[@]}" ) >"$WORK/build.log" 2>&1; then
  bad "fail-closed: the build SUCCEEDED on a stale lock"
elif grep -q -- "--locked" "$WORK/build.log" && [ "$(md5sum < "$C/Cargo.lock")" = "$before" ]; then
  ok "fail-closed: the build REFUSES a stale lock and leaves it untouched"
else
  bad "fail-closed: the build failed, but NOT with cargo's --locked refusal (or the lock changed):"
  tail -5 "$WORK/build.log" | sed 's/^/        /'
fi

# 4. gate.sh: every cargo command run in $CLOCK carries --locked
if out="$(python3 - "$ROOT/tools/gate.sh" <<'PY'
import re, sys
src = open(sys.argv[1]).read().replace('\\\n', ' ')
miss = []
for ln in src.splitlines():
    if re.search(r'cd "\$(BUILD_ROOT|ROOT)/\$CLOCK"', ln) and re.search(r'\bcargo (check|clippy|build|test)\b', ln):
        if 'LOCKED[@]' not in ln and 'REPRO_CARGO_ARGS[@]' not in ln:
            miss.append(ln.strip()[:110])
print(len(miss)); print('\n'.join(miss))
PY
)" && [ "$(head -1 <<<"$out")" = 0 ]; then ok "gate.sh: every cargo command in \$CLOCK carries --locked"
else bad "gate.sh: cargo command(s) in \$CLOCK without --locked:"; tail -n +2 <<<"$out" | sed 's/^/        /'; fi

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
