# 3D-printable cases for the ESP32-C3 SuperMini + 0.42" OLED

> Compiled by the **SHELL** agent, 2026-07-07.

**Board facts (for modeling your own):** the 01Space "ESP32-C3-0.42 OLED" PCB is **~24.8 × 20.45 mm** (the bare SuperMini is ~22.5 × 18 mm). Pin rows 2.54 mm pitch, ~17.78 mm apart. The **0.42" OLED sits at one end** (SSD1306 / I²C); its lit window is only **72×40 px (~8.5×5 mm)** centered in a larger glass module — **size the cutout to the physical glass, not the lit pixels.** **USB-C is on the short edge opposite the OLED.** RESET + BOOT buttons and red/blue LEDs are worth exposing. (Some units ship Micro-USB — verify yours.)

**Bottom line:** several cases fit this exact board, but **no dedicated handheld/game-console shell exists** — the "tiny game player" use is software-only (atomic14) with no printed shell. A pocket player = modify one of these + add a button at GPIO9.

## Tier 1 — made for the 0.42" OLED board, with OLED window
1. **ESP32-C3 0.42 OLED case** — Printables (Tengrom) · https://www.printables.com/model/1466752-esp32-c3-042-oled-case · 2-part snap, front OLED window · Free, 2× STL. *Most-referenced exact match.*
2. **case for ESP32C3 0.42 OLED** — MakerWorld (db59) · https://makerworld.com/en/models/1711229-case-for-esp32c3-0-42-oled · with/without side wiring window · Free STL/3MF. *Best-reviewed exact fit.*
3. **ESP32-C3 0.42 OLED CASE** — MakerWorld 2264801 · https://makerworld.com/en/models/2264801-esp32-c3-0-42-oled-case · snap fit, OLED + USB-C access, side vents, no supports · Free STL/3MF. *Nicest finished look.*
4. **ESP32-C3 0,42" OLED Super Mini Gehäuse** — Cults3D · https://cults3d.com/en/3d-model/gadget/esp32-c3-0-42-zoll-oled-super-mini-gehaeuse-esp-32-c3-oled-display-supermini-ho · print-in-place, cutouts for OLED + RESET/BOOT + LEDs · **PAID**. *Most feature-complete.*
5. **ESP32 C3 .42" OLED case** — GrabCAD · https://grabcad.com/library/esp32-c3-42-oled-case-1 · CAD source (STEP/native) · Free (login). *Best editable base for remixing into a handheld.*

## Tier 2 — 0.42" board, fit-dependent
6. **ESP32-C3 OLED Mini Slim Case** — Thingiverse (pm3dka) · https://www.thingiverse.com/thing:6866262 · ultra-slim, **headerless boards only** · Free STL.
7. **SCD4x CO₂ + AHT20 + BMP280 + OLED enclosure (remix)** — Thingiverse (evaristorivi) · https://www.thingiverse.com/thing:7343751 · multi-sensor; confirm OLED cutout variant (base is 0.96") · Free STL.

## Tier 3 — generic "ESP32-C3 + OLED" (verify OLED size)
8. **Holder/case for ESP32-C3 with OLED** — Printables (nsty) · https://www.printables.com/model/1232657 · reset pinhole + LED/button access · Free STL + Fusion 360 source.
9. **Case for ESP32-C3 with OLED** — Thingiverse (dlushni) · https://www.thingiverse.com/thing:7273901 · slide-in holder + lid · Free STL.

## Bare SuperMini (no OLED) — for modification
- **ESP32 C3/C6/H2/S3 Super Mini Case** — Printables (jcvg) · https://www.printables.com/model/1137008 · **parametric OpenSCAD** → regenerate with an OLED window · source: https://github.com/jeroenvangrondelle/esp32-super-mini-case
- **Simple case with buttons** — Printables (Superminaren) · https://www.printables.com/model/752889 · has button access; add an OLED window.

## Wrong OLED size (form-factor reference only)
- **Tiny TV | 0.96" OLED** — https://makerworld.com/en/models/581700 · retro-TV form.
- **DIY Smartwatch | 0.96" OLED** — https://makerworld.com/en/models/1749335 · wearable form.

## Top 3 for a pocket game-player
1. **GrabCAD (#5)** — editable CAD; widen the window + add a GPIO9 button boss.
2. **Cults3D (#4)** — already exposes buttons + LEDs + OLED (print-in-place); paid.
3. **MakerWorld 2264801 (#3)** free base + **jcvg parametric OpenSCAD** to add a button cutout.

**Mod note:** game input = momentary button to **GPIO9 (BOOT)** — plan a side/top hole + button perch; keep the OLED window at the glass size; leave the USB-C short edge open.
