#!/usr/bin/env python3
"""#338/#339: assert the DIAG shed order in code matches the order declared beside it.

Why this exists. `diag_record`'s sheddable tail appends fields with `room_for` while budget lasts,
so APPEND POSITION IS SHED PRIORITY: the last field offered is the first to disappear. A comment
above the block described that order in prose — and the prose was WRONG. It named `cc`/`io` as
"first to go (least missed)", but `cc` is appended 4th and so survives LONGER than `ap`, `cfg` and
`io`. An operator who lost `ap=` and trusted the comment would go looking on exactly the fields the
#204/#217 coexist diagnosis rests on.

Prose cannot be tested, so it drifted. This makes the order a machine-checked fact: the source
carries a single `SHED-ORDER:` declaration, and this script proves the actual `room_for` sequence
equals it. Reorder the appends without updating the declaration (or vice versa) and the gate goes
red. Same principle as printing the stack number — put the fact where a machine checks it.

It deliberately does NOT judge whether the order is *correct* — that's the open question in #339,
a call about diagnosis priorities. This only guarantees the documented order and the real one are
the same thing, so #339 can be argued about a fact rather than a stale comment.

Usage: tools/check_shed_order.py [path/to/mode.rs]   (exit 0 match, 1 mismatch, 2 malformed)
"""
import re
import sys

DECL = re.compile(r"SHED-ORDER:\s*([A-Za-z0-9_,\s]+?)\s*(?:\*/|\n|$)")


def extract_block(src: str) -> str:
    """The sheddable tail: from the `shed` counter to where the counter is reported."""
    start = src.index("let mut shed = 0u8;")
    end = src.index("if shed > 0", start)
    return src[start:end]


def appended_fields(block: str):
    """Field keys in APPEND ORDER (= reverse shed priority).

    A `room_for` call contributes either the first key of its format literal
    (`alloc::format!("|cc={}|degraded={}"` -> `cc`) or, when the payload is a prebuilt string, the
    variable's name (`room_for(&mut rec, lg_core)` -> `lg_core`). Covering the variable form matters:
    #181's ledger fields are built above the block and passed by value, so a literal-only parser
    would silently not cover the very fields whose shed priority prompted this check.
    """
    out = []
    for m in re.finditer(r"room_for\s*\(", block):
        tail = block[m.end():m.end() + 400]
        lit = re.search(r'alloc::format!\s*\(\s*"\|([A-Za-z0-9_]+)=', tail)
        var = re.match(r"\s*&mut\s+rec\s*,\s*([a-z_][A-Za-z0-9_]*)\s*\)", tail)
        if lit and (not var or lit.start() < var.end()):
            out.append(lit.group(1))
        elif var:
            out.append(var.group(1))
    return out


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "rust/clock/src/net/mode.rs"
    src = open(path, encoding="utf-8").read()

    m = DECL.search(src)
    if not m:
        print(f"FATAL: no `SHED-ORDER:` declaration found in {path}.", file=sys.stderr)
        print("       The shed order must be declared where it can be checked — see this script's", file=sys.stderr)
        print("       docstring for why prose alone was not enough.", file=sys.stderr)
        return 2
    declared = [t for t in re.split(r"[,\s]+", m.group(1)) if t]

    try:
        block = extract_block(src)
    except ValueError:
        # FAIL CLOSED: if the block moved or was renamed, this check silently covering nothing is
        # strictly worse than it failing loudly — that is how the prose rotted in the first place.
        print(f"FATAL: could not locate the sheddable tail in {path} (markers moved?).", file=sys.stderr)
        return 2

    actual = appended_fields(block)
    if not actual:
        print(f"FATAL: found the shed block but parsed ZERO room_for appends in {path}.", file=sys.stderr)
        return 2

    if actual == declared:
        print(f"  shed order: {' -> '.join(actual)}  ({len(actual)} fields, first-shed last)")
        return 0

    print("FATAL: the DIAG shed order does not match its declaration.", file=sys.stderr)
    print(f"  declared (SHED-ORDER:): {' -> '.join(declared)}", file=sys.stderr)
    print(f"  actual   (room_for  ): {' -> '.join(actual)}", file=sys.stderr)
    extra = [f for f in actual if f not in declared]
    missing = [f for f in declared if f not in actual]
    if extra:
        print(f"  appended but not declared: {', '.join(extra)}", file=sys.stderr)
    if missing:
        print(f"  declared but not appended: {', '.join(missing)}", file=sys.stderr)
    if not extra and not missing:
        print("  same fields, DIFFERENT ORDER — which changes what is lost first under budget", file=sys.stderr)
    print("  Append position IS shed priority: the LAST field offered is the FIRST dropped.", file=sys.stderr)
    print("  Update the SHED-ORDER declaration if the change is intended (see #339).", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
