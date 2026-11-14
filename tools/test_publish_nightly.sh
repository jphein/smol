#!/usr/bin/env bash
# test_publish_nightly.sh — #574. Prove tools/publish_nightly.sh commits one complete set or nothing,
# against a FAKE `gh` that keeps the "release" in a directory (no network; no real release touched).
# Each arm is observed:
#   1. a complete set commits: images + notes on the release, the per-run manifest written after them
#      and verifying, plain SHA256SUMS a copy of it, old SETS pruned whole, recent ones kept;
#   2. incomplete or wrong sets REFUSE (exit 2) with the release untouched: an image missing from the
#      expected set, an extra one, an image without NOTES, a name in two staged sets, images from two
#      runs, no staged sets;
#   3. an upload whose bytes do not match (digest) -> exit 1 and NO manifest is written;
#   4. a per-run manifest that already exists with other content is never overwritten (exit 1);
#   5. capacity: a release too full for this set refuses before uploading (exit 1), after pruning;
#   6. the prune cap deletes whole sets only; the set plain SHA256SUMS names is never pruned;
#   7. release_targets.sh --expected, run on the REAL manifests, lists the images the publisher demands.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUB="$ROOT/tools/publish_nightly.sh"
W="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/var/tmp}}/test-publish-nightly.XXXXXX")"; trap 'rm -rf "$W"' EXIT
pass=0; fail=0
ok()  { printf '   PASS %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '   FAIL %s\n' "$1"; fail=$((fail+1)); }

# the fake gh: release = $FAKE_REL/ ; calls logged to $FAKE_REL.log ; FAKE_CORRUPT=<name> uploads other bytes
cat > "$W/gh" <<'GH'
#!/usr/bin/env bash
REL="${FAKE_REL:?}"; echo "$*" >> "$REL.log"
if [ "$1" = api ] && [ "$2" = -X ]; then rm -f "$REL/${4#fake://}"; exit 0; fi
if [ "$1" = api ] && [ "$2" = --paginate ]; then
  [ -n "${FAKE_LIST_FAIL:-}" ] && exit 1
  for f in "$REL"/*; do [ -e "$f" ] || continue
    n="$(basename "$f")"; d="sha256:$(sha256sum < "$f" | cut -d' ' -f1)"; [ "$n" = "${FAKE_NODIGEST:-}" ] && d=""
    printf '%s\t%s\tuploaded\tfake://%s\n' "$n" "$d" "$n"
  done; exit 0; fi
if [ "$1" = api ]; then [ -d "$REL" ] && { echo 1; exit 0; }; exit 1; fi
case "$1 $2" in
  "release create") mkdir -p "$REL" ;;
  "release upload")
    shift 3
    while [ $# -gt 0 ]; do
      case "$1" in --repo) shift 2; continue ;; --clobber) shift; continue ;; esac
      n="$(basename "$1")"
      if [ "$n" = "${FAKE_CORRUPT:-}" ]; then echo corrupted > "$REL/$n"; else cp "$1" "$REL/"; fi
      shift
    done ;;
  *) echo "fake gh: unexpected $*" >&2; exit 9 ;;
esac
GH
chmod +x "$W/gh"
export GH="$W/gh" GH_REPO=fake/repo TODAY=2026-11-13 NIGHTLY_KEEP_DAYS=14 FAKE_REL="$W/rel"
printf 'smol-c3\nsmol-c3-oled\nsmol-s3-cyd\nsmol-watch-c6\n' > "$W/expected"
export EXPECTED_CMD="cat $W/expected"
S=2026-11-13-gabc1234def0

stage() { # $1 dir: three staged sets holding exactly the expected four images
  rm -rf "$1"; mkdir -p "$1/riscv" "$1/gui" "$1/s3"
  for p in riscv/smol-c3 riscv/smol-c3-oled gui/smol-watch-c6 s3/smol-s3-cyd; do
    printf 'image %s\n' "$p" > "$1/$p-$S.bin"; printf 'notes %s\n' "$p" > "$1/$p-$S.bin.NOTES.md"
  done
}
put_set() { # $1 stamp: an old committed-looking set on the fake release
  echo "old $1" > "$FAKE_REL/smol-c3-$1.bin"; echo n > "$FAKE_REL/smol-c3-$1.bin.NOTES.md"
  echo "sums $1" > "$FAKE_REL/SHA256SUMS-$1"
}
fresh_release() {
  rm -rf "$FAKE_REL" "$FAKE_REL.log"; mkdir -p "$FAKE_REL"
  put_set 2026-10-01-g1111111aaaa; put_set 2026-10-02-g2222222bbbb; put_set 2026-11-10-g3333333cccc
  echo "previous sums" > "$FAKE_REL/SHA256SUMS"
}
snapshot() { (cd "$FAKE_REL" && for f in *; do printf '%s %s\n' "$f" "$(md5sum < "$f")"; done); }

# 1. a complete set commits
stage "$W/s"; fresh_release
if "$PUB" "$W/s" >"$W/out" 2>&1; then
  n=$(ls "$FAKE_REL"/*"$S".bin 2>/dev/null | wc -l)
  [ "$n" = 4 ] && ok "complete set: all 4 images published" || bad "complete set: $n of 4 images"
  m_line=$(grep -n "SHA256SUMS-$S" "$FAKE_REL.log" | head -1 | cut -d: -f1)
  i_line=$(grep -n "release upload.*\.bin" "$FAKE_REL.log" | head -1 | cut -d: -f1)
  [ -n "$m_line" ] && [ -n "$i_line" ] && [ "$m_line" -gt "$i_line" ] && [[ "$(grep 'release upload' "$FAKE_REL.log" | tail -1)" == *" SHA256SUMS --clobber" ]] \
    && ok "order: images, then SHA256SUMS-$S, then plain SHA256SUMS last" || bad "upload order wrong: $(grep 'release upload' "$FAKE_REL.log" | tr '\n' '|')"
  (cd "$FAKE_REL" && sha256sum -c "SHA256SUMS-$S" >/dev/null 2>&1) && [ "$(wc -l < "$FAKE_REL/SHA256SUMS-$S")" = 4 ] \
    && cmp -s "$FAKE_REL/SHA256SUMS" "$FAKE_REL/SHA256SUMS-$S" \
    && ok "the manifest verifies, covers exactly this run's 4 images, and SHA256SUMS copies it" || bad "manifest wrong"
  [ -z "$(ls "$FAKE_REL" | grep -E '2026-10-0[12]')" ] && ok "sets past retention pruned WHOLE (image, notes, manifest)" \
    || bad "old sets left: $(ls "$FAKE_REL" | grep -E '2026-10-0[12]' | tr '\n' ' ')"
  [ -e "$FAKE_REL/smol-c3-2026-11-10-g3333333cccc.bin" ] && [ -e "$FAKE_REL/SHA256SUMS-2026-11-10-g3333333cccc" ] \
    && ok "a set inside retention (2026-11-10) is kept" || bad "a recent set was pruned"
else bad "complete set: exit $? — $(tail -2 "$W/out")"; fi

# 2. refusals leave the release untouched
refuse_case() { # $1 name, $2 setup
  stage "$W/s"; eval "$2"; fresh_release; before="$(snapshot)"
  "$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
  if [ "$rc" = 2 ] && [ "$before" = "$(snapshot)" ] && ! grep -q "release upload\|-X DELETE" "$FAKE_REL.log" 2>/dev/null; then
    ok "refuses (exit 2, release untouched): $1"
  else bad "$1: exit $rc; $(tail -1 "$W/out")"; fi
}
refuse_case "an expected image is missing"     'rm -f "$W/s/s3/"*'"; rmdir \"\$W/s/s3\""
refuse_case "an image the manifests do not expect" 'printf x > "$W/s/s3/smol-extra-$S.bin"; printf n > "$W/s/s3/smol-extra-$S.bin.NOTES.md"'
refuse_case "an image without its NOTES"       'rm -f "$W/s/s3/"*.NOTES.md'
refuse_case "one name in two staged sets"      'cp "$W/s/s3/"*.bin "$W/s/gui/"; cp "$W/s/s3/"*.NOTES.md "$W/s/gui/"'
refuse_case "images from two runs"             'for f in "$W/s/s3/"*; do mv "$f" "${f/abc1234def0/fff9999eee8}"; done'
refuse_case "no staged sets"                   'rm -rf "$W/s/"*'

# 3. a corrupted upload: exit 1, no manifest, plain SHA256SUMS untouched
stage "$W/s"; fresh_release
FAKE_CORRUPT="smol-s3-cyd-$S.bin" "$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
if [ "$rc" = 1 ] && [ ! -e "$FAKE_REL/SHA256SUMS-$S" ] && [ "$(cat "$FAKE_REL/SHA256SUMS")" = "previous sums" ] \
   && grep -q "NOT PUBLISHED: smol-s3-cyd" "$W/out"; then
  ok "a digest mismatch fails (exit 1): no manifest, SHA256SUMS unchanged"
else bad "corrupt upload: exit $rc; manifest $( [ -e "$FAKE_REL/SHA256SUMS-$S" ] && echo written || echo absent)"; fi

# 4. an existing per-run manifest with other content is never overwritten
stage "$W/s"; fresh_release; echo "someone else's" > "$FAKE_REL/SHA256SUMS-$S"
"$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
[ "$rc" = 1 ] && [ "$(cat "$FAKE_REL/SHA256SUMS-$S")" = "someone else's" ] && [ "$(cat "$FAKE_REL/SHA256SUMS")" = "previous sums" ] \
  && [ -e "$FAKE_REL/smol-c3-2026-10-01-g1111111aaaa.bin" ] && ! grep -q -- "-X DELETE\|release upload" "$FAKE_REL.log" \
  && ok "an existing manifest with other content: exit 1 before ANY mutation (not even retention)" || bad "existing manifest: exit $rc"
stage "$W/s"; fresh_release; cp "$FAKE_REL/SHA256SUMS-2026-11-10-g3333333cccc" "$FAKE_REL/SHA256SUMS-$S"; before="$(snapshot)"
FAKE_NODIGEST="SHA256SUMS-$S" "$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
[ "$rc" = 1 ] && [ "$before" = "$(snapshot)" ] && grep -q "digest cannot be read" "$W/out" \
  && ok "a manifest present without a readable digest is refused, not taken for absent" || bad "no-digest manifest: exit $rc"
stage "$W/s"; fresh_release; before="$(snapshot)"
FAKE_LIST_FAIL=1 "$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
[ "$rc" = 1 ] && [ "$before" = "$(snapshot)" ] && ! grep -q -- "-X DELETE\|release upload" "$FAKE_REL.log" \
  && ok "a failing asset listing stops the run before any mutation (exit 1)" || bad "listing failure: exit $rc"

# 5. capacity: too full even after pruning -> exit 1 before any upload
stage "$W/s"; fresh_release
NIGHTLY_CAP=8 "$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
[ "$rc" = 1 ] && ! grep -q "release upload" "$FAKE_REL.log" && grep -q "cap is 8" "$W/out" \
  && ok "a release too full for the set refuses before uploading (exit 1)" || bad "capacity: exit $rc; $(tail -1 "$W/out")"

# 6. the prune cap deletes whole sets only, and never the committed one
stage "$W/s"; fresh_release
NIGHTLY_PRUNE_MAX=4 "$PUB" "$W/s" >"$W/out" 2>&1
a=$(ls "$FAKE_REL" | grep -c 2026-10-01); b=$(ls "$FAKE_REL" | grep -c 2026-10-02)
{ [ "$a" = 0 ] && [ "$b" = 3 ]; } && ok "prune cap 4: one whole 3-asset set deleted, the next kept whole" || bad "prune cap: 10-01 left $a, 10-02 left $b"
stage "$W/s"; fresh_release; rm -f "$FAKE_REL"/*2026-11-10*
cp "$FAKE_REL/SHA256SUMS-2026-10-02-g2222222bbbb" "$FAKE_REL/SHA256SUMS"   # plain SHA256SUMS copies 10-02
"$PUB" "$W/s" >/dev/null 2>&1 || true
# after the run the committed set is this run's; the PREVIOUS committed (latest manifest before the
# run: 10-02) must have survived the prune that ran before the commit
[ -e "$FAKE_REL/smol-c3-2026-10-02-g2222222bbbb.bin" ] && ok "the set SHA256SUMS named at prune time is never pruned" \
  || bad "the previously committed set was pruned"

# 8. a committed set is immutable: a rerun with the same bytes verifies and uploads no image; a rerun
#    with other bytes under the same date+hash is refused before any upload
stage "$W/s"; fresh_release; "$PUB" "$W/s" >/dev/null 2>&1; : > "$FAKE_REL.log"
"$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
[ "$rc" = 0 ] && ! grep -q "release upload.*\.bin" "$FAKE_REL.log" && grep -q "already committed" "$W/out" \
  && ok "a rerun of a committed set verifies and re-uploads no image" || bad "idempotent rerun: exit $rc"
printf 'different bytes\n' > "$W/s/s3/smol-s3-cyd-$S.bin"; before="$(snapshot)"; : > "$FAKE_REL.log"
"$PUB" "$W/s" >"$W/out" 2>&1; rc=$?
[ "$rc" = 1 ] && [ "$before" = "$(snapshot)" ] && ! grep -q "release upload" "$FAKE_REL.log" \
  && ok "other bytes under a committed date+hash: refused before any upload (exit 1)" || bad "conflicting rerun: exit $rc"

# 9. pruning deletes a set's manifest BEFORE its images
stage "$W/s"; fresh_release; "$PUB" "$W/s" >/dev/null 2>&1
m=$(grep -n "DELETE fake://SHA256SUMS-2026-10-01" "$FAKE_REL.log" | cut -d: -f1); i=$(grep -n "DELETE fake://smol-c3-2026-10-01" "$FAKE_REL.log" | head -1 | cut -d: -f1)
[ -n "$m" ] && [ -n "$i" ] && [ "$m" -lt "$i" ] && ok "prune: the manifest is deleted before the set's images" || bad "prune order: manifest@$m image@$i"

# 7. the real manifests
real="$(bash "$ROOT/tools/release_targets.sh" --expected)"
if [ -n "$real" ] && ! grep -qv '^smol-[a-z0-9-]*$' <<<"$real"; then ok "release_targets.sh --expected on the real manifests: $(tr '\n' ' ' <<<"$real")"
else bad "release_targets.sh --expected: '$real'"; fi

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
