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
| M2 | WiFi STA associates + DHCP lease (credentials via `option_env!`, never committed) | **associate ✅ PROVEN on glass** (ch1, WPA2, aid 10909). DHCP unproven — first flash OOM-panicked before the lease; fixed, **awaiting reflash** |
| M3 | ESP-NOW hello/ack on the air (`--features radio`) | code drafted, **unflashed**. Compiles ✅. Needs `SPIKE_ESPNOW_ONLY=1` — see below, the AP and the mesh are on different channels |
| M4 | MQTT + retained HA discovery as node 162 | **code complete, UNFLASHED** — rides `--features wifi`; 10/10 gates |

## Feature tiers

Each tier is a superset of the one below. **Default is M1 only** — no radio, no
allocator, no network stack.

| build | tiers | what it adds |
|---|---|---|
| `cargo build --release` | M1 | bare metal: PSRAM, SPI2, panel, button, heartbeat |
| `--features wifi` | M1+M2+M4 | esp-radio · esp-rtos · esp-alloc · smoltcp · STA associate + DHCP + MQTT |
| `--features radio` | M1+M2+M3 | + `esp-radio/esp-now`, the SMOLv1 hello/ack probe |
| `SPIKE_ESPNOW_ONLY=1 … --features radio` | M1+M3 | radio up, channel pinned, **association skipped** |

**`radio` stacks on `wifi` rather than being orthogonal, and upstream forces
that**: esp-radio defines `esp-now = ["wifi", ...]`, so ESP-NOW cannot exist
without WiFi. It is also true on the silicon — ESP-NOW *is* WiFi, same radio,
same channel — which is the fact the whole coexistence story rests on.

### ⚠️ M3 needs `SPIKE_ESPNOW_ONLY=1` — the channels do not line up

```bash
SPIKE_ESPNOW_ONLY=1 ./build-remote.sh --features radio     # channel 6 (default)
```

**One radio, one channel.** A STA association owns the radio's channel, and M2's
flash glass-verified the AP on **channel 1** (`ssid jplovescl, bssid
9e:5c:8e:cb:db:90, channel 1`). The smol mesh is on **channel 6**. An associated
board therefore *cannot hear the mesh at all* — and the probe would report a dead
mesh while every part of it worked correctly, which is exactly the failure this
fleet once misread as a coexistence/physics problem.

So `--features radio` alone associates and broadcasts into ch1, which is useful
only if the mesh moves. `SPIKE_ESPNOW_ONLY=1` skips association and pins the
channel (default 6, override with `SPIKE_ESPNOW_CHANNEL`).

An earlier version of this README argued M3 should "run co-channel or not at all,
rather than hide the single-radio constraint phase 2 has to face". **That was
right about phase 2 and wrong about the probe.** Phase 2 does have to solve
co-channel operation; M3's job is to prove ESP-NOW reaches the mesh *that
exists*. Refusing to look until the network is rearranged is not rigour.

The channel is validated at COMPILE time (`1..=14`, the whole legal 2.4 GHz
space) — exercised in both directions: `SPIKE_ESPNOW_CHANNEL=99` and `=0` fail
the build with a named message, `=14` passes. A bad channel would otherwise be a
silent no-op on the air, indistinguishable from the dead-mesh symptom this mode
exists to rule out.

Verified by construction rather than by reading the manifest: `--features radio`
compiles while referencing `interfaces.esp_now`, a field esp-radio gates behind
`cfg(all(feature = "esp-now", feature = "unstable"))`; `--features wifi` compiles
without it. (`cargo tree -e features | grep esp-now` does **not** show this —
that grep comes back empty either way and is a blind instrument, not evidence.)

## M2 — credentials

The PSK is pulled from **Vaultwarden on katana at build time**, passed to the
remote cargo as an environment variable over ssh, and baked in by `option_env!`.
It is never written to disk on either host and never echoed into a log — only its
length is printed.

- Vault item: **`Homelab jplovescl WiFi (jplovescl SSID)`**
- SSID: **`jplovescl`** — the FT-off IoT SSID (VLAN 8) the **smol fleet** lives on.
- Convention and env var names (`SPIKE_WIFI_SSID` / `SPIKE_WIFI_PSK`) match
  `cyd-c5/spike/build-remote.sh`, so one operator habit covers both spikes.

⚠️ **Not emberburrito's SSID.** That board deliberately joins the *admin* VLAN,
because it is a hearth terminal talking to `hearthd` on katana's own subnet —
their product's network, not the fleet's. Never read `burrito-fw/wifi.local.toml`.

**A build with no credentials still compiles, flashes and runs**, printing
`no wifi credentials in this build` and leaving the M1 screens working. This is a
deliberate divergence from cyd-c5, which uses `env!` and hard-fails the build —
the person most likely to meet a locked vault is someone debugging something
else. A default (M1) build never touches the vault at all.

One limitation stated rather than papered over: on the no-credentials path
`set_config` is never called, and in esp-radio 0.18 `set_config` is what *starts*
the controller. So a credential-less `--features radio` build proves compilation
and boot, not the air.

## M4 — MQTT + retained HA discovery

Rides `--features wifi` (an association and a lease). It does **not** need
`radio`: MQTT is TCP over the STA interface, unrelated to ESP-NOW.

| | |
|---|---|
| Broker | `10.0.8.111:1883` — the HA VM's **VLAN8** leg, the lease's own subnet |
| Discovery (retained) | `homeassistant/sensor/smol_162/telemetry/config` |
| Telemetry | `smol/162/telemetry`, every 15 s, QoS 0, not retained |
| Payload | `up=<secs>s heap=<bytes>B beat=<n>` — bare, no id prefix (the topic carries it) |
| Model string | `smol ESP32-S3 CYD` — hand-written, **distinct** from the Ember label per **#396**'s interim rule; #396 owns the final string |

**Why that payload.** This spike has no sensors module — no temperature, no
battery read, no AP-info readback (that needs `esp-wifi-sys`, which M2 does not
depend on). Publishing a field we don't measure would be worse than publishing
fewer: *a plausible zero is harder to disbelieve than an absent field.* Uptime and
free heap are both things this build genuinely knows, and both are what a bring-up
rung actually wants — uptime proves it isn't silently rebooting, and **free heap is
the direct readout of the M2 OOM's blast radius.** If that number trends down over
an hour, the RX-pool question was not settled after all.

### Every network wait is bounded — a deliberate divergence from cyd-c5

The C5 spike waits for TCP-writable and for CONNACK in `loop { … }` with no
deadline, and `panic!`s on a bad CONNACK. On the failure this code is *most*
likely to meet — the wrong broker leg, where CONNACK never arrives — that spins
forever and the board stops heart-beating, which reads as a crash rather than a
misconfiguration.

Here every wait carries a deadline and every failure is a logged state
transition. **The no-CONNACK case prints the diagnosis**, because the signature is
specific and otherwise costs an afternoon:

```
[mqtt] ⚠️ NO CONNACK in 5000 ms, but TCP OPENED.
[mqtt]    That is the WRONG-BROKER-LEG signature: a cross-VLAN leg
[mqtt]    completes the handshake and silently drops the CONNACK.
[mqtt]    10.0.8.x -> 10.0.8.111 | 10.0.11.x -> 10.0.11.110 | 10.0.6.x -> 10.0.6.108
```

Same rule as `espnow_probe::send_bounded`: different hazard (a silent hang rather
than a CPU spin), identical discipline — **the heartbeat is the liveness signal
and nothing may take it hostage.**

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
- **Not a place for credentials.** M1 has no radio at all; M2's PSK arrives via
  `option_env!` at build time, from the vault. Nothing goes in the tree, and
  nothing is written to disk on either build host.

## Build & flash

Xtensa is Tier 3 and needs the **esp-rs compiler fork** — mainline rustup has no
`xtensa-esp32s3-none-elf`. Two lines, and **neither is on PATH by default**:

```bash
export PATH="$HOME/.cargo/bin:$PATH" && source ~/export-esp.sh
cd /home/jp/Projects/smol/targets/s3-cyd/spike

cargo check --release                      # M1
cargo check --release --features wifi      # M2
cargo check --release --features radio     # M3 (includes M2)
cargo run   --release                      # build + flash (goes through ./flash.sh)

# preferred: build on familiar (24 cores, espup pinned to match katana),
# ELF pulled back to the local target/ path so ./flash.sh finds it
./build-remote.sh                          # M1
./build-remote.sh --features wifi          # M2 — fetches the PSK from the vault
./build-remote.sh --features radio         # M3
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

### Build-time knobs

| env | default | what |
|---|---|---|
| `SPIKE_ESPNOW_ONLY=1` | off | skip association, pin a channel (M3) |
| `SPIKE_ESPNOW_CHANNEL` | `6` | channel to pin; validated `1..=14` at compile time |
| `SPIKE_HEAP_KB` | `96` | esp-radio heap. **`=64` reproduces the M2 OOM's original heap** — see below |

**The heap knob exists to keep an unproven fix from hiding.** M2's OOM was fixed
by two simultaneous changes (96 KiB heap *and* a continuous RX drain), and the
C5's counter-example differs from our failing build in *both* variables — so
neither datum isolates the cause. One flash settles it:

```bash
SPIKE_HEAP_KB=64 ./build-remote.sh --features wifi
```

DHCP completing at 64 KiB means the **cadence** was the fix and 96 KiB is margin;
still OOM-ing means the **heap** was load-bearing. The default stays 96 KiB
because a build meant to work should match smol's known-good pairing — but
headroom must not be allowed to launder an untested hypothesis.

### Diagnostics

The xtensa esp fork ships nightly features, so esp-radio's raw WiFi-driver
logging (`print-logs-from-driver`) **works on the S3** — it is unavailable on the
C5. Useful for watching RX arrival rates while chasing the OOM question above.
**Diagnostic builds only; never the committed default.**

## ⛔ The flash guard, continued

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

4. **The esp-radio heap must stay in internal RAM, and at 96 KiB.** S3 atomics
   silently misbehave in PSRAM, and the WiFi driver is full of them — so it
   cannot move. It also cannot shrink: **M2's first flash panicked at 64 KiB**
   with `memory allocation of 96 bytes failed`. The 96-byte request was the
   victim, not the culprit — the heap was already exhausted by esp-radio's
   demand-driven RX pool (`static_rx 16` / `dynamic_rx 40`). Those counts are
   smol's #140 tuning, copied correctly; **what was not copied was the 96 KiB
   heap they were sized against, nine lines above them in the same file.**
   Take that pairing whole or not at all.
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
src/net.rs           M2: creds, radio bring-up, STA associate, smoltcp DHCP
src/radio_dev.rs     smoltcp phy shim (SAME meaning as smol's + cyd-c5's file
                     of this name — do not repurpose it)
src/espnow_probe.rs  M3: ESP-NOW hello/ack probe, behind --features radio
```
