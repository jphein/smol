# smol — ESP32-C3 clock (Rust, `no_std`)

A `no_std` bare-metal Rust firmware for **"smol"**: an ESP32-C3 SuperMini with a
0.42" SSD1306 OLED (72×40, I²C). It shows a clock, syncs real time over WiFi via
SNTP, does ESP-NOW peer messaging with an honest WiFi↔ESP-NOW radio switch, and
reads two on-board sensors (chip die-temperature + battery voltage via ADC) onto
the display.

Built on `esp-hal` 1.0 (RISC-V bare metal — **not** ESP-IDF/std).

---

## What compiles today

All three phases build cleanly with `cargo build --release` (zero warnings):

| Phase | Feature flag        | What it does                                                        | Status                    |
|-------|---------------------|---------------------------------------------------------------------|---------------------------|
| 1     | *(default)*         | Clock on the OLED, free-running from a compile-time start constant   | ✅ compiles + links         |
| 2     | `--features wifi`   | + WiFi STA, DHCP, SNTP real-time sync (raw `smoltcp`, blocking)      | ✅ compiles + links         |
| 3     | `--features espnow` | + WiFi/NTP burst, ESP-NOW HELLO/ACK peer handshake, blue-LED state machine, WiFi↔ESP-NOW time-share | ✅ **flashed + verified on HW** |

`espnow` implies `wifi`. Every phase produces a valid RISC-V
(`riscv32imc-unknown-none-elf`) ELF.

### Build commands

```bash
. "$HOME/.cargo/env"          # put cargo on PATH
cd rust/clock

cargo build --release                       # Phase 1
cargo build --release --features wifi       # Phase 2
cargo build --release --features espnow     # Phase 3
```

### Honest status of each phase

* **Phase 1** is complete and self-contained: I²C on GPIO5/6, `ssd1306`
  `DisplaySize72x40`, `embedded-graphics` text, `HH:MM:SS` counting once per
  second off a blocking `Delay`.
* **Phase 2** is a full blocking WiFi→DHCP→SNTP path written directly against
  `smoltcp` (no async executor, no git-only crates). The **`wifi`-only** binary
  hasn't itself been flashed, but its `run_ntp_burst` is the *same* code the
  hardware-verified `espnow` build runs, and there it associates, gets a DHCP
  lease, and returns correct UTC time (see
  [Hardware-verified behaviour](#hardware-verified-behaviour)). Needs real WiFi
  credentials — see [Configuration](#configuration).
* **Phase 3** implements the ESP-NOW HELLO/ACK peer handshake, an explicit
  `RadioManager` with a `Mode` enum + `switch()` covering **both** single-radio
  strategies (coexist / time-share), and the blue-LED peer-state machine. It is
  **flashed and verified on hardware** (see [Hardware-verified behaviour](#hardware-verified-behaviour)):
  at boot it runs a **real** WiFi → DHCP → SNTP burst (the STA `WifiDevice` and
  the ESP-NOW handle both come from the same `Interfaces`; the burst drives the
  STA device, then drops it before pinning the ESP-NOW channel, so the single
  radio is never double-driven), syncs real UTC time, then time-shares the radio
  to ESP-NOW on a fixed channel and runs the clock + handshake loop.

---

## Pinned crate versions (and WHY)

The `esp-hal` / `esp-wifi` / `esp-alloc` cluster is extremely version-sensitive
because **esp-wifi links against esp-hal *internal* APIs**, and its Cargo
requirement (`esp-hal ^1.0.0-rc.0`) is *looser than what actually compiles*.

**The specific gotcha that bit this build:** `esp-wifi 0.15.x` calls
`esp_hal::rng::Rng::new(peripherals.RNG)` internally. In **esp-hal 1.0.0-rc.1**
and the **1.0.0** release, `Rng::new()` was changed to take **no argument**.
Cargo's semver check still passes (1.0.0 satisfies `^1.0.0-rc.0`), but esp-wifi
then fails to compile with:

```
error[E0061]: this function takes 0 arguments but 1 argument was supplied
   --> esp-wifi-0.15.0/src/common_adapter/mod.rs
    | let mut rng = hal::rng::Rng::new(... RNG::steal());
```

So we pin esp-hal to the **exact release esp-wifi 0.15.x was built against** —
the 2025‑07‑16 cluster, where `Rng::new(RNG)` still takes the peripheral:

| Crate             | Version       | Notes                                                        |
|-------------------|---------------|--------------------------------------------------------------|
| `esp-hal`         | `=1.0.0-rc.0` | `Rng::new(RNG)` — matches esp-wifi. `esp32c3` + `unstable`.  |
| `esp-wifi`        | `=0.15.0`     | WiFi + ESP-NOW. `builtin-scheduler` re-enabled (see below).  |
| `esp-alloc`       | `=0.8.0`      | esp-wifi 0.15 requires `esp-alloc ^0.8.0`.                   |
| `esp-backtrace`   | `=0.17.0`     | panic + backtrace (2025‑07‑16 sibling).                     |
| `esp-println`     | `=0.15.0`     | logging over JTAG/UART.                                       |
| `ssd1306`         | `=0.10.0`     | has `DisplaySize72x40`; targets `embedded-hal` 1.0.          |
| `embedded-graphics` | `=0.8.1`    | text rendering.                                              |
| `smoltcp`         | `=0.12.0`     | version esp-wifi 0.15 uses; DHCP + UDP for SNTP.            |

Two more non-obvious flags that were required to link:

* **`esp-wifi/builtin-scheduler`** — esp-wifi's os-adapter references
  `esp_wifi_preempt_*` symbols provided by a preemption scheduler. That feature
  is in esp-wifi's *default* set, but we use `default-features = false`, so it
  must be re-added or the link fails with `undefined symbol: esp_wifi_preempt_*`.
* **esp-backtrace without `exception-handler`** — esp-hal's default features
  already install the exception handler; enabling esp-backtrace's copy too gives
  a duplicate `ExceptionHandler` symbol and an LTO bitcode-load failure.

### Toolchain / build config

* `rust-toolchain.toml` pins Rust `1.96.1` + target `riscv32imc-unknown-none-elf`
  (the stock RISC-V target — no Xtensa compiler fork needed for the C3).
* `.cargo/config.toml` sets the target, `build-std = ["core", "alloc"]`, and the
  esp-hal linker script (`-Tlinkall.x`).
* **Host-toolchain note:** this build box had a non-compiler `cc` shim earlier on
  `PATH`, so `.cargo/config.toml` pins `gcc` as the host linker
  (`[target.x86_64-unknown-linux-gnu] linker = "gcc"`) and `rust-lld` for the
  RISC-V target. On a normal box these lines are harmless; keep them if `cc` is
  unusual, drop them otherwise.

---

## Hardware wiring

| Signal | ESP32-C3 pin | OLED |
|--------|--------------|------|
| SDA    | GPIO5        | SDA  |
| SCL    | GPIO6        | SCL  |
| VCC    | 3V3          | VCC  |
| GND    | GND          | GND  |

I²C address `0x3C`, 400 kHz. Panel: 0.42" SSD1306, 72×40 visible window.

The blue status LED is on **GPIO8** and the battery-voltage ADC read is on
**GPIO4** (via an external divider — see [Sensors](#sensors)). Free-pin map:
GPIO5/6 = OLED I²C, GPIO8 = LED, GPIO9 = BOOT, GPIO2 = a strapping pin — leaving
GPIO0/1/3/4 as the safe ADC1 inputs (GPIO4 is used here).

### 72×40 offset

The SSD1306 controller has 128×64 RAM; this glass only exposes a 72×40 window at
a hardware offset. The `ssd1306` crate's `DisplaySize72x40` encodes this as
`OFFSETX = 28, OFFSETY = 0` (verified in the crate source), so `embedded-graphics`
coordinates `(0,0)..(72,40)` map onto the visible area — no manual offset needed.

> Calibration note: some 0.42" modules also want a small **vertical** offset
> (community drivers that bypass the crate often use `y ≈ 7`). If the top/bottom
> rows look clipped on your specific panel, that's the knob to nudge. This build
> trusts the crate's built-in `OFFSETY = 0`, which is correct for the common
> variant.

---

## Sensors

Two on-board readouts are sampled once per second and shown on the OLED's bottom
line (all three phases). They live in [`src/sensors.rs`](src/sensors.rs) and are
compiled into **every** build (Phase 1+) — no feature flag needed.

The bottom line **alternates every ~4 s** between its normal label (in Phase 3,
the last ESP-NOW peer message; otherwise `"smol"`) and a compact sensor readout
like `23C 3.9V`. The big `HH:MM` clock at the top is never touched, and the
sensor string (~8–10 chars in `FONT_5X8`) stays well inside the 72 px width. The
full detail, including the rough battery **percentage**, is also logged once per
second at `debug` level (`smol: chip 23C, batt 3.94V (~71%)`) — build with
`ESP_LOG=debug` to see it on the serial console.

### Chip temperature (internal — NOT ambient)

Uses the C3's on-chip temperature sensor via esp-hal's **`esp_hal::tsens`**
module (`TemperatureSensor::new(peripherals.TSENS, Config::default())`, read with
`get_temperature().to_celsius()`). Its measuring range is −40..125 °C.

> **This is the die temperature, not the room.** The silicon self-heats from the
> CPU clock and I/O load, so it typically reads **several degrees above ambient**.
> Treat it as a rough "how hot is the chip" gauge — the OLED labels it plainly as
> the chip temp (e.g. `23C`), not as an ambient thermometer. It is uncalibrated
> (esp-hal 1.0.0-rc.0 does not yet apply the per-range calibration offset).

### Battery voltage via ADC (needs an external divider)

One **ADC1 oneshot** read on **`GPIO4`** (ADC1 channel 4), at 11 dB attenuation
for the widest input range. The pin, divider ratio and curve are documented
consts in [`src/sensors.rs`](src/sensors.rs):

| Const              | Value  | Meaning                                                        |
|--------------------|--------|----------------------------------------------------------------|
| `BATT_ADC_GPIO`    | `4`    | GPIO / ADC1 channel used for the battery read.                 |
| `BATT_DIVIDER`     | `2.0`  | External divider ratio `Vbatt / Vpin` (e.g. 100 kΩ / 100 kΩ).  |
| `ADC_FULL_SCALE_V` | `3.3`  | Uncalibrated nominal: code 4095 ≈ 3.3 V at the pin.            |
| `BATT_EMPTY_V`     | `3.3`  | 1S-LiPo empty → 0 %.                                            |
| `BATT_FULL_V`      | `4.2`  | 1S-LiPo full → 100 %.                                          |

**Conversion.** The 12-bit code (0..4095) → pin volts →  battery volts → %:

```text
v_pin  = (raw / 4095) * ADC_FULL_SCALE_V      # ~0..3.3 V at the pin
v_batt = v_pin * BATT_DIVIDER                 # undo the external divider
```

**Percentage (rough).** Linear between the two 1S-LiPo endpoints, clamped:

```text
pct = clamp( (v_batt - 3.3) / (4.2 - 3.3) * 100 , 0 , 100 )
       3.30 V → 0 %      3.75 V → 50 %      4.20 V → 100 %
```

This is a deliberately crude linear map — a real LiPo discharge curve is flatter
in the middle — but it is fine for a "roughly how full" readout.

> ⚠️ **You must wire a resistor divider from the battery + to `GPIO4`.**
> A 1S LiPo reaches 4.2 V, which is above the C3's ~3.3 V ADC ceiling, so the
> battery **must** be divided down (the `2.0` default halves it to ≤2.1 V at the
> pin). **With no divider wired, `GPIO4` floats and the voltage/percentage are
> meaningless** (whatever charge is on the floating pin). The absolute voltage is
> also only a ballpark: the ADC is uncalibrated (esp-hal 1.0.0-rc.0 does not apply
> the eFuse calibration), so expect the reading to be a few percent off even with
> a good divider.

> **Not hardware-verified.** These sensors are **build-verified only** — the code
> compiles and links into all three phases, but it was **not flashed** (the boards
> are currently running the game). In particular the battery path has never been
> exercised against a real divider + cell, and the chip-temperature absolute value
> was not checked against a reference.

---

## Configuration

### WiFi credentials (git-ignored — the repo is PUBLIC)

WiFi credentials live in **`src/secrets.rs`**, which is **git-ignored**
(`.gitignore`: `**/secrets.rs`) so real credentials never land in this public
repo. A tracked template `src/secrets.rs.example` holds placeholders. On a fresh
clone:

```bash
cd rust/clock
cp src/secrets.rs.example src/secrets.rs      # then edit with your SSID/password
```

`src/secrets.rs` exposes two consts consumed by the `wifi`/`espnow` builds:

```rust
pub const WIFI_SSID: &str = "your-ssid";
pub const WIFI_PASS: &str = "your-password";
```

The Phase-1 (default) build needs no WiFi and does not compile the secrets module.

> ⚠️ Never `git add` `src/secrets.rs`. Confirm with
> `git check-ignore rust/clock/src/secrets.rs` (should echo the path) and check
> `git status` does not list it before committing.

### Other compile-time settings

* Clock start (Phase-1 fallback) — `START_SECONDS_OF_DAY` in `src/main.rs`.
* This unit's ESP-NOW id — passed to `net::mode::start(..., 7, ..)` in `src/main.rs`
  (each board on the mesh should get a **distinct** id, 0–255).
* Fixed ESP-NOW channel (time-share mode) — `ESP_NOW_FIXED_CHANNEL` in
  `src/net/mode.rs` (default 6; all peers must agree).
* Peer staleness window — `PEER_STALE_MS` in `src/net/mode.rs` (default 3000 ms).
* Blue-LED polarity — `LED_ACTIVE_LOW` in `src/led.rs` (default `true`).
* NTP server — `NTP_SERVER_IP` in `src/net/wifi.rs` (default: Cloudflare NTP
  anycast `162.159.200.123`; hardcoded IP so no DNS resolver is needed).
* Battery ADC pin / divider / LiPo curve — `BATT_ADC_GPIO`, `BATT_DIVIDER`,
  `BATT_EMPTY_V`, `BATT_FULL_V` in `src/sensors.rs` (see [Sensors](#sensors)).

---

## WiFi ↔ ESP-NOW switching design (single-radio reality)

**The ESP32-C3 has exactly ONE 2.4 GHz radio and one PHY, tunable to exactly ONE
channel at a time.** WiFi (infrastructure STA) and ESP-NOW are not two radios —
they are two ways of using the same PHY. This is a hard physical constraint, not
a software limitation, and the firmware is built around it honestly (see the long
doc comment at the top of `src/net/mode.rs`).

Consequences:

* While associated to an AP, the radio **must** sit on that AP's channel.
  ESP-NOW frames still work, but only on that same channel — every peer must be
  on the AP's channel.
* ESP-NOW is connectionless and channel-specific: a receiver only hears frames
  on the channel it is currently tuned to.

`RadioManager` in `src/net/mode.rs` exposes a `Mode` enum and `switch(mode)` that
implements **both** honest options:

* **`Mode::WifiSta` → COEXIST.** Stay associated; ESP-NOW rides the AP's channel.
  *Pro:* WiFi (NTP/weather) stays available. *Con:* peers must discover/match the
  AP's channel (which can change via band-steering), and DTIM power-save adds
  ESP-NOW RX latency.
* **`Mode::EspNow` → TIME-SHARE.** Drop the WiFi association to free the air, then
  pin the PHY to a fixed, well-known channel (`ESP_NOW_FIXED_CHANNEL`) all peers
  agree on. *Pro:* deterministic channel, lower power. *Con:* no WiFi while in
  ESP-NOW mode; re-syncing time means another WiFi burst.

The default `main` flow (Phase 3) uses **TIME-SHARE**: `net::mode::start()` brings
the radio up in STA mode, runs a real WiFi→DHCP→SNTP burst, then `switch(Mode::EspNow)`
pins the fixed channel and the clock loop runs the peer handshake (below) while
displaying the most recent peer activity on the OLED's bottom line. Because
esp-wifi's `init` must run exactly once, `RadioManager` initialises the radio a
single time and keeps both the `WifiController` and the `EspNow` handle alive for
the program's lifetime — "switching" chooses which stack is serviced and retunes
the channel; it never re-inits the radio.

---

## Blue status LED (GPIO8) + peer handshake

The onboard **blue LED on GPIO8** is a four-state indicator of WiFi/NTP progress
and ESP-NOW peer link status. GPIO8 on the C3 SuperMini is **active-low** (driving
it LOW lights it); this polarity is a single documented const,
[`LED_ACTIVE_LOW`](src/led.rs) (`true`), and everything else in `src/led.rs` works
in logical on/off. GPIO8 is also a boot **strapping pin** — the firmware creates
the `Output` initialised to the logical-OFF level and only drives it *after* boot,
so it is never held low through a reset.

### The four states (distinct blink rates, tellable apart by eye)

| State          | LED behaviour       | Meaning                                                                 |
|----------------|---------------------|-------------------------------------------------------------------------|
| **WiFi/NTP sync** | **FAST blink ~10 Hz** (50 ms on/off) | WiFi associating / DHCP / SNTP in progress (the boot-time burst) |
| **Idle**          | **OFF**            | No ESP-NOW peer heard (last peer beacon older than `PEER_STALE_MS` ≈ 3 s) |
| **Peer detected** | **SLOW blink ~2 Hz** (250 ms on/off) | Heard a peer `HELLO` beacon, but the two-way link is **not** yet confirmed |
| **Connected**     | **SOLID on**       | Bidirectional handshake confirmed (we heard the peer **and** hold an `ACK` proving the peer heard us) |

The 10 Hz vs 2 Hz split is a 5× separation, so "still connecting" and "found a
peer" are obvious without counting flashes. WiFi/NTP normally runs first at boot,
so the fast blink appears immediately; once NTP finishes, the loop falls through
to the peer states (off → slow → solid). Blinking is derived from a monotonic
millisecond clock (a square wave), not a blocking sleep, so the OLED clock keeps
rendering and the LED never stalls it.

### The handshake (why "detected" ≠ "connected")

ESP-NOW is connectionless: a broadcast tells you nothing about who received it. To
*honestly* distinguish "I can hear a peer" from "a peer and I have a working link",
each unit runs a tiny explicit handshake on top of ESP-NOW broadcasts (see the
long comment in `src/net/mode.rs`):

* Every unit periodically **broadcasts** a `HELLO` beacon carrying its own id
  (`"SMOLv1 HELLO NNN"`, ~every 2 s).
* On hearing unit A's `HELLO`, unit B registers A as a peer and replies with a
  **unicast** `ACK` echoing A's id (`"SMOLv1 ACK NNN"` — "I, B, heard you, A").
* When A receives an `ACK` carrying **A's own id**, A now has proof its frame was
  received and the peer is talking back → the link is **bidirectional** →
  *connected*. Hearing only a `HELLO` (no `ACK` for us yet) → *detected*.

State decays purely on timestamps: if a peer goes away, its frames stop arriving
and the LED drops Connected → Detected → Idle within `PEER_STALE_MS`.

---

## Hardware-verified behaviour

Flashed to the ESP32-C3 SuperMini on `/dev/ttyACM0` and observed over the USB
Serial/JTAG console (`espflash flash` + serial capture). **Observed** boot log of
the `espnow` build (Info level; times are real UTC and matched wall-clock):

```
INFO - smol booting: Phase 1 clock
INFO - smol: WiFi associated to '<ssid>'
INFO - smol: DHCP address 10.0.11.123/24
INFO - smol: radio -> ESP-NOW (time-share) on ch 6
INFO - smol: time synced via NTP -> 72657 s-of-day (UTC)      # = 20:10:57 UTC
INFO - smol: LED -> Idle
```

Confirmed on the single available board:

* **Boots cleanly** — one boot, no panic, no reset loop (verified quiet for >10 s
  of steady-state after boot).
* **WiFi + real NTP work** — associates to the AP, gets a DHCP lease, and the SNTP
  reply decodes to the correct current UTC time.
* **Radio time-shares to ESP-NOW** on the fixed channel, then the clock loop runs.
* **LED defaults to the no-peer OFF state** — the firmware logs `LED -> Idle` and
  never spuriously transitions with no peer present.

> **Single-board honesty note.** Only **one** board was available, so the
> **fast-blink (WiFi/NTP)**, **slow-blink (detected)**, **solid (connected)** and
> the OLED contents were **not** visually/camera-confirmed. What *is* confirmed is
> that the firmware **drives** the correct state: the serial `LED -> <state>`
> trace (logged on every state change) shows it selecting `Idle` at rest, and the
> `esp-println` log proves the WiFi/NTP/ESP-NOW path (during which the code drives
> the fast-blink) executes to completion. Verifying the blink/solid transitions
> and the OLED visually requires a **second unit** — see below.

### esp-println console note

esp-println is pinned to its **`jtag-serial`** backend (not the default `auto`) so
logs always reach `/dev/ttyACM0`; `auto` only routes to USB-JTAG when it detects
host SOF packets at write time, which dropped app logs to the (unwired) UART0 on
this board. Log level is baked in at compile time from `ESP_LOG`, so build with
`ESP_LOG=info cargo build ...` to see the `INFO` lines above (an unset `ESP_LOG`
compiles the level to `Off` and the app runs **silently**).

---

## Testing the LED states with two boards

The blink/solid states need two units on the **same `ESP_NOW_FIXED_CHANNEL`**.

1. **Give each board a distinct id.** In `src/main.rs`, board A keeps
   `net::mode::start(..., 7, ..)`; change board B to a different id, e.g. `8`.
   (Both must share the same `ESP_NOW_FIXED_CHANNEL`, default 6.)
2. **Flash both** with `--features espnow` (see [Flashing](#flashing-with-espflash)).
   Each can use the same or different WiFi in `secrets.rs`; NTP is independent.
3. **Power-on sequence + expected LED:**
   * At boot each board **fast-blinks (~10 Hz)** during its WiFi/NTP burst, then
     goes **OFF** (Idle) because it hasn't heard a peer yet.
   * Bring the **second** board up (or reset it) within a few seconds of the first.
     As soon as board A hears board B's `HELLO`, A **slow-blinks (~2 Hz)**
     (*detected*). The instant A also receives B's `ACK` for A's own id, A goes
     **SOLID** (*connected*) — and symmetrically for B. In practice both settle to
     **SOLID** within one or two `HELLO` periods (≈2–4 s).
   * **Power off / move one board out of range:** the other drops SOLID → SLOW →
     OFF within `PEER_STALE_MS` (~3 s) as beacons/ACKs stop arriving.
4. **Watch it on serial too:** each board logs `smol: LED -> PeerDetected` then
   `-> Connected` (and back to `-> Idle` when the peer leaves), and the OLED bottom
   line shows `peer NNN` / `linked`. Capture with:

   ```bash
   sudo chmod a+rw /dev/ttyACM0
   espflash monitor --port /dev/ttyACM0          # Ctrl+R resets, Ctrl+C exits
   ```

To sanity-check the blink *rates* with a single board, temporarily hard-code
`peer_led_state` to return `LedState::PeerDetected` (or `Connected`) — but the real
two-way transitions require the second unit.

---

## Flashing with espflash

The `espnow` build is flashed with **espflash v3** (see the install note below).
With the board on USB (default `/dev/ttyACM0`):

```bash
# The board's port is root:dialout and the user may not be in `dialout`:
sudo chmod a+rw /dev/ttyACM0

# Build (ESP_LOG=info so boot/WiFi/NTP/ESP-NOW logs are visible on serial):
ESP_LOG=info cargo build --release --features espnow

# Flash the built ELF (espflash v3 auto-generates bootloader + partition table):
espflash flash --port /dev/ttyACM0 \
  target/riscv32imc-unknown-none-elf/release/clock

# Then watch the serial console:
espflash monitor --port /dev/ttyACM0        # Ctrl+R = reset, Ctrl+C = exit
```

> **Use espflash v3, not v4.** espflash **v4+** requires an ESP-IDF *app
> descriptor* (`esp_bootloader_esp_idf::esp_app_desc!()`) that this esp-hal
> `1.0.0-rc.0` project does not emit, and refuses to flash without it. Install
> the v3 line instead:
> ```bash
> cargo install espflash --version "^3"
> ```

### Installing espflash on a box with a broken `cc` shim

If `cargo install espflash` fails with `Unknown: -m64` (host build-script link
errors) or a `ring`/cc-rs C-compile error, the machine has a non-compiler `cc`
earlier on `PATH` (this box does — see `.cargo/config.toml`). Force the real GCC
for both linking **and** cc-rs C compilation:

```bash
CC=gcc \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=gcc \
cargo install espflash --version "^3"
```

`esp-println` output (boot logs, WiFi/DHCP/SNTP status, LED state transitions,
ESP-NOW traffic) appears in the serial monitor. If your OLED shows nothing,
re-check the I²C wiring and the 72×40 vertical-offset calibration note above.

---

## Project layout

```
rust/clock/
├── Cargo.toml            # pinned deps + phase feature flags
├── rust-toolchain.toml   # rustc 1.96.1 + riscv32imc target
├── .cargo/config.toml    # target, build-std, linker scripts, host-cc workaround
└── src/
    ├── main.rs           # entry, display, clock+LED render loop, phase wiring
    ├── sensors.rs        # chip die-temp (tsens) + battery ADC (GPIO4) readouts
    ├── led.rs            # Phase 3: blue-LED (GPIO8) 4-state machine + polarity
    ├── secrets.rs        # LOCAL, git-ignored WiFi credentials (copy from .example)
    ├── secrets.rs.example# tracked template for secrets.rs
    └── net/
        ├── mod.rs (net.rs)  # feature gating + shared heap init
        ├── wifi.rs          # Phase 2: WiFi STA + DHCP + SNTP (smoltcp, blocking)
        └── mode.rs          # Phase 3: ESP-NOW HELLO/ACK handshake + RadioManager
```
