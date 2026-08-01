#!/usr/bin/env python3
"""#333 regression test — the DEAD ROWS audit must see rows inside PRESERVED live-only cards.

    HA_TOKEN=<llat> python3 ha/dashboard/test_dead_rows.py        # exit 0 = pass

WHY THIS FILE EXISTS. `report_dead_rows()` used to audit `view["cards"]` alone — the cards a real
run WRITES — while `classify()` returned the preserved live-only cards separately and nobody passed
them in. So on 2026-08-01 it printed `DEAD ROWS · 0` while four dead rows were rendering on JP's
dashboard inside a preserved card wired to placeholder ids that have never existed in this HA.

The fix is small. The thing worth protecting is that **the check can actually fail**, because this
codebase's recurring defect is a gate that answers a narrower question than it appears to and is
therefore never observed failing. Four instances in one session (a field-name grep that missed a
bare list item; a silenced FATAL read as a quiet fleet; a watcher outrun by its own trigger; a
fan-out test published onto the value the board already held). A check nobody has watched fail is
indistinguishable from one that cannot.

So this asserts BOTH directions on live HA state:
  * the real historical card that fooled the old audit now trips the new one, and
  * the old scope (extras omitted) still misses it — i.e. the gap was real and is what closed.

Fixture is the ACTUAL card from the incident, not an invention: `vertical-stack|sha:19daffe159d0`,
retired via RETIRE_LIVE on 2026-08-01. Its four entities are asserted absent from live HA, so if
somebody ever creates a `sensor.house_load` this test says so instead of silently passing.
"""
import json, os, sys, urllib.request, importlib.util

HERE = os.path.dirname(os.path.abspath(__file__))
TOKEN = os.environ.get("HA_TOKEN") or sys.exit("FATAL: set HA_TOKEN")
BASE = os.environ.get("HA_BASE", "https://ha.jphe.in")


def api(path):
    req = urllib.request.Request(f"{BASE}{path}", headers={"Authorization": f"Bearer {TOKEN}"})
    return json.loads(urllib.request.urlopen(req, timeout=30).read())


def load_generator():
    """Import build_control_room without running it. It reads HA_TOKEN at import time."""
    spec = importlib.util.spec_from_file_location("bcr", os.path.join(HERE, "build_control_room.py"))
    m = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, ["test"]          # the module inspects sys.argv for --check/--from-worktree
    try:
        spec.loader.exec_module(m)
    finally:
        sys.argv = argv
    return m


# The card from the 2026-08-01 incident, verbatim in shape: a pre-#305 power/solar stack wired to
# four placeholder ids. Nested two levels deep inside a vertical-stack on purpose — the audit walks
# structurally, and a shallow scan would miss exactly this.
PLACEHOLDERS = ["sensor.battery_bank_soc", "sensor.ev_battery_soc",
                "sensor.house_load", "sensor.solar_charge_current"]
INCIDENT_CARD = {
    "type": "vertical-stack",
    "view_layout": {"grid-column": "span 4"},
    "cards": [
        {"type": "horizontal-stack", "cards": [
            {"type": "gauge", "entity": PLACEHOLDERS[0], "name": "battery bank"},
            {"type": "gauge", "entity": PLACEHOLDERS[1], "name": "EV HV"}]},
        {"type": "entities", "title": "power & solar · sources on the glass",
         "entities": [{"entity": PLACEHOLDERS[2], "name": "house load"},
                      {"entity": PLACEHOLDERS[3], "name": "solar charge"}]},
    ],
}

fails = []


def check(label, cond, detail=""):
    print(f"  {'PASS' if cond else 'FAIL'}  {label}{('  — ' + detail) if detail else ''}")
    if not cond:
        fails.append(label)


def main():
    m = load_generator()
    st = {e["entity_id"]: e for e in api("/api/states")}
    print(f"live HA: {len(st)} entities\n")

    print("0 · fixture is still a valid negative (none of the four may exist in HA)")
    for e in PLACEHOLDERS:
        check(f"{e} absent from live HA", e not in st,
              "" if e not in st else "IT EXISTS NOW — pick new placeholder ids for this fixture")

    print("\n1 · THE BUG: old scope (built cards only) does NOT see the dead rows")
    empty_view = {"cards": []}
    old = m.report_dead_rows(empty_view, st)                     # extras omitted == pre-#333 behaviour
    check("old scope reports 0 dead rows", len(old) == 0, f"got {len(old)}")

    print("\n2 · THE FIX: preserved live-only cards are in scope, and it goes RED")
    new = m.report_dead_rows(empty_view, st, extras=[INCIDENT_CARD])
    check("new scope reports 4 dead rows", len(new) == 4, f"got {len(new)}: {sorted(new)}")
    check("all four are the placeholders", sorted(new) == sorted(PLACEHOLDERS))
    check("all tagged kind=absent", all(k == "absent" for k, _, _ in new.values()),
          str({e: v[0] for e, v in new.items()}))
    check("all tagged source=live-only", all(s == {"live-only"} for _, _, s in new.values()),
          str({e: sorted(v[2]) for e, v in new.items()}))

    print("\n3 · sources are distinguished, because they imply different fixes")
    both = m.report_dead_rows({"cards": [dict(INCIDENT_CARD)]}, st, extras=[INCIDENT_CARD])
    check("a row in BOTH sets is tagged both", all(s == {"built", "live-only"} for _, _, s in both.values()),
          str({e: sorted(v[2]) for e, v in both.items()}))

    print("\n4 · GREEN AGAIN: remove the bad card and the audit clears")
    clean = m.report_dead_rows(empty_view, st, extras=[])
    check("no extras → 0 dead rows", len(clean) == 0, f"got {len(clean)}")

    print("\n5 · a HEALTHY live-only card must NOT be flagged (no crying wolf)")
    live_ent = next((e for e, s in st.items()
                     if e.startswith("sensor.") and s["state"] not in ("unavailable", "unknown")
                     and not (s.get("attributes") or {}).get("restored")), None)
    check("found a healthy sensor to test with", live_ent is not None, str(live_ent))
    if live_ent:
        ok = m.report_dead_rows(empty_view, st,
                                extras=[{"type": "entities", "title": "healthy",
                                         "entities": [{"entity": live_ent}]}])
        check("healthy live-only card → 0 dead rows", len(ok) == 0, f"got {sorted(ok)}")

    print()
    if fails:
        print(f"FAILED ({len(fails)}): " + "; ".join(fails))
        return 1
    print("all assertions passed — the audit sees preserved cards AND can still go green")
    return 0


if __name__ == "__main__":
    sys.exit(main())
