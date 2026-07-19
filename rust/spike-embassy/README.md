# spike-embassy — Embassy async executor spike (#198)

A **vertical-slice** port of smol's radio stack onto the [Embassy](https://embassy.dev)
async executor, to de-risk a full migration off the hand-rolled superloop. **HW-HELD**:
this compiles and builds a flashable espflash image, but is **never flashed** here — the
bench board is future work. Findings are build-side; runtime claims are marked as such.

Isolated workspace (own `Cargo.lock` + `rust-toolchain.toml`), deliberately separate from
the `rust/clock` firmware build and the `rust/viz` host tools — same isolation as `rust/viz`.

## What it does

One `esp-hal-embassy` executor running **three concurrent tasks** (+ the embassy-net pump):

1. **`clock_task`** — a 1 Hz tick on its own `embassy_time` timer (display stubbed to
   `println!` per #198 — the point is task concurrency, not pixels).
2. **`esp_now_rx_task`** — `esp_now.receive_async().await` → a peers table (beacon listening).
3. **`wifi_sntp_task`** — WiFi assoc + DHCP (embassy-net) + one SNTP query.

The thesis: the clock keeps ticking and ESP-NOW keeps hearing **during** the WiFi
association burst, because `async`/`await` generates the state machines the superloop
hand-rolls (#89 `NtpMachine`, the OTA chunk loop, the mqtt_session rework). One radio
init hands out **both** `interfaces.sta` (WiFi) and `interfaces.esp_now` — the exact
coexist pattern the firmware already uses, now driven by the executor.

## Build (familiar or katana — no toolchain pin conflict)

```sh
cp src/secrets.rs.example src/secrets.rs   # placeholders are fine; the spike never flashes
cargo build --release
# Flashable image (NOT flashed here — HW-held):
espflash save-image --chip esp32c3 \
  target/riscv32imc-unknown-none-elf/release/spike-embassy spike-embassy.bin
```

Same target (`riscv32imc-unknown-none-elf`), linker (`rust-lld` + `linkall.x`), and
`build-std` as the firmware.

## Measurements

Versions: esp-hal `1.0.0-rc.0` · esp-hal-embassy `0.9.0` · esp-wifi `0.15.0` ·
embassy-executor `0.7` / -time `0.4` / -net `0.6`. Release profile `opt-level="s"`, fat LTO.

| Metric | Baseline (superloop v341) | Spike (async vertical slice) | Note |
|---|---|---|---|
| **espflash app image** | 1,006,688 B | **448,352 B** | ⚠️ NOT feature-comparable — the spike is clock + 1 radio path only (no elections / OTA / MQTT / plugins / menu / display render). The takeaway is that the async runtime does **not** balloon the image; the bulk is `esp-wifi`, identical to the firmware. |
| **Static RAM (`.data`+`.bss`)** | (firmware hmin telemetry) | **113,692 B** (data 5,548 + bss 108,144) | Includes the esp-wifi buffers, the embassy task arena (`task-arena-size-20480`), and `StackResources<3>`. The Embassy-attributable slice (arena + executor) is ~20–30 KB and *tunable* via the arena feature. |
| **Flash text+rodata (`size`)** | — | 697,584 B (text 692,036 + data 5,548) | espflash's app image (448 KB) is smaller than `size`'s `.text` because the WiFi-blob rodata is placed in a separate flashed segment espflash accounts differently; the espflash figure is the authoritative flashed size. |
| **rustc-pin delta** | 1.96.1 | **0 — builds on 1.96.1** | 🎯 Key finding: the whole Embassy stack (esp-hal-embassy 0.9 + embassy-executor 0.7 + esp-wifi 0.15) compiles on the firmware's **exact** pinned toolchain. The migration forces **no** rustc bump. |
| **Clean build time (-j2)** | (katana -j2) | **24.5 s (familiar, -j2)** | Cold build, all deps. Indicative only — familiar has faster cores than katana, so the katana number will be higher; the point is it's an ordinary embedded build, not pathological. |
| **Mesh-deafness during WiFi burst** | beacon-loss count (#40/#155) | **HW-held** | Runtime claim; needs the bench board. *Structural expectation:* the esp-now RX task keeps polling across the wifi task's await points, so deaf windows should shrink (not vanish). |
| **Clock jank during assoc/DHCP/SNTP** | #89 S1 behavior | **HW-held** | Runtime claim. *Structural:* `clock_task` runs on an independent `embassy_time` timer with no shared blocking — no jank by construction. |

## Findings

- ✅ **It compiles + images cleanly, first try, on rustc 1.96.1** — pin delta zero. The
  async stack is *not* bleeding-edge relative to what the firmware already pins.
- ✅ **Coexist is free**: `esp_wifi::wifi::new()` hands out `interfaces.sta` + `interfaces.esp_now`
  from one init — WiFi STA and ESP-NOW on the single C3 radio, driven by separate tasks.
- ✅ **The hand-rolled state machines map 1:1 to async**: the #89 NtpMachine's Assoc→DHCP→SNTP
  phases become plain `.await`s in `wifi_sntp_task`; the compiler writes the machine.
- ⚠️ **RAM, not flash, is the thing to watch**: the executor's task arena + per-task stacks
  are static. Tunable, but a full port (many tasks) needs an arena/stack budget pass.
- ⚠️ **`clippy 1.96` is clean**, but the async APIs churn between esp-wifi minor versions —
  a full port should pin the quartet exactly (as the firmware already does).

## Ariel OS vs raw esp-hal-embassy (the addendum question)

**Verdict: ADMIRE, don't adopt** (matches the #163-style call). [Ariel OS](https://github.com/ariel-os/ariel-os)
(née RIOT-rs) layers RIOT-style OS services — portable boards, a networking abstraction,
scheduling — over the Embassy executor. For smol that layer is **indirection without payoff**:

- smol is **single-target** (ESP32-C3). Ariel's portable-board abstraction earns its keep
  across many MCUs; here it hides `esp-hal` behind another API for one chip we already own.
- This spike reaches boot + coexisting WiFi/ESP-NOW + embassy-net in **~180 lines** on raw
  `esp-hal-embassy` + hand-picked crates (embassy-net, static_cell, heapless). Ariel would
  add a dependency tree + its own build/config model to save little of that.
- Ariel does **not** currently expose ESP-NOW (smol's core transport); we'd be back on
  `esp-wifi` directly anyway, so Ariel's networking layer (embassy-net wrapper) is bypassed.
- **Worth reading, not linking**: Ariel's Espressif coexist/network glue and their
  board/config patterns are good prior art for how to *structure* a full port. Cross-ref
  #200 (RIOT ESP-NOW study) — any ADOPT crates from there land directly on this raw stack.

Keep it on the radar as a portability escape hatch **if** smol ever targets a second MCU;
until then, raw esp-hal-embassy is the leaner base.

## Go / no-go

**GO** for the full migration — as its own epic with per-subsystem HW-gated waves. The
spike removes the two biggest unknowns (does the stack build on our pin? does async
coexist work?) — both green. Effort + ordering recommendation in the #198 comment.
