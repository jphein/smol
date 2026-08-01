#!/usr/bin/env python3
"""#350 — read `tools/build-matrix.toml` and be the ONE place the (chip, tier) matrix exists.

`tools/gate.sh` derives its tier loops from `emit`, CI derives its job matrix from
`ci-matrix`, and `check` refuses the ways the declaration can drift from the rest of the
tree. Nothing downstream hand-lists a tier or a chip; that is the whole design.

Why a checker and not just a reader: the roster is necessarily stated twice — here, and as
`ChipBudget` consts in `rust/clock/src/budget.rs`, which must be Rust because its mechanism
is a compile-time const-assertion and which moves to `smol-core` at #347 Phase 2. Two
statements of one fact is exactly what rots. #339 solved the same shape for the DIAG shed
order by making the agreement a machine-checked fact rather than a convention, and this is
that move again.

Exit codes:  0 ok · 1 a check failed · 2 the manifest is malformed
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_MANIFEST = HERE / "build-matrix.toml"
DEFAULT_REPRO = HERE / "repro_build.sh"
DEFAULT_BUDGET = HERE.parent / "rust" / "clock" / "src" / "budget.rs"


# ── loading ───────────────────────────────────────────────────────────────────


class Bad(Exception):
    """Malformed manifest. Distinct from a failed CHECK: one is a broken file, the other is
    a true statement about a tree that disagrees with itself."""


def load(path: Path) -> dict:
    try:
        with open(path, "rb") as fh:
            doc = tomllib.load(fh)
    except FileNotFoundError:
        raise Bad(f"no manifest at {path}")
    except tomllib.TOMLDecodeError as exc:
        raise Bad(f"{path}: {exc}")

    meta = doc.get("meta") or {}
    chips = doc.get("chip") or {}
    tiers = doc.get("tier") or {}
    if not chips:
        raise Bad("manifest declares no chips")
    if not tiers:
        raise Bad("manifest declares no tiers")

    canon_chip = meta.get("canonical_chip")
    canon_tier = meta.get("canonical_tier")
    if canon_chip not in chips:
        raise Bad(f"meta.canonical_chip {canon_chip!r} is not a declared chip")
    if canon_tier not in tiers:
        raise Bad(f"meta.canonical_tier {canon_tier!r} is not a declared tier")

    # `${canonical}` expands to the canonical tier's feature string, so a combined tier
    # cannot drift from the fleet list it is supposed to extend. Expanded once, here, so
    # every consumer sees the same resolved string.
    canon_features = tiers[canon_tier].get("features")
    if canon_features is None:
        raise Bad(f"canonical tier {canon_tier!r} declares no `features`")
    for name, tier in tiers.items():
        if "features" not in tier:
            raise Bad(f"tier {name!r} declares no `features`")
        tier["features"] = tier["features"].replace("${canonical}", canon_features)
        if "${" in tier["features"]:
            raise Bad(f"tier {name!r} has an unexpanded placeholder: {tier['features']!r}")

    for name, chip in chips.items():
        for field in ("target", "builds", "ships"):
            if field not in chip:
                raise Bad(f"chip {name!r} declares no `{field}`")

    return {"meta": meta, "chips": chips, "tiers": tiers,
            "canonical_chip": canon_chip, "canonical_tier": canon_tier}


# ── the matrix ────────────────────────────────────────────────────────────────


def matrix(doc: dict) -> list[dict]:
    """(chip, tier) pairs, ONE AXIS AT A TIME.

    every buildable chip x the canonical tier   UNION   the canonical chip x every tier

    Deliberately not the cross product. With 3 chips and 6 tiers that would be 18 jobs to
    this function's 8, and the ratio is what kills a matrix as chips accumulate — Tasmota
    ships 99 binaries a release precisely because it refuses to cross its 27-locale axis
    with its variant axis (27 x 10 would be 270). The refusal is the feature.
    """
    ct, cc = doc["canonical_tier"], doc["canonical_chip"]
    out, seen = [], set()
    for chip, spec in doc["chips"].items():
        if spec["builds"]:
            out.append({"chip": chip, "tier": ct, "target": spec["target"],
                        "features": doc["tiers"][ct]["features"]})
            seen.add((chip, ct))
    for tier, spec in doc["tiers"].items():
        if (cc, tier) in seen:
            continue
        out.append({"chip": cc, "tier": tier, "target": doc["chips"][cc]["target"],
                    "features": spec["features"]})
    return out


# ── checks ────────────────────────────────────────────────────────────────────


def repro_fleet_features(path: Path) -> str:
    """The canonical tier as the PACKAGING path defines it.

    Read lexically on purpose: `repro_build.sh` must stay a plain sourced library with no
    parser in it, because `ota_publish.sh` sources it and the publish path is the last
    place to add new failure surface.
    """
    text = path.read_text(encoding="utf-8")
    m = re.search(r'^REPRO_FLEET_FEATURES="\$\{REPRO_FLEET_FEATURES:-([^}"]*)\}"',
                  text, re.M)
    if not m:
        raise Bad(f"could not read REPRO_FLEET_FEATURES from {path}")
    return m.group(1)


def budget_chips(path: Path) -> set[str]:
    """Chip ids declared as `ChipBudget` consts.

    FAILS CLOSED. This is a lexical scrape of Rust, which is a thing to be nervous about, so
    it does not merely collect what it recognises: it counts `ChipBudget {` literals and
    requires a `chip:` string for each one. A declaration written in a form this does not
    recognise therefore makes the check RED rather than silently shrinking the roster it is
    supposed to be comparing — the same discipline `repro_stack_check` uses when it cannot
    read an ELF. `chip: "host"` is skipped: it is the non-device fallback for host builds,
    not a fleet target.
    """
    text = path.read_text(encoding="utf-8")
    # `= ChipBudget {` is an INITIALISER. Anchoring on the `=` matters: a bare
    # `ChipBudget\s*\{` also matches `pub struct ChipBudget {` and `impl ChipBudget {`, which
    # made the first version of this check report 4 literals against 2 `chip:` fields and go
    # red on a correct tree. That is the fail-closed behaviour working as designed — it
    # refused to compare rather than quietly comparing a roster it had miscounted — but the
    # pattern still had to be right, so: initialisers only.
    literals = len(re.findall(r"=\s*ChipBudget\s*\{", text))
    found = re.findall(r'chip:\s*"([A-Za-z0-9_-]+)"', text)
    if len(found) < literals:
        raise Bad(
            f"{path}: found {literals} `ChipBudget {{` literals but only {len(found)} "
            f"`chip:` fields — refusing to compare a roster I cannot fully read")
    return {c for c in found if c != "host"}


def check(doc: dict, repro: Path, budget: Path) -> list[str]:
    fails: list[str] = []
    chips, tiers = doc["chips"], doc["tiers"]

    # 1. ships => builds. You cannot publish what nothing compiles.
    for name, spec in chips.items():
        if spec["ships"] and not spec["builds"]:
            fails.append(f"chip {name}: ships = true but builds = false")
    for name, spec in tiers.items():
        if spec.get("ships") and name != doc["canonical_tier"]:
            # A shipped tier that is not the canonical one means two fleet images exist and
            # `REPRO_FLEET_FEATURES` no longer names "the" image. That may become true, but
            # it is a design change, not a config tweak.
            fails.append(f"tier {name}: ships = true, but only the canonical tier ships")

    # 2. the canonical tier equals what the packaging path builds.
    try:
        rff = repro_fleet_features(repro)
        declared = tiers[doc["canonical_tier"]]["features"]
        if rff != declared:
            fails.append(
                f"canonical tier drift: manifest says {declared!r}, "
                f"REPRO_FLEET_FEATURES says {rff!r}")
    except Bad as exc:
        fails.append(str(exc))

    # 3. the chip roster agrees with the #348 budgets, in BOTH directions.
    if budget.exists():
        try:
            declared_chips = budget_chips(budget)
            manifest_chips = set(chips)
            for missing in sorted(manifest_chips - declared_chips):
                if chips[missing]["builds"]:
                    fails.append(
                        f"chip {missing}: builds = true but no ChipBudget in {budget.name} "
                        f"— a buildable chip with no declared memory budget")
            for extra in sorted(declared_chips - manifest_chips):
                fails.append(
                    f"chip {extra}: has a ChipBudget in {budget.name} but no manifest row")
        except Bad as exc:
            fails.append(str(exc))

    # 4. the emitted matrix is not a cross product.
    n = len(matrix(doc))
    buildable = sum(1 for c in chips.values() if c["builds"])
    if buildable and n > buildable + len(tiers):
        fails.append(f"matrix emitted {n} jobs; one-axis-at-a-time allows at most "
                     f"{buildable + len(tiers)} — this is a cross product")
    return fails


# ── cli ───────────────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("command", choices=("emit", "chips", "ci-matrix", "check"))
    ap.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    ap.add_argument("--repro", type=Path, default=DEFAULT_REPRO)
    ap.add_argument("--budget", type=Path, default=DEFAULT_BUDGET)
    ap.add_argument("--for", dest="phase", choices=("check", "clippy"), default="check",
                    help="emit: which gate phase's tier list to produce")
    ap.add_argument("--builds", action="store_true", help="chips: only buildable ones")
    args = ap.parse_args()

    try:
        doc = load(args.manifest)
    except Bad as exc:
        print(f"build-matrix: MALFORMED — {exc}", file=sys.stderr)
        return 2

    if args.command == "emit":
        # `name<TAB>features`, one per line, for the gate's tier loops. Tab-separated so an
        # empty feature string (the `default` tier) survives the round trip — a colon-joined
        # form cannot express "no features" distinguishably from a missing field.
        for name, spec in doc["tiers"].items():
            if spec.get(args.phase, True):
                print(f"{name}\t{spec['features']}")
        return 0

    if args.command == "chips":
        for name, spec in doc["chips"].items():
            if not args.builds or spec["builds"]:
                print(name)
        return 0

    if args.command == "ci-matrix":
        print(json.dumps({"include": matrix(doc)}, separators=(",", ":")))
        return 0

    fails = check(doc, args.repro, args.budget)
    jobs = matrix(doc)
    builds = [c for c, s in doc["chips"].items() if s["builds"]]
    ships = [c for c, s in doc["chips"].items() if s["ships"]]
    print(f"  build matrix: {len(jobs)} jobs · chips builds={','.join(builds) or '-'} "
          f"ships={','.join(ships) or '-'} · {len(doc['tiers'])} tiers")
    if fails:
        for f in fails:
            print(f"  FAIL {f}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
