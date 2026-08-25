# c3-oled — the fleet node with a face

The same ESP32-C3 fleet firmware as [`targets/c3`](../c3/README.md), on the
~$2.76 board variant carrying a 72×40 SSD1306 OLED over I²C. One firmware, one
tier — the display is driven by the same fleet image (screens, Cast, the Bard's
stories); there is no separate build for this variant today.

- **Chip**: ESP32-C3 (riscv32imc) · **node ids**: shared 1–99 block
- **Display**: SSD1306 72×40, I²C @400kHz, rotated 180° (`DISPLAY_ROTATION` in
  `rust/clock/src/main.rs` — the case hangs from the USB-C end)
- **Firmware/images**: identical to `targets/c3` — this folder exists for
  OLED-variant artifacts (case STLs, panel notes) as they accrue.

Status: shipping, mixed into the fleet alongside headless c3 boards.
