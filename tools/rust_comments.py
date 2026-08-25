#!/usr/bin/env python3
r"""One `strip_comments` for every text checker in `tools/`. (#426)

── WHY THIS MODULE EXISTS ────────────────────────────────────────────────────────────────────
Two checkers were found counting matches inside COMMENTS on the same day (2026-08-25), both by
the same route — someone wrote a doc comment about the thing being checked and the checker
counted its own prose. `check_station_consumers.py` (#416) and `check_elect_send_path.py` (#397
STEP B2) each grew a fix, and the fixes were byte-identical code with different docstrings.

The second one's docstring said "Shared implementation with check_station_consumers.py". It was
not shared; it was a copy that said it was shared — a correct comment describing a property the
code did not have, which is the exact defect shape (#371) the checkers exist to catch, appearing
inside the fix for a different instance of itself.

#426's sweep would have made that six copies. So there is one, here, and the callers import it.

── WHAT A COMMENT-BLIND CHECKER COSTS ────────────────────────────────────────────────────────
Both directions are real and the cheap-looking one is not the dangerous one:

  false RED    prose about the invariant trips the gate. Annoying, visible, gets worked around
               by rewording — which leaves the mechanism in place. (#424 did exactly that.)
  false GREEN  a real site is commented out and the count still passes. Silent, and it is an
               ABSENCE check's whole job to notice that. `check_verifier_wiring` was proved to
               report a module SOUND — wired into main.rs — with its `mod` decl inside `/* */`.

── WHICH COMMENT SYNTAX REACHES A CHECKER IS A PROPERTY OF ITS PATTERN, NOT OF THE CHECKER ────
An earlier draft of this header said "`//` is safe everywhere — every Rust-side checker anchors
with `^\s*`". morpheus-391 corrected it, and the counter-example is the checker it wrote:

    RAW_SEND_RE = re.compile(r"esp_now\s*\.\s*send(?:_async)?\s*\(")   # UNANCHORED
    DEVICE_CTOR = "SmolWifiDevice::new"                              # UNANCHORED substring

Both match inside a `///` doc comment, and that is exactly how #397 STEP B1 tripped its own gate.
So the honest statement is narrower:

  * ANCHORED patterns (`^\s*(?:pub )?mod X;`, `^\s*#\[cfg(..)\]`) cannot match a LINE comment —
    the `//` sits where the anchor demands whitespace. They CAN match inside a BLOCK comment,
    which puts the construct at line-start and defeats the anchor. That is the vector that made
    `check_verifier_wiring` report a module SOUND with its `mod` decl inside `/* */`.
  * UNANCHORED patterns match in either kind of comment, with nothing to stop them.

The moment someone adds an unanchored pattern to a checker that reasoned "my patterns are
anchored, so I do not need stripping", `//` becomes a live vector again with no warning. That is
the argument for stripping UNCONDITIONALLY here rather than per-checker reasoning about which
comment syntax can reach which regex — the reasoning is correct today and silently expires.

Practical consequence for anyone auditing a checker: probing with `//` alone can return a clean
bill of health from an anchored-pattern checker that is still vulnerable to `/* */`. Probe both.

── SOME CHECKERS READ COMMENTS ON PURPOSE ────────────────────────────────────────────────────
`check_shed_order` (SHED-ORDER:), `check_diag_budget` (DIAG-WIDTHS:/DIAG-TAIL:) and
`check_byte_free` (`byte-free` claims) take their DECLARATIONS from comments and compare them
against code. For those, comments are load-bearing input, not noise: strip the whole file and
`check_diag_budget` has nothing left to check. Strip for the CODE-side scan only, and pass the
raw text to the declaration scan. Callers are responsible for that split; this module only
blanks comments.
"""

from pathlib import Path


def strip_comments(text: str) -> str:
    """Blank every comment, PRESERVING length and newlines so offsets and line numbers still map.

    Not a nicety — it is load-bearing, and the first run of this script proved it. The roster's own
    doc comment explains the hazard using the words `embassy_net::new(interfaces.station, ..)`, and
    an unstripped scan counted that PROSE as a real call site and failed the gate. A checker whose
    verdict can be flipped by documentation about the thing it checks is worse than no checker: the
    same mechanism that produces a false RED here could be used to produce a false GREEN elsewhere
    (comment out a real site, or bury a roster-shaped string in a comment).

    Handles `//`, `/* */` (nested, as Rust allows), ordinary strings, char literals and raw strings
    (`r"..."`, `r#"..."#`), because a `//` inside a string literal is not a comment and blanking it
    would corrupt the code view.
    """
    out = list(text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        # raw string: r"..." / r#"..."# / r##"..."##
        if c == "r" and i + 1 < n and text[i + 1] in '"#':
            j = i + 1
            hashes = 0
            while j < n and text[j] == "#":
                hashes += 1
                j += 1
            if j < n and text[j] == '"':
                close = '"' + "#" * hashes
                end = text.find(close, j + 1)
                i = n if end < 0 else end + len(close)
                continue
        if c == '"' or c == "'":
            quote = c
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == quote:
                    j += 1
                    break
                if text[j] == "\n" and quote == "'":
                    break  # not a char literal after all (e.g. a lifetime)
                j += 1
            i = j
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            j = n if j < 0 else j
            for k in range(i, j):
                out[k] = " "
            i = j
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            depth, j = 0, i
            while j < n:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                    if depth == 0:
                        break
                else:
                    j += 1
            for k in range(i, min(j, n)):
                if out[k] != "\n":
                    out[k] = " "
            i = j
            continue
        i += 1
    return "".join(out)


def _grep_stripped(pattern: str, root: str) -> int:
    """`rust_comments.py --grep <pattern> <path>` — exit 0 if found in CODE, 1 if not.

    THE SEAM FOR BASH. `tools/status_check.sh` is a shell script and cannot import this module,
    and its `grep-absent` kind had the same defect from the other end: it greps RAW source, so
    #148's `grep-absent ELECT_ENFORCE rust/clock/src` failed the doc because the symbol survives
    as PROSE in `net.rs:121` and `net/election.rs:143` describing the WATCH's flag. The claim was
    true; the check was reading comments.

    Exposing a CLI rather than letting the shell re-implement stripping is the point: a second
    implementation in awk would be a second statement of one fact, which is the duplication this
    module exists to end. One implementation, two callers, one of them over a process boundary.
    """
    p = Path(root)
    # A MISSING PATH IS NOT AN ABSENCE. `rglob` on a nonexistent directory yields nothing and
    # would return "not found in code" — a vacuous pass, and the same shape `status_check.sh`
    # already refuses for `grep-absent` against a path that does not exist. Caught by testing
    # the error case rather than assuming the happy path generalised.
    if not p.exists():
        raise FileNotFoundError(f"no such path: {root}")
    files = [p] if p.is_file() else sorted(p.rglob("*.rs"))
    if not files:
        raise FileNotFoundError(f"no .rs files under {root} — refusing to report an absence")
    for f in files:
        try:
            if pattern in strip_comments(f.read_text(encoding="utf-8", errors="replace")):
                print(f"{f}: found in code")
                return 0
        except OSError:
            continue
    return 1


if __name__ == "__main__":
    import sys as _sys
    # EXIT 2 ON ERROR, NEVER 1. `1` means "absent from code" and a caller acts on it; a crash
    # that also exits 1 is indistinguishable from a real answer. Found the hard way — an
    # unimported `Path` produced a traceback whose exit code read exactly like the verdict I
    # was expecting, and briefly looked like a passing test.
    if len(_sys.argv) == 4 and _sys.argv[1] == "--grep":
        try:
            _sys.exit(_grep_stripped(_sys.argv[2], _sys.argv[3]))
        except Exception as exc:                      # noqa: BLE001 — deliberate: report, do not guess
            print(f"rust_comments: {exc}", file=_sys.stderr)
            _sys.exit(2)
    if len(_sys.argv) == 2:                      # emit the stripped view, for eyeballing
        print(strip_comments(Path(_sys.argv[1]).read_text(encoding="utf-8", errors="replace")), end="")
        _sys.exit(0)
    print("usage: rust_comments.py --grep <pattern> <path> | rust_comments.py <file>", file=_sys.stderr)
    _sys.exit(2)
