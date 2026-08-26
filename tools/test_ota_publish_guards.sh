#!/usr/bin/env bash
# test_ota_publish_guards.sh — #314: `ota_publish.sh install 42` must refuse, loudly, and publish
# nothing. 42 is the C6 watch's unset-config sentinel, an alias two different watches can publish
# under at different times, so an install aimed at it is aimed at an unknown board.
#
# Two levels, because each alone is a gate that cannot fail in the way that matters:
#   1. the DECISION  — assert_ota_targetable refuses 42 and refuses NOTHING else;
#   2. the PLACEMENT — the real script reaches that refusal before it sources a credential or
#      touches mosquitto. A correct predicate called after the publish would pass level 1.
#
# Levels 3-5 add #400: `stage` must not stamp an identity it cannot honour. Same two-level shape,
# because the same two ways of being wrong apply:
#   3. the DECISION  — assert_stamp_is_head / assert_stampable_inputs refuse the right states and,
#      just as importantly, refuse NOTHING else (a guard that refuses every stage is unusable);
#   4. the PLACEMENT — the real script refuses before credentials, AND on the --dirty path actually
#      omits SMOL_RELEASE at the repro_build_bin call. That last one is the whole point of the fix
#      and is invisible to any test that only reads exit codes: the old code force-stamped a clean
#      release onto uncommitted bytes while a comment three lines up said it could not happen. So
#      level 4 asserts the ENV AT THE CALL, via a stubbed repro_build.sh, not the printed message.
#   5. the HELP RANGE — usage() hardcodes `sed -n '2,27p'`, a duplicated fact about how far the
#      header comment runs. Both ends are pinned so editing the header without it fails here.
#
# NO broker, NO vault, NO publish: `bw` and `mosquitto_pub` are PATH-stubbed to touch a marker and
# fail, so any attempt to use them is both harmless and visible. Levels 1-2 run no git command;
# levels 3-5 run git against a THROWAWAY fixture repo under /var/tmp — never this checkout, and
# never /tmp (JP directive: katana's /tmp is a 16 GB tmpfs).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/ota_publish.sh"
[ -f "$SCRIPT" ] || { echo "missing $SCRIPT" >&2; exit 2; }

pass=0; fail=0
ok(){  pass=$((pass+1)); echo "ok   - $1"; }
no(){  fail=$((fail+1)); echo "FAIL - $1"; }
eq(){ if [ "$2" = "$3" ]; then ok "$1 ($2)"; else no "$1: want [$2] got [$3]"; fi; }

echo "== 1. the decision: assert_ota_targetable =="
# Pull in ONLY the function under test — no side effects, none of the parent's `set -e`.
eval "$(awk '/^assert_ota_targetable\(\)\{/,/^\}/' "$SCRIPT")"
type assert_ota_targetable >/dev/null 2>&1 || { echo "extraction failed" >&2; exit 2; }

out="$( (assert_ota_targetable 42) 2>&1 )"; rc=$?
eq  "42 exits 22 (client error)" "22" "$rc"
case "$out" in *"REFUSED"*)            ok "42 refusal says REFUSED" ;;          *) no "not loud: $out" ;; esac
case "$out" in *"sentinel"*)           ok "42 refusal explains the sentinel" ;; *) no "no why: $out" ;; esac
case "$out" in *"NOTHING WAS PUBLISHED"*) ok "42 refusal states nothing published" ;; *) no "no claim: $out" ;; esac
case "$out" in *"real id"*|*"REAL id"*) ok "42 refusal states the fix" ;;       *) no "no fix: $out" ;; esac

# And it must refuse nothing else — a substring or numeric-prefix hazard here would silently
# lock the operator out of real boards (142/420/4/2 all contain or resemble 42).
for id in 7 8 9 2 4 13 41 43 142 420 4242; do
  out="$( (assert_ota_targetable "$id") 2>&1 )"; rc=$?
  if [ "$rc" = 0 ] && [ -z "$out" ]; then ok "id$id allowed, silently"; else no "id$id wrongly refused (rc=$rc) $out"; fi
done

echo "== 2. the placement: the real script, before any credential or publish =="
stub="$(mktemp -d)"; marks="$(mktemp -d)"
cat > "$stub/bw" <<EOF
#!/usr/bin/env bash
touch "$marks/bw"; exit 1
EOF
cat > "$stub/mosquitto_pub" <<EOF
#!/usr/bin/env bash
touch "$marks/pub"; exit 0
EOF
chmod +x "$stub/bw" "$stub/mosquitto_pub"

out="$(PATH="$stub:$PATH" "$SCRIPT" install 42 2>&1)"; rc=$?
eq "install 42 exits 22" "22" "$rc"
case "$out" in *"REFUSED"*) ok "install 42 prints the refusal" ;; *) no "silent skip: $out" ;; esac
[ -e "$marks/pub" ] && no "install 42 PUBLISHED (mosquitto_pub ran)" || ok "install 42 published nothing"
[ -e "$marks/bw" ]  && no "install 42 sourced credentials first"     || ok "install 42 refused before credentials"

# Negative control — proves the two markers above are not simply unreachable. A real id must get
# PAST the guard and reach the credential seam (where the stubbed `bw` fails it deterministically,
# so this control never arms a board).
out="$(PATH="$stub:$PATH" "$SCRIPT" install 8 2>&1)"; rc=$?
[ "$rc" = 22 ] && no "control: id8 was refused as a sentinel" || ok "control: id8 not refused (rc=$rc)"
[ -e "$marks/bw" ] && ok "control: id8 reached the credential seam (markers do fire)" \
                   || no "control: id8 never reached credentials — level-2 markers prove nothing"
[ -e "$marks/pub" ] && no "control: id8 published despite a failed credential source" \
                    || ok "control: id8 published nothing (bw stub failed it first)"
rm -rf "$stub" "$marks"

# ---------------------------------------------------------------------------------------------
# #400 — the stamp must describe the bytes
# ---------------------------------------------------------------------------------------------
TMPROOT="${TMPDIR:-/var/tmp}"; case "$TMPROOT" in /tmp|/tmp/*) TMPROOT=/var/tmp ;; esac
gitq(){ git -c user.email=t@t -c user.name=t -c commit.gpgsign=false -C "$1" "${@:2}"; }

echo "== 3. the decision: assert_stamp_is_head / assert_stampable_inputs =="
d3="$(mktemp -d "$TMPROOT/smol400d-XXXXXX")"
mkdir -p "$d3/rust/clock/src" "$d3/docs"
echo seed > "$d3/rust/clock/src/lib.rs"; echo doc > "$d3/docs/x.md"
gitq "$d3" init -q
gitq "$d3" add rust docs; gitq "$d3" commit -qm one
echo more >> "$d3/docs/x.md"; gitq "$d3" add docs; gitq "$d3" commit -qm two

# Extract ONLY the functions under test — no side effects, none of the parent's traps.
eval "$(awk '/^stage_input_dirt\(\)\{/,/^\}/'        "$SCRIPT")"
eval "$(awk '/^assert_stamp_is_head\(\)\{/,/^\}/'    "$SCRIPT")"
eval "$(awk '/^assert_stampable_inputs\(\)\{/,/^\}/' "$SCRIPT")"
for f in stage_input_dirt assert_stamp_is_head assert_stampable_inputs; do
  type "$f" >/dev/null 2>&1 || { echo "extraction of $f failed" >&2; exit 2; }
done
export REPO="$d3"

out="$( (assert_stamp_is_head HEAD) 2>&1 )"; rc=$?
if [ "$rc" = 0 ] && [ -z "$out" ]; then ok "HEAD accepted, silently"; else no "HEAD refused (rc=$rc) $out"; fi
out="$( (assert_stamp_is_head "$(gitq "$d3" rev-parse HEAD)") 2>&1 )"; rc=$?
eq "HEAD by full sha accepted" "0" "$rc"   # the same commit spelled differently is the SAME source
out="$( (assert_stamp_is_head HEAD~1) 2>&1 )"; rc=$?
eq "non-HEAD commit exits 22" "22" "$rc"
case "$out" in *"WORKING TREE"*) ok "refusal names the real cause" ;;    *) no "no cause: $out" ;; esac
case "$out" in *"NOTHING WAS PUBLISHED"*) ok "refusal states nothing published" ;; *) no "no claim: $out" ;; esac
case "$out" in *"git worktree add"*) ok "refusal states the fix" ;;      *) no "no fix: $out" ;; esac
out="$( (assert_stamp_is_head deadbeef99) 2>&1 )"; rc=$?
eq "unresolvable ref exits 22" "22" "$rc"

# clean tree
out="$( (assert_stampable_inputs 0) 2>&1 )"; rc=$?
if [ "$rc" = 0 ] && [ -z "$out" ]; then ok "clean inputs accepted, silently"; else no "clean refused (rc=$rc) $out"; fi
# THE SCOPE CONTROL, and the arm most worth keeping: dirt OUTSIDE rust/clock must NOT refuse.
# Without this, "the guard fires" is satisfied by a guard that refuses any dirty repo at all —
# which would refuse a stage over an unrelated docs edit, and a gate that fires on innocent
# states is one operators route around (#338). This asserts the scope is load-bearing.
echo stray >> "$d3/docs/x.md"
out="$( (assert_stampable_inputs 0) 2>&1 )"; rc=$?
if [ "$rc" = 0 ] && [ -z "$out" ]; then ok "dirt OUTSIDE rust/clock accepted (scope is real)"; else no "scope too wide: refused an unrelated docs edit (rc=$rc)"; fi
gitq "$d3" checkout -q -- docs/x.md
# dirty build input
echo stray >> "$d3/rust/clock/src/lib.rs"
out="$( (assert_stampable_inputs 0) 2>&1 )"; rc=$?
eq "dirty build input exits 22" "22" "$rc"
case "$out" in *"rust/clock/src/lib.rs"*) ok "refusal NAMES the dirty file" ;; *) no "does not say what is dirty: $out" ;; esac
case "$out" in *"--dirty"*) ok "refusal offers the named override" ;; *) no "no override offered: $out" ;; esac
out="$( (assert_stampable_inputs 1) 2>&1 )"; rc=$?
if [ "$rc" = 0 ] && [ -z "$out" ]; then ok "--dirty accepts the same dirt"; else no "--dirty still refused (rc=$rc) $out"; fi
# An untracked, git-IGNORED file must not count as dirt: board.rs/secrets.rs are ignored by design
# and permanently absent from git's index, so counting untracked files would refuse every stage.
gitq "$d3" checkout -q -- rust/clock/src/lib.rs
echo 'ignored' > "$d3/rust/clock/src/board.rs"
printf 'board.rs\n' > "$d3/.gitignore"
out="$( (assert_stampable_inputs 0) 2>&1 )"; rc=$?
eq "an ignored untracked build-input file is NOT dirt" "0" "$rc"
unset REPO
rm -rf "$d3"

echo "== 4. the placement: refuses before credentials, and omits SMOL_RELEASE on --dirty =="
# A fixture checkout with a STUBBED repro_build.sh, so the build call is observable without cargo.
# The stub records the env it was called WITH — the assertion the fix actually needs, and the one
# the old folklore comment would have passed while being false.
f4="$(mktemp -d "$TMPROOT/smol400p-XXXXXX")"
mkdir -p "$f4/tools" "$f4/rust/clock/src/net" "$f4/docs"
cp "$SCRIPT" "$f4/tools/ota_publish.sh"
cat > "$f4/tools/repro_build.sh" <<'STUB'
# test stub: record the release-stamp env AT THE CALL, then fail so nothing is hosted or published.
repro_build_bin(){ printf '%s' "${SMOL_RELEASE-UNSET}" > "$SMOL400_LOG"; return 1; }
STUB
echo '// seed' > "$f4/rust/clock/src/net/etx.rs"; echo doc > "$f4/docs/x.md"
gitq "$f4" init -q
gitq "$f4" add tools rust docs; gitq "$f4" commit -qm one
echo more >> "$f4/docs/x.md"; gitq "$f4" add docs; gitq "$f4" commit -qm two

stub2="$(mktemp -d "$TMPROOT/smol400s-XXXXXX")"; marks2="$(mktemp -d "$TMPROOT/smol400m-XXXXXX")"
for t in bw mosquitto_pub mosquitto_sub; do
  printf '#!/usr/bin/env bash\ntouch %s/%s\nexit 1\n' "$marks2" "$t" > "$stub2/$t"; chmod +x "$stub2/$t"
done
LOG="$marks2/release"
run4(){ # [extra stage args...] -> sets R4 (exit code) and OUT4 (output). `stage` is ALREADY supplied,
        # so `run4` alone is a bare stage; do not pass it again (that becomes the <commit> argument).
  # Clear EVERY marker, not just the log: these markers are cumulative touch-files, so a previous
  # run's credential marker would otherwise be read as this run's, and each "refused before
  # credentials" arm would fail against correct code. (It did, exactly once, while writing this.)
  rm -f "$LOG" "$marks2/bw" "$marks2/mosquitto_pub" "$marks2/mosquitto_sub"
  OUT4="$(SMOL400_LOG="$LOG" PATH="$stub2:$PATH" bash "$f4/tools/ota_publish.sh" stage "$@" 2>&1)"; R4=$?
}
stamp4(){ [ -f "$LOG" ] && cat "$LOG" || echo "NOT-CALLED"; }

run4;                  eq "clean stage reaches the build"            "1"          "$R4"
eq  "clean stage stamps a RELEASE (SMOL_RELEASE=1)"                  "1"          "$(stamp4)"

echo '// stray' >> "$f4/rust/clock/src/net/etx.rs"
run4;                  eq "dirty stage exits 22"                     "22"         "$R4"
eq  "dirty stage never reached the build"                            "NOT-CALLED" "$(stamp4)"
[ -e "$marks2/bw" ] && no "dirty stage sourced credentials first" || ok "dirty stage refused before credentials"

run4 --dirty;         eq "--dirty stage reaches the build"          "1"          "$R4"
# THE KEYSTONE ASSERTION. If this reads "1" the fix is cosmetic: the image would still carry a
# clean release stamp over uncommitted bytes, which is exactly the masquerade #400 filed.
eq  "--dirty stage omits SMOL_RELEASE (DEV stamp)"                   "UNSET"      "$(stamp4)"
case "$OUT4" in *"stamping DEV"*) ok "--dirty says so on stdout" ;; *) no "--dirty is silent about the dev stamp" ;; esac
gitq "$f4" checkout -q -- rust/clock/src/net/etx.rs

# Scope control at the SCRIPT level, not just the predicate: an unrelated edit must still release.
echo stray >> "$f4/docs/x.md"
run4;                  eq "unrelated docs edit does not block a stage" "1"        "$R4"
eq  "...and still stamps a RELEASE"                                  "1"          "$(stamp4)"
gitq "$f4" checkout -q -- docs/x.md

run4 HEAD~1;          eq "non-HEAD stage exits 22"                   "22"        "$R4"
eq  "non-HEAD stage never reached the build"                          "NOT-CALLED" "$(stamp4)"
[ -e "$marks2/bw" ] && no "non-HEAD stage sourced credentials first" || ok "non-HEAD refused before credentials"
# Negative control: the credential marker must be ABLE to fire, or the two assertions above prove
# nothing. A clean stage gets past the guards and hits the broker read on its way to the build.
run4
[ -e "$marks2/bw" ] && ok "control: a clean stage DOES reach credentials (markers fire)" \
                    || no "control: nothing ever reaches credentials — placement arms are vacuous"
rm -rf "$f4" "$stub2" "$marks2"

echo "== 5. the help range: usage() covers the whole header and no more =="
help="$(bash "$SCRIPT" 2>&1)"; rc=$?
eq "no-args prints help and exits 1" "1" "$rc"
case "$help" in *"--dirty"*)      ok "help documents --dirty" ;;      *) no "help omits --dirty — the sed range is short" ;; esac
case "$help" in *"IDENTITY (#400)"*) ok "help documents the identity rule" ;; *) no "help omits the #400 rule" ;; esac
# Both ENDS. Short-range rot truncates the tail; long-range rot spills code into the help text.
case "$help" in *"choose_build()"*) ok "help reaches the end of the header block" ;; *) no "sed range ends early" ;; esac
case "$help" in *"set -euo"*|*"die()"*) no "sed range overran into code" ;; *) ok "help stops before the code" ;; esac

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
