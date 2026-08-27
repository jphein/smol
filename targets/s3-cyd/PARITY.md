# S3 CYD feature parity — vs esp32c6-watch and cyd-c5 (JP goal of record, 2026-08-26)

Goal (JP, verbatim intent): *"S3 board works perfectly and has all the features of the
esp32c6-watch and the cyd-c5 that the hardware allows; it is a full target of smol."*

Two flavors, one board: **smol-native** (rust/clock, fleet tier) and **watch-GUI**
(targets/c6-watch, board-esp32s3-cyd). Parity is judged per flavor.

## Verified DONE (hardware-witnessed)

| Feature | Flavor | Evidence |
|---|---|---|
| Boot + PSRAM (8 MB octal, registered FIRST) | GUI | `[PSRAM] … (registered FIRST)`, #445/#447 |
| Display render (ILI9341V, landscape) | both | JP on glass: "gui looks good" (928d35d arm_ramwr); smol-native M1 earlier |
| Touch (FT6336U taps + swipes + wake-on-tap) | GUI | JP: "the s3 touch works really well"; deaf-Monitor fix e2efaad, read-backs clean |
| Mesh (SMOLv1 leaf id 162, relays, election ch6) | both | 20/20 acks; smol-native M3 36/36 |
| WiFi + NTP + MQTT + HA | both | `[NTP] synced`, `[MQTT] published` (broker leg 10.0.8.111 — VLAN8 seat); smol-native M4 |
| Display idle-sleep + wake | GUI | JP witnessed |
| Bard, games, cast-tap plumbing, sensors, budget row | native | #411, four-chip check |
| mesh-ota feature compiled (ed25519-dalek on xtensa) | GUI | Cargo.toml S3 arm |
| A/B partition table (6 MiB slots) | both | partitions-ota-s3.csv, flashed |

## GAPS — hardware allows, firmware doesn't yet

1. **Audio (BIGGEST)** — ES8311 codec + mic (LMA2718B381) + 3 W amp are ON THIS BOARD
   and the codec init already ACKs (`[AUDIO] Codec OK` — same chip as the C6, shared
   I2C). But: `has-audio` is OFF in the S3 feature arm, and the I2S/amp wiring in
   main.rs is the C6's — S3 needs I2S MCLK=GPIO4/BCK=5/WS=7/DOUT=8/DIN=6 and
   **amp = GPIO1 ACTIVE-LOW (LOW = ON — inverted vs the C6's GPIO6)**; codec is
   BCLK-derived (BOARD.md landmine L5). Emberburrito has the whole working S3 audio
   stack to port from (burrito-fw: I2S + ES8311 + sound_wave). Unlocks: Sound
   meter/FFT, TTS, Voice, Story audio, UI clicks.
2. **Battery %** — no PMU (has-pmu correctly off, #448 gates the AXP sites) but the
   board HAS a battery ADC: GPIO9, 2:1 divider (BOARD.md). Needs a `has-batt-adc`
   capability arm feeding the same battery UI the PMU feeds on the C6. Kills the 0%
   cosmetic honestly.
3. **`[IMU] OK` is a VACUOUS LIE on this board** — init is ungated, `let _ =` swallows
   the NACK, and the ES3C28P has no QMI8658 (absent from BOARD.md pin map). Fix:
   print by result (OK/absent), gate consumers on `has-imu` (Null-stub pattern already
   exists for touch). Hardware does NOT allow IMU features here — document, not port.
4. **has-light-sleep** — off for S3. esp-hal S3 has rtc_cntl sleep (unlike the C5);
   worth enabling for AOD power. Needs a bench current check, not just a compile.
5. **WS2812 status LED (GPIO42)** — smol-native parity with C3/C5 status light; RMT
   driver. (#398 follow-up.)
6. **LEDC backlight dimming (GPIO45)** — both flavors run the backlight as a bare
   GPIO today; the GUI's brightness slider is a threshold, not a dim.
7. **Cast mirror blank on S3** — smol-native known bug (#398 follow-up).
8. ~~A/B OTA first roll~~ **DONE 2026-08-26 ~07:07**: id 162 took build 345→1405
   over the air through the sanctioned pipeline — dual-path fetch (crown relay +
   self), slot flip, `ota=confirmed:1405` (self-test passed, no rollback). The
   fourth silicon family has OTA citizenship. Evidence + en-route findings
   (flash.sh otadata guard, SMOL_NODE_ID build-time trap, v922 crown gaps) on #398.
9. ~~#413 release packaging~~ **DONE 2026-08-27**: phase 3.1 (`a3d7302`) puts the
   GUI flavors — watch-c6, c5-cyd-gui, **s3-cyd-gui** — in the nightly alongside the
   fleet tiers. Public images build via `tools/ci_provision_gui.sh` with placeholder
   creds (the 192.168.1.10 default IS the honest public value); JP's personal builds
   bake the VLAN-seat leg.

## The cyd-c5 half of the goal — SATISFIED by construction
The C5's feature surface decomposes into: (a) the smol-native fleet tier — the S3
runs the same tier and EXCEEDS it (Bard on-device, OTA citizenship proven 345→1405);
(b) the watch-GUI flavor — the S3 shares the identical GUI stack (same shell, same
scenes, same renderer; the C5's on-glass bless and the S3's landed the same night);
(c) WS2812 status light — DONE on the S3 (RMT driver, OTA-delivered, JP-verified
green); (d) the Zigbee-bridge role — NOT a C5 hardware feature at all: it is a
two-chip design (ESP32-H2 companion over UART, JP-back-burnered 08-25) and the
802.15.4 radio it leans on does not exist on the S3, so it falls under
hardware-does-not-allow here. No C5 feature the S3's hardware allows is missing.

## Not hardware-allowed (documented exclusions)
- IMU features (pedometer, raise-to-wake, tilt) — no IMU on ES3C28P.
- PMU features (charge control, power-key latch, fuel gauge) — no AXP2101; battery %
  arrives via gap 2 instead.
- has-die-temp — esp-hal 1.1.x exposes no TSENS reading on the S3 (smol-native #407
  ships the `Option<f32>` fallback for the same reason).

## Watch-lane items in flight (theirs)
- NTP re-sync deadlock (announce gated on ntp_synced; retry only while associated).
- C5 swipe work (vesper's XPT2046 driver; invisible-shade dirty-propagation suspect —
  verdict may apply to the S3's draw_if_needed too).
- Merge train: #446/#447/#448 + 928d35d + e2efaad → watch main → subtree → smol main.
