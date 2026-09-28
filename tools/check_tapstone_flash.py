#!/usr/bin/env python3
"""check_tapstone_flash.py — the vendored tapstone crates' flash budget (JP's ruling 2026-09-27: 64 KB).

Compares two ELFs of the SAME chip and the same canonical feature set:

  --fleet-elf     the canonical fleet ELF (tools/gate.sh fw's stack-floor build)
  --tapstone-elf  that same composition plus `tapstone,tapstone-probe`

and fails when the tapstone ELF carries more than --budget bytes (default 65,536) of flash image
beyond the fleet one.

WHAT COUNTS AS FLASH: every section that is SHF_ALLOC and not SHT_NOBITS. That is the bytes the
image holds: code and read-only data run from flash, plus the IRAM/DRAM initialisers (.rwtext,
.data) the bootloader copies out of it. `.bss` is NOBITS and costs no flash, and debug sections are
not ALLOC. The ELF is parsed here directly (ELF32, little-endian: RISC-V C3/C6 and Xtensa S3
alike) instead of via `readelf`/`size`, because the chip's binutils are only on PATH after
`export-esp.sh`. On 2026-09-21 an `nm` run that lacked them returned zero hits for everything and
read as "nothing linked".

WHY A PROBE, AND WHY THERE IS A FLOOR: nothing in a `tapstone` build calls the engine until the
mesh is wired (smol issue 1), so fat LTO strips it and the shipping tree reads ~2 KB. The probe
(`tapstone::flash_probe`) makes the crates reachable. A delta BELOW --floor (default 4,096) means
the probe stopped holding them, so LTO won and the instrument cannot see. That FAILS rather than
passing, because a zero from an instrument nobody has shown can see is not evidence.

Exit 0 within budget · 1 over budget or blind · 2 usage / unreadable ELF.
`--self-test` proves both failure arms on synthetic ELFs, with no toolchain.
"""
from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

SHF_ALLOC = 0x2
SHT_NOBITS = 8
BUDGET = 64 * 1024
FLOOR = 4 * 1024


class Bad(Exception):
    pass


def flash_sections(data: bytes) -> dict[str, int]:
    """Name -> size of every flash-resident section (ALLOC, not NOBITS) of an ELF32 LE image."""
    if data[:4] != b"\x7fELF":
        raise Bad("not an ELF file")
    if data[4] != 1 or data[5] != 1:
        raise Bad("not ELF32 little-endian (every smol chip is)")
    shoff, = struct.unpack_from("<I", data, 0x20)
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x2E)
    if shoff == 0 or shnum == 0 or shentsize < 40:
        raise Bad("no section headers")

    def hdr(i: int) -> tuple[int, int, int, int, int]:
        name, typ, flags, _addr, off, size = struct.unpack_from("<IIIIII", data, shoff + i * shentsize)
        return name, typ, flags, off, size

    _, _, _, stroff, strsize = hdr(shstrndx)
    strtab = data[stroff:stroff + strsize]
    out: dict[str, int] = {}
    for i in range(shnum):
        name, typ, flags, _off, size = hdr(i)
        if flags & SHF_ALLOC and typ != SHT_NOBITS and size:
            n = strtab[name:strtab.index(b"\0", name)].decode()
            out[n] = out.get(n, 0) + size
    return out


def verdict(fleet: dict[str, int], tap: dict[str, int], budget: int, floor: int) -> tuple[bool, str]:
    f, t = sum(fleet.values()), sum(tap.values())
    delta = t - f
    lines = [f"  fleet {f:,} B · tapstone+probe {t:,} B · delta {delta:+,} B (budget {budget:,}, floor {floor:,})"]
    for name in sorted(set(fleet) | set(tap)):
        d = tap.get(name, 0) - fleet.get(name, 0)
        if d:
            lines.append(f"    {name:<20} {d:+,}")
    if delta > budget:
        lines.append(f"OVER BUDGET: the vendored tapstone crates cost {delta:,} B of flash, over {budget:,} B.")
        return False, "\n".join(lines)
    if delta < floor:
        lines.append(f"BLIND: delta {delta:,} B is under the {floor:,} B floor. The probe no longer keeps the "
                     "engine reachable (LTO stripped it), so this cannot measure the budget.")
        return False, "\n".join(lines)
    lines.append(f"ok: {delta:,} B of {budget:,} B ({budget - delta:,} B headroom)")
    return True, "\n".join(lines)


def synth_elf(sections: list[tuple[str, int, int, int]]) -> bytes:
    """A minimal ELF32 LE with the given (name, type, flags, size) sections plus .shstrtab."""
    names = b"\0" + b"".join(n.encode() + b"\0" for n, *_ in sections) + b".shstrtab\0"
    body = bytearray(0x34)
    body[:6] = b"\x7fELF\x01\x01"
    strtab_off = len(body)
    body += names
    shoff = len(body)
    hdrs = [struct.pack("<10I", 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)]
    pos = 1
    for n, typ, flags, size in sections:
        hdrs.append(struct.pack("<10I", pos, typ, flags, 0, 0, size, 0, 0, 0, 0))
        pos += len(n) + 1
    hdrs.append(struct.pack("<10I", pos, 3, 0, 0, strtab_off, len(names), 0, 0, 0, 0))
    body += b"".join(hdrs)
    struct.pack_into("<I", body, 0x20, shoff)
    struct.pack_into("<HHH", body, 0x2E, 40, len(hdrs), len(hdrs) - 1)
    return bytes(body)


def self_test() -> int:
    PROG = 1
    base = [(".text", PROG, 0x6, 100_000), (".rodata", PROG, SHF_ALLOC, 20_000),
            (".bss", SHT_NOBITS, 0x3, 50_000), (".debug_info", PROG, 0, 900_000)]
    fleet = flash_sections(synth_elf(base))
    assert fleet == {".text": 100_000, ".rodata": 20_000}, fleet  # NOBITS and non-ALLOC excluded

    def with_delta(text: int, bss: int = 0, dbg: int = 0) -> dict[str, int]:
        s = [(".text", PROG, 0x6, 100_000 + text), (".rodata", PROG, SHF_ALLOC, 20_000),
             (".bss", SHT_NOBITS, 0x3, 50_000 + bss), (".debug_info", PROG, 0, 900_000 + dbg)]
        return flash_sections(synth_elf(s))

    cases = [
        ("exactly the budget passes", with_delta(BUDGET), True),
        ("one byte over fails", with_delta(BUDGET + 1), False),
        ("a normal engine passes", with_delta(10_000), True),
        ("under the floor fails (blind)", with_delta(FLOOR - 1), False),
        ("bss growth is not flash", with_delta(10_000, bss=10**6), True),
        ("debug growth is not flash", with_delta(10_000, dbg=10**7), True),
    ]
    bad = 0
    for name, tap, want in cases:
        got, _ = verdict(fleet, tap, BUDGET, FLOOR)
        print(f"   {'PASS' if got == want else 'FAIL'}  {name}")
        bad += got != want
    for blob, name in [(b"nope", "not an ELF"), (b"\x7fELF\x02\x01" + bytes(60), "ELF64 refused")]:
        try:
            flash_sections(blob)
            print(f"   FAIL  {name}: accepted"); bad += 1
        except Bad:
            print(f"   PASS  {name}")
    print(f"{len(cases) + 2 - bad} passed, {bad} failed")
    return 1 if bad else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--fleet-elf", type=Path)
    ap.add_argument("--tapstone-elf", type=Path)
    ap.add_argument("--budget", type=int, default=BUDGET)
    ap.add_argument("--floor", type=int, default=FLOOR)
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not a.fleet_elf or not a.tapstone_elf:
        ap.error("--fleet-elf and --tapstone-elf are required")
    try:
        fleet = flash_sections(a.fleet_elf.read_bytes())
        tap = flash_sections(a.tapstone_elf.read_bytes())
    except (OSError, Bad, struct.error) as e:
        print(f"FATAL: {e}", file=sys.stderr)
        return 2
    ok, text = verdict(fleet, tap, a.budget, a.floor)
    print(text)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
