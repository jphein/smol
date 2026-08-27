#!/usr/bin/env bash
# test_check_station_consumers.sh — proves tools/check_station_consumers.py can FAIL (#335 STEP G).
#
# The invariant: exactly ONE consumer of the STA transport is live per tier. It is not a style rule.
# `esp_radio::wifi::Interface` is `Copy`, so `embassy_net::new(station, ..)` does not consume the
# handle and a `Stack` can be live beside a `SmolWifiDevice` over the same interface. That COMPILES
# CLEAN, and then both pop `data_queue_rx()` — keyed by `InterfaceType` alone — so frames are stolen
# nondeterministically. No error, no panic, nothing to grep for in a log.
#
# Which means a checker that only ever passes is the same bug wearing a green badge (#350's
# test_build_matrix.sh lesson, and `[[gate-that-cannot-fail]]`). Every arm below MUTATES A COPY of
# the firmware tree into one of the shapes enumerated as "satisfies the compiler and still ships the
# theft", and asserts the checker goes red FOR THE RIGHT REASON — not merely red. The fail-closed
# arms assert rc=2, because a checker that silently stops covering anything is how prose rots.
#
# The last arm is a REGRESSION arm, not a hazard arm: the first run of the checker counted its own
# doc comment's prose as a real call site. Documentation about the invariant must never be able to
# change the verdict, in either direction.
#
# SAFETY, and it is not boilerplate: every arm works on a copy under mktemp. No git command, no
# write anywhere inside the repo. Two separate incidents in this repo destroyed uncommitted work
# through a self-test that operated on the real tree; this one cannot, because the real tree's path
# is never in a writable position.
#
# TMPDIR: defaults to the repo's gitignored tmp/ (JP directive 2026-08-25 — katana's /tmp is a
# 16 GB tmpfs, i.e. RAM). mktemp honours TMPDIR, so this is the whole fix.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHK="$ROOT/tools/check_station_consumers.py"
if [ -z "${TMPDIR:-}" ]; then
  mkdir -p "$ROOT/tmp" && TMPDIR="$ROOT/tmp" && export TMPDIR
fi
pass=0; fail=0
ok(){ pass=$((pass+1)); echo "ok   - $1"; }
no(){ fail=$((fail+1)); echo "FAIL - $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

seed() {
  rm -rf "$work/tree"
  mkdir -p "$work/tree/rust/clock"
  cp -r "$ROOT/rust/clock/src" "$work/tree/rust/clock/src"
}

patch1() {   # patch1 <abs-file> <old>TAB<new>
  python3 - "$1" "$2" <<'PY'
import sys
path, spec = sys.argv[1], sys.argv[2]
old, new = spec.split("\t")
s = open(path).read()
if old not in s:
    sys.exit(f"fixture setup failed: {old!r} not found — this test needs updating")
open(path, "w").write(s.replace(old, new, 1))
PY
}

# arm <name> <want-rc> <want-substring> <file-rel-to-src> <old>TAB<new>
arm() {
  local name="$1" want_rc="$2" want="$3" file="$4" spec="$5"
  seed
  if ! patch1 "$work/tree/rust/clock/src/$file" "$spec"; then no "$name (fixture setup)"; return; fi
  local out rc
  out="$("$CHK" "$work/tree" 2>&1)"; rc=$?
  if [ "$rc" != "$want_rc" ]; then no "$name: rc $rc, want $want_rc — $out"; return; fi
  case "$out" in *"$want"*) ok "$name (rc=$rc)" ;; *) no "$name: rc right, WRONG REASON — $out" ;; esac
}

# #335 STEP T re-pointed every fixture. The roster now counts calls to the SHARED bring-up
# helper (one per radio tier) instead of `SmolWifiDevice::new`, because STEP T deleted that
# shim — see the checker's docstring. The two call sites are textually identical, which is
# fine: `patch1` edits one named file at a time.
DEV_WIFI='let stack = super::bring_up_stack(spawner, interfaces.station, &mut rng);'
DEV_MODE='let stack = super::bring_up_stack(spawner, interfaces.station, &mut rng);'
ROSTER='/// STATION-CONSUMER-SITES: mode.rs::RadioManager::new:1, wifi.rs::try_time_sync:1'
STACKROSTER='/// STATION-STACK-SITES: net.rs::bring_up_stack:1'

echo "== baseline: the real tree must satisfy the invariant =="
out="$("$CHK" "$ROOT" 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "unmodified tree: $out"; else no "unmodified tree FAILS: $out"; fi

echo "== arm 1 (count): the roster drifts from the tree =="
# A SECOND device in an ALREADY-LISTED function — the shape a name-only allowlist would miss.
arm "second bring-up in a listed fn" 1 "arm 1 (count)" net/wifi.rs \
"$DEV_WIFI	$DEV_WIFI
    let _steal = super::bring_up_stack(spawner, interfaces.station, &mut rng);"
# A bring-up in a function nobody declared.
arm "bring-up in an undeclared fn" 1 "arm 1 (count)" net/wifi.rs \
"$DEV_WIFI	$DEV_WIFI
}
pub fn sneaky(spawner: Spawner, i: Interface<'static>, rng: &mut Rng) {
    let _ = super::bring_up_stack(spawner, i, rng);"
# A declared site that no longer exists — a stale roster entry reads as a decision.
arm "declared site removed" 1 "declared but ABSENT" net/mode.rs \
"$DEV_MODE	let stack = todo!();"

echo "== arm 2 (coexist): THE packet-theft shape =="
# The first embassy_net::new must be deliberate, not inherited.
arm "undeclared embassy_net::new" 1 "arm 2 (coexist)" net/mode.rs \
"pub fn now_ms() -> u64 {	pub fn now_ms() -> u64 {
    let _ = embassy_net::new(1, 2, 3, 4);"
# The acute form: one function holding both consumers over one interface.
arm "one fn holds an inline stack AND a helper call" 1 "holds BOTH" net/wifi.rs \
"$DEV_WIFI	$DEV_WIFI
    let _stack = embassy_net::new(interfaces.station, cfg, res, seed);"

echo "== arm 3 (per-tier): a cfg guard was edited — the arm that matters after STEP T =="
# Widening the try_time_sync gate to plain `wifi` makes BOTH consumers reachable on espnow tiers.
arm "guard widened to plain wifi" 1 "arm 3 (per-tier)" net.rs \
'#[cfg(all(feature = "wifi", not(feature = "espnow")))]
pub use wifi::try_time_sync;	#[cfg(feature = "wifi")]
pub use wifi::try_time_sync;'
# Moving the gated item out from under its cfg is the same failure with a different shape.
arm "gated item detached from its cfg" 1 "arm 3 (per-tier)" net.rs \
'#[cfg(all(feature = "wifi", not(feature = "espnow")))]
pub use wifi::try_time_sync;	#[cfg(all(feature = "wifi", not(feature = "espnow")))]
pub use wifi::CFG_KEY_LED;
pub use wifi::try_time_sync;'

echo "== arm 4 (shim-stays-dead): the deleted smoltcp phy shim comes back =="
# The type reappearing anywhere in CODE is the whole assertion — a second STA transport is
# being reintroduced, and arms 1-3 only ever look at bring-up sites, so none would see it.
arm "shim type reintroduced in a source file" 1 "arm 4 (shim-stays-dead)" net/wifi.rs \
"$DEV_WIFI	$DEV_WIFI
    let _shim = SmolWifiDevice::new(interfaces.station);"
# ...and the module file itself returning is called out with its own message.
seed
mkdir -p "$work/tree/rust/clock/src/net"
printf 'pub struct SmolWifiDevice(Interface<%s>);\n' "'static" > "$work/tree/rust/clock/src/net/radio_dev.rs"
out="$("$CHK" "$work/tree" 2>&1)"; rc=$?
if [ "$rc" = 1 ] && case "$out" in *"is back in the tree"*) true ;; *) false ;; esac; then
  ok "radio_dev.rs restored (rc=1)"
else no "radio_dev.rs restored: rc $rc — $out"; fi

echo "== fail-closed: a blind checker must refuse to pass, never quietly succeed =="
arm "roster deleted" 2 "no \`STATION-CONSUMER-SITES:\` declaration" net/mode.rs \
"$ROSTER	/// (roster deleted by the test)"
arm "stack roster deleted" 2 "no \`STATION-STACK-SITES:\` declaration" net/mode.rs \
"$STACKROSTER	/// (stack roster deleted by the test)"
arm "roster count malformed" 2 "malformed STATION-CONSUMER-SITES" net/mode.rs \
"$ROSTER	/// STATION-CONSUMER-SITES: mode.rs::RadioManager::new:many, wifi.rs::try_time_sync:1"
arm "guard names a gate file that does not exist" 2 "not found in the source tree" net/mode.rs \
"| net.rs | pub mod mode;	| nowhere.rs | pub mod mode;"
arm "a declared site has no guard" 2 "have no STATION-CONSUMER-GUARD" net/mode.rs \
'/// STATION-CONSUMER-GUARD: mode.rs::RadioManager::new | net.rs | pub mod mode; | feature = "espnow"	/// (guard deleted by the test)'
arm "guard declaration malformed" 2 "malformed STATION-CONSUMER-GUARD" net/mode.rs \
"| net.rs | pub mod mode; | feature	| net.rs | feature"
# Zero call sites means the pattern moved and every count arm is blind. Two files, so inline.
seed
if patch1 "$work/tree/rust/clock/src/net/wifi.rs" "$DEV_WIFI	let stack = make_stack(interfaces.station);" \
   && patch1 "$work/tree/rust/clock/src/net/mode.rs" "$DEV_MODE	let stack = make_stack(interfaces.station);"; then
  out="$("$CHK" "$work/tree" 2>&1)"; rc=$?
  if [ "$rc" != 2 ]; then no "zero bring-up sites: rc $rc, want 2 — $out"
  else case "$out" in *"found ZERO"*) ok "zero bring-up sites (rc=2)" ;;
       *) no "zero bring-up sites: rc right, WRONG REASON — $out" ;; esac
  fi
else no "zero bring-up sites (fixture setup)"; fi
# A tree with no sources at all must also refuse.
mkdir -p "$work/empty/rust/clock/src"
out="$("$CHK" "$work/empty" 2>&1)"; rc=$?
if [ "$rc" = 2 ]; then ok "empty source tree refuses (rc=2)"; else no "empty tree: rc $rc, want 2 — $out"; fi

echo "== regression: PROSE must not move the verdict, in either direction =="
# The checker's first run counted its own doc comment as a call site. A comment naming both
# constructors must leave a clean tree clean.
seed
if patch1 "$work/tree/rust/clock/src/net/wifi.rs" \
"$DEV_WIFI	$DEV_WIFI
// Prose: embassy_net::new(interfaces.station, ..) beside SmolWifiDevice::new(interfaces.station)
/* and a block comment: SmolWifiDevice::new(x); embassy_net::new(y); bring_up_stack(z); */"; then
  out="$("$CHK" "$work/tree" 2>&1)"; rc=$?
  if [ "$rc" = 0 ]; then ok "comments naming both ctors stay green (rc=0)"
  else no "PROSE FLIPPED THE VERDICT: rc $rc — $out"; fi
else no "prose regression (fixture setup)"; fi
# ...and a real site COMMENTED OUT must not silently satisfy the roster either.
arm "commenting out a real site is caught" 1 "declared but ABSENT" net/mode.rs \
"$DEV_MODE	// $DEV_MODE"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
