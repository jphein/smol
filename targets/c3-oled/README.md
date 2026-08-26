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
- **Downloads**: `target.toml` declares `alias_of = "c3"`, so
  `tools/release_targets.sh` resolves it and does **not** build a second image.
  Flash the `c3` artifact on this board; the release notes name both.
- **The case**: `experiments/pocketwatch/` generates a round, chain-hung case for
  exactly this variant — which is why the panel is rotated 180°.

Status: 🟢 shipping, mixed into the fleet alongside headless c3 boards.
