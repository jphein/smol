//! Card-level SD probe in SPI mode, **read-only by construction**: the only commands this file can
//! send are CMD0, CMD8, CMD55/ACMD41, CMD58, CMD9, CMD10 and CMD17 (read one block). No write,
//! erase or format command exists here.
//!
//! This level is the positive control for the slot and the probe, independent of any filesystem:
//! a card that answers CMD0 with R1 = 0x01, then CMD8's echo, and returns a CID and CSD, proves
//! the pins, the bus and the card are all there.
use embedded_hal::spi::SpiBus;
use esp_hal::gpio::Output;
use esp_println::println;

pub struct Card {
    pub sdhc: bool,
    pub cid: [u8; 16],
    pub csd: [u8; 16],
}

fn xfer<S: SpiBus<u8>>(spi: &mut S, b: u8) -> u8 {
    let mut x = [b];
    let _ = spi.transfer_in_place(&mut x);
    x[0]
}

/// Send a command frame and return its R1 (0xFF: no answer within 16 bytes). CS stays LOW.
fn cmd<S: SpiBus<u8>>(spi: &mut S, cs: &mut Output<'_>, idx: u8, arg: u32, crc: u8) -> u8 {
    cs.set_high();
    xfer(spi, 0xFF);
    cs.set_low();
    xfer(spi, 0xFF);
    let a = arg.to_be_bytes();
    for b in [0x40 | idx, a[0], a[1], a[2], a[3], crc] {
        xfer(spi, b);
    }
    for _ in 0..16 {
        let r = xfer(spi, 0xFF);
        if r & 0x80 == 0 {
            return r;
        }
    }
    0xFF
}

/// Read a data block of `out.len()` bytes after the 0xFE start token (CSD/CID are 16 B).
fn data<S: SpiBus<u8>>(spi: &mut S, out: &mut [u8]) -> bool {
    let mut tok = 0xFF;
    for _ in 0..50_000 {
        tok = xfer(spi, 0xFF);
        if tok != 0xFF {
            break;
        }
    }
    if tok != 0xFE {
        println!("[sdraw] data token 0x{:02X}, not 0xFE", tok);
        return false;
    }
    for b in out.iter_mut() {
        *b = xfer(spi, 0xFF);
    }
    xfer(spi, 0xFF); // CRC16, unchecked
    xfer(spi, 0xFF);
    true
}

/// Identify the card. Every step is printed, so a failure says where it stopped.
pub fn identify<S: SpiBus<u8>>(spi: &mut S, cs: &mut Output<'_>, delay: &esp_hal::delay::Delay) -> Option<Card> {
    cs.set_high();
    for _ in 0..10 {
        xfer(spi, 0xFF); // >= 74 clocks with CS high
    }
    let mut r = 0xFF;
    for _ in 0..10 {
        r = cmd(spi, cs, 0, 0, 0x95);
        if r == 0x01 {
            break;
        }
        delay.delay_millis(10);
    }
    println!("[sdraw] CMD0 R1=0x{:02X} {}", r, if r == 0x01 { "(idle: a card answers)" } else { "(no card answered)" });
    if r != 0x01 {
        cs.set_high();
        return None;
    }
    let r8 = cmd(spi, cs, 8, 0x1AA, 0x87);
    let mut echo = [0u8; 4];
    for b in echo.iter_mut() {
        *b = xfer(spi, 0xFF);
    }
    let v2 = r8 == 0x01 && echo[2] & 0x0F == 0x01 && echo[3] == 0xAA;
    println!("[sdraw] CMD8 R1=0x{:02X} echo {:02X?} -> {}", r8, echo, if v2 { "SD v2" } else { "v1 / MMC" });
    let mut ok = false;
    for i in 0..200 {
        cmd(spi, cs, 55, 0, 0x01);
        let r41 = cmd(spi, cs, 41, if v2 { 0x4000_0000 } else { 0 }, 0x01);
        if r41 == 0x00 {
            println!("[sdraw] ACMD41 ready after {} tries", i + 1);
            ok = true;
            break;
        }
        delay.delay_millis(10);
    }
    if !ok {
        println!("[sdraw] ACMD41 never left idle (2 s)");
        cs.set_high();
        return None;
    }
    let r58 = cmd(spi, cs, 58, 0, 0x01);
    let mut ocr = [0u8; 4];
    for b in ocr.iter_mut() {
        *b = xfer(spi, 0xFF);
    }
    let sdhc = ocr[0] & 0x40 != 0;
    println!("[sdraw] CMD58 R1=0x{:02X} OCR {:02X?} -> {}", r58, ocr, if sdhc { "SDHC/SDXC (block addressed)" } else { "SDSC (byte addressed)" });
    let mut cid = [0u8; 16];
    let mut csd = [0u8; 16];
    let rc = cmd(spi, cs, 10, 0, 0x01);
    let cid_ok = rc == 0 && data(spi, &mut cid);
    let rs = cmd(spi, cs, 9, 0, 0x01);
    let csd_ok = rs == 0 && data(spi, &mut csd);
    cs.set_high();
    xfer(spi, 0xFF);
    if !(cid_ok && csd_ok) {
        println!("[sdraw] CID/CSD read failed (CMD10 R1=0x{:02X}, CMD9 R1=0x{:02X})", rc, rs);
        return None;
    }
    Some(Card { sdhc, cid, csd })
}

pub fn report(c: &Card) {
    let cid = &c.cid;
    let oem = [cid[1], cid[2]];
    let name = [cid[3], cid[4], cid[5], cid[6], cid[7]];
    let serial = u32::from_be_bytes([cid[9], cid[10], cid[11], cid[12]]);
    let year = 2000 + (((cid[13] & 0x0F) as u32) << 4 | (cid[14] >> 4) as u32);
    let month = cid[14] & 0x0F;
    println!(
        "[sdraw] CID: MID 0x{:02X} OEM {:?} product {:?} rev {}.{} serial 0x{:08X} made {}-{:02}",
        cid[0],
        core::str::from_utf8(&oem).unwrap_or("?"),
        core::str::from_utf8(&name).unwrap_or("?"),
        cid[8] >> 4,
        cid[8] & 0x0F,
        serial,
        year,
        month
    );
    let csd = &c.csd;
    let bytes: u64 = match csd[0] >> 6 {
        1 => {
            // CSD v2: C_SIZE is 22 bits, capacity = (C_SIZE + 1) * 512 KiB
            let c_size = ((csd[7] as u64 & 0x3F) << 16) | (csd[8] as u64) << 8 | csd[9] as u64;
            (c_size + 1) * 512 * 1024
        }
        _ => {
            let read_bl_len = csd[5] & 0x0F;
            let c_size = ((csd[6] as u64 & 0x03) << 10) | (csd[7] as u64) << 2 | (csd[8] as u64) >> 6;
            let mult = ((csd[9] & 0x03) << 1) | (csd[10] >> 7);
            (c_size + 1) * (1u64 << (mult + 2)) * (1u64 << read_bl_len)
        }
    };
    println!(
        "[sdraw] CSD v{}: capacity {} bytes ({} MiB)",
        (csd[0] >> 6) + 1,
        bytes,
        bytes / (1024 * 1024)
    );
}

/// Read block `lba` (512 B) with CMD17. Read-only.
pub fn read_block<S: SpiBus<u8>>(spi: &mut S, cs: &mut Output<'_>, sdhc: bool, lba: u32, out: &mut [u8; 512]) -> bool {
    let addr = if sdhc { lba } else { lba * 512 };
    let r = cmd(spi, cs, 17, addr, 0x01);
    let ok = r == 0 && data(spi, out);
    cs.set_high();
    xfer(spi, 0xFF);
    if !ok {
        println!("[sdraw] CMD17 lba {} failed (R1=0x{:02X})", lba, r);
    }
    ok
}

/// Describe block 0 (an MBR, or a FAT boot sector with no partition table) and, for a FAT
/// partition, its boot sector. Returns nothing: this is the filesystem level's evidence.
pub fn describe_layout<S: SpiBus<u8>>(spi: &mut S, cs: &mut Output<'_>, sdhc: bool) {
    let mut b = [0u8; 512];
    if !read_block(spi, cs, sdhc, 0, &mut b) {
        return;
    }
    let sig = b[510] == 0x55 && b[511] == 0xAA;
    println!("[sdraw] block 0: signature 55AA {}", if sig { "present" } else { "ABSENT (blank or unformatted?)" });
    if !sig {
        println!("[sdraw] block 0 head {:02X?}", &b[..32]);
        return;
    }
    if fat_name(&b).is_some() {
        println!("[sdraw] block 0 is a FAT boot sector (no partition table): {:?}", fat_name(&b));
        boot_sector(&b);
        return;
    }
    let mut first = None;
    for i in 0..4 {
        let e = &b[446 + 16 * i..446 + 16 * i + 16];
        let kind = e[4];
        let start = u32::from_le_bytes([e[8], e[9], e[10], e[11]]);
        let len = u32::from_le_bytes([e[12], e[13], e[14], e[15]]);
        if kind != 0 {
            println!(
                "[sdraw] MBR partition {}: type 0x{:02X} ({}) start lba {} length {} sectors ({} MiB){}",
                i,
                kind,
                match kind {
                    0x0B | 0x0C => "FAT32",
                    0x04 | 0x06 | 0x0E => "FAT16",
                    0x01 => "FAT12",
                    0x07 => "exFAT/NTFS",
                    0xEE => "GPT protective",
                    0x83 => "Linux",
                    _ => "other",
                },
                start,
                len,
                len as u64 * 512 / (1024 * 1024),
                if e[0] == 0x80 { " boot" } else { "" }
            );
            first.get_or_insert(start);
        }
    }
    if let Some(start) = first {
        let mut p = [0u8; 512];
        if read_block(spi, cs, sdhc, start, &mut p) {
            let sig = p[510] == 0x55 && p[511] == 0xAA;
            println!("[sdraw] partition 0 boot sector at lba {}: 55AA {}, OEM {:?}", start, sig, core::str::from_utf8(&p[3..11]).unwrap_or("?"));
            if &p[3..11] == b"EXFAT   " {
                println!("[sdraw] exFAT: embedded-sdmmc cannot mount it (FAT12/16/32 only)");
            } else {
                boot_sector(&p);
            }
        }
    } else {
        println!("[sdraw] MBR has no partitions");
    }
}

fn fat_name(b: &[u8; 512]) -> Option<&str> {
    let fat32 = &b[82..90];
    let fat16 = &b[54..62];
    if fat32.starts_with(b"FAT") {
        core::str::from_utf8(fat32).ok()
    } else if fat16.starts_with(b"FAT") {
        core::str::from_utf8(fat16).ok()
    } else {
        None
    }
}

fn boot_sector(p: &[u8; 512]) {
    let bps = u16::from_le_bytes([p[11], p[12]]);
    let spc = p[13];
    let label32 = &p[71..82];
    println!(
        "[sdraw] boot sector: {:?}, {} B/sector, {} sectors/cluster, FAT32 label {:?}",
        fat_name(p),
        bps,
        spc,
        core::str::from_utf8(label32).unwrap_or("?")
    );
}
