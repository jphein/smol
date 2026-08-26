#!/usr/bin/env python3
"""check_symbol_sizes.py — #390: a per-tier baseline of the big writable statics.

WHAT THIS EXISTS FOR
--------------------
#335 P1.1 added six embassy dependency lines and changed NO source. `embassy-net` turned on
smoltcp's `async` feature; smoltcp is SHARED with the hand-driven gateway (one `v0.13.1` in the
graph), so `WakerRegistration` per socket grew `net::wifi::NTP_SOCK_STORAGE` from 0x540 to 0x580
— 64 B of `.data` taken straight out of the `.stack` region, and invisible to:

  * code review        — there was no source diff;
  * the compiler       — no arity or type change;
  * `repro_stack_check`— 0.06 % of an aggregate compared against a threshold;
  * #351's exclusion checker — it asserts PRESENCE/ABSENCE, never size.

It was found by a hand `nm` A/B, which is not a process. This file makes such a resize land as a
REVIEWED DIFF LINE in `tools/symbol-sizes.<tier>.txt` instead.

It GENERALISES the #300 stack floor rather than duplicating it. The floor watches one derived
aggregate (`.stack`) against a threshold; this watches the individual statics whose growth is what
MOVES that aggregate. The floor tells you the roof came down; this tells you which box grew.

WHY KEY ON THE MANGLED NAME, NOT A DEMANGLED ONE
------------------------------------------------
#390 proposed keying on "DEMANGLED hash-stripped names". Demangling is the right instinct — the raw
names carry a crate-metadata hash that churns between builds (`Cs9FWucCYWq3y_`), and it buries the
signal — but a DEMANGLER IS ITSELF A VERSION-DEPENDENT TRANSFORM. Keying on its output makes this
baseline depend on which `rustfilt`/`llvm-cxxfilt` happens to be installed, which is the same class
of churn the baseline exists to eliminate, just relocated into the toolchain. There is also no
demangler guaranteed present in CI.

So: key on the mangled symbol with the VOLATILE COMPONENTS SUBSTITUTED (not deleted — a placeholder
keeps the name readable and keeps two distinct symbols distinct). The key then depends only on the
ELF. Verified on the real canonical ELF: 29 tracked statics, ZERO normalization collisions.

WHAT IS SKIPPED, AND WHY IT IS BROADER THAN THE ISSUE SAID
----------------------------------------------------------
#390 named `.L_MergedGlobals*` and `.Lswitch.table.*`. The real ELF also carries
`.Lanon.<32-hex>.<n>` — CONTENT-ADDRESSED names that move whenever the constant they hold moves.
Enumerating the three patterns would leave the next one to be discovered the same way. So the rule
is the general one: `.L` is the assembler's local-label prefix, so ANY `.L*` symbol is
compiler-generated and not a static anybody declared. 6 of 35 candidates on the canonical ELF.

Usage:
    check_symbol_sizes.py --tier <name> --elf <path>            # check against the baseline
    check_symbol_sizes.py --tier <name> --elf <path> --bless    # rewrite the baseline
    check_symbol_sizes.py --tier <name> --sections F --symbols G # test seam: pre-captured readelf

Exit: 0 = matches baseline · 1 = DRIFT (or a real failure) · 2 = COULD NOT CHECK.
`2` is distinct on purpose: "the check did not run" must never be reported as "the check passed".
"""

import argparse
import os
import re
import subprocess
import sys

# Symbols at or above this many bytes. 256 B is #390's threshold: below it the population is
# dominated by small per-function statics whose churn would make the baseline unreviewable, which
# is the failure mode that makes a gate get deleted.
DEFAULT_THRESHOLD = 256

# A floor on how many statics we expect to find. This is the anti-vacuous-pass guard: if a readelf
# format change, a section-name change, or a bad filter silently matches nothing, the honest result
# is a hard failure, NOT "no drift detected". The canonical ELF yields 29; 10 leaves room for tier
# variation while still catching a filter that has stopped working.
MIN_SYMBOLS = 10

SECTION_RE = re.compile(r"^\s*\[\s*(\d+)\]\s+(\S+)")
# The v0 crate disambiguator (`Cs9FWucCYWq3y_`) and the legacy-mangling hash suffix
# (`17h0123456789abcdefE`). Substituted with a placeholder rather than deleted.
VOLATILE = [
    (re.compile(r"Cs[0-9A-Za-z]{8,}_"), "Cs*_"),
    (re.compile(r"17h[0-9a-f]{16}E"), "17h*E"),
]
# Writable static storage. Prefix-matched so `.data.wifi` counts; `.rtc_fast.data` deliberately
# does NOT (it is a different memory, currently zero-sized, and would need its own budget story).
TRACKED_SECTION_RE = re.compile(r"^\.(data|bss)\b")


def normalize(name):
    for pat, repl in VOLATILE:
        name = pat.sub(repl, name)
    return name


def run_readelf(elf, flag):
    exe = os.environ.get("READELF", "readelf")
    try:
        p = subprocess.run([exe, flag, elf], capture_output=True, text=True)
    except FileNotFoundError:
        die2("`%s` not found. This check reads the ELF's symbol table; without it the check "
             "cannot run, which is not the same as passing." % exe)
    if p.returncode != 0:
        die2("%s %s %s failed (rc=%d):\n%s" % (exe, flag, elf, p.returncode, p.stderr.strip()))
    return p.stdout


def die2(msg):
    sys.stderr.write("CANNOT CHECK: %s\n" % msg)
    sys.exit(2)


def parse_sections(text):
    out = {}
    for line in text.splitlines():
        m = SECTION_RE.match(line)
        if m:
            out[m.group(1)] = m.group(2)
    if not out:
        die2("parsed ZERO sections from `readelf -SW`. The output format is not what this "
             "checker expects, so every symbol would fall outside .data/.bss and the run would "
             "vacuously report no drift.")
    return out


def parse_symbols(text, sections, threshold):
    rows = {}
    skipped_local = 0
    saw_object = 0
    for line in text.splitlines():
        parts = line.split()
        # Num: Value Size Type Bind Vis Ndx Name
        if len(parts) < 8 or parts[3] != "OBJECT":
            continue
        saw_object += 1
        try:
            size = int(parts[2])
        except ValueError:
            continue  # readelf prints hex for huge sizes; none of ours are that large
        section = sections.get(parts[6])
        if section is None or not TRACKED_SECTION_RE.match(section):
            continue
        if size < threshold:
            continue
        name = parts[7]
        if name.startswith(".L"):
            skipped_local += 1
            continue
        rows[normalize(name)] = (size, section)
    if saw_object == 0:
        die2("found ZERO symbols of type OBJECT. Either the ELF is stripped or the symbol-table "
             "format changed; either way this check did not run.")
    return rows, skipped_local


def load_baseline(path):
    rows = {}
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != 3:
                die2("malformed baseline line in %s: %r (want '<size>\\t<section>\\t<name>')"
                     % (path, line))
            rows[parts[2]] = (int(parts[0]), parts[1])
    return rows


def write_baseline(path, tier, rows):
    total = sum(s for s, _ in rows.values())
    with open(path, "w") as fh:
        fh.write("# tools/symbol-sizes.%s.txt — #390 writable-static size baseline.\n" % tier)
        fh.write("# GENERATED: tools/check_symbol_sizes.py --tier %s --elf <elf> --bless\n" % tier)
        fh.write("#\n")
        fh.write("# Every line is a static of >=%d B in .data*/.bss*. A dependency bump that\n"
                 % DEFAULT_THRESHOLD)
        fh.write("# resizes one of these shows up HERE as a diff line, which is the whole point:\n")
        fh.write("# the #390 instance (NTP_SOCK_STORAGE 1344->1408 via a smoltcp feature) had no\n")
        fh.write("# source diff and no compiler complaint, so review had nothing to look at.\n")
        fh.write("#\n")
        fh.write("# Do NOT hand-edit. Re-bless, and explain the delta in the commit message.\n")
        fh.write("# %d symbols, %d bytes total.\n" % (len(rows), total))
        fh.write("#\n")
        fh.write("# size\tsection\tsymbol\n")
        for name in sorted(rows):
            size, section = rows[name]
            fh.write("%d\t%s\t%s\n" % (size, section, name))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tier", required=True)
    ap.add_argument("--elf")
    ap.add_argument("--sections", help="pre-captured `readelf -SW` output (test seam)")
    ap.add_argument("--symbols", help="pre-captured `readelf -sW` output (test seam)")
    ap.add_argument("--baseline-dir", default=os.path.dirname(os.path.abspath(__file__)))
    ap.add_argument("--threshold", type=int, default=DEFAULT_THRESHOLD)
    ap.add_argument("--bless", action="store_true")
    args = ap.parse_args()

    if args.sections and args.symbols:
        sec_text = open(args.sections).read()
        sym_text = open(args.symbols).read()
    elif args.elf:
        if not os.path.exists(args.elf):
            die2("no ELF at %s" % args.elf)
        sec_text = run_readelf(args.elf, "-SW")
        sym_text = run_readelf(args.elf, "-sW")
    else:
        die2("need --elf, or both --sections and --symbols")

    sections = parse_sections(sec_text)
    current, skipped = parse_symbols(sym_text, sections, args.threshold)

    if len(current) < MIN_SYMBOLS:
        die2("only %d tracked statics found (floor is %d). A working filter on this firmware "
             "finds ~29. Refusing to compare — or to bless — a set this small, because an empty "
             "or near-empty set makes every future run pass while guarding nothing."
             % (len(current), MIN_SYMBOLS))

    path = os.path.join(args.baseline_dir, "symbol-sizes.%s.txt" % args.tier)

    if args.bless:
        write_baseline(path, args.tier, current)
        print("blessed %s: %d symbols, %d bytes (skipped %d .L* compiler-generated)"
              % (os.path.basename(path), len(current),
                 sum(s for s, _ in current.values()), skipped))
        return 0

    if not os.path.exists(path):
        die2("no baseline at %s. Create it with --bless and COMMIT it; a missing baseline is a "
             "gap to close, not a pass." % path)

    base = load_baseline(path)
    grew, shrank, new, gone = [], [], [], []
    for name, (size, section) in sorted(current.items()):
        if name not in base:
            new.append((name, size, section))
        elif base[name][0] != size:
            (grew if size > base[name][0] else shrank).append((name, base[name][0], size))
    for name, (size, section) in sorted(base.items()):
        if name not in current:
            gone.append((name, size, section))

    if not (grew or shrank or new or gone):
        print("symbol sizes: %d statics, %d bytes — matches %s"
              % (len(current), sum(s for s, _ in current.values()), os.path.basename(path)))
        return 0

    delta = sum(s for s, _ in current.values()) - sum(s for s, _ in base.values())
    sys.stderr.write("SYMBOL SIZE DRIFT (#390) vs %s — net %+d B\n" % (os.path.basename(path), delta))
    for name, was, now in grew:
        sys.stderr.write("  GREW   %+7d B  %-8d -> %-8d  %s\n" % (now - was, was, now, name))
    for name, was, now in shrank:
        sys.stderr.write("  SHRANK %+7d B  %-8d -> %-8d  %s\n" % (now - was, was, now, name))
    for name, size, section in new:
        sys.stderr.write("  NEW    %+7d B  %-19s %s (%s)\n" % (size, "", name, section))
    for name, size, section in gone:
        sys.stderr.write("  GONE   %+7d B  %-19s %s (%s)\n" % (-size, "", name, section))
    sys.stderr.write(
        "\nThis is not automatically a problem — it is a change that was previously invisible.\n"
        "If it is intended, re-bless and say WHY in the commit message:\n"
        "  tools/check_symbol_sizes.py --tier %s --elf <elf> --bless\n"
        "Watch the .stack region: .data/.bss growth is taken out of it (#300 floor).\n" % args.tier)
    return 1


if __name__ == "__main__":
    sys.exit(main())
