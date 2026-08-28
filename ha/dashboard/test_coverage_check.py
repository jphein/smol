#!/usr/bin/env python3
"""Proves `report_unrepresented` (#356) can FAIL — and can stay quiet when it should.

Why this exists. The defect #356 describes is a checker-shaped one: `smol-telemetry` drifted to
covering 2 of 8 fleet members while nobody noticed, because #340's `--check` only ever asked
"is this card wired to a node that exists?" and never the inverse. Shipping the inverse check
without watching it go red would be the same mistake one level up — this repo has a standing rule
that a gate must be shown failing for the right reason, not merely shown green.

Three arms, all offline (no HA, no websocket, no token beyond a dummy for the module's import-time
`os.environ["HA_TOKEN"]`):

  A. real dashboard + real fleet          -> MUST find the gap (and name the LIVE ones)
  B. same, but this run would add two     -> those two MUST stop being flagged
  C. a fleet the dashboard fully covers   -> MUST report nothing

Arm B is the one that matters for whether anyone keeps the check. Without it the check fires every
time a node joins between deploys — a true statement about a normal event — and a checker that
cries wolf on normal events is one people learn to scroll past. Arm C is the control: a check that
cannot be silent is not measuring anything.

The dashboard fixture is the REAL file when present (`~/Projects/ha/dashboards/…`), falling back to
a synthetic one so this is runnable on a machine that has never deployed. The fallback is built to
the same shape rather than to make the test pass.

⚠️ NOT WIRED INTO tools/gate.sh, deliberately, and recorded here so the next auditor finds a
decision rather than an omission: importing the generator pulls `yaml` and `websockets`, which the
gate's CI image does not install (it deliberately runs no HA-side tooling). Wiring it would trade a
real check for an import error. It runs where the generator runs. If the gate ever grows those deps,
this becomes a one-line arm.

Usage:  ha/dashboard/test_coverage_check.py      (exit 0 pass, 1 fail)
"""
import importlib.util
import json
import os
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
GEN = HERE / "build_control_room.py"

FLEET = {i: {"sigil": s} for i, s in [
    (5, "Obsidian Aegis"), (8, "Eldritch Jewel"), (50, "Mystic Chalice"), (51, "Ashen Vigil"),
    (122, "watch-c6"), (162, "s3-cyd"), (176, "c5-cyd"), (236, "watch-c6"),
]}
LIVE = {5, 8, 50, 51, 162, 176}

# The two ids the real telemetry view carries. Kept as data so a future fix to #356 makes the
# EXPECTATIONS below fail loudly ("fixture setup") rather than the test quietly passing on a
# dashboard that has changed underneath it.
COVERED_BY_REAL_DASHBOARD = {5, 8, 50, 51}


def load_module():
    os.environ.setdefault("HA_TOKEN", "dummy-for-import")
    spec = importlib.util.spec_from_file_location("bcr", GEN)
    mod = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(mod)
    except SystemExit:
        pass  # the module guards on argv/env; the functions we need are already bound
    except ImportError as e:
        print(f"SKIP-IMPOSSIBLE: the generator's imports are unavailable here ({e}).", file=sys.stderr)
        print("  This is a HARD FAIL, not a skip: a test that cannot run must not report success.",
              file=sys.stderr)
        sys.exit(1)
    return mod


def load_dashboard():
    real = pathlib.Path.home() / "Projects/ha/dashboards/smol-mesh.dashboard.json"
    if real.exists():
        return json.loads(real.read_text()), f"real ({real})"
    # Synthetic fallback, same shape: two views, one carrying a couple of nodes.
    return {"views": [
        {"path": "smol-control", "cards": [{"type": "markdown",
         "content": "sensor.smol_5_heap sensor.smol_8_heap sensor.smol_50_heap sensor.smol_51_heap"}]},
        {"path": "smol-telemetry", "cards": [{"type": "entities",
         "entities": [{"entity": "sensor.smol_5_ota_death"}, {"entity": "sensor.smol_8_mac_health"}]}]},
    ]}, "synthetic fallback"


def main():
    mod = load_module()
    if not hasattr(mod, "report_unrepresented"):
        print("FAIL - build_control_room.py has no report_unrepresented (#356) — this test is stale")
        return 1
    cfg, src = load_dashboard()
    print(f"dashboard fixture: {src}")

    covered = {i for i in FLEET if any(
        __import__("re").search(r"smol_%d[_\"]|smol%d[_\"]" % (i, i), json.dumps(v))
        for v in cfg.get("views", []))}
    if covered != COVERED_BY_REAL_DASHBOARD:
        print(f"fixture setup: the dashboard now covers {sorted(covered)}, "
              f"expected {sorted(COVERED_BY_REAL_DASHBOARD)}.")
        print("  If #356 has been FIXED, update COVERED_BY_REAL_DASHBOARD — do not delete the arms.")
        return 1

    fails = 0

    print("\n== A. real fleet vs the live dashboard — must FIND the gap ==")
    a = mod.report_unrepresented(cfg, FLEET, LIVE)
    expect_a = len(FLEET) - len(COVERED_BY_REAL_DASHBOARD)
    if a == expect_a:
        print(f"ok   - reported {a} unrepresented")
    else:
        print(f"FAIL - reported {a}, expected {expect_a}"); fails += 1

    print("\n== B. a pending run would add 162+176 — those must NOT be flagged ==")
    pending = {"path": "smol-control", "cards": [{"type": "markdown",
               "content": "sensor.smol_162_heap sensor.smol_176_heap"}]}
    b = mod.report_unrepresented(cfg, FLEET, LIVE, pending)
    if b == expect_a - 2:
        print(f"ok   - reported {b}; the two this run fixes are excluded")
    else:
        print(f"FAIL - reported {b}, expected {expect_a - 2} — the check would cry wolf on a "
              "node that merely joined since the last deploy"); fails += 1

    print("\n== C. control — a fully covered fleet must report NOTHING ==")
    c = mod.report_unrepresented(cfg, {i: FLEET[i] for i in COVERED_BY_REAL_DASHBOARD},
                                 set(COVERED_BY_REAL_DASHBOARD))
    if c == 0:
        print("ok   - reported 0")
    else:
        print(f"FAIL - reported {c}, expected 0 — a check that cannot be silent measures nothing")
        fails += 1

    print("\n== D. id-boundary — smol_5 must not be satisfied by smol_50 ==")
    only50 = {"views": [{"path": "v", "cards": [{"type": "markdown",
              "content": "sensor.smol_50_heap"}]}]}
    d = mod.report_unrepresented(only50, {5: {"sigil": "five"}}, {5})
    if d == 1:
        print("ok   - id5 correctly reported missing despite smol_50 being present")
    else:
        print("FAIL - id5 was matched by smol_50; the trailing separator is not doing its job, "
              "and the check would report full coverage for a node with none"); fails += 1

    print(f"\n{4 - fails} passed, {fails} failed")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
