#!/usr/bin/env bash
# #338: create the git-ignored per-board provisioning a firmware build needs, for a CLEAN CHECKOUT
# (CI, or a fresh worktree). `src/secrets.rs` and `src/board.rs` are git-ignored by design — the repo
# is public — so nothing in a fresh clone compiles the firmware until they exist. That is precisely
# why no CI job ever built a tier: the build was impossible without a manual step nobody had written
# down. This writes throwaway values good enough to COMPILE and never good enough to ship.
#
# NON-DESTRUCTIVE: an existing secrets.rs/board.rs is never rewritten or clobbered. A developer's
# real WiFi creds and fleet GROUP_KEY must survive running the gate locally — clobbering them would
# make `tools/gate.sh` something people avoid, and a gate people avoid is the failure #338 is about.
#
# #359 — TOP-UP, not present/absent. Present/absent is the wrong predicate for a file whose required
# contents GROW: #190 added `GROUP_KEY`/`GROUP_KEY_EPOCH` to `secrets.rs.example`, so every worktree
# provisioned before it kept a file that existed, was "left untouched", and failed EVERY espnow tier
# with `cannot find value GROUP_KEY` — while CI stayed green (a fresh checkout provisions from the
# current example). It reads as a code bug; it cost two agents a debugging round each. So an existing
# file is now diffed AGAINST the example by SYMBOL, and anything the example declares that the file
# does not is APPENDED (values = the example placeholders) and reported loudly. Existing symbols are
# never touched — a real credential is never overwritten by a placeholder. The trap otherwise
# re-arms every single time a feature adds a secret.
#
# Usage: tools/ci_provision.sh [--check] [clock_dir]     (default clock_dir: rust/clock)
#   --check   report only; make NO change; exit 3 if any file is missing symbols the example
#             declares. For a gate that wants to fail loudly instead of self-healing.
set -euo pipefail

mode=apply
args=()
for a in "$@"; do
  case "$a" in
    --check) mode=check ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) args+=("$a") ;;
  esac
done
clock="${args[0]:-rust/clock}"
src="$clock/src"
[ -d "$src" ] || { echo "ci_provision: no such dir: $src" >&2; exit 1; }

# Symbol-level top-up. Parses the .example for top-level item declarations (const / static / fn /
# const fn / struct / enum / union / trait / type / mod), carrying each item's attributes and
# doc-comment header, and appends only the ones the target does not already declare.
#   argv: <example> <target> <apply|check>
#   stdout: one missing symbol name per line;  exit 0 = nothing missing, 3 = something was missing
TOPUP_PY='
import re, sys

example, target, mode = sys.argv[1], sys.argv[2], sys.argv[3]

START = re.compile(
    r"^(?:pub(?:\s*\([^)]*\))?\s+)?(?:"
    r"const\s+fn\s+(\w+)"
    r"|(?:const|static)\s+(?:mut\s+)?(\w+)"
    r"|(?:struct|enum|union|trait|type|mod)\s+(\w+)"
    r"|(?:async\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?fn\s+(\w+)"
    r")\b")

def code(line):
    """The line with a trailing // comment dropped (these files hold no // inside a literal)."""
    return line.split("//", 1)[0]

def declares(text, name):
    return re.search(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?"
                     r"(?:const\s+fn|const|static|struct|enum|union|trait|type|mod|fn)\s+"
                     r"(?:mut\s+)?" + re.escape(name) + r"\b", text) is not None

lines = open(example).read().splitlines(keepends=True)
items, seen, i = [], set(), 0
while i < len(lines):
    m = START.match(lines[i].lstrip())
    if not m:
        i += 1
        continue
    name = next(g for g in m.groups() if g)
    # back up over the contiguous attribute / doc-comment header (a blank line ends it)
    j = i
    while j > 0:
        prev = lines[j - 1].strip()
        if prev.startswith("#[") or (prev.startswith("//") and not prev.startswith("//!")):
            j -= 1
        else:
            break
    # forward to the end of the item: brackets balanced AND the line closes it
    depth, k = 0, i
    while k < len(lines):
        c = code(lines[k])
        depth += c.count("{") + c.count("[") + c.count("(")
        depth -= c.count("}") + c.count("]") + c.count(")")
        t = c.rstrip()
        if depth <= 0 and (t.endswith(";") or t.endswith("}")):
            break
        k += 1
    if name not in seen:
        seen.add(name)
        items.append((name, "".join(lines[j:k + 1])))
    i = k + 1

tgt = open(target).read()
missing = [(n, t) for n, t in items if not declares(tgt, n)]
if not missing:
    sys.exit(0)
for n, _ in missing:
    print(n)
if mode == "apply":
    ex = example.rsplit("/", 1)[-1]
    with open(target, "a") as fh:
        fh.write("\n// " + "-" * 74 + "\n"
                 "// APPENDED BY tools/ci_provision.sh (#359) — symbols `" + ex + "` declares that\n"
                 "// this file did not. The values below are the EXAMPLE PLACEHOLDERS: they make the\n"
                 "// tree COMPILE and they are NOT fleet-valid. Set the real values before flashing\n"
                 "// anything you expect to join the fleet.\n"
                 "// " + "-" * 74 + "\n")
        for _, t in missing:
            fh.write("\n" + t.rstrip("\n") + "\n")
sys.exit(3)
'

topped_up=""
for f in secrets board; do
  if [ ! -f "$src/$f.rs" ]; then
    if [ -f "$src/$f.rs.example" ]; then
      [ "$mode" = check ] && { echo "ci_provision: $src/$f.rs is ABSENT (run tools/ci_provision.sh)" >&2; exit 3; }
      cp "$src/$f.rs.example" "$src/$f.rs"
      echo "  $f.rs: created from $f.rs.example"
    else
      echo "ci_provision: missing $src/$f.rs.example" >&2; exit 1
    fi
  elif [ ! -f "$src/$f.rs.example" ]; then
    echo "  $f.rs: present; no $f.rs.example to diff against"
  else
    set +e
    miss="$(python3 -c "$TOPUP_PY" "$src/$f.rs.example" "$src/$f.rs" "$mode")"
    rc=$?
    set -e
    if [ "$rc" = 0 ]; then
      echo "  $f.rs: present, complete against $f.rs.example"
    elif [ "$rc" = 3 ]; then
      names="$(echo "$miss" | tr '\n' ' ')"
      if [ "$mode" = check ]; then
        echo "ci_provision: $src/$f.rs is MISSING symbols $f.rs.example declares: $names" >&2
        echo "ci_provision: fix — run 'tools/ci_provision.sh $clock' (tops them up), or copy them" >&2
        echo "ci_provision: by hand from $src/$f.rs.example and set real values." >&2
        exit 3
      fi
      echo "  ⚠️  $f.rs: was MISSING symbols $f.rs.example declares — APPENDED: $names"
      echo "  ⚠️  $f.rs: those are PLACEHOLDER values. This tree now COMPILES; a board flashed"
      echo "  ⚠️  $f.rs: from it is NOT fleet-valid until you set them in $src/$f.rs."
      topped_up="$topped_up $f"
    else
      echo "ci_provision: top-up check failed for $src/$f.rs (python exit $rc)" >&2; exit 1
    fi
  fi
done

# #190/#336 forward-compatibility. `secrets.rs.example` ships `GROUP_KEY = [0u8; 32]` AND
# `net/mode.rs` carries a compile-time assert that REFUSES the all-zero key (the repo is public, so
# the example key is a published credential). A CI build from the unedited example would therefore
# fail to compile — correctly, but uselessly. So if the key is present and zeroed, substitute a
# random one. This also catches the key the #359 top-up just appended.
#
# This key is a BUILD-LOCAL THROWAWAY and must never reach a board: it is regenerated every run, is
# not the fleet key, and a node built with it cannot talk to the fleet. That is the intended
# property — CI proves the code COMPILES and the guard WORKS, and cannot accidentally emit a
# flashable image that would join the mesh.
if [ "$mode" = apply ] && [ -f "$src/secrets.rs" ] && grep -q "GROUP_KEY" "$src/secrets.rs"; then
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

[ -n "$topped_up" ] && echo "  (#359 top-up ran for:$topped_up — present/absent would have let the build fail instead)"
exit 0
