//! spike-sd — **ES3C28P MicroSD slot + P3 RC522 presence probe (smol#547).**
//!
//! Console only (USB-JTAG). Every 5 s it re-runs two probes so a late serial
//! attach still sees a full round:
//!
//!   1. **RC522 on P3** (SPI3: SCK 14, MOSI 21, MISO 2, CS 3 — the scry
//!      harness, `../spike-scry`). Reads VersionReg (0x37) twice: once with the
//!      MISO pad pulled UP and once pulled DOWN. A wired reader drives MISO, so
//!      both reads agree on a real version byte (0x91/0x92, clones 0x82/0x88/
//!      0xB2). An unwired jack reads 0xFF then 0x00: the pad follows the pull,
//!      i.e. nothing is driving it. That split IS the instrument's own control.
//!   2. **MicroSD in SPI mode** on the schematic's slot (SPI3 again, re-pinned:
//!      CLK 38, CMD→MOSI 40, D0→MISO 39, D3→CS 47; D1 41 / D2 48 idle on their
//!      10K pull-ups). Init at 400 kHz, then 10 MHz; prints card size and type,
//!      lists the FAT root, and reads the first regular file end to end with a
//!      byte count, an FNV-1a hash and a throughput figure. Read-only: the card
//!      is never written.
//!
//! The two probes time-share SPI3 through `reborrow()`: the S3 has only SPI2
//! (the panel) and SPI3, so a station with a reader AND a card must do the
//! same. This spike is the proof that the hand-off works.
//!
//! Pins: `rust/clock/src/board_s3.rs` (`PIN_SD_*`), from the vendor schematic.

#![no_std]
#![no_main]

use embedded_hal::spi::SpiBus;
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{Mode as FileMode, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    main,
    spi::{
        master::{Config as SpiConfig, Spi},
        Mode,
    },
    time::{Instant, Rate},
};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

mod sdraw;

struct NoClock;
impl TimeSource for NoClock {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 56,
            zero_indexed_month: 8,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

fn fnv1a(mut h: u32, bytes: &[u8]) -> u32 {
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[main]
fn main() -> ! {
    let mut p = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();
    let mut round: u32 = 0;

    loop {
        round += 1;
        println!();
        println!("[spike-sd] ===== round {} =====", round);

        // ---- 1. RC522 on P3 --------------------------------------------------
        let mut versions = [0u8; 2];
        for (i, pull) in [Pull::Up, Pull::Down].into_iter().enumerate() {
            let miso = Input::new(p.GPIO2.reborrow(), InputConfig::default().with_pull(pull));
            let mut spi = Spi::new(
                p.SPI3.reborrow(),
                SpiConfig::default()
                    .with_frequency(Rate::from_mhz(1))
                    .with_mode(Mode::_0),
            )
            .expect("spi3 rc522 config")
            .with_sck(p.GPIO14.reborrow())
            .with_mosi(p.GPIO21.reborrow())
            .with_miso(miso);
            let mut cs = Output::new(p.GPIO3.reborrow(), Level::High, OutputConfig::default());
            delay.delay_millis(2);
            cs.set_low();
            // VersionReg 0x37: address byte = 0x80 (read) | (0x37 << 1).
            let mut buf = [0x80 | (0x37 << 1), 0x00];
            let ok = SpiBus::transfer_in_place(&mut spi, &mut buf).is_ok();
            let _ = SpiBus::flush(&mut spi);
            cs.set_high();
            versions[i] = buf[1];
            println!(
                "[spike-sd] rc522 VersionReg (MISO pull {}): 0x{:02X} {}",
                if i == 0 { "UP  " } else { "DOWN" },
                buf[1],
                if ok { "" } else { "(spi error)" }
            );
        }
        let verdict = match versions {
            [0xFF, 0x00] => "ABSENT: MISO follows the pad pull, nothing drives it",
            [a, b] if a == b && matches!(a, 0x82 | 0x88 | 0x90 | 0x91 | 0x92 | 0xB2) => {
                "PRESENT: both reads agree on a known MFRC522 version"
            }
            _ => "UNCLEAR: reads disagree or an unknown byte, see above",
        };
        println!("[spike-sd] rc522 verdict: {}", verdict);

        // ---- 2a. MicroSD, card level (raw, read-only) ---------------------
        {
            let mut spi = Spi::new(
                p.SPI3.reborrow(),
                SpiConfig::default()
                    .with_frequency(Rate::from_khz(400))
                    .with_mode(Mode::_0),
            )
            .expect("spi3 sdraw config")
            .with_sck(p.GPIO38.reborrow())
            .with_mosi(p.GPIO40.reborrow())
            .with_miso(p.GPIO39.reborrow());
            let mut cs = Output::new(p.GPIO47.reborrow(), Level::High, OutputConfig::default());
            match sdraw::identify(&mut spi, &mut cs, &delay) {
                None => println!("[spike-sd] card level: NO CARD answered CMD0 (see [sdraw] lines)"),
                Some(card) => {
                    sdraw::report(&card);
                    let _ = spi.apply_config(
                        &SpiConfig::default()
                            .with_frequency(Rate::from_mhz(4))
                            .with_mode(Mode::_0),
                    );
                    sdraw::describe_layout(&mut spi, &mut cs, card.sdhc);
                }
            }
        }

        // ---- 2b. MicroSD, filesystem (embedded-sdmmc, read-only) -------------
        {
            let mut spi = Spi::new(
                p.SPI3.reborrow(),
                SpiConfig::default()
                    .with_frequency(Rate::from_khz(400))
                    .with_mode(Mode::_0),
            )
            .expect("spi3 sd config")
            .with_sck(p.GPIO38.reborrow())
            .with_mosi(p.GPIO40.reborrow())
            .with_miso(p.GPIO39.reborrow());
            let cs = Output::new(p.GPIO47.reborrow(), Level::High, OutputConfig::default());
            // SD power-up: >= 74 clocks with CS high before CMD0.
            let _ = SpiBus::write(&mut spi, &[0xFF; 10]);
            let _ = SpiBus::flush(&mut spi);
            let dev = ExclusiveDevice::new(spi, cs, delay).expect("sd spi device");
            let card = SdCard::new(dev, delay);

            match card.num_bytes() {
                Err(e) => println!("[spike-sd] sd: NO CARD / no answer: {:?}", e),
                Ok(n) => {
                    println!(
                        "[spike-sd] sd: card answers, {} bytes ({} MiB), type {:?}",
                        n,
                        n / (1024 * 1024),
                        card.get_card_type()
                    );
                    card.spi(|d| {
                        let _ = d.bus_mut().apply_config(
                            &SpiConfig::default()
                                .with_frequency(Rate::from_mhz(10))
                                .with_mode(Mode::_0),
                        );
                    });
                    probe_fs(card);
                }
            }
        }

        delay.delay_millis(5000);
    }
}

fn probe_fs<S, D>(card: SdCard<S, D>)
where
    S: embedded_hal::spi::SpiDevice<u8>,
    D: embedded_hal::delay::DelayNs,
{
    let vm: VolumeManager<_, _, 4, 4, 1> = VolumeManager::new_with_limits(card, NoClock, 0);
    let volume = match vm.open_volume(VolumeIdx(0)) {
        Ok(v) => v,
        Err(e) => {
            println!("[spike-sd] fat: volume 0 did not open: {:?}", e);
            return;
        }
    };
    let root = match volume.open_root_dir() {
        Ok(r) => r,
        Err(e) => {
            println!("[spike-sd] fat: root dir did not open: {:?}", e);
            return;
        }
    };
    let mut first: Option<(embedded_sdmmc::ShortFileName, u32)> = None;
    let mut count = 0u32;
    let r = root.iterate_dir(|e| {
        count += 1;
        if count <= 40 {
            println!(
                "[spike-sd] fat: {:12} {:>10} {}",
                e.name,
                e.size,
                if e.attributes.is_directory() { "<DIR>" } else { "" }
            );
        }
        if first.is_none()
            && !e.attributes.is_directory()
            && !e.attributes.is_volume()
            && !e.attributes.is_hidden()
            && !e.attributes.is_system()
            && e.size > 0
        {
            first = Some((e.name.clone(), e.size));
        }
    });
    println!("[spike-sd] fat: root has {} entries ({:?})", count, r.map(|_| "ok"));
    // The positive-control file JP writes: its exact text, if present.
    match root.open_file_in_dir("TAPSTONE.TXT", FileMode::ReadOnly) {
        Ok(f) => {
            let mut t = [0u8; 64];
            let n = f.read(&mut t).unwrap_or(0);
            println!(
                "[spike-sd] fat: TAPSTONE.TXT ({} B) says {:?}",
                n,
                core::str::from_utf8(&t[..n]).unwrap_or("<not utf-8>")
            );
        }
        Err(e) => println!("[spike-sd] fat: no TAPSTONE.TXT ({:?})", e),
    }
    let Some((name, size)) = first else {
        println!("[spike-sd] fat: no regular file in root to read back");
        return;
    };
    let file = match root.open_file_in_dir(&name, FileMode::ReadOnly) {
        Ok(f) => f,
        Err(e) => {
            println!("[spike-sd] fat: open {} failed: {:?}", name, e);
            return;
        }
    };
    let mut buf = [0u8; 512];
    let mut total = 0u32;
    let mut h = 0x811C_9DC5u32;
    let mut head = [0u8; 32];
    let mut head_len = 0usize;
    let t0 = Instant::now();
    while !file.is_eof() {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if head_len < head.len() {
                    let k = (head.len() - head_len).min(n);
                    head[head_len..head_len + k].copy_from_slice(&buf[..k]);
                    head_len += k;
                }
                total += n as u32;
                h = fnv1a(h, &buf[..n]);
            }
            Err(e) => {
                println!("[spike-sd] fat: read error after {} bytes: {:?}", total, e);
                return;
            }
        }
    }
    let us = t0.elapsed().as_micros().max(1);
    println!(
        "[spike-sd] fat: read {} ({} of {} bytes) fnv1a=0x{:08X} in {} us = {} KiB/s",
        name,
        total,
        size,
        h,
        us,
        (total as u64 * 1_000_000 / us) / 1024
    );
    println!("[spike-sd] fat: head {:02X?}", &head[..head_len]);
    println!("[spike-sd] VERDICT: SD MOUNTED, FAT root listed, file read back");
}
