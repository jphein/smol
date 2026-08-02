#!/usr/bin/env python3
"""#367 — every host verifier must test code the firmware actually contains.

The `experiments/*_verify` harnesses `#[path]`-include a firmware source file so a pure module
can be host-tested without dragging in the no_std/riscv deps. That escape hatch is legitimate and
worth keeping — it is how `wire`, `flood`, `treehead` and friends get real host coverage.

What it does NOT do is prove the module is *wired in*. `#[path]` reads a FILE. It does not care
whether the crate declares `mod <name>;` anywhere, so a verifier can be green against a source
file that is compiled into no firmware tier at all.

That is not hypothetical. `rust/clock/src/net/crdt.rs` (#185) has no `mod crdt;` anywhere in the
crate: `experiments/185_crdt_verify` passes, `tools/gate.sh` goes green, and the fleet binary has
never contained a byte of it. The gate cannot distinguish "wired and working" from "on disk and
never compiled" — which makes a passing verifier read as evidence of delivery when it is not.

This check closes that gap with one machine-checked fact per verifier:

    for each `#[path]` target under the firmware crate, does the crate declare it as a module?

VERDICTS
  SOUND    declared → compiled into at least one tier → the verifier tests shipped code.
  PHANTOM  no declaration anywhere → the verifier certifies code the fleet never runs. FAILS.

A declaration's `#[cfg(...)]` is reported so a reader can see which tiers carry it; a module
gated to a feature no tier enables is a narrower question this check deliberately does not try to
answer (it would need the tier matrix, which lives in gate.sh and moves). Reporting the cfg
verbatim is honest; inferring reachability from it would be a guess dressed as a fact.

Usage:  tools/check_verifier_wiring.py [repo_root]
Exit:   0 all sound · 1 at least one PHANTOM · 2 usage/IO error
"""
import re
import sys
from pathlib import Path

# Phantoms that are KNOWN and TRACKED. An entry is a promise that someone owns the decision, so
# each one must name the issue that owns it — an unexplained entry is just a way to make red go
# green. The list is checked in BOTH directions: an unlisted phantom fails (a new one cannot slip
# in), and a listed entry that has since become SOUND *also* fails, so the list cannot quietly rot
# into a permanent excuse for something already fixed.
KNOWN_PHANTOMS = {
    "rust/clock/src/net/crdt.rs":
        "#185 — L4 CRDT core landed + host-tested, but no consumer exists yet. Wiring it means "
        "first deciding what multi-writer state smol has (RPG loot / gravestone / seen-set are "
        "all still hypothetical), which is a design call, not a mechanical fix.",
}

PATH_ATTR = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')
# `mod x;` / `pub mod x;` / `pub(crate) mod x;` — declaration only, never `mod x { .. }` inline
# (an inline module is not file-backed, so it cannot be a #[path] target).
MOD_DECL = re.compile(r'^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;')


def reachable_from(root: Path, crate_src: Path):
    """Every source file in the module tree rooted at `root`, resolved the way rustc resolves it.

    THIS IS A PER-ROOT WALK, AND THAT IS THE ENTIRE POINT. The clock crate has TWO roots that
    declare overlapping files with different gating: `main.rs` (the firmware bin) and `lib.rs`
    (the `hostsim` library the web emulator builds). Keying on a module NAME and searching the
    whole tree conflates them — #351's first checker did exactly that, picked up `lib.rs`'s
    `#[cfg(feature = "hostsim")]`, and declared `app`/`clock`/`input`/`sensors`/`snake` excluded
    from a firmware image that plainly contains them. Five false negatives from one shortcut.

    Walking each root separately also makes this immune to three traps that bite gate-parsing
    tools on this crate, because the verdict never consults a `cfg` at all:
      * `#![cfg_attr(not(espnow), allow(dead_code))]` in `wifi.rs` is a LINT attribute, not a
        gate — a checker that greps for cfg-shaped things near a module reads it as one;
      * a module's gate can live in a different file (`net/cast.rs` is gated from `net.rs`);
      * feature membership is transitive (`espnow` -> `wifi` -> `hw`), so direct membership
        under-approximates.
    Reachability answers "is this file in the tree rustc compiles for this root", which is the
    question, and needs none of that modelling.
    """
    seen, out, stack = set(), set(), [root]
    while stack:
        f = stack.pop()
        if f in seen or not f.exists():
            continue
        seen.add(f)
        out.add(f.resolve())
        lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
        for i, line in enumerate(lines):
            m = MOD_DECL.match(line)
            if not m:
                continue
            name = m.group(1)
            # A `#[path = ".."]` attribute on the declaration overrides the filename. `lib.rs`
            # uses this for the bard cores, so it is not hypothetical.
            override = None
            for j in range(max(0, i - 4), i):
                pm = PATH_ATTR.search(lines[j])
                if pm:
                    override = pm.group(1)
            if override:
                cands = [f.parent / override, crate_src / override]
            else:
                base = f.parent if f.name in ("main.rs", "lib.rs") else f.parent / f.stem
                cands = [base / f"{name}.rs", base / name / "mod.rs"]
            for c in cands:
                if c.exists():
                    stack.append(c)
                    break
    return out


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    experiments, crate_src = root / "experiments", root / "rust" / "clock" / "src"
    if not experiments.is_dir() or not crate_src.is_dir():
        print(f"error: run from the smol repo root (got {root})", file=sys.stderr)
        return 2

    ships = reachable_from(crate_src / "main.rs", crate_src)   # the firmware bin
    hostsim = reachable_from(crate_src / "lib.rs", crate_src)  # the hostsim/web-emu library
    if not ships:
        print("error: main.rs reached no modules — the walk is broken, refusing to report",
              file=sys.stderr)
        return 2

    rows, phantoms, missing_files = [], [], []
    for main_rs in sorted(experiments.rglob("*.rs")):
        text = main_rs.read_text(encoding="utf-8", errors="replace")
        verifier = main_rs.relative_to(experiments).parts[0]
        for rel in PATH_ATTR.findall(text):
            target = (main_rs.parent / rel).resolve()
            try:
                target.relative_to(crate_src)
            except ValueError:
                continue  # including another experiment's helper claims nothing about firmware
            if not target.exists():
                missing_files.append((verifier, rel))
                continue
            shown = str(target.relative_to(root))
            if target in ships:
                verdict, where = "SOUND", "main.rs tree"
            elif target in hostsim:
                verdict, where = "HOST-ONLY", "lib.rs tree (hostsim)"
            else:
                verdict, where = "PHANTOM", "neither root"
                phantoms.append((verifier, shown))
            rows.append((verifier, shown, where, verdict))

    if rows:
        w = [max(len(str(r[i])) for r in rows) for i in range(3)]
    else:
        w = [8, 8, 8]
    print(f"{'verifier':<{w[0]}}  {'#[path] target':<{w[1]}}  {'reachable from':<{w[2]}}  verdict")
    print("-" * (sum(w) + 14))
    order = {"PHANTOM": 0, "HOST-ONLY": 1, "SOUND": 2}
    for r in sorted(rows, key=lambda r: (order[r[3]], r[0])):
        print(f"{r[0]:<{w[0]}}  {r[1]:<{w[1]}}  {r[2]:<{w[2]}}  {r[3]}")

    n_sound = sum(1 for r in rows if r[3] == "SOUND")
    n_host = sum(1 for r in rows if r[3] == "HOST-ONLY")
    print(f"\n{len(rows)} #[path] include(s) into the firmware crate across "
          f"{len({r[0] for r in rows})} verifier(s): "
          f"{n_sound} sound, {n_host} host-only, {len(phantoms)} phantom.")
    if n_host:
        print("HOST-ONLY = reachable from lib.rs but not main.rs: real code, but it ships in the\n"
              "web emulator rather than the firmware. Not a defect; just not a firmware claim.")

    for verifier, rel in missing_files:
        print(f"error: {verifier} includes {rel}, which does not exist", file=sys.stderr)

    tracked = [(v, m) for v, m in phantoms if m in KNOWN_PHANTOMS]
    untracked = [(v, m) for v, m in phantoms if m not in KNOWN_PHANTOMS]
    sound_paths = {r[1] for r in rows if r[3] in ("SOUND", "HOST-ONLY")}
    stale = sorted(sound_paths & set(KNOWN_PHANTOMS))

    if tracked:
        print("\nKNOWN phantoms (tracked, not failing):")
        for verifier, mod in tracked:
            print(f"  {verifier} -> {mod}\n      {KNOWN_PHANTOMS[mod]}")

    if stale:
        print("\nSTALE allowlist — these are now reachable and must be removed from KNOWN_PHANTOMS:",
              file=sys.stderr)
        for mod in stale:
            print(f"  {mod}", file=sys.stderr)
        print("An allowlist that outlives its reason is how a check stops checking.", file=sys.stderr)

    if untracked:
        print("\nPHANTOM — a green verifier over code NEITHER crate root compiles:", file=sys.stderr)
        for verifier, mod in untracked:
            print(f"  {verifier} -> {mod}", file=sys.stderr)
        print("\nFix EITHER by wiring the module (declare it in the root that should carry it) OR by\n"
              "removing the verifier. A verifier over uncompiled source is worse than no verifier:\n"
              "it reports the code as covered.\n"
              "If it is a deliberate, owned exception, add it to KNOWN_PHANTOMS with its issue.",
              file=sys.stderr)

    if untracked or stale:
        return 1
    return 2 if missing_files else 0


if __name__ == "__main__":
    raise SystemExit(main())
