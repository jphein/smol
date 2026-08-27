#!/usr/bin/env python3
"""#438/#335: prove the stack-paint instrument is CONSERVATIVE against the image we ship.

Why this exists, and why #438's prose was not enough.

`stack-paint-lite` (#438) exists because the previous paint composition was a NEAR-MISS of the
shipped image: it hard-required `bard`, and #434 found that composition could not even boot. #438's
whole warrant — the sentence that made its measurements trustworthy — was:

    "its `.stack` is byte-identical to the fleet tier's (86,200 B), so the instrument costs zero
     DRAM and its peak is a peak for the shipped image rather than for a near-miss of it."

**That sentence was PROSE, and on 2026-08-27 it silently stopped being true.** STEP T (#335) grew
`.bss` on both tiers by different amounts, and the two regions came apart by 1,136 B. Nothing
announced it, because nothing was checking it. The peak measured on that instrument was about to
decide a ship gate at a ~1 KB margin.

WHAT REPLACES IT — and the replacement INVERTS the original claim, which is the point.

⚠️ **BYTE-IDENTITY IS NOW A FAILURE, NOT A PASS.** On 2026-08-27 a bench handoff shipped two paint
images that were byte-identical in region to the fleet tier, passed every other check — md5, region,
`stack_paint` symbol count, seed verification — and were **MUTE**. `ESP_LOG` is compile-time on this
tree: without `ESP_LOG=info` the sentinel is compiled IN and its report line is compiled OUT. The
board runs the instrument and says nothing; the soak returns no number.

So #438's "byte-identical, zero DRAM" warrant was never true of an image that can PERFORM the
measurement. It described the silent build. The instrument as it must actually be flashed costs
~1.1 KB of region, and a paint image that costs *nothing* is a paint image that reports nothing.

The three things worth asserting, in order of what they catch:

  1. FITNESS FOR PURPOSE — the paint ELF contains the report line the soak reads. This is the check
     that would have caught the silent handoff, and no other check on that list could have.
  2. CONSERVATISM — `paint_region <= fleet_region`

     `_stack_start` is pinned at the top of DRAM, so the boundary that moves is `_stack_end`, rising
     as `.bss` grows. A paint image with a SMALLER region runs with LESS room than the image we
     ship, and the instrument only ever ADDS stack use of its own, so `P_paint >=~ P_shipped` — the
     fail-safe direction. The forbidden direction: if the paint image had MORE room, a peak that
     fits during the soak could be one the shipped image cannot hold, and the gate would read green
     off a measurement taken under easier conditions than shipping.
  3. SANITY BAND — the delta sits inside `[MIN_DELTA, MAX_DELTA]`. The LOWER bound is the inverted
     claim: a zero delta means the instrument cost nothing, which means it is not really there. The
     UPPER bound catches a paint tier that has drifted into carrying something else entirely.

Checks 1 and 3's lower bound overlap deliberately — they catch the same defect by different means
(a missing string vs a missing cost), and after 2026-08-27 that redundancy is bought and paid for.

⚠️ THIS ARM NEEDS SHIPPED-GEOMETRY ELFs. Do NOT feed it the `excl` arm's binaries: those are built
with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, which gate.sh's own header records as CHANGING
the image (464,180 of 1,027,816 ALLOC bytes differ; `.text` grew 46 B). A region comparison between
two debug-geometry binaries answers a different question than the one asked here.

Fails CLOSED: a missing symbol, an unreadable ELF, or a nonsensical region exits 2 rather than
passing. An instrument-validity check that can quietly cover nothing is the exact defect it exists
to prevent — see #438 above.

Usage:
  check_paint_warrant.py --fleet-elf <path> --paint-elf <path>
  check_paint_warrant.py --fleet-syms <file> --paint-syms <file>   # pre-captured `readelf -sW`
Exit: 0 ok · 1 warrant violated · 2 malformed/blind
"""
import argparse
import re
import subprocess
import sys

# The two symbols the linker script defines around the stack region. Spelled here so a rename
# cannot silently empty this arm — it will fail closed instead.
START_SYM = "_stack_start"
END_SYM = "_stack_end"
# A region below this is not a plausible C3 stack; treat it as a broken read, not a tiny stack.
MIN_PLAUSIBLE_REGION = 4096
# The line the soak actually greps for. If this is absent the board is MUTE and the image is
# useless no matter how good every other number looks — see the docstring's 2026-08-27 incident.
REPORT_MARKER = b"#434 paint"
# Sanity band on `fleet_region - paint_region`. Measured on d2cf3aa: 1,136 B (fleet tier) and
# 1,088 B (espnow tier) with ESP_LOG=info. The bounds are deliberately loose — this is a band, not
# a pin, because the exact cost moves with the log level and with the image around it. What it
# refuses is a delta of ZERO (an instrument that costs nothing is not present) and a delta so large
# the paint tier must be carrying something other than the instrument.
MIN_DELTA = 1
MAX_DELTA = 8192


def die(code, msg, *extra):
    print(f"FATAL: {msg}", file=sys.stderr)
    for line in extra:
        print(f"  {line}", file=sys.stderr)
    return code


def read_syms(elf=None, syms_file=None):
    """Symbol text, or None meaning BLIND (caller must exit 2, never 1).

    ⚠️ An unreadable input must NOT share an exit code with a violated warrant. An earlier draft of
    this file let `open()` raise, which exited 1 — the same code as "the paint image is roomier than
    the shipped image". CI reading rc==1 would have reported a real, alarming, and entirely
    fictional finding whenever a path was wrong. A new way to fail needs its own name.
    """
    if syms_file:
        try:
            with open(syms_file, encoding="utf-8", errors="replace") as fh:
                return fh.read()
        except OSError as e:
            print(f"FATAL: cannot read {syms_file}: {e}", file=sys.stderr)
            return None
    try:
        r = subprocess.run(["readelf", "-sW", elf], capture_output=True, text=True)
    except OSError as e:
        print(f"FATAL: cannot run readelf: {e}", file=sys.stderr)
        return None
    if r.returncode != 0:
        print(f"FATAL: readelf failed on {elf}: {r.stderr.strip()[:200]}", file=sys.stderr)
        return None
    return r.stdout


def region_of(text, label):
    """(_stack_start - _stack_end) from `readelf -sW` output, or (None, reason)."""
    found = {}
    for line in text.splitlines():
        f = line.split()
        # readelf -sW: Num: Value Size Type Bind Vis Ndx Name
        if len(f) >= 8 and f[7] in (START_SYM, END_SYM):
            try:
                found[f[7]] = int(f[1], 16)
            except ValueError:
                return None, f"{label}: {f[7]} has an unparseable value {f[1]!r}"
    for s in (START_SYM, END_SYM):
        if s not in found:
            return None, (
                f"{label}: `{s}` is ABSENT from the symbol table. The linker script renamed it, or "
                "this is not a linked firmware ELF. This arm cannot compare regions it cannot find."
            )
    region = found[START_SYM] - found[END_SYM]
    if region < MIN_PLAUSIBLE_REGION:
        return None, (
            f"{label}: region computes to {region} B, below the {MIN_PLAUSIBLE_REGION} B plausibility "
            "floor. That is a broken read (symbols swapped, or a stripped/partial ELF), not a small "
            "stack — refusing to compare."
        )
    return region, None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--fleet-elf")
    ap.add_argument("--paint-elf")
    ap.add_argument("--fleet-syms", help="pre-captured `readelf -sW` output (test seam)")
    ap.add_argument("--paint-syms", help="pre-captured `readelf -sW` output (test seam)")
    a = ap.parse_args()

    if not (a.fleet_elf or a.fleet_syms) or not (a.paint_elf or a.paint_syms):
        return die(2, "both a fleet and a paint input are required (--*-elf or --*-syms).")

    fleet_txt = read_syms(a.fleet_elf, a.fleet_syms)
    paint_txt = read_syms(a.paint_elf, a.paint_syms)
    if fleet_txt is None:
        return die(2, f"could not read symbols from the fleet ELF ({a.fleet_elf}).")
    if paint_txt is None:
        return die(2, f"could not read symbols from the paint ELF ({a.paint_elf}).")

    fleet, err = region_of(fleet_txt, "fleet")
    if err:
        return die(2, err)
    paint, err = region_of(paint_txt, "paint")
    if err:
        return die(2, err)

    # ── check 1: FITNESS FOR PURPOSE ──────────────────────────────────────────────────────────
    # Only meaningful against a real ELF; with the --*-syms test seam there is no binary to scan.
    if a.paint_elf:
        try:
            with open(a.paint_elf, "rb") as fh:
                blob = fh.read()
        except OSError as e:
            return die(2, f"cannot read the paint ELF for the report-line check: {e}")
        if REPORT_MARKER not in blob:
            print("FATAL: the paint image is MUTE — it cannot produce a measurement.", file=sys.stderr)
            print(
                f"  `{REPORT_MARKER.decode()}` is absent from {a.paint_elf}.\n"
                "  `ESP_LOG` is COMPILE-TIME on this tree: the sentinel is compiled in and its\n"
                "  report line is compiled out, so the board runs the instrument and says nothing.\n"
                "  Rebuild with `ESP_LOG=info`. (2026-08-27: two handoff images shipped in exactly\n"
                "  this state and passed md5, region, symbol-count and seed checks — every number\n"
                "  looked right and the soak would have returned nothing.)",
                file=sys.stderr,
            )
            return 1

    # ── check 2: CONSERVATISM ─────────────────────────────────────────────────────────────────
    delta = fleet - paint
    if delta < 0:
        print("FATAL: the paint instrument has MORE stack room than the image we ship.", file=sys.stderr)
        print(
            f"  fleet region {fleet} B · paint region {paint} B · paint is {-delta} B LARGER.\n"
            "  A peak measured under easier conditions than shipping cannot bound the shipped\n"
            "  image: the soak could pass a depth the fleet image has no room for. Do NOT read a\n"
            "  peak off this pair.",
            file=sys.stderr,
        )
        return 1

    # ── check 3: SANITY BAND ──────────────────────────────────────────────────────────────────
    if delta < MIN_DELTA:
        print("FATAL: the paint instrument costs NOTHING, so it is not really there.", file=sys.stderr)
        print(
            f"  fleet region {fleet} B · paint region {paint} B · delta {delta} B.\n"
            "  ⚠️ Byte-identity USED to be #438's warrant. It is now a FAILURE: a paint build that\n"
            "  costs no region is the signature of a build with the reporting compiled out. The\n"
            "  instrument as it must actually be flashed costs ~1.1 KB.",
            file=sys.stderr,
        )
        return 1
    if delta > MAX_DELTA:
        print("FATAL: the paint tier costs far more region than the instrument explains.", file=sys.stderr)
        print(
            f"  fleet region {fleet} B · paint region {paint} B · delta {delta} B "
            f"(band {MIN_DELTA}..{MAX_DELTA}).\n"
            "  The paint tier is carrying something beyond the sentinel — check its feature set\n"
            "  before reading a peak off it; the measurement would describe a different program.",
            file=sys.stderr,
        )
        return 1

    print(
        f"  paint warrant: fleet {fleet} B · paint {paint} B — paint is {delta} B tighter "
        f"(conservative, in band), report line present"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
