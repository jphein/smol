# spike-sd — the ES3C28P MicroSD slot and P3 reader probe (smol#547)

Console-only bring-up probe. Every 5 s it re-runs two probes, so a serial port opened late still
sees a full round.

1. **RC522 on the P3 jack.** It reads the VersionReg register twice on SPI3 (SCK 14, MOSI 21,
   MISO 2, CS 3), once with the MISO pad pulled up and once pulled down.
   - A wired reader drives MISO, so both reads agree on a version byte.
   - With nothing wired, the reads come back `0xFF` then `0x00`: the pad just follows the pull.
     That split is the probe's own evidence that nothing drives the line.
2. **MicroSD in SPI mode** on the schematic's slot: CLK 38, CMD→MOSI 40, D0→MISO 39, D3→CS 47.
   Pins and citations are in `rust/clock/src/board_s3.rs` (`PIN_SD_*`). It initialises at
   400 kHz and then runs at 10 MHz. It prints the card's size and type, lists the FAT root, and
   reads the first regular file end to end, reporting its byte count, FNV-1a hash and throughput.
   **Read-only:** it never writes the card.

The two probes take turns on SPI3 through `reborrow()`. The S3 has only SPI2, which the panel
uses, and SPI3, so a station with both a reader and a card must share SPI3 the same way.

## Build and flash

Build on familiar, then flash from the host the board is plugged into, holding the board lock:

```sh
cd targets/s3-cyd/spike-sd && . ~/export-esp.sh && cargo build --release
espflash flash --port /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_<MAC>-if00 \
  target/xtensa-esp32s3-none-elf/release/spike-sd
```

The probe replaces the board's firmware. **Back up the flash first**
(`espflash read-flash 0 0x1000000`, about 24 min over USB-JTAG). espflash writes only below
0x40000, so writing the first 2 MiB of the backup back is a complete restore.

## Results

| date | board | RC522 on P3 | SD slot |
|---|---|---|---|
| 2026-09-27 | `14:C1:9F:D1:C0:88` (Tapstone id62) | **absent** (`0xFF` / `0x00`) | `CardNotFound` |
| 2026-09-27 | `14:C1:9F:D1:C6:38` (Tapstone id61) | **absent** (`0xFF` / `0x00`) | `CardNotFound` |

`CardNotFound` is a **weak negative**. An empty slot and a wiring fault look the same until a card
answers, and no card was in either slot. The mount is **not yet proven on glass**. The positive
control this still needs is a FAT-formatted card with a known file in the slot. The probe should
then print `VERDICT: SD MOUNTED`, with the file's size and hash matching the file on the host.
Both boards were restored to tapstone-gw 875dd70 afterwards, and each answered `@TS1 PING` with
`HELLO`.
