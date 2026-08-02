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
# (an inline module is not a file-backed one, so it cannot be a #[path] target).
MOD_DECL = re.compile(r'^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;')
CFG_ATTR = re.compile(r'^\s*#\[\s*(cfg|cfg_attr)\b')


def crate_declarations(crate_src: Path):
    """Map module name -> list of (file, line, cfg-lines-immediately-above)."""
    decls = {}
    for rs in sorted(crate_src.rglob("*.rs")):
        try:
            lines = rs.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError as e:
            print(f"error: cannot read {rs}: {e}", file=sys.stderr)
            raise SystemExit(2)
        for i, line in enumerate(lines):
            m = MOD_DECL.match(line)
            if not m:
                continue
            # Walk back over contiguous attribute lines to capture the gating cfg(s).
            cfgs, j = [], i - 1
            while j >= 0 and (CFG_ATTR.match(lines[j]) or lines[j].lstrip().startswith("#[")):
                if CFG_ATTR.match(lines[j]):
                    cfgs.insert(0, lines[j].strip())
                j -= 1
            decls.setdefault(m.group(1), []).append((rs, i + 1, cfgs))
    return decls


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    experiments, crate_src = root / "experiments", root / "rust" / "clock" / "src"
    if not experiments.is_dir() or not crate_src.is_dir():
        print(f"error: run from the smol repo root (got {root})", file=sys.stderr)
        return 2

    decls = crate_declarations(crate_src)
    rows, phantoms, missing_files = [], [], []

    for main_rs in sorted(experiments.rglob("*.rs")):
        text = main_rs.read_text(encoding="utf-8", errors="replace")
        verifier = main_rs.relative_to(experiments).parts[0]
        for rel in PATH_ATTR.findall(text):
            target = (main_rs.parent / rel).resolve()
            # Only firmware-crate sources are in scope; a verifier including another experiment's
            # helper is not making a claim about shipped code.
            try:
                target.relative_to(crate_src)
            except ValueError:
                continue
            if not target.exists():
                missing_files.append((verifier, rel))
                continue
            name = target.stem
            where = decls.get(name, [])
            if where:
                sites = "; ".join(
                    f"{p.relative_to(root)}:{ln}" + (f" {' '.join(c)}" if c else " (no cfg)")
                    for p, ln, c in where
                )
                rows.append((verifier, str(target.relative_to(root)), "yes", sites, "SOUND"))
            else:
                rows.append((verifier, str(target.relative_to(root)), "NO", "-", "PHANTOM"))
                phantoms.append((verifier, str(target.relative_to(root))))

    w = [max(len(str(r[i])) for r in rows + [("verifier", "module", "decl", "declared at", "verdict")])
         for i in range(5)] if rows else [8] * 5
    hdr = ("verifier", "#[path] target", "declared?", "declared at", "verdict")
    print(f"{hdr[0]:<{w[0]}}  {hdr[1]:<{w[1]}}  {hdr[2]:<{w[2]}}  {hdr[3]:<{w[3]}}  {hdr[4]}")
    print("-" * (sum(w) + 8))
    for r in sorted(rows, key=lambda r: (r[4] != "PHANTOM", r[0])):
        print(f"{r[0]:<{w[0]}}  {r[1]:<{w[1]}}  {r[2]:<{w[2]}}  {r[3]:<{w[3]}}  {r[4]}")

    print(f"\n{len(rows)} #[path] include(s) into the firmware crate across "
          f"{len({r[0] for r in rows})} verifier(s): "
          f"{sum(1 for r in rows if r[4] == 'SOUND')} sound, {len(phantoms)} phantom.")

    for verifier, rel in missing_files:
        print(f"error: {verifier} includes {rel}, which does not exist", file=sys.stderr)

    # Split phantoms into tracked (allowlisted, reported loudly) and new (fatal).
    tracked = [(v, m) for v, m in phantoms if m in KNOWN_PHANTOMS]
    untracked = [(v, m) for v, m in phantoms if m not in KNOWN_PHANTOMS]
    sound_paths = {r[1] for r in rows if r[4] == "SOUND"}
    stale = sorted(sound_paths & set(KNOWN_PHANTOMS))

    if tracked:
        print("\nKNOWN phantoms (tracked, not failing):")
        for verifier, mod in tracked:
            print(f"  {verifier} -> {mod}\n      {KNOWN_PHANTOMS[mod]}")

    if stale:
        print("\nSTALE allowlist — these are now WIRED and must be removed from KNOWN_PHANTOMS:",
              file=sys.stderr)
        for mod in stale:
            print(f"  {mod}", file=sys.stderr)
        print("An allowlist that outlives its reason is how a check stops checking.", file=sys.stderr)

    if untracked:
        print("\nPHANTOM — a green verifier over code no firmware tier compiles:", file=sys.stderr)
        for verifier, mod in untracked:
            print(f"  {verifier} -> {mod}", file=sys.stderr)
        print("\nFix EITHER by wiring the module (add `mod <name>;` to the crate, with the cfg the\n"
              "consumer needs) OR by removing the verifier. A verifier over uncompiled source is\n"
              "worse than no verifier: it reports the code as covered.\n"
              "If it is a deliberate, owned exception, add it to KNOWN_PHANTOMS with its issue.",
              file=sys.stderr)

    if untracked or stale or missing_files:
        return 1 if (untracked or stale) else 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
