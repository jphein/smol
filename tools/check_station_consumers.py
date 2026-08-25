#!/usr/bin/env python3
"""#335 STEP G: prove there is exactly ONE live consumer of the STA transport per tier.

Why this exists, and why the type system cannot do it.

`esp_radio::wifi::Interface` is `Copy` (`esp-radio-0.18.0/src/wifi/mod.rs:1306`,
`#[derive(Clone, Copy, ...)]`). So `embassy_net::new(interfaces.station, ..)` does **not consume**
the handle, and neither does `SmolWifiDevice::new(interfaces.station)`. A live `Stack` and a live
`SmolWifiDevice` can therefore exist at the same time, over the same interface, **and it compiles
clean**. Both bottom out in `data_queue_rx()`, which is keyed by `InterfaceType` alone — so two
consumers pop ONE queue and frames are stolen nondeterministically between them.

No error. No panic. No failing gate. The invariant "all STA transport consumers move together" is
today enforced by *nothing at all*, which is this tree's dominant defect shape: a correct comment
describing behaviour the binary does not have (`[[stubbed-intentions-under-deliver-silently]]`).

This is the `tools/check_elect_send_path.py` idiom applied to that invariant: a declared roster in
the source, checked in BOTH directions, fail-closed, with a regression suite
(`tools/test_check_station_consumers.sh`) proving each arm can actually go red. A gate demonstrated
only in its passing state is not evidence (#350's `test_build_matrix.sh` lesson).

THE FOUR ARMS — each is a way to satisfy the compiler and still ship the packet-theft bug:

  1. count        the per-function count of `SmolWifiDevice::new` drifts from the declared roster.
                  Counts, not just names, so adding a SECOND device to an already-listed function
                  fails too — the property that makes the ELECT checker actually work.
  2. coexist      an `embassy_net::new` appears. THE packet-theft shape. Declared with its own
                  roster (empty today) so the first one to land must update the roster deliberately
                  and re-argue the invariant, rather than inheriting a silent pass. Also flags the
                  acute form directly: one function holding BOTH a stack and a device.
  3. per-tier     the two consumers stop being mutually exclusive, i.e. someone edits a cfg guard.
                  THE ARM THAT MATTERS AFTER STEP T, because STEP T is what could make them overlap.
  4. no-new-ctor  a second way to build the station device appears (`::from`, `::wrap`, a `pub`
                  tuple field) that arm 1's call-site count would never see.

⚠️ ARM 3'S DELIBERATE LIMITATION, stated rather than oversold. Deciding "these two cfg predicates
are mutually exclusive" is cfg algebra, and this checker does not attempt it. It asserts the
*literal* cfg attribute string at each declared gate site against an allow-list recorded in the
declaration, and fails closed on ANY change. That detects "someone edited a guard" — which is the
real failure path — and nothing more. If you need algebra, this is not the tool.

⚠️ AND THE STRUCTURAL SUBTLETY THE ROSTER ENCODES, because it surprised the author of this script:
the two consumers are NOT gated at their own definitions. Neither `try_time_sync` nor
`RadioManager::new` carries a `#[cfg]` attribute. `mod wifi` is compiled on EVERY radio tier, so on
an espnow tier BOTH `SmolWifiDevice::new` call sites are *compiled*. What makes them mutually
exclusive is REACHABILITY, one level up in `net.rs`: the `try_time_sync` re-export is
`#[cfg(all(feature = "wifi", not(feature = "espnow")))]` and `pub mod mode` is
`#[cfg(feature = "espnow")]`. That is why each guard declaration names the GATE FILE and the GATED
ITEM, not the function — pinning a cfg that does not exist where you expect it is how a gate ends up
green forever.

Usage: tools/check_station_consumers.py [repo-root]   (exit 0 ok, 1 violation, 2 malformed)
"""
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_comments import strip_comments  # noqa: E402  (#426 — one implementation, imported)

# The constructor whose call sites ARE the roster.
DEVICE_CTOR = "SmolWifiDevice::new"
# The device type, for the no-new-ctor arm.
DEVICE_TYPE = "SmolWifiDevice"
# The module that is allowed to construct the device by tuple syntax (it owns the type).
DEVICE_HOME = "radio_dev.rs"
# The embassy-net stack constructor — the other consumer of the same queue.
STACK_CTOR = "embassy_net::new"

DECL_SITES = re.compile(r"STATION-CONSUMER-SITES:\s*(.+?)\s*(?:\*/|\n|$)")
DECL_STACK = re.compile(r"STATION-STACK-SITES:\s*(.+?)\s*(?:\*/|\n|$)")
# `<site> | <gate file> | <gated item literal> | <cfg inner>` — pipe-delimited because a cfg
# predicate contains commas, `=` and parentheses, but never a pipe.
DECL_GUARD = re.compile(r"STATION-CONSUMER-GUARD:\s*(.+?)\s*(?:\*/|\n|$)")
NONE_TOKENS = ("none", "(none)", "-")


def fail(msg, *extra):
    print(f"FATAL: {msg}", file=sys.stderr)
    for line in extra:
        print(f"  {line}", file=sys.stderr)


def rust_sources(src: Path):
    return sorted(p for p in src.rglob("*.rs") if p.is_file())


# strip_comments moved to tools/rust_comments.py (#426). It was duplicated verbatim into
# check_elect_send_path.py the same day, and the sweep would have made it six copies — so the
# fix for "a checker counts its own prose" stopped being a thing each checker owns a copy of.

def enclosing_fn(text: str, idx: int):
    """Name of the nearest `fn` declared at or above `idx`. None if there isn't one."""
    best = None
    for m in re.finditer(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+(\w+)",
        text[:idx],
        re.M,
    ):
        best = m.group(1)
    return best


IMPL_RE = re.compile(
    r"^[ \t]*impl\s*(?:<[^>]*>\s*)?(?:[\w:]+(?:<[^>]*>)?\s+for\s+)?(\w+)", re.M
)


def impl_spans(text: str):
    """[(open_idx, close_idx, type_name)] for every `impl` block, by BALANCED BRACES.

    Deliberately not "the nearest `impl` above the site": a free function that merely FOLLOWS a
    closed impl block would be misattributed to it, which would silently rename a roster key and
    make arm 1 red for a reason that has nothing to do with the invariant. `try_time_sync` is
    exactly that shape — a free fn in a file with impls above it.
    """
    spans = []
    for m in IMPL_RE.finditer(text):
        open_at = text.find("{", m.end())
        if open_at < 0:
            continue
        depth = 0
        for i in range(open_at, len(text)):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    spans.append((open_at, i, m.group(1)))
                    break
    return spans


def site_key(path: Path, text: str, idx: int, spans=None):
    """`file.rs::Type::fn` if genuinely inside an impl, else `file.rs::fn`."""
    fn = enclosing_fn(text, idx)
    if fn is None:
        return None
    if spans is None:
        spans = impl_spans(text)
    # Innermost containing span wins (widest-first ordering is not guaranteed, so pick by size).
    best = None
    for open_at, close_at, name in spans:
        if open_at < idx < close_at and (best is None or (close_at - open_at) < best[0]):
            best = (close_at - open_at, name)
    if best is not None:
        return f"{path.name}::{best[1]}::{fn}"
    return f"{path.name}::{fn}"


def parse_roster(decl_text: str, label: str):
    """`a::b:1, c::d:2` -> {site: count}. `none` -> {}. Returns None on malformed."""
    if decl_text.strip().lower() in NONE_TOKENS:
        return {}
    out = {}
    for tok in re.split(r",\s*", decl_text.strip()):
        tok = tok.strip()
        if not tok:
            continue
        site, _, count = tok.rpartition(":")
        if not count.isdigit() or not site:
            fail(f"malformed {label} entry {tok!r} — expected `file.rs::path::fn:count`.")
            return None
        out[site] = int(count)
    return out


def code_line_after(text: str, idx: int):
    """The first non-blank, non-comment line strictly after the line containing `idx`."""
    for line in text[text.find("\n", idx) + 1 :].split("\n"):
        s = line.strip()
        if not s or s.startswith("//"):
            continue
        return s
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
    # Two views of every file. `files` is RAW and is what the declaration regexes read, because the
    # roster deliberately lives in a doc comment. `code` has comments blanked (offsets preserved)
    # and is what every call-site scan reads. Mixing the two up is how a checker gets fooled by
    # prose — see `strip_comments`.
    code = {p: strip_comments(t) for p, t in files.items()}

    bad, notes = [], []

    # ── the declaration (one roster, file-qualified, so it cannot be half-updated) ─────────────
    decl_sites = decl_stack = None
    guards_raw = []
    for text in files.values():
        if decl_sites is None:
            m = DECL_SITES.search(text)
            if m:
                decl_sites = m.group(1)
        if decl_stack is None:
            m = DECL_STACK.search(text)
            if m:
                decl_stack = m.group(1)
        guards_raw += [m.group(1) for m in DECL_GUARD.finditer(text)]
    if decl_sites is None:
        fail(
            "no `STATION-CONSUMER-SITES:` declaration found in the firmware source.",
            "The roster of functions that construct a STA transport consumer must live where a",
            "machine can check it — see this script's docstring.",
        )
        return 2
    if decl_stack is None:
        fail(
            "no `STATION-STACK-SITES:` declaration found in the firmware source.",
            f"The roster of `{STACK_CTOR}` sites must be declared even while it is empty — that is",
            "what makes the FIRST one land deliberately instead of inheriting a silent pass.",
        )
        return 2

    declared = parse_roster(decl_sites, "STATION-CONSUMER-SITES")
    if declared is None:
        return 2
    if not declared:
        fail(
            "`STATION-CONSUMER-SITES` declares NO sites.",
            "The tree has at least one STA consumer; an empty roster means this arm is blind.",
        )
        return 2
    declared_stack = parse_roster(decl_stack, "STATION-STACK-SITES")
    if declared_stack is None:
        return 2

    # ── arm 1: declared per-function device-constructor counts, both directions ────────────────
    actual = {}
    for path, text in code.items():
        for m in re.finditer(re.escape(DEVICE_CTOR) + r"\s*\(", text):
            key = site_key(path, text, m.start())
            if key is None:
                fail(f"a `{DEVICE_CTOR}` in {path.relative_to(root)} is not inside any fn")
                return 2
            actual[key] = actual.get(key, 0) + 1
    if not actual:
        fail(
            f"found ZERO `{DEVICE_CTOR}` call sites.",
            "That cannot be right while the smoltcp shim is still the transport; the pattern must",
            "have changed, so this arm is blind and refuses to pass.",
        )
        return 2
    if actual != declared:
        added = {k: v for k, v in actual.items() if declared.get(k) != v}
        gone = {k: v for k, v in declared.items() if k not in actual}
        if added:
            bad.append(
                "arm 1 (count): undeclared or miscounted station-consumer sites: "
                + ", ".join(f"{k}x{v} (declared {declared.get(k, 0)})" for k, v in sorted(added.items()))
                + f". Every `{DEVICE_CTOR}` is a consumer of the one shared rx queue, so an "
                "undeclared one is a frame thief that compiles clean."
            )
        if gone:
            bad.append(
                "arm 1 (count): declared but ABSENT: "
                + ", ".join(f"{k}x{v}" for k, v in sorted(gone.items()))
                + ". A stale roster entry reads as a considered decision and is worse than none."
            )

    # ── arm 2: the packet-theft shape ─────────────────────────────────────────────────────────
    stack_actual = {}
    for path, text in code.items():
        for m in re.finditer(re.escape(STACK_CTOR) + r"\s*\(", text):
            key = site_key(path, text, m.start())
            if key is None:
                fail(f"an `{STACK_CTOR}` in {path.relative_to(root)} is not inside any fn")
                return 2
            stack_actual[key] = stack_actual.get(key, 0) + 1
    if stack_actual != declared_stack:
        added = {k: v for k, v in stack_actual.items() if declared_stack.get(k) != v}
        gone = {k: v for k, v in declared_stack.items() if k not in stack_actual}
        if added:
            bad.append(
                "arm 2 (coexist): undeclared "
                + f"`{STACK_CTOR}` site(s): "
                + ", ".join(f"{k}x{v}" for k, v in sorted(added.items()))
                + f". `Interface` is `Copy`, so a `Stack` and a `{DEVICE_TYPE}` over the same "
                "interface BOTH pop `data_queue_rx()` and steal each other's frames, with no error. "
                "If this is STEP T, move every consumer in the same commit and update the roster."
            )
        if gone:
            bad.append(
                "arm 2 (coexist): declared but ABSENT: "
                + ", ".join(f"{k}x{v}" for k, v in sorted(gone.items()))
                + ". Remove the entry when the site goes, or the roster stops describing the tree."
            )
    # The acute form, called out separately because it is unambiguous wherever the cfgs land:
    both = sorted(set(stack_actual) & set(actual))
    if both:
        bad.append(
            "arm 2 (coexist): "
            + ", ".join(both)
            + f" holds BOTH an `{STACK_CTOR}` and a `{DEVICE_CTOR}`. That is the frame-theft shape "
            "in its most direct form — one function, two consumers, one queue."
        )

    # ── arm 3: the cfg guards are literally unchanged ──────────────────────────────────────────
    if not guards_raw:
        fail(
            "no `STATION-CONSUMER-GUARD:` declarations found.",
            "Each declared site needs its gate pinned, or arm 3 covers nothing.",
        )
        return 2
    guarded = set()
    for raw in guards_raw:
        parts = [p.strip() for p in raw.split("|")]
        if len(parts) != 4:
            fail(
                f"malformed STATION-CONSUMER-GUARD {raw!r}",
                "expected `<site> | <gate file> | <gated item> | <cfg inner>` (4 pipe-separated).",
            )
            return 2
        site, gate_file, gated_item, cfg_inner = parts
        guarded.add(site)
        gate_paths = [p for p in code if p.name == gate_file]
        if not gate_paths:
            fail(f"guard for {site}: gate file {gate_file!r} not found in the source tree.")
            return 2
        gtext = code[gate_paths[0]]
        needle = f"#[cfg({cfg_inner})]"
        hit = None
        for m in re.finditer(re.escape(needle), gtext):
            nxt = code_line_after(gtext, m.start())
            if nxt is not None and gated_item in nxt:
                hit = m
                break
        if hit is None:
            bad.append(
                f"arm 3 (per-tier): the guard for {site} is not where the roster says. Expected "
                f"`{needle}` immediately above `{gated_item}` in {gate_file}. "
                "The two station consumers are mutually exclusive ONLY because of these guards; if "
                "one moved, they may now both be live on one tier — which compiles, and steals "
                "frames. Re-derive the exclusion, then update the roster."
            )
    missing_guards = sorted(set(declared) - guarded)
    if missing_guards:
        fail(
            "these declared sites have no STATION-CONSUMER-GUARD: " + ", ".join(missing_guards),
            "An unguarded site is one this arm cannot see, so the roster refuses to pass.",
        )
        return 2

    # ── arm 4: `new` stays the only way to build the device ────────────────────────────────────
    home = [p for p in code if p.name == DEVICE_HOME]
    if not home:
        fail(f"the device's home module {DEVICE_HOME!r} was not found.")
        return 2
    htext = code[home[0]]
    struct_m = re.search(r"pub\s+struct\s+" + DEVICE_TYPE + r"\s*(\(|\{|;)", htext)
    if struct_m is None:
        fail(
            f"could not find `pub struct {DEVICE_TYPE}` in {DEVICE_HOME}.",
            "Arm 4 anchors on that declaration; without it the arm is blind.",
        )
        return 2
    # A `pub` field turns tuple-construction into a public constructor arm 1 would never see.
    if struct_m.group(1) == "(":
        tail = htext[struct_m.end() - 1 :]
        close = tail.find(")")
        if close > 0 and re.search(r"\bpub\b", tail[:close]):
            bad.append(
                f"arm 4 (no-new-ctor): `{DEVICE_TYPE}`'s tuple field is now `pub`, so "
                f"`{DEVICE_TYPE}(iface)` is a public constructor anywhere in the crate. Arm 1 "
                "counts `::new` call sites and would never see it."
            )
    # Any other associated fn returning Self is a second constructor.
    for m in re.finditer(r"impl\s+" + DEVICE_TYPE + r"\s*\{", htext):
        depth, i = 0, m.end() - 1
        while i < len(htext):
            if htext[i] == "{":
                depth += 1
            elif htext[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        body = htext[m.end() : i]
        for fm in re.finditer(r"fn\s+(\w+)\s*\([^)]*\)\s*->\s*([^{;]+)", body):
            name, ret = fm.group(1), fm.group(2).strip()
            if name == "new":
                continue
            if re.search(r"\bSelf\b|\b" + DEVICE_TYPE + r"\b", ret):
                bad.append(
                    f"arm 4 (no-new-ctor): `{DEVICE_TYPE}::{name}` also returns "
                    f"`{ret}` — a second constructor. Arm 1's roster counts `::new` sites only, so "
                    "a device built through this one is invisible to it."
                )
    # Tuple construction outside the owning module.
    for path, text in code.items():
        if path.name == DEVICE_HOME:
            continue
        for m in re.finditer(r"\b" + DEVICE_TYPE + r"\s*\(", text):
            bad.append(
                f"arm 4 (no-new-ctor): {path.relative_to(root)}:"
                f"{text.count(chr(10), 0, m.start()) + 1} builds `{DEVICE_TYPE}` by tuple syntax "
                f"rather than `::new`, so arm 1 does not count it."
            )

    if bad:
        print("FATAL: the one-station-consumer invariant is violated.", file=sys.stderr)
        for b in bad:
            print(f"  - {b}", file=sys.stderr)
        print(
            f"  `Interface` is `Copy`, so two consumers over one interface COMPILE and then pop the\n"
            f"  same `data_queue_rx()` — frames vanish nondeterministically with no error. See this\n"
            f"  script's docstring and docs/embassy/PHASE3-PLAN.md STEP G.",
            file=sys.stderr,
        )
        return 1

    total = sum(actual.values())
    notes.append(f"{total} station consumer(s) across {len(actual)} declared fn(s)")
    notes.append(f"{sum(stack_actual.values())} `{STACK_CTOR}` site(s)")
    notes.append(f"{len(guards_raw)} cfg guard(s) pinned")
    print("  station consumers: " + "; ".join(notes))
    return 0


if __name__ == "__main__":
    sys.exit(main())
