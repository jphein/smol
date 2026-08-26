#!/usr/bin/env python3
"""check_board_consts.py — #419: a board constant that declares a fact nobody reads.

THE TRAP THIS ARMS AGAINST
--------------------------
`targets/c6-watch/src/board/*.rs` is the board-facts seam: `board/mod.rs` re-exports exactly one
board's constants so consumers write `board::LCD_WIDTH` and never name a board. A constant declared
there is a STATEMENT ABOUT THE HARDWARE.

The dangerous shape is a board constant with ZERO readers whose NAME is also declared file-locally
somewhere else. Then two things are true at once: the board says the value is X, and the code that
would use it quietly uses its own Y. Nothing fails, nothing warns, and nothing compiles differently
— a declared constant with zero readers is indistinguishable from a wired one at a glance.

Measured cost of one real instance (in the standalone esp32c6-watch repo, where it is live): a
gesture at dx=-23 dy=31 classified as `Tap` where the board's declared vertical threshold would have
made it `Down`.

This is LATENT in smol today, not live. `board/cyd_c5.rs` here is the early 107-line form and
declares none of the three constants; the colliding declarations live in the standalone repo's
451-line version. **The arming event is a routine subtree refresh** — which is why a tripwire is the
right artifact and a fix is not. #419 filed it at the smol tracker owner's request; the derivation
lives in jphein/esp32c6-watch#91.

WHY NOT THE SHELL ONE-LINER FROM THE ISSUE
------------------------------------------
#419 proposed a `grep -oE 'pub const'` loop. Two things make it report the wrong answer here:

  1. IT COUNTS COMMENTS AS READERS. `ui/slint_shell.rs` refers to `HOLD_SLOP_PX` in a doc comment
     ("drifting past [`HOLD_SLOP_PX`] disarms it") as well as in code. A constant mentioned ONLY in
     prose would read as wired — precisely inverted, since prose about a constant nobody applies is
     the very thing being hunted. So references are counted in CODE ONLY, via the shared
     `tools/rust_comments.py` strip (#426). Not a second stripper: that file exists because this
     repo already collapsed two copies of it into one.

  2. IT HARDCODES `cyd_c5.rs`. There are four board modules (`cyd_c5`, `esp32s3_cyd`,
     `waveshare_c6`, `mod`), and the same trap arms identically in any of them. Board modules and
     targets are GLOB-DISCOVERED, for the reason gate.sh already argues about verifier suites: the
     last list-shaped check silently stopped covering what was added after it.

WHAT FAILS AND WHAT ONLY REPORTS
--------------------------------
  * SHADOWED  — 0 code readers AND the name is declared file-locally elsewhere. This is the
                contradiction. Exit 1.
  * UNREAD    — 0 code readers, no competing declaration. REPORTED, never failed: the board may
                legitimately declare a pin or address whose value is hardcoded at its use site
                (esp32c6-watch#92 tracks 12 such). Failing on these would make the arm fire on a
                dozen innocent constants on day one, and a gate that cries wolf is one people route
                around (#338) — which would take the SHADOWED finding down with it.

Exit: 0 clean · 1 a shadowed board constant · 2 COULD NOT CHECK.
`2` is distinct because "the scan found nothing to scan" must never read as "nothing is wrong".
"""

import glob
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
try:
    from rust_comments import strip_comments
except ImportError:
    sys.stderr.write("CANNOT CHECK: tools/rust_comments.py not importable — this checker counts "
                     "references in CODE ONLY and will not fall back to a comment-blind grep, "
                     "because that inverts the result it exists to produce.\n")
    sys.exit(2)

PUB_CONST_RE = re.compile(r"^\s*pub const ([A-Z][A-Z_0-9]*)\s*:", re.M)
# A file-local declaration of the same name, anywhere outside the board seam. Deliberately matches
# a non-`pub` const too — a private shadow is the more dangerous one, since nothing outside can even
# observe that it exists.
def local_const_re(name):
    return re.compile(r"^\s*(?:pub\s+)?const %s\s*:" % re.escape(name), re.M)


def die2(msg):
    sys.stderr.write("CANNOT CHECK: %s\n" % msg)
    sys.exit(2)


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "..")
    root = os.path.abspath(root)

    board_files = sorted(
        p for p in glob.glob(os.path.join(root, "targets", "*", "src", "board", "*.rs"))
        if os.path.basename(p) != "mod.rs"
    )
    if not board_files:
        die2("found no targets/*/src/board/*.rs. Either the layout moved or the glob is wrong; "
             "either way this check did not run and must not report success.")

    findings, unread, wired_count = [], [], 0

    for board_file in board_files:
        target_root = board_file.split(os.sep + "src" + os.sep)[0]
        target = os.path.basename(target_root)
        board_dir = os.path.dirname(board_file)

        consts = PUB_CONST_RE.findall(open(board_file, encoding="utf-8", errors="replace").read())
        if not consts:
            # Not fatal on its own — a board module may legitimately be a stub — but say so, since
            # "no constants" and "no findings" look identical in a summary line.
            print("  note: %s declares no pub consts" % os.path.relpath(board_file, root))
            continue

        # Every .rs in this target OUTSIDE the board seam, comments stripped once each.
        others = {}
        for path in sorted(glob.glob(os.path.join(target_root, "src", "**", "*.rs"),
                                     recursive=True)):
            if os.path.dirname(path) == board_dir:
                continue
            try:
                others[path] = strip_comments(open(path, encoding="utf-8",
                                                   errors="replace").read())
            except OSError:
                continue
        if not others:
            die2("target %s has a board module but no other .rs files to search. Every constant "
                 "would score zero readers and be reported, which is noise, not a finding." % target)

        for name in sorted(set(consts)):
            word = re.compile(r"\b%s\b" % re.escape(name))
            qualified = re.compile(r"board\s*::\s*%s\b" % re.escape(name))
            shadows = [p for p, code in others.items() if local_const_re(name).search(code)]

            # THE WHOLE CHECK IS HERE, and a bare-identifier search gets it BACKWARDS.
            # `ui/slint_shell.rs` declares its own `const HOLD_SLOP_PX = 24` and uses the bare name
            # at :657. That reference resolves to the FILE-LOCAL const, not to `board::HOLD_SLOP_PX`
            # — so counting bare occurrences scores the shadowing file as a READER of the board
            # constant and reports the trap as wired. Verified against the real armed trap before
            # this line existed: the checker printed "0 SHADOWED", exit 0, with the collision live.
            # Same identifier, two bindings; only the qualified path names the board's one.
            #
            # So: a file that declares its own `const NAME` cannot be a bare reader of the board's
            # NAME. It can still be a reader via the explicit `board::NAME` path, which resolves
            # unambiguously even when a local of the same name exists — so that form always counts.
            readers = [p for p, code in others.items() if qualified.search(code)]
            readers += [p for p, code in others.items()
                        if p not in shadows and p not in readers and word.search(code)]

            if readers:
                wired_count += 1
                continue
            rel_board = os.path.relpath(board_file, root)
            if shadows:
                findings.append((target, name, rel_board,
                                 [os.path.relpath(s, root) for s in shadows]))
            else:
                unread.append((target, name, rel_board))

    print("board-fact seam: %d board module(s), %d constants with code readers, "
          "%d unread, %d SHADOWED" % (len(board_files), wired_count, len(unread), len(findings)))

    if unread:
        print("\n  UNREAD (reported, not failed — a pin/address may be hardcoded at its use site;"
              "\n  esp32c6-watch#92 tracks the benign remainder):")
        for target, name, rel in unread:
            print("    %-28s %s (%s)" % (name, rel, target))

    if not findings:
        return 0

    sys.stderr.write(
        "\nSHADOWED BOARD CONSTANT(S) — a board fact with zero code readers, whose name is\n"
        "declared file-locally elsewhere. The board says one value; the code uses another, and\n"
        "nothing fails or compiles differently (#419):\n")
    for target, name, rel, shadows in findings:
        sys.stderr.write("  %s\n" % name)
        sys.stderr.write("    declared (unread): %s\n" % rel)
        for s in shadows:
            sys.stderr.write("    shadowed by:       %s\n" % s)
    sys.stderr.write(
        "\nFix: wire the consumer to `board::%s`, or delete the board declaration. Do NOT wire a\n"
        "subset — the per-axis SWIPE_MIN and HOLD_SLOP_PX are one bundle, and wiring SWIPE_MIN_Y\n"
        "without HOLD_SLOP_PX makes hold-slop EQUAL the vertical swipe minimum, so at dy=24 the\n"
        "hold stays armed while the gesture also qualifies as a swipe — both true at once, which\n"
        "is the ambiguity the declared invariant exists to prevent (esp32c6-watch#91).\n"
        % findings[0][1])
    return 1


if __name__ == "__main__":
    sys.exit(main())
