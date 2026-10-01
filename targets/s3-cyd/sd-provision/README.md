# sd-provision: write the shrine's voice pack to the card in its own slot

A separate ES3C28P image that formats the MicroSD card in the board's slot as FAT32 and writes
tapstone's voice pack (`/TAPSTONE/VOICE/SET1/`, tapstone 0033), sent over the board's USB-Serial-JTAG
link. It is the one smol image that writes a card. The station image mounts its card read-only by
construction (`check_readonly.sh --fs`). This image has no radio.

The host side is tapstone's `tools/sd_provision.py`. The wire format is `rust/sdprov-proto`, and its
test vectors are shared with the Python tests.

## Who may be written
JP authorised exactly the cards then in the two shrines, board 61 (`14:C1:9F:D1:C6:38`) and board 62
(`14:C1:9F:D1:C0:88`): "ok both sd cards in the slots can be formatted by you" (2026-09-30 16:4x,
the ANSWERS.jsonl record of his tapstone pane). A one-time grant for those two cards. The host tool refuses:
- any other board, by MAC;
- the scry station (`…CC:64`), by name;
- any port that isn't `/dev/serial/by-id`.

The board refuses every write until the host ARMs it with the card size AND the card's CID (its
manufacturer, product name and serial number, read with CMD10). INFO and ARM both read the CID from
the card at that moment, so the host's checks and the write are bound to one card: a card swapped
after the check, even the same model and size, is refused at ARM. The board names itself to nobody,
so the host's exact by-id check is the guard on WHICH board. The host also refuses a card that isn't
blank (sd_prepare's test, failing closed, on blocks read through READ, between two INFOs whose CIDs
must match) unless it is given `--format-authorised "<who said so, when>"`, which it records,
fsynced, in a regular audit file before anything is written.

## How it works
1. **INFO**: the board reports the card's size and CID. **READ**: the host checks the card is blank (an
   all-zero first MiB, or one FAT partition whose root holds only a label).
2. The host builds the card's whole image on disk as a sparse file of that size: an MBR, then FAT32
   by `mkfs.vfat` (label `SHRINE`) and the pack by mtools, using `sd_prepare.py`'s code.
   - The label must not be `TAPSTONE`. A label is a root entry, and embedded-sdmmc opens the first
     root entry with a matching name, so a `TAPSTONE` label hides the `TAPSTONE` directory.
     Board 61 showed this on 2026-09-30.
3. **ARM** (size + CID, re-read and compared by the board), then **ZERO** the metadata zone (MBR through the root directory's cluster). Then
   **WRITE** every block of the image's data extents, 8 blocks (4 KB) per frame.
4. **FILE** for every file in the pack: the board mounts the card read-only through embedded-sdmmc,
   the station's own path, and returns the file's size and sha256. The host compares them with the
   pack.

Every frame carries a CRC32. A corrupted frame is answered NAK and resent, and every request is
idempotent.

## Run it (from the board host; boards only by `/dev/serial/by-id`)
```
cd targets/s3-cyd/sd-provision && cargo build --release        # familiar: PATH + . ~/export-esp.sh
espflash flash --chip esp32s3 --port /dev/serial/by-id/usb-Espressif_..._<MAC>-if00 \
  --flash-size 16mb --no-stub --partition-table ../partitions-ota-s3.csv --non-interactive \
  target/xtensa-esp32s3-none-elf/release/sd-provision
python3 tools/sd_provision.py --device /dev/serial/by-id/usb-Espressif_..._<MAC>-if00 --pack SET1   # check only
python3 tools/sd_provision.py --device ... --pack SET1 --format                                   # write
```
Then flash the station image back (`../../s3-tapstone-station/README.md`). Its boot log says
`[voice] sd: pack mounted, 280 clips indexed in … ms`.

## Evidence (2026-09-30, katana's table)
| board | card | written | verified on the card | station afterwards |
|---|---|---|---|---|
| 61 | 30,953,963,520 B | 1,698 writes (6,955,008 B) in 141.6 s, 0 resends | 281 files, sha256 equal to the pack's | pack mounted, 280 clips indexed in 159 ms |
| 62 | 31,914,983,424 B | 1,698 writes in 141.6 s, 0 resends | 281 files | pack mounted, 280 clips indexed in 153 ms |

App size: 125,792 B (2.0% of a 6 MiB slot).
