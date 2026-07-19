//! Transient on-glass **toast** overlay (#197) — a single short message composited over
//! whatever screen is active, auto-cleared on expiry. Deliberately a GENERAL primitive,
//! not herald-specific: the parked mesh-RPG LitRPG engine needs exactly this ("stat toasts,
//! one line, ~2 s"), so this is its substrate. Replace-on-new (no queue).
//!
//! **Never persisted** — the toast lives only in RAM with an expiry; a reboot never
//! re-shows it (the #197 transient invariant, mirrored by the never-cached CFG-`M` relay).
//!
//! Set from the CFG-`M` apply arm (`net/mode.rs`); drawn from the main render loop after the
//! active plugin has painted (the SSD1306 framebuffer persists, so a re-composite each tick
//! survives a plugin repaint). Word-wrap matches the HA herald composer's greedy behaviour so
//! the same message renders the same from the operator dock and from HA.

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

/// Max stored message bytes (fits the CFG-value cap; `net/wifi.rs` `CFG_VALUE_MAX = 64`).
const TOAST_MAX: usize = 64;
/// 72 px panel / (5 px glyph + 1 px advance) = 12 glyphs per line (FONT_5X8).
const COLS: usize = 12;
/// 40 px panel: 3 lines × ~11 px + padding fits with margin.
const ROWS: usize = 3;
const LINE_H: i32 = 10; // FONT_5X8 is 8 px tall; 10 px baseline pitch leaves a gap
const PANEL_W: i32 = 72;
const PANEL_H: i32 = 40;

/// Default on-glass duration when the wire carries no `~<dur>` prefix.
pub const TOAST_DEFAULT_S: u16 = 5;
/// Clamp a wire-supplied duration so a bad value can't pin a toast on glass forever.
const TOAST_MAX_S: u16 = 60;

/// Parse the notify wire value `[~<dur>]<msg>` → `(dur_seconds, msg_bytes)`. A leading
/// `~<digits>` sets the TTL (clamped to `TOAST_MAX_S`); absent → `TOAST_DEFAULT_S`. Panic-free,
/// bounded (untrusted relayed value — the #46 clamp discipline). The returned slice borrows
/// the input.
pub fn parse_wire(v: &[u8]) -> (u16, &[u8]) {
    if v.first() == Some(&b'~') {
        let mut i = 1;
        let mut dur: u32 = 0;
        while i < v.len() && v[i].is_ascii_digit() {
            dur = dur.saturating_mul(10).saturating_add((v[i] - b'0') as u32);
            i += 1;
        }
        // An optional single separator (space or '|') after the duration is consumed.
        if i < v.len() && (v[i] == b' ' || v[i] == b'|') {
            i += 1;
        }
        let dur = (dur.min(TOAST_MAX_S as u32) as u16).max(1);
        (dur, &v[i..])
    } else {
        (TOAST_DEFAULT_S, v)
    }
}

struct Toast {
    buf: [u8; TOAST_MAX],
    len: usize,
    expiry_ms: u64,
}

static mut ACTIVE: Toast = Toast { buf: [0; TOAST_MAX], len: 0, expiry_ms: 0 };

fn active() -> &'static mut Toast {
    // Single-threaded, single-core (like `OTA_WINDOW_BUF` et al.) — no aliasing.
    unsafe { &mut *core::ptr::addr_of_mut!(ACTIVE) }
}

/// Show `msg` for `dur_ms` from `now_ms` (replace-on-new). `msg` is truncated to
/// `TOAST_MAX`; non-UTF8 is stored verbatim (draw sanitizes). Empty `msg` clears.
pub fn set(msg: &[u8], now_ms: u64, dur_ms: u64) {
    let t = active();
    if msg.is_empty() {
        t.expiry_ms = 0;
        t.len = 0;
        return;
    }
    let n = msg.len().min(TOAST_MAX);
    t.buf[..n].copy_from_slice(&msg[..n]);
    t.len = n;
    t.expiry_ms = now_ms.saturating_add(dur_ms);
}

/// Is a toast currently showing?
pub fn is_active(now_ms: u64) -> bool {
    let t = active();
    t.len > 0 && now_ms < t.expiry_ms
}

/// Greedy word-wrap `s` into up to `ROWS` lines of ≤`COLS` glyphs — the HA composer's
/// behaviour. Returns `(line_byte_ranges, n_lines)`. A single word longer than `COLS` is
/// hard-split. Pure + host-testable (no display).
fn wrap(s: &str) -> ([(usize, usize); ROWS], usize) {
    let mut lines = [(0usize, 0usize); ROWS];
    let mut n = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && n < ROWS {
        // Skip leading spaces on a new line.
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        let mut last_break = 0usize; // byte index of the last space that fits, 0 = none
        let mut col = 0usize;
        while i < bytes.len() && bytes[i] != b'\n' {
            if col == COLS {
                break; // line full
            }
            if bytes[i] == b' ' {
                last_break = i;
            }
            col += 1;
            i += 1;
        }
        // Decide the line end: prefer the last space (word boundary) unless none fits.
        let end = if i < bytes.len() && bytes[i] != b'\n' && last_break > start {
            let e = last_break;
            i = last_break; // resume after the space (skipped next iteration)
            e
        } else {
            if i < bytes.len() && bytes[i] == b'\n' {
                let e = i;
                i += 1; // consume the newline
                e
            } else {
                i // hard split / end of string
            }
        };
        lines[n] = (start, end);
        n += 1;
    }
    (lines, n)
}

/// Composite the active toast over the current framebuffer: a filled box (inverted text,
/// `draw_center_banner` style) centred on the panel. Call AFTER the plugin has drawn, then
/// flush. No-op if inactive.
pub fn draw<D>(display: &mut D, now_ms: u64)
where
    D: DrawTarget<Color = BinaryColor>,
{
    if !is_active(now_ms) {
        return;
    }
    let t = active();
    let s = core::str::from_utf8(&t.buf[..t.len]).unwrap_or("");
    let (lines, n) = wrap(s);
    if n == 0 {
        return;
    }
    // Box sized to the widest line.
    let mut widest = 0usize;
    for &(a, b) in &lines[..n] {
        widest = widest.max(b - a);
    }
    let box_w = ((widest as i32) * 6 + 5).clamp(0, PANEL_W);
    let box_h = (n as i32) * LINE_H + 3;
    let x = (PANEL_W - box_w) / 2;
    let y = (PANEL_H - box_h) / 2;

    Rectangle::new(Point::new(x, y), Size::new(box_w as u32, box_h as u32))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(display)
        .ok();
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::Off)
        .build();
    for (row, &(a, b)) in lines[..n].iter().enumerate() {
        let line = &s[a..b];
        let ly = y + 2 + row as i32 * LINE_H;
        Text::with_baseline(line, Point::new(x + 3, ly), style, Baseline::Top)
            .draw(display)
            .ok();
    }
}

// NOTE: this crate builds for bare-metal riscv32imc (no host test harness), matching the
// convention that no `clock/src` module carries `#[cfg(test)]`. `wrap()` is pure and a
// candidate for an `experiments/` host-test crate (the flood_verify/relay_compat pattern);
// meanwhile the HW-gate canary validates on-glass rendering directly.
