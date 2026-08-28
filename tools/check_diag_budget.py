#!/usr/bin/env python3
"""#306: assert the DIAG record's worst-case budget arithmetic against the real format string.

Why this exists. `DIAG_BUDGET` is a CLIFF, not a truncation: `encode_publish` returns `None` when
the payload will not fit, so an over-long record does not lose its tail — it stops publishing
ENTIRELY, and on a fleet with no serial a healthy board then looks dead. `DIAG_CORE_MAX` is the
compile-time proof that the unsheddable core can never reach that cliff.

But that proof is only as good as its own inputs, and its inputs were a HAND-SUMMED comment. On
2026-08-01 the positional term read 220 against a type-provable 228 (`rst=` counted at `brownout`
when `deep-sleep` is the longest token; `led=`'s mode counted below `status`) — 8 B of the bound
did not exist. Nothing had overflowed, because other terms over-counted by more, but an assertion
whose operands drift silently is not an assertion. And the file simultaneously carried THREE
different margins in prose — 51, 23 and the real one — so every reader got a different answer to
"how much room is left".

So the widths are DECLARED in the source, next to the constant, and this proves the declaration and
the format string are the same object:

  * every `|key={}` in the format string has a declared width, in the same ORDER, with the same
    number of placeholders (`led={}:{}` -> `led=6:3`);
  * the declared widths SUM to the positional term of `DIAG_CORE_MAX`;
  * the format-string LITERAL length equals that constant's first term;
  * `DIAG_BUDGET` re-derives from its own expression, and the margin is PRINTED, not asserted to be
    "fine" — a gate that answers "green" instead of a number is how the stale 51 survived.

The PROTECTED tail (the unconditional `rec.push_str` appends) is checked the same way, against a
`DIAG-TAIL:` declaration keyed by each push's first key. That block had drifted too, in the OTHER
direction: it read 19 + 38 + 28 = 85 for appends that need 69, with labels that did not match their
own numbers. An over-count is safe for the cliff but not free — 16 B of unspendable margin is how a
legitimate field gets called unaffordable. Both directions are now caught.

What it still cannot do is derive a WIDTH from a type; the widths are declared. What it guarantees
is that the declaration is COMPLETE and CURRENT — every field in the record has one, nothing
declared has vanished, and the totals are the constant's own terms.

Usage: tools/check_diag_budget.py [path/to/mode.rs]   (exit 0 consistent, 1 drift, 2 malformed)
"""
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_comments import strip_comments  # noqa: E402  (#426)

FMT = re.compile(r'let mut rec = alloc::format!\(\s*\n\s*"(.*?)",\n', re.S)
DECL = re.compile(r"DIAG-WIDTHS:\s*(.+)")
TAIL_DECL = re.compile(r"DIAG-TAIL:\s*(.+)")
# #382: the DERIVED numbers a human reads. Everything above is a declaration of an INPUT, which is
# why the inputs stopped drifting and the read-out did not: `fef377d` (#323) widened `ota=11`->`ota=20`
# and `228`->`237` because this checker refused the build until it did, and left the prose two lines
# above at "core 464 - margin 31 B" because nothing asked. The drift resumed at exactly this
# checker's coverage boundary, in the very next commit to touch the constant. A number in prose is a
# copy of mutable state with no invalidation channel; the fix is to give it one.
DERIVED_DECL = re.compile(r"DIAG-DERIVED:\s*(.+)")
DERIVED_ORDER = ("literal", "positional", "tail", "core", "budget", "margin")
# The unconditional appends live between the PROTECTED-tail banner and the sheddable block. Only
# `rec.push_str` counts: the #181 ledger strings are BUILT in that region but appended via
# `room_for`, so they shed and are correctly outside the bound.
TAIL_START = "PROTECTED tail: appended unconditionally"
TAIL_END = "let mut shed = 0u8;"
PUSH = re.compile(r"rec\.push_str\(\s*(?:&alloc::format!\(\s*)?\"\|([a-z0-9]+)=")
CORE = re.compile(r"const DIAG_CORE_MAX: usize =\s*([0-9+\s]+);")
BUDGET = re.compile(r'const DIAG_BUDGET: usize = (\d+) - (\d+) - "([^"]+)"\.len\(\);')
KEY = re.compile(r"\|([a-z0-9]+)=((?:\{\}:?)+)")


def declared_widths(src: str, pattern=DECL):
    """[(key, [w, ...]), ...] in declaration order, from the DIAG-WIDTHS:/DIAG-TAIL: doc lines."""
    out = []
    for line in pattern.findall(src):
        for tok in line.split():
            if "=" not in tok:
                return None, f"malformed DIAG-WIDTHS token {tok!r} (want key=w or key=w:w)"
            k, _, ws = tok.partition("=")
            try:
                out.append((k, [int(x) for x in ws.split(":")]))
            except ValueError:
                return None, f"malformed width in DIAG-WIDTHS token {tok!r}"
    return out, None


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "rust/clock/src/net/mode.rs"
    try:
        src = open(path).read()
    except OSError as e:
        print(f"FATAL: {e}", file=sys.stderr)
        return 2

    # #426 — TWO VIEWS. The DECLARATIONS (`DIAG-WIDTHS:` / `DIAG-TAIL:`) live in COMMENTS by
    # design and are read from `src`; the CODE they are checked against is read from `code`.
    # Proved necessary: a `/* … rec.push_str("|probe=") … */` written to EXPLAIN the record made
    # this checker fail with `appended unconditionally but UNDECLARED: probe` — a doc comment
    # about the invariant breaking the gate that guards it.
    #
    # Stripping the WHOLE file would be the opposite mistake: the declarations would vanish and
    # this checker would have nothing left to check.
    code = strip_comments(src)
    mf, mc, mb = FMT.search(code), CORE.search(code), BUDGET.search(code)
    for name, m in (("the DIAG format string", mf), ("DIAG_CORE_MAX", mc), ("DIAG_BUDGET", mb)):
        if not m:
            print(f"FATAL: could not find {name} in {path} — this checker has gone blind, fix it.",
                  file=sys.stderr)
            return 2

    fmt = mf.group(1)
    terms = [int(t) for t in mc.group(1).replace(" ", "").split("+") if t]
    if len(terms) < 2:
        print(f"FATAL: DIAG_CORE_MAX has {len(terms)} term(s); expected literal + values + tail.",
              file=sys.stderr)
        return 2
    budget = int(mb.group(1)) - int(mb.group(2)) - len(mb.group(3))

    actual = KEY.findall(fmt)                       # [(key, "{}" | "{}:{}"), ...] in wire order
    decl, err = declared_widths(src)
    if err:
        print(f"FATAL: {err}", file=sys.stderr)
        return 2
    if not decl:
        print(f"FATAL: no DIAG-WIDTHS: declaration found in {path}.", file=sys.stderr)
        return 2

    fail = []

    # 1. the declaration and the format string must be the same list of fields, in the same order.
    a_keys = [k for k, _ in actual]
    d_keys = [k for k, _ in decl]
    if a_keys != d_keys:
        extra = [k for k in a_keys if k not in d_keys]
        gone = [k for k in d_keys if k not in a_keys]
        fail.append("the DIAG-WIDTHS declaration does not match the format string")
        if extra:
            fail.append(f"  in the record but UNDECLARED: {', '.join(extra)}"
                        "  <- add key=<type-max width> to DIAG-WIDTHS")
        if gone:
            fail.append(f"  declared but no longer in the record: {', '.join(gone)}")
        if not extra and not gone:
            fail.append(f"  same fields, different ORDER\n    record:  {' '.join(a_keys)}"
                        f"\n    declared:{' '.join(d_keys)}")
    else:
        for (k, ph), (_, ws) in zip(actual, decl):
            if ph.count("{}") != len(ws):
                fail.append(f"  {k}: {ph.count('{}')} placeholder(s) in the record, "
                            f"{len(ws)} width(s) declared")

    # 2. the widths must sum to the term the constant claims for them.
    total = sum(sum(ws) for _, ws in decl)
    if total != terms[1]:
        fail.append(f"positional value widths sum to {total}, but DIAG_CORE_MAX's second term is "
                    f"{terms[1]} ({total - terms[1]:+d})")

    # 3. the PROTECTED tail: every unconditional push_str must be declared, and vice versa.
    tail, err = declared_widths(src, TAIL_DECL)
    if err:
        print(f"FATAL: {err}", file=sys.stderr)
        return 2
    if not tail:
        print(f"FATAL: no DIAG-TAIL: declaration found in {path}.", file=sys.stderr)
        return 2
    try:
        # The markers are found in RAW (`TAIL_START` is itself comment text) and the slice is
        # taken from CODE. That is exactly why `strip_comments` preserves length and offsets:
        # an index located in one view is valid in the other, so the two never disagree about
        # where the protected tail begins.
        _s = src.index(TAIL_START)
        region = code[_s:src.index(TAIL_END, _s)]
    except ValueError:
        print(f"FATAL: could not delimit the PROTECTED tail in {path} — this checker has gone "
              "blind, fix it.", file=sys.stderr)
        return 2
    pushed = PUSH.findall(region)
    if not pushed:
        print(f"FATAL: found the PROTECTED tail but parsed ZERO push_str appends in {path}.",
              file=sys.stderr)
        return 2
    t_keys = [k for k, _ in tail]
    if pushed != t_keys:
        undeclared = [k for k in pushed if k not in t_keys]
        vanished = [k for k in t_keys if k not in pushed]
        fail.append("the DIAG-TAIL declaration does not match the unconditional appends")
        if undeclared:
            fail.append(f"  appended unconditionally but UNDECLARED: {', '.join(undeclared)}"
                        "  <- a protected field outside the bound is exactly what makes a healthy"
                        " board go silent; add key=<literal+values> to DIAG-TAIL")
        if vanished:
            fail.append(f"  declared but no longer appended: {', '.join(vanished)}")
        if not undeclared and not vanished:
            fail.append(f"  same fields, different ORDER\n    appended:{' '.join(pushed)}"
                        f"\n    declared:{' '.join(t_keys)}")
    t_total = sum(sum(ws) for _, ws in tail)
    if t_total != sum(terms[2:]):
        fail.append(f"protected-tail widths sum to {t_total}, but DIAG_CORE_MAX's remaining terms "
                    f"are {sum(terms[2:])} ({t_total - sum(terms[2:]):+d})")

    # 4. the format-string literal must be the length the first term claims.
    lit = len(fmt) - 2 * sum(p.count("{}") for _, p in actual)
    if lit != terms[0]:
        fail.append(f"format-string literal is {lit} B, but DIAG_CORE_MAX's first term is "
                    f"{terms[0]} ({lit - terms[0]:+d})")

    core = sum(terms)
    margin = budget - core
    if margin < 0:
        fail.append(f"the core does NOT fit: {core} > budget {budget}")

    # 5. #382: the DERIVED read-out must match what was just derived.
    #
    # Checks 1-4 all prove a declared INPUT against the source. None of them looks at the numbers a
    # reader is actually shown, so those went stale the first time an input legitimately moved — and
    # a stale margin is not a cosmetic defect here, it is a WRONG AFFORDANCE. The prose advertised
    # 31 B against a real 22 B, which is enough room to design a 30 B field, pass review against the
    # doc block, and put the core 8 B past a CLIFF that publishes nothing at all.
    #
    # So the read-out is a declaration too, and this proves it. On mismatch we print the corrected
    # line verbatim: a checker that says "wrong" and makes you re-derive by hand is a checker people
    # route around, and the whole failure being fixed here is what happens when a human is the only
    # thing keeping two numbers equal.
    truth = {
        "literal": terms[0],
        "positional": terms[1],
        "tail": sum(terms[2:]),
        "core": core,
        "budget": budget,
        "margin": margin,
    }
    md = DERIVED_DECL.search(src)
    if not md:
        print(f"FATAL: no DIAG-DERIVED: declaration found in {path}. Add this line to the "
              f"DIAG_CORE_MAX doc block:\n    /// DIAG-DERIVED: "
              + " ".join(f"{k}={truth[k]}" for k in DERIVED_ORDER), file=sys.stderr)
        return 2
    got = {}
    for tok in md.group(1).split():
        k, _, v = tok.partition("=")
        if not v.lstrip("-").isdigit() or k not in truth:
            print(f"FATAL: DIAG-DERIVED: has a malformed or unknown term {tok!r}; expected "
                  f"{' '.join(DERIVED_ORDER)}.", file=sys.stderr)
            return 2
        got[k] = int(v)
    missing = [k for k in DERIVED_ORDER if k not in got]
    if missing:
        print(f"FATAL: DIAG-DERIVED: is missing {', '.join(missing)} — a PARTIAL read-out is how "
              "the stale half survives. Declare all six.", file=sys.stderr)
        return 2
    wrong = [k for k in DERIVED_ORDER if got[k] != truth[k]]
    if wrong:
        fail.append("the DIAG-DERIVED read-out no longer matches what it reports on")
        for k in wrong:
            fail.append(f"  {k}: doc says {got[k]}, derived {truth[k]} ({truth[k] - got[k]:+d})")
        fail.append("  replace that line with:\n    /// DIAG-DERIVED: "
                    + " ".join(f"{k}={truth[k]}" for k in DERIVED_ORDER))
        if "margin" in wrong and got["margin"] > truth["margin"]:
            fail.append(f"  ⚠ the doc OVERSTATES the margin by {got['margin'] - truth['margin']} B "
                        "— anyone sizing a new field against it is being told they can afford more "
                        "than exists, and the overrun is a silent fleet-wide publish cliff.")

    if fail:
        print("FATAL: the DIAG budget arithmetic has drifted from the record it describes.",
              file=sys.stderr)
        for line in fail:
            print(f"  {line}", file=sys.stderr)
        print(f"  budget={budget}  core={core}  margin={margin}", file=sys.stderr)
        print("  This record is a CLIFF: over budget, encode_publish publishes NOTHING and a",
              file=sys.stderr)
        print("  healthy board looks dead. Re-derive, don't guess (#306).", file=sys.stderr)
        return 1

    print(f"  DIAG budget={budget}  core={core}  margin={margin} B  "
          f"({len(a_keys)} positional + {len(t_keys)} protected fields, "
          f"{sum(p.count('{}') for _, p in actual)} values, "
          f"literal {lit} B)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
