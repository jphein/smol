#!/usr/bin/env bash
# test_check_elect_send_path.sh — proves tools/check_elect_send_path.py can FAIL (#278/#269).
#
# The invariant it guards is not a style rule: a leaf CANNOT verify an ELECT announcement before
# acting on it (a scan drops the association — `coex_background_scan` is hardcoded false), so an
# unauthenticated ELECT is a remote fleet-stranding primitive. Stage 1 stated that in prose, and
# prose would have survived any refactor that broke it.
#
# Which means a checker that only ever passes is the same failure wearing a green badge. Every arm
# below MUTATES A COPY of the firmware tree into one of the specific shapes that was enumerated as
# "satisfies the type system and still ships the bug", and asserts the checker goes red FOR THE
# RIGHT REASON — not merely red. A clean baseline must stay green.
#
# SAFETY, and it is not boilerplate: every arm works on a copy under mktemp. No git command, no
# write anywhere inside the repo. Two separate incidents in this repo destroyed uncommitted work
# through a self-test that operated on the real tree; this one cannot, because it never has the
# real tree's path in a writable position.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHK="$ROOT/tools/check_elect_send_path.py"
pass=0; fail=0
ok(){ pass=$((pass+1)); echo "ok   - $1"; }
no(){ fail=$((fail+1)); echo "FAIL - $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A pristine copy of just the firmware sources, laid out at the path the checker expects.
seed() {
  rm -rf "$work/tree"
  mkdir -p "$work/tree/rust/clock"
  cp -r "$ROOT/rust/clock/src" "$work/tree/rust/clock/src"
}

# patch1 <abs-file> <old>TAB<new> — for arms that assert rc=0 and so cannot use `arm`.
# Deliberately NOT a refactor of `arm` below: `arm` drives the twelve pre-existing arms of a
# SECURITY gate, and this addition has no business changing how those are executed.
patch1() {
  python3 - "$1" "$2" <<'PYEOF'
import sys
path, spec = sys.argv[1], sys.argv[2]
old, new = spec.split("\t")
s = open(path).read()
if old not in s:
    sys.exit(f"fixture setup failed: {old!r} not found — this test needs updating")
open(path, "w").write(s.replace(old, new, 1))
PYEOF
}

# arm <name> <want-rc> <want-substring> <file-rel-to-src> <old>TAB<new>
arm() {
  local name="$1" want_rc="$2" want="$3" file="$4" spec="$5"
  seed
  python3 - "$work/tree/rust/clock/src/$file" "$spec" <<'PY'
import sys
path, spec = sys.argv[1], sys.argv[2]
old, new = spec.split("\t")
s = open(path).read()
if old not in s:
    sys.exit(f"fixture setup failed: {old!r} not found — this test needs updating")
open(path, "w").write(s.replace(old, new, 1))
PY
  if [ $? -ne 0 ]; then no "$name (fixture setup)"; return; fi
  local out rc
  out="$("$CHK" "$work/tree" 2>&1)"; rc=$?
  if [ "$rc" != "$want_rc" ]; then no "$name: rc $rc, want $want_rc — $out"; return; fi
  case "$out" in *"$want"*) ok "$name (rc=$rc)" ;; *) no "$name: rc right, WRONG REASON — $out" ;; esac
}

echo "== baseline: the real tree must satisfy the invariant =="
out="$("$CHK" "$ROOT" 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "unmodified tree: $out"; else no "unmodified tree FAILS: $out"; fi

echo "== arm 1: the likeliest regression — the sink body re-routed to the raw sender =="
arm "impl body calls send_arb_raw" 1 "arm 1 (impl-body)" net/mode.rs \
  "$(printf 'self.send_to(dst, frame);\tself.send_arb_raw(*dst, frame);')"

echo "== arm 1b: routed to a NEW helper, so send_to disappears without send_arb_raw appearing =="
arm "impl body routes elsewhere" 1 "does NOT route to" net/mode.rs \
  "$(printf 'self.send_to(dst, frame);\tself.send_elect_someday(dst, frame);')"

echo "== arm 2: a second sink implementation =="
arm "two sink impls" 1 "arm 2 (impl-count)" net/mode.rs \
  "$(printf 'impl mesh_elect::GroupMacSink for RadioManager {\timpl mesh_elect::GroupMacSink for PeerTracker {\n    fn send_group_mac(&mut self, _dst: &[u8; 6], _frame: &[u8]) {}\n}\n\nimpl mesh_elect::GroupMacSink for RadioManager {')"

echo "== arm 3: the seal grows a byte accessor, and stops sealing anything =="
arm "SealedElect leaks its bytes" 1 "arm 3 (no-accessor)" net/mesh_elect.rs \
  "$(printf '    /// Hand the frame to the authenticated send path. Consumes `self`.\t    pub fn as_bytes(&self) -> &[u8] { &self.buf }\n\n    /// Hand the frame to the authenticated send path. Consumes `self`.')"

echo "== arm 4: the encoder called outside the seal =="
arm "stray encoder call site" 1 "arm 4 (one-encoder)" net/mode.rs \
  "$(printf 'let d = self.elect_announcer.decision(self.id);\tlet mut scratch = [0u8; 61];\n        let _ = mesh_elect::wire::encode(&f, &mut scratch);\n        let d = self.elect_announcer.decision(self.id);')"

echo "== arm 5: the frame hand-built from a second copy of the literal =="
arm "prefix literal written twice" 1 "arm 5 (no-hand-build)" net/mode.rs \
  "$(printf 'const HELLO_PREFIX: &[u8] = b"SMOLv1 HELLO ";\tconst HELLO_PREFIX: &[u8] = b"SMOLv1 HELLO ";\nconst HAND_BUILT_ELECT: &[u8] = b"SMOLv1 ELECT ";')"

echo "== arm 6: a NEW raw send site appears =="
arm "undeclared raw send" 1 "arm 6 (raw-sends)" net/mode.rs \
  "$(printf 'pub fn broadcast_hello(&mut self) {\tpub fn broadcast_hello(&mut self) {\n        let _ = self.esp_now.send(&BROADCAST_ADDRESS, b"x");')"

echo "== arm 6b: an EXTRA send inside an already-declared fn (counts, not just names) =="
arm "miscounted raw send" 1 "arm 6 (raw-sends)" net/mode.rs \
  "$(printf 'match self.esp_now.send(dst, out) {\tlet _ = self.esp_now.send(dst, out);\n        match self.esp_now.send(dst, out) {')"

echo "== fail-closed: the anchors going missing must NOT read as success =="
arm "sink impl deleted" 2 "no \`impl GroupMacSink" net/mode.rs \
  "$(printf 'impl mesh_elect::GroupMacSink for RadioManager {\timpl DeletedSink for RadioManager {')"
arm "prefix literal renamed away" 2 "appears NOWHERE" net/mesh_elect.rs \
  "$(printf 'b"SMOLv1 ELECT "\tb"SMOLv1 ELECTX"')"
arm "declaration deleted" 2 "no \`RAW-SEND-SITES:\` declaration" net/mode.rs \
  "$(printf 'RAW-SEND-SITES:\tRAW-SEND-SITES-WAS-HERE:')"

# ── #397 STEP B2: the `send_async` form, and comment-blindness ────────────────────────────────
# Arm 6 was widened to count `send_async` because bounding the two OTA-announce sends moved them to
# that form. A widened pattern is worth exactly as much as its proof of failure: if `send_async` is
# counted only in the passing direction, the widening is a claim rather than a check.
echo "== arm 6c: the send_async form is counted too (#397 STEP B2) =="
arm "undeclared send_async site" 1 "arm 6 (raw-sends)" net/mode.rs \
  "$(printf 'pub fn crown_term(&self) -> u16 {\tpub fn leak_async(&mut self) {\n        let _ = self.esp_now.send_async(&BROADCAST_ADDRESS, b"x");\n    }\n\n    pub fn crown_term(&self) -> u16 {')"
arm "extra send_async in a declared fn" 1 "arm 6 (raw-sends)" net/mode.rs \
  "$(printf 'let fut = self.esp_now.send_async(&dst, frame);\tlet _ = self.esp_now.send_async(&dst, frame);\n            let fut = self.esp_now.send_async(&dst, frame);')"

# Both directions of the comment-stripping fix, because a checker that documentation can move is
# worse than none — and the FALSE-GREEN direction is the one that matters on an absence check.
echo "== regression: comments must not move the verdict, either way (#426) =="
seed
if patch1 "$work/tree/rust/clock/src/net/mode.rs" \
  "$(printf 'pub fn crown_term(&self) -> u16 {\t// prose: self.esp_now.send(&x, y) and self.esp_now.send_async(&x, y)\n    /* and a block comment: esp_now.send(a, b); */\n    pub fn crown_term(&self) -> u16 {')"; then
  out="$("$CHK" "$work/tree" 2>&1)"; rc=$?
  if [ "$rc" = 0 ]; then ok "prose naming both send forms stays green (rc=0)"
  else no "PROSE FLIPPED THE VERDICT: rc $rc — $out"; fi
else no "prose regression (fixture setup)"; fi
# The false-GREEN direction: commenting a real send out must not satisfy the roster.
arm "commenting out a real send is caught" 1 "arm 6 (raw-sends)" net/mode.rs \
  "$(printf 'match self.esp_now.send(dst, out) {\tmatch NOTHING { // self.esp_now.send(dst, out) {')"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
