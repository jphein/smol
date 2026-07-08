# Building & flashing smol

Everything needed to build and flash the firmware in this repo, plus the
hardware facts and the non-obvious gotchas we hit (so you don't have to).

## Hardware (read off the chip with esptool)

- **MCU:** ESP32-C3 (QFN32) rev **v0.4** — single-core RISC-V @ 160 MHz, ~400 KB SRAM, **no PSRAM**
- **Flash:** 4 MB embedded (XMC)
- **Radio:** Wi-Fi + **Bluetooth 5 LE only** (no Bluetooth Classic → no A2DP/HFP, no USB-HID; BLE HID is fine)
- **USB:** native USB **Serial/JTAG** (enumerates as `/dev/ttyACM0`) — not USB-OTG
- **Display:** 0.42" **SSD1306, 72×40**, 1-bit, I²C addr **0x3C**
- **Board:** ESP32-C3 SuperMini + 0.42" OLED, **USB-C**. Buttons (RST + BOOT) at the OLED/antenna end; the two LEDs (PWR + IO8) flank the USB-C connector at the other end.

### Pin map
| Pin | Use |
|---|---|
| GPIO5 / GPIO6 | I²C SDA / SCL (OLED) |
| GPIO8 | onboard **blue LED** (IO8, active-LOW); also a strapping pin |
| GPIO9 | **BOOT** button (input, active-low); strapping pin |
| GPIO4 | free ADC1 channel — used for **battery voltage** (needs a divider) |
| GPIO10 | suggested **piezo** buzzer (see docs/sound.md) |
| GPIO3/4/6/7/10 | if adding an ST7735 color TFT for NES (see docs/nes-on-c3.md) |

## Toolchain setup

### Arduino (games: Block Digger, Snake, 2-player Snake, atomic14 pack)
```bash
# arduino-cli in ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/arduino/arduino-cli/master/install.sh | sh
arduino-cli config init
arduino-cli config add board_manager.additional_urls \
  https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json \
  https://raw.githubusercontent.com/ricardoquesada/esp32-arduino-lib-builder/master/bluepad32_files/package_esp32_bluepad32_index.json
arduino-cli core update-index
arduino-cli core install esp32:esp32              # ~1–2 GB toolchain
arduino-cli core install esp32-bluepad32:esp32    # NOTE: HYPHEN, not underscore
arduino-cli lib install U8g2
```

### Rust (the unified firmware: Clock + Snake + Bench)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
rustup target add riscv32imc-unknown-none-elf     # C3 is a stock upstream RISC-V target
# espflash v3 (v4 refuses esp-hal 1.0.0-rc.0 images — see gotchas):
CC=gcc cargo install espflash --version "^3"
```

## Flashing

The port is `root:dialout`; if your user isn't in `dialout`, run this before every upload/monitor:
```bash
sudo chmod a+rw /dev/ttyACM0
```

### Arduino games
```bash
FQBN=esp32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M          # Block Digger uses the -bluepad32 core
arduino-cli compile --fqbn "$FQBN" games/snake
arduino-cli upload  --fqbn "$FQBN" -p /dev/ttyACM0 games/snake
```
Block Digger needs the Bluepad32 core: `esp32-bluepad32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M`.

### Rust unified firmware (`rust/clock/`)
```bash
cd rust/clock
cp src/board.rs.example   src/board.rs        # then set NODE_ID (per board) — git-ignored, ALL builds (#19)
cp src/secrets.rs.example src/secrets.rs      # then edit WIFI_SSID / WIFI_PASS — git-ignored, wifi/espnow only
ESP_LOG=info cargo build --release --features espnow   # full build: Clock + Snake + Bench
espflash flash --port /dev/ttyACM0 target/riscv32imc-unknown-none-elf/release/clock
```
Feature tiers: default = Clock + Snake · `--features wifi` = + NTP · `--features espnow` = + ESP-NOW peer LED/mesh + Bench.

## Gotchas (the ones that cost us time)

- **espflash v4 won't flash** esp-hal `1.0.0-rc.0` images (wants an ESP-IDF app descriptor). Use **espflash v3**.
- **`esp-wifi` pins to esp-hal internals:** it needs **`esp-hal = "=1.0.0-rc.0"`** exactly (newer rc.1/1.0 changed `Rng::new()` and break the build despite passing semver). Full working pin-set is in `rust/clock/Cargo.toml` + comments.
- **Rust serial logs go over USB-JTAG:** build with `ESP_LOG=info` (level is compile-time) and view with `espflash monitor` — plain `cat /dev/ttyACM0` won't show them, and the monitor needs a real TTY (fails under a pipe).
- **`espflash monitor` reset mode matters on this native-USB C3:** `--before default-reset` (the UART-bridge DTR/RTS reset) **fails silently** — it drops the chip into download/stub mode, so you get the monitor banner and then nothing. Use **`--before usb-reset`** (the USB-JTAG-Serial peripheral reset) to actually reboot the app and catch its boot log. The firmware only logs at **boot + state changes**, so a silent idle board looks identical to a broken capture — you must catch the boot.
- **Capture logs through a PTY, not a pipe:** espflash block-buffers when stdout isn't a terminal, so `espflash monitor | tee` stalls. Wrap it in a pseudo-terminal: `timeout <N> script -qec "espflash monitor --port <port> --before usb-reset" <capfile>`. Kill it by **exact** name only (`pgrep -x espflash` / `pkill -x espflash`) so you don't nuke unrelated processes.
- **Identify boards by USB vendor / MAC, never by `ttyACMx`:** the number isn't stable — a board re-enumerates on replug (we saw `ttyACM2 → ttyACM0`, same MAC) and other USB-serial devices can squat a lower number. Espressif boards are **`303a:…`** (`lsusb`; `303a:1001` = the USB-JTAG/serial peripheral); pin the exact unit by **MAC** (`udevadm info /dev/ttyACM* | grep -i serial`, or read it from the boot log). On this box `ttyACM1 = 1209:2201` is a **Dygma keyboard** — opening it as if it were the board is a real mistake, so match `303a:` first.
- **Broken `cc` shim on PATH** on this box → prefix cargo installs with `CC=gcc`.
- **`CDCOnBoot=cdc`** is required in the Arduino FQBN for Serial over USB-Serial/JTAG.
- **Bluepad32 package is `esp32-bluepad32`** (hyphen), not `esp32_bluepad32`.
- **Display 180°:** the pocket-watch case hangs from the USB-C end, so the firmware sets `DisplayRotation::Rotate180`. On a bare board with USB-C down it reads upside-down — flip it USB-up (or set `Rotate0` for bench use).
- **Secrets:** real WiFi creds live only in git-ignored `rust/clock/src/secrets.rs` (the repo is public).

## Multi-board / ESP-NOW mesh
Give each board a **distinct peer id** (`rust/clock/src/main.rs`, the `mode::start(..., N, ...)` arg — we flashed 7 / 8 / 9). Distinct ids let the blue-LED handshake and the Bench link stats work between boards (same id can be filtered as self-echo). Boards auto-pair over ESP-NOW on the AP's channel; watch the blue LED go slow-blink (detected) → solid (connected).

Each id maps to a deterministic **magical name** (via realm-sigil) — id 7 = *Draconic Dominion*, id 8 = *Eldritch Nexus*, id 9 = *Jade Herald*. The name is that board's identity in the mesh: it shows on peers' World-Snake screens and in the leaderboard.

### "Which board am I holding?" — identify by name / MAC, not the port
With several identical boards on the bench, don't trust the `ttyACMx` number (it's not stable, and a keyboard can squat a low one — see the espflash gotchas above). Instead:
- **On-screen:** the board prints its name at boot (`smol: I am Draconic Dominion (id 7)`) and shows it in the mesh UI — read the OLED to know which physical unit you're holding.
- **By USB vendor/MAC:** Espressif boards are `303a:…` (`lsusb`); pin the exact unit by MAC (`espflash board-info`, `udevadm info /dev/ttyACM* | grep -i serial`, or the boot log). Keep an id ↔ MAC ↔ name map for your fleet. Verified today: `ac:a7:04:b9:77:14` = id 7 *Draconic Dominion* (the WiFi/NTP root), `ac:a7:04:ba:1f:24` = id 8 *Eldritch Nexus*, `10:00:3b:ce:95:cc` = id 9 *Jade Herald*.
- **Final-flash flow:** confirm the target unit by MAC/`board-info` first, flash with its intended id (`mode::start(…, <id>, …)`), then watch the boot log echo the expected name — that name+id on the OLED is your confirmation you flashed the right physical board.

The mesh wire protocol (HELLO/ACK, BEACON, TIME, RELAY, and the design-stage SNK) — exact byte layouts, cadence, and per-frame verification status — is documented in **[docs/protocol.md](protocol.md)**.
