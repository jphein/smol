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
| Swipe navigation, machine-verified (no C5-class bog) | GUI | 2026-08-27 debug-console suite: synthetic swipes paged 0→1→2→1→0; page-flip frame 74–79 ms, idle 150 µs (perf cmd) |
| AOD state entry after idle | GUI | suite: `state` screen 3→1 after 20 s idle on watchface (1 = AOD in the ladder) |
| Audio TX path, electrical (acoustic awaits speaker install) | GUI | `beep`: 1600/1600 B queued through I2S; codec+clock clean. JP has not installed the speaker yet |
| Battery gauge = measured % by construction | GUI | shell.set_battery and the `[BATT]` println consume the same batt_pct (main.rs:2091/2109); live 3.93 V→73% |

**Bench rig of record (2026-08-27):** the `debug-console` cargo feature + `tools/ui_test.py`
inject synthetic taps/swipes on the real touch path and report per-frame render timings —
the S3 test suite (`targets/s3-cyd/tmp/s3_suite.py`) runs the whole matrix with no hands.
(Corrected 2026-08-27: an earlier note here claimed the battery probe was boot-only —
wrong; the poll loop re-samples every 180 s (600 s screen-off), main.rs ~3168. Only the
`[BATT]` diag PRINT is boot-only. Retracted with the watch lane same hour.)

## GAPS — hardware allows, firmware doesn't yet

> **Each open gap below now has an issue** (filed 2026-08-27 from `docs/CAPABILITIES.md`, PR #474,
> all `[matrix]`-prefixed): **1** → #476 (fleet audio path) · #477 (GUI audio-out first listen) ·
> #478 (GUI audio-in, phase 2) · **2** → #479 · **3** → #480 · **4** → #481 · **6** → #482 ·
> **7** → #483. Plus two this list did not have: #484 (the xtensa stack high-water instrument, which
> is what pins the floor at `ObservedSufficient`) and #491 (the GUI flavor's missing WS2812 driver —
> see gap 5). The matrix cell is each issue's acceptance test, so this list and the matrix cannot
> drift apart without one of them failing its own check.

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
5. ~~WS2812 status LED (GPIO42)~~ **DONE — and this row was stale, not open**: the
   smol-native driver is in the tree. `rust/clock/src/led.rs` carries a hand-written
   WS2812 RMT frame encoder (26 pulse codes, 12.5 ns/tick at 80 MHz, GRB) and
   `rust/clock/src/board_s3.rs` declares `PIN_WS2812 = 42`. Hand-written because
   `esp-hal-smartled` 0.17 wants esp-hal ~1.0 and is incompatible with 1.1.x
   (BOARD.md L-row for GPIO 42 says so). §"the cyd-c5 half" below already said DONE,
   so **this file contradicted itself** — caught by the capability matrix
   (`docs/CAPABILITIES.md`, PR #474) and struck here.
   ⚠️ **Scope of the DONE: the smol-native flavor only.** The watch-GUI flavor
   declares `WS2812_GPIO = 42` in `targets/c6-watch/src/board/esp32s3_cyd.rs` and
   nothing in that tree reads it — tracked as **#491**, which also covers the fleet
   LED's peer-state semantics (off → blink → solid, settable via CFG `L`) that the
   GUI flavor has no counterpart for.
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
(c) WS2812 status light — DONE on the S3's smol-native flavor (RMT driver,
OTA-delivered, JP-verified green). ⚠️ **Corrected 2026-08-27**: this clause used to
read as parity *against a C5 status light*, and the C5 does not have one to match —
`targets/c6-watch/src/board/cyd_c5.rs` declares `WS2812_GPIO = 27` and **nothing in
the GUI tree reads it**, so only the constant exists (#486 scopes the board, #491
covers the missing GUI-flavor driver on both boards). The S3 therefore does not
merely match the C5 here, it exceeds it — which does not change this section's
verdict, but the old wording asserted a C5 capability nobody had verified;
(d) the Zigbee-bridge role — NOT a C5 hardware feature at all: it is a
two-chip design (ESP32-H2 companion over UART, JP-back-burnered 08-25) and the
802.15.4 radio it leans on does not exist on the S3, so it falls under
hardware-does-not-allow here. No C5 feature the S3's hardware allows is missing.

## Not hardware-allowed (documented exclusions)
- IMU features (pedometer, raise-to-wake, tilt) — no IMU on ES3C28P.
- PMU features (charge control, power-key latch, fuel gauge) — no AXP2101; battery %
  arrives via gap 2 instead.
- has-die-temp — esp-hal 1.1.x exposes no TSENS reading on the S3 (smol-native #407
  ships the `Option<f32>` fallback for the same reason).

## GUI flavor vs fleet node — superset ruling (#473, JP 2026-08-27)
The GUI/watch flavor must be a SUPERSET of the smol fleet node ("all the features of
the smol node, just added stuff"). Fleet features missing from the GUI flavor score
as GAPS with issue links, never N/A-by-design. First confirmed instance: the GUI
flavor does not render smol custom-screen pages (bench-ask convention unusable while
the S3 runs the GUI image) — #473. Watch lane owns the sweep + fix.

## Watch-lane items in flight (theirs)
- NTP re-sync deadlock (announce gated on ntp_synced; retry only while associated).
- C5 swipe work (vesper's XPT2046 driver; invisible-shade dirty-propagation suspect —
  verdict may apply to the S3's draw_if_needed too).
- Merge train: #446/#447/#448 + 928d35d + e2efaad → watch main → subtree → smol main.
