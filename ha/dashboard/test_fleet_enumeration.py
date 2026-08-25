#!/usr/bin/env python3
"""#425 / #314 — enumeration must not drop a live board, and must REFUSE id42 by name.

    python3 ha/dashboard/test_fleet_enumeration.py        # exit 0 = pass

NO HA, NO TOKEN, NO NETWORK. The classification and the roster cross-check are pure, so the
finding paths run on every invocation rather than only when the registry happens to be untidy.

WHY THIS FILE EXISTS. Enumeration used to require BOTH `model.startswith("smol ")` AND
`fullmatch(r"smol(\\d{1,3})")`. **id176 has two device-registry entries and each was rejected by
the check the other would have passed** — `smol_176` had a good model and an underscore the regex
refused; `smol176` matched the regex with an empty model. A board on the crown's roster, owning 43
entities, was enumerated by nothing: it could not appear even as `dormant`.

And every check downstream is blind to that by construction, because they all start from the
fleet. #421's `UNCOVERED · 0 — all 5 live node(s) are wired` was **true and misleading**: 5 was the
whole fleet it could see, and id176 was not in it.

The scenarios below are the real registry, not inventions — captured 2026-08-25.
"""
import os, sys, importlib.util

HERE = os.path.dirname(os.path.abspath(__file__))
os.environ.setdefault("HA_TOKEN", "offline-test-not-a-real-token")
FAILURES = []


def check(label, got, want):
    if got == want:
        print(f"  PASS  {label}")
    else:
        print(f"  FAIL  {label}\n          got:  {got}\n          want: {want}")
        FAILURES.append(label)


def load():
    spec = importlib.util.spec_from_file_location("bcr", os.path.join(HERE, "build_control_room.py"))
    m = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, ["test"]
    try:
        spec.loader.exec_module(m)
    finally:
        sys.argv = argv
    return m


def classify(m, ident):
    """Mirror of discover_fleet's decision for ONE identifier: ('sentinel'|'accept'|'reject', nid)."""
    loose = m._LOOSE_IDENT_RE.fullmatch(ident)
    if loose and int(loose.group(1)) in m.SENTINEL_IDS:
        return ("sentinel", int(loose.group(1)))
    hit = m._IDENT_RE.fullmatch(ident)
    return ("accept", int(hit.group(1))) if hit else ("reject", None)


def main():
    m = load()

    # ── the id176 case: BOTH spellings must resolve to the same node ──────────────────────────
    # Neither entry alone was enough before; either alone is enough now, which is what stops a
    # board disappearing because one of its two registry rows was tidied away.
    check("smol_176 (underscore, good model) resolves", classify(m, "smol_176"), ("accept", 176))
    check("smol176 (no underscore, EMPTY model) resolves", classify(m, "smol176"), ("accept", 176))

    # ── the id162 regression team-lead asked to pin ───────────────────────────────────────────
    # id162 survived the old code ONLY because it owns a second, well-formed entry (`smol162`).
    # Its `smol_162` entry failed the regex exactly like id176's. So deleting the duplicate as
    # housekeeping — a change that looks like tidying — would have made a live board vanish with
    # no error. Pinned here: the underscore spelling must stand ALONE.
    check("smol_162 ALONE resolves (delete-the-duplicate must not erase it)",
          classify(m, "smol_162"), ("accept", 162))

    # ── #314: the sentinel is refused BY NAME, however it is spelt ────────────────────────────
    # It used to be excluded only because `smolwatch042` matched nothing — true by luck. Widening
    # the acceptance pattern would have turned that accident into an inclusion.
    for spelling in ("smolwatch042", "smol42", "smol_42"):
        check(f"{spelling} is refused as the sentinel", classify(m, spelling), ("sentinel", 42))
    check("42 carries a stated reason", "sentinel" in m.SENTINEL_IDS[42], True)

    # ── ordinary boards and non-candidates ────────────────────────────────────────────────────
    check("smol8 resolves", classify(m, "smol8"), ("accept", 8))
    check("a non-numeric smol id is rejected", classify(m, "smolxyz"), ("reject", None))
    check("a 4-digit tail is not a node id", classify(m, "smol1234"), ("reject", None))

    # ── the roster cross-check: enumeration's own blind spot ──────────────────────────────────
    ros = {50: {"ch": 6, "peers": {8: {}, 51: {}, 162: {}, 176: {}, 236: {}}}}
    check("a roster peer enumeration never produced is reported",
          m.roster_orphans(ros, {8: {}, 51: {}, 162: {}, 236: {}}), {50: [176]})
    check("nothing reported when the fleet covers the roster",
          m.roster_orphans(ros, {8: {}, 51: {}, 162: {}, 176: {}, 236: {}}), {})
    check("an empty roster invents nothing", m.roster_orphans({}, {8: {}}), {})
    check("a crown with no peers invents nothing",
          m.roster_orphans({5: {"ch": 0, "peers": {}}}, {}), {})
    # Two crowns can be retained on the broker at once (a ghost roster from a former crown), so
    # orphans are reported PER CROWN rather than merged — the operator needs to know which crown
    # is doing the talking before deciding whether a peer matters.
    check("orphans are attributed per crown",
          m.roster_orphans({5: {"ch": 0, "peers": {9: {}}}, 50: {"ch": 6, "peers": {176: {}}}}, {}),
          {5: [9], 50: [176]})

    # ── name selection between duplicates ─────────────────────────────────────────────────────
    # This one is here because the first draft SHIPPED THE REGRESSION and only a live run caught
    # it: taking the first registry entry's name rendered id162 as `cyd` (a hand-typed label on
    # `smol_162`) instead of `Argent Brazier` (the firmware sigil on `smol162`). Registry order is
    # not an authority. The rule is "the entry HA actually populates wins", which is a fact about
    # the registry rather than a guess about what a name looks like — a string heuristic would
    # break the first time a sigil word is lowercase.
    def pick(names, counts):
        best = max(names, key=lambda dn: counts.get(dn[0], 0))
        named = [n for _, n in names if not n.startswith("unnamed")]
        return best[1] if not best[1].startswith("unnamed") or not named else named[0]

    check("the entry owning the entities supplies the name",
          pick([("dev_cyd", "cyd"), ("dev_sigil", "Argent Brazier")],
               {"dev_cyd": 0, "dev_sigil": 43}), "Argent Brazier")
    check("...and order does not decide it",
          pick([("dev_sigil", "Argent Brazier"), ("dev_cyd", "cyd")],
               {"dev_cyd": 0, "dev_sigil": 43}), "Argent Brazier")
    check("an entity-less node still gets a real name over 'unnamed'",
          pick([("a", "unnamed id176"), ("b", "Luminous Ember")], {}), "Luminous Ember")
    check("ties keep registry order",
          pick([("a", "First"), ("b", "Second")], {"a": 5, "b": 5}), "First")

    print()
    if FAILURES:
        print(f"FAILED: {len(FAILURES)} check(s)")
        return 1
    print("test_fleet_enumeration: all checks passed — no live board is dropped, and 42 is refused by name")
    return 0


if __name__ == "__main__":
    sys.exit(main())
