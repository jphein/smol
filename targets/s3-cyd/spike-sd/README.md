# spike-sd — the ES3C28P MicroSD slot and P3 reader probe (smol#547)

Console-only bring-up probe. Every 5 s it re-runs two probes, so a serial port opened late still
sees a full round.

1. **RC522 on the P3 jack.** It reads the VersionReg register twice on SPI3 (SCK 14, MOSI 21,
   MISO 2, CS 3), once with the MISO pad pulled up and once pulled down.
   - A wired reader drives MISO, so both reads agree on a version byte.
   - With nothing wired, the reads come back `0xFF` then `0x00`: the pad just follows the pull.
     That split is the probe's own evidence that nothing drives the line.
2. **MicroSD, card level** (`src/sdraw.rs`, raw SPI, **read-only by construction**: the only
   commands it can send are CMD0/8/55/41/58/9/10 and CMD17, a one-block read). It prints each init
   step, the CID (manufacturer, product, date), the CSD capacity, then block 0 (the MBR or a FAT
   boot sector) and the first partition's boot sector. That is the positive control for the slot
   and the probe, with no filesystem involved.
3. **MicroSD, filesystem, in SPI mode** on the schematic's slot: CLK 38, CMD→MOSI 40, D0→MISO 39, D3→CS 47.
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

| date | board | RC522 on P3 | SD card level | SD filesystem |
|---|---|---|---|---|
| 2026-09-27 | `14:C1:9F:D1:C0:88` (id62) | **absent** (`0xFF` / `0x00`) | **answers**: SD v2, SDHC, CID SanDisk (MID 0x03) `SU32G` 2013-03, CSD 31,914,983,424 B | MBR, FAT32 partition 0 (0x0C, lba 2048, 7.4 GiB, mkfs.fat): **mounted**, root listed (21 entries), a file read back at 359 KiB/s |
| 2026-09-27 | `14:C1:9F:D1:C6:38` (id61) | **absent** | CMD0 `0xFF`: no card in the slot | — |

The mount is **proven on glass** (id62, with JP's card). id61's `0xFF` is a real negative,
because the same probe on the same board class answers when a card is present. An earlier run,
before the card went in, read `CardNotFound` on both boards, which was then an unproven negative.
The card-level lines are what made it trustworthy.

The card is JP's. The probe never writes, and the README records no file names from it.
Both boards were restored to tapstone-gw 875dd70 afterwards, and each answered `@TS1 PING` with
`HELLO`.
