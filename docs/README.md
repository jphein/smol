# smol — docs

Docs for the **smol** ESP32-C3 handheld, gathered/written by agents during the build (2026-07-07).

## Firmware, protocol & play
- **[BUILDING.md](BUILDING.md)** — toolchain, flashing, pin map, the "which board am I holding?" name/MAC guide, and the gotchas that cost us time.
- **[protocol.md](protocol.md)** — the canonical **SMOLv1** wire-protocol reference: every ESP-NOW frame (HELLO/ACK, BEACON, TIME, RELAY/RELAYACK, SNK) with exact byte layout, cadence, and honest per-frame verification status.
- **[mesh-snake.md](mesh-snake.md)** — how to play **World Snake**, the shared-world MMO: one-button controls, the six treasure-powers, the leaderboard, joining a mesh.
- **[relay.md](relay.md)** — operator guide for the **ESP-NOW → internet relay**: leaf vs gateway roles, the flush cycle + its single-radio cost, configuring the collector, and the freeze-fix backoff semantics.

## Research
- **[gaming-firmware.md](gaming-firmware.md)** — Can retro emulators run on the C3? (Verdict: display-limited; custom 1-bit games are the sweet spot.)
- **[firmware-ideas.md](firmware-ideas.md)** — The broad survey of cool things to flash on an ESP32-C3 (ESPHome, OpenMQTTGateway, Rust, BLE HID… and why USB BadUSB is *out*).
- **[nes-on-c3.md](nes-on-c3.md)** — A concrete plan to actually run NES on the C3 (needs a color ST7735 TFT + ESP-IDF; a genuine port).

**Hardware:** ESP32-C3 SuperMini · 0.42" SSD1306 OLED (72×40, I²C `0x3C`, SDA=GPIO5 / SCL=GPIO6) · Bluetooth 5 LE · 4 MB flash · single-core RISC-V @160 MHz, no PSRAM.

**The build:** `blockdigger/` (the game, Arduino + Bluepad32) · `oled_test/` (hardware sanity check) · `site/` (this project's editable web hub) · `experiments/nes-c3/` (trimmed NES-emulator source for a future port).
