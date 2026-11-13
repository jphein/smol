# NuttX on a bench ESP32-C3 — build-prep + flash notes

**Issue:** [#199](https://github.com/jphein/smol/issues/199) · **Lineage:** [#163 Babel](althea-babel-study.md) · [#181 ledger](mesh-ledger-study.md) · [#189 coverage](inspirations-coverage.md) · [#200 RIOT](riot-espnow-study.md) · **Status:** **BUILD-VERIFIED / FLASH-UNVERIFIED** — no bench board exists yet · **Author:** nebula-babel · **Date:** 2026-07-19

> ⚠️ **STATUS — read first.** The **build** is verified end-to-end (nuttx-13.0.0 + xPack
> riscv-none-elf-gcc 14.2.0, on `familiar`, a real `nuttx.bin` produced). The **flash +
> bring-up procedure below is UNVERIFIED** — it is derived from NuttX's own build output +
> config, *not* run against hardware (no bench board exists per #199's constraint). **Do not
> treat the flash steps as proven fact until a bench board confirms them.** PR is held for
> that reason.
>
> **The headline for bench day: flashing is a one-liner.** This config uses
> `CONFIG_ESPRESSIF_SIMPLE_BOOT=y` — **no separate 2nd-stage bootloader, no partition table**.
> The app image *is* the boot image at offset **0x0**:
> `esptool -c esp32c3 -p <port> write-flash 0x0 nuttx.bin`. That's the whole flash.
>
> **The build is a four-gotcha gauntlet — all four are now documented and fixed** (§2). The
> two that will bite anyone: NuttX now **requires `kconfiglib`, not `kconfig-frontends`** (the
> apt default silently drops a computed string default and the build dies with a cryptic
> `hal_.mk: No such file`), and the **Ubuntu `gcc-riscv64-unknown-elf` lacks newlib** (`sys/lock.h`)
> — you need the **xPack `riscv-none-elf-gcc`**. And: **build from a release tag
> (nuttx-13.0.0), not master** — master HEAD had a `bootloader_support` PSA-crypto drift.
>
> **ESP-NOW verdict (D4): richer than "they don't have one."** NuttX *does* ship ESP-NOW — an
> `esp_espnow_pktradio` driver for **Xtensa ESP32**, exposing it as a **pktradio + 6LoWPAN**
> (architecturally identical to [RIOT's](riot-espnow-study.md) netdev + 6LoWPAN, wrapping the
> *same* Espressif blob). The C3 has the `esp_now` blob present but no driver wired, so a C3
> port is **adapt the existing Xtensa driver**, not greenfield. → filed.

---

## 0. Environment (what was installed, for auditability/reversibility)

Built on **`familiar`** (Ubuntu 24.04, passwordless sudo). Everything is **outside the smol
repo**, in `~/nuttx-bench/` on familiar. Installed (standard, reversible dev tooling):

| Where | What | Note |
|---|---|---|
| apt | `kconfig-frontends gcc-riscv64-unknown-elf flex bison gperf libncurses-dev ninja-build` | kconfig-frontends turned out to be the *wrong* kconfig (see §2b); the riscv gcc lacks newlib (§2c) — both superseded below but harmless |
| pip --user | `kconfiglib` (14.1.0), `esptool` (5.3.1) | `~/.local/bin` — the **required** kconfig + the image tool |
| userspace tarball | **xPack riscv-none-elf-gcc 14.2.0** → `~/nuttx-bench/xpack-riscv-none-elf-gcc-14.2.0-3/` | the toolchain that actually works (newlib) |
| clones | `apache/nuttx` + `apache/nuttx-apps` @ **nuttx-13.0.0**; `espressif/esp-hal-3rdparty` @ pinned `b90b1837` | all under `~/nuttx-bench/`, never committed |

To remove: `sudo apt remove kconfig-frontends gcc-riscv64-unknown-elf` (optional) + `rm -rf
~/nuttx-bench`. **`familiar`'s `/tmp` is a 512 MB tmpfs (shared with llama.cpp) and was 100%
full** — the build needs `TMPDIR` pointed at disk (§2a).

---

## 1. The verified build — copy-paste sequence

```bash
# on familiar, all under ~/nuttx-bench (OUT of any repo)
export TMPDIR=~/nuttx-bench/tmp && mkdir -p "$TMPDIR"           # (§2a: /tmp tmpfs is full)
export PATH=~/nuttx-bench/xpack-riscv-none-elf-gcc-14.2.0-3/bin:~/.local/bin:$PATH
#   ^ xPack riscv-none-elf-gcc (newlib) + kconfiglib's `menuconfig` on PATH (both required)

# repos at a RELEASE TAG (not master — §2d)
git -C nuttx  checkout nuttx-13.0.0
git -C apps   checkout nuttx-13.0.0
# esp-hal-3rdparty at the pinned SHA (auto-clones, or stage manually — §2b):
#   into nuttx/arch/risc-v/src/esp32c3/esp-hal-3rdparty @ b90b1837…

cd nuttx
make distclean
./tools/configure.sh esp32c3-devkit:nsh          # (esp32c3-"generic" is now esp32c3-devkit — §2e)
# switch toolchain: GNU_RV64 (Ubuntu, no newlib) → GNU_RVG (auto-prefers xPack riscv-none-elf-)
sed -i 's/^CONFIG_RISCV_TOOLCHAIN_GNU_RV64=y/CONFIG_RISCV_TOOLCHAIN_GNU_RVG=y/' .config
sed -i '/# CONFIG_RISCV_TOOLCHAIN_GNU_RVG is not set/d' .config
make olddefconfig                                # kconfiglib now materializes CHIP_SERIES
make -j4                                          # → nuttx (ELF), nuttx.hex, nuttx.bin
```

**Result (D1/D3, measured):** `nuttx.bin` = **242,872 B (~237 KB)**; ELF `nuttx` = 403 KB;
sections **text 146,814 / data 48,128 / bss 155,102** → **~199 KB static RAM** (data+bss) of
the C3's ~400 KB, leaving ~200 KB for heap/stack. Clean `-j4` build (post-configure) = **79 s**
on familiar. Boot time: **UNVERIFIED** (needs hardware).

**vs smol's ~1 MB image:** this `nsh` config is **~237 KB** — but note it's a *minimal shell*
(`esp32c3-devkit:nsh`) with **WiFi not enabled**; the `esp32c3-devkit:wifi` / `sta_softap`
configs (which pull the same esp-wifi blobs smol carries) will be substantially larger and are
the fair comparison for a networked bench tool.

---

## 2. The four (well, five) gotchas — paint-by-numbers

**a) `/tmp` tmpfs full → cryptic "No space left on device".** familiar's `/tmp` is a 512 MB
tmpfs shared with llama.cpp, sitting at 100%. gcc/host-tool temp files land there and the very
first host-tool compile (`incdir`) dies. **Fix:** `export TMPDIR=~/nuttx-bench/tmp` (disk-backed).

**b) kconfig-frontends silently drops a computed string default → `chip/hal_.mk: No such file`.**
`ESPRESSIF_CHIP_SERIES` is a non-prompt string with `default "esp32c3" if ARCH_CHIP_ESP32C3`;
NuttX's Makefile does a hard `include chip/hal_${CHIP_SERIES}.mk`. The apt `kconfig-frontends`
`kconfig-conf --olddefconfig` **doesn't emit** that symbol → `CHIP_SERIES` empty → the include
resolves to `hal_.mk` and the build dies at parse. **Fix:** install **kconfiglib**
(`pip install --user kconfiglib`) and ensure its `menuconfig` is on PATH — NuttX's
`tools/Unix.mk` auto-selects the kconfiglib branch iff `command -v menuconfig` succeeds, and
kconfiglib *does* materialize the computed default. (This is now the documented NuttX
requirement; kconfig-frontends is deprecated.)

**c) Ubuntu `gcc-riscv64-unknown-elf` has no newlib → `fatal error: sys/lock.h`.** The apt riscv
gcc is a bare compiler without libc headers; NuttX's `platform_include/sys/lock.h` does an
`#include_next <sys/lock.h>` that finds nothing. **Fix:** use the **xPack `riscv-none-elf-gcc`**
(bundles newlib) and switch the NuttX toolchain choice `GNU_RV64` → `GNU_RVG` (Toolchain.defs
auto-prefers `riscv-none-elf-` when it's on PATH).

**d) master HEAD `bootloader_support` PSA-crypto drift → `fatal error: psa/crypto.h`.** On
`master`, the app build compiles `esp-hal-3rdparty`'s `bootloader_sha.c`, which `#include`s
`psa/crypto.h` (mbedtls PSA) that the NuttX build doesn't provide. **Fix:** build from a
**release tag** (`nuttx-13.0.0` — nuttx + apps together), which is a coherent (nuttx, apps,
3rdparty) triple; the same pinned 3rdparty compiles cleanly there.

**e) `esp32c3-generic` no longer exists → use `esp32c3-devkit`.** #199 says "esp32c3-generic";
the new board tree (`boards/risc-v/esp32c3/`) has `esp32c3-devkit`, `esp32c3-xiao`,
`esp32-c3-zero` (the old `esp32c3-generic` was the *legacy* tree). For a supermini,
**`esp32c3-xiao`** is the closest match — and notably it ships a **`usbnsh`** config (nsh over
**native USB-CDC**, no external serial adapter), ideal for a headless $1 board.

---

## 3. Flash + recovery — FLASH-UNVERIFIED (derived from build output, not hardware)

The build's own MKIMAGE step runs:
`esptool.py -c esp32c3 elf2image --ram-only-header -fs 4MB -fm dio -ff 80m -o nuttx.bin nuttx`
and the config is **`CONFIG_ESPRESSIF_SIMPLE_BOOT=y`** (BL_OFFSET/APP_OFFSET both `0x0`). So:

**Flash (single image, offset 0x0 — no bootloader/partition-table):**
```bash
# put the board in download mode if needed: hold BOOT (GPIO9), tap RESET, release BOOT
esptool -c esp32c3 -p /dev/ttyACM0 -b 460800 write-flash 0x0 nuttx.bin
#   (esptool v5: 'esptool'/'write-flash'; 'esptool.py'/'write_flash' are deprecated aliases)
```

**nsh over serial:**
- `esp32c3-devkit:nsh` → nsh on **UART0** (the USB-serial bridge / an external adapter): `115200 8N1`.
  `picocom -b 115200 /dev/ttyACM0` (or `screen`).
- `esp32c3-xiao:usbnsh` → nsh on the **native USB-CDC** (no adapter): after flash, the board
  re-enumerates as a CDC-ACM `/dev/ttyACM*`; open it at any baud. **Best fit for a headless
  supermini.**

**Recovery (bench board only — NEVER a fleet/unknown board):**
```bash
esptool -c esp32c3 -p /dev/ttyACM0 erase-flash      # wipe
esptool -c esp32c3 -p /dev/ttyACM0 write-flash 0x0 nuttx.bin   # re-flash
# bricked/no-enumerate → force download mode (hold BOOT, tap RESET) then re-flash
```
The ESP32-C3 mask ROM download mode is unbrickable-by-flash-content — a bad app can always be
erased/re-flashed from download mode, so the bench board is safe to experiment on.

**⚠️ Every offset, baud, port, and the download-mode chord above MUST be confirmed on the first
bench board before this section is treated as fact.** (E.g. `esp32c3-xiao:usbnsh` boot-to-CDC
timing and whether SIMPLE_BOOT-at-0x0 needs `--ram-only-header` semantics honored by the flash
step are exactly the things hardware will confirm or correct.)

---

## 4. D4 — ESP-NOW on NuttX: feasibility (verified from source)

**NuttX already has ESP-NOW — but only on Xtensa ESP32.** `arch/xtensa/src/common/espressif/esp_espnow_pktradio.{c,h}`
wraps the Espressif blob (`esp_now_init`/`esp_now_send`/`esp_now_recv_cb`) into a NuttX
**`pktradio`** (`nuttx/wireless/pktradio.h`) with **6LoWPAN** on top (`nuttx/net/sixlowpan.h`,
`sixlowpan_reassbuf_s`). Enabled by `CONFIG_ESPRESSIF_ESPNOW_PKTRADIO=y` (+ `WIRELESS_PKTRADIO`,
`PKTRADIO_ADDRLEN=2`, a board `espnow` config exists for `esp32-devkitc`).

**This is architecturally identical to [RIOT's](riot-espnow-study.md) model** (ESP-NOW as an
L2 interface under 6LoWPAN, wrapping the same blob) — a strong cross-confirmation that
"ESP-NOW-as-an-L2-under-6LoWPAN" is the standard RTOS approach (the one smol deliberately skips
in favor of app-layer framing).

**On the C3 (RISC-V):** the `esp_now` blob **is present** (`arch/risc-v/src/esp32c3/esp-hal-3rdparty/components/esp_wifi/include/esp_now.h`),
and the WiFi netdev base is shared (`arch/risc-v/src/common/espressif/esp_wlan_netdev.c`,
`esp_wifi_api.c`) — but **no ESP-NOW driver is wired**. So a C3 ESP-NOW driver = **port the
existing Xtensa `esp_espnow_pktradio` onto the RISC-V `common/espressif` base**, not greenfield.

**Verdict / effort:**
- **ADOPT-lite (as a rig tool, not smol firmware) → filed.** A C3 running NuttX + a ported
  ESP-NOW pktradio = a scriptable mesh sniffer/injector (POSIX FS + sockets + shell). Effort is
  **moderate** (reference driver exists, blob present, WiFi base shared) — *not* the "write from
  scratch" the issue assumed. If the port is done, the bench board can `cat`/`printf` frames
  to/from the smol mesh from a shell — genuinely useful for #26/07-14-class forensics.
- **SKIP for smol firmware.** smol is no_std Rust bare-metal; NuttX's pktradio+6LoWPAN is the
  opposite tradeoff. This is a *bench instrument* verdict, not a smol-architecture one.

---

## 5. D5 — NuttX vs MicroPython as the scriptable bench instrument

The real question #199 poses: for rig work (packet probes, HTTP pulls from VLAN8-like vantages,
ESP-NOW sniff/inject), is a **POSIX shell** (NuttX) or **MicroPython** the better bench tool?

| Criterion | NuttX (nsh) | MicroPython |
|---|---|---|
| ESP-NOW | needs the C3 driver port (§4) | **native `espnow` module** (import espnow; send/recv today) |
| Scripting loop | C programs / nsh builtins (compile-flash cycle) | **REPL + `.py` files** — edit-run, no reflash |
| Sockets / HTTP pull | POSIX `socket()` + real TCP/IP | `socket` / `urequests` — higher-level |
| Filesystem | real VFS (`open/read`, SmartFS/littlefs) | `os`/`open` on the internal FS |
| WiFi STA | esp_wlan_netdev (blob) | `network.WLAN` — trivial |
| Iteration speed | **slow** (cross-compile + flash per change) | **fast** (paste into REPL) |
| Fidelity to smol | closer to bare-metal semantics | higher-level, hides the radio |
| Effort to first useful probe | high (build gauntlet §2 + ESP-NOW port) | **low** (flash MicroPython, `import espnow`) |

**Recommended criteria + verdict (to test on the bench board):**
1. **If the job is ESP-NOW sniff/inject for smol forensics → MicroPython wins now.** Its native
   `espnow` module means a scriptable mesh sniffer/injector is a 10-line REPL script *today*,
   vs NuttX needing the driver port (§4). This directly serves the #26/07-14 unicast forensics.
2. **If the job is POSIX-fidelity experiments** (real `pthread`/VFS/socket semantics as an
   education or a bare-metal-adjacent test harness) → **NuttX wins** — it's a genuine POSIX RTOS,
   MicroPython is not.
3. **Iteration speed strongly favors MicroPython** for ad-hoc probing (REPL vs compile-flash).

**Net:** as a *scriptable bench instrument for smol's mesh*, **MicroPython is the faster path to
value** (native espnow, REPL); NuttX is the better *POSIX playground* and becomes the better
*mesh instrument* only if/when the C3 ESP-NOW pktradio port (§4) is done. → both filed as options.

---

## 6. Findings → issues
- **N1 — [#207](https://github.com/jphein/smol/issues/207)** — port the Xtensa `esp_espnow_pktradio` to esp32c3 (RISC-V) so a NuttX bench board is a scriptable ESP-NOW sniffer/injector. Moderate effort (reference driver + blob present).
- **N2 — [#208](https://github.com/jphein/smol/issues/208)** — evaluate MicroPython (native `espnow`) as the scriptable bench instrument — likely the faster path to a mesh sniffer/injector than the NuttX port; bench-board head-to-head.

(No smol *firmware* issues — NuttX/MicroPython are bench instruments, not fleet members, per #199.)

---

## 7. Executive summary
NuttX builds and runs on a $1 ESP32-C3, and this note makes bench-day paint-by-numbers: a real
`nuttx.bin` (**~237 KB**, ~199 KB static RAM of 400 KB, 79 s build) was produced on familiar
with **nuttx-13.0.0 + xPack riscv-none-elf-gcc 14.2.0**, and flashing is a **single
`esptool write-flash 0x0 nuttx.bin`** (SIMPLE_BOOT — no bootloader/partition-table). The path
has four sharp gotchas, all now fixed in §2 (the two that will bite: **kconfiglib is required,
not kconfig-frontends**; and **use xPack riscv-none-elf-gcc, not Ubuntu's newlib-less riscv gcc**
— plus **build from a release tag, not master**). On ESP-NOW, NuttX is richer than assumed: it
ships an `esp_espnow_pktradio` (pktradio + 6LoWPAN, same shape as RIOT) for Xtensa ESP32, and a
C3 port is an adaptation, not greenfield. And for the actual rig goal — a scriptable mesh
sniffer/injector — **MicroPython's native `espnow` module is the faster path to value**, with
NuttX the better POSIX playground. **The flash/bring-up section is UNVERIFIED and must be
confirmed on the first bench board before it is trusted; the PR is held for that.**
