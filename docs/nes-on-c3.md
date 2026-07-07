# NES on ESP32-C3 — feasibility & build plan

> Compiled by the **ARCADE** agent, 2026-07-07. Verdict: **possible, but a real port + a color TFT.**

## Recommended base repo
**Shim06/Anemoia-ESP32** (https://github.com/Shim06/Anemoia-ESP32, GPLv3) — the best actually-fetchable base. Clean, modular C++ NES core (cpu6502 / ppu2C02 / bus / cartridge + mappers 0,1,2,3,4,69), explicitly **needs no PSRAM**, and uses per-scanline line buffers (static `256x8` ≈ 4KB) so RAM fits the C3 easily.

**The proven-on-C3 emulator remains closed-source.** [rvembedded.com](https://rvembedded.com/blog_post/2/) is the only project actually demonstrated on a C3 SuperMini (~33fps, ST7735, custom core, ESP-IDF, RAM squeezed 340KB→63KB), but it still says only "I'll clean up the code and link to github at a later date." No release yet.

Others assessed: `derdacavga/Esp32-S3-nes-emulator`, `espressif/esp32-nesemu` (+ badvision/pebri86 forks) are Nofrendo-based, dual-core/PSRAM-oriented — poor C3 fits. `binji/smolnes` is tiny (MIT) but SDL-desktop — algorithm reference only. InfoNES/LaiNES are desktop references needing a full port.

## Honest blocker: Anemoia does NOT build for C3 as-is
Source review (not just the README) shows it's architected for a free 2nd Xtensa core:
- Pins APU + input tasks to core 0; the APU task is a tight `while(true){apu->clock();}` that would starve the single C3 core.
- `setCpuFrequencyMhz(240)` (C3 max is 160); built-in-DAC audio is ESP32/S3-only (C3 has no DAC).
- Its `platform.txt` force-injects **`-mlongcalls`**, an **Xtensa-only flag that breaks the RISC-V compiler**.

So it's a genuine **port** (drop audio, remove the APU task, cap 160 MHz, single SPI bus, embed ROM instead of SD) — not a config tweak.

## Toolchain: ESP-IDF (not Arduino)
The emulator needs explicit no-PSRAM RAM control (`heap_caps_*`, IRAM hot-loop placement), per-scanline SPI-DMA timing, and ROM-embedded-in-flash via CMake `target_add_binary_data` (rvembedded's trick — no SD card needed). The one working C3 build used IDF. Arduino is fine only for quick display bring-up.

## Display + exact C3 SuperMini wiring
The 72×40 1-bit OLED **cannot show NES playably** — confirmed. Two paths:

**(a) Recommended — add an ST7735 128×160 SPI TFT (~$3–4).** Matches the proven rvembedded result. C3 has one user SPI bus (SPI2) and ~11 safe GPIOs (avoid GPIO8 = onboard LED, GPIO9 = strap):

| TFT pin | C3 GPIO | | TFT pin | C3 GPIO |
|---|---|---|---|---|
| VCC | 3V3 | | DC/A0 | GPIO10 |
| GND | GND | | RST | GPIO3 |
| SCK | GPIO4 | | BLK | 3V3 (or GPIO1 PWM) |
| MOSI | GPIO6 | | CS | GPIO7 |

Buttons: active-low `INPUT_PULLUP` on GPIO0/1/2/5/21. C3 is GPIO-tight, so use a resistor-ladder-on-one-ADC-pin or a shift register for all 8 NES buttons. (ST7789 240×240 also works but needs more bandwidth + scaling; ST7735 is the better match.)

**(b) OLED tech-demo — not worth shipping.** A Floyd–Steinberg-dithered 256×240→72×40 1-bit frame is a cute "it runs!" screenshot but illegible. Worth it only as a Phase-0 milestone to validate the core on-device before the TFT arrives.

## Build plan (homebrew ROMs ONLY)
> **LEGAL:** use only homebrew / public-domain NES ROMs (nesdev.org homebrew, itch.io, Micro Mages demo, Alter Ego freeware). Do **not** source or embed commercial ROMs.

0. Wire ST7735 per the table; draw a test pattern via arduino-cli + TFT_eSPI (fast win, validates pins).
1. Install ESP-IDF v5.x: `git clone -b v5.3 --recursive https://github.com/espressif/esp-idf ~/esp/esp-idf` → `install.sh esp32c3` → `. export.sh`.
2. `idf.py set-target esp32c3`; pull in Anemoia's `src/core/` (start mapper000 — homebrew is usually mapper 0).
3. Replace TFT_eSPI writes with an `esp_lcd` ST7735 panel driver; downscale `renderScanline()` 256→128 into a DMA line buffer.
4. Delete the APU task + audio; poll buttons inline once/frame; cap CPU 160 MHz.
5. Embed the `.nes` via CMake `target_add_binary_data(... EMBED_FILES)`; point the cartridge loader at the embedded symbol (no SD).
6. `idf.py build flash monitor`; optimize hot paths into IRAM toward ~30fps.

## Difficulty & status
- **Difficulty: 4/5.** Main risk: hitting playable fps with per-scanline SPI-DMA on one 160 MHz core *while* porting Anemoia off its dual-core assumptions. Fallback: wait for / ask rvembedded to open-source.
- **Fetched:** Anemoia-ESP32 (depth 1) cloned into `experiments/nes-c3/Anemoia-ESP32/`, nested `.git` deleted, trimmed from 142 MB of PCB/3D assets down to **596 KB of pure source** (full `src/core/` + all mappers + configs + README + LICENSE). Confirmed the esp32 core exposes C3 FQBNs. No compile attempted (cannot succeed as-is), serial port untouched.

**Sources:** [rvembedded C3 NES](https://rvembedded.com/blog_post/2/) · [Anemoia-ESP32](https://github.com/Shim06/Anemoia-ESP32) · [C3 SuperMini pinout](https://lastminuteengineers.com/esp32-c3-super-mini-pinout-reference/) · [C3+ST7735 wiring](https://thesolaruniverse.wordpress.com/2024/09/01/esp32-c3-super-mini-and-the-128160-pixel-display-with-st7735-controller/) · [smolnes](https://github.com/binji/smolnes)
