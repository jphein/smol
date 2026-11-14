#!/usr/bin/env bash
# publish_nightly.sh — publish ONE complete nightly set to the `nightly` prerelease, or commit nothing
# (#574).
#
# WHY. The three build jobs used to each `gh release upload nightly out/* --clobber` on their own and a
# separate job checksummed whatever .bin files the release held. So a cancelled or failed job left a
# MIXTURE of old and new public assets (2026-11-13, run 43415382132: c3 + xtensa published while
# c6-watch failed; that day ended with 16 assets dated 2026-11-13 where a complete night is 10), and
# SHA256SUMS covered all of it. Assets are date+hash named, so --clobber never replaced anything either:
# the release had grown to 817 assets, against GitHub's 1000-per-release cap.
#
# WHAT IS AUTHORITATIVE. GitHub has no atomic multi-asset swap on a release, so images can be visible
# before the set is committed. The rule is therefore: a set is published when its own manifest
# `SHA256SUMS-<date>-g<hash>` exists. That manifest is written only after every image of the set is on
# the release and its digest matches, under a name nothing ever overwrites. Plain `SHA256SUMS` is a
# convenience copy of the latest manifest, replaced last; for a moment between delete and upload it
# can be absent, never wrong. docs/RELEASES.md states the same.
#
# STEPS, in this order:
#   1. completeness, before the release is touched: the staged images are EXACTLY the set
#      `release_targets.sh --expected` derives from the manifests (no missing, no extra), each with its
#      NOTES, all of one run (one date, one hash), no name in two staged sets;
#   2. capacity and retention: delete whole sets (every asset of a -<date>-g<hash>) dated more than
#      NIGHTLY_KEEP_DAYS (14) before today, oldest first, never this run's set nor the set plain
#      SHA256SUMS copies (matched by digest), its manifest FIRST and confirmed gone before its images,
#      at most NIGHTLY_PRUNE_MAX (200) assets per run; then refuse if this set still would not fit
#      under the cap. Pruning runs BEFORE uploading, so a full release can always recover;
#   3. upload this run's images and notes, unless this exact set is already committed (then verify only;
#      a committed set with other content is refused before any mutation);
#   4. verify every one is on the release in state "uploaded" with OUR sha256 digest, else exit 1;
#   5. commit: upload SHA256SUMS-<date>-g<hash> (never clobbered: an existing one must match), verify
#      its digest, then replace plain SHA256SUMS.
#
# Usage: tools/publish_nightly.sh <staged-dir>   (each artifact set in its own subdirectory)
# Env: GH_REPO (owner/repo, required), GH (default gh), NIGHTLY_TAG (nightly), NIGHTLY_KEEP_DAYS (14),
#      NIGHTLY_PRUNE_MAX (200), NIGHTLY_CAP (1000), TODAY (UTC date; tests), EXPECTED_CMD (tests)
# Exit: 0 committed · 1 publish/verify/capacity failed (nothing committed) · 2 incomplete set or bad
#       usage (release untouched).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAGED="${1:?usage: publish_nightly.sh <staged-dir>}"
GH="${GH:-gh}"; TAG="${NIGHTLY_TAG:-nightly}"; KEEP="${NIGHTLY_KEEP_DAYS:-14}"
PMAX="${NIGHTLY_PRUNE_MAX:-200}"; CAP="${NIGHTLY_CAP:-1000}"; TODAY="${TODAY:-$(date -u +%F)}"
REPO="${GH_REPO:?publish_nightly: GH_REPO (owner/repo) is required}"
EXPECTED_CMD="${EXPECTED_CMD:-bash $ROOT/tools/release_targets.sh --expected}"
WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/var/tmp}}/publish-nightly.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
DIST="$WORK/dist"; mkdir -p "$DIST"
refuse() { echo "publish_nightly: REFUSING — $* (release untouched)" >&2; exit 2; }
abort()  { echo "publish_nightly: $* — nothing committed; the last committed set stands" >&2; exit 1; }
SUFFIX_RE='-([0-9]{4}-[0-9]{2}-[0-9]{2})-g([0-9a-f]{7,})\.bin(\.NOTES\.md)?$'

# ── 1. completeness ────────────────────────────────────────────────────────────────────────────────
sets=0
for d in "$STAGED"/*/; do
  [ -d "$d" ] || continue
  sets=$((sets+1))
  compgen -G "$d*.bin" >/dev/null || refuse "staged set $(basename "$d") has no .bin"
  for f in "$d"*; do
    n="$(basename "$f")"
    [ -e "$DIST/$n" ] && refuse "$n is in two staged sets"
    [[ "$n" =~ $SUFFIX_RE ]] || refuse "unexpected file $n in $(basename "$d")"
    case "$n" in
      *.bin) [ -f "$f.NOTES.md" ] || refuse "$n has no $n.NOTES.md" ;;
      *.bin.NOTES.md) [ -f "${f%.NOTES.md}" ] || refuse "$n has no .bin" ;;
    esac
    cp "$f" "$DIST/$n"
  done
done
[ "$sets" -gt 0 ] || refuse "no staged sets under $STAGED"
mapfile -t BINS < <(cd "$DIST" && ls -1 *.bin | sort)
stamps="$(printf '%s\n' "${BINS[@]}" | sed -E "s/.*$SUFFIX_RE/\\1-g\\2/" | sort -u)"
[ "$(wc -l <<<"$stamps")" = 1 ] || refuse "images from more than one run: $(tr '\n' ' ' <<<"$stamps")"
STAMP="$stamps"                                                  # <date>-g<hash> of this run
got="$(printf '%s\n' "${BINS[@]}" | sed -E "s/$SUFFIX_RE//" | sort)"
want="$($EXPECTED_CMD | sort)"
[ -n "$want" ] || refuse "release_targets.sh --expected listed nothing"
if [ "$got" != "$want" ]; then
  refuse "staged images are not the expected set (missing: $(comm -13 <(echo "$got") <(echo "$want") | tr '\n' ' ')extra: $(comm -23 <(echo "$got") <(echo "$want") | tr '\n' ' '))"
fi
MANIFEST="SHA256SUMS-$STAMP"
( cd "$DIST" && sha256sum "${BINS[@]}" > "$MANIFEST" )
echo "publish_nightly: complete set $STAMP — ${#BINS[@]} image(s), $sets staged set(s)"

# ── the release and its assets (REST: digest + state, paginated past 100) ─────────────────────────
if ! RID="$("$GH" api "repos/$REPO/releases/tags/$TAG" --jq .id 2>/dev/null)"; then
  "$GH" release create "$TAG" --repo "$REPO" --prerelease --title "nightly per-target images" \
    --notes "Rolling nightly builds - see each artifact's NOTES.md. A set is published when its SHA256SUMS-<date>-g<hash> exists (SHA256SUMS is a copy of the latest). New-hardware flashing only; fleet boards update over mesh OTA."
  RID="$("$GH" api "repos/$REPO/releases/tags/$TAG" --jq .id)"
fi
assets() { "$GH" api --paginate "repos/$REPO/releases/$RID/assets?per_page=100" \
             --jq '.[] | [.name, (.digest // ""), .state, .url] | @tsv'; }

# ── 2. retention (whole sets, before uploading) and capacity ──────────────────────────────────────
listing="$(assets)"
# The set plain SHA256SUMS currently copies: the per-run manifest with the SAME digest (xreview r2: not
# the newest name, which differs when a run committed its manifest but not the copy). A legacy plain
# SHA256SUMS that matches no per-run manifest (written before #574, covering every .bin) protects
# nothing in particular; this run replaces it.
plain="$(awk -F'\t' '$1=="SHA256SUMS" {print $2}' <<<"$listing")"
committed=""
if [ -n "$plain" ]; then
  committed="$(awk -F'\t' -v d="$plain" '$1 ~ /^SHA256SUMS-/ && $2==d {sub(/^SHA256SUMS-/, "", $1); print $1}' <<<"$listing" | head -1)"
fi
cutoff="$(date -u -d "$TODAY - $KEEP days" +%F)"
pruned=0
while read -r set_stamp; do
  [ -n "$set_stamp" ] || continue
  set_date="${set_stamp%%-g*}"
  date -u -d "$set_date" +%F >/dev/null 2>&1 || continue       # not a real date: not ours to judge
  [[ "$set_date" < "$cutoff" ]] || continue
  [ "$set_stamp" = "$STAMP" ] || [ "$set_stamp" = "$committed" ] && continue
  mapfile -t urls < <(awk -F'\t' -v s="$set_stamp" '$1 ~ ("-" s "\\.bin(\\.NOTES\\.md)?$") {print $4}' <<<"$listing")
  murl="$(awk -F'\t' -v n="SHA256SUMS-$set_stamp" '$1==n {print $4}' <<<"$listing")"
  [ $((pruned + ${#urls[@]} + (${#murl} > 0))) -le "$PMAX" ] || break   # whole sets only, under the cap
  # The manifest goes FIRST and its absence is confirmed before any image goes (xreview r2): an
  # interruption then leaves an unpublished leftover, never a manifest naming deleted images.
  if [ -n "$murl" ]; then
    "$GH" api -X DELETE "$murl" >/dev/null
    awk -F'\t' -v n="SHA256SUMS-$set_stamp" '$1==n' <<<"$(assets)" | grep -q . \
      && abort "SHA256SUMS-$set_stamp is still on the release after its delete; its images are kept"
    pruned=$((pruned + 1))
  fi
  for u in "${urls[@]}"; do "$GH" api -X DELETE "$u" >/dev/null; done
  pruned=$((pruned + ${#urls[@]}))
done < <(awk -F'\t' '{print $1}' <<<"$listing" | sed -nE "s/.*$SUFFIX_RE/\\1-g\\2/p" | sort -u)
echo "publish_nightly: pruned $pruned asset(s) of sets dated before $cutoff (keep $KEEP days, cap $PMAX/run)"
listing="$(assets)"
need=$(( $(ls "$DIST" | wc -l) ))                                # images + notes + this run's manifest
have=$(grep -c . <<<"$listing" || true)
already=$(for f in "$DIST"/*; do awk -F'\t' -v n="$(basename "$f")" '$1==n' <<<"$listing"; done | grep -c . || true)
[ $((have - already + need + 1)) -le "$CAP" ] || abort "the release holds $have assets; this set needs $((need - already + 1)) more and the cap is $CAP"

# ── 3. upload this run's images and notes — unless this exact set is already committed ─────────────
# A committed set is immutable (xreview r2): if its manifest exists, nothing of it is re-uploaded. Same
# manifest content -> verify the committed assets are still ours and finish; different -> refuse
# before any mutation. Only a set with NO manifest (an unfinished run) is ever clobbered.
want_m="sha256:$(sha256sum < "$DIST/$MANIFEST" | cut -d' ' -f1)"
existing="$(awk -F'\t' -v n="$MANIFEST" '$1==n {print $2}' <<<"$(assets)")"
if [ -n "$existing" ] && [ "$existing" != "$want_m" ]; then
  abort "$MANIFEST already exists with different content (a committed set is never overwritten)"
fi
if [ -z "$existing" ]; then
  ( cd "$DIST" && "$GH" release upload "$TAG" --repo "$REPO" *.bin *.bin.NOTES.md --clobber )
else
  echo "publish_nightly: $MANIFEST is already committed with this content; verifying, not re-uploading"
fi

# ── 4. verify: state "uploaded" and OUR digest, for every image and note ───────────────────────────
verify() { # $@ local files -> 0 when each is on the release, uploaded, with our sha256
  local listing f n want got bad=0; listing="$(assets)"
  for f in "$@"; do
    n="$(basename "$f")"; want="sha256:$(sha256sum < "$f" | cut -d' ' -f1)"
    got="$(awk -F'\t' -v n="$n" '$1==n && $3=="uploaded" {print $2}' <<<"$listing")"
    if [ "$got" != "$want" ]; then echo "  NOT PUBLISHED: $n (release has '${got:-none}')" >&2; bad=1; fi
  done
  return "$bad"
}
verify "$DIST"/*.bin "$DIST"/*.bin.NOTES.md || abort "an image or note is not on the release with our digest"

# ── 5. commit: the immutable per-run manifest, then the convenience copy ──────────────────────────
if [ -z "$existing" ]; then
  ( cd "$DIST" && "$GH" release upload "$TAG" --repo "$REPO" "$MANIFEST" )
fi
verify "$DIST/$MANIFEST" || abort "$MANIFEST is not on the release with our digest"
cp "$DIST/$MANIFEST" "$DIST/SHA256SUMS"
( cd "$DIST" && "$GH" release upload "$TAG" --repo "$REPO" SHA256SUMS --clobber )
echo "publish_nightly: COMMITTED $MANIFEST (${#BINS[@]} image(s)); SHA256SUMS now copies it"
