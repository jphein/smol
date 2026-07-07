# Retro emulators & games on the ESP32-C3?

> Compiled by the **RECON** agent, 2026-07-07.

**Short answer:** the CPU *can* (someone ran NES at ~33 fps on a C3) — but the **display is the wall**. Every emulator targets a color SPI TFT (+ often PSRAM), and a **72×40 1-bit OLED cannot render a 256×240 color frame** in any playable way.

## Emulators (all need a color TFT — not our bare board)
| Project | C3? | Notes |
|---|---|---|
| [rvembedded NES](https://rvembedded.com/blog_post/2/) | **Only demo on C3** | ST7735 color TFT, ~33 fps, mapper-0, no audio, ~63 KB RAM. **Source not released.** |
| [retro-go](https://github.com/ducalex/retro-go) | No | Dual-core ESP32/S3 + PSRAM + color TFT. |
| [Espeon](https://github.com/Ryuzaki-MrL/Espeon) (Game Boy) | No | ILI9341 320×240, classic ESP32. |
| esplay / Nofrendo / SMSPlusGX | No | Dual-core + PSRAM handheld firmware. |

→ NES/GB/GBC emulation is **off the table on the bare board**. See [nes-on-c3.md](nes-on-c3.md) for the real path (add an ST7735 + ESP-IDF).

## What genuinely fits this board
- **U8g2** — *the* graphics library for the 72×40 panel (handles the offset). The foundation for custom 1-bit games.
- **[atomic14/esp32-c3-oled-single-button-games](https://github.com/atomic14/esp32-c3-oled-single-button-games)** — 5 games built for **this exact board** (72×40, BOOT-button input). Flash-and-go, public domain.
- **[peff74/ESP32-C3_OLED](https://github.com/peff74/ESP32-C3_OLED)** — correct display setup reference.
- Original 1-bit games: Snake, Breakout, Pong, Tetris-ish, endless runners.

## Verdict
Custom 1-bit games are the sweet spot — which is exactly why we built **Block Digger** (a Minecraft-flavored digger) rather than chasing an emulator. The CPU is plenty; the tiny mono screen is the real constraint.
