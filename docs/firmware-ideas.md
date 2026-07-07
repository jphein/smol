# Cool things to flash on an ESP32-C3 SuperMini + 0.42" OLED

> Compiled by the **SCOUT** agent, 2026-07-07. Broad survey beyond gaming.

**Myth-bust up front:** the C3's "native USB" is a **fixed-function USB Serial/JTAG** controller — hardware-locked to serial + JTAG. It is **not** the software-configurable USB-OTG peripheral, so **the C3 cannot enumerate as a USB HID keyboard/mouse** (that's S2/S3 only). So USB "BadUSB" is out on this chip — but **BLE HID works great**, and that's the correct path.

Board constraints that shape everything: single-core RISC-V @160 MHz, ~400 KB SRAM, **no PSRAM**, 4 MB flash, **BLE-only**, single Wi-Fi radio.

Legend: **CONFIRMED** = explicitly targets/tests C3 · **UNKNOWN** = generic-ESP32, likely works, unverified · **NO** = C3 excluded.

## 1. Home / IoT
- **ESPHome** — CONFIRMED (`esp32c3` first-class). YAML sensors/displays into Home Assistant with OTA. https://esphome.io/components/esp32/
- **Tasmota** — CONFIRMED (dedicated `tasmota32c3.factory.bin`). Local MQTT/HTTP/rules, web UI, no cloud. [SuperMini template](https://templates.blakadder.com/SuperMini-ESP32-C3.html)
- **WLED** — CONFIRMED but "experimental" on C3 (only 2 RMT channels → few parallel strips; best for small strings). https://github.com/wled/WLED

**Your 0.42" OLED:** SSD1306-compatible 72×40 at I²C **0x3C**, **SDA=GPIO5 / SCL=GPIO6**. Use `U8G2_SSD1306_72X40_ER_F_HW_I2C` (no offset) or the 128×64 constructor **+ offset x=30, y=12**.

## 2. Comms / mesh
- **OpenMQTTGateway** (BLE mode) — CONFIRMED, runs on the bare board. Bridges hundreds of BLE sensors (Xiaomi/Mi Flora, TPMS, scales) → MQTT/HA. Best use of the C3's radio. https://github.com/1technophile/OpenMQTTGateway
- **Meshtastic** — CONFIRMED in build matrix, but **needs an external LoRa radio (SX1262/LLCC68)** wired up. https://github.com/meshtastic/firmware
- **ESP-NOW** — CONFIRMED (Espressif examples). Router-less device-to-device; single radio → use channel-hopping patterns.
- **macless-haystack** (Apple Find My) — community C3 PR; pure BLE advertising.

## 3. Security / pentest *(dual-use — authorized/educational only)*
The big suites are S3-first; the C3 is strong at the **pure-radio** subset.
- **mjlee111/esp32_wifi_deauther** — CONFIRMED on "ESP32 C3 Super-Mini" + OLED. WiFi scan + deauth rig.
- **EvilAppleJuice-ESP32** — CONFIRMED on C3. BLE popup-spam demo (modern iOS just shows prompts).
- **WiFi/BLE scanning & wardriving** — stock `WiFi.scanNetworks()` / `BLEScan` APIs; the C3's comfort zone.
- **ESP32 Marauder** / **Bruce** — UNKNOWN / NO for C3 (S3-first; need RAM + USB-HID the C3 lacks).
> Deauth, BLE spam, and evil-portal TX are illegal against gear you don't own or lack written permission to test.

## 4. USB HID — the reframe
Native USB HID does **NOT** work on the C3. The working path is **BLE HID**:
- **pr4u4t/ESP32C3-BLE-Keyboard** — CONFIRMED (XIAO C3). *The* answer for a wireless keyboard / macropad / media remote. Use a NimBLE-based fork.
- **Bucky** (BLE ducky) — UNKNOWN, BLE HID, chip-agnostic. The architecturally-correct BadUSB equivalent for C3.
- Rotary encoder + the BLE-keyboard lib = a **BLE volume knob**.

## 5. Dev frameworks (all CONFIRMED on C3)
- **Rust via `esp-hal`** — ⭐ the standout. C3 is RISC-V → an **upstream Rust+LLVM target, no forked toolchain**. `rustup target add riscv32imc-unknown-none-elf` and go. The smoothest embedded-Rust board Espressif makes.
- **MicroPython** — prebuilt `.bin`, biggest ecosystem, REPL over USB.
- **CircuitPython** — works, but no CIRCUITPY drive (uses Web Workflow over WiFi).
- **Espruino** (JS REPL), **TinyGo** (v0.41 added C3 WiFi), **NuttX** (POSIX RTOS + shell), **Toit** (OTA app updates).

## 6. Displays / desktop toys (best fit for the 0.42" OLED)
- **peff74/ESP32-C3_OLED** — CONFIRMED, the de-facto reference sketch (offset baked in).
- **Pharkie/ESP32-C3-OLED-Demo** — CONFIRMED, prebuilt `firmware.bin`, 9 rotating demos. Best calibration toy.
- **Zephyr board `esp32c3_042_oled`** — CONFIRMED in-tree target for this exact board.
- **NerdMiner_v2** — sold on this exact hardware; mainline via forks. A lottery-BTC stat toy.

## Top 5 to actually flash
1. **ESPHome / Tasmota** — Home Assistant sensor node rendering live data on the OLED.
2. **OpenMQTTGateway (BLE)** — bare-board BLE→MQTT bridge. Best use of the radio.
3. **Rust via esp-hal** — the ideal chip to learn embedded Rust.
4. **ESP32C3-BLE-Keyboard** — a BLE media/volume remote or macro button.
5. **Pharkie OLED demo → your own dashboard** — NTP clock / weather / crypto ticker.

**Two expectation-setters:** no USB BadUSB/HID on the C3 (S3 thing), and Meshtastic needs an added LoRa radio.
