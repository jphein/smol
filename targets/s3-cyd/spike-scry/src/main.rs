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

#[cfg(feature = "wifi")]
mod mqtt;
#[cfg(feature = "wifi")]
mod net;
#[cfg(feature = "wifi")]
mod radio_dev;
#[cfg(feature = "wifi")]
mod scry;

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

    // ---- network (feature `wifi`) — the station fetches its own face --------
    #[cfg(feature = "wifi")]
    let mut net = net::init(peripherals.TIMG0, peripherals.SW_INTERRUPT, peripherals.WIFI);

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
    // The status page stays up for a minute after a tap and REFRESHES while it
    // is up, so the chronicle streams on the glass exactly like on a phone.
    let mut tick: u32 = 0;
    let mut frame_until: u32 = 0;
    let mut idle_painted = false;
    let mut host_shown = [0u8; 48];
    let mut host_shown_len = 0usize;
    const FRAME_HOLD_TICKS: u32 = 400; // ~60 s at 150 ms
    const REFRESH_TICKS: u32 = 33; // ~5 s

    loop {
        // ⚠️ The network stack is SERVICED for the poll window, never slept
        // through — net.rs is explicit that sleeping here starves the RX path
        // and was M2's panic. Without this call the state machine never leaves
        // Backoff and the station silently never gets a lease (cost: one flash).
        #[cfg(feature = "wifi")]
        net.tick(&delay, 150);
        #[cfg(not(feature = "wifi"))]
        delay.delay_millis(150);
        tick = tick.wrapping_add(1);

        #[cfg(feature = "wifi")]
        if tick % 40 == 0 {
            println!("[scry-{}] link: {}", NODE_ID, net.state().label());
        }

        // While idle, re-fetch the resting face every ~10 s. It carries a live
        // clock, and it is how the imbue prompt ("present a blank card") and the
        // "card imbued" confirmation reach the glass without any push channel.
        #[cfg(feature = "wifi")]
        if idle_painted && frame_until == 0 && tick % 67 == 0 {
            let _ = net.with_stack(|iface, dev, socks, h| {
                scry::paint_idle(iface, dev, socks, h, &delay, &mut display)
            });
        }

        // First link-up: put on the station's face. Until then the local
        // bootstrap text stands, so a station with no AP is still honest.
        #[cfg(feature = "wifi")]
        if !idle_painted {
            if let Some(Some(ms)) = net.with_stack(|iface, dev, socks, h| {
                scry::paint_idle(iface, dev, socks, h, &delay, &mut display)
            }) {
                println!("[scry-{}] idle face painted in {} ms", NODE_ID, ms);
                idle_painted = true;
            }
        }

        // live refresh of the page being shown
        #[cfg(feature = "wifi")]
        if tick < frame_until && host_shown_len > 0 && tick % REFRESH_TICKS == 0 {
            let hb = host_shown;
            let host = core::str::from_utf8(&hb[..host_shown_len]).unwrap_or("");
            let _ = net.with_stack(|iface, dev, socks, h| {
                scry::paint(iface, dev, socks, h, &delay, &mut display, host, false)
            });
        }
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
                        let mut text_buf = [0u8; 40];
                        let text = fmt_uid(bytes, &mut text_buf);

                        // THE SCRY: resolve the sigil server-side, then wear the
                        // status page. Everything below is best-effort — a
                        // station with no link still reads tags and says so.
                        #[cfg(feature = "wifi")]
                        {
                            let mut host_buf = [0u8; 48];
                            let mut painted = false;
                            let mut host_len = 0usize;
                            net.with_stack(|iface, dev, socks, h| {
                                if let Some(host) =
                                    scry::tap(iface, dev, socks, h, &delay, text, &mut host_buf)
                                {
                                    host_len = host.len();
                                }
                            });
                            if host_len > 0 {
                                let host_copy = host_buf;
                                let host = core::str::from_utf8(&host_copy[..host_len]).unwrap_or("");
                                println!("[scry-{}] sigil bound to '{}' — summoning + painting", NODE_ID, host);
                                if let Some(Some(ms)) = net.with_stack(|iface, dev, socks, h| {
                                    scry::paint(iface, dev, socks, h, &delay, &mut display, host, false)
                                }) {
                                    println!("[scry-{}] frame painted in {} ms", NODE_ID, ms);
                                    painted = true;
                                    frame_until = tick + FRAME_HOLD_TICKS;
                                    host_shown = host_copy;
                                    host_shown_len = host_len;
                                }
                            } else {
                                if let Some(Some(ms)) = net.with_stack(|iface, dev, socks, h| {
                                    scry::paint(iface, dev, socks, h, &delay, &mut display, text, true)
                                }) {
                                    println!("[scry-{}] unbound sigil frame in {} ms", NODE_ID, ms);
                                    painted = true;
                                    frame_until = tick + FRAME_HOLD_TICKS;
                                }
                            }
                            if !painted {
                                paint_local_tap(&mut display, text, title, body, uid_area);
                            }
                        }
                        #[cfg(not(feature = "wifi"))]
                        paint_local_tap(&mut display, text, title, body, uid_area);

                        last = Some(buf);
                        last_len = bytes.len();
                    }
                    let _ = rc522.hlta();
                }
            }
            Err(_) => {
                empty_polls += 1;
                #[cfg(feature = "wifi")]
                if frame_until > 0 && tick >= frame_until {
                    frame_until = 0;
                    host_shown_len = 0;
                    if net
                        .with_stack(|iface, dev, socks, h| {
                            scry::paint_idle(iface, dev, socks, h, &delay, &mut display)
                        })
                        .flatten()
                        .is_none()
                    {
                        display.clear(BG).ok();
                        Text::new("tap a card...", Point::new(20, 130), body)
                            .draw(&mut display)
                            .ok();
                    }
                }
                if empty_polls == 20 && last.is_some() {
                    println!("[scry-{}] field clear — re-armed", NODE_ID);
                    // The card was lifted: the glass returns to its resting
                    // face. ~3 s of grace (the re-arm threshold) so a quick tap
                    // still leaves a readable beat; the summoned session and the
                    // phone page carry the detail from here.
                    #[cfg(feature = "wifi")]
                    if frame_until > 0 {
                        frame_until = 0;
                        host_shown_len = 0;
                        if net
                            .with_stack(|iface, dev, socks, h| {
                                scry::paint_idle(iface, dev, socks, h, &delay, &mut display)
                            })
                            .flatten()
                            .is_some()
                        {
                            println!("[scry-{}] card lifted — back to the orb", NODE_ID);
                        }
                    }
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

/// Degraded paint: no link (or the server refused) — say so honestly and show
/// the sigil, so the station is still useful as a reader.
fn paint_local_tap<D>(
    display: &mut D,
    text: &str,
    title: MonoTextStyle<'static, Rgb565>,
    body: MonoTextStyle<'static, Rgb565>,
    uid_area: Rectangle,
) where
    D: DrawTarget<Color = Rgb565>,
{
    uid_area
        .into_styled(PrimitiveStyle::with_fill(BG))
        .draw(display)
        .ok();
    Text::new("TAP:", Point::new(20, 175), title).draw(display).ok();
    Text::new(text, Point::new(80, 175), body).draw(display).ok();
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
