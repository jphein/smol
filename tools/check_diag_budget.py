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

It deliberately does NOT check the three protected-tail terms (19 + 38): those are hand-measured
against push_str literals and are conservative, so they can only make the bound stricter. Anyone
wanting that ~18 B back must derive it the same machine-checked way first.

Usage: tools/check_diag_budget.py [path/to/mode.rs]   (exit 0 consistent, 1 drift, 2 malformed)
"""
import re
import sys

FMT = re.compile(r'let mut rec = alloc::format!\(\s*\n\s*"(.*?)",\n', re.S)
DECL = re.compile(r"DIAG-WIDTHS:\s*(.+)")
CORE = re.compile(r"const DIAG_CORE_MAX: usize =\s*([0-9+\s]+);")
BUDGET = re.compile(r'const DIAG_BUDGET: usize = (\d+) - (\d+) - "([^"]+)"\.len\(\);')
KEY = re.compile(r"\|([a-z0-9]+)=((?:\{\}:?)+)")


def declared_widths(src: str):
    """[(key, [w, ...]), ...] in declaration order, from the DIAG-WIDTHS: doc lines."""
    out = []
    for line in DECL.findall(src):
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

    mf, mc, mb = FMT.search(src), CORE.search(src), BUDGET.search(src)
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

    # 3. the format-string literal must be the length the first term claims.
    lit = len(fmt) - 2 * sum(p.count("{}") for _, p in actual)
    if lit != terms[0]:
        fail.append(f"format-string literal is {lit} B, but DIAG_CORE_MAX's first term is "
                    f"{terms[0]} ({lit - terms[0]:+d})")

    core = sum(terms)
    margin = budget - core
    if margin < 0:
        fail.append(f"the core does NOT fit: {core} > budget {budget}")

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
          f"({len(a_keys)} positional fields, {sum(p.count('{}') for _, p in actual)} values, "
          f"literal {lit} B)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
