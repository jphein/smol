#!/usr/bin/env bash
# publish_nightly.sh — publish ONE complete nightly set to the `nightly` prerelease, or nothing (#574).
#
# WHY. The three build jobs used to each `gh release upload nightly out/* --clobber` on their own and
# a separate job checksummed whatever .bin files the release held. So a cancelled or failed job left
# a MIXTURE of old and new public assets (2026-11-13, run 43415382132: c3 + xtensa published while
# c6-watch failed; that day ended with 16 assets dated 2026-11-13 where a complete night is 10), and
# SHA256SUMS covered all of it. Separately, assets are date+hash named, so --clobber never replaced
# anything: the release had grown to 817 assets, against GitHub's 1000-per-release cap.
#
# NOW. The build jobs only STAGE (actions/upload-artifact); release-targets.yml's one publish job
# runs only when every build job succeeded, downloads the staged sets and runs this:
#   1. refuse an incomplete set BEFORE touching the release: a staged set that is empty, a .bin
#      without its .NOTES.md, or one file name in two sets (the merge would silently keep one);
#   2. upload this run's .bin + .NOTES.md;
#   3. VERIFY every one is on the release at the size we hold, else exit 1;
#   4. only then upload SHA256SUMS (this run's .bin files only). SHA256SUMS is the COMMIT POINT: a
#      failure at 2 or 3 leaves it describing the previous complete set, never a mixture;
#   5. prune date-stamped assets older than NIGHTLY_KEEP_DAYS (default 14), never this run's files, at
#      most NIGHTLY_PRUNE_MAX (default 200) per run so the 817-asset backlog drains over a few nights
#      inside the API rate limit rather than in one burst.
# GitHub has no atomic multi-asset swap; this is as close as one release allows, and it says so.
#
# Usage: tools/publish_nightly.sh <staged-dir>   (each artifact set in its own subdirectory)
# Env: GH (default gh), NIGHTLY_TAG (nightly), NIGHTLY_KEEP_DAYS (14), NIGHTLY_PRUNE_MAX (200), TODAY (tests)
# Exit: 0 published · 1 publish/verify failed · 2 incomplete set or bad usage (nothing touched).
set -euo pipefail
STAGED="${1:?usage: publish_nightly.sh <staged-dir>}"
GH="${GH:-gh}"; TAG="${NIGHTLY_TAG:-nightly}"; KEEP="${NIGHTLY_KEEP_DAYS:-14}"; PMAX="${NIGHTLY_PRUNE_MAX:-200}"
TODAY="${TODAY:-$(date -u +%F)}"
WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/var/tmp}}/publish-nightly.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
DIST="$WORK/dist"; mkdir -p "$DIST"
refuse() { echo "publish_nightly: REFUSING — $* (nothing was published)" >&2; exit 2; }

# 1. completeness, before the release is touched
sets=0
for d in "$STAGED"/*/; do
  [ -d "$d" ] || continue
  sets=$((sets+1))
  compgen -G "$d*.bin" >/dev/null || refuse "staged set $(basename "$d") has no .bin"
  for f in "$d"*; do
    n="$(basename "$f")"
    [ -e "$DIST/$n" ] && refuse "$n is in two staged sets"
    case "$n" in
      *.bin) [ -f "$f.NOTES.md" ] || refuse "$n has no $n.NOTES.md" ;;
      *.bin.NOTES.md) [ -f "${f%.NOTES.md}" ] || refuse "$n has no .bin" ;;
      *) refuse "unexpected file $n in $(basename "$d")" ;;
    esac
    cp "$f" "$DIST/$n"
  done
done
[ "$sets" -gt 0 ] || refuse "no staged sets under $STAGED"
mapfile -t BINS < <(cd "$DIST" && ls -1 *.bin | sort)
echo "publish_nightly: $sets staged set(s), ${#BINS[@]} image(s)"

# 2. upload this run's images and notes
"$GH" release view "$TAG" >/dev/null 2>&1 || "$GH" release create "$TAG" --prerelease \
  --title "nightly per-target images" \
  --notes "Rolling nightly builds - see each artifact's NOTES.md. New-hardware flashing only; fleet boards update over mesh OTA. SHA256SUMS covers the latest complete set."
( cd "$DIST" && "$GH" release upload "$TAG" *.bin *.bin.NOTES.md --clobber )

# 3. verify: every file we hold is on the release at our size
assets() { "$GH" release view "$TAG" --json assets --jq '.assets[] | [.name, (.size|tostring), .apiUrl] | @tsv'; }
listing="$(assets)"
missing=0
for f in "$DIST"/*; do
  n="$(basename "$f")"; want="$(stat -c %s "$f")"
  got="$(awk -F'\t' -v n="$n" '$1==n {print $2}' <<<"$listing")"
  if [ "$got" != "$want" ]; then echo "  NOT PUBLISHED: $n (release has '${got:-none}', want $want)" >&2; missing=1; fi
done
[ "$missing" -eq 0 ] || { echo "publish_nightly: upload incomplete; SHA256SUMS left on the previous complete set" >&2; exit 1; }

# 4. the commit point: SHA256SUMS over THIS run's images only
( cd "$DIST" && sha256sum "${BINS[@]}" > SHA256SUMS && "$GH" release upload "$TAG" SHA256SUMS --clobber )
echo "publish_nightly: SHA256SUMS now covers ${#BINS[@]} image(s) of $TODAY"

# 5. retention: date-stamped assets older than KEEP days, never this run's
cutoff="$(date -u -d "$TODAY - $KEEP days" +%F)"
pruned=0
while IFS=$'\t' read -r name _size url; do
  [ -n "$name" ] || continue
  [ -e "$DIST/$name" ] && continue
  d="$(sed -n 's/.*-\([0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}\)-g[0-9a-f]\{7,\}\.bin\(\.NOTES\.md\)\{0,1\}$/\1/p' <<<"$name")"
  [ -n "$d" ] || continue                      # not a dated image (SHA256SUMS and anything else stays)
  [ "$pruned" -ge "$PMAX" ] && break
  if [[ "$d" < "$cutoff" ]]; then "$GH" api -X DELETE "$url" >/dev/null; pruned=$((pruned+1)); fi
done <<<"$(assets)"
echo "publish_nightly: pruned $pruned asset(s) dated before $cutoff (keep $KEEP days)"
