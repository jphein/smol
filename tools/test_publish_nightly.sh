#!/usr/bin/env bash
# test_publish_nightly.sh — #574. Prove tools/publish_nightly.sh publishes one complete set or nothing,
# against a FAKE `gh` that keeps the "release" in a directory (no network, no real release touched).
# Arms (each observed):
#   1. a complete set: every image + notes published, SHA256SUMS uploaded LAST and covering exactly
#      this run's images, old assets beyond retention pruned, recent ones and SHA256SUMS kept;
#   2. incomplete sets REFUSE (exit 2) with the release untouched: an empty set, a .bin without its
#      NOTES, a name in two sets, no sets at all;
#   3. an upload that silently drops a file -> exit 1 and SHA256SUMS is NOT replaced (the commit point).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUB="$ROOT/tools/publish_nightly.sh"
W="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/var/tmp}}/test-publish-nightly.XXXXXX")"; trap 'rm -rf "$W"' EXIT
pass=0; fail=0
ok()  { printf '   PASS %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '   FAIL %s\n' "$1"; fail=$((fail+1)); }

# the fake gh: release = $REL/ ; log of calls = $REL.log ; FAKE_DROP=<name> silently skips one upload
cat > "$W/gh" <<'GH'
#!/usr/bin/env bash
REL="${FAKE_REL:?}"; echo "$*" >> "$REL.log"
case "$1 $2" in
  "release view")
    [ -d "$REL" ] || exit 1
    if [ "${4:-}" = "--json" ]; then
      for f in "$REL"/*; do [ -e "$f" ] || continue; n="$(basename "$f")"
        printf '%s\t%s\tfake://%s\n' "$n" "$(stat -c %s "$f")" "$n"; done
    fi ;;
  "release create") mkdir -p "$REL" ;;
  "release upload")
    shift 3
    for f in "$@"; do [ "$f" = --clobber ] && continue
      [ "$(basename "$f")" = "${FAKE_DROP:-}" ] && continue
      cp "$f" "$REL/"; done ;;
  "api -X") rm -f "$REL/${4#fake://}" ;;
  *) echo "fake gh: unexpected $*" >&2; exit 9 ;;
esac
GH
chmod +x "$W/gh"
export GH="$W/gh" TODAY=2026-11-13 NIGHTLY_KEEP_DAYS=14

stage() { # $1 dir; writes three sets: c3 (2 images), gui (1), s3 (1)
  rm -rf "$1"; mkdir -p "$1/riscv" "$1/gui" "$1/s3"
  for p in "riscv/smol-c3" "riscv/smol-c3-oled" "gui/smol-c6-watch-gui" "s3/smol-s3-cyd"; do
    printf 'image %s\n' "$p" > "$1/$p-2026-11-13-gabc1234def0.bin"
    printf 'notes %s\n' "$p" > "$1/$p-2026-11-13-gabc1234def0.bin.NOTES.md"
  done
}
fresh_release() { # an existing release with an old night, a recent night and an old SHA256SUMS
  rm -rf "$FAKE_REL" "$FAKE_REL.log"; mkdir -p "$FAKE_REL"
  for d in 2026-10-01 2026-11-10; do
    echo old > "$FAKE_REL/smol-c3-$d-g1111111aaaa.bin"; echo n > "$FAKE_REL/smol-c3-$d-g1111111aaaa.bin.NOTES.md"; done
  echo "old sums" > "$FAKE_REL/SHA256SUMS"
}
export FAKE_REL="$W/rel"

# 1. complete set
stage "$W/s"; fresh_release
if "$PUB" "$W/s" >"$W/out" 2>&1; then
  n=$(ls "$FAKE_REL"/*2026-11-13*.bin 2>/dev/null | wc -l)
  last="$(grep 'release upload' "$FAKE_REL.log" | tail -1)"
  sums_ok="$(cd "$FAKE_REL" && sha256sum -c SHA256SUMS >/dev/null 2>&1 && wc -l < SHA256SUMS)"
  [ "$n" = 4 ] && ok "complete set: all 4 images published" || bad "complete set: $n of 4 images published"
  [[ "$last" == *" SHA256SUMS --clobber" ]] && ok "SHA256SUMS is the LAST upload (the commit point)" || bad "last upload was: $last"
  [ "$sums_ok" = 4 ] && ok "SHA256SUMS verifies and covers exactly this run's 4 images" || bad "SHA256SUMS: '$sums_ok'"
  [ ! -e "$FAKE_REL/smol-c3-2026-10-01-g1111111aaaa.bin" ] && [ ! -e "$FAKE_REL/smol-c3-2026-10-01-g1111111aaaa.bin.NOTES.md" ] \
    && ok "an asset past retention (2026-10-01) is pruned, notes too" || bad "old assets were not pruned"
  [ -e "$FAKE_REL/smol-c3-2026-11-10-g1111111aaaa.bin" ] && ok "a recent night (2026-11-10) is kept" || bad "a recent night was pruned"
else bad "complete set: exit $? — $(tail -2 "$W/out")"; fi

# 2. incomplete sets refuse with the release untouched
refuse_case() { # $1 name, $2 setup command
  stage "$W/s"; eval "$2"; fresh_release; before="$(cd "$FAKE_REL" && ls | md5sum)"
  "$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
  if [ "$rc" = 2 ] && [ "$before" = "$(cd "$FAKE_REL" && ls | md5sum)" ] && ! grep -q "release upload" "$FAKE_REL.log" 2>/dev/null; then
    ok "refuses (exit 2, release untouched): $1"
  else bad "$1: exit $rc; $(tail -1 "$W/out")"; fi
}
refuse_case "an empty staged set"            'rm -f "$W/s/gui/"*'
refuse_case "a .bin without its NOTES"       'rm -f "$W/s/s3/"*.NOTES.md'
refuse_case "one name in two staged sets"    'cp "$W/s/s3/"*.bin "$W/s/gui/"'
refuse_case "no staged sets at all"          'rm -rf "$W/s/"*'

# 3. a silently dropped upload: exit 1, SHA256SUMS stays on the previous complete set
stage "$W/s"; fresh_release
FAKE_DROP="smol-s3-cyd-2026-11-13-gabc1234def0.bin" "$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
if [ "$rc" = 1 ] && [ "$(cat "$FAKE_REL/SHA256SUMS")" = "old sums" ] && grep -q "NOT PUBLISHED: smol-s3-cyd" "$W/out"; then
  ok "a dropped upload fails (exit 1) and SHA256SUMS is NOT replaced"
else bad "dropped upload: exit $rc, SHA256SUMS='$(head -c 40 "$FAKE_REL/SHA256SUMS")'"; fi

# 4. the prune cap: with NIGHTLY_PRUNE_MAX=1 only one old asset goes per run
stage "$W/s"; fresh_release
NIGHTLY_PRUNE_MAX=1 "$PUB" "$W/s" >"$W/out" 2>&1
left=$(ls "$FAKE_REL"/*2026-10-01* 2>/dev/null | wc -l)
[ "$left" = 1 ] && ok "the prune cap holds: 1 of 2 old assets deleted this run" || bad "prune cap: $left old assets left (want 1)"

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
