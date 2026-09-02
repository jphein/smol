//! spike-scry — **harness proof for the realm scry station (smol node id 163).**
//!
//! Board: the third ES3C28P (`14:C1:9F:D1:CC:64`) + MFRC522 reader wired to the
//! P3 "Expanded IO" jack. This is a THROWAWAY bring-up spike in the ../spike
//! tradition: it exists to prove, in order,
//!
//!   1. the chip boots and the USB-JTAG console works
//!   2. the RC522 answers on SPI3 (version register) — i.e. the four P3
//!      signal wires + P4 power/RST-high harness is GOOD  [PROVEN: 0x82]
//!   3. a MIFARE tag in the field yields a UID, debounced one-per-presence
//!      [PROVEN: 61:1C:6E:66, multiple tap/re-arm cycles on glass]
//!   4. (bench QoL) the panel lights and shows station state + last UID —
//!      display recipe copied verbatim from ../spike; the REAL UI is the
//!      Slint GUI flavor's job (smol#540).
//!
//! Harness (labels/scry/rc522-s3cyd-wiring.md, silk-verified 2026-09-01):
//!   P3: IO2=MISO · IO3=CS · IO14=SCK · IO21=MOSI
//!   P4: GND + 3V3 (RC522 RST tied to 3V3 — soft reset only) · IRQ n/c
//!
//! Display: SPI2 CLK=12 MOSI=11 CS=10 DC=46, backlight 45 active-HIGH,
//! MADCTL 0x28 landscape, INVON + BGR, NoResetPin (RST bonded to CHIP_PU).
//! Entirely separate bus from the RC522 — no interaction.
//!
//! IO3 is an S3 strapping pin (JTAG select) — safe as a runtime output that
//! idles high. Nothing here touches GPIO18 (⛔ FT6336 landmine) or 33–37
//! (octal PSRAM).

#![no_std]
#![no_main]

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::Text,
};
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
use mfrc522::comm::blocking::spi::SpiInterface as Rc522SpiInterface;
use mfrc522::Mfrc522;
use mipidsi::{
    interface::SpiInterface as DisplaySpiInterface,
    models::ILI9341Rgb565,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

const NODE_ID: u8 = 163;

/// Landscape MADCTL 0x28 — see ../spike/src/main.rs for why the
/// `.flip_vertical()` is load-bearing and must not be "tidied away".
const ORIENTATION: Orientation = Orientation::new()
    .rotate(Rotation::Deg90)
    .flip_vertical();

static DISPLAY_BUF: StaticCell<[u8; 512]> = StaticCell::new();

const BG: Rgb565 = Rgb565::new(2, 2, 8); // deep night-purple, realmwatch-ish
const INK: Rgb565 = Rgb565::new(29, 56, 25); // parchment
const ACCENT: Rgb565 = Rgb565::new(23, 40, 31); // pale violet

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let mut delay = Delay::new();

    println!();
    println!(
        "[scry-{}] === scry station spike — ES3C28P + RC522 on P3 (smol#540) ===",
        NODE_ID
    );

    // ---- display (SPI2, recipe verbatim from ../spike) ---------------------
    let mut backlight = Output::new(peripherals.GPIO45, Level::Low, OutputConfig::default());
    let lcd_spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(40))
            .with_mode(Mode::_0),
    )
    .expect("spi2 config")
    .with_sck(peripherals.GPIO12)
    .with_mosi(peripherals.GPIO11);
    let lcd_cs = Output::new(peripherals.GPIO10, Level::High, OutputConfig::default());
    let lcd_dc = Output::new(peripherals.GPIO46, Level::Low, OutputConfig::default());
    let lcd_dev = ExclusiveDevice::new(lcd_spi, lcd_cs, delay).expect("lcd spi device");
    let di = DisplaySpiInterface::new(lcd_dev, lcd_dc, DISPLAY_BUF.init([0u8; 512]));
    let mut display = Builder::new(ILI9341Rgb565, di)
        .display_size(240, 320)
        .orientation(ORIENTATION)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut delay)
        .expect("ili9341 init");
    display.clear(BG).ok();
    backlight.set_high();

    let title = MonoTextStyle::new(&FONT_10X20, ACCENT);
    let body = MonoTextStyle::new(&FONT_10X20, INK);
    Text::new("SCRY STATION 163", Point::new(60, 40), title)
        .draw(&mut display)
        .ok();

    // ---- RC522 (SPI3 via GPIO matrix, P3 harness) ---------------------------
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

    let mut rc522 = match Mfrc522::new(Rc522SpiInterface::new(spi_dev)).init() {
        Ok(m) => m,
        Err(_) => {
            println!("[scry-{}] RC522 init FAILED — check P3 harness / P4 power.", NODE_ID);
            Text::new("READER FAULT — check harness", Point::new(20, 120), body)
                .draw(&mut display)
                .ok();
            loop {
                delay.delay_millis(5000);
            }
        }
    };

    match rc522.version() {
        Ok(v) => {
            println!("[scry-{}] RC522 version: 0x{:02X} — harness GOOD", NODE_ID, v);
            Text::new("reader: GOOD", Point::new(20, 90), body)
                .draw(&mut display)
                .ok();
        }
        Err(_) => {
            println!("[scry-{}] RC522 version read FAILED", NODE_ID);
        }
    }
    Text::new("tap a card...", Point::new(20, 130), body)
        .draw(&mut display)
        .ok();

    println!("[scry-{}] polling for tags — tap a card or fob…", NODE_ID);

    let uid_area = Rectangle::new(Point::new(0, 150), Size::new(320, 60));
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
                        // paint: clear the UID strip, then hex the tag
                        uid_area
                            .into_styled(PrimitiveStyle::with_fill(BG))
                            .draw(&mut display)
                            .ok();
                        let mut text_buf = [0u8; 40];
                        let text = fmt_uid(bytes, &mut text_buf);
                        Text::new("TAP:", Point::new(20, 175), title)
                            .draw(&mut display)
                            .ok();
                        Text::new(text, Point::new(80, 175), body)
                            .draw(&mut display)
                            .ok();
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
                    uid_area
                        .into_styled(PrimitiveStyle::with_fill(BG))
                        .draw(&mut display)
                        .ok();
                    Text::new("tap a card...", Point::new(20, 175), body)
                        .draw(&mut display)
                        .ok();
                    last = None;
                    last_len = 0;
                }
            }
        }
    }
}

fn print_uid(bytes: &[u8]) {
    esp_println::print!("[scry-163] TAP uid=");
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            esp_println::print!(":");
        }
        esp_println::print!("{:02X}", b);
    }
    println!();
}

/// Hex-format a UID into a caller buffer, no alloc. Returns the &str view.
fn fmt_uid<'a>(bytes: &[u8], out: &'a mut [u8; 40]) -> &'a str {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut n = 0;
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out[n] = b':';
            n += 1;
        }
        out[n] = HEX[(b >> 4) as usize];
        out[n + 1] = HEX[(b & 0xF) as usize];
        n += 2;
    }
    core::str::from_utf8(&out[..n]).unwrap_or("?")
}
