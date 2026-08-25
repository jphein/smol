//! Touch probe — FT6336U on I²C0. Feature `touch`.
//!
//! **This exists to settle ONE placeholder**, and then to be deleted.
//! `board-staging/board_es3c28p.rs` carries `TOUCH_SWAP_XY` / `TOUCH_INVERT_X` /
//! `TOUCH_INVERT_Y` as **PLACEHOLDER-grade** constants, transcribed from
//! retro-go's table and never confirmed against a real finger under Rust. The
//! procedure named there is: *"paint a known corner marker, tap each of the four
//! corners, log raw + transformed coordinates. Ten seconds, once."*
//!
//! This module is the "log raw + transformed" half; `main.rs`'s corner marker is
//! the other half. Together they make `BENCH-RUNBOOK.md` §5.4's four-corner tap
//! **runnable** rather than skippable.
//!
//! # Why a frame-free readout
//!
//! The output prints the raw controller coordinates AND the mapped ones, on one
//! line, **labelled as placeholder-grade in the line itself**. That matters: an
//! operator reading `mapped=(300,12)` has no way to know whether the mapping is
//! trusted unless the line says so. A number that looks authoritative and is not
//! is worse than a number with an honest label — and the label travels with the
//! evidence into whatever log the bench session keeps.
//!
//! Being capacitive there is **no calibration span to measure** (unlike the C5's
//! resistive XPT2046). The transform is either right or visibly wrong, which is
//! why four taps and a dot settle it in ten seconds.
//!
//! ---------------------------------------------------------------------------
//! ⛔ GPIO18 (CTP_RST) IS NEVER CONFIGURED — landmine L1
//! ---------------------------------------------------------------------------
//! **This module does not mention GPIO18, and that is the trick, not an
//! oversight.** The widely-repeated claim is "touch locks up I²C"; that was
//! derived from the schematic and never tested, and it is wrong. The real rule is
//! narrower: **driving GPIO18 breaks the FT6336** — the bus itself is fine.
//! ember's ESPHome config records BOTH failed attempts (`reset_pin: GPIO18`, and
//! a plain output held high at boot), each producing *"touch driver failed to
//! start"*. Left unconfigured, the FT6336 pulls RSTN high internally and reports
//! chip id 100.
//!
//! The schematic shows no pull on that net, so in theory it floats and ought to
//! be driven. That reasoning is correct and the hardware disagrees. **Tested beats
//! derived.** A future reader "completing" the driver by adding the reset line
//! will break working touch and will have every reason to think they fixed
//! something.
//!
//! ---------------------------------------------------------------------------
//! ⚠️ I²C INIT ORDER — L6 does NOT apply to this build, and here is why
//! ---------------------------------------------------------------------------
//! Landmine L6 says **codec first, touch second**: the ES8311 (`0x18`) and the
//! FT6336 (`0x38`) share I²C0; the codec needs the bus once at boot and never
//! again, touch needs it forever, so initialising the codec first means nothing
//! has to be shared — no `RefCellDevice`, no bus manager.
//!
//! **This feature has no codec.** Touch owns I²C0 outright, so there is no
//! ordering to get wrong here. **Do not cargo-cult this file's structure into
//! phase 2**, where the codec does exist and L6 governs again: there, the codec
//! is initialised *before* this driver takes the bus, and reversing them turns the
//! cheapest fix on the table into a refactor of the busiest driver on the board.

use esp_hal::{
    i2c::master::{Config as I2cConfig, I2c},
    peripherals,
    time::Rate,
};
use esp_println::println;

// ------------------------------------------------------------------ device ---

/// FT6336U I²C address.
const ADDR: u8 = 0x38;

/// `0xA3` chip id. This panel reports **100 (`0x64`)** — the same value ESPHome
/// sees, which is what makes it a usable handshake rather than a guess.
const REG_CHIP_ID: u8 = 0xA3;
/// `0x02`, low nibble = number of active touch points.
const REG_TD_STATUS: u8 = 0x02;
/// `0x03..0x06` = P1 XH, XL, YH, YL.
const REG_P1: u8 = 0x03;

/// Native panel geometry, PORTRAIT — the frame the controller reports in.
const RAW_W: u16 = 240;
const RAW_H: u16 = 320;

/// Logical geometry, LANDSCAPE — the frame the display draws in.
pub const SCREEN_W: i32 = 320;
pub const SCREEN_H: i32 = 240;

/// Verbose sample budget. burrito-fw's convention: the first few taps print in
/// full, then the log quiets down so a resting finger cannot flood the console.
/// Four is deliberate — it is exactly the number of corners the procedure taps.
const VERBOSE_SAMPLES: u8 = 4;

// --------------------------------------------------------------- transform ---

/// The placeholder transform under test, from `board-staging/board_es3c28p.rs`:
/// `SWAP_XY = true`, `INVERT_X = false`, `INVERT_Y = true`.
///
/// Written out longhand rather than as three bools folded into a loop, because
/// **the whole point of this build is to find out whether these three lines are
/// right**, and a reader comparing them against the board file should be able to
/// do it by eye.
///
/// ```text
///   swap_xy     : screen_x <- raw_y      screen_y <- raw_x
///   invert_x=0  : screen_x unchanged
///   invert_y=1  : screen_y <- (RAW_W-1) - raw_x
/// ```
///
/// Matches `burrito-fw/src/touch.rs::map_landscape` line for line — deliberately,
/// so a disagreement between the two boards' firmwares would be visible rather
/// than silently absorbed here.
fn map_landscape(raw_x: u16, raw_y: u16) -> (i32, i32) {
    let sx = raw_y.min(RAW_H - 1) as i32;
    let sy = (RAW_W - 1).saturating_sub(raw_x.min(RAW_W - 1)) as i32;
    (sx, sy)
}

// ------------------------------------------------------------------- probe ---

pub struct Probe {
    i2c: I2c<'static, esp_hal::Blocking>,
    /// Verbose lines emitted so far.
    logged: u8,
    /// Consecutive I²C failures, for rate-limiting the error line.
    err_streak: u32,
    /// Successful reads since boot — the denominator that makes "0 touches" and
    /// "a dead bus" distinguishable.
    reads_ok: u32,
    /// Was a finger down on the previous poll? Edge-detect so one tap prints one
    /// line instead of one per poll.
    was_down: bool,
}

/// Bring up I²C0 and handshake with the controller.
///
/// Returns `None` if the bus will not configure — logged, never panicked: a board
/// with no touch is a worse board, a board that will not boot is a broken one.
pub fn init(
    i2c0: peripherals::I2C0<'static>,
    sda: peripherals::GPIO16<'static>,
    scl: peripherals::GPIO15<'static>,
) -> Option<Probe> {
    // 100 kHz, shared-bus speed from the board file. GPIO18 appears nowhere.
    let i2c = match I2c::new(i2c0, I2cConfig::default().with_frequency(Rate::from_khz(100))) {
        Ok(i2c) => i2c.with_sda(sda).with_scl(scl),
        Err(e) => {
            println!("[touch] I2C0 config failed: {:?} — no touch this boot", e);
            return None;
        }
    };

    let mut probe = Probe {
        i2c,
        logged: 0,
        err_streak: 0,
        reads_ok: 0,
        was_down: false,
    };

    // ---- chip-id handshake -------------------------------------------------
    // A failure here is reported and then IGNORED. The probe still polls: a
    // controller that will not answer `0xA3` but does answer `0x02` is a real and
    // informative state, and refusing to look would hide it.
    let mut id = [0u8; 1];
    match probe.i2c.write_read(ADDR, &[REG_CHIP_ID], &mut id) {
        Ok(()) if id[0] == 0x64 => println!(
            "[touch] FT6336 at 0x{:02X}, chip id {} (0x{:02X}) — GPIO18 left floating on purpose",
            ADDR, id[0], id[0]
        ),
        Ok(()) => println!(
            "[touch] ⚠️ FT6336 at 0x{:02X} answered chip id {} (0x{:02X}), expected 100 (0x64)",
            ADDR, id[0], id[0]
        ),
        Err(e) => println!(
            "[touch] ⚠️ no answer from 0x{:02X}: {:?} — polling anyway, see if 0x02 responds",
            ADDR, e
        ),
    }

    println!("[touch] probe ready — tap the four corners; first {VERBOSE_SAMPLES} taps print in full");
    println!("[touch] the ORANGE DOT marks logical TOP-LEFT (0,0). A tap on it should read mapped=(~0,~0)");
    Some(probe)
}

impl Probe {
    /// Poll once. Call from the heartbeat loop.
    ///
    /// Prints one line per finger-DOWN edge, not per poll — a resting finger is
    /// one event, and four corner taps should produce exactly four lines.
    pub fn poll(&mut self) {
        let mut n = [0u8; 1];
        if let Err(e) = self.i2c.write_read(ADDR, &[REG_TD_STATUS], &mut n) {
            // A wedged bus must not look like a finger that never touched the
            // glass — that ambiguity cost burrito-fw a hardware window. Rate
            // limited so a permanently dead bus cannot flood the console.
            if self.err_streak == 0 || self.err_streak.is_multiple_of(64) {
                println!(
                    "[touch] I2C read FAILED: {:?} (streak {}, {} good reads since boot)",
                    e, self.err_streak, self.reads_ok
                );
            }
            self.err_streak = self.err_streak.saturating_add(1);
            return;
        }
        if self.err_streak > 0 {
            println!("[touch] I2C recovered after {} failures", self.err_streak);
            self.err_streak = 0;
        }
        self.reads_ok = self.reads_ok.saturating_add(1);

        let down = (n[0] & 0x0F) != 0;
        if !down {
            self.was_down = false;
            return;
        }
        if self.was_down {
            return; // still the same press
        }
        self.was_down = true;

        let mut p = [0u8; 4];
        if self.i2c.write_read(ADDR, &[REG_P1], &mut p).is_err() {
            println!("[touch] point read failed after td-status said {} point(s)", n[0] & 0x0F);
            return;
        }

        // XH bits 3..0 are the coordinate's high nibble; bits 7..6 are the event
        // flag and MUST be masked off — leaving them in silently adds 0x40/0x80
        // to x on press/release, which reads as a wildly miscalibrated panel.
        let raw_x = (((p[0] & 0x0F) as u16) << 8) | p[1] as u16;
        let raw_y = (((p[2] & 0x0F) as u16) << 8) | p[3] as u16;
        let (mx, my) = map_landscape(raw_x, raw_y);

        // ⚠️ The label is part of the evidence, not decoration. Anyone reading
        // this line in a bench log must be able to tell that `mapped=` is the
        // OUTPUT OF AN UNCONFIRMED TRANSFORM without going and reading the source.
        if self.logged < VERBOSE_SAMPLES {
            self.logged += 1;
            println!(
                "[touch] #{} raw=({},{}) mapped=({},{}) [transform: PLACEHOLDER retro-go swap_xy=1 invert_x=0 invert_y=1]",
                self.logged, raw_x, raw_y, mx, my
            );
            println!(
                "[touch]    corner guess: {} (screen is {}x{} landscape)",
                corner_of(mx, my),
                SCREEN_W,
                SCREEN_H
            );
        } else {
            println!(
                "[touch] raw=({},{}) mapped=({},{}) [PLACEHOLDER transform]",
                raw_x, raw_y, mx, my
            );
        }
    }
}

/// Name the quadrant a mapped point falls in, so the operator can compare "where
/// I tapped" against "what the transform says" without doing arithmetic at the
/// bench. **This reports the transform's OPINION**, not ground truth — if you tap
/// top-left and this says `bottom-left`, the transform is wrong, which is exactly
/// the finding the probe exists to produce.
fn corner_of(x: i32, y: i32) -> &'static str {
    match (x < SCREEN_W / 2, y < SCREEN_H / 2) {
        (true, true) => "TOP-LEFT",
        (false, true) => "TOP-RIGHT",
        (true, false) => "BOTTOM-LEFT",
        (false, false) => "BOTTOM-RIGHT",
    }
}
