//! s3-cyd spike — **M1: board is alive, screen is lit, button reads.**
//!
//! Target: ES3C28P (ESP32-S3 N16R8) — the new blank dev board, **smol node id 162**.
//! Not board 161 (emberburrito's hearth terminal), not the ember satellites.
//!
//! This is a THROWAWAY bring-up ladder, not the phase-2 smol image. M1 proves the
//! four things that must be true before anything else is worth writing:
//!
//!   1. the chip boots and the console works (heartbeat log)
//!   2. octal PSRAM maps (expect 8388608 B — the SIZE, not the address)
//!   3. the panel initialises and paints in the right orientation
//!   4. the one hardware button reads
//!
//! No WiFi, no radio, no touch, no audio in M1.
//!
//! Later tiers stack on top and are OFF by default:
//!   * `--features wifi`  -> M2: `net.rs`, STA associate + smoltcp DHCP.
//!   * `--features radio` -> M3: the above, plus `espnow_probe.rs`.
//!
//! ---------------------------------------------------------------------------
//! BOARD FACTS ENCODED HERE — ground truth, not guesses
//! ---------------------------------------------------------------------------
//! Traced from the vendor ES3C28P schematic into emberboy's
//! `retro-go/.../targets/ember-s3/config.h`, independently confirmed by a live
//! ESPHome config (`ember.realm.watch/esphome/ember-satellite.yaml`), and proven a
//! third time by burrito-fw running on this exact hardware.
//!
//!   * SPI2 @ 40 MHz — CLK=GPIO12, MOSI=GPIO11, CS=GPIO10, DC=GPIO46.
//!     MISO=GPIO13 is unused (write-only panel).
//!   * NO LCD RESET PIN — it is bonded to CHIP_PU/EN, so mipidsi gets `NoResetPin`
//!     and relies on the software reset inside `init()`.
//!   * Panel needs INVON -> `ColorInversion::Inverted`, and BGR colour order.
//!   * Landscape MADCTL is **0x28** — see `ORIENTATION` below. Load-bearing.
//!   * Backlight GPIO45 is active-HIGH (BSS138 gate).
//!   * GPIO0 is the BOOT button, active-LOW, and is the board's ENTIRE hardware
//!     input budget. (K1/RESET is not readable.)
//!
//! ---------------------------------------------------------------------------
//! ⛔ TWO PINS THIS FILE MUST NEVER TOUCH
//! ---------------------------------------------------------------------------
//!
//! **GPIO18 (CTP_RST) — the touch controller's reset.**
//! The widely-repeated claim is "touch locks up I2C". That was DERIVED FROM THE
//! SCHEMATIC AND NEVER TESTED, and it is wrong. The real rule is narrower:
//! **driving GPIO18 breaks the FT6336 touch controller** — the I2C bus itself is
//! fine. ember's ESPHome config records BOTH failed attempts (`reset_pin: GPIO18`,
//! and a plain GPIO output held high at boot), each producing "touch driver failed
//! to start". Left unconfigured, the FT6336 pulls RSTN high internally and reports
//! chip id 100.
//!
//! The schematic shows no pull on that net, so in *theory* it floats and ought to
//! be driven. That reasoning is correct and the hardware disagrees. **Tested beats
//! derived.** So: **the absence of GPIO18 from this file is the trick, not an
//! oversight.** "Fixing" the floating reset line will break touch, and it will
//! look like a bus problem. (If it is ever attempted properly, it needs a TIMED
//! LOW PULSE BEFORE i2c setup — not a static level.)
//!
//! **GPIO33–GPIO37 — consumed by the octal PSRAM.**
//! The N16R8's 8 MB OPI PSRAM uses all five. Configuring any of them as a GPIO
//! corrupts PSRAM access, and the failure is a memory fault far from the cause.

#![no_std]
#![no_main]

#[cfg(feature = "radio")]
mod espnow_probe;
#[cfg(feature = "wifi")]
mod mqtt;
#[cfg(feature = "wifi")]
mod net;
/// smoltcp phy shim. Same filename, same meaning as smol main's
/// `net/radio_dev.rs` and cyd-c5's `radio_dev.rs` — do not repurpose it.
#[cfg(feature = "wifi")]
mod radio_dev;
#[cfg(feature = "touch")]
mod touch;

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    main,
    psram::{Psram, PsramConfig, PsramMode, PsramSize},
    spi::{
        master::{Config as SpiConfig, Spi},
        Mode,
    },
    time::Rate,
};
use esp_println::println;
use mipidsi::{
    interface::SpiInterface,
    models::ILI9341Rgb565,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
// NOTE: `mipidsi::NoResetPin` is NOT imported, and that is correct rather than an
// omission. It is the type the builder's RST parameter INFERS when `.reset_pin()`
// is never called — which is exactly this board's situation (RST is bonded to
// CHIP_PU/EN, so there is no reset GPIO to hand it). Importing it produces an
// `unused_imports` warning, and the natural "fix" for that warning is to delete
// the line rather than to add a `.reset_pin()` call — so it is named here in a
// comment instead, where it documents the board fact without tripping the lint.
use static_cell::StaticCell;

// REQUIRED. espflash 4.5 REFUSES to write an image with no app descriptor. This
// does not fail the build — it fails the flash, after the fat-LTO link you just
// waited for.
esp_bootloader_esp_idf::esp_app_desc!();

/// This board's smol fleet node id. The new blank ES3C28P, **not** 161.
pub const NODE_ID: u16 = 162;

/// mipidsi's SPI scratch buffer. A `StaticCell` rather than a `static mut` so
/// there is no `unsafe` and no aliasing question — it is handed out exactly once.
static DISPLAY_BUF: StaticCell<[u8; 512]> = StaticCell::new();

/// # MADCTL ground truth — ES3C28P / ILI9341V, landscape
///
/// **Load-bearing for every screen this board will ever draw. Derive from here;
/// do not re-guess by iteration.**
///
/// mipidsi builds MADCTL (`0x36`) from `Orientation` via
/// `MemoryMapping::from_orientation`: `reverse_rows -> MY(0x80)`,
/// `reverse_columns -> MX(0x40)`, `swap_rows_and_columns -> MV(0x20)`,
/// `ColorOrder::Bgr -> 0x08`, with `Deg90 => (rev_rows=false, rev_cols=true)` and
/// `reverse_columns ^= mirrored`.
///
/// The two LEGAL landscape values are therefore:
/// ```text
///   .rotate(Deg90).flip_vertical()    -> MV|BGR       = 0x28   <-- THIS ONE
///   .rotate(Deg90).flip_horizontal()  -> MY|MX|MV|BGR = 0xE8   (same, rotated 180)
/// ```
/// These are exactly the canonical ILI9341 landscape pair (Adafruit rotation 1/3).
///
/// ## ⚠️ The 0x68 trap
///
/// emberboy's retro-go target writes `ILI9341_CMD(0x36, 0x68)` and comments it
/// `(MX|MV|BGR) = landscape`. **`0x68` is `0x28` WITH MX SET — a horizontal
/// mirror.** retro-go compensates for it in its own framebuffer scan; a normal
/// renderer must not. Taking that value at face value is what shipped
/// mirror-writing text in burrito-fw v0.1 (2026-08-15, fixed same day).
///
/// The orientation ground truth is ember's ESPHome config, which drives this exact
/// panel in native portrait with **no `transform:` / `mirror_x` / `mirror_y` at
/// all** — a panel needing zero mirror correction at Deg0 obeys the standard
/// rotation table, so landscape is `0x28`, not `0x28|MX`.
///
/// ## The escape hatch
///
/// If the screen is ever **upside down but readable**, the fix is
/// `.flip_horizontal()` (`0xE8`) — **never** re-introduce a mirror to correct a
/// rotation. Note that the correct value carries `mirrored: true`
/// (`flip_vertical()` on a vertical rotation toggles `mirrored` to *cancel*
/// Deg90's column reversal), so the fix READS LIKE THE BUG. Anyone who greps
/// `mirror` after a future "screen is mirrored" report, or tidies away the
/// "redundant" `.flip_vertical()`, lands straight back on `0x68`.
const ORIENTATION: Orientation = Orientation::new()
    .rotate(Rotation::Deg90)
    .flip_vertical();

/// Heartbeat period. Slow enough to read on a serial console, fast enough that a
/// wedged board is obvious within a couple of seconds.
const HEARTBEAT_MS: u32 = 1000;

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let mut delay = Delay::new();

    println!();
    println!("[s3-cyd] === M1 bring-up spike — ES3C28P, smol node id {} ===", NODE_ID);

    // ---- PSRAM ------------------------------------------------------------
    // M1 does not USE PSRAM. Initialising it here proves octal mapping works
    // before anything depends on it — the terminal/framebuffer work in M4 does.
    //
    // ⚠️ `mode` is pinned to `OctalSpi`, NOT left on `PsramMode::Auto`. The N16R8
    // is the documented autodetect failure case: autodetect can settle on QUAD,
    // which maps a plausible-looking smaller region that then misbehaves under
    // load. Never leave this on Auto for this module.
    //
    // ⚠️ This is a RUNTIME field. esp-hal 1.1.x has no `psram` cargo feature and
    // no `ESP_HAL_CONFIG_PSRAM_MODE` env knob — both were re-verified wrong on
    // 2026-08-14. See .cargo/config.toml.
    //
    // ⚠️ Requires a RELEASE build. In debug the mapping silently comes back 0.
    let psram = Psram::new(
        peripherals.PSRAM,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            size: PsramSize::AutoDetect,
            ..Default::default()
        },
    );
    let (psram_start, psram_size) = psram.raw_parts();
    if psram_size == 0 {
        println!("[s3-cyd] PSRAM: ❌ INIT FAILED (octal) — 0 bytes mapped.");
        println!("[s3-cyd]        Check you built --release; debug builds map nothing.");
    } else {
        println!(
            "[s3-cyd] PSRAM: octal ok — {} bytes ({} MiB) mapped at {:p}",
            psram_size,
            psram_size / (1024 * 1024),
            psram_start
        );
        // ⚠️ ASSERT THE SIZE, NEVER THE ADDRESS. The mapping base is
        // IMAGE-dependent, not a board constant — flash-mapped segments shift
        // it. burrito-fw sees 0x3c020000 on this same board class; this spike's
        // M1 measured 0x3c060000. An "expected address" here would have turned a
        // correct PSRAM init into a false alarm on the first flash of every new
        // image. (Corrected 2026-08-24 after the M1 flash disproved it; the
        // original line named 0x3c020000 as if it were a property of the board.)
        println!("[s3-cyd]        (expect SIZE 8388608 on an N16R8; base address varies by image)");
    }

    // ---- 📌 M4 IMPLEMENTER: what this 8 MiB can actually do ----------------
    //
    // **The S3 CAN DMA directly to and from PSRAM.** esp-hal sets the
    // `dma_can_access_psram` cfg for this chip and exposes `ExternalBurstConfig`
    // / `DmaExtMemBKSize` to configure the external-memory burst size. Verified by
    // construction, not by reading a table: a reference to
    // `esp_hal::dma::ExternalBurstConfig` compiles for `esp32s3` in this crate.
    //
    // ⚠️ DO NOT COPY THE C5's RULE HERE. The cyd-c5 spike requires DMA staging
    // buffers to live in internal SRAM, and that is a **C5 hardware limit**, not a
    // house style. Carrying it over would silently forfeit the single biggest
    // advantage this board has over the rest of the fleet — 8 MiB of framebuffer.
    //
    // ⚠️ AND DO NOT READ burrito-fw's INTERNAL-SRAM ROW BANDS AS THE SAME LIMIT.
    // They are a MEASURED CHOICE, not an impossibility: DMA out of PSRAM carries a
    // 32-byte alignment requirement, and burrito-fw judged that keeping the band
    // buffers internal was cheaper than aligning tile geometry to 32 B. That is a
    // trade-off with numbers behind it, and the numbers may well come out the
    // other way for a full-screen framebuffer, which is what M4 wants.
    //
    // So: PSRAM framebuffer is on the table for M4. Honour the 32-byte alignment
    // (`ExternalBurstConfig`), and measure rather than inherit either precedent.
    //
    // (The heap is a different question with a different answer — the esp-radio
    // heap must stay in INTERNAL RAM because S3 atomics misbehave in PSRAM. See
    // net.rs. DMA and atomics are unrelated hazards; do not merge the two rules.)

    // ---- display ----------------------------------------------------------
    // Backlight starts LOW and stays low until the first full paint has landed,
    // so the hearth appears lit rather than fading up out of vendor-firmware
    // garbage. Active-HIGH on this board (BSS138 gate) — no `inverted` anywhere.
    //
    // GPIO45 is also the VDD_SPI strapping pin. That is SAFE here: the schematic
    // has R32 = 10K gate-to-GND on the BSS138, which hard-wires the strap LOW in
    // hardware. The strap latches at reset while GPIO drivers are still inputs,
    // so R32 always wins and no firmware setting can cause the 1.8 V brownout.
    let mut backlight = Output::new(peripherals.GPIO45, Level::Low, OutputConfig::default());

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(40))
            .with_mode(Mode::_0),
    )
    .expect("spi2 config")
    .with_sck(peripherals.GPIO12)
    .with_mosi(peripherals.GPIO11);
    // MISO (GPIO13) is deliberately not wired: the panel is write-only.

    let cs = Output::new(peripherals.GPIO10, Level::High, OutputConfig::default());
    let dc = Output::new(peripherals.GPIO46, Level::Low, OutputConfig::default());

    let spi_dev = ExclusiveDevice::new(spi, cs, delay).expect("spi device");
    let di = SpiInterface::new(spi_dev, dc, DISPLAY_BUF.init([0u8; 512]));

    let mut display = Builder::new(ILI9341Rgb565, di)
        .display_size(240, 320) // NATIVE portrait; ORIENTATION rotates it to 320x240.
        // No .reset_pin(): RST is bonded to CHIP_PU/EN on this board, so there is
        // no reset GPIO to give it. mipidsi falls back to the software reset
        // inside init(), which is correct here and is why NoResetPin is the type.
        .orientation(ORIENTATION)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted) // panel requires INVON (raw 0x21)
        .init(&mut delay)
        .expect("ili9341 init");

    println!("[s3-cyd] ILI9341V up: 320x240 landscape, MADCTL 0x28, inverted, NoResetPin");

    // ---- M1 colour test ---------------------------------------------------
    // Four full-width bars + a border, drawn as FILLED RECTANGLES so every write
    // goes through mipidsi's `fill_solid` (one windowed contiguous write per
    // rect). Do NOT paint this with per-pixel `draw_iter`: that costs roughly one
    // SPI command PER PIXEL and is measurably ~2x slower than a full repaint.
    //
    // What to look for on the glass:
    //   * bars in the order red / green / blue / white, TOP to BOTTOM
    //   * the thin border touching all four edges (proves the full 320x240 window)
    //   * text-free, so a mirror is NOT detectable from this screen alone —
    //     it needs glyphs, which no tier draws yet. JP's eyeball on the bars +
    //     border is the M1 orientation check.
    const W: i32 = 320;
    const H: i32 = 240;
    let bars = [
        (Rgb565::RED, 0),
        (Rgb565::GREEN, 1),
        (Rgb565::BLUE, 2),
        (Rgb565::WHITE, 3),
    ];
    for (colour, slot) in bars {
        Rectangle::new(Point::new(0, slot * (H / 4)), Size::new(W as u32, (H / 4) as u32))
            .into_styled(PrimitiveStyle::with_fill(colour))
            .draw(&mut display)
            .expect("bar fill");
    }
    Rectangle::new(Point::new(0, 0), Size::new(W as u32, H as u32))
        .into_styled(PrimitiveStyle::with_stroke(Rgb565::CSS_MAGENTA, 3))
        .draw(&mut display)
        .expect("border");

    // ---- the corner marker (feature `touch`) -------------------------------
    // A dot at LOGICAL TOP-LEFT, i.e. display coordinate (0,0) after ORIENTATION.
    //
    // This is the orientation eyeball's **frame-free anchor**, and the reason it
    // beats "is red at the top?": with the dot on the glass and a finger on the
    // dot, ONE GLANCE answers both open questions at once — where the display
    // thinks (0,0) is, and where the touch transform thinks the finger is. No
    // reference frame has to be agreed in advance, and no one has to remember
    // which edge the ribbon cable is on.
    //
    // Deliberately NOT centred and NOT symmetric: a marker that looks the same
    // under a mirror or a 180° rotation cannot distinguish the cases it exists to
    // distinguish. Top-left is the one corner every rotation moves.
    #[cfg(feature = "touch")]
    {
        const DOT: u32 = 16;
        Rectangle::new(Point::new(4, 4), Size::new(DOT, DOT))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::CSS_ORANGE))
            .draw(&mut display)
            .expect("corner marker");
        println!(
            "[s3-cyd] corner marker: {}x{} ORANGE dot at display (4,4) = LOGICAL TOP-LEFT",
            DOT, DOT
        );
    }

    // Backlight ON — only now, after a full frame is on the panel.
    backlight.set_high();
    println!("[s3-cyd] backlight on — colour test painted (R/G/B/W bars + magenta border)");

    // ---- button ------------------------------------------------------------
    // GPIO0 = BOOT, ACTIVE LOW, and it is the ENTIRE hardware input budget on this
    // board. It is also a strapping pin — reading it is fine, driving it is not.
    // Pull-up so an unpressed button reads HIGH.
    let button = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );

    // ---- touch probe (feature `touch`) -------------------------------------
    // Touch OWNS I2C0 in this build — there is no codec here, so landmine L6's
    // codec-first ordering has nothing to order. See src/touch.rs; do not carry
    // this shape into phase 2, where the codec exists and L6 governs again.
    #[cfg(feature = "touch")]
    let mut touch = touch::init(peripherals.I2C0, peripherals.GPIO16, peripherals.GPIO15);

    // ---- M2 network / M3 radio probe (feature-gated) ------------------------
    // Both off by default. ONE radio bring-up serves both tiers — `net::init`
    // owns it, and `radio` stacks on `wifi` so the ordering is not optional.
    #[cfg(feature = "wifi")]
    let mut net = net::init(peripherals.TIMG0, peripherals.SW_INTERRUPT, peripherals.WIFI);
    #[cfg(feature = "radio")]
    let mut radio = net.take_esp_now().map(espnow_probe::attach);

    // ---- heartbeat ----------------------------------------------------------
    println!("[s3-cyd] M1 complete — entering heartbeat loop. Press BOOT (GPIO0) to test input.");
    let mut tick: u32 = 0;
    let mut last_pressed = false;
    loop {
        // Polled here rather than on an interrupt: GPIO17 (CTP_INT) exists, but
        // for a four-tap procedure the loop rate is plenty and an ISR is state
        // this probe does not need. (burrito-fw reached the same conclusion at
        // 4 Hz for scrolling.)
        #[cfg(feature = "touch")]
        if let Some(t) = touch.as_mut() {
            t.poll();
        }

        // Active LOW: is_low() == pressed.
        let pressed = button.is_low();
        if pressed != last_pressed {
            println!("[s3-cyd] BOOT button {}", if pressed { "PRESSED" } else { "released" });
            last_pressed = pressed;
        }

        // The network stack is serviced FOR the whole heartbeat window rather
        // than the loop sleeping through it — see net.rs. Sleeping here is what
        // starved the RX path and turned an undersized heap into M2's panic.
        #[cfg(feature = "wifi")]
        net.tick(&delay, HEARTBEAT_MS as u64);
        #[cfg(feature = "radio")]
        if let Some(r) = radio.as_mut() {
            r.tick(tick);
        }
        // ---- the heartbeat line -------------------------------------------
        //
        // Every segment reports MEASURED state. That rule was bought at a bench
        // window: this line used to assert "channel pinned" while the pin had
        // failed, and it argued against the truth on the console for sixty
        // seconds. A status string that claims a state it never checked is worse
        // than none — it does not merely fail to help, it misleads.
        //
        // Segments, each independently falsifiable:
        //   link  — can it talk?           (net::Link)
        //   mqtt  — is anyone listening?   (mqtt::Mqtt)
        //   radio — are frames leaving?    (espnow_probe measured pin + tx streak)
        #[cfg(feature = "radio")]
        let radio_part: &str = radio.as_ref().map(|r| r.label()).unwrap_or("radio: absent");
        #[cfg(not(feature = "radio"))]
        let radio_part: &str = "";

        #[cfg(feature = "wifi")]
        {
            let link = net.state().label();
            let mqtt = net.mqtt_state().map(|m| m.label()).unwrap_or("");
            print_heartbeat(tick, link, mqtt, radio_part);
        }
        #[cfg(not(feature = "wifi"))]
        print_heartbeat(tick, "", "", radio_part);
        tick = tick.wrapping_add(1);
        // Only the radio-less build sleeps here; with `wifi` the window was
        // already spent inside net.tick() polling the stack.
        #[cfg(not(feature = "wifi"))]
        delay.delay_millis(HEARTBEAT_MS);
    }
}


/// Print one heartbeat line, eliding segments this build does not have.
///
/// Centralised so the separator logic exists once. An empty segment is a build
/// that lacks the feature — NOT a feature that is silent, which is why absent
/// segments vanish rather than printing an empty status that would read as a
/// measurement.
fn print_heartbeat(tick: u32, link: &str, mqtt: &str, radio: &str) {
    let mut line = HeartLine::new();
    let _ = core::fmt::Write::write_fmt(
        &mut line,
        format_args!("[s3-cyd] heartbeat {} — node {} alive", tick, NODE_ID),
    );
    for seg in [link, mqtt, radio] {
        if !seg.is_empty() {
            let _ = core::fmt::Write::write_fmt(&mut line, format_args!(" — {}", seg));
        }
    }
    println!("{}", line.as_str());
}

/// Fixed 160-byte formatter for the heartbeat. Truncates rather than failing:
/// a clipped status line is worth more than a panic in the liveness signal.
struct HeartLine {
    buf: [u8; 160],
    len: usize,
}

impl HeartLine {
    fn new() -> Self {
        Self {
            buf: [0; 160],
            len: 0,
        }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("[s3-cyd] heartbeat <unprintable>")
    }
}

impl core::fmt::Write for HeartLine {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}
