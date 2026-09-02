//! spike-scry — **harness proof for the realm scry station (smol node id 163).**
//!
//! Board: the third ES3C28P (`14:C1:9F:D1:CC:64`) + MFRC522 reader wired to the
//! P3 "Expanded IO" jack. This is a THROWAWAY bring-up spike in the ../spike
//! tradition: it exists to prove, in order,
//!
//!   1. the chip boots and the USB-JTAG console works
//!   2. the RC522 answers on SPI3 (version register 0x91/0x92) — i.e. the
//!      four P3 signal wires + P4 power/RST-high harness is GOOD
//!   3. a MIFARE tag in the field yields a UID, debounced one-per-presence
//!
//! No display, no radio, no mesh — the real station firmware (smol#540) owns
//! those. If (2) prints a version, JP's soldering is vindicated.
//!
//! Harness (labels/scry/rc522-s3cyd-wiring.md, silk-verified 2026-09-01):
//!   P3: IO2=MISO · IO3=CS · IO14=SCK · IO21=MOSI
//!   P4: GND + 3V3 (RC522 RST tied to 3V3 — soft reset only) · IRQ n/c
//!
//! IO3 is an S3 strapping pin (JTAG select) — safe as a runtime output that
//! idles high, which is also its safe strap state. Nothing here touches GPIO18
//! (⛔ FT6336 landmine) or 33–37 (octal PSRAM).

#![no_std]
#![no_main]

use embedded_hal_bus::spi::ExclusiveDevice;
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
};
use esp_println::println;
use mfrc522::comm::blocking::spi::SpiInterface;
use mfrc522::Mfrc522;

esp_bootloader_esp_idf::esp_app_desc!();

const NODE_ID: u8 = 163;

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let mut delay = Delay::new();

    println!();
    println!(
        "[scry-{}] === scry station spike — ES3C28P + RC522 on P3 (smol#540) ===",
        NODE_ID
    );

    // RC522 tops out at 10 MHz; 5 MHz leaves margin for the pigtail run.
    let spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(5))
            .with_mode(Mode::_0),
    )
    .expect("spi3 config")
    .with_sck(peripherals.GPIO14)
    .with_mosi(peripherals.GPIO21)
    .with_miso(peripherals.GPIO2);

    let cs = Output::new(peripherals.GPIO3, Level::High, OutputConfig::default());
    let spi_dev = ExclusiveDevice::new(spi, cs, delay).expect("spi device");

    let mut rc522 = match Mfrc522::new(SpiInterface::new(spi_dev)).init() {
        Ok(m) => m,
        Err(_) => {
            println!("[scry-{}] RC522 init FAILED — check P3 harness / P4 power.", NODE_ID);
            loop {
                delay.delay_millis(5000);
                println!("[scry-{}] (init failed; halted)", NODE_ID);
            }
        }
    };

    // THE HARNESS PROOF: the version register answers 0x91 (v1) / 0x92 (v2)
    // on genuine/clone MFRC522s. Any sane value here = SPI wiring is GOOD.
    match rc522.version() {
        Ok(v) => println!(
            "[scry-{}] RC522 version: 0x{:02X} — harness GOOD {}",
            NODE_ID,
            v,
            match v {
                0x91 => "(MFRC522 v1)",
                0x92 => "(MFRC522 v2)",
                0x88 => "(FM17522 clone)",
                _ => "(unrecognised but talking)",
            }
        ),
        Err(_) => println!(
            "[scry-{}] RC522 version read FAILED — SCK/MISO/MOSI/CS suspect.",
            NODE_ID
        ),
    }

    println!("[scry-{}] polling for tags — tap a card or fob…", NODE_ID);

    // One event per tag presence: remember the last UID, re-arm after the
    // field has been empty for a few polls.
    let mut last: Option<[u8; 10]> = None;
    let mut last_len = 0usize;
    let mut empty_polls: u32 = 0;

    loop {
        delay.delay_millis(150);
        match rc522.reqa() {
            Ok(atqa) => {
                empty_polls = 0;
                if let Ok(uid) = rc522.select(&atqa) {
                    let bytes = uid.as_bytes();
                    let mut buf = [0u8; 10];
                    buf[..bytes.len()].copy_from_slice(bytes);
                    let is_new = match last {
                        Some(prev) => prev[..last_len] != buf[..bytes.len()],
                        None => true,
                    };
                    if is_new {
                        print_uid(bytes);
                        last = Some(buf);
                        last_len = bytes.len();
                    }
                    let _ = rc522.hlta();
                }
            }
            Err(_) => {
                empty_polls += 1;
                if empty_polls == 20 && last.is_some() {
                    println!("[scry-{}] field clear — re-armed", NODE_ID);
                    last = None;
                    last_len = 0;
                }
            }
        }
    }
}

fn print_uid(bytes: &[u8]) {
    // No alloc in this spike — print hex bytes piecewise.
    esp_println::print!("[scry-163] TAP uid=");
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            esp_println::print!(":");
        }
        esp_println::print!("{:02X}", b);
    }
    println!();
}
