#!/usr/bin/env python3
"""#356 regression test — a live node that no card wires must be reported, and a covered one must not.

    python3 ha/dashboard/test_uncovered_nodes.py        # exit 0 = pass

NO HA INSTANCE, NO TOKEN, NO NETWORK. `uncovered_nodes()` is pure on purpose, and this is the
payoff: the finding path runs on every invocation instead of only when someone happens to have a
dashboard in the wrong state. `test_dead_rows.py` needs live HA because it asserts things about
real entities; this needs nothing, and a check that can only be tested against production is a
check that mostly is not tested.

WHY THIS FILE EXISTS. `smol-telemetry` builds per-node tiles from a hardcoded id list (7/8/9). The
fleet moved to other ids and those boards simply had no row — for weeks, with nothing saying so.
The existing audits could not catch it *by construction*: `report_dead_rows` walks the CARDS and
asks what HA is missing, so a node with no card contributes nothing to walk. `DEAD ROWS · 0` and
`VIEWS AUDITED · 2 of 2` were both true and both silent about a live board that was nowhere.

So this asserts BOTH directions, which is the property that actually matters:
  * an uncovered live node IS reported — the check can fail; and
  * a covered one is NOT — the check is not merely reporting everything, which would pass
    direction 1 while being useless.

That second half is not padding. A version of this that returned every node would satisfy "it can
fail" and would cry wolf on every board, which is the fastest way to get a check ignored — the
#338 failure mode this repo already has a name for.

DORMANT NODES DO NOT APPEAR HERE, and that is the design rather than a gap: `main()` splits the
fleet into `nodes` (live) and `dormant` before either is printed, so `uncovered_nodes` is handed
only the live list and never decides liveness itself. See the fixture note below for what happened
when an earlier draft of this test assumed otherwise.
"""
import os, sys, importlib.util

HERE = os.path.dirname(os.path.abspath(__file__))
# The module reads os.environ["HA_TOKEN"] at import time (line ~58) and would KeyError. Nothing is
# dialled — no connection is opened unless main() runs, and it does not.
os.environ.setdefault("HA_TOKEN", "offline-test-not-a-real-token")

FAILURES = []


def check(label, got, want):
    if got == want:
        print(f"  PASS  {label}")
    else:
        print(f"  FAIL  {label}\n          got:  {got}\n          want: {want}")
        FAILURES.append(label)


def load_generator():
    """Import build_control_room without running it."""
    spec = importlib.util.spec_from_file_location("bcr", os.path.join(HERE, "build_control_room.py"))
    m = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, ["test"]      # the module inspects sys.argv for --check/--from-worktree
    try:
        spec.loader.exec_module(m)
    finally:
        sys.argv = argv
    return m


# Shaped like the LIST `main()` passes: one entry per node already judged live, `own` from the
# entity registry and `fw` the resolved firmware entities.
#
# THE LIST IS ALREADY THE LIVE SET. An earlier draft of this test invented a `{id: meta}` dict and
# had `uncovered_nodes` re-filter on `meta["on"]`, and it passed — the fixture encoded my
# assumption about the caller rather than the caller. A live run against real HA raised
# `'list' object has no attribute 'items'` on the first line that touched it. Worse than the crash
# was what the crash hid: `main()` derives `live` from `on` PLUS the crown's roster PLUS the crown
# itself, so re-filtering on `on` would have dropped a C6 watch the crown hears at -35 dBm but
# which owns no hand-written heartbeat sensor. Green test, wrong answer, in the direction #312
# already fixed once.
def node(nid, name, own=(), fw=None):
    return {"id": nid, "name": name, "own": list(own), "fw": fw or {}, "sw": "v922"}


def main():
    bcr = load_generator()
    u = bcr.uncovered_nodes

    # The real shape of the #356 defect: two live boards, one on a tile and one not.
    fleet = [node(8,  "Eldritch Jewel", own=["sensor.smol_8_rssi"]),
             node(51, "Ashen Vigil",    own=["sensor.smol_51_rssi"])]
    wired = {"sensor.smol_8_rssi"}
    check("an uncovered live node is reported", sorted(u(fleet, wired)), [51])
    check("a covered live node is not reported", 8 in u(fleet, wired), False)

    # The cry-wolf guard, expressed the way the caller actually expresses it: a dormant board is
    # simply ABSENT from this list (`main()` puts it in `dormant`), so the check must never be the
    # thing deciding liveness. Passing only live nodes, nothing here should be invented.
    check("a node absent from the live list cannot be reported", u([], set()), {})

    # Coverage may come from either source — `own` (entity registry) or `fw` (resolved firmware
    # entities). Counting only one would report a board that is plainly on the glass.
    fleet_fw = [node(50, "Mystic Chalice", fw={"rssi": "sensor.smol_50_rssi"})]
    check("coverage via the resolved fw entity counts",
          u(fleet_fw, {"sensor.smol_50_rssi"}), {})
    check("...and its absence is still caught",
          sorted(u(fleet_fw, {"sensor.something_else"})), [50])

    # A node with NO entities at all cannot be covered by anything — report it rather than let an
    # empty intersection read as "fine".
    check("a live node owning no entities is reported",
          sorted(u([node(176, "no entities yet")], {"sensor.smol_8_rssi"})), [176])

    # Whole fleet uncovered — the state #356 actually describes.
    check("every live node uncovered when nothing is wired",
          sorted(u(fleet, set())), [8, 51])

    # Empty fleet must not invent a finding.
    check("an empty fleet reports nothing", u([], {"sensor.anything"}), {})

    # Output is id-ordered regardless of input order, because the report prints it verbatim.
    check("result is sorted by node id",
          list(u([node(51, "b"), node(8, "a")], set())), [8, 51])

    # `nodes_covered_by` — the per-source counter behind the informational breakdown. This is the
    # number that actually exposes #356: on live HA it prints `view:smol-telemetry covers 0` while
    # the dashboard-wide UNCOVERED is a clean 0, because `smol-control` carries every node. Both
    # facts are true and only the pair of them is informative.
    c = bcr.nodes_covered_by
    check("counts only nodes with a wired entity", c({"sensor.smol_8_rssi"}, fleet), 1)
    check("a source wiring nothing covers 0", c(set(), fleet), 0)
    check("a source wiring everything covers all",
          c({"sensor.smol_8_rssi", "sensor.smol_51_rssi"}, fleet), 2)
    check("unrelated entities cover nothing", c({"sensor.kitchen_light"}, fleet), 0)

    print()
    if FAILURES:
        print(f"FAILED: {len(FAILURES)} check(s)")
        return 1
    print("test_uncovered_nodes: all checks passed — the inverse audit finds, and does not cry wolf")
    return 0


if __name__ == "__main__":
    sys.exit(main())
