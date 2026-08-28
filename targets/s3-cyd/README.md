# targets/s3-cyd — the ES3C28P as a smol fleet target

**Board:** LCDWIKI/QDtech **ES3C28P** (sold as "Hosyond 2.8in ESP32-S3 Touchscreen"),
ESP32-S3 **N16R8** — 16 MB flash, 8 MB octal PSRAM, ILI9341V 2.8" panel, FT6336U
capacitive touch, ES8311 audio codec.
**Physical unit:** a NEW, BLANK board JP supplied 2026-08-24 — *"same one we use in
emberburrito"*. **Fleet node id 162** (`docs/protocol.md` id-block table, #388 block).

## The name, before it misleads anyone

The directory is called `s3-cyd` because the ES3C28P is *dimensionally* drop-in with the
classic CYD (ESP32-2432S028 — identical outline and hole pattern, per
`ember.realm.watch/docs/enclosure.md` §4). **Dimensional compatibility is not hardware
compatibility**: this board is an ILI9341V + capacitive-I²C-touch + Xtensa machine, and
nothing from a classic CYD's (or the C5 CYD's ST7789/XPT2046) driver layer transfers.
Every hardware fact in this directory names the board **ES3C28P**.

## One board model, six physical units — read this before any flash

The ES3C28P around this workstation is a *batch*: six units, all enumerating as the same
`303a:1001 USB JTAG/serial debug unit`. The MAC in `ID_SERIAL_SHORT` is the **only**
discriminator, and identification is **passive `udevadm` only** (opening the port resets
the target).

| unit | MAC / `ID_SERIAL_SHORT` | status |
|---|---|---|
| **this target (id 162)** | `14:C1:9F:D1:C8:10` | ✅ the only sanctioned flash target here |
| emberburrito hearth terminal (id 161) | `28:84:85:44:45:94` | ⛔ another lane's board (emberburrito repo) |
| ember-satellite (JP's desk) | `28:84:85:44:59:20` | ⛔ **live family service** (ember.realm.watch, HA Assist) |
| ember-mobile (battery handheld) | `28:84:85:44:3E:C4` | ⛔ **live family service** |
| ember-dad | `28:84:85:44:3E:A4` | ⛔ **live family service, deployed off-site — maximal caution** |
| reliquary sealed vault board | `14:C1:9F:D1:C3:C8` | ⛔ **sealed, flashed once, never again** |

The `28:84:85:44:*` prefix is the whole batch — **a prefix match is not identity**. Worse:
**this target's own serial (`14:C1:9F:D1:C8:10`) and reliquary's sealed board
(`14:C1:9F:D1:C3:C8`) come from the same batch and differ only in the last two octets.**
An eyeballed comparison *will* eventually confuse them; only a byte-exact serial match is
identity. `spike/flash.sh` encodes this table as a deny-list and refuses by default.
(Identified 2026-08-24 23:03 by passive bus-diff: sole new device, JP-plugged, JP-named.)

## Two flavors, one board

This is the only target that runs **both** of smol's firmwares, and parity is judged per flavor:

- **smol-native (fleet)** — `rust/clock` with `--features esp32s3`. The apps, the SMOLv1 mesh,
  the Bard, signed OTA, the measured `ESP32S3_CYD` budget row. Xtensa, so it needs the espup
  `esp` toolchain and builds at `opt_level = 2` (an LLVM scavenger workaround declared in
  `tools/build-matrix.toml` — and part of this chip's (chip, profile) sha lineage).
- **watch-GUI** — the `board-esp32s3-cyd` arm of the [`targets/c6-watch`](../c6-watch/)
  workspace: Slint, touch, the launcher, PSRAM-backed scenes.

## What's here

| file | what it is |
|---|---|
| `BOARD.md` | the hardware truth: pin map (triple-sourced), landmines, power block, identity |
| `PARITY.md` | **the feature-parity matrix** — verified-done vs. hardware-allows-but-not-yet vs. documented exclusions, against the C6 watch and the C5 CYD |
| `PORT-SCOPING.md` | the decision log: verdicts with evidence, phases, operational rules, status |
| `PARTITIONS.md` · `partitions-ota-s3.csv` | the A/B layout (two 6 MiB slots), flashed |
| `DISPLAY-PACKAGE.md` · `BENCH-RUNBOOK.md` | the panel bring-up package and the bench procedure |
| `spike/` | the phase-1 bring-up crate (four-milestone ladder, cyd-c5 pattern, throwaway) |

## Status — 🟢 glass-verified (2026-08-26)

A human watched this board do all of it. **Verified on hardware, in the flavor noted:**

| | flavor | |
|---|---|---|
| Boot + 8 MB octal PSRAM (registered **first**) | GUI | #445 / #447 |
| Display render — ILI9341V, landscape | both | JP on glass |
| Touch — FT6336U taps, swipes, wake-on-tap | GUI | JP on glass |
| Mesh — SMOLv1 leaf id 162, relays, election on ch6 | both | 20/20 acks (GUI), 36/36 (native) |
| WiFi + NTP + MQTT → Home Assistant | both | `[NTP] synced`, `[MQTT] published` |
| Display idle-sleep + wake | GUI | JP witnessed |
| Bard, games, cast-tap plumbing, sensors, budget row | native | #411 |
| A/B partition table (6 MiB slots) | both | flashed |
| **Mesh OTA — 345 → 1405 over the air** | native | slot flip + `ota=confirmed:1405`, self-test passed, no rollback |

**Not yet, and named rather than implied:**

- **CI cannot build this chip.** `[chip.esp32s3] builds = false` — the *only* remaining blocker
  is the runner toolchain (espup's `esp` channel is not on GitHub runners). Everything else —
  manifest pins, the `linkall.x` + `opt_level` workaround (#408/#409), the display arm, the
  measured budget row — is done. `.github/workflows/xtensa-spike.yml` has shown a stock runner
  provisioning and building it.
- ~~No A/B OTA roll yet.~~ **✅ OTA citizenship confirmed, 2026-08-26** — id 162 took build
  **345 → 1405 over the air**: slot flip, `ota=confirmed:1405`, self-test passed, no rollback
  (~40 s for 1 MB). **The fourth silicon family is a full fleet citizen**, and this is the first
  cross-architecture OTA in smol's history. ⚠️ **Transport: a WiFi SELF-FETCH via the per-chip
  staged line** (`smol/ota/staged/esp32s3`) — the mesh half was the announce relay. Not a
  mesh-OTA *receive*, which #518 records as unsupported by construction across chips. *(#398; three traps burned en route, incl. `flash.sh`
  lacking an otadata erase — after any slot flip a USB flash silently boots the stale slot.)*
- **Stack-floor provenance is `ObservedSufficient`, not `Derived`** — the stack-measuring
  instrument is known-broken on this chip, so `ESP32S3_STACK_FLOOR_BYTES` is the largest region
  proven to run clean in bench operation. Real protection, weaker provenance than the C3's.
- **Audio, battery %, light-sleep, LEDC backlight dimming** — the hardware allows them and the
  firmware does not do them yet. See `PARITY.md` for what each one needs.
- **The IMU log is honest as of `ae80072`** — the ES3C28P has no IMU, and the boot line now says
  so (`[IMU] absent (init NACK - no IMU on this board)`) instead of an unconditional `OK` (#480).
  IMU and PMU features remain *documented exclusions* here, not gaps.

The dated running status lives at the bottom of `PORT-SCOPING.md`; `PARITY.md` is the matrix.
