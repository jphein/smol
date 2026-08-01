#!/usr/bin/env bash
# #338: create the git-ignored per-board provisioning a firmware build needs, for a CLEAN CHECKOUT
# (CI, or a fresh worktree). `src/secrets.rs` and `src/board.rs` are git-ignored by design — the repo
# is public — so nothing in a fresh clone compiles the firmware until they exist. That is precisely
# why no CI job ever built a tier: the build was impossible without a manual step nobody had written
# down. This writes throwaway values good enough to COMPILE and never good enough to ship.
#
# NON-DESTRUCTIVE: an existing secrets.rs/board.rs is left ALONE. A developer's real WiFi creds and
# fleet GROUP_KEY must survive running the gate locally — clobbering them would make `tools/gate.sh`
# something people avoid, and a gate people avoid is the failure this issue is about.
#
# Usage: tools/ci_provision.sh [clock_dir]     (default: rust/clock)
set -euo pipefail

clock="${1:-rust/clock}"
src="$clock/src"
[ -d "$src" ] || { echo "ci_provision: no such dir: $src" >&2; exit 1; }

for f in secrets board; do
  if [ -f "$src/$f.rs" ]; then
    echo "  $f.rs: present, left untouched"
  elif [ -f "$src/$f.rs.example" ]; then
    cp "$src/$f.rs.example" "$src/$f.rs"
    echo "  $f.rs: created from $f.rs.example"
  else
    echo "ci_provision: missing $src/$f.rs.example" >&2; exit 1
  fi
done

# #190/#336 forward-compatibility. Once the #266 core lands, `secrets.rs.example` ships
# `GROUP_KEY = [0u8; 32]` AND `net/mode.rs` carries a compile-time assert that REFUSES the all-zero
# key (the repo is public, so the example key is a published credential). A CI build from the
# unedited example would therefore fail to compile — correctly, but uselessly. So if the key is
# present and zeroed, substitute a random one.
#
# This key is a BUILD-LOCAL THROWAWAY and must never reach a board: it is regenerated every run, is
# not the fleet key, and a node built with it cannot talk to the fleet. That is the intended
# property — CI proves the code COMPILES and the guard WORKS, and cannot accidentally emit a
# flashable image that would join the mesh.
if [ -f "$src/secrets.rs" ] && grep -q "GROUP_KEY" "$src/secrets.rs"; then
  if grep -qE 'GROUP_KEY: *\[u8; *32\] *= *\[0u8; *32\]' "$src/secrets.rs"; then
    key=$(od -An -tu1 -N32 /dev/urandom | tr -s ' ' | tr ' ' '\n' | grep -E '^[0-9]+$' | paste -sd, -)
    # Guarantee non-zero even in the (astronomically unlikely) all-zero draw — the point is the
    # guard, and a gate that can emit the value it exists to reject is not a gate.
    key="1,${key#*,}"
    python3 - "$src/secrets.rs" "$key" <<'PY'
import re, sys
path, key = sys.argv[1], sys.argv[2]
s = open(path).read()
s = re.sub(r'(GROUP_KEY: *\[u8; *32\] *= *)\[0u8; *32\]',
           lambda m: m.group(1) + '[' + key + ']', s)
open(path, 'w').write(s)
PY
    echo "  secrets.rs: GROUP_KEY was the published all-zero example — substituted a random CI key"
  else
    echo "  secrets.rs: GROUP_KEY already set, left untouched"
  fi
fi
