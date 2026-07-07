# More repos for the ESP32-C3 + 0.42" OLED board

> Ongoing discovery (PROSPECTOR = GitHub deep sweep, BEACON = community sources).
> These are NEW finds beyond the 8 already on the site's "Built for this board" list.

## PROSPECTOR — GitHub sweep (2026-07-07)

### Confirmed for the 0.42" 72×40 OLED board
| Repo | Link | ⭐ | What | Stack |
|---|---|---|---|---|
| **lijiachang/rust-esp32c3-oled-0.42-inch** | https://github.com/lijiachang/rust-esp32c3-oled-0.42-inch | 1 | **Rust!** WiFi + HTTPS + BTC/ETH/SOL crypto price ticker; documents 72×40 origin (30,14) | Rust (esp-hal) |
| zhuhai-esp/ESP32-C3-ABrobot-OLED | https://github.com/zhuhai-esp/ESP32-C3-ABrobot-OLED | 37 | Arduino examples for the ABrobot 0.42" board: CDC, blink, clock, OLED driver | C++ / Arduino |
| sigmdel/mini_esp32c3_oled_sketches | https://github.com/sigmdel/mini_esp32c3_oled_sketches | 0 | Simultaneous I²C (OLED) + SPI; provides board defs | C++ / PlatformIO |
| julioformiga/zephyr_esp32c3_042_oled | https://github.com/julioformiga/zephyr_esp32c3_042_oled | 1 | VL53L0X ToF distance on the OLED (EMA, progress bar) | Zephyr + LVGL |
| ESP32Home/oled_042 | https://github.com/ESP32Home/oled_042 | 0 | "Hello world" test; SSD1306 72×40, SDA5/SCL6, 0x3C | C / ESP-IDF + PlatformIO |
| karamo/ESP32-C3-mini-with-0.42-OLED | https://github.com/karamo/ESP32-C3-mini-with-0.42-OLED | 4 | Custom SSD1306.py driver + text demo (ABrobot) | MicroPython |
| ludwich66/Pflanzenfeuchtesensor_ESP32-C3-OLED | https://github.com/ludwich66/Pflanzenfeuchtesensor_ESP32-C3-OLED | 0 | Soil-moisture → MQTT/Home Assistant; µPython 72×40 driver w/ flip | MicroPython |
| Sarah-C/01Space_ESP32_Bat_Detector_Display | https://github.com/Sarah-C/01Space_ESP32_Bat_Detector_Display | 0 | Bat-detector frequency meter on the 0.42" OLED | C++ / Arduino |

### Likely (OLED is one part of a larger collection)
| Repo | Link | ⭐ | Note |
|---|---|---|---|
| sigmdel/supermini_esp32c3_sketches | https://github.com/sigmdel/supermini_esp32c3_sketches | 112 | Broad SuperMini collection; sketch `27_i2c_oled` targets the 0.42" OLED |
| jandelgado/arduino | https://github.com/jandelgado/arduino | — | Personal libs; has a documented 0.42" OLED (72×40, u8g2) section |

### Context (not standalone repos)
- **Zephyr in-tree**: `boards/01space/esp32c3_042_oled` + shield `boards/shields/abrobot_esp32c3_oled` (upstream monorepo).
- **ESPHome**: built-in via `model: SSD1306 72x40` (no dedicated repo).

### Excluded (different/larger display, verified): dude84 CO2 (128×64), makepkg Internet-Radio (0.91" 128×32), Makerfabs MaESP (1.3"), Kerrbty Clock (ST7789), verbrannter-toast (ILI9341).

> PROSPECTOR note: web search saturated here; deeper low-visibility coverage would need authenticated `gh api` code-search.

## BEACON — community sources (2026-07-07)

New repos/gists beyond PROSPECTOR's (via espboards.dev, sigmdel.ca, HomeDing wiki, YouTube, Arduino Project Hub):

| Repo / gist | What | Match |
|---|---|---|
| chlordk/Adafruit_SSD1306_72x40 | Adafruit_SSD1306 fork, "fix for 72×40 displays" (library) | CONFIRMED |
| AbdulKus/72x40oled_lib | Lightweight 72×40 OLED library (SSD1315/1306) | CONFIRMED |
| mathertel/HomeDing | IoT firmware with a board profile for this exact board (OLED + GPIO9 button + NeoPixel) | CONFIRMED |
| bitbank2 gist `8b1b34…` | SCD41 CO₂ demo via Larry Bank's OneBitDisplay `OLED_72x40` | CONFIRMED |
| palaniraja gist `8000da…` | Minimal PlatformIO counter/LED demo (xOffset=28) | CONFIRMED |
| pixelEDI/ESP32_XIAO | 32 PlatformIO examples incl. 0.42" OLED (on a XIAO C3, not SuperMini) | LIKELY |

Inline-code sources (no repo): Arduino Project Hub (Dziubym), Hackster/ronfrtek (Visuino stopwatch + NTP), Instructables/CheshirCa (clock + weather), ESPHome forum t/852490.

**Coverage:** saturated — 6 agents converged on the same set. Reddit was hard-blocked in this environment (0 results; worth a manual pass). GitLab/Codeberg/SourceHut: empty for this board — the ecosystem lives on GitHub.
