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
# NO broker, NO vault, NO publish: `bw` and `mosquitto_pub` are PATH-stubbed to touch a marker and
# fail, so any attempt to use them is both harmless and visible. Runs no git command.
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

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
