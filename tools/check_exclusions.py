#!/usr/bin/env python3
"""#351 — turn the BYTE-FREE tier claims into a machine-checked fact.

── WHAT WAS WRONG ────────────────────────────────────────────────────────────
The source asserts, repeatedly and with confidence, that a lower build tier contains
none of a higher tier's code:

    rust/clock/Cargo.toml   "the default build is BYTE-FREE of it (symbol-absence provable)"
    rust/clock/src/net.rs   "the default/wifi/espnow builds are byte-free of it"
    rust/clock/src/main.rs  "so the default/wifi/espnow builds are BYTE-FREE of it (#44)"

NOTHING verified any of them. They were aspirations in comments — the same species as
gate.sh's "any feature not covered below should be added", which on 2026-08-01 turned out
to be false for three features at once (#350). A claim that no instrument can refute is
not a guarantee; it is a habit.

── WHY DWARF, AND WHY NOT WHAT THE PRIOR ART SAYS ───────────────────────────
WLED gates the same property and documents the trap: under LTO the LINKER MAP CANNOT
attribute code to modules, so their script uses `readelf --debug-dump=info` and asserts
each selected module contributed a compilation unit
(scratch/parity/multitarget-prior-art.md).

That instrument does not transfer unchanged, and the difference matters:

  * WLED is C++ — one CU per .cpp, so "module" and "CU" coincide.
  * smol's firmware is ONE Rust crate built with `codegen-units = 1` + `lto = "fat"`.
    MEASURED: the linked ELF has exactly ONE CU whose `DW_AT_comp_dir` is the crate
    (`src/main.rs/@/clock.<hash>-cgu.0`). A per-CU assertion would therefore be TRUE for
    every module in every tier, forever — a gate that cannot fail, which is the failure
    this whole issue is about.

The unit that DOES survive is one level down: the CU's **line-table file set**. Every
instruction the linker kept has a line-table row naming the source file it came from, and
that attribution SURVIVES INLINING — verified on this tree, where addresses inside
ssd1306 carry rows pointing at display-interface-i2c. So: build the tier, read the clock
CU's line table, and assert the excluded module contributed no row.

── WHAT IT PROVES, AND WHAT IT DOES NOT ─────────────────────────────────────
PROVES: no *executable code* from the excluded file survived into the tier's binary,
inlined or otherwise. That is what every one of the claims above is actually about — they
are claims about hooks, encoders and drivers.

DOES NOT PROVE: that the file contributed no *data*. A module consisting only of `const`
/ `static` items lands in `.rodata` with no line-table rows, and this check would not see
it. Not hypothetical: the default tier's file set omits `budget.rs` and `board.rs` for
exactly this reason. The `--require-observed` rule below is what keeps that limitation
from silently widening into "the instrument sees nothing and says PASS".

── THE ARM THE BINARY CANNOT PROVIDE ────────────────────────────────────────
Planting a real leak exposed a hole in the above, and it is the hole that matters most. The
realistic regression is not "someone references an excluded module" — that does not compile,
the module is not there. It is "someone drops the `#[cfg]` so their new call site compiles,
and leaves the BYTE-FREE comment". MEASURED: doing that to `pub mod target;` put
src/net/target.rs into the default tier (11 crate files → 12) and this checker reported
**0 leaked** — the claim it would have violated was derived from the very `#[cfg]` that was
deleted. A gate that can be silenced by editing its subject is not a gate.

So the commitment is ALSO declared as data, in build-matrix.toml's `[tier_exclusive]`, and
the source is checked against it in both directions (removed gate / added gate / changed
gate). Removing a gate now fails until the declaration is deleted too, which puts the
decision in the diff where a reviewer sees it.

── THE ANTI-VACUITY RULE ────────────────────────────────────────────────────
An absence check is worthless unless the instrument can be shown to detect presence. So
across a whole run, EVERY claim must be OBSERVED at least once: some checked tier must
enable the module's gate AND find its file in that tier's line table. A module nothing can
see is reported as UNPROVEN, not as passing. That is the difference between this and the
comments it replaces.

Exit codes:  0 ok · 1 a claim failed or is unproven · 2 the tree could not be read
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_CRATE = HERE.parent / "rust" / "clock"
DEFAULT_MANIFEST = HERE / "build-matrix.toml"


class Bad(Exception):
    """The tree could not be read well enough to judge it. Distinct from a FAILED claim:
    one means the instrument is broken, the other means the code is."""


# ── the feature graph ─────────────────────────────────────────────────────────


def feature_graph(cargo_toml: Path) -> dict[str, list[str]]:
    try:
        with open(cargo_toml, "rb") as fh:
            feats = tomllib.load(fh).get("features") or {}
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise Bad(f"{cargo_toml}: cannot read [features] — {exc}")
    if not feats:
        raise Bad(f"{cargo_toml}: no [features] table")
    return {k: list(v) for k, v in feats.items()}


def resolve(features_csv: str, graph: dict[str, list[str]]) -> set[str]:
    """The features a tier actually builds with.

    `default` is included because the gate's cargo invocations do NOT pass
    `--no-default-features` — a resolver that forgot that would compute a smaller feature
    set than the compiler saw, and every extra module in the binary would look like a
    violation. Entries naming a dependency (`dep:esp-hal`) or another crate's feature
    (`esp-wifi/esp-now`) are not OUR features and are dropped: they cannot gate a `#[cfg]`
    in this crate.
    """
    todo = ["default"] + [f.strip() for f in features_csv.split(",") if f.strip()]
    out: set[str] = set()
    while todo:
        f = todo.pop()
        if f in out or f not in graph:
            continue
        out.add(f)
        todo.extend(graph[f])
    return out


# ── the claims, read out of the source ────────────────────────────────────────

MOD_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
ATTR = re.compile(r"^\s*#!?\[(.*)\]\s*$")
CFG_FEATURE = re.compile(r'^cfg\(feature\s*=\s*"([A-Za-z0-9_.-]+)"\)$')
PATH_ATTR = re.compile(r'^path\s*=\s*"([^"]+)"$')
INNER_CFG = re.compile(r'^\s*#!\[cfg\(feature\s*=\s*"([A-Za-z0-9_.-]+)"\)\]\s*$')


class Claim:
    """One module and the feature conjunction that must hold for it to exist."""

    def __init__(self, path: Path, gates: frozenset[str], sites: tuple[str, ...]):
        self.path = path          # relative to the crate root, e.g. src/net/wled.rs
        self.gates = gates        # {"cast"} — ALL must be on, or the file must be absent
        self.sites = sites        # where the gates are written, for the failure message

    def __repr__(self) -> str:  # pragma: no cover — diagnostics only
        return f"Claim({self.path}, {sorted(self.gates)})"


def _attrs_above(lines: list[str], i: int) -> list[str]:
    """The attributes attached to the item on line `i`.

    Walks back over the contiguous run of attributes, comments and blank lines. Comments are
    skipped rather than terminating the walk because this codebase puts a paragraph of
    rationale between the `#[cfg]` and the `mod` it gates — which is the house style and
    must not make the gate blind.
    """
    out, j = [], i - 1
    while j >= 0:
        line = lines[j]
        if not line.strip() or line.lstrip().startswith("//"):
            j -= 1
            continue
        m = ATTR.match(line)
        if not m:
            break
        out.append(m.group(1).strip())
        j -= 1
    return out


def _resolve_mod_file(crate: Path, parent_rel: Path, name: str, path_attr: str | None) -> Path:
    """Where `mod name;` in `parent_rel` reads from. FAILS CLOSED on a miss."""
    base = parent_rel.parent if parent_rel.name in ("main.rs", "lib.rs", "mod.rs") \
        else parent_rel.parent / parent_rel.stem
    cands = [base / path_attr] if path_attr else [base / f"{name}.rs", base / name / "mod.rs"]
    for c in cands:
        if (crate / c).is_file():
            return c
    raise Bad(f"{parent_rel}: `mod {name};` resolves to none of "
              f"{', '.join(str(c) for c in cands)} — refusing to judge a module I cannot find")


def collect_claims(crate: Path, root_rel: Path = Path("src/main.rs")) -> list[Claim]:
    """Walk the FIRMWARE crate root transitively and record every cfg-gated module.

    Rooted at `src/main.rs`, not at a glob over `src/`, for two reasons. It is the binary
    the tiers build — `src/lib.rs` is the #152 hostsim library, a different compilation with
    a different feature set, and folding its `hostsim` gates in here would produce claims no
    firmware tier can ever observe. And a transitive walk cannot silently include a file the
    build has stopped referencing, which a glob would.

    Gates COMPOSE: a module inside a `#[cfg(feature = "espnow")]` module is present only if
    both features are on, so a child inherits its parents' conjunction.
    """
    claims: list[Claim] = []
    seen: set[Path] = set()
    stack: list[tuple[Path, frozenset[str], tuple[str, ...]]] = [(root_rel, frozenset(), ())]

    while stack:
        rel, gates, sites = stack.pop()
        if rel in seen:
            continue
        seen.add(rel)
        try:
            lines = (crate / rel).read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            raise Bad(f"cannot read {rel}: {exc}")

        # A file may gate ITSELF from the inside — `net/wled.rs` and `net/cast_oled.rs` both
        # carry `#![cfg(feature = "…")]`. That is the form the Cargo.toml comments cite, so
        # it is read as a first-class claim rather than assumed redundant with the `mod`.
        for line in lines[:80]:
            m = INNER_CFG.match(line)
            if m and m.group(1) not in gates:
                gates = gates | {m.group(1)}
                sites = sites + (f"{rel}: #![cfg(feature = \"{m.group(1)}\")]",)

        if gates:
            claims.append(Claim(rel, frozenset(gates), sites))

        for i, line in enumerate(lines):
            m = MOD_DECL.match(line)
            if not m:
                continue
            name = m.group(1)
            child_gates, child_sites, path_attr = set(gates), list(sites), None
            for attr in _attrs_above(lines, i):
                mp = PATH_ATTR.match(attr)
                if mp:
                    path_attr = mp.group(1)
                    continue
                if not attr.startswith("cfg"):
                    continue
                mc = CFG_FEATURE.match(attr)
                if mc:
                    child_gates.add(mc.group(1))
                    child_sites.append(f"{rel}:{i + 1}: #[{attr}]")
                elif "feature" in attr:
                    # `any(...)` / `not(...)` / `all(...)` over features. Guessing at the
                    # truth condition is how a checker starts quietly passing; refuse.
                    raise Bad(f"{rel}:{i + 1}: `mod {name};` is gated by a compound cfg "
                              f"`#[{attr}]` this checker does not model — teach it or "
                              f"simplify the gate, but do not let it guess")
            stack.append((_resolve_mod_file(crate, rel, name, path_attr),
                          frozenset(child_gates), tuple(child_sites)))
    return claims


# ── the instrument: what the ELF says it is made of ───────────────────────────


def _readelf(elf: Path, what: str, extra: tuple[str, ...] = ()) -> str:
    try:
        r = subprocess.run(["readelf", f"--debug-dump={what}", *extra, str(elf)],
                           capture_output=True, text=True)
    except FileNotFoundError:
        raise Bad("readelf not found — binutils is required (the #300 stack floor gate "
                  "already depends on it, so this adds no new tool)")
    if r.returncode != 0:
        raise Bad(f"readelf --debug-dump={what} {elf}: {r.stderr.strip()[:200]}")
    return r.stdout


def _line_programs(elf: Path) -> dict[int, tuple[dict[int, str], list[tuple[int, str]]]]:
    progs, off, dirs, files, mode = {}, None, {}, [], None
    for line in _readelf(elf, "rawline").splitlines():
        m = re.match(r"\s*Offset:\s+(?:0x)?([0-9a-fA-F]+)\s*$", line)
        if m:
            if off is not None:
                progs[off] = (dirs, files)
            off, dirs, files, mode = int(m.group(1), 16), {}, [], None
            continue
        if "The Directory Table" in line:
            mode = "d"
            continue
        if "The File Name Table" in line:
            mode = "f"
            continue
        if "Line Number Statements" in line:
            mode = None
            continue
        if mode == "d":
            m = re.match(r"\s*(\d+)\t(.*)$", line)
            if m:
                dirs[int(m.group(1))] = m.group(2).strip()
        elif mode == "f":
            m = re.match(r"\s*(\d+)\t(\d+)\t\S+\t\S+\t(.*)$", line)
            if m:
                files.append((int(m.group(2)), m.group(3).strip()))
    if off is not None:
        progs[off] = (dirs, files)
    return progs


def elf_crate_files(elf: Path, crate: Path) -> set[str]:
    """Crate-relative source files the ELF's line table attributes code to.

    FAILS CLOSED three ways, because each corresponds to a way this could go quietly
    vacuous: no DWARF at all (the release profile ships `debug = false`, so an ELF built
    without `CARGO_PROFILE_RELEASE_DEBUG` has NOTHING to read and every absence check would
    pass); no CU belonging to this crate; or a CU whose file set does not contain the crate
    root, which would mean the join between CU and line program went wrong.
    """
    crate = crate.resolve()
    info = _readelf(elf, "info", ("--dwarf-depth=1",))
    if not info.strip():
        raise Bad(f"{elf}: no DWARF. The release profile does not set `debug`, so this ELF "
                  f"must be built with CARGO_PROFILE_RELEASE_DEBUG=line-tables-only")
    progs = _line_programs(elf)

    cus, cur = [], {}
    for line in info.splitlines():
        if "Compilation Unit @ offset" in line:
            if cur:
                cus.append(cur)
            cur = {}
        m = re.search(r"DW_AT_(comp_dir)\s*:\s*(.*)$", line)
        if m:
            v = m.group(2).strip()
            if v.startswith("("):           # "(indirect string, offset: 0x…): <value>"
                v = v.rsplit("): ", 1)[-1]
            cur["comp_dir"] = v.strip()
        m = re.search(r"DW_AT_stmt_list\s*:\s*(?:0x)?([0-9a-fA-F]+)", line)
        if m:
            cur["stmt_list"] = int(m.group(1), 16)
    if cur:
        cus.append(cur)

    mine = [c for c in cus if c.get("comp_dir") == str(crate)]
    if not mine:
        raise Bad(f"{elf}: no compilation unit has DW_AT_comp_dir = {crate} — cannot tell "
                  f"which code is this crate's")

    found: set[str] = set()
    for cu in mine:
        dirs, files = progs.get(cu.get("stmt_list", -1), ({}, []))
        for di, name in files:
            d = dirs.get(di, "")
            p = Path(d) / name if d else Path(name)
            if not p.is_absolute():
                p = crate / p
            try:
                found.add(str(Path(p).resolve().relative_to(crate)))
            except (ValueError, OSError):
                continue                     # another crate inlined into ours; not our claim
    if "src/main.rs" not in found:
        raise Bad(f"{elf}: the crate CU's line table does not mention src/main.rs — the "
                  f"instrument is reading the wrong thing, so its silence proves nothing")
    return found


# ── the check ─────────────────────────────────────────────────────────────────


def violations(claim: Claim, present: set[str]) -> list[str]:
    """Files attributed to an EXCLUDED module. A directory module drags its whole subtree,
    so `src/bard.rs` being excluded excludes `src/bard/*` too — checking only the named file
    would miss the common case of the submodule that got linked."""
    sub = str(claim.path.parent / claim.path.stem) + "/" \
        if claim.path.name != "mod.rs" else str(claim.path.parent) + "/"
    return sorted(f for f in present if f == str(claim.path) or f.startswith(sub))


def load_declared(manifest: Path) -> dict[str, str]:
    """`[tier_exclusive]` — the modules smol COMMITS to keeping out of lower tiers.

    ── WHY A SECOND STATEMENT OF A FACT THE SOURCE ALREADY CARRIES ───────────────
    Found by planting a real leak, which is the only way it could have been found. The
    realistic regression is not "someone references an excluded module" — that does not
    compile, because the module is not there. It is "someone drops the `#[cfg]` so their new
    call site compiles, and leaves the BYTE-FREE comment above it untouched". MEASURED: doing
    exactly that to `pub mod target;` put `src/net/target.rs` into the default tier (11 crate
    files → 12) and the checker said **0 leaked** — because the claim it would have violated
    was derived from the very `#[cfg]` the regression deleted. The expectation evaporated with
    the gate. A gate that can be silenced by editing its subject is not a gate.

    So the commitment is declared HERE and the source is checked AGAINST it, in both
    directions — the #350 move for the chip roster (`build-matrix.toml` vs `budget.rs`),
    applied to the thing that turned out to need it. Ungating a module now has to be written
    down, where a reviewer sees it in the diff, instead of being a deletion nobody notices.

    Values are the gate conjunction (`"bard+stack-paint"`), so CHANGING a gate is caught too,
    not just removing one. Regenerate with `tools/check_exclusions.py claims --toml`.
    """
    if not manifest.exists():
        return {}
    with open(manifest, "rb") as fh:
        return dict(tomllib.load(fh).get("tier_exclusive") or {})


def declaration_fails(claims: list[Claim], declared: dict[str, str]) -> list[str]:
    fails = []
    derived = {str(c.path): "+".join(sorted(c.gates)) for c in claims}
    for path, gates in sorted(declared.items()):
        if path not in derived:
            fails.append(
                f"DECLARATION: {path} is declared tier-exclusive ({gates}) but is NOT "
                f"cfg-gated in the source any more — it now compiles into EVERY tier. If "
                f"that is intended, delete the [tier_exclusive] line in the same commit and "
                f"fix the comment that still calls the lower tiers byte-free of it.")
        elif derived[path] != gates:
            fails.append(
                f"DECLARATION: {path} is declared to require {gates}, but the source gates "
                f"it on {derived[path]} — one of the two moved.")
    for path, gates in sorted(derived.items()):
        if path not in declared:
            fails.append(
                f"DECLARATION: {path} is cfg-gated on {gates} but has no [tier_exclusive] "
                f"entry — add it so removing the gate later cannot go unnoticed.")
    return fails


def load_unobservable(manifest: Path) -> dict[str, str]:
    """Modules the instrument provably cannot see, each with a written reason.

    Same discipline as #350's `[exempt]`: an omission is fine, an UNDECLARED omission is
    not — and an entry that has stopped applying is worse than none, so a declared-
    unobservable module that IS observed fails the gate.
    """
    if not manifest.exists():
        return {}
    with open(manifest, "rb") as fh:
        return dict(tomllib.load(fh).get("unobservable") or {})


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", choices=("claims", "files", "check"))
    ap.add_argument("--crate", type=Path, default=DEFAULT_CRATE)
    ap.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    ap.add_argument("--elf", action="append", default=[], metavar="TIER=FEATURES=PATH",
                    help="check: one built tier. Repeatable — pass them ALL in one call so "
                         "the anti-vacuity rule can see the whole run. `files`: a bare path.")
    ap.add_argument("--toml", action="store_true",
                    help="claims: emit the [tier_exclusive] block for build-matrix.toml")
    ap.add_argument("--fileset", action="append", default=[], metavar="TIER=FEATURES=PATH",
                    help="check: a tier whose file set is a saved newline list instead of an "
                         "ELF (what `files` prints). Lets tools/test_check_exclusions.sh "
                         "prove every arm can fail without a cross toolchain — the ELF "
                         "reader has its own arms in that suite.")
    args = ap.parse_args()

    try:
        graph = feature_graph(args.crate / "Cargo.toml")
        claims = collect_claims(args.crate)
    except Bad as exc:
        print(f"exclusions: CANNOT READ THE TREE — {exc}", file=sys.stderr)
        return 2

    if args.command == "claims":
        if args.toml:
            # Regenerator for build-matrix.toml's [tier_exclusive]. Printed, never written in
            # place: the whole value of that table is that a human sees the change in a diff.
            print("[tier_exclusive]")
            for c in sorted(claims, key=lambda c: str(c.path)):
                print(f'"{c.path}" = "{"+".join(sorted(c.gates))}"')
            return 0
        print(f"  {len(claims)} cfg-gated modules derived from src/main.rs:")
        for c in sorted(claims, key=lambda c: str(c.path)):
            print(f"    {str(c.path):32} requires {'+'.join(sorted(c.gates))}")
        return 0

    if args.command == "files":
        if len(args.elf) != 1:
            print("exclusions: `files` takes exactly one --elf PATH", file=sys.stderr)
            return 2
        try:
            for f in sorted(elf_crate_files(Path(args.elf[0]), args.crate)):
                print(f)
        except Bad as exc:
            print(f"exclusions: {exc}", file=sys.stderr)
            return 2
        return 0

    # ── check ────────────────────────────────────────────────────────────────
    if not args.elf and not args.fileset:
        print("exclusions: check needs at least one --elf/--fileset TIER=FEATURES=PATH",
              file=sys.stderr)
        return 2
    try:
        unobs = load_unobservable(args.manifest)
        declared = load_declared(args.manifest)
    except Exception as exc:                                  # noqa: BLE001 — reported below
        print(f"exclusions: {args.manifest}: {exc}", file=sys.stderr)
        return 2

    # FIRST, and before a single ELF is read: does the source still make the commitments the
    # manifest says it makes? This is the arm that catches the regression the ELF arm cannot —
    # a deleted `#[cfg]` deletes the claim, so there is nothing left for the binary to violate.
    #
    # Returns IMMEDIATELY on a mismatch rather than folding into the ELF verdict. Once the
    # derived claim set and the declared one disagree, the ELF arm is checking a different
    # question than the one that was committed to, and printing its cheerful per-tier
    # "0 leaked" underneath would be the most misleading output this tool could produce.
    if declared:
        dfails = declaration_fails(claims, declared)
        if dfails:
            print(f"  declaration: {len(declared)} declared vs {len(claims)} derived — "
                  f"{len(dfails)} disagreement(s); NOT reading any ELF")
            for f in dfails:
                print(f"  FAIL {f}", file=sys.stderr)
            return 1

    fails: list[str] = []
    observed: set[str] = set()
    checked = 0

    for kind, spec in [("elf", s) for s in args.elf] + [("set", s) for s in args.fileset]:
        try:
            tier, feats, path = spec.split("=", 2)
        except ValueError:
            print(f"exclusions: --{kind} wants TIER=FEATURES=PATH, got {spec!r}",
                  file=sys.stderr)
            return 2
        try:
            if kind == "elf":
                present = elf_crate_files(Path(path), args.crate)
            else:
                present = {l.strip() for l in Path(path).read_text().splitlines() if l.strip()}
                if "src/main.rs" not in present:
                    raise Bad(f"{path}: file set omits src/main.rs — same refusal the ELF "
                              f"reader makes, for the same reason")
        except (Bad, OSError) as exc:
            print(f"exclusions: {exc}", file=sys.stderr)
            return 2
        on = resolve(feats, graph)
        checked += 1
        excluded = leaked = 0
        for c in claims:
            if c.gates <= on:
                if str(c.path) in present:
                    observed.add(str(c.path))
                continue
            excluded += 1
            bad = violations(c, present)
            if bad:
                leaked += 1
                missing = "+".join(sorted(c.gates - on))
                fails.append(
                    f"tier {tier}: {', '.join(bad)} contributed code, but {missing} is OFF\n"
                    f"        the claim: {'; '.join(c.sites) or c.path}")
        print(f"  {tier:16} {len(present):3} crate files · {excluded:2} modules "
              f"claimed absent · {leaked} leaked")

    # Anti-vacuity: a claim nothing ever saw is UNPROVEN, not passing.
    for c in claims:
        p = str(c.path)
        if p in observed or p in unobs:
            continue
        fails.append(
            f"UNPROVEN: {p} was never observed in any checked tier that enables "
            f"{'+'.join(sorted(c.gates))}, so its absence elsewhere proves nothing. Either a "
            f"tier that builds it is missing from the run, or it contributes no executable "
            f"code — declare it in [unobservable] with the reason if the latter.")
    for p, why in unobs.items():
        if p in observed:
            fails.append(f"[unobservable] names {p}, but it WAS observed — drop the entry "
                         f"({why!r}) rather than let a stale reason stand")
        elif not any(str(c.path) == p for c in claims):
            fails.append(f"[unobservable] names {p}, which is not a cfg-gated module")

    print(f"  {checked} tiers · {len(claims)} claims · {len(observed)} observed"
          + (f" · {len(unobs)} declared unobservable" if unobs else "")
          + (f" · {len(declared)} declared tier-exclusive, source agrees" if declared else ""))
    if fails:
        for f in fails:
            print(f"  FAIL {f}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Bad as exc:
        print(f"exclusions: CANNOT READ THE TREE — {exc}", file=sys.stderr)
        sys.exit(2)
