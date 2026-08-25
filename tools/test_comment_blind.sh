#!/usr/bin/env bash
# test_comment_blind.sh — prove each swept checker ignores comments, in BOTH directions. (#426)
#
# ── WHY ───────────────────────────────────────────────────────────────────────────────────────
# Three checkers were counting matches inside comments. Each is fixed by stripping; this proves
# the strip took, and — more importantly — that it did NOT strip too much. Three of these
# checkers read their DECLARATIONS from comments on purpose (`SHED-ORDER:`, `DIAG-WIDTHS:`,
# `byte-free` claims), so an over-eager strip leaves them with nothing to check and green forever.
# That failure would look exactly like success, which is why the "still sees its declarations"
# arms are here beside the "no longer sees prose" ones.
#
# BOTH DIRECTIONS, per checker:
#   false RED    prose about the invariant must not trip the gate
#   false GREEN  a real site commented out must not still count
#
# ── `//` IS SAFE; `/* */` IS THE VECTOR ───────────────────────────────────────────────────────
# Every Rust-side checker anchors with `^\s*`, so a LINE comment can never match. Probing with
# `//` returns a clean bill of health on all of them, which is how this survived. Every probe
# below therefore uses a BLOCK comment; a `//` probe would pass against the unfixed code too and
# prove nothing.
#
# USAGE:  tools/test_comment_blind.sh        # exit 0 = every arm behaved
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$ROOT/tmp/test-comment-blind"
pass=0; fail=0
ok()  { printf '   \033[32mok\033[0m   %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '   \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
rm -rf "$WORK"; mkdir -p "$WORK"

NET="$ROOT/rust/clock/src/net.rs"
MODE="$ROOT/rust/clock/src/net/mode.rs"
cp "$NET" "$WORK/net.orig"; cp "$MODE" "$WORK/mode.orig"
restore() { cp "$WORK/net.orig" "$NET"; cp "$WORK/mode.orig" "$MODE"; }
trap 'restore; cleanup' EXIT

echo "── comment-blind sweep (#426)"

# ── 1. the shared helper itself ───────────────────────────────────────────────────────────────
python3 - <<'PY' && ok "strip_comments: offsets and lines preserved, strings kept" \
                 || bad "strip_comments: basic properties"
import sys; sys.path.insert(0, "tools")
from rust_comments import strip_comments
src = 'pub mod a;\n/* b\npub mod b;\n*/\n// pub mod c;\nlet s = "/* not a comment */";\n'
out = strip_comments(src)
assert len(out) == len(src),                "length not preserved"
assert out.count("\n") == src.count("\n"),  "newlines not preserved"
assert "pub mod a;" in out,                 "real decl lost"
assert "pub mod b;" not in out,             "block comment not blanked"
assert "/* not a comment */" in out,        "string literal corrupted"
PY

# ── 2. check_verifier_wiring — the false GREEN ────────────────────────────────────────────────
# A verifier over a module the firmware does not compile must read PHANTOM. Before the fix a
# block-commented `pub mod` made it read SOUND: the checker believed the firmware contained code
# it does not. That is the direction an ABSENCE check exists to catch.
python3 - "$NET" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
open(p, "w").write(s.replace("pub mod mesh_elect;", "/* probe:\npub mod mesh_elect;\n*/", 1))
PY
# CAPTURE, THEN MATCH. Under `set -o pipefail` a pipeline takes the FAILING member's status, and
# this checker exits nonzero precisely when it finds an untracked phantom — so
# `checker | grep -q PHANTOM` reports failure at the moment it succeeds. Cost one confused debug
# cycle; same family as the `grep -q`-under-pipefail hazard this repo already has a rule about.
out="$(python3 "$ROOT/tools/check_verifier_wiring.py" 2>&1 || true)"
if printf '%s' "$out" | grep -q "mesh_elect_verify.*PHANTOM"; then
    ok "verifier_wiring: block-commented \`mod\` no longer counts as wired (false GREEN closed)"
else
    bad "verifier_wiring: still treats a commented-out mod as wired"
fi
restore

# ── 3. check_verifier_wiring — the real decl must still count ─────────────────────────────────
out="$(python3 "$ROOT/tools/check_verifier_wiring.py" 2>&1 || true)"
if printf '%s' "$out" | grep -q "mesh_elect_verify.*SOUND"; then
    ok "verifier_wiring: the REAL decl still counts (not over-stripped)"
else
    bad "verifier_wiring: over-stripped — a live module now reads as phantom"
fi

# ── 4. check_byte_free — prose must not override a real gate ──────────────────────────────────
python3 - <<'PY' && ok "byte_free: commented cfg no longer overrides the real gate" \
                 || bad "byte_free: prose still decides a module's gate"
import importlib.util, sys
from pathlib import Path
sys.path.insert(0, "tools")
from rust_comments import strip_comments
spec = importlib.util.spec_from_file_location("bf", "tools/check_byte_free.py")
bf = importlib.util.module_from_spec(spec); a = sys.argv; sys.argv = ["t"]
spec.loader.exec_module(bf); sys.argv = a
orig = Path("rust/clock/src/net.rs").read_text()
doc = orig.replace("pub mod mesh_elect;",
                   '/* prose:\n#[cfg(feature = "bard")]\npub mod mesh_elect;\n*/\npub mod mesh_elect;', 1)
got = dict(bf.mod_gates(strip_comments(doc).splitlines())).get("mesh_elect")
assert got == 'feature = "espnow"', f"gate became {got!r}, expected the real espnow gate"
PY

# ── 5. check_byte_free — the CLAIMS are prose and must SURVIVE ────────────────────────────────
# The over-strip direction. `byte-free` assertions live in comments by design; if the strip
# reached them this reports zero claims and passes forever.
bf_out="$(python3 "$ROOT/tools/check_byte_free.py" 2>&1 || true)"
claims=$(printf '%s' "$bf_out" | sed -n 's/.*source: \([0-9][0-9]*\) byte-free claim.*/\1/p')
if [ "${claims:-0}" -gt 0 ] 2>/dev/null; then
    ok "byte_free: still reads its $claims prose claims (not over-stripped)"
else
    bad "byte_free: reads ZERO claims — the strip reached the declarations"
fi

# ── 6. check_diag_budget — the false RED ──────────────────────────────────────────────────────
python3 - "$MODE" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
i = s.index("fn diag_record"); j = s.index("room_for", i); k = s.rfind("\n", i, j) + 1
open(p, "w").write(s[:k] + '        /* probe:\n           if room_for(&mut rec, probe) { rec.push_str("|probe="); }\n        */\n' + s[k:])
PY
if python3 "$ROOT/tools/check_diag_budget.py" "$MODE" >/dev/null 2>&1; then
    ok "diag_budget: prose about the record no longer fails the gate (false RED closed)"
else
    bad "diag_budget: a doc comment still breaks the budget check"
fi
restore

# ── 7. check_diag_budget — declarations live in comments and must still be read ───────────────
out="$(python3 "$ROOT/tools/check_diag_budget.py" "$MODE" 2>&1 || true)"
if printf '%s' "$out" | grep -q "budget="; then
    ok "diag_budget: still reads DIAG-WIDTHS/DIAG-TAIL from comments (not over-stripped)"
else
    bad "diag_budget: lost its declarations — over-stripped"
fi

# ── 8. status_check's grep-absent — the real ELECT_ENFORCE case ───────────────────────────────
# The symbol is gone; the STRING survives as prose describing the watch's flag. Asking about
# code must say absent, and the raw grep must still find the prose — otherwise this arm is
# testing nothing.
python3 "$ROOT/tools/rust_comments.py" --grep 'ELECT_ENFORCE' "$ROOT/rust/clock/src" >/dev/null 2>&1
rc=$?
if [ $rc -eq 1 ] && grep -rq 'ELECT_ENFORCE' "$ROOT/rust/clock/src" --include='*.rs'; then
    ok "grep-absent: ELECT_ENFORCE absent from CODE while present as prose (false RED closed)"
else
    bad "grep-absent: rc=$rc, or the prose fixture has vanished (arm no longer meaningful)"
fi

# ── 9. a real symbol must still be FOUND — the over-strip direction ───────────────────────────
if python3 "$ROOT/tools/rust_comments.py" --grep 'fn diag_record' "$ROOT/rust/clock/src" >/dev/null 2>&1; then
    ok "grep-absent: a live symbol is still found in code (not over-stripped)"
else
    bad "grep-absent: a live symbol reads as absent — over-stripped"
fi

# ── 10/11. a missing or empty path is NOT an absence ──────────────────────────────────────────
python3 "$ROOT/tools/rust_comments.py" --grep 'x' /no/such/path >/dev/null 2>&1
[ $? -eq 2 ] && ok "grep seam: missing path exits 2, never 1 (a crash must not read as absent)" \
             || bad "grep seam: missing path did not exit 2"
mkdir -p "$WORK/empty"
python3 "$ROOT/tools/rust_comments.py" --grep 'x' "$WORK/empty" >/dev/null 2>&1
[ $? -eq 2 ] && ok "grep seam: a path with no .rs files refuses to report an absence" \
             || bad "grep seam: empty dir reported an absence"

echo
echo "   $pass ok, $fail failed"
[ $fail -eq 0 ] || exit 1
