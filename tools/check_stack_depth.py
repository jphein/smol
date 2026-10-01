#!/usr/bin/env python3
"""check_stack_depth.py — the S3 tapstone station's worst-case stack, from the ELF (tapstone#132).

On 2026-10-01 both shrines rebooted with stack-overflow panics on an image that passed the stack
gate (`check_station_stack.py`). That gate measured one frame, `run`'s. The overflow was a call
chain under it: `run` (30,304 B) → `RadioManager::diag_record` (2,960) → `ota::read_net_cfg`
(7,248) → `FlashStorage::read` (4,144) → the ROM, then an interrupt on top (the panic's backtrace).
This gate sums the chain:

    worst = depth(main) + IRQ_RESERVE + MARGIN <= .stack

depth(f) is f's frame plus the deepest of its callees, over the whole call graph from `main` (the
main task's entry; .stack is that task's stack). Frames come from each function's Xtensa prologue,
read the same way `check_station_stack.py` reads them (`entry a1, N`, or `entry a1, 32` + `l32r` +
`sub` + `movsp`, which is 32 + VALUE bytes). Calls are `call0..12 <sym>`, and `callx` through a
register last loaded by `l32r` from a literal objdump annotated with a symbol.

What the graph cannot see, and the rule for each. Every rule is either a bound or a refusal:
  - An executor dispatch: the indirect `callx` in `Executor::run_inner` polls the embassy tasks.
    Those edges are added by name: run_inner → every `TaskStorage<..>::poll`. Finding no task
    poll is UNREADABLE (exit 2).
  - Any other indirect `callx` (a `dyn` method, a waker, a callback), and a call to a code address
    with no symbol: bounded by INDIRECT, the deepest function whose address sits in a data section
    (the vtables and fn-pointer tables an indirect call can reach), with that function's own
    indirect calls counted at 0. Multi-level indirection (a dyn call inside a dyn-called function:
    `core::fmt` is the usual one) is NOT bounded by this. It is named here instead of hidden.
    IRQ_RESERVE and the station's runtime `stackfree` witness cover it.
  - A call into the mask ROM (memcpy, memset, __udivdi3, ets_delay_us …): ROM_FRAME bytes each.
    These are leaf routines with frames under 100 B, and 256 B is the declared bound.
  - Recursion: a cycle in the direct graph has no static depth. Each cycle must match
    RECURSION_OK (a reason per entry). Any other cycle is UNREADABLE (exit 2), never a pass.
  - Interrupts: on esp-rtos an interrupt runs on the interrupted task's stack. IRQ_RESERVE is
    2,048 B. Measured on this image: the level-1 vector's own chain (`__level_1_interrupt` →
    `trigger_task_switch`, the panic's frame) is printed by the check, and the reserve must cover
    it plus one nested higher-level interrupt plus the 0x100 B exception save area per level.
    On glass, the 2026-10-01 panic put sp 1,564 B under `_stack_end` where the static chain
    overflowed by ~1,036 B. So that moment's ROM + interrupt part was ~530 B, a quarter of this
    reserve.

    tools/check_stack_depth.py <elf>                       # needs the xtensa binutils (. ~/export-esp.sh)
    tools/check_stack_depth.py --dis <objdump -d -C text> --stack N   # a fixture; no toolchain
    tools/check_stack_depth.py --self-test                 # the tests/testdata fixture, red and perturbed
Exit: 0 fits, 1 does not fit, 2 could not read (never a pass).
"""
import argparse
import os
import re
import struct
import subprocess
import sys

PREFIX = os.environ.get("XTENSA_PREFIX", "xtensa-esp32s3-elf-")
ROOT = "main"
IRQ_RESERVE = 2560
MARGIN = 1024
ROM_FRAME = 256
EXC_FRAME = 0x100
ROM = range(0x40000000, 0x40060000)  # the S3's mask ROM
DISPATCH = re.compile(r"^<esp_rtos::embassy::Executor>::run_inner::")
TASK_POLL = re.compile(r"^<embassy_executor::raw::TaskStorage<.*>>::poll$")
# The panic machinery is terminal: a panic halts the board, so how deep its message formatting goes
# is not a state a running station is ever in. Its frames are counted, its callees are not.
TERMINAL = re.compile(r"^(core::panicking::|__rustc::rust_begin_unwind$|esp_backtrace::|__default_\w*exception$"
                      r"|core::result::unwrap_failed$|core::option::(unwrap|expect)_failed$)")
IRQ_ENTRIES = ("__level_1_interrupt", "__level_2_interrupt", "__level_3_interrupt")
# What a vector dispatches to: the static `__INTERRUPTS` table, plus the handlers esp-hal/esp-rtos/
# esp-radio bind at runtime (their addresses are loaded as literals, never called directly).
HANDLER_RX = re.compile(r"(handler|_isr|Handler>::dispatch|^Software\d|^WIFI_MAC)$|_from_isr$")
# Each entry: (regex on a function on the cycle, reason). Measured on the station image, 2026-10-01.
RECURSION_OK = [
    (r"^core::slice::sort::", "core's sorts recurse on one partition only, depth bounded by log2(len)"),
    (r"^<?core::str::|slice_error_fail", "core::str error path: formats a panic message, then halts"),
    (r"^core::fmt::|<.* as core::fmt::", "core::fmt Display/Debug nesting: depth follows the value's nesting"),
    (r"SplitInternal", "core::str::Split: next_back retries once on an empty tail"),
    (r"^_etoa|^_ftoa|^_ntoa|^_out_rev|^_vsnprintf|printf", "the wifi blob's printf: %e falls back to %f once"),
    (r"^wifi_log|^ets_printf|^esp_log", "the wifi blob's logger: its format path, as above"),
    (r"treehead", "smol treehead: Merkle height, bounded by log2 of the fixed leaf count"),
]
FN = re.compile(r"^([0-9a-f]+) <(.+)>:$")
INS = re.compile(r"^\s*([0-9a-f]+):\s+(\S+)\s*(.*)$")
LIT = re.compile(r"^(a\d+),.*\(([0-9a-f]+)(?:\s<.*)?\)\s*$")
CALL = re.compile(r"^([0-9a-f]+)\s*<")


class Unreadable(Exception):
    pass


def parse(text):
    funcs, cur = {}, None
    for line in text.splitlines():
        m = FN.match(line)
        if m:
            cur = {"addr": int(m.group(1), 16), "name": m.group(2), "ins": []}
            funcs[cur["addr"]] = cur
            continue
        m = INS.match(line) if cur else None
        if m:
            cur["ins"].append((int(m.group(1), 16), m.group(2), m.group(3)))
    # The wifi blob's static functions have no symbols: one symbol's span can hold several, and
    # they are called at `<sym+0xNN>`. Split a span wherever a direct call lands on an `entry`.
    targets = set()
    for f in funcs.values():
        for _, op, args in f["ins"]:
            if op in ("call0", "call4", "call8", "call12"):
                m = CALL.match(args)
                if m:
                    targets.add(int(m.group(1), 16))
    for f in list(funcs.values()):
        cuts = [i for i, (pc, op, _) in enumerate(f["ins"]) if i and pc in targets and op == "entry"]
        for i in reversed(cuts):
            pc = f["ins"][i][0]
            funcs[pc] = {"addr": pc, "name": f"{f['name']}+{pc - f['addr']:#x}", "ins": f["ins"][i:]}
            f["ins"] = f["ins"][:i]
    return funcs


def frame(f):
    ins = f["ins"]
    if not ins or ins[0][1] != "entry":
        return None
    n = int(ins[0][2].split(",")[1].strip(), 0)
    if n == 32 and any(i[1] == "movsp" and i[2].startswith("a1,") for i in ins[1:6]):
        if len(ins) > 3 and ins[1][1] == "l32r" and ins[2][1] == "sub" and ins[3][1] == "movsp":
            m = LIT.match(ins[1][2])
            if m:
                return 32 + int(m.group(2), 16)
        raise Unreadable(f"a large-frame prologue this checker cannot read in {f['name']}")
    return n


def edges(funcs):
    """addr -> (direct callees, rom calls, unresolved indirect calls)."""
    out = {}
    for a, f in funcs.items():
        callees, rom, ind, regs, slots = set(), 0, 0, {}, {}
        for _, op, args in f["ins"]:
            # A literal spilled to a stack slot and reloaded (`s32i aX, aB, off` … `l32i aY, aB, off`).
            if op in ("s32i", "s32i.n"):
                src, base, off = (x.strip() for x in args.split(","))
                if src in regs:
                    slots[(base, off)] = regs[src]
                else:
                    slots.pop((base, off), None)
                continue
            if op in ("l32i", "l32i.n"):
                d, base, off = (x.strip() for x in args.split(","))
                if (base, off) in slots:
                    regs[d] = slots[(base, off)]
                else:
                    regs.pop(d, None)
                slots = {k: v for k, v in slots.items() if k[0] != d}
                continue
            if op == "l32r":
                m = LIT.match(args)
                if m:
                    regs[m.group(1)] = int(m.group(2), 16)
                continue
            t = None
            if op in ("call0", "call4", "call8", "call12"):
                m = CALL.match(args)
                t = int(m.group(1), 16) if m else -1
            elif op in ("callx0", "callx4", "callx8", "callx12"):
                t = regs.get(args.strip(), -1)
            else:
                d = args.split(",")[0].strip()
                src = args.split(",")[1].strip() if op in ("mov", "mov.n") and "," in args else None
                if src in regs:
                    regs[d] = regs[src]
                else:
                    regs.pop(d, None)
                slots = {k: v for k, v in slots.items() if k[0] != d}
                continue
            if t in funcs:
                callees.add(t)
            elif t in ROM:
                rom += 1
            else:
                ind += 1
        out[a] = (callees, rom, ind)
    return out


def data_taken(elf, funcs):
    taken = set()
    for sec in (".rodata", ".data", ".rodata.wifi", ".data.wifi"):
        try:
            r = subprocess.run([PREFIX + "objcopy", "-O", "binary", "--only-section=" + sec, elf, "/dev/stdout"],
                               capture_output=True)
        except FileNotFoundError:
            raise Unreadable(f"{PREFIX}objcopy not found (source ~/export-esp.sh)")
        b = r.stdout
        for i in range(0, len(b) - 3, 4):
            w = struct.unpack_from("<I", b, i)[0]
            if w in funcs:
                taken.add(w)
    return taken


class Graph:
    def __init__(self, funcs, taken):
        self.funcs, self.e = funcs, edges(funcs)
        polls = {a for a, f in funcs.items() if TASK_POLL.match(f["name"])}
        self.dispatch = {a for a, f in funcs.items() if DISPATCH.match(f["name"])}
        if self.dispatch and not polls:
            raise Unreadable("an executor dispatch but no `TaskStorage::poll` to dispatch to")
        for a in self.dispatch:
            c, rom, ind = self.e[a]
            self.e[a] = (c | polls, rom, 0)
        self.cycles = []
        self.indirect = 0
        if taken:
            self.indirect = max(self.depth(a)[0] for a in taken)
            self.memo = {}
        self.cycles = []

    def depth(self, root):
        self.memo = getattr(self, "memo", {})
        stack, onstack = [], set()
        sys.setrecursionlimit(200000)

        def d(a):
            if a in self.memo:
                return self.memo[a]
            if a in onstack:
                i = stack.index(a)
                self.cycles.append([self.funcs[x]["name"] for x in stack[i:]])
                return (0, None)
            onstack.add(a)
            stack.append(a)
            fr = frame(self.funcs[a])
            if fr is None:
                raise Unreadable(f"no `entry` prologue in {self.funcs[a]['name']}")
            c, rom, ind = self.e[a]
            if TERMINAL.match(self.funcs[a]["name"]):
                c, rom, ind = (), 0, 0
            best, via = (ROM_FRAME if rom else 0), None
            if ind and self.indirect > best:
                best = self.indirect
            for t in c:
                x = d(t)[0]
                if x > best:
                    best, via = x, t
            stack.pop()
            onstack.discard(a)
            self.memo[a] = (fr + best, via)
            return self.memo[a]

        return d(root)

    def chain(self, root):
        out, a = [], root
        while a is not None:
            tot, via = self.memo[a]
            out.append((frame(self.funcs[a]), tot, self.funcs[a]["name"]))
            a = via
        return out


def interrupts_table(elf, funcs):
    """Function addresses in the static `__INTERRUPTS` vector table (esp-hal's peripheral table)."""
    a = next((x for x, f in funcs.items() if f["name"] == "__INTERRUPTS"), None)
    if a is None:
        return set()
    nxt = min((x for x in funcs if x > a), default=a + 4 * 128)
    dump = run_tool("objdump", "-s", f"--start-address={a:#x}", f"--stop-address={nxt:#x}", elf)
    out = set()
    for line in dump.splitlines():
        m = re.match(r"^\s*[0-9a-f]{8}\s((?:\s?[0-9a-f]{8}){1,4})", line)
        if m:
            for w in m.group(1).split():
                v = struct.unpack("<I", bytes.fromhex(w))[0]
                if v in funcs:
                    out.add(v)
    return out


def isr_bound(g, funcs, elf):
    """Deepest vector + handler chain: a vector's own frames, then the deepest handler it can
    dispatch to (handlers' own indirect calls at 0, as for any indirect target). (bytes, name)."""
    if not elf:
        return 0, "no ELF: not measured"
    vectors = [a for a, f in funcs.items() if f["name"] in IRQ_ENTRIES]
    if not vectors:
        raise Unreadable("no interrupt vector (`__level_1_interrupt`) in the image")
    code = set()
    for f in funcs.values():
        for _, op, args in f["ins"]:
            m = LIT.match(args) if op == "l32r" else None
            if m and int(m.group(2), 16) in funcs:
                code.add(int(m.group(2), 16))
    handlers = interrupts_table(elf, funcs) | {a for a in code if HANDLER_RX.search(funcs[a]["name"])}
    handlers = {a for a in handlers if frame(funcs[a]) is not None}
    saved, g.indirect, g.memo = g.indirect, 0, {}
    try:
        if not handlers:
            raise Unreadable("no interrupt handler found for the vectors to dispatch to")
        h = max(handlers, key=lambda a: g.depth(a)[0])
        hd = g.depth(h)[0]
        v = max(vectors, key=lambda a: frame(funcs[a]))
        return frame(funcs[v]) + hd, f"{funcs[v]['name']} {frame(funcs[v])} + {funcs[h]['name'][:50]} {hd}"
    finally:
        g.indirect, g.memo = saved, {}


def find(funcs, name):
    for a, f in funcs.items():
        if f["name"] == name:
            return a
    raise Unreadable(f"no `{name}` in the disassembly")


# A C symbol (no Rust path): the closed wifi/phy blob. Its static functions are stripped, so several
# share one symbol's span, and objdump loses sync on the literal data between them. A call from one
# to another then reads as a call to itself. Only a SELF-cycle in the blob is accepted on this
# ground; a cycle through two named functions is still UNREADABLE.
BLOB_RX = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*(\+0x[0-9a-f]+)?$")


def bad_cycles(cycles):
    bad = []
    for cyc in cycles:
        if len(cyc) == 1 and BLOB_RX.match(cyc[0]):
            continue
        if not any(re.search(rx, n) for rx, _ in RECURSION_OK for n in cyc):
            bad.append(cyc)
    return bad


def analyse(text, stack, elf=None, out=print):
    funcs = parse(text)
    taken = data_taken(elf, funcs) if elf else set()
    g = Graph(funcs, taken)
    root = find(funcs, ROOT)
    worst = g.depth(root)[0]
    ch = g.chain(root)
    isr, isr_via = isr_bound(g, funcs, elf)
    # One level-1 interrupt, one nested higher level: two vectors' chains and two save areas.
    isr_need = 2 * (isr + EXC_FRAME) if isr else 0
    total = worst + IRQ_RESERVE + MARGIN
    out(f"worst chain from `{ROOT}`: {worst:,} B ({len(ch)} frames)")
    for fr, tot, name in ch:
        out(f"  {fr:6,} {tot:7,}  {name[:110]}")
    out(f"indirect bound {g.indirect:,} B over {len(taken)} data-referenced functions; ROM call {ROM_FRAME} B")
    out(f"interrupt reserve {IRQ_RESERVE:,} B: vector chain {isr:,} B ({isr_via}); "
        f"two nested levels + 0x{EXC_FRAME:x} B save areas need {isr_need:,} B")
    seen, bad = set(), bad_cycles(g.cycles)
    for cyc in g.cycles:
        k = tuple(sorted(cyc))
        if k not in seen:
            seen.add(k)
    out(f"recursion: {len(seen)} cycle(s), {len(bad)} not in RECURSION_OK")
    for cyc in bad[:10]:
        out("  UNREADABLE cycle: " + " -> ".join(n[:60] for n in cyc))
    if bad:
        raise Unreadable(f"{len(bad)} recursion cycle(s) with no declared bound")
    if isr_need > IRQ_RESERVE:
        raise Unreadable(f"the interrupt reserve {IRQ_RESERVE} B is under the measured need {isr_need} B")
    ok = total <= stack
    out(f"station stack depth: {worst:,} + irq {IRQ_RESERVE:,} + margin {MARGIN:,} = {total:,} B "
        f"vs .stack {stack:,} B: {'fits, ' + format(stack - total, ',') + ' B spare' if ok else 'DOES NOT FIT'}")
    return ok, worst, ch


def run_tool(*args):
    try:
        r = subprocess.run([PREFIX + args[0], *args[1:]], capture_output=True, text=True)
    except FileNotFoundError:
        raise Unreadable(f"{PREFIX}{args[0]} not found (source ~/export-esp.sh)")
    if r.returncode:
        raise Unreadable(f"{PREFIX}{args[0]} failed: {r.stderr.strip()[:200]}")
    return r.stdout


def stack_of(elf):
    for line in run_tool("size", "-A", elf).splitlines():
        f = line.split()
        if f and f[0] == ".stack":
            return int(f[1])
    raise Unreadable(f"no .stack section in {elf}")


FIXTURE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testdata", "stack_depth_52e8a00b.dis")
FIXTURE_STACK = 43828  # .stack of the 52e8a00b image both shrines panicked on


def self_test():
    text = open(FIXTURE).read()
    quiet = lambda *_: None
    ok, worst, ch = analyse(text, FIXTURE_STACK, out=quiet)
    names = [n for _, _, n in ch]
    assert not ok, "the image that panicked on both shrines must not fit"
    assert any("diag_record" in n for n in names) and any("read_net_cfg" in n for n in names), \
        f"the red chain must be the panic's: {names}"
    assert worst == 44864, f"fixture worst chain moved: {worst}"
    # Perturbation: the same image with read_net_cfg's 7,248 B frame (its 3 KB partition-table
    # buffer) cut to 64 B fits. The red above is that chain, not something else in the fixture.
    cut = re.sub(r"(<clock::ota::read_net_cfg>:\n[0-9a-f]+:\tentry\ta1, )0x1c50", r"\g<1>64", text)
    assert cut != text, "the perturbation must change read_net_cfg's frame"
    ok2, worst2, ch2 = analyse(cut, FIXTURE_STACK, out=quiet)
    assert ok2 and worst2 < worst, f"with read_net_cfg's frame at 64 B it fits ({worst2})"
    # Fail closed: an unreadable large frame, a missing prologue, an undeclared cycle.
    try:
        # run's frame in the large form, with a literal objdump did not annotate.
        large = re.sub(r"(<clock::run::\{closure#0\}>:\n)([0-9a-f]+):\tentry\ta1, 0x7660\n",
                       r"\1\2:\tentry\ta1, 32\n\2:\tl32r\ta8, 42000000 <x>\n\2:\tsub\ta8, a1, a8\n"
                       r"\2:\tmovsp\ta1, a8\n", text)
        assert large != text, "the large-form perturbation must apply"
        analyse(large, FIXTURE_STACK, out=quiet)
        raise AssertionError("an unreadable large-frame prologue must be UNREADABLE")
    except Unreadable:
        pass
    try:
        analyse(re.sub(r"(<main>:\n\s*[0-9a-f]+:\s+)entry\s+a1, \d+", r"\1nop", text), FIXTURE_STACK, out=quiet)
        raise AssertionError("a function without `entry` must be UNREADABLE")
    except Unreadable:
        pass
    looped = text.replace("<clock::ota::read_net_cfg>:\n", "<clock::ota::read_net_cfg>:\n"
                          "4200fff0:\tcall8\t420d0000 <<clock::net::mode::RadioManager>::diag_record>\n", 1)
    looped = looped.replace("4200fff0:", "{:x}:".format(int(re.search(
        r"^([0-9a-f]+) <clock::ota::read_net_cfg>:", text, re.M).group(1), 16) + 1), 1)
    looped = looped.replace("420d0000", re.search(
        r"^([0-9a-f]+) <<clock::net::mode::RadioManager>::diag_record>:", text, re.M).group(1), 1)
    try:
        analyse(looped, FIXTURE_STACK, out=quiet)
        raise AssertionError("a recursion cycle not in RECURSION_OK must be UNREADABLE")
    except Unreadable:
        pass
    print("check_stack_depth self-test: 5 ok (52e8a00b red on the diag_record→read_net_cfg chain, "
          f"{worst:,} B vs {FIXTURE_STACK:,}; fits at {worst2:,} B with that frame at 64 B; 3 fail-closed)")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("elf", nargs="?")
    ap.add_argument("--dis")
    ap.add_argument("--stack", type=int)
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    try:
        if a.self_test:
            self_test()
            return 0
        if a.dis:
            if a.stack is None:
                ap.error("--dis needs --stack")
            ok, _, _ = analyse(open(a.dis).read(), a.stack)
        elif a.elf:
            text = run_tool("objdump", "-d", "-C", "--no-show-raw-insn", a.elf)
            ok, _, _ = analyse(text, stack_of(a.elf), elf=a.elf)
        else:
            ap.error("an ELF, --dis, or --self-test")
    except Unreadable as e:
        print(f"station stack depth: CANNOT READ — {e}", file=sys.stderr)
        return 2
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
