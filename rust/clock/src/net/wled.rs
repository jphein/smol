//! #25 WLED WiZmote-emit — smol as a WLED "linked remote" over ESP-NOW.
//!
//! smol impersonates a WLED WiZmote remote: it broadcasts the 13-byte WiZmote
//! ESP-NOW frame so a WLED controller (with smol's MAC set as its "linked remote")
//! reacts — on / off / preset / dim / nightlight. Feature-gated `wled` (= espnow);
//! NONE of this exists in the default/wifi/espnow builds (symbol-absence provable).
//!
//! ## Input model (adaptation of the #25 spec's D-pad+A grid)
//! The #25 spec assumed a D-pad (select) + A-button (emit). smol has ONE BOOT button
//! (short-tap / long-press only, and long-press is the universal Menu escape). So the
//! grid maps to a coherent ONE-button model: **short-tap = advance the highlighted
//! action**; **dwell `DWELL_MS` after you stop tapping = emit the highlighted button**
//! (low-latency ESP-NOW broadcast, NOT a WiFi burst); **long-press = Menu** (exit). No
//! gesture collision. (Flagged to the orchestrator as a UI decision luna may redesign;
//! only this file's `on_button`/`update` change if so.)
//!
//! ## Pairing (one-time, WLED 0.14+)
//! Config → Sync Interfaces → enable "ESP-NOW", then set "Linked Remote" MAC = smol's
//! ESP-NOW/STA MAC (shown on the About screen). WLED then acts only on frames from that
//! MAC → no cross-talk with other smol boards. WLED must be co-channel with the smol
//! mesh (one of 1/6/11); a per-emit channel hop is NOT done (it would make the mesh
//! deaf for the hop — the very thing #23 retired).
#![cfg(feature = "wled")]

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_9X15, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// A WiZmote button. `Preset(n)` is clamped to 1..=4 at encode.
#[derive(Clone, Copy)]
pub enum WledButton {
    On,
    Off,
    Night,
    BrightUp,
    BrightDown,
    Preset(u8),
}

impl WledButton {
    /// `(program_byte, button_code)` per WLED `remote.cpp` `WizMoteMessageStructure`.
    /// `program` is `0x91` for ON, `0x81` for everything else; button codes:
    /// `ON=1, OFF=2, NIGHT=3, BRIGHT_DOWN=8, BRIGHT_UP=9`, presets = `15 + n` (1→16..4→19).
    const fn codes(self) -> (u8, u8) {
        match self {
            WledButton::On => (0x91, 1),
            WledButton::Off => (0x81, 2),
            WledButton::Night => (0x81, 3),
            WledButton::BrightDown => (0x81, 8),
            WledButton::BrightUp => (0x81, 9),
            // clamp 1..=4 then +15 → 16..=19; saturating so no overflow, no panic.
            WledButton::Preset(n) => (
                0x81,
                15u8.saturating_add(if n < 1 {
                    1
                } else if n > 4 {
                    4
                } else {
                    n
                }),
            ),
        }
    }
}

/// Encode the 13-byte WiZmote frame. Fixed array literal — no alloc, no runtime
/// indexing on external data → total/panic-free. Layout:
/// `program | seq[4] LE | dt1=0x20 | button | dt2=0x01 | batLevel | 0 0 0 0`.
pub fn encode_wizmote(btn: WledButton, seq: u32, bat_level: u8) -> [u8; 13] {
    let (program, button) = btn.codes();
    let s = seq.to_le_bytes(); // LSB-first
    [
        program,
        s[0],
        s[1],
        s[2],
        s[3],
        0x20,
        button,
        0x01,
        bat_level.min(100),
        0,
        0,
        0,
        0,
    ]
}

/// The one-button remote's action ring (the spec's grid, plus Night). Tap cycles
/// through it; dwell emits the highlighted one. `(button, short label)`.
const ACTIONS: [(WledButton, &str); 9] = [
    (WledButton::Off, "Off"),
    (WledButton::On, "On"),
    (WledButton::Preset(1), "P1"),
    (WledButton::Preset(2), "P2"),
    (WledButton::Preset(3), "P3"),
    (WledButton::Preset(4), "P4"),
    (WledButton::BrightDown, "Dim-"),
    (WledButton::BrightUp, "Dim+"),
    (WledButton::Night, "Nite"),
];

/// Emit the highlighted action this long after the last tap (tapping stopped).
const DWELL_MS: u64 = 1200;

/// The WLED remote screen state (the `Plugin` is implemented on it). `sel` is always
/// `< ACTIONS.len()` (kept by the `% len` in `on_button`), so `ACTIONS[sel]` never
/// panics. `bat_level` for the frame is a constant 100 (cosmetic — WLED shows the
/// remote's battery; smol's true SOC extraction is a later nicety).
pub struct WledRemoteState {
    /// Highlighted action index (0..ACTIONS.len()).
    sel: usize,
    /// A dwell-emit is pending (a tap happened, waiting for tapping to stop).
    armed: bool,
    /// `now_ms` of the last tap (the dwell timer's anchor).
    last_tap_ms: u64,
    /// Count of frames emitted this session (on-screen feedback + WiZmote-seq sanity).
    sent: u32,
    /// Render dedup: only repaint when the visible state changes (or forced).
    last_render: Option<(usize, bool, u32)>,
}

impl WledRemoteState {
    pub fn new(_now_ms: u64) -> Self {
        Self {
            sel: 0,
            armed: false,
            last_tap_ms: 0,
            sent: 0,
            last_render: None,
        }
    }
}

impl Plugin for WledRemoteState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu (the universal escape).
            Press::Long => Transition::Switch(AppKind::Menu),
            // Short tap = advance the highlighted action + (re)arm the dwell-emit.
            Press::Short => {
                self.sel = (self.sel + 1) % ACTIONS.len();
                self.last_tap_ms = ctx.now_ms;
                self.armed = true;
                ctx.redraw = true;
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Dwell-emit: once tapping has stopped for DWELL_MS, fire the highlighted
        // button as an ESP-NOW WiZmote broadcast (immediate, low-latency — NOT a WiFi
        // flush). No-op if radio bring-up failed (`ctx.radio` = None). bat_level = 100.
        if self.armed && ctx.now_ms.saturating_sub(self.last_tap_ms) >= DWELL_MS {
            self.armed = false;
            let (btn, label) = ACTIONS[self.sel];
            if let Some(r) = ctx.radio.as_mut() {
                r.broadcast_wled_button(btn, 100);
            }
            self.sent = self.sent.wrapping_add(1);
            ctx.redraw = true;
            log::info!("smol #25: WLED '{}' emitted (#{})", label, self.sent);
        }

        // Repaint only on a visible-state change (or a forced redraw on entry).
        let key = (self.sel, self.armed, self.sent);
        if !(ctx.redraw || self.last_render != Some(key)) {
            return;
        }
        self.last_render = Some(key);

        let small = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::On)
            .build();
        let big = MonoTextStyleBuilder::new()
            .font(&FONT_9X15)
            .text_color(BinaryColor::On)
            .build();

        ctx.display.clear(BinaryColor::Off).ok();

        // Title + session emit count.
        let mut top = Line::new();
        let _ = write!(top, "WLED  #{}", self.sent);
        Text::with_baseline(top.as_str(), Point::new(2, 0), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        // The highlighted action, BIG + bracketed so the selection reads clearly.
        let mut mid = Line::new();
        let _ = write!(mid, "[{}]", ACTIONS[self.sel].1);
        Text::with_baseline(mid.as_str(), Point::new(2, 12), big, Baseline::Top)
            .draw(ctx.display)
            .ok();

        // Bottom hint: while armed, tell the user a send is imminent; else the grammar.
        let hint = if self.armed { "sending..." } else { "tap:next  hold:menu" };
        Text::with_baseline(hint, Point::new(2, 31), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        ctx.display.flush().ok();
    }
}

use core::fmt::Write;

/// Tiny heap-free line builder (mirrors `about.rs`).
struct Line {
    buf: [u8; 24],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self { buf: [0; 24], len: 0 }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}
