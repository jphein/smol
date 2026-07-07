# smol watch

A starter smartwatch firmware for the **ESP32-C3 SuperMini + 0.42" OLED**
(72×40 SSD1306, I²C SDA=5 / SCL=6, addr `0x3C`).

**Status:** starter scaffold. Implements an NTP clock + live weather over WiFi.
Notifications / calendar / alarms / sleep are scaffolded as TODOs, not faked.

---

## Quick start

1. **Arduino IDE** → install the **ESP32 board package** (Espressif). Select board
   **"ESP32C3 Dev Module"**. Enable **USB CDC On Boot** so `Serial` works over USB-C.
2. **Library Manager** → install **U8g2** (oliver) and **ArduinoJson** (Benoit Blanchon).
   `WiFi` / `HTTPClient` / `WiFiClientSecure` / `time.h` come with the ESP32 core.
3. Edit the config block at the top of `watch.ino`:
   - `WIFI_SSID` / `WIFI_PASSWORD`
   - `WEATHER_LAT` / `WEATHER_LON` (from https://www.latlong.net/)
   - `TZ_STRING` (POSIX TZ; default is US Central — examples in the file)
4. Compile & upload. Watch the Serial Monitor at **115200** baud.

> Sanity first: if the OLED doesn't light up, flash `../oled_test/oled_test.ino`
> — it proves the display + pins + I²C address before you debug the watch.

---

## Toolchain recommendation

**Use the Arduino-ESP32 core (Arduino framework on top of ESP-IDF).** ← recommended

For *this* feature set (WiFi NTP + weather JSON, BLE notifications, HTTPS/TLS, a
tiny UI, and eventually power/sleep), the deciding factor is **library maturity
for the two hard parts: TLS+JSON and BLE notification *hosting***. Arduino wins
on both, and it's the fastest path to a working watch on the C3.

| | **Arduino / ESP-IDF** (recommended) | **Zephyr RTOS** | **Rust** (`esp-idf-svc` / `esp-hal`) |
|---|---|---|---|
| WiFi + NTP | `WiFi.h` + `configTzTime` — trivial | native, more setup | works (`esp-idf-svc`) |
| Weather (HTTPS+JSON) | `WiFiClientSecure` + `ArduinoJson` — turnkey | mbedTLS + a JSON lib — more wiring | `reqwest`/`embedded-svc` + `serde` — works |
| **BLE notifications** | **NimBLE-Arduino** (incl. iOS **ANCS**) — the reference stack | BLE stack is solid but ANCS host code is DIY | `esp32-nimble` peripheral OK; **ANCS host not demonstrated** |
| UI | U8g2 (this board's constructor is known-good) / LVGL | **LVGL first-class** | U8g2-via-C or `embedded-graphics` |
| Power / sleep | `esp_sleep` — easy; RTOS niceties absent | **best-in-class** threads/power mgmt | good, less documented for this combo |
| Learning curve | **lowest** | steepest | medium–steep (borrow checker + embedded) |
| Examples for *this exact board* | many (see `../docs/board-repos.md`) | in-tree board `01space/esp32c3_042_oled` | `lijiachang/rust-esp32c3-oled-0.42-inch` (WiFi+HTTPS+JSON!) |

**Why not the others (for now):**

- **Zephyr** is the best-*architected* option — real threads, first-class LVGL,
  excellent power management, and there's even an in-tree board definition for
  this exact display (`boards/01space/esp32c3_042_oled`). If this grows into a
  serious multi-app watch, Zephyr is where you'd graduate to. But the BLE
  *notification-host* work (ANCS) is all hand-rolled, and the ramp is steep for
  a first firmware. **Pick Zephyr later, not now.**
- **Rust** is genuinely viable here — `esp32-nimble` does BLE peripherals with
  NVS-persisted bonding, and there's already a Rust WiFi+HTTPS+JSON app for this
  very board (`lijiachang/rust-esp32c3-oled-0.42-inch`). The gaps are the two
  least-mature pieces: a polished **ANCS host** and rich **UI** aren't
  demonstrated. Great choice if you *want* to write Rust; not the fastest path.

**Bottom line:** start on **Arduino**, reuse the community C3 smartwatch code
(below), and only move to Zephyr/Rust if you outgrow it.

---

## Recommended base to modify

The closest existing firmware to fork is **HDRobotica's ESP32-C3 smartwatch**
(the gist referenced from `../docs/wearables.md`):
https://gist.github.com/HDRobotica/b0418fc0393713ee0247296dacedbc56

It's **Arduino, confirmed on the ESP32-C3**, and already ships **NTP + weather +
5 clock faces + calendar + 3 alarms + stopwatch + Snake + deep sleep + multi-
language + WiFi setup UI**. Two changes are needed for our board:

1. **Display driver + resolution.** It uses `Adafruit_SSD1306` at **128×64** on
   pins 8/9. Swap to our **U8g2 `U8G2_SSD1306_72X40_ER_F_HW_I2C`** constructor on
   **SDA=5 / SCL=6**, and *redesign every screen for 72×40* (see UX warning below).
   This is the bulk of the port — it's a re-layout, not a driver swap.
2. **Weather API.** It uses **OpenWeatherMap (requires an API key)**. Prefer
   **Open-Meteo** (no key), as this starter does.

**Ranking for this use case:**

1. **HDRobotica gist** — *best base.* Arduino, C3-native, richest feature set
   (NTP/weather/alarms/faces/stopwatch/sleep). Cost = re-layout for 72×40 + API swap.
2. **jhud/hackwatch** — Arduino, ESP32, designed for cheap boards with a LiPo
   circuit + small OLED; NTP clock, stopwatch, timer, hardcoded calendar. Same
   author publishes an ANCS notifications library, so it's the best base if
   **iOS notifications** are your priority. Targets a 0.96" (color or mono) OLED,
   so still needs a 72×40 re-layout. https://github.com/jhud/hackwatch
3. **Bellafaire/ESP32-Smart-Watch** — most *feature-complete* as a product
   (reads phone notifications, Spotify control, calendar, companion Android app),
   but built around **custom hardware** (larger display, dedicated PCB, deep/light
   sleep). Great **reference for the notification + companion-app architecture**;
   a heavier lift to retarget to the SuperMini. https://github.com/Bellafaire/ESP32-Smart-Watch
4. **ESP32-ANCS reference** — not a watch, but the **notification building block**
   for iOS: `Smartphone-Companions/ESP32-ANCS-Notifications` (Arduino/NimBLE).
   Known-working on classic ESP32; see the C3 caveat under Notifications.
   https://github.com/Smartphone-Companions/ESP32-ANCS-Notifications

---

## Feature feasibility (honest, per feature)

| Feature | Verdict | Notes |
|---|---|---|
| **Time / date (NTP)** | ✅ Trivial | `configTzTime()` + `getLocalTime()`. POSIX TZ handles DST. **Done in this starter.** |
| **Weather** | ✅ Easy | **Open-Meteo** over WiFi, JSON, **no API key**. **Done in this starter.** |
| **Temperature** | ⚠️ Clarify | The C3's *internal* sensor reads **chip die temperature**, not ambient — it's inaccurate and runs hot. For "what's the temp" use the **weather API's** value (this starter does). For true room/skin temp, **add an I²C sensor**: **AHT20** (temp+humidity, cheap) or **BMP280** (temp+pressure) — both sit on the same SDA=5/SCL=6 bus. |
| **Calendar** | ⚠️ Hard on-device | No good fully-standalone option. Simplest → fetch a **public `.ics` (ICS) URL** over WiFi and parse the next event(s). Google Calendar API and CalDAV both need **OAuth/auth flows** that are painful on an MCU. **Recommended: a public/secret ICS URL**, or a phone companion that pushes today's events. A monthly grid does *not* fit 72×40. |
| **Notifications** | ❌ The hard part | **Split by OS.** **iOS:** doable via **BLE ANCS** (NimBLE-Arduino) — but there's an **open 2025 issue** getting ANCS advertising working specifically on the **ESP32-C3**; budget debugging. **Android:** **no ANCS equivalent** — needs a **companion app** (Gadgetbridge with a device profile, or a custom `NotificationListenerService` app) forwarding over a custom GATT characteristic. Stubbed, not faked, in `watch.ino`. |

### ⚠️ UX reality: 72×40 is a severe constraint
The panel is **72×40 ≈ 2,880 monochrome pixels** — enough for a big clock and one
short weather line (what this starter shows). It is **genuinely bad** for calendar
grids and for reading notification text (you get ~1–2 short lines). If
notifications or calendar are core to what you want, a **1.28" round color TFT**
(e.g. GC9A01, 240×240) is **far** better and is what most ESP32 watch projects use.
This board shines as a **clock + glanceable-status** watch; treat richer features
as marquee/scroll experiments, not primary UIs. (See `../docs/wearables.md`.)

---

## Charging / battery

The SuperMini has **no onboard charging**, and you can't wire a raw 4.2 V LiPo to
its 3.3 V pin. Plan for a **LiPo + TP4056 charge board** (+ a boost/buck-boost to
3.3 V, or a combined charge-boost module) and a **USB-C cutout** in the case.
A "charging for free" alternative is switching to the **DFRobot Beetle ESP32-C3**
(onboard TP4057). See **`../docs/power.md`** (battery/charging design) and the
battery section of **`../docs/wearables.md`** for the gotchas and part links.

---

## Feature roadmap

- [x] **Phase 0** — OLED sanity check (`../oled_test/`)
- [x] **Phase 1** — NTP clock face (HH:MM + date) — *this starter*
- [x] **Phase 2** — Live weather via Open-Meteo (temp + condition) — *this starter*
- [ ] **Phase 3** — Multiple clock faces + a button (GPIO9) to cycle screens
- [ ] **Phase 4** — Optional AHT20/BMP280 for real ambient temperature/humidity
- [ ] **Phase 5** — Calendar: fetch + parse a public ICS URL, show next event
- [ ] **Phase 6** — Notifications:
  - [ ] iOS via **BLE ANCS** (NimBLE) — resolve C3 advertising first
  - [ ] Android via a **companion app** (Gadgetbridge / custom) → GATT
- [ ] **Phase 7** — Power: light/deep sleep, wake-on-button, battery gauge
- [ ] **Phase 8** — Persist config (WiFi/location) in `Preferences` (NVS) instead of `#define`s

---

## Honest limitations of this starter

- **Notifications, calendar, alarms, and sleep are NOT implemented** — they are
  documented TODOs / stubs so you can add them cleanly. The BLE section is a
  deliberate no-op; it does not fake receiving notifications.
- **TLS is `setInsecure()`** (no cert validation) for the weather fetch — fine for
  a hobby read, but pin Open-Meteo's root CA if you care.
- **Config is compile-time `#define`s.** No on-device WiFi/location setup yet
  (Phase 8). Re-flash to change networks.
- **Power is unoptimized** — a busy `loop()` with WiFi up. Expect poor battery
  life until Phase 7. Great on USB; not yet a multi-day wearable.
- **Temperature shown is the weather API's**, not a local sensor (see table).
