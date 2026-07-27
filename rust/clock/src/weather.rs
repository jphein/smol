//! #227 WEATHER screen — Open-Meteo current conditions on the 72×40 OLED (`cfg(wifi)`).
//!
//! The GATEWAY fetches current weather over plain HTTP during its WiFi flush window
//! (`net/wifi.rs::fetch_weather` — the cast-stream slot, riding the still-live association)
//! and re-broadcasts it as a **SMOLv1 WX2** freshness-flooded frame so every leaf fills its
//! cache too — the exact BATT display-case shape (#16/#13 Stage B), with the source being an
//! HTTP fetch instead of an HA retained topic. This module is transport-agnostic: it renders
//! whatever `WX|<tempF>|<code>` bytes the cache holds + how long ago they arrived.
//!
//! ## Payload format
//! ASCII, ≤ [`crate::net::wx::WX_PAYLOAD_MAX`] bytes: `WX|<tempF>|<code>` — the marker, the
//! signed integer Fahrenheit temperature, and the WMO weather-interpretation code. Stored
//! **verbatim, marker included** (the WX2 frame is a memcpy of the cache, like BATT); parsed
//! only at render time via [`crate::net::wx::parse_wx`]. Extra future fields are ignored by
//! the parser — the additive/#100 discipline.
//!
//! ## Split of responsibility (the BATT pattern, verbatim)
//!   * [`WxCache`] — the payload bytes + a fetch timestamp. Owned by `main`; filled by the
//!     gateway's fetch or an inbound WX2 frame while this screen may be inactive; the plugin
//!     only READS it, borrowed through [`crate::app::Ctx::wx`].
//!   * [`WxState`] — the [`Plugin`]: big temperature (honoring the #43 fleet units — the
//!     payload is always °F on the wire; °C is a render-time conversion) + the WMO condition
//!     label + the fetch age. Single page: Long → Menu, Short → no-op.

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// The payload marker (validated on [`store`](WxCache::store), like BATT's `BATT|`).
// Both fill paths (gateway fetch, WX2 offer) are espnow-only — unlike BATT, whose boot-burst
// downlink also fills it on plain wifi — so a wifi-without-espnow build never stores (the
// screen renders its empty state) and `store`/MARKER read as dead there. cfg_attr, not cfg:
// the code is correct in that tier, it just has no caller.
#[cfg_attr(not(feature = "espnow"), allow(dead_code))]
const MARKER: &[u8] = b"WX|";

/// Max bytes the cache retains — mirrors [`crate::net::wx::WX_PAYLOAD_MAX`].
const CACHE_CAP: usize = crate::net::wx::WX_PAYLOAD_MAX;

/// The weather cache: the verbatim `WX|…` payload + when it last arrived. Owned by `main`,
/// filled by the gateway fetch (espnow) or an inbound WX2 frame, read by [`WxState`].
pub struct WxCache {
    payload: [u8; CACHE_CAP],
    len: usize,
    /// Monotonic-ms stamp of the last successful store (`None` = never — title age `--`).
    fetched_at_ms: Option<u64>,
}

impl WxCache {
    /// An empty cache (never fetched). `const` so `main` seeds it cheaply.
    pub const fn new() -> Self {
        Self {
            payload: [0; CACHE_CAP],
            len: 0,
            fetched_at_ms: None,
        }
    }

    /// Store a `WX|…` payload VERBATIM and stamp the time. Rejects anything not starting
    /// with the marker OR that doesn't parse — a corrupt/foreign frame never wipes a good
    /// reading (the BATT `store` discipline, plus the parse gate since WX is machine-read).
    // espnow-only callers (fetch + WX2 offer) — see the MARKER note.
    #[cfg_attr(not(feature = "espnow"), allow(dead_code))]
    pub fn store(&mut self, payload: &[u8], now_ms: u64) {
        if !payload.starts_with(MARKER) || crate::net::wx::parse_wx(payload).is_none() {
            return;
        }
        let n = payload.len().min(CACHE_CAP);
        self.payload[..n].copy_from_slice(&payload[..n]);
        self.len = n;
        self.fetched_at_ms = Some(now_ms);
    }

    /// The verbatim payload bytes, for the gateway's WX2 broadcast (frame = tag + seq +
    /// `bytes()`). espnow-only: only the espnow build broadcasts.
    #[cfg(feature = "espnow")]
    pub fn bytes(&self) -> &[u8] {
        &self.payload[..self.len]
    }

    /// True until a payload has been cached — gates the gateway's periodic re-broadcast.
    #[cfg(feature = "espnow")]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Milliseconds-stamp of the last store (read by the fetch-cadence gate: the gateway
    /// re-fetches when this is `None` or older than the refresh interval).
    #[cfg(feature = "espnow")]
    pub fn fetched_at(&self) -> Option<u64> {
        self.fetched_at_ms
    }

    /// The parsed reading `(temp_f, wmo_code)`, or `None` if never fetched.
    fn reading(&self) -> Option<(i16, u8)> {
        crate::net::wx::parse_wx(&self.payload[..self.len])
    }
}

/// WEATHER screen state — render-dedup bookkeeping only (data lives in the cache).
/// Repaints once/second (live age), on a forced redraw, and on a fresh reading.
pub struct WxState {
    last_s: Option<u32>,
    last_fetch: Option<u64>,
}

impl WxState {
    /// Fresh state. Single-page screen — no boot-page seed needed.
    pub fn new() -> Self {
        Self {
            last_s: None,
            last_fetch: None,
        }
    }
}

impl Plugin for WxState {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // Single page — a tap is a deliberate no-op (never opens a burst, #46 style).
            Press::Short => Transition::Stay,
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        let sec = (ctx.now_ms / 1000) as u32;
        let fetched = ctx.wx.fetched_at_ms;
        if !(ctx.redraw || self.last_s != Some(sec) || self.last_fetch != fetched) {
            return;
        }
        self.last_s = Some(sec);
        self.last_fetch = fetched;
        let age_s = fetched.map(|f| ctx.now_ms.saturating_sub(f) / 1000);
        render(ctx, age_s);
    }
}

/// Paint the screen: `Weather` + age title row, the BIG temperature centered (°F on the
/// wire; converted to °C at render when the #43 fleet units say so), and the WMO condition
/// label centered on the bottom row.
fn render(ctx: &mut Ctx, age_s: Option<u64>) {
    let big = MonoTextStyleBuilder::new()
        .font(&FONT_10X20)
        .text_color(BinaryColor::On)
        .build();
    let title_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let small = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    ctx.display.clear(BinaryColor::Off).ok();

    Text::with_baseline("Weathr", Point::new(2, 0), title_style, Baseline::Top)
        .draw(ctx.display)
        .ok();
    let mut age = AgeBuf::new();
    match age_s {
        Some(s) => write_age(&mut age, s),
        None => {
            let _ = age.write_str("--");
        }
    }
    Text::with_baseline(age.as_str(), Point::new(48, 1), small, Baseline::Top)
        .draw(ctx.display)
        .ok();

    match ctx.wx.reading() {
        Some((temp_f, code)) => {
            // BIG temperature, unit-converted at render (wire is always °F). `NNNF` ≤ 5
            // glyphs (`-99F`); 10 px advance → always fits + centers on 72 px.
            let (t, unit) = if ctx.units.temp_f {
                (temp_f as i32, 'F')
            } else {
                // Round-half-away-from-zero (f−32)·5⁄9 in integers — no float on the C3.
                let x = (temp_f as i32 - 32) * 5;
                ((x * 2 + if x >= 0 { 9 } else { -9 }) / 18, 'C')
            };
            let mut tb = AgeBuf::new();
            let _ = write!(tb, "{}{}", t.clamp(-999, 999), unit);
            let w = tb.as_str().chars().count() as i32 * 10;
            let x = ((72 - w) / 2).max(1);
            Text::with_baseline(tb.as_str(), Point::new(x, 12), big, Baseline::Top)
                .draw(ctx.display)
                .ok();
            // WMO condition label, centered on the bottom row (≤ 13 chars @ 5 px + 1).
            let label = crate::net::wx::wmo_label(code);
            let lw = label.chars().count() as i32 * 6;
            let lx = ((72 - lw) / 2).max(0);
            Text::with_baseline(label, Point::new(lx, 32), small, Baseline::Top)
                .draw(ctx.display)
                .ok();
        }
        None => {
            Text::with_baseline("no data yet", Point::new(9, 18), small, Baseline::Top)
                .draw(ctx.display)
                .ok();
        }
    }

    ctx.display.flush().ok();
}

/// Compact age (`45s` / `12m` / `3h`) — the batt.rs helper, verbatim.
fn write_age(out: &mut AgeBuf, secs: u64) {
    if secs < 60 {
        let _ = write!(out, "{}s", secs);
    } else if secs < 3600 {
        let _ = write!(out, "{}m", secs / 60);
    } else {
        let _ = write!(out, "{}h", secs / 3600);
    }
}

use core::fmt::Write;

/// Tiny heap-free text buffer (the batt.rs `AgeBuf`, also reused for the temp string).
struct AgeBuf {
    buf: [u8; 8],
    len: usize,
}

impl AgeBuf {
    fn new() -> Self {
        Self { buf: [0; 8], len: 0 }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for AgeBuf {
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
