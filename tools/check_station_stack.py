#!/usr/bin/env python3
"""check_station_stack.py — the S3 tapstone station's stack headroom, asserted (tapstone#132).

On 2026-09-30 the station image with the voice panicked at boot on board 61 ("Detected a write to
the stack guard value on ProCpu"). The main task's poll frame (`clock::run::{closure#0}`) was
already ~38 KB, and the voice's .bss shrank the .stack region until the boot paint's callees had
~1.2 KB left. Nothing gated it: the #300 floor covers the canonical ELF, and the hand build has
none. This is that gate. It is not a high-water measurement (stack-paint is invalid on the S3,
budget.rs). It is the room left for everything `run` calls:

    margin = .stack region - run's poll frame >= --min (12,288 B by default)

The image that ran clean before the voice had ~13,090 B, the fixed one has 13,524 B, and the one
that panicked had 684 B (frames counted as 32 + VALUE in the large form, the conservative way).

    tools/check_station_stack.py <elf> [--min BYTES]      # needs the xtensa binutils (. ~/export-esp.sh)
    tools/check_station_stack.py --self-test              # the parser, on canned objdump text; no toolchain

The frame comes from the function's Xtensa prologue, the only place it is written down:
  - `entry a1, N`: a frame of N bytes (N <= 32,760);
  - `entry a1, 32` then `l32r aX, <lit> (VALUE)`, `sub aX, a1, aX`, `movsp a1, aX`: a large frame,
    32 + VALUE bytes.
Exit: 0 ok, 1 margin under the floor, 2 could not read the ELF (never a pass).
"""
import argparse
import os
import re
import subprocess
import sys

RUN_SYMBOL = "clock::run::{closure#0}"
PREFIX = os.environ.get("XTENSA_PREFIX", "xtensa-esp32s3-elf-")


class Unreadable(Exception):
    pass


def frame_from_disasm(text):
    """The frame size from the first instructions of a disassembly, or Unreadable."""
    ins = [l.split("\t", 2)[-1].strip() for l in text.splitlines() if re.match(r"^\s*[0-9a-f]+:\t", l)]
    if not ins:
        raise Unreadable("no instructions in the disassembly")
    m = re.match(r"entry\s+a1,\s*(0x[0-9a-f]+|\d+)$", ins[0])
    if not m:
        raise Unreadable(f"the first instruction is not `entry a1, N`: {ins[0]!r}")
    n = int(m.group(1), 0)
    if n > 32:
        return n
    # entry 32 is either a small frame, or the head of the large form. Any `movsp a1` in the
    # prologue means the large form, which must then parse in full: a literal whose value objdump
    # did not annotate, or any other shape, is UNREADABLE (exit 2), never a 32-byte frame. That
    # 32-byte reading is exactly what the image that panicked on glass would have passed with.
    if not any(re.match(r"movsp\s+a1,", i) for i in ins[1:6]):
        return n
    if len(ins) >= 4:
        lit = re.match(r"l32r\s+(a\d+),\s*[0-9a-f]+\s*<[^>]*>\s*\((0x)?([0-9a-f]+)", ins[1])
        sub = re.match(r"sub\s+(a\d+),\s*a1,\s*(a\d+)$", ins[2])
        movsp = re.match(r"movsp\s+a1,\s*(a\d+)$", ins[3])
        if lit and sub and movsp and sub.group(2) == lit.group(1) and movsp.group(1) == sub.group(1):
            return 32 + int(lit.group(3), 16)
    raise Unreadable(f"a large-frame prologue this checker cannot read: {ins[1:4]!r}")


def run_tool(*args):
    try:
        r = subprocess.run([PREFIX + args[0], *args[1:]], capture_output=True, text=True)
    except FileNotFoundError:
        raise Unreadable(f"{PREFIX}{args[0]} not found (source ~/export-esp.sh)")
    if r.returncode:
        raise Unreadable(f"{PREFIX}{args[0]} failed: {r.stderr.strip()[:200]}")
    return r.stdout


def measure(elf):
    addr = None
    for line in run_tool("nm", "-C", elf).splitlines():
        f = line.split(" ", 2)
        if len(f) == 3 and f[2] == RUN_SYMBOL:
            addr = int(f[0], 16)
    if addr is None:
        raise Unreadable(f"no `{RUN_SYMBOL}` in {elf} (not a station image?)")
    dis = run_tool("objdump", "-d", f"--start-address={addr:#x}", f"--stop-address={addr + 16:#x}", elf)
    frame = frame_from_disasm(dis)
    stack = None
    for line in run_tool("size", "-A", elf).splitlines():
        f = line.split()
        if f and f[0] == ".stack":
            stack = int(f[1])
    if stack is None:
        raise Unreadable(f"no .stack section in {elf}")
    return stack, frame


SMALL = """
42030964:\tec6136        \tentry\ta1, 0x7630
42030967:\t017d      \tmov.n\ta7, a1
"""
LARGE = """
42040a64:\t004136        \tentry\ta1, 32
42040a67:\tc6ed81        \tl32r\ta8, 4203261c <idle_hook_fn+0x25d4> (9690 <RESERVE_ICACHE+0x1690>)
42040a6a:\tc08180        \tsub\ta8, a1, a8
42040a6d:\t001810        \tmovsp\ta1, a8
"""


LARGE_UNANNOTATED = """
42040a64:\t004136        \tentry\ta1, 32
42040a67:\tc6ed81        \tl32r\ta8, 4203261c <idle_hook_fn+0x25d4>
42040a6a:\tc08180        \tsub\ta8, a1, a8
42040a6d:\t001810        \tmovsp\ta1, a8
"""


def self_test():
    assert frame_from_disasm(SMALL) == 0x7630, "entry a1, N"
    assert frame_from_disasm(LARGE) == 32 + 0x9690, "entry 32 + l32r/sub/movsp"
    try:
        frame_from_disasm(LARGE_UNANNOTATED)
        raise AssertionError("a large frame whose literal objdump did not annotate must be unreadable, not 32 B")
    except Unreadable:
        pass
    try:
        frame_from_disasm("42000000:\t0000\tnop\n")
        raise AssertionError("a function without `entry` must be unreadable, not a frame of 0")
    except Unreadable:
        pass
    assert check(43828, 0x7660, 12288)[0], "the fixed image passes"
    assert not check(39260, 32 + 0x9690, 12288)[0], "the image that panicked on glass fails"
    print("check_station_stack self-test: 6 ok")


def check(stack, frame, floor):
    margin = stack - frame
    return margin >= floor, margin


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("elf", nargs="?")
    ap.add_argument("--min", type=int, default=12288)
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.self_test:
        self_test()
        return 0
    if not a.elf:
        ap.error("an ELF, or --self-test")
    try:
        stack, frame = measure(a.elf)
    except Unreadable as e:
        print(f"station stack: CANNOT READ — {e}", file=sys.stderr)
        return 2
    ok, margin = check(stack, frame, a.min)
    print(f"station stack: .stack {stack:,} B, run frame {frame:,} B, margin {margin:,} B "
          f"(floor {a.min:,} B): {'ok' if ok else 'UNDER THE FLOOR'}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
