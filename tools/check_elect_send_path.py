#!/usr/bin/env python3
"""#278/#269: prove the ELECT announcement can only reach the air AUTHENTICATED.

Why this exists, and why a comment was not enough.

A leaf cannot verify a channel-change announcement before acting on it. esp-radio 0.18 hardcodes
`coex_background_scan: false`, so a scan DROPS the association — checking an announcement costs the
leaf the very association it would need if the announcement were false. It has to trust the frame.
That makes an unauthenticated `SMOLv1 ELECT` a remote fleet-stranding primitive: broadcast
"everybody move to channel N" and the fleet goes, with no cheap way for any leaf to detect the lie.

#190's group-MAC trailer closes it, but ONLY on the `send_to` path. `send_arb_raw` deliberately does
not append the trailer (correct for the #237 OTA arbitration frames it exists for), so routing ELECT
that way silently converts a safe channel hop into a remote fleet partition. Stage 1 wrote that down
as a DESIGN INVARIANT in prose, and prose would have survived any refactor that violated it.

So the invariant is structural first — `SealedElect` has no byte accessor and can only be handed to
a `GroupMacSink` — and this script covers the shapes the type system cannot see. Each arm exists
because it was ENUMERATED as a way to satisfy the types and still ship the bug:

  1. impl-body      the one-line `GroupMacSink` body is rewritten to another sender. THE likeliest
                    shape: a one-line edit, in the direction of "make it compile", that looks like a
                    simplification and passes every test.
  2. impl-count     a SECOND `GroupMacSink` impl appears that sends raw.
  3. no-accessor    `SealedElect` grows an `as_bytes`/`pub` field, and the seal stops sealing.
  4. one-encoder    `wire::encode` is called somewhere other than `SealedElect::seal`, so a caller
                    gets the frame bytes without ever touching the seal.
  5. no-hand-build  the `SMOLv1 ELECT ` literal is written a second time, building the frame by hand
                    and bypassing the encoder entirely (which arm 4 would not see).
  6. raw-sends      a new raw `esp_now.send` call site appears. Declared IN THE SOURCE, checked in
                    BOTH directions with counts, so adding a send to an already-listed function
                    fails too.

It deliberately does NOT check that `send_to` appends the trailer — that is `should_group_mac`, and
it is pinned by an ELECT case in `experiments/mac_verify` where the decision is pure and host-tested.
Two mechanisms, two places, neither able to silence the other.

Every arm FAILS CLOSED: if an anchor cannot be found, this exits 2 rather than passing. A check that
quietly stops covering anything is how the prose rotted in the first place.

Usage: tools/check_elect_send_path.py [repo-root]   (exit 0 ok, 1 violation, 2 malformed)
"""
import re
import sys
from pathlib import Path

# The frame's own prefix, spelled here so a rename of the const cannot silently empty arm 5.
ELECT_LITERAL = 'b"SMOLv1 ELECT "'
SINK_TRAIT = "GroupMacSink"
# Senders that must NEVER appear in the sink impl. `send_to` is the required one and is handled
# separately — this is the deny-list of everything that skips the trailer.
FORBIDDEN_SENDERS = ("send_arb_raw", "esp_now.send", "esp_now .send")
DECL = re.compile(r"RAW-SEND-SITES:\s*([A-Za-z0-9_:,\s]+?)\s*(?:\*/|\n|$)")


def fail(msg, *extra):
    print(f"FATAL: {msg}", file=sys.stderr)
    for line in extra:
        print(f"  {line}", file=sys.stderr)


def rust_sources(src: Path):
    return sorted(p for p in src.rglob("*.rs") if p.is_file())


def enclosing_fn(text: str, idx: int):
    """Name of the nearest `fn` declared at or above `idx`. None if there isn't one."""
    best = None
    for m in re.finditer(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?fn\s+(\w+)", text[:idx], re.M):
        best = m.group(1)
    return best


def block_after(text: str, start: int):
    """The brace-balanced block that opens at or after `start`. None if unbalanced."""
    open_at = text.find("{", start)
    if open_at < 0:
        return None
    depth = 0
    for i in range(open_at, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at : i + 1]
    return None


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    src = root / "rust" / "clock" / "src"
    if not src.is_dir():
        fail(f"no firmware source tree at {src}")
        return 2
    files = {p: p.read_text(encoding="utf-8") for p in rust_sources(src)}
    if not files:
        fail(f"parsed ZERO rust sources under {src}")
        return 2

    bad = []
    notes = []

    # ── arms 1 + 2: the sink impl(s) ──────────────────────────────────────────────────────────
    impls = []
    for path, text in files.items():
        for m in re.finditer(rf"impl\s+(?:[\w:]+::)?{SINK_TRAIT}\s+for\s+(\w+)", text):
            body = block_after(text, m.end())
            if body is None:
                fail(f"unbalanced braces after the {SINK_TRAIT} impl in {path.relative_to(root)}")
                return 2
            impls.append((path, m.group(1), body))

    if not impls:
        fail(
            f"no `impl {SINK_TRAIT} for …` found anywhere in {src.relative_to(root)}.",
            "The ELECT send path is gone or was renamed. This check cannot prove an absent",
            "invariant, so it refuses to pass — see this script's docstring.",
        )
        return 2
    if len(impls) > 1:
        where = ", ".join(f"{p.relative_to(root)}:{t}" for p, t, _ in impls)
        bad.append(
            f"arm 2 (impl-count): {len(impls)} `{SINK_TRAIT}` implementations ({where}). "
            "Exactly one may exist — a second is a second send path, which is the whole thing "
            "this invariant forbids."
        )

    for path, target, body in impls:
        rel = path.relative_to(root)
        if "send_to" not in body:
            bad.append(
                f"arm 1 (impl-body): `{SINK_TRAIT} for {target}` ({rel}) does NOT route to "
                "`send_to`. That is the ONLY path appending #190's group-MAC trailer; anything "
                "else emits an unauthenticated ELECT, which is a remote fleet-stranding primitive."
            )
        for s in FORBIDDEN_SENDERS:
            if s in body:
                bad.append(
                    f"arm 1 (impl-body): `{SINK_TRAIT} for {target}` ({rel}) names `{s}`, which "
                    "does not append the group-MAC trailer."
                )
    if not bad:
        notes.append(f"1 `{SINK_TRAIT}` impl, routed through send_to")

    # ── arm 3: SealedElect exposes no way to get at the bytes ─────────────────────────────────
    sealed_impls = 0
    for path, text in files.items():
        for m in re.finditer(r"impl\s+SealedElect\s*\{", text):
            sealed_impls += 1
            body = block_after(text, m.end() - 1)
            if body is None:
                fail(f"unbalanced braces in `impl SealedElect` ({path.relative_to(root)})")
                return 2
            for fm in re.finditer(r"pub\s+(?:const\s+)?fn\s+(\w+)\s*\([^)]*\)\s*->\s*([^{;]+)", body):
                name, ret = fm.group(1), fm.group(2).strip()
                leaks = "[u8" in ret or "&[u8]" in ret or "str" in ret
                if leaks:
                    bad.append(
                        f"arm 3 (no-accessor): `SealedElect::{name}` returns `{ret}` — the sealed "
                        "bytes must have NO accessor, or the seal is decoration and any sender "
                        "can be handed the frame."
                    )
        for sm in re.finditer(r"pub\s+struct\s+SealedElect\s*\{([^}]*)\}", text):
            if re.search(r"\bpub\s+\w+\s*:", sm.group(1)):
                bad.append(
                    "arm 3 (no-accessor): `SealedElect` has a PUBLIC field — the buffer must be "
                    "private."
                )
    if sealed_impls == 0:
        fail("no `impl SealedElect` found — the sealed-frame type is gone or renamed.")
        return 2

    # ── arm 4: exactly one firmware call site of the encoder, and it is the seal ───────────────
    encode_sites = []
    for path, text in files.items():
        for m in re.finditer(r"\bwire::encode\s*\(|(?<![\w:])encode\s*\(\s*f\s*,", text):
            fn = enclosing_fn(text, m.start())
            encode_sites.append((path.relative_to(root), fn))
    if not encode_sites:
        fail(
            "found ZERO call sites of the ELECT encoder.",
            "Either it was renamed or the seal no longer encodes — both leave this arm covering",
            "nothing, so it fails rather than passes.",
        )
        return 2
    stray = [(p, fn) for p, fn in encode_sites if fn != "seal"]
    if stray:
        where = ", ".join(f"{p}:{fn}" for p, fn in stray)
        bad.append(
            f"arm 4 (one-encoder): the ELECT encoder is called outside `SealedElect::seal` "
            f"({where}). A caller that encodes for itself holds raw frame bytes and can send them "
            "down any path."
        )
    else:
        notes.append(f"{len(encode_sites)} encoder call site(s), all in SealedElect::seal")

    # ── arm 5: the frame prefix is written exactly once ───────────────────────────────────────
    literal_sites = []
    for path, text in files.items():
        for m in re.finditer(re.escape(ELECT_LITERAL), text):
            literal_sites.append((path.relative_to(root), text.count("\n", 0, m.start()) + 1))
    if not literal_sites:
        fail(
            f"the literal {ELECT_LITERAL} appears NOWHERE.",
            "The frame prefix was changed or moved; this arm can no longer detect a hand-built",
            "frame, so it refuses to pass.",
        )
        return 2
    if len(literal_sites) > 1:
        where = ", ".join(f"{p}:{ln}" for p, ln in literal_sites)
        bad.append(
            f"arm 5 (no-hand-build): {ELECT_LITERAL} is written {len(literal_sites)} times "
            f"({where}). A second spelling means a frame built by hand, which never touches the "
            "encoder and so is invisible to arm 4."
        )

    # ── arm 6: every raw esp_now.send site is declared, with its count ─────────────────────────
    decl_text = None
    for text in files.values():
        m = DECL.search(text)
        if m:
            decl_text = m.group(1)
            break
    if decl_text is None:
        fail(
            "no `RAW-SEND-SITES:` declaration found in the firmware source.",
            "The allowlist of functions permitted to call `esp_now.send` directly must live where",
            "a machine can check it — see this script's docstring.",
        )
        return 2
    declared = {}
    for tok in re.split(r"[,\s]+", decl_text):
        if not tok:
            continue
        name, _, count = tok.partition(":")
        if not count.isdigit():
            fail(f"malformed RAW-SEND-SITES entry {tok!r} — expected `fn_name:count`.")
            return 2
        declared[name] = int(count)

    actual = {}
    for path, text in files.items():
        for m in re.finditer(r"esp_now\s*\.\s*send\s*\(", text):
            fn = enclosing_fn(text, m.start())
            if fn is None:
                fail(f"an `esp_now.send` in {path.relative_to(root)} is not inside any fn")
                return 2
            actual[fn] = actual.get(fn, 0) + 1
    if not actual:
        fail(
            "found ZERO raw `esp_now.send` call sites — including the `send_to` choke itself.",
            "That cannot be right; the pattern must have changed, so this arm is blind.",
        )
        return 2
    if actual != declared:
        added = {k: v for k, v in actual.items() if declared.get(k) != v}
        gone = {k: v for k, v in declared.items() if k not in actual}
        if added:
            bad.append(
                "arm 6 (raw-sends): undeclared or miscounted raw send sites: "
                + ", ".join(f"{k}×{v} (declared {declared.get(k, 0)})" for k, v in sorted(added.items()))
                + ". Every function that bypasses `send_to` must be declared, because every one of "
                "them is a path an ELECT frame could take unauthenticated."
            )
        if gone:
            bad.append(
                "arm 6 (raw-sends): declared but ABSENT: "
                + ", ".join(f"{k}×{v}" for k, v in sorted(gone.items()))
                + ". A stale allowlist entry reads as a considered decision and is worse than none."
            )
    else:
        total = sum(actual.values())
        notes.append(f"{total} raw send sites across {len(actual)} declared fns")

    if bad:
        print("FATAL: the ELECT send-path invariant is violated.", file=sys.stderr)
        for b in bad:
            print(f"  - {b}", file=sys.stderr)
        print(
            "  A leaf cannot verify an announcement before acting on it (a scan drops the "
            "association),\n  so an unauthenticated ELECT strands the fleet. See "
            "net/mesh_elect.rs's security section.",
            file=sys.stderr,
        )
        return 1

    print("  elect send path: " + "; ".join(notes))
    return 0


if __name__ == "__main__":
    sys.exit(main())
