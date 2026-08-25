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
DEFAULT_CARGO = HERE.parent / "rust" / "clock" / "Cargo.toml"
# #413: the two halves of the chip id<->name map. `net/target.rs` is AUTHORITATIVE — the id is the
# #349 wire format the device reads out of an image's target descriptor — and `ota_publish.sh`
# carries a python copy so it can name the chip it is about to stage.
DEFAULT_TARGET_RS = HERE.parent / "rust" / "clock" / "src" / "net" / "target.rs"
DEFAULT_OTA_PUBLISH = HERE / "ota_publish.sh"


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
        # `checks` (#347 Part 2) is REQUIRED like the other three, not defaulted. A default would
        # have to be either `true` (claiming every new chip compiles — the optimistic lie) or
        # `false` (claiming none do, so a chip that compiles goes unnoticed — the pessimistic one).
        # There is no safe default for a measurement, so the manifest must state it.
        for field in ("target", "builds", "ships", "checks"):
            if field not in chip:
                raise Bad(f"chip {name!r} declares no `{field}`")

    return {"meta": meta, "chips": chips, "tiers": tiers,
            "exempt": doc.get("exempt") or {},
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


def chip_ids_from_firmware(path: Path) -> dict[int, str]:
    """`CHIP_ESP32C3: u8 = 1;` -> {1: "esp32c3"}. The AUTHORITATIVE map: the id is #349's wire
    format, so the firmware that stamps it defines it.

    Guarded the way `budget_chips` is: if the count of `CHIP_*` declarations does not match the
    count of ids parsed, refuse rather than compare a roster we cannot fully read. A regex over
    Rust is acceptable here (the precedent is `budget_chips`), a SILENT partial parse is not.
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    decls = re.findall(r"^pub const CHIP_(ESP32[A-Z0-9]+): u8 = (\d+);", text, re.M)
    # `CHIP_UNKNOWN` is deliberately excluded by the pattern: 0 is "no chip", not a chip.
    literal_count = len(re.findall(r"^pub const CHIP_ESP32[A-Z0-9]+: u8 =", text, re.M))
    if literal_count != len(decls):
        raise Bad(f"{path}: found {literal_count} CHIP_ESP32* declarations but parsed "
                  f"{len(decls)} ids — refusing to compare a roster I cannot fully read")
    if not decls:
        raise Bad(f"{path}: no CHIP_ESP32* declarations found — anchor lost, failing closed")
    return {int(n): name.lower() for name, n in decls}


def chip_ids_from_publisher(path: Path) -> dict[int, str]:
    """The `CHIPS = {1: "esp32c3", ...}` dict inside `ota_publish.sh`'s embedded python.

    Parsed rather than imported because it lives in a heredoc. Same fail-closed guard: the entry
    count must match, or we are comparing against a partial read.
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    m = re.search(r"^CHIPS = \{(.*?)\}", text, re.M | re.S)
    if not m:
        raise Bad(f"{path}: no `CHIPS = {{...}}` map found — anchor lost, failing closed")
    body = m.group(1)
    pairs = re.findall(r"(\d+)\s*:\s*\"([a-z0-9]+)\"", body)
    if len(pairs) != body.count(":"):
        raise Bad(f"{path}: CHIPS has {body.count(':')} entries but {len(pairs)} parsed — "
                  f"refusing to compare a map I cannot fully read")
    return {int(n): name for n, name in pairs}


def budget_chips(path: Path) -> set[str]:
    """Chip ids declared as `ChipBudget` consts.

    FAILS CLOSED. This is a lexical scrape of Rust, which is a thing to be nervous about, so
    it does not merely collect what it recognises: it counts `ChipBudget {` literals and
    requires a `chip:` string for each one. A declaration written in a form this does not
    recognise therefore makes the check RED rather than silently shrinking the roster it is
    supposed to be comparing — the same discipline `repro_stack_check` uses when it cannot
    read an ELF. `chip: "host"` is skipped: it is the non-device fallback for host builds,
    not a fleet target.

    `chip: "unmeasured"` is skipped for the same KIND of reason and it is worth being precise
    about, because the two look alike and are not (#347 Part 2). `host` is a real build with no
    device budget. `unmeasured` is a real DEVICE whose budget nobody has measured yet — the poison
    row a declared chip selects until a hardware measurement exists, every field zero so that
    `fits_dram`/`fits_flash` answer no and the budget-predicated features refuse to compile.

    Neither is a fleet target, which is the only property this function is about: it compares the
    chip ROSTER against `build-matrix.toml`, and a row that stands for "no chip" would demand a
    `[chip.unmeasured]` section — a build job for the absence of a board.

    ⚠️ Note what is deliberately NOT skipped: a chip that is declared with real numbers but is
    not in the matrix still fails the check, in both directions. The skip list is for rows that
    are not chips, never for chips that are inconvenient.
    """
    # Rows that are not fleet targets. Kept as a named set rather than two inline `!=` tests so
    # that adding a third one requires reading the docstring's rule for what belongs here.
    NOT_A_FLEET_TARGET = {"host", "unmeasured"}
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
    return {c for c in found if c not in NOT_A_FLEET_TARGET}


def cargo_features(path: Path) -> set[str]:
    """Feature names from `[features]`. Parsed as TOML, not scraped — Cargo.toml IS TOML, so
    there is no excuse for a regex here (unlike budget.rs, which is Rust and has one)."""
    try:
        with open(path, "rb") as fh:
            return set((tomllib.load(fh).get("features") or {}))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise Bad(f"{path}: cannot read [features] — {exc}")


def check(doc: dict, repro: Path, budget: Path) -> list[str]:
    fails: list[str] = []
    chips, tiers = doc["chips"], doc["tiers"]

    # 1. ships => builds => checks. You cannot publish what nothing builds, and you cannot
    #    build what does not compile. #347 Part 2 added the third rung; the ladder is checked
    #    one step at a time so the failure names the rung that broke rather than the whole chain.
    for name, spec in chips.items():
        if spec["ships"] and not spec["builds"]:
            fails.append(f"chip {name}: ships = true but builds = false")
        if spec["builds"] and not spec["checks"]:
            fails.append(
                f"chip {name}: builds = true but checks = false — CI is declared to produce an "
                f"artifact for a chip whose source is declared not to compile")
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

    # 3b. #413: the chip id<->name map agrees between the FIRMWARE (authoritative — the id is
    #     #349's wire format) and the PUBLISHER's copy. This is the FOURTH copy of the chip roster
    #     in the tree and the one nothing compared, which is how `ota_publish.sh` came to be
    #     silently short esp32c5 (id 4). The consequence was not a crash: a valid C5 descriptor
    #     was reported as ABSENT and the image staged legacy-only, sending anyone debugging it
    #     into the firmware's descriptor emission. Checked in BOTH directions, so neither side can
    #     grow or lose a chip alone.
    try:
        fw = chip_ids_from_firmware(doc.get("_target_rs") or DEFAULT_TARGET_RS)
        pub = chip_ids_from_publisher(doc.get("_ota_publish") or DEFAULT_OTA_PUBLISH)
        for cid, name in sorted(fw.items()):
            if cid not in pub:
                fails.append(f"chip id {cid} ({name}) is declared in net/target.rs but MISSING "
                             f"from ota_publish.sh's CHIPS — a valid {name} image would be "
                             f"reported as having no target descriptor and staged legacy-only")
            elif pub[cid] != name:
                fails.append(f"chip id {cid}: net/target.rs says {name!r}, "
                             f"ota_publish.sh says {pub[cid]!r}")
        for cid, name in sorted(pub.items()):
            if cid not in fw:
                fails.append(f"chip id {cid} ({name}) is in ota_publish.sh's CHIPS but has no "
                             f"CHIP_* constant in net/target.rs — the publisher would name a chip "
                             f"the firmware cannot stamp")
    except Bad as exc:
        fails.append(str(exc))

    # 4. every cargo feature is covered by a tier, or exempted with a reason.
    #
    # `tools/gate.sh` carried this as an aspiration in a comment — "any feature listed in
    # Cargo.toml's [features] that is not covered below should be added" — and prose cannot
    # be tested, so it wasn't: `wled`, `coexist-soak` and `mesh-test` were all in Cargo.toml
    # with no tier, and `stack-paint` had stopped compiling entirely without anyone noticing.
    # An omission is fine; an UNDECLARED omission is not.
    cargo_toml = doc.get("_cargo_toml")
    if cargo_toml and cargo_toml.exists():
        feats = cargo_features(cargo_toml)
        covered = {f for spec in tiers.values()
                   for f in spec["features"].split(",") if f}
        exempt = set(doc.get("exempt") or {})
        for gap in sorted(feats - covered - exempt):
            fails.append(
                f"feature {gap!r} is in Cargo.toml but no tier builds it and it is not in "
                f"[exempt] — a code path nothing compiles (#338)")
        for stale in sorted(exempt & covered):
            fails.append(f"feature {stale!r} is both exempted and covered by a tier — "
                         f"drop the [exempt] entry so the reason cannot go stale")
        for ghost in sorted(exempt - feats):
            fails.append(f"[exempt] names {ghost!r}, which is not a feature in Cargo.toml")

    # 5. the emitted matrix is not a cross product.
    n = len(matrix(doc))
    buildable = sum(1 for c in chips.values() if c["builds"])
    if buildable and n > buildable + len(tiers):
        fails.append(f"matrix emitted {n} jobs; one-axis-at-a-time allows at most "
                     f"{buildable + len(tiers)} — this is a cross product")
    return fails


# ── cli ───────────────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("command",
                    choices=("emit", "chips", "chip-checks", "canonical-chip", "config-markers",
                             "ci-matrix", "check"))
    ap.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    ap.add_argument("--repro", type=Path, default=DEFAULT_REPRO)
    ap.add_argument("--budget", type=Path, default=DEFAULT_BUDGET)
    ap.add_argument("--cargo", type=Path, default=DEFAULT_CARGO)
    # #413: fixture hooks for the id<->name roster arm, mirroring --budget/--cargo so the
    # regression suite can craft a SHORT roster instead of sabotaging the real files.
    ap.add_argument("--target-rs", type=Path, default=DEFAULT_TARGET_RS)
    ap.add_argument("--ota-publish", type=Path, default=DEFAULT_OTA_PUBLISH)
    ap.add_argument("--for", dest="phase", choices=("check", "clippy"), default="check",
                    help="emit: which gate phase's tier list to produce")
    ap.add_argument("--builds", action="store_true", help="chips: only buildable ones")
    ap.add_argument("--chip", default=None, help="config-markers: restrict to one chip")
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

    if args.command == "config-markers":
        # #280: `<target>\t<marker>` per line, for `tools/assert_cargo_config.sh`.
        #
        # A SEPARATE COMMAND rather than two more columns on `chip-checks`, deliberately. The
        # guard has a second caller that wants nothing else from the row (the publish path, for
        # #413), and widening a tab-separated contract to serve a consumer that needs one field
        # is how the sentinel-and-shifting-fields bug in `check_chips.sh` was born.
        #
        # Chip filter is positional-free: pass `--chip`, or get every chip. An unknown chip is an
        # ERROR, not an empty result — a guard handed a typo'd chip name must not report success
        # by finding nothing to check.
        chips = doc["chips"]
        if args.chip:
            if args.chip not in chips:
                print(f"config-markers: unknown chip {args.chip!r} — "
                      f"known: {', '.join(sorted(chips))}", file=sys.stderr)
                return 2
            chips = {args.chip: chips[args.chip]}
        for name, spec in chips.items():
            for marker in spec.get("config_markers") or []:
                print(f"{spec['target']}\t{marker}")
        return 0

    if args.command == "chip-checks":
        # #347 Part 2: one line per chip for `tools/check_chips.sh`, which runs the per-chip
        # `cargo check` that `checks` declares the outcome of. Everything the invocation needs
        # comes from here, so the harness hardcodes NO chip, NO triple and NO toolchain — the
        # same rule `emit` follows for tiers.
        #
        # Fields, tab-separated:
        #   chip · target · expect(check|fail) · toolchain · build_std · opt_level · features
        #
        # `opt_level` (#398): a per-chip release-profile override, threaded as
        # CARGO_PROFILE_RELEASE_OPT_LEVEL by the consumer. Exists for toolchain-bug workarounds
        # (the S3's fat-LTO Xtensa crash) — the global profile is shared with the canonical chip
        # and must never carry a per-chip deviation. Inert for `cargo check` (no codegen), carried
        # anyway so every consumer of this record — including future build rungs — gets it from
        # ONE place.
        #
        # ⚠️ EMPTY OPTIONAL FIELDS ARE WRITTEN AS "-", NOT LEFT EMPTY, and that is not cosmetic.
        # `emit` gets away with bare tabs because it has TWO fields and only the last can be empty.
        # This record has six with two optional ones in the MIDDLE, and a tab is an IFS *whitespace*
        # character in bash — so `read -r a b c d e f` COLLAPSES a run of consecutive tabs and every
        # field after the gap shifts left. Measured, not theorised: the first run of check_chips.sh
        # read the C3's feature string as its toolchain name and tried `cargo +espnow,cast,io`.
        # A sentinel is immune to it, greppable, and survives any reader's IFS.
        SENTINEL = "-"
        def opt(v: object) -> str:
            s = str(v or "").strip()
            return s if s else SENTINEL
        #
        # `features` is the CANONICAL TIER's, always. The per-chip axis crosses one tier only —
        # rule 2 of the manifest, ONE AXIS AT A TIME — so this deliberately cannot be asked for
        # a cross product of chips and tiers.
        canon_features = doc["tiers"][doc["canonical_tier"]]["features"]
        for name, spec in doc["chips"].items():
            expect = "check" if spec["checks"] else "fail"
            print("\t".join((name, spec["target"], expect,
                             opt(spec.get("toolchain")),
                             opt(spec.get("build_std")),
                             opt(spec.get("opt_level")),
                             canon_features)))
        return 0

    if args.command == "canonical-chip":
        # #413: `tools/gate.sh` needs the chip whose floor its stack arm measures against, and
        # since that arm builds the CANONICAL tier the answer is `meta.canonical_chip`. Emitted
        # here rather than hardcoded there, for the same reason every other roster fact is: a
        # second copy of "the canonical chip is esp32c3" would be free to drift from this one.
        print(doc["canonical_chip"])
        return 0

    if args.command == "ci-matrix":
        print(json.dumps({"include": matrix(doc)}, separators=(",", ":")))
        return 0

    doc["_cargo_toml"] = args.cargo
    doc["_target_rs"] = args.target_rs
    doc["_ota_publish"] = args.ota_publish
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
