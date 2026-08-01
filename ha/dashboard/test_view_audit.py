#!/usr/bin/env python3
"""#340 regression test — the DEAD ROWS audit must cover EVERY view, not just the generated one.

    HA_TOKEN=<llat> python3 ha/dashboard/test_view_audit.py        # exit 0 = pass

WHY THIS FILE EXISTS. `classify()` resolves ONE view by path and `report_check` printed one view's
numbers, so every other Lovelace view on the dashboard was audited by nothing. Measured cost:
`sensor.smol_{7,9}_{mac_health,ota_death}` were wired as structured rows in the `smol-telemetry`
view and rendered `unavailable` on JP's dashboard for as long as ids 7 and 9 have been retired,
while `--check` printed `DEAD ROWS · 0`. The check built to find exactly that could not see it.

This is the THIRD face of #333, and the shape of all three is the same: the audit answered a
narrower question than it appeared to. Face 1 was scope within a view (preserved cards omitted),
face 2 was timing (it ran only before a change), face 3 is scope across views. So the property this
file protects is not "the audit is correct today" but **the audit cannot be narrowed silently**:

  * a husk on a NON-PRIMARY view trips it (and the old single-view scope proves it did not),
  * ADDING A VIEW enrolls that view with no code change — the enumeration is over `cfg["views"]`
    itself, so there is no list to forget to update,
  * an unrecognised view SHAPE is still audited, because a `sections` view or some future layout
    keeps its cards somewhere this file has never heard of,
  * and it still goes GREEN, and still does not cry wolf on healthy entities.

The last two matter as much as the first. A gate nobody has watched fail is indistinguishable from
one that cannot fail — and a gate that fires on healthy state gets ignored, which is the same
outcome by a slower route.
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


# Ids that must not exist in this HA. Asserted below rather than assumed, so if somebody ever
# creates one of them this test says so instead of silently passing on a fixture that stopped
# being a negative (test_dead_rows.py learned the same lesson).
HUSK = "sensor.smol_340_canary_husk"
HUSK2 = "sensor.smol_340_second_husk"

fails = []


def check(label, cond, detail=""):
    print(f"  {'PASS' if cond else 'FAIL'}  {label}{('  — ' + detail) if detail else ''}")
    if not cond:
        fails.append(label)


def telemetry_like(path, entity):
    """A non-primary view shaped like the real `smol-telemetry`: the husk sits two levels deep
    inside a horizontal-stack, because the audit walks structurally and a shallow scan would miss
    exactly this arrangement — which is the arrangement the real four husks are in."""
    return {"path": path, "title": f"smol · {path}", "icon": "mdi:chart-line",
            "cards": [{"type": "markdown", "content": "## telemetry"},
                      {"type": "horizontal-stack", "cards": [
                          {"type": "custom:mushroom-template-card", "entity": entity,
                           "primary": "{{ states('sensor.whatever') }}"}]}]}


def main():
    m = load_generator()
    st = {e["entity_id"]: e for e in api("/api/states")}
    print(f"live HA: {len(st)} entities\n")

    print("0 · fixtures are still valid negatives (neither husk may exist in HA)")
    for e in (HUSK, HUSK2):
        check(f"{e} absent from live HA", e not in st,
              "" if e not in st else "IT EXISTS NOW — pick a new id for this fixture")

    # The generated view, healthy: nothing wired to anything missing. Every finding below therefore
    # comes from the OTHER view, which is the whole point.
    built = {"cards": [{"type": "markdown", "content": "BANNER filled"}]}
    primary = {"path": m.VIEW_PATH, "title": "smol · Control Room", "cards": list(built["cards"])}
    cfg = {"views": [primary, telemetry_like("smol-telemetry", HUSK)]}

    print("\n1 · THE BUG: the old single-view scope does NOT see a husk on another view")
    old = m.report_dead_rows(built, st, extras=[])
    check("old scope reports 0 dead rows", len(old) == 0, f"got {sorted(old)}")

    print("\n2 · THE FIX: every view is audited, and it goes RED")
    dead, audited = m.audit_views(cfg, built, primary, [], st)
    check("new scope reports 1 dead row", len(dead) == 1, f"got {sorted(dead)}")
    check("it is the husk", sorted(dead) == [HUSK])
    if HUSK in dead:
        kind, idents, src = dead[HUSK]
        check("tagged kind=absent", kind == "absent", kind)
        check("tagged with the view it is ON", src == {"view:smol-telemetry"}, str(sorted(src)))
        check("names the card, not just the view",
              any("horizontal-stack" in i for i in idents), str(sorted(idents)))

    print("\n3 · COVERAGE is reported per view, and 'N of N' is a fact not a hope")
    check("one audited row per view", len(audited) == len(cfg["views"]), f"got {len(audited)}")
    check("the generated view is audited as built+live-only",
          "built" in audited[0][1], audited[0][1])
    check("the other view is audited as live", "live" in audited[1][1], audited[1][1])

    print("\n4 · THE PROPERTY: adding a view enrolls it with NO code change")
    #     This is the assertion that makes #340 unrepeatable. If a future view has to be added to a
    #     list somewhere, this fails — which is the only cheap way to notice that the enumeration
    #     stopped being an enumeration.
    grown = {"views": cfg["views"] + [telemetry_like("smol-newview", HUSK2)]}
    dead3, audited3 = m.audit_views(grown, built, primary, [], st)
    check("audited count follows the view count", len(audited3) == 3, f"got {len(audited3)}")
    check("the brand-new view's husk is found", HUSK2 in dead3, f"got {sorted(dead3)}")
    check("both husks now red", len(dead3) == 2, f"got {sorted(dead3)}")
    check("the new view is named in its own tag",
          dead3.get(HUSK2, (None, None, set()))[2] == {"view:smol-newview"},
          str(sorted(dead3.get(HUSK2, (None, None, set()))[2])))

    print("\n5 · SHAPES: cards do not always live under view['cards']")
    #     HA `sections` views nest cards one level further down, badges are their own list, and a
    #     custom layout can put them somewhere this file has never heard of. A view shape is just
    #     another way for coverage to be quietly narrower than it looks.
    sect = {"path": "sect", "type": "sections",
            "sections": [{"type": "grid", "cards": [{"type": "tile", "entity": HUSK}]}]}
    d, _ = m.audit_views({"views": [sect]}, built, None, [], st)
    check("a sections-view card is audited", HUSK in d, f"got {sorted(d)}")

    badge = {"path": "badge", "cards": [], "badges": [{"type": "entity", "entity": HUSK}]}
    d, _ = m.audit_views({"views": [badge]}, built, None, [], st)
    check("a badge is audited", HUSK in d, f"got {sorted(d)}")

    weird = {"path": "weird", "type": "custom:some-future-layout",
             "panels": [{"cards": [{"type": "tile", "entity": HUSK}]}]}
    d, _ = m.audit_views({"views": [weird]}, built, None, [], st)
    check("an UNKNOWN view shape is still audited (fail-closed backstop)", HUSK in d, f"got {sorted(d)}")

    print("\n6 · prev=None must still audit — it used to `return 0` having looked at nothing")
    d, a = m.audit_views({"views": [telemetry_like("only", HUSK)]}, built, None, [], st)
    check("a dashboard with no Control Room still goes red", HUSK in d, f"got {sorted(d)}")
    check("the built view is audited too", any("built" in how for _, how, _ in a), str(a))

    print("\n7 · GREEN AGAIN: unwire the row and the audit clears")
    clean = {"views": [primary, telemetry_like("smol-telemetry", "sensor.smol_mesh_channel")]}
    d, a = m.audit_views(clean, built, primary, [], st)
    check("0 dead rows once the card is repointed", len(d) == 0, f"got {sorted(d)}")
    check("still audited both views while green", len(a) == 2, f"got {len(a)}")

    print("\n8 · NO CRYING WOLF: a healthy entity on a non-primary view is not a finding")
    live_ent = next((e for e, s in st.items()
                     if e.startswith("sensor.") and s["state"] not in ("unavailable", "unknown")
                     and not (s.get("attributes") or {}).get("restored")), None)
    check("found a healthy sensor to test with", live_ent is not None, str(live_ent))
    if live_ent:
        d, _ = m.audit_views({"views": [primary, telemetry_like("t", live_ent)]}, built, primary, [], st)
        check("healthy row on another view → 0 dead rows", len(d) == 0, f"got {sorted(d)}")

    print("\n9 · TEXT SEARCH FAILS BOTH WAYS — the resolver must be structural")
    #     Measured 2026-08-01, both directions, same afternoon: a raw-JSON grep of this very view
    #     returned FOUR FALSE hits out of Jinja bodies, and a literal grep for constructed ids
    #     reported "nothing references these" while two dead rows shipped to JP's dashboard.
    jinja_only = {"path": "j", "cards": [{"type": "markdown",
                  "content": "{% set s = states('" + HUSK + "') %}{{ s }}"}]}
    d, _ = m.audit_views({"views": [jinja_only]}, built, None, [], st)
    check("a husk mentioned ONLY in Jinja is not a dead row (no false hit)", HUSK not in d,
          f"got {sorted(d)}")
    constructed = {"path": "c", "cards": [{"type": "entities",
                   "entities": [{"entity": f"sensor.smol_{340}_canary_husk"}]}]}
    d, _ = m.audit_views({"views": [constructed]}, built, None, [], st)
    check("a CONSTRUCTED id is resolved by construction (no miss)", HUSK in d, f"got {sorted(d)}")

    print()
    if fails:
        print(f"FAILED ({len(fails)}): " + "; ".join(fails))
        return 1
    print("all assertions passed — every view is audited, adding one enrolls it, and it still greens")
    return 0


if __name__ == "__main__":
    sys.exit(main())
