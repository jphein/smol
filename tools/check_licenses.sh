#!/usr/bin/env bash
# check_licenses.sh — every package smol owns declares smol's license, as cargo itself reports it.
#
# smol is AGPL-3.0-or-later since #555 (JP, 2026-09-27). #555 changed LICENSE and the README but not
# one manifest, so the packages kept saying "MIT OR Apache-2.0", or nothing at all. Crate metadata is
# what `cargo metadata`, crates tooling and SBOM scanners read, so a stale field is a false licence
# claim that outlives the prose it contradicts. This makes the metadata a checked fact.
#
# It asks CARGO, not grep: `cargo metadata --no-deps` per tracked Cargo.toml, so `license.workspace
# = true` resolves to what the workspace really says. A package with no field reads as null and
# fails like a wrong one. A manifest cargo cannot read fails closed: an unreadable manifest is an
# unchecked one.
#
# NOT smol's to relicense, and so declared here rather than silently skipped:
#   targets/c6-watch/**   third-party subtree (waveshare-watch-rs, MIT OR Apache-2.0; its vendored
#                         i-slint-renderer-software is GPL-3.0-only OR Slint), per README "License"
#   rust/sigil-names      a vendored copy of JP's realm-sigil, which is LGPL-3.0-only upstream.
#                         #555 relicensed smol's own code, not a copy of another repo's
#
# Exit 0 all agree · 1 a package disagrees or cannot be read · 2 no cargo.
set -uo pipefail

EXPECTED="AGPL-3.0-or-later"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXEMPT_RE='^(targets/c6-watch/|rust/sigil-names/Cargo\.toml$)'

command -v cargo >/dev/null || { echo "FATAL: no cargo on PATH" >&2; exit 2; }
cd "$ROOT"
rc=0; n=0
while read -r m; do
  [[ "$m" =~ $EXEMPT_RE ]] && continue
  if ! json=$(cargo metadata --no-deps --offline --format-version 1 --manifest-path "$ROOT/$m" 2>/dev/null); then
    echo "FAIL: $m — cargo metadata could not read it (unchecked is not checked)"; rc=1; continue
  fi
  # The package whose manifest IS this file. A virtual workspace root has none, and its members are
  # checked through their own Cargo.toml.
  line=$(MANIFEST="$ROOT/$m" python3 -c '
import json, os, sys
d = json.load(sys.stdin)
for p in d["packages"]:
    if os.path.realpath(p["manifest_path"]) == os.path.realpath(os.environ["MANIFEST"]):
        print(p["name"] + "\t" + str(p["license"]))
' <<<"$json")
  [[ -z "$line" ]] && continue
  name="${line%%$'\t'*}"; lic="${line#*$'\t'}"
  n=$((n + 1))
  if [[ "$lic" != "$EXPECTED" ]]; then
    echo "FAIL: $m ($name) declares license '$lic', not '$EXPECTED'"; rc=1
  fi
done < <(git ls-files '*Cargo.toml')
if [[ $n -eq 0 ]]; then echo "FAIL: no packages checked — the instrument saw nothing"; exit 1; fi
[[ $rc -eq 0 ]] && echo "ok: $n packages declare $EXPECTED (exempt: targets/c6-watch/**, rust/sigil-names)"
exit "$rc"
