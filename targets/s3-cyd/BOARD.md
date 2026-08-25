# ES3C28P — the hardware truth (board for smol node id 162)

Every fact below is **triple-sourced** unless marked otherwise: vendor
`ES3C28P_Schematic.pdf` (fetched 2026-08-01 from lcdwiki, archived at
`~/Projects/ember.realm.watch/docs/vendor/`) → emberboy's
`retro-go/components/retro-go/targets/ember-s3/config.h` → confirmed live by ESPHome
(`ember.realm.watch/esphome/ember-satellite.yaml`) and again by Rust firmware on glass
(`emberburrito/burrito-fw`). Distilled 2026-08-24 (recon report:
`~/.claude/projects/-home-jp/scratch/s3-cyd-target/explore-ember.md`).

## Identity

| | |
|---|---|
| Board | LCDWIKI/QDtech **ES3C28P**, "Hosyond 2.8in ESP32-S3 Touchscreen", black PCB |
| Module | **ESP32-S3 N16R8** — Xtensa LX7 dual-core, 2.4 GHz WiFi + BLE 5.0, **no 802.15.4** |
| Flash | 16 MB |
| PSRAM | **8 MB octal (OPI)** — measured on this board class: `8388608 bytes mapped at 0x3c020000` |
| Chip revision | **unknown** — recorded nowhere; needs a deliberate `espflash board-info` (which RESETS the target) |
| USB | native USB-Serial/JTAG, `303a:1001`, CDC-ACM; `ID_SERIAL_SHORT` = base MAC |
| This unit | **`14:C1:9F:D1:C8:10`**, smol node id **162** (`docs/protocol.md` id block) |
| Target triple | `xtensa-esp32s3-none-elf` (Tier 3 — needs the espup toolchain + `build-std`) |

⚠️ Same-batch eyeball trap: reliquary's **sealed** board is `14:C1:9F:D1:C3:C8` — last two
octets are the only difference from this unit. Byte-exact serial matching only.

## Pin map (authoritative)

| GPIO | net | function | notes |
|---:|---|---|---|
| **0** | BOOT / K2 | button, active-low | the **entire** hardware input budget; strapping pin; RTC-capable wake source |
| **1** | AMP_SD | speaker amp shutdown | ⚠️ **ACTIVE LOW** — LOW = amp ON |
| **4** | I2S_MCLK | ES8311 MCLK | wired, but the codec is **BCLK-derived** — see landmine L5 |
| **5** | I2S_BCK | ES8311 SCLK/BCLK | |
| **6** | I2S_DO | ES8311 ASDOUT → ESP DIN (**microphone**) | silkscreen names data pins from the *codec's* side — this pin gets dropped from third-party pinouts |
| **7** | I2S_WS | ES8311 LRCK/WS | |
| **8** | I2S_DI | ESP DOUT → ES8311 DSDIN (**playback**) | |
| **9** | BAT_ADC | battery voltage | onboard **2:1 divider** (×2.0, 12 dB atten). **Floats and reads noise with no cell fitted** |
| **10** | LCD_CS | SPI2 chip select | |
| **11** | LCD_MOSI | SPI2 MOSI | |
| **12** | LCD_CLK | SPI2 SCK | 40 MHz, Mode 0 |
| **13** | LCD_MISO | SPI2 MISO | **unused** — write-only panel |
| **15** | I2C_SCL | shared I²C0 | FT6336U `0x38` + ES8311 `0x18`, 100 kHz |
| **16** | I2C_SDA | shared I²C0 | |
| **17** | CTP_INT | touch interrupt | ESPHome uses it; burrito-fw polls at UI frame rate instead |
| **18** | CTP_RST | touch reset | ⛔ **NEVER CONFIGURE — landmine L1** |
| **33–37** | — | **consumed by octal PSRAM** | do not use |
| **42** | RGB_LED | WS2812 ×1, GRB, via RMT | ⚠️ `esp-hal-smartled` 0.17 wants esp-hal ~1.0 — incompatible with 1.1.x; drive RMT directly (~50 lines) |
| **45** | LCD_BL | backlight, **active-HIGH** | BSS138 gate. Also the VDD_SPI strapping pin — safe: schematic `R32` (10K to GND) hard-wires the strap LOW and latches at reset before GPIO drivers exist |
| **46** | LCD_DC | display data/command | strapping pin; fine as a runtime output |
| — | LCD_RST | **bonded to CHIP_PU/EN** | **there is no LCD reset GPIO** — software `SWRESET` only |
| — | SD card | **does not exist on this board** | use a FAT partition on internal flash if storage is ever needed |
| — | LDR | **does not exist** | (a classic-CYD feature this board lacks) |

Free/unclaimed (**inferred, not schematic-verified**): 2, 3, 14, 19, 20, 21, 38–41, 43,
44, 47, 48 — note 19/20 are the native USB D-/D+.

## Display

**ILI9341V** (mipidsi model `ILI9341Rgb565`), 240×320 native portrait driven as 320×240
landscape. `ColorInversion::Inverted` (INVON) + **BGR** required. Landscape MADCTL is
**`0x28`** = `Orientation::new().rotate(Deg90).flip_vertical()` in mipidsi 0.10.

- ⛔ retro-go's `0x36, 0x68` is **`0x28` plus MX = a horizontal mirror** it compensates in
  its own scan order. Copying it shipped mirror-writing in burrito-fw v0.1.
- Upside-down-but-readable → the answer is `.flip_horizontal()` (`0xE8`), **never**
  re-adding a mirror.
- Rendering: rasterise in internal SRAM, blit as contiguous windowed writes
  (`fill_contiguous`); per-pixel `draw_iter` ≈ one SPI command per pixel. Measured:
  per-cell SPI windows are 2× slower than a full-screen repaint.

## Touch

FT6336U/G capacitive @ I²C `0x38`, reports chip id 100 (0x64). Landscape transform
(from retro-go, flagged by burrito-fw as awaiting real-finger confirmation on Rust):
`SWAP_XY=1, INVERT_X=0, INVERT_Y=1`.

## Audio

ES8311 codec @ I²C `0x18` (shares I²C0 with touch), I²S slave, SC8002B 3 W class-AB amp
behind GPIO1 (active-low). Mic LMA2718B381 into ES8311 ASDOUT. Speaker sits inches from
the mic and there is no AEC.

## Power / battery (from the vendor schematic, settled by ember.realm.watch #44)

TP4054 charger (PROG 3.3K → ~290 mA) + SL2305 P-FET power path + B5819W Schottky +
200K/200K BAT_ADC divider. **NO protection IC** — the 2-pin BAT connector goes straight
to the cell; over-discharge floor is only the ME6217 LDO dropout (~3.4 V brownout), after
which the divider (~9 µA) keeps draining. **Bare cells require an external 1S protection
strip.** Power path is real: on VBUS the system runs from USB and full charge current
goes to the cell.

## Landmines (each cost a sibling project real debugging time)

1. **L1 — never configure GPIO18 (CTP_RST).** *Driving* it breaks the FT6336 — both a
   `reset_pin:` and a plain output-high were tested and both kill touch. Left alone, the
   FT6336 pulls RSTN high internally. The schematic says the pin floats and should be
   driven; **tested beats derived**. Its absence from touch code is the trick, not an
   oversight.
2. **L2 — PSRAM octal is a RUNTIME field** in esp-hal 1.1.x:
   `PsramConfig { mode: PsramMode::OctalSpi, size: PsramSize::AutoDetect, .. }`. There is
   **no `psram` cargo feature** and **no `ESP_HAL_CONFIG_PSRAM_MODE`** (both claims
   re-verified false 2026-08-14). Never leave an N16R8 on `Auto`. Release builds only.
3. **L3 — never put the esp-radio heap in PSRAM.** On the S3, atomics in PSRAM are
   silently non-atomic (esp-alloc's own docs); the WiFi driver's internals are full of
   synchronisation primitives. Spend the 64 KiB of internal RAM knowingly.
4. **L4 — MADCTL `0x28`, not `0x68`** (see Display above).
5. **L5 — ES8311 reg `0x01` = `0xBF`** (BCLK-derived), not `0x3F` — MCLK being physically
   wired to GPIO4 is what makes the wrong value look right. Fails as silence, never as an
   error. BCLK-derived mode refuses sample rates below 22050 Hz. Init order is
   load-bearing: reset (`0x00=0x00`) first, power-on (`0x00=0x80`) **last**.
6. **L6 — I²C init order: codec first, touch second.** The codec needs the bus once at
   boot; touch needs it forever. Right order ⇒ zero bus-sharing machinery.
7. **L7 — the `.bss`/stack-gap trap.** The stack is the leftover gap under RAM top; growing
   `.bss` silently steals it and the failure presents as a fault **inside the WiFi RX path
   at connect** — the wrong place entirely. Port burrito-fw's boot-time `STACK_FLOOR`
   assert with a floor **measured on this board** (burrito-fw measured a 132,300 B clean
   gap; the number is per-firmware, only the mechanism transfers). Same lesson as smol's
   `stack is not headroom`.
8. **L8 — USB-Serial/JTAG corrupts above default baud** — flash tools must never pass
   `--baud` (emberboy field note, encoded in every sibling flash guard).
