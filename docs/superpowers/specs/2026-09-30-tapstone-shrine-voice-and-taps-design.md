# The Tapstone shrine's voice from SD, and card taps (design note)

Date: 2026-09-30 · Lane: Selene (tapstone) · For tapstone#132 (the shrine station, `targets/s3-tapstone-station`).
JP's answers (2026-09-30 11:5x, relayed by the lead): one speaker per board in the SPK socket, not
plugged in yet; fresh SD cards from JP (**never write or format the card already in board 62**);
RC522 modules (JP wires them); reuse smol's existing S3 audio work rather than a parallel driver.

## Goals

1. **The voice from SD (tapstone 0033).**
   - The station mounts the FAT32 card read-only, finds `/TAPSTONE/VOICE/SET1/MANIFEST.TSV`, and plays
     the clip for each voice line.
   - With no card or no pack it stays text-only. It never blocks the seat or the radio: clips are
     streamed a block at a time, never loaded whole.
2. **RC522 card taps.**
   - The reader sits on the P3 jack and time-shares SPI3 with the SD slot.
   - A tap is the card's UID, mapped to the seat's move the way tapstone's decisions say (0009, 0032,
     0036).
   - With no reader present the seat keeps `Autoplay`.
3. **A wiring sheet** for the RC522 on P3.
4. Measured flash and RAM costs, the gate and CI green, and the hardware left on the new image.

## What exists, and what is reused

| Need | Reused | Source |
|---|---|---|
| ES8311 register sequence | c6-watch's `Es8311` (16 kHz, MCLK from pin), and emberboy's **acoustically verified** S3 sequence (BCLK-derived, `0x01 = 0xBF`, `pre_multi` ×8, ≥ 22,050 Hz; emberboy CLAUDE.md "Sound ✓ 2026-08-09") | `targets/c6-watch/src/peripherals/audio.rs`; `~/Projects/emberboy/.../drivers/audio/i2s.c` |
| Pin facts (I²S 4/5/7/8, amp GPIO1 active-low, codec 0x18, I²C 16/15, SD 38/40/39/47, P3 2/3/14/21) | `board_s3.rs` (single source) | smol |
| SD over SPI, read-only, and the RC522 on the same SPI3 by `reborrow()` | `targets/s3-cyd/spike-sd` (mount proven on glass on id62, 2026-09-27) and its `check_readonly.sh` | smol#547, #554 |
| RC522 driver | `mfrc522 0.8` (spike-scry, spike-sd) | crates.io |
| The pack's manifest reader | `shrine_render::pack` (tapstone #175), vendored at **rules-v0.2.3** (tapstone-game 74c8e87, tagged 2026-09-30) | tapstone |
| Which sentence is on the band | `shrine_render::voice::Voice::text()`; the pack's rows carry `text_sha256` of exactly that text | tapstone |
| The seat | `tapstone_proto::shrine::Shrine::propose_to`, `copy_uid`, `design_of`, `stamp_uids` | tapstone |

**One ES8311 driver, not two.** The register code moves into a small shared crate,
`rust/es8311`, with both clock modes:
- `Clock::MclkPin16k`: c6-watch's sequence, byte for byte (a host test records the writes and pins
  them).
- `Clock::BclkDerived { rate }`: emberboy's verified one, for the station at 22,050 Hz.

c6-watch then uses the crate. Why this mode for the station:
- c6-watch's S3 setting (MCLK from pin, 16 kHz) has never been heard (`docs/CAPABILITIES.md`:
  "acoustic proof pending").
- `board_s3.rs` landmine L5 says `0xBF`.
- The pack is 22,050 Hz for exactly that reason.

## Approach

### The voice

- **Mount.** SPI3 in SPI mode (400 kHz init, then 10 MHz) and `embedded-sdmmc`, **read-only by
  construction**: `spike-sd`'s `check_readonly.sh` rules extend to the station's SD module, and the gate
  fails on any write-class command or write API.
- **Clip lookup.** A band change gives a `Voice`. The station hashes `v.text()` (the `sha2` it already
  links) and scans the manifest rows (`pack::parse_row`) for that `text_sha256`. There's no new id
  scheme: the pack's text is the band's spoken text.
- **Stream.** The clip WAV is read 256 B at a time. Each IMA-ADPCM block restarts from its own header,
  so no state crosses blocks. It's decoded to 16-bit (a small decoder, bit-exact against ffmpeg on a
  committed fixture) and pushed into an I²S TX circular DMA ring at 22,050 Hz, stereo 16-bit (the
  codec plays the left slot).
- **Never blocks.** Feeding happens in `Station::service`, a few blocks per tick, and only while the
  ring has room. A panel repaint (75–84 ms measured) would outlast the ring, so the station defers
  repaints while a clip plays, capped at a short maximum.
  - The deferral is c6-watch's measured lesson (`audio_out.rs`: grow the ring and you starve the
    heap; move the repaint instead).
  - The amp (GPIO1, active-low) is on only while a clip plays.
- **Degrade.** No card, no FAT, no manifest, a refused manifest, or a missing clip: log once, then
  text-only. A card read error mid-clip stops that clip.

### Card taps

- **Detection.** At boot, read VersionReg twice, with the MISO pad pulled up and then down (spike-sd's
  proof that something drives the line). No reader: the seat keeps `Autoplay`. A reader: the seat is
  manual, and taps drive `propose_to`.
- **SPI3 time-share.** One owner, `Spi3Share`, holds SPI3 and the six pins. SD and the reader take turns
  only between whole transactions: a block read completes before the reader is polled, because both
  run in the same `service` call, one after the other. The bus is re-pinned per use. Chip-select pins
  idle high (the SD's own pull-up; the reader's CS driven high).
- **Tap → move** (tapstone 0009, 0032 item 13, 0036). A tap gives a UID:
  - **the figurine (castle):** Pass. Inside the mulligan window, it opens a 3 s prompt: a second castle
    tap is a Mulligan, and expiry or any other action keeps and passes.
  - **one of this seat's copies while a draw is owed:** a `Draw` of that copy (0036: a draw is a tap).
  - **a copy in hand:** the cast-or-charge prompt. A second tap of the same card within 3 s charges;
    otherwise, after 0009's 5 s countdown, it casts with 0009's defaults (a unit into the lane with the
    fewest of your units whose entry is free; a spell at the nearest legal target). The engine
    trial-applies every candidate, as `Autoplay` does.
  - **anything else** (another seat's copy, a card already played, an unknown tag): refused locally,
    with nothing sent.
  - The resolver is a pure module with host tests that drive a whole prompt cycle, including 0032's
    "castle taps alone reach the mulligan".
  - PROPOSAL: move it into `tapstone_proto` (tapstone's rule: fixes go there first) once it has run on
    glass. It lives in smol now because the station is where the reader is.

## Budget (measured in the PRs)

- **Flash.** The S3 station image before and after, section by section. The vendored crates' 64 KB
  gate (`tools/tapstone_flash_chip.sh esp32s3`) stays green.
- **RAM.** The I²S ring (planned 4 × 1,024 B ≈ 23 ms at 22,050 Hz stereo, internal RAM for DMA), SD
  buffers (one 512 B block plus embedded-sdmmc's volume state), and the manifest read row by row, never
  whole. Checked against the S3 stack floor.

## Files

- **smol PR A:** re-vendor at rules-v0.2.3 (`tools/tapstone_vendor.sh --sync`), plus this note.
- **smol PR B:** `rust/es8311/` (new); `targets/c6-watch` switched to it; `rust/clock/src/tapstone_station/{voice,sd,adpcm}.rs`
  and the audio bring-up in `main.rs`; the read-only check extended.
- **smol PR C:** `rust/clock/src/tapstone_station/{reader,taps}.rs`; `targets/s3-tapstone-station/WIRING.md`.
- **tapstone:** `tools/sd_prepare.py` (the safe card preparer) and its tests; JP's plug-in test sheet.
