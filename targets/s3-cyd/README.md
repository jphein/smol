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

## What's here

| file | what it is |
|---|---|
| `BOARD.md` | the hardware truth: pin map (triple-sourced), landmines, power block, identity |
| `PORT-SCOPING.md` | the decision log: verdicts with evidence, phases, operational rules, status |
| `spike/` | the phase-1 bring-up crate (four-milestone ladder, cyd-c5 pattern, throwaway) |

## Status

See the dated status section at the bottom of `PORT-SCOPING.md`.
