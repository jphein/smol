#!/usr/bin/env bash
# measure_debuginfo_delta.sh — #351. Does enabling debug info change the SHIPPED bytes?
#
# ── WHY THIS IS KEPT ──────────────────────────────────────────────────────────
# `tools/gate.sh excl` links every tier with CARGO_PROFILE_RELEASE_DEBUG so it has DWARF to
# read. That is only sound if the flag does not change the artifact — and it DOES. This is the
# harness that settled it, kept because the answer is TREE-DEPENDENT and will need re-taking at
# the next dependency wave: on the pre-#233 tree the ALLOC sections came out the same SIZE and
# that was written into a comment as "no shipped byte moves"; after the esp-radio 0.18 bump
# `.text` moves by 46 B. A measurement carried across a codegen-relevant boundary without being
# re-taken is how a true number becomes a false claim.
#
# ── THE SANDWICH, AND WHY IT IS NOT JUST TWO BUILDS ───────────────────────────
# n1 (no debug) / d (debug) / n2 (no debug). The n1-vs-n2 CONTROL is what makes the treatment
# delta attributable: without it, a difference between the debug and non-debug builds could just
# as easily be build nondeterminism, and there is no way to tell from the treatment alone. The
# first attempt at this measurement had no control — someone edited the shared tree mid-run — and
# the resulting number had to be published with a caveat instead of a conclusion.
#
# ── RESULT AS OF b1eb271 (fleet tier, riscv32imc) ─────────────────────────────
#   control    3 of 1,027,770 ALLOC bytes differ — all 3 in .flash.appdesc (the ESP-IDF build
#              stamp); every section identical in ADDRESS and SIZE, so the tree holds still
#   treatment  464,180 of 1,027,816 differ, and .text GROWS 871,618 -> 871,664 (+46 B)
# Byte churn at constant size would be layout. A SIZE change means codegen differs, so inlining
# moved — and inlining is exactly what the line-table attribution in check_exclusions.py rides
# on. That is why that arm is corroboration and [tier_exclusive] is the proof.
#
# Usage: tools/measure_debuginfo_delta.sh [worktree-root] [debug-value]
#        debug-value defaults to line-tables-only (what gate.sh uses); try `1` or `2` to compare.
# Cost: three full LTO builds of the fleet tier, ~3 min, into the repo `tmp/`. Touches no tracked file.
set -uo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
WT="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
# Temp output goes in the REPO, never /tmp (JP directive 2026-08-25): katana's /tmp is a 16 GB
# tmpfs (RAM+swap), and this script is the heaviest offender in tools/ — THREE full LTO target
# dirs of the fleet tier. Putting those in RAM is how 13 GB of tmpfs vanished on 2026-08-25.
# `$WT/tmp` is git-ignored (tmp/.gitignore) and disk-backed.
O="$WT/tmp/smol-debuginfo-delta"
rm -rf "$O"; mkdir -p "$O"
# Children (cargo, readelf) inherit this; cargo's own scratch then lands on disk too.
export TMPDIR="$WT/tmp"
build() { # build <name> <debugflag-or-empty>
  # `env`, not a bare VAR=x prefix: a prefix assembled by ${2:+...} expansion is a WORD, and
  # bash tries to execute it. First run died `CARGO_PROFILE_RELEASE_DEBUG=…: command not found`.
  local extra=(); [ -n "$2" ] && extra=("CARGO_PROFILE_RELEASE_DEBUG=$2")
  ( cd "$WT/rust/clock" && env CARGO_TARGET_DIR="$O/t-$1" "${extra[@]}" \
      cargo build --release --bin clock --features espnow,cast,io ) > "$O/$1.log" 2>&1
  echo "$1 rc=$? $(tail -1 "$O/$1.log")"
}
build n1 ""
build d  "${2:-line-tables-only}"
build n2 ""
# `O` is EXPORTED into the python below rather than re-spelled there. The heredoc is quoted
# (`<<'EOF'`) so the shell does not expand it, which is why the path used to appear twice as a
# literal — two definitions of one path, free to drift, and the reason this fix touches python.
O="$O" python3 - <<'EOF'
import subprocess, re, hashlib, os
def secs(p):
    out = subprocess.run(["readelf","-S","-W",p],capture_output=True,text=True).stdout
    r = {}
    for l in out.splitlines():
        m = re.match(r"\s*\[\s*\d+\]\s+(\S+)\s+(\S+)\s+([0-9a-f]+)\s+([0-9a-f]+)\s+([0-9a-f]+)\s+\S+\s+(\S*)", l)
        if not m: continue
        n,t,a,o,s,f = m.groups()
        if "A" not in f or t == "NOBITS": continue
        r[n] = (int(a,16), int(o,16), int(s,16))
    return r
O = os.environ["O"]  # set by the shell above; single definition of the output root
P = {k: f"{O}/t-{k}/riscv32imc-unknown-none-elf/release/clock" for k in ("n1","d","n2")}
S = {k: secs(v) for k,v in P.items()}
B = {k: open(v,'rb').read() for k,v in P.items()}
def cmp(x,y,label):
    tot=diff=0; sizediff=[]
    for n in sorted(S[x]):
        ax,ox,zx = S[x][n]; ay,oy,zy = S[y][n]
        if zx != zy or ax != ay: sizediff.append(n)
        u,v = B[x][ox:ox+zx], B[y][oy:oy+zy]
        d = sum(1 for i in range(min(len(u),len(v))) if u[i]!=v[i]) + abs(len(u)-len(v))
        tot += max(zx,zy); diff += d
        if d: print(f"    {n:16} {zx:>8} B  {d:>7} differing ({100*d/max(zx,1):5.2f}%)")
    print(f"  {label}: {diff} of {tot} ALLOC bytes differ"
          + (f"  ·  SIZE/ADDR moved: {sizediff}" if sizediff else "  ·  every section same addr+size"))
    return diff
print("CONTROL  n1 vs n2 (both no-debug):")
c = cmp("n1","n2","control")
print("TREATMENT n1 vs d (line-tables-only):")
t = cmp("n1","d","treatment")
print()
print(f"VERDICT: control={c} treatment={t} → "
      + ("line-tables-only PERTURBS the shipped ALLOC bytes" if t > c == 0 else
         "the tree is NOT byte-reproducible; the treatment delta is not attributable" if c else
         "line-tables-only leaves the ALLOC bytes IDENTICAL"))
EOF
