# s3-cyd-spike

Phase-1 bring-up ladder for the **ES3C28P** (ESP32-S3 N16R8, 2.8" black-PCB CYD)
as a smol fleet target — **smol node id 162**, the new blank dev board.

## What this is

A **throwaway** four-milestone ladder that answers "does this board work, and does
our stack run on it?" one layer at a time. Each milestone is meant to be flashed,
looked at, and thrown away.

| Milestone | Proves | Status |
|---|---|---|
| **M1** | boots · console · octal PSRAM maps · panel paints in the right orientation · button reads | **FLASHED 2026-08-24 23:2x, RUNNING** — first guarded flash succeeded; serial heartbeat verified live (`[s3-cyd] heartbeat N — node 162 alive`, counter consistent with uptime). Panel orientation awaiting a human eyeball on the glass |
| M2 | WiFi STA joins (credentials via `option_env!`, never committed) | not started |
| M3 | ESP-NOW hello/ack on the air (`--features radio`) | code drafted, **unflashed**. Compile verdict: **✅ `wifi` + `esp-now` DO build together on esp32s3** (see below) |
| M4 | PSRAM framebuffer + a real smol screen | not started |

## The M3 compile verdict (answered 2026-08-24)

`src/radio_dev.rs` exists to answer one question before anything goes on the air:
**does esp-radio 0.18's `wifi` + `esp-now` pair compile and link on `esp32s3`?**
smol main proves that pair on **C3**; burrito-fw proves `wifi` alone on **S3**;
nothing proved the intersection.

```
cargo check  --release --features radio   -> exit 0
cargo clippy --release --features radio -- -D warnings   -> exit 0
```

**✅ YES.** S3 ESP-NOW is no longer an unknown at the compile layer. It is still
unproven **on the air** — that is what flashing M3 is for, and a compile verdict
is not a radio verdict.

Two notes that came out of running it:

- ⚠️ **`cargo clippy --release` without `--features radio` CANNOT SEE
  `radio_dev.rs`.** The module is `#[cfg(feature = "radio")]`, so the default
  lint run is structurally blind to it and returns a cheerful 0 having never
  looked. It found two real defects once the feature was on. **Run both.**
- The `ieee802154` feature-panic hazard that governs the C5/C6 work **does not
  apply here** — the S3 has no 802.15.4 radio, so that feature is not selectable
  for `esp32s3`. The S3's radio hazard is WiFi/BLE antenna contention (`coex`),
  which we deliberately do not enable.

## What this is NOT

- **Not burrito-fw.** emberburrito's firmware is a finished hearth terminal for a
  *different board* (node 161) that talks HTTP/WebSocket to `hearthd`. This spike
  borrows its **proven hardware lines** — pin map, MADCTL, PSRAM init, the
  landmine comments — and nothing else.
- **Not the phase-2 smol image.** No mesh, no election, no OTA, no MQTT, no
  id-block. When the ladder is done, that work starts in a real crate; this one
  gets deleted.
- **Not a place for credentials.** M1 has no radio at all. M2's WiFi will arrive
  through `option_env!` at build time. Nothing goes in the tree.

## Build & flash

Xtensa is Tier 3 and needs the **esp-rs compiler fork** — mainline rustup has no
`xtensa-esp32s3-none-elf`. Two lines, and **neither is on PATH by default**:

```bash
export PATH="$HOME/.cargo/bin:$PATH" && source ~/export-esp.sh
cd /home/jp/Projects/smol/targets/s3-cyd/spike

cargo check --release                      # M1
cargo check --release --features radio     # M3 probe
cargo run   --release                      # build + flash (goes through ./flash.sh)
```

**`--release` is mandatory, not a preference** — esp-hal's PSRAM init path only
maps correctly when optimised; a debug build reports 0 bytes and looks like a
hardware fault.

If you see `linker `xtensa-esp32s3-elf-gcc` not found`, you did **not** source
`~/export-esp.sh` in this shell. The toolchain is fine. See `rust-toolchain.toml`
for the second disguise this trap wears.

## ⛔ The flash guard

`cargo run` does **not** invoke espflash directly — `.cargo/config.toml` sets
`runner = "./flash.sh"`, which resolves the port **by serial** and **refuses by
default**.

**ARMED 2026-08-24** — `ALLOW_SERIAL = 14:C1:9F:D1:C8:10` (smol node id 162),
confirmed against the live bus at `/dev/ttyACM3`. The refuse-if-empty branch is
kept anyway, as the safe default for anyone cloning this pattern.

### ⛔ Why the comparison is byte-exact, and must stay that way

```
target (id 162)     14:C1:9F:D1:C8:10   <- the only sanctioned target
reliquary (SEALED)  14:C1:9F:D1:C3:C8   <- NEVER WRITE TO THIS
                    ^^^^^^^^^^^^ first four octets identical
```

They differ in the last two octets and both contain `C8`. **Nobody may ever
"fix" a serial mismatch by loosening the match to a prefix** — a prefix on this
bench matches the sealed board. If the guard says a serial is absent, find out
why; never widen the pattern until it matches. That widening is the exact motion
that destroys the vault unit, and it is what a frustrated person reaches for at
1am.

Four devices on this bus are the **same model** as this board (three live
ember.realm.watch satellites — one off-site at JP's dad's house — plus reliquary),
sharing the `28:84:85:44:*` prefix. A prefix match is not identity.

`flash.sh` also refuses `--baud` (this USB-JTAG link corrupts above default),
refuses a port another process holds (reporting the PID rather than killing it),
and has **no override flag**.

**All four refusal paths have been exercised, not just written** — empty allow,
allow-collides-with-deny, board absent, and `--baud` each refuse with exit 1 and
open no port; the armed happy path selects `/dev/ttyACM3` and correctly labels a
live deny-listed C6 watch sitting on `/dev/ttyACM2`. A gate that has never failed
is not known to work.

## The three landmines, in one place

They are commented at the code that would otherwise trip them; repeated here
because reading the source is not how anyone discovers a landmine.

1. **Never configure GPIO18.** Driving the FT6336's reset breaks the touch
   controller. The "it locks up I2C" story was derived from the schematic and
   never tested; the real rule is narrower. Its **absence from `main.rs` is the
   trick, not an oversight.**
2. **Never configure GPIO33–37.** Consumed by the octal PSRAM.
3. **Landscape MADCTL is `0x28`.** retro-go's `0x68` is `0x28` with MX set — a
   horizontal mirror. Copying it shipped mirror-writing text in burrito-fw v0.1.
   The upside-down escape hatch is `0xE8`, never a re-added mirror.

Plus two that only bite with the radio on:

4. **The esp-radio heap must stay in internal RAM.** S3 atomics silently
   misbehave in PSRAM, and the WiFi driver is full of them.
5. **Never call `EspNowSender::send()` and let the `SendWaiter` live or die.**
   Both `wait()` and its `Drop` are unbounded non-yielding spins on a private
   atomic (`esp-radio-0.18.0/src/esp_now/mod.rs:590` and `:604`) — one lost TX
   completion pins the CPU forever, and *not* calling `wait()` is the same spin
   spelled invisibly. `send_bounded()` in `radio_dev.rs` polls `send_async`'s
   `SendFuture` (which has **no** `Drop`, so abandoning it is free) against a
   30 ms deadline. The war story is on the function.

## Layout

```
Cargo.toml           own workspace root (xtensa/riscv cannot share one)
rust-toolchain.toml  channel = "esp" + the two-disguise trap
.cargo/config.toml   target, build-std, runner = ./flash.sh
flash.sh             THE FLASH GUARD — refuses by default
build-remote.sh      build on familiar (espup pinned 1.95.0.0), pull the ELF back
src/main.rs          M1: PSRAM, SPI2, ILI9341, colour test, GPIO0, heartbeat
src/radio_dev.rs     M3: ESP-NOW probe, behind --features radio
```
