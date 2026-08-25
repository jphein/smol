#!/usr/bin/env bash
# assert_cargo_config.sh — refuse to build a chip whose `.cargo/config.toml` is STALE. (#280)
#
# SOURCE IT for the function, or run it directly for one chip:
#
#   . tools/assert_cargo_config.sh && assert_cargo_config esp32s3     # in a script
#   tools/assert_cargo_config.sh esp32s3                              # by hand
#
# ── WHY ───────────────────────────────────────────────────────────────────────────────────────
# `rust/clock/.cargo/config.toml` is git-TRACKED, and `~/Projects/.stignore` ignores `.cargo`. So
# on a remote builder the file arrives OUT OF BAND — this repo's own instructions tell agents to
# "copy .cargo/config.toml if missing". That workaround keeps working just well enough to hide
# that nothing keeps it CURRENT: a manual copy satisfies "present" and never satisfies "correct".
#
# On 2026-08-25 familiar's copy was byte-current except for one hunk — the S3 arm's
# `-C link-arg=-Tlinkall.x`, added that morning by #398. The C3 `[build] target` was intact, so
# every C3 build was green and nothing suggested a problem. An S3 build there would have passed
# `cargo check` (check never links) and then failed the LINK with 129 undefined references —
# `_bss_start`, `__exception`, the whole vector table — which reads as a broken toolchain, not as
# a stale file. This turns that into one line, before the build.
#
# ── WHY MARKERS AND NOT A CHECKSUM ────────────────────────────────────────────────────────────
# A checksum needs ground truth from another host, and there is no authority to fetch it from on
# a builder that is deliberately offline-capable. The markers need only what the repo already
# declares: `tools/build-matrix.toml` states each chip's `target` and its `config_markers`, so
# the manifest that knows a chip exists is also the thing that knows what its build needs.
#
# ── WHY THE SECTION SCOPE IS THE WHOLE CHECK ──────────────────────────────────────────────────
# A whole-file `grep -q -- -Tlinkall.x` PASSES on the exact stale file that motivated this, because
# the two riscv arms carry the marker and only the xtensa arm was missing it. A file-wide check
# would have reported green on the file whose staleness costs 129 link errors. So markers are
# asserted INSIDE `[target.<triple>]`, and the section extractor stops at the next `[` heading.
#
# ── WHAT IT DOES NOT DO ───────────────────────────────────────────────────────────────────────
# It does not prove the file is current — only that it carries the markers this chip needs. A
# stale-but-marker-complete file still passes, and that is the honest limit: this narrows the
# window to "edits that add a new load-bearing key", which is the moment to add a marker for it.
# It is a fence around a known cliff, not a proof of freshness.
set -uo pipefail

_acc_root() { cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd; }

# assert_cargo_config <chip> [config-path]
# 0 = markers present · 1 = a marker is MISSING (stale) · 2 = cannot check
assert_cargo_config() {
    local chip="${1:-}" cfg="${2:-}" root matrix
    root="$(_acc_root)"
    matrix="$root/tools/build_matrix.py"
    cfg="${cfg:-$root/rust/clock/.cargo/config.toml}"

    [ -n "$chip" ] || { echo "assert_cargo_config: no chip given" >&2; return 2; }
    [ -x "$matrix" ] || { echo "assert_cargo_config: no $matrix" >&2; return 2; }
    if [ ! -f "$cfg" ]; then
        echo "REFUSED: $cfg is MISSING." >&2
        echo "    It is git-tracked but \`.stignore\` ignores \`.cargo\`, so a synced tree may not" >&2
        echo "    have it. Re-sync from the canonical tree (#280)." >&2
        return 1
    fi

    local markers rc=0
    markers="$("$matrix" config-markers --chip "$chip" 2>&1)" || {
        echo "assert_cargo_config: $markers" >&2; return 2; }
    # A chip with no declared markers is a MANIFEST gap, not a pass. Reporting green here would
    # make "nobody wrote the markers yet" indistinguishable from "the config is correct" — the
    # vacuous-pass shape this repo keeps paying for.
    [ -n "$markers" ] || {
        echo "assert_cargo_config: [chip.$chip] declares no config_markers in build-matrix.toml" >&2
        echo "    A chip with nothing to assert cannot be verified; add its markers (#280)." >&2
        return 2; }

    # `seen_target` is NOT `target`: the herestring below appends a trailing newline, so the final
    # `read` yields an empty line that `continue` skips — after having already blanked `target`.
    # The first draft's failure message therefore said "[target.]" with the triple missing, which
    # is the one piece of information the operator needs. Keep the last NON-EMPTY value.
    local target marker section seen_target="" missing=()
    while IFS=$'\t' read -r target marker; do
        [ -n "$target" ] || continue
        seen_target="$target"
        # The chip's own section only — see the header. `[target.X]` up to the next `[` heading.
        section="$(awk -v want="[target.$target]" '
            $0 ~ /^\[/ { inside = ($0 == want) ? 1 : 0 }
            inside { print }
        ' "$cfg")"
        if [ -z "$section" ]; then
            echo "REFUSED: $cfg has no [target.$target] section (chip $chip)." >&2
            echo "    stale out-of-band copy? re-sync from the canonical tree (#280)." >&2
            return 1
        fi
        # -F: markers are literals, not patterns (`-Tlinkall.x` contains a regex `.`).
        printf '%s' "$section" | grep -qF -- "$marker" || missing+=("$marker")
    done <<< "$markers"

    if [ ${#missing[@]} -gt 0 ]; then
        echo "REFUSED: $cfg lacks ${missing[*]} in [target.$seen_target] (chip $chip) — stale out-of-band copy?" >&2
        echo "    re-sync from the canonical tree (#280)." >&2
        echo "    Each missing marker is silent at \`cargo check\` and expensive later:" >&2
        echo "      -Tlinkall.x         check never links; \`ld\` then emits ~129 undefined refs" >&2
        echo "      force-frame-pointers  esp-backtrace cannot unwind a panic" >&2
        echo "      linker-flavor       the target link routes through a non-compiler \`cc\` shim" >&2
        rc=1
    fi
    return $rc
}

# Direct invocation: `tools/assert_cargo_config.sh <chip> [config]`.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    assert_cargo_config "$@" && echo "ok: .cargo/config.toml carries ${1:-?}'s declared markers"
fi
