# smol — ESP32-C3 clock (Rust, `no_std`)

A `no_std` bare-metal Rust firmware for **"smol"**: an ESP32-C3 SuperMini with a
0.42" SSD1306 OLED (72×40, I²C). It shows a clock, syncs real time over WiFi via
SNTP, and does ESP-NOW peer messaging with an honest WiFi↔ESP-NOW radio switch.

Built on `esp-hal` 1.0 (RISC-V bare metal — **not** ESP-IDF/std).

---

## What compiles today

All three phases build cleanly with `cargo build --release` (zero warnings):

| Phase | Feature flag        | What it does                                                        | Status            |
|-------|---------------------|---------------------------------------------------------------------|-------------------|
| 1     | *(default)*         | Clock on the OLED, free-running from a compile-time start constant   | ✅ compiles + links |
| 2     | `--features wifi`   | + WiFi STA, DHCP, SNTP real-time sync (raw `smoltcp`, blocking)      | ✅ compiles + links |
| 3     | `--features espnow` | + ESP-NOW broadcast/receive + WiFi↔ESP-NOW time-share switching      | ✅ compiles + links |

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
  `smoltcp` (no async executor, no git-only crates). It compiles and links. It
  is *untested on hardware* (build-only task) and needs real WiFi credentials
  before it can associate — see [Configuration](#configuration).
* **Phase 3** implements the ESP-NOW send/receive loop and an explicit
  `RadioManager` with a `Mode` enum + `switch()` covering **both** single-radio
  strategies (coexist / time-share). It compiles and links.
  **One integration honesty note:** esp-wifi hands out the WiFi STA `WifiDevice`
  exactly once from `Interfaces`. In the `espnow` build the ESP-NOW handle is
  kept live for the clock loop, so the Phase-3 NTP burst currently associates to
  the AP (proving the WiFi side) but does **not** re-drive the full DHCP/SNTP
  smoltcp stack — that full run lives in the `wifi`-only build
  (`net::wifi::try_time_sync`). Wiring the single shared `WifiDevice` through the
  Phase-3 flow is the one remaining integration step; it is *not* faked green.

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

## Configuration

Set these `const` placeholders before flashing (currently dummy values):

* WiFi credentials — `WIFI_SSID` / `WIFI_PASSWORD` in **`src/net/wifi.rs`**
  (Phase 2) and **`src/net/mode.rs`** (Phase 3).
* Clock start (Phase-1 fallback) — `START_SECONDS_OF_DAY` in `src/main.rs`.
* This unit's ESP-NOW id — passed to `net::mode::start(..., 7)` in `src/main.rs`.
* Fixed ESP-NOW channel (time-share mode) — `ESP_NOW_FIXED_CHANNEL` in
  `src/net/mode.rs` (default 6; all peers must agree).
* NTP server — `NTP_SERVER_IP` in `src/net/wifi.rs` (default: Cloudflare NTP
  anycast `162.159.200.123`; hardcoded IP so no DNS resolver is needed).

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
the radio up in STA mode, does a WiFi burst, then `switch(Mode::EspNow)` pins the
fixed channel and the clock loop broadcasts `"hello from smol NNN"` every ~5 s
while displaying the most recent inbound peer message on the OLED's bottom line.
Because esp-wifi's `init` must run exactly once, `RadioManager` initialises the
radio a single time and keeps both the `WifiController` and the `EspNow` handle
alive for the program's lifetime — "switching" chooses which stack is serviced
and retunes the channel; it never re-inits the radio.

---

## Flashing later (with espflash)

This task was **build-only** — nothing was flashed. To flash on a machine with
the board on USB (default `/dev/ttyACM0`):

```bash
cargo install espflash            # one-time

# Flash + open serial monitor (the runner in .cargo/config.toml already does this):
cargo run --release                     # Phase 1
cargo run --release --features espnow   # Phase 3

# …or explicitly:
espflash flash --monitor \
  target/riscv32imc-unknown-none-elf/release/clock
```

`esp-println` output (boot logs, WiFi/DHCP/SNTP status, ESP-NOW traffic) appears
in the serial monitor. If your OLED shows nothing, re-check the I²C wiring and
the 72×40 vertical-offset calibration note above.

---

## Project layout

```
rust/clock/
├── Cargo.toml            # pinned deps + phase feature flags
├── rust-toolchain.toml   # rustc 1.96.1 + riscv32imc target
├── .cargo/config.toml    # target, build-std, linker scripts, host-cc workaround
└── src/
    ├── main.rs           # entry, display, clock render loop, phase wiring
    └── net/
        ├── mod.rs (net.rs)  # feature gating + shared heap init
        ├── wifi.rs          # Phase 2: WiFi STA + DHCP + SNTP (smoltcp, blocking)
        └── mode.rs          # Phase 3: ESP-NOW + RadioManager (WiFi↔ESP-NOW switch)
```
