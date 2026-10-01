//! sd-provision: the ES3C28P writes the shrine's voice pack (tapstone 0033) to the card in its own
//! MicroSD slot, sent over the board's USB-Serial-JTAG link by tapstone's `tools/sd_provision.py`.
//!
//! A separate image, never the station: the station mounts its card read-only by construction
//! (`check_readonly.sh --fs`), and this is the one smol image that writes a card. It has no radio.
//! Flash it, provision, then flash the station image back (`README.md`).
//!
//! The board writes what the host tells it to, in 512 B blocks: the host builds the FAT32 image
//! with `mkfs.vfat` and mtools (tapstone's `sd_prepare.py` code), so the format is a trusted
//! tool's. The wire format is `rust/sdprov-proto`:
//! - **INFO**: the card's size.
//! - **ARM**: must echo that size before any write. A card swapped mid-session, or a host talking
//!   to the wrong board, is refused.
//! - **READ**: blocks, read-only (the host's blank check, before anything is written).
//! - **WRITE** / **ZERO**: blocks.
//! - **FILE**: a file's size and sha256, read back through the FAT with a read-only mount (the
//!   path the station reads), so each file is checked on the card itself.
//!
//! Every answer echoes the request's seq. A frame with a bad CRC is answered NAK, and the host
//! sends it again.
#![no_std]
#![no_main]

use embedded_hal::spi::SpiBus;
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{
    Block, BlockDevice, BlockIdx, Mode as FileMode, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager,
};
use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    main,
    spi::{
        master::{Config as SpiConfig, Spi},
        Mode,
    },
    time::Rate,
    usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagTx},
    Blocking,
};
use esp_println::println;
use sdprov_proto as proto;
use sha2::{Digest, Sha256};

esp_bootloader_esp_idf::esp_app_desc!();

struct NoClock;
impl TimeSource for NoClock {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp { year_since_1970: 56, zero_indexed_month: 8, zero_indexed_day: 29, hours: 0, minutes: 0, seconds: 0 }
    }
}

type Card = SdCard<ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, Delay>, Delay>;

fn reply(tx: &mut UsbSerialJtagTx<'static, Blocking>, kind: u8, seq: u16, payload: &[u8]) {
    let mut out = [0u8; proto::FRAME_MAX];
    if let Some(n) = proto::encode(kind, seq, payload, &mut out) {
        let _ = tx.write(&out[..n]);
        let _ = tx.flush_tx();
    }
}

/// A file's size and sha256 through a read-only FAT mount. The card goes in and comes back out.
fn hash_file(card: Card, path: &str) -> (Card, Result<(u32, [u8; 32]), u8>) {
    let vm: VolumeManager<Card, NoClock, 4, 1, 1> = VolumeManager::new_with_limits(card, NoClock, 0);
    let r = (|| {
        let vol = vm.open_raw_volume(VolumeIdx(0)).map_err(|_| proto::E_CARD)?;
        let mut dir = vm.open_root_dir(vol).map_err(|_| proto::E_CARD)?;
        let mut parts = path.split('/').peekable();
        let mut out = Err(proto::E_NOT_FOUND);
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                let d = vm.open_dir(dir, part).map_err(|_| proto::E_NOT_FOUND)?;
                let _ = vm.close_dir(dir);
                dir = d;
                continue;
            }
            let f = vm.open_file_in_dir(dir, part, FileMode::ReadOnly).map_err(|_| proto::E_NOT_FOUND)?;
            let mut h = Sha256::new();
            let mut size = 0u32;
            let mut buf = [0u8; 512];
            let mut ok = true;
            loop {
                match vm.read(f, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        h.update(&buf[..n]);
                        size += n as u32;
                    }
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            let _ = vm.close_file(f);
            let mut sha = [0u8; 32];
            sha.copy_from_slice(&h.finalize());
            out = if ok { Ok((size, sha)) } else { Err(proto::E_CARD) };
        }
        let _ = vm.close_dir(dir);
        let _ = vm.close_volume(vol);
        out
    })();
    let (card, _) = vm.free();
    (card, r)
}

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();
    println!("[sdprov] tapstone SD provisioner (sdprov-proto). Nothing is written until ARM.");

    // The slot on SPI3 (`board_s3::SD_PINS`), as the station and spike-sd drive it.
    let mut spi = Spi::new(p.SPI3, SpiConfig::default().with_frequency(Rate::from_khz(400)).with_mode(Mode::_0))
        .expect("spi3")
        .with_sck(p.GPIO38)
        .with_mosi(p.GPIO40)
        .with_miso(p.GPIO39);
    let cs = Output::new(p.GPIO47, Level::High, OutputConfig::default());
    let _ = SpiBus::write(&mut spi, &[0xFF; 10]);
    let _ = SpiBus::flush(&mut spi);
    let dev = ExclusiveDevice::new(spi, cs, delay).expect("sd device");
    let sd = SdCard::new(dev, delay);
    let bytes = sd.num_bytes().ok();
    match bytes {
        Some(n) => {
            sd.spi(|d| {
                let _ = d.bus_mut().apply_config(&SpiConfig::default().with_frequency(Rate::from_mhz(10)).with_mode(Mode::_0));
            });
            println!("[sdprov] card: {} bytes ({} MiB)", n, n >> 20);
        }
        None => println!("[sdprov] no card"),
    }

    let (mut rx, mut tx) = UsbSerialJtag::new(p.USB_DEVICE).split();
    let mut dec = proto::Decoder::new();
    let mut card = Some(sd);
    let mut armed = false;
    let mut blocks: [Block; proto::MAX_BLOCKS] = core::array::from_fn(|_| Block::new());
    loop {
        let mut chunk = [0u8; 64];
        let n = rx.drain_rx_fifo(&mut chunk);
        let mut off = 0;
        while off < n {
            off += dec.push(&chunk[off..n]);
            while let Some(ev) = dec.next_event() {
                let (kind, seq, pl) = match ev {
                    proto::Event::Corrupt { seq } => {
                        reply(&mut tx, proto::NAK, seq, &[]);
                        continue;
                    }
                    proto::Event::Frame { kind, seq, payload } => (kind, seq, payload),
                };
                let u32_at = |i: usize| u32::from_le_bytes([pl[i], pl[i + 1], pl[i + 2], pl[i + 3]]);
                match kind {
                    proto::INFO => match bytes {
                        Some(b) => reply(&mut tx, proto::OK, seq, &b.to_le_bytes()),
                        None => reply(&mut tx, proto::ERR, seq, &[proto::E_CARD]),
                    },
                    proto::ARM => {
                        let want = (pl.len() == 8).then(|| u64::from_le_bytes(pl.try_into().unwrap_or([0; 8])));
                        if bytes.is_some() && want == bytes {
                            armed = true;
                            reply(&mut tx, proto::OK, seq, &[]);
                        } else {
                            reply(&mut tx, proto::ERR, seq, &[proto::E_ARM_MISMATCH]);
                        }
                    }
                    proto::WRITE | proto::ZERO if !armed => reply(&mut tx, proto::ERR, seq, &[proto::E_NOT_ARMED]),
                    proto::WRITE => {
                        let data = pl.len().saturating_sub(4);
                        let k = data / 512;
                        let Some(c) = card.as_ref() else { continue };
                        if pl.len() < 4 + 512 || data % 512 != 0 || k > proto::MAX_BLOCKS {
                            reply(&mut tx, proto::ERR, seq, &[proto::E_ARGS]);
                            continue;
                        }
                        for (i, b) in blocks[..k].iter_mut().enumerate() {
                            b.contents.copy_from_slice(&pl[4 + 512 * i..4 + 512 * (i + 1)]);
                        }
                        match c.write(&blocks[..k], BlockIdx(u32_at(0))) {
                            Ok(()) => reply(&mut tx, proto::OK, seq, &[]),
                            Err(_) => reply(&mut tx, proto::ERR, seq, &[proto::E_CARD]),
                        }
                    }
                    proto::ZERO => {
                        let Some(c) = card.as_ref() else { continue };
                        if pl.len() != 8 {
                            reply(&mut tx, proto::ERR, seq, &[proto::E_ARGS]);
                            continue;
                        }
                        let (mut lba, mut left) = (u32_at(0), u32_at(4));
                        let zeros: [Block; proto::MAX_BLOCKS] = core::array::from_fn(|_| Block::new());
                        let mut ok = true;
                        while left > 0 && ok {
                            let k = (left as usize).min(proto::MAX_BLOCKS);
                            ok = c.write(&zeros[..k], BlockIdx(lba)).is_ok();
                            lba += k as u32;
                            left -= k as u32;
                        }
                        reply(&mut tx, if ok { proto::OK } else { proto::ERR }, seq, if ok { &[] } else { &[proto::E_CARD] });
                    }
                    proto::READ => {
                        let Some(c) = card.as_ref() else { continue };
                        let k = if pl.len() == 5 { pl[4] as usize } else { 0 };
                        if k == 0 || k > proto::MAX_BLOCKS {
                            reply(&mut tx, proto::ERR, seq, &[proto::E_ARGS]);
                            continue;
                        }
                        match c.read(&mut blocks[..k], BlockIdx(u32_at(0))) {
                            Ok(()) => {
                                let mut data = [0u8; proto::MAX_BLOCKS * 512];
                                for (i, b) in blocks[..k].iter().enumerate() {
                                    data[512 * i..512 * (i + 1)].copy_from_slice(&b.contents);
                                }
                                reply(&mut tx, proto::OK, seq, &data[..512 * k]);
                            }
                            Err(_) => reply(&mut tx, proto::ERR, seq, &[proto::E_CARD]),
                        }
                    }
                    proto::FILE => {
                        let Ok(path) = core::str::from_utf8(pl) else {
                            reply(&mut tx, proto::ERR, seq, &[proto::E_ARGS]);
                            continue;
                        };
                        let Some(c) = card.take() else { continue };
                        let (c, r) = hash_file(c, path);
                        card = Some(c);
                        match r {
                            Ok((size, sha)) => {
                                let mut a = [0u8; 36];
                                a[..4].copy_from_slice(&size.to_le_bytes());
                                a[4..].copy_from_slice(&sha);
                                reply(&mut tx, proto::OK, seq, &a);
                            }
                            Err(e) => reply(&mut tx, proto::ERR, seq, &[e]),
                        }
                    }
                    _ => reply(&mut tx, proto::ERR, seq, &[proto::E_UNKNOWN]),
                }
            }
        }
    }
}
