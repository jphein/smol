# Smartwatch & pocket-watch cases for the 0.42" OLED SuperMini

> Compiled by the **TIMEPIECE** agent, 2026-07-07.

**Bottom line:** there is **no exact-fit wearable case** for the 0.42" OLED SuperMini — no smartwatch, no pocket watch, no pendant. Every 0.42"-specific model is a plain protective box. All ESP32 watch designs target **larger displays** (0.96" OLED or 1.28"+ round TFT). So the lists below are **adaptable bases + build references**. (The 72×40 panel is *why* the community treats this board as a desk gadget, not a watch — worth weighing.)

## (A) Smartwatch bases
1. **DIY Smartwatch Case for ESP32-C3 & 0.96" OLED** — https://makerworld.com/en/models/1749335-diy-smartwatch-case-for-esp32-c3-0-96-oled · wristband (3 sizes) + magnetic variant + **LiPo bay**. ADAPTABLE (0.96", not 0.42"). Free STL. Matching open firmware: https://gist.github.com/HDRobotica/b0418fc0393713ee0247296dacedbc56 . **Best base** — already solves strap + battery.
2. **OLED Watch Enclosure** — https://cults3d.com/en/3d-model/fashion/oled-watch-enclosure · spring-bar lugs + 3 buttons, Fusion360 source. ADAPTABLE (generic 0.96"/1.3").
3. **Smartwatch case for Waveshare S3 1.28" round** — https://www.thingiverse.com/thing:7038776 · reference for a proven LiPo bay (~500 mAh, 44×39×4 mm) + strap clip.

## (B) Pocket-watch references (none use a small OLED)
1. **DigiPclock** — https://hackaday.io/project/191207-digipclock-a-digital-pocket-clock · pocket-watch styling, **DFRobot Beetle C3 (onboard TP4057 charging!)** + round GC9A01 TFT, 600 mAh. STL on Tinkercad/GitHub (`vishalsoniindia/digiPclock`). Best power story.
2. **Diamond Age-Inspired Pocket Watch** — https://hackaday.com/2026/02/18/... · S3 + 1.75" round AMOLED. STLs promised but not yet posted.
3. Pendant: none exists; add a bail to a plain 0.42" box.

## ⚠️ Battery + charging reality (critical for any wearable OR the battery mod)
- Most **SuperMinis have NO onboard charging**, and their LDO input is a narrow ~3.0–3.6 V — you **cannot** wire a raw 4.2 V LiPo straight to a pin.
- Plan for a **TP4056 charge board (~17×17 mm) + a boost/buck-boost to 3.3 V**, or a combined charge-boost module — and budget case volume **plus a USB-C cutout** for it.
- "Charging for free" alternative: switch to the **DFRobot Beetle ESP32-C3** (onboard TP4057), as DigiPclock uses.
- SuperMini + TP4056 gotchas: https://forum.arduino.cc/t/1302012

## Modeling your own
Board ~24.8×20.45 mm, OLED one short end, USB-C the other; 72×40 effective. Editable STEP of the board + a case on GrabCAD: https://grabcad.com/library/esp32-c3-42-oled-module-by-01space-1 and https://grabcad.com/library/esp32-c3-42-oled-case-1 .
