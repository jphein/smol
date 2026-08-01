#!/usr/bin/env python3
"""#351 — make the "BYTE-FREE" tier claims checkable instead of aspirational.

The source asserts, in a dozen places, that lower tiers are byte-free of higher-tier
features — "the default build is provably BYTE-FREE of it (#44)". Nothing verified that.
It is the same species as #339's shed order and #350's tier coverage: a true-sounding
sentence with no mechanism behind it, which is how it stops being true without anyone
noticing.

## ⚠️ THE TWO ARMS ARE NOT REDUNDANT. DO NOT DELETE ONE.

A future reader will see two checks of "the same thing" and remove one. They are different
claims and only the first is load-bearing:

  (a) SOURCE STRUCTURE — the PROOF.
      A module behind `#[cfg(feature = "F")]` is not compiled at all in a build without F,
      and any reference to it from outside a matching cfg is a COMPILE ERROR. `tools/gate.sh`
      already builds every tier, so the compiler proves the absence; this arm proves the
      GATE EXISTS and that every byte-free claim has one behind it. Zero build cost,
      LTO-proof, and it reasons about the shipped configuration.

  (b) SYMBOL TABLE — CORROBORATION, and UNSOUND ALONE.
      Reads the real shipped ELF and looks for symbols naming an excluded module. It cannot
      be the proof: with `lto = "fat"` and `codegen-units = 1`, a genuinely-linked module can
      be inlined until not one symbol survives, so ABSENCE OF A SYMBOL IS NOT ABSENCE OF
      CODE. It is here to catch a leak path (a) did not anticipate — a false NEGATIVE it can
      find, a false POSITIVE it cannot rule out.

If you only keep one, keep (a). If (b) ever fails, believe it — it has no false-alarm mode
that matters, since a symbol naming the module means the module's name reached the linker.

## Why not DWARF, which is what #351 originally proposed

Measured 2026-08-01, and recorded so nobody re-derives it:

  * The release ELF has NO debug info — `[profile.release]` sets no `debug`, so cargo's
    default (off) applies: 0 debug sections, 0 `DW_TAG_compile_unit`. Nothing to enumerate;
    a CU-based checker would have passed vacuously.
  * Turning debug on yields 202 CUs, but `codegen-units = 1` puts the ENTIRE `clock` crate in
    ONE CU (`src/main.rs/@/clock.<hash>-cgu.0`); the rest are dependency crates. Every
    byte-free claim is about a module INSIDE that crate, so CU enumeration cannot see any of
    them. (The line program's file table can — that granularity does exist.)
  * Decisively: enabling debug info CHANGES THE SHIPPED IMAGE. n/d/n sandwich, same commit,
    same tree — nodebug `ebcc3ab79bc76555`, debug=1 `da2de3957ae5a279`, nodebug again
    `ebcc3ab79bc76555`. The two nodebug builds are byte-identical (so the tree held still and
    the build is reproducible), and the debug build differs by **267,807 bytes across ~170 of
    the image's 266 4 KB regions** — not a metadata field.

    A gate that runs on a debug build certifies a DIFFERENT BINARY than the one that ships.
    That is the defect this project spent a day removing (a stack floor measured at package
    time, a map file unusable under LTO), so it was rejected rather than adopted.

Exit codes: 0 ok · 1 a check failed · 2 malformed input
"""

from __future__ import annotations

import argparse
import re
import subprocess
import tomllib
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_SRC = HERE.parent / "rust" / "clock" / "src"

CLAIM = re.compile(r"byte[- ]free", re.I)
# A GATE is `#[cfg(...)]` / `#![cfg(...)]` — it decides whether the item exists.
# `#[cfg_attr(...)]` is NOT a gate: it conditionally applies an attribute to an item that
# exists either way, and reading one as a gate is how the first draft of this checker
# reported `wifi.rs` as gated on `espnow` (it has `#![cfg_attr(not(espnow), allow(dead_code))]`).
CFG_GATE = re.compile(r"^\s*#!?\[cfg\((.+)\)\]\s*$")
CFG_ANY = re.compile(r"^\s*#!?\[cfg(?:_attr)?\((.+)\)\]\s*$")
MOD_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
FEATURE = re.compile(r'feature\s*=\s*"([A-Za-z0-9_-]+)"')

# How far above a claim we will look for the gate that backs it. Claims sit in the doc block
# immediately above the item, and the block can be long; 12 lines covers every current site
# without reaching into an unrelated item above.
CLAIM_WINDOW = 12


def rs_files(src: Path):
    return sorted(p for p in src.rglob("*.rs") if p.is_file())


def file_gate(lines: list[str]) -> str | None:
    """The whole-file `#![cfg(...)]`, if any. Only meaningful in the file's header, before
    the first item, so we stop at the first non-attribute, non-comment, non-blank line."""
    for ln in lines:
        s = ln.strip()
        if not s or s.startswith("//") or s.startswith("#!["):
            m = CFG_GATE.match(ln)
            if m and ln.lstrip().startswith("#!["):
                return m.group(1).strip()
            continue
        return None
    return None


def mod_gates(lines: list[str]) -> dict[str, str | None]:
    """module name -> the cfg predicate on its `mod` declaration (None if ungated).

    Attributes stack, so we walk upward through a contiguous run of attributes and comments
    and take the first `cfg`. Anything else terminates the run — an intervening item means
    the attribute belongs to that item, not to this `mod`.
    """
    out: dict[str, str | None] = {}
    for i, ln in enumerate(lines):
        m = MOD_DECL.match(ln)
        if not m:
            continue
        gate = None
        j = i - 1
        while j >= 0:
            s = lines[j].strip()
            if not s or s.startswith("//"):
                j -= 1
                continue
            c = CFG_GATE.match(lines[j])
            if c and not lines[j].lstrip().startswith("#!["):
                gate = c.group(1).strip()
                break
            if s.startswith("#["):       # some other attribute — keep walking
                j -= 1
                continue
            break                         # an item or code: the run is over
        out[m.group(1)] = gate
    return out


# ── arm (a): source structure ────────────────────────────────────────────────


def check_source(src: Path) -> list[str]:
    fails: list[str] = []
    files = rs_files(src)
    gates_by_file = {}
    for p in files:
        lines = p.read_text(encoding="utf-8").splitlines()
        gates_by_file[p] = (lines, file_gate(lines), mod_gates(lines))

    # Every `mod` gate in the crate, by module name — a claim inside `net/cast.rs` is backed
    # by `#[cfg(feature = "cast")] pub mod cast;` in `net.rs`, i.e. by a gate in ANOTHER FILE.
    # The first draft only looked within the claim's own file and reported cast.rs as
    # unbacked, which was the checker being wrong about where the mechanism lives.
    gated_modules = {name for _p, (_l, _f, mg) in gates_by_file.items()
                     for name, g in mg.items() if g}

    # a1. Every byte-free CLAIM must have a mechanism behind it. Three shapes count, and they
    # are the three the codebase actually uses:
    #   * an item-level `#[cfg(...)]` near the claim   (main.rs's io / wifi statics)
    #   * a whole-file `#![cfg(...)]`                   (net/wled.rs, net/cast_oled.rs)
    #   * a cfg-gated `mod` declaration elsewhere       (net/cast.rs, gated from net.rs)
    # A claim with none of the three reads as verified and is not — that is the find.
    for p, (lines, fg, _mg) in gates_by_file.items():
        backed_by_file = fg is not None or p.stem in gated_modules
        for i, ln in enumerate(lines):
            if not CLAIM.search(ln):
                continue
            lo = max(0, i - CLAIM_WINDOW)
            window = lines[lo:i + CLAIM_WINDOW + 1]
            if backed_by_file or any(CFG_GATE.match(w) for w in window):
                continue
            fails.append(
                f"{p.relative_to(src.parent)}:{i + 1}: a byte-free claim with no mechanism — "
                f"no `#[cfg(...)]` within {CLAIM_WINDOW} lines, no whole-file `#![cfg(...)]`, "
                f"and `mod {p.stem};` is not cfg-gated anywhere")

    # a2. A whole-file `#![cfg(F)]` and the module's `mod` declaration must agree.
    #
    # A file gate ALONE already makes the module empty in an excluded build, so a mismatch is
    # not unsound — but it is two declarations of one fact, and one of them is then decoration
    # that a reader will trust and an editor will change. Same argument as #339: make the
    # agreement a checked fact, not a convention.
    decl_of: dict[str, tuple[Path, str | None]] = {}
    for p, (_l, _fg, mg) in gates_by_file.items():
        for name, gate in mg.items():
            decl_of.setdefault(name, (p, gate))
    for p, (_lines, fg, _mg) in gates_by_file.items():
        if fg is None:
            continue
        name = p.stem
        if name not in decl_of:
            fails.append(f"{p.name}: has `#![cfg({fg})]` but no `mod {name};` declares it")
            continue
        where, decl_gate = decl_of[name]
        if decl_gate is None:
            fails.append(
                f"{p.name}: file says `#![cfg({fg})]` but `mod {name};` in {where.name} is "
                f"UNGATED — one of the two is decoration; gate the declaration too")
        elif set(FEATURE.findall(decl_gate)) != set(FEATURE.findall(fg)):
            fails.append(
                f"{p.name}: file gate `{fg}` disagrees with `mod {name};` gate "
                f"`{decl_gate}` in {where.name}")
    return fails


# ── arm (b): symbol corroboration ────────────────────────────────────────────


def check_symbols(elf: Path, excluded: list[str]) -> tuple[list[str], list[str]]:
    """Look for symbols naming an excluded module in a REAL shipped ELF.

    Returns (failures, notes). See the header: this arm cannot prove absence — it can only
    catch a module whose name reached the linker. `nm` is asked for the demangled table; a
    Rust path renders as `clock::net::wled::…`.
    """
    fails: list[str] = []
    try:
        out = subprocess.run(["nm", "-C", str(elf)], capture_output=True, text=True,
                             timeout=120)
    except (OSError, subprocess.SubprocessError) as exc:
        return [f"could not read symbols from {elf}: {exc}"], []
    if out.returncode != 0:
        return [f"nm failed on {elf}: {out.stderr.strip()[:120]}"], []

    syms = out.stdout
    total = sum(1 for line in syms.splitlines() if "clock::" in line)
    for mod in excluded:
        hits = [line.split()[-1] for line in syms.splitlines()
                if f"clock::net::{mod}::" in line or f"clock::{mod}::" in line]
        if hits:
            fails.append(
                f"symbol table names excluded module `{mod}` — {len(hits)} symbol(s), "
                f"e.g. {hits[0][:90]}")
    notes = [f"  symbols: {total} `clock::` symbols in {elf.name}; "
             f"checked {len(excluded)} excluded module(s): {', '.join(excluded) or '-'}"]
    return fails, notes


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--src", type=Path, default=DEFAULT_SRC)
    ap.add_argument("--elf", type=Path, help="a REAL (non-debug) release ELF for arm (b)")
    ap.add_argument("--excluded", default="",
                    help="comma-separated module names the --elf's tier excludes")
    ap.add_argument("--cargo", type=Path,
                    default=HERE.parent / "rust" / "clock" / "Cargo.toml")
    ap.add_argument("--features", default="",
                    help="the --elf's tier feature string; the excluded-module list is "
                         "DERIVED from it rather than hand-listed (#350's rule). Direct "
                         "membership only — a module gated on a feature that is merely "
                         "IMPLIED by an enabled one is treated as excluded, which is the "
                         "conservative direction (a false FAIL, never a false pass).")
    args = ap.parse_args()

    if not args.src.is_dir():
        print(f"check_byte_free: no source tree at {args.src}", file=sys.stderr)
        return 2

    fails = check_source(args.src)
    notes: list[str] = []
    n_claims = sum(1 for p in rs_files(args.src)
                   for ln in p.read_text(encoding="utf-8").splitlines() if CLAIM.search(ln))
    notes.append(f"  source: {n_claims} byte-free claim(s), each required to sit within "
                 f"{CLAIM_WINDOW} lines of a cfg gate")

    if args.elf:
        excluded = [m.strip() for m in args.excluded.split(",") if m.strip()]
        if args.features and not excluded:
            # TRANSITIVE closure over Cargo.toml's [features]. Direct membership is not
            # enough and the first draft proved it: the fleet tier is `espnow,cast,io`, and
            # `espnow` implies `wifi` implies `hw`, so a direct-membership test called
            # `wifi`, `ota`, `input`, `snake` and eight more "excluded" and produced twelve
            # false FAILs against a healthy image. `--features` also KEEPS default features
            # (gate.sh does not pass --no-default-features), so `default` seeds the closure.
            enabled = {f.strip() for f in args.features.split(",") if f.strip()} | {"default"}
            try:
                with open(args.cargo, "rb") as fh:
                    table = tomllib.load(fh).get("features") or {}
            except (OSError, tomllib.TOMLDecodeError) as exc:
                print(f"check_byte_free: cannot read {args.cargo}: {exc}", file=sys.stderr)
                return 2
            grew = True
            while grew:
                grew = False
                for f in list(enabled):
                    for dep in table.get(f, []):
                        d = dep.split("/")[0].removeprefix("dep:").strip()
                        if d and d in table and d not in enabled:
                            enabled.add(d); grew = True
            # THE CRATE HAS TWO ROOTS. `lib.rs` declares the pure cores gated on `hostsim`
            # (the host-test view) and `main.rs` declares the same files UNGATED (the firmware
            # view). A model keyed on module NAME sees only whichever it read first — the first
            # draft took lib.rs's `#[cfg(feature = "hostsim")]` for `app`/`clock`/`input`/
            # `sensors`/`snake` and reported five modules as excluded from a firmware image
            # that plainly contains them. A firmware ELF is the `main.rs` root, so lib.rs's
            # gates say nothing about it and are skipped here.
            for q in rs_files(args.src):
                if q.name == "lib.rs":
                    continue
                ls = q.read_text(encoding="utf-8").splitlines()
                for name, g in mod_gates(ls).items():
                    feats = set(FEATURE.findall(g or ""))
                    if feats and not (feats & enabled):
                        excluded.append(name)
            excluded = sorted(set(excluded))
        f2, n2 = check_symbols(args.elf, excluded)
        fails += f2
        notes += n2
    else:
        notes.append("  symbols: SKIPPED (no --elf) — arm (a) is the proof; this is only "
                     "corroboration, so its absence does not weaken the verdict")

    for n in notes:
        print(n)
    if fails:
        for f in fails:
            print(f"  FAIL {f}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
