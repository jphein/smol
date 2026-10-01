# Wiring an RC522 card reader to a Tapstone shrine (ES3C28P)

The shrine station reads card taps from an **RC522 (MFRC522) module on the P3 jack**. With no reader
wired it plays its seat by itself (Autoplay), so this sheet is optional. Source of truth for the pins:
`rust/clock/src/board_s3.rs` (`P3_JACK_PINS`), from the scry station's silk-verified sheet
(`labels/scry/rc522-s3cyd-wiring.md`, 2026-09-01). The firmware side is
`rust/clock/src/tapstone_station/taps.rs`.

> ⚠️ **3.3 V only.** The RC522 is a 3.3 V part, and so are the S3's pins. Never feed it 5 V.
> Power it from the board's **3V3** pin on P4, never from a 5 V or VBUS pin.

> ⚠️ **USB unplugged while you wire.** Plug it back in only when every step is done.

## What you need
- One RC522 module (the common blue board, 8 pins: SDA, SCK, MOSI, MISO, IRQ, GND, RST, 3.3V).
- Two 4-pin 1.25 mm JST pigtails (the ES3C28P's P3 and P4 jacks are 4-pin, though the schematic
  draws 6).
- Six female jumper ends, or solder, for the RC522 side.

## Pin table

**Pigtail 1 → P3 "Expanded IO"** (silkscreen order 2 · 3 · 14 · 21):

| P3 silk | ESP32-S3 pin | RC522 pin | role |
|---|---|---|---|
| IO2 | GPIO2 | MISO | SPI3 MISO (data from the reader) |
| IO3 | GPIO3 | SDA | chip select (the RC522 calls it SDA; it is not I²C) |
| IO14 | GPIO14 | SCK | SPI3 clock |
| IO21 | GPIO21 | MOSI | SPI3 data to the reader |

**Pigtail 2 → P4** (power only):

| P4 | RC522 pin |
|---|---|
| GND | GND |
| 3V3 | 3.3V **and** RST (tie RST to 3.3V; the firmware resets the chip over SPI) |
| SDA | **not connected**: tape it off |
| SCL | **not connected**: tape it off |

RC522 **IRQ: not connected** (the firmware polls).

Why P4's SDA and SCL must stay loose: they are the board's I²C bus (GPIO16/15), which the speaker's
codec (ES8311) uses. The RC522 talks SPI on P3, not I²C.

## Steps
1. Unplug USB, and take the SD card out if you want the boot log to be about the reader alone.
2. Find P3 and P4 on the board's edge. Read the silkscreen beside each jack: P3 says **2 3 14 21**,
   and P4 says **GND 3V3 SDA SCL** (or the same four names).
3. Pigtail 1 into P3. Wire its four leads to the RC522 per the first table: IO2→MISO, IO3→SDA,
   IO14→SCK, IO21→MOSI.
4. Pigtail 2 into P4. Wire GND→GND. Wire 3V3 to **both** the RC522's 3.3V and RST pins.
5. Tape off P4's SDA and SCL leads separately, so neither touches anything.
6. Check every wire against the tables once more, especially that nothing goes to a 5 V pin.
7. Plug USB in. On the console (the board's own USB port), the boot prints one of:
   - `[taps] RC522 0x92 on P3 - card taps drive the seat (no-card timeout 5 ms)`: wired. Clones
     report 0x82, 0x88 or 0xB2 instead of 0x91 or 0x92.
   - `[taps] no reader on P3 (VersionReg 0xFF pulled up, 0x00 pulled down) - the seat plays by
     Autoplay`: nothing is driving MISO. Check IO2→MISO, the 3V3 lead, and CS (IO3→SDA).
   - Two equal bytes other than 0x00 and 0xFF, but no "RC522" line: the chip answered and then
     failed to start. Check RST is tied to 3.3V.
8. Tap a card. The console prints `[taps] 04:..:..: registered as Castle (1 bound)` for the first
   fresh tag, then `registered as Copy(0)`, `Copy(1)`, and so on for each new one (below).

## How the shrine reads taps
- **The first fresh tag after power-up is the castle.** Each fresh tag after that becomes the next
  copy in the deck list. Tapstone's playtest stock is blank NTAG215 cards (decision 0003), and this is
  the scry bridge's inline registration. Bindings last until power-off, and nothing is written
  anywhere.
- **Castle tap**: pass. In the mulligan window, it opens a 3 s prompt, and a second castle tap
  mulligans.
- **A card while a draw is owed**: draws it (0036).
- **A card in hand**: tap once to cast with the default lane or target after 3 s, or tap twice
  within 3 s to charge.
- A card left on the reader counts once. Lift it and tap again for a second tap.
