//! GRID screen — Home-Assistant grid-power lines on the 72×40 OLED (`cfg(wifi)`).
//!
//! The **twin of [`crate::batt`]** (issue #16): same transport, same cache shape,
//! same wire mechanics — a DELIBERATE copy, not a shared generic (two clear twins
//! read better than one clever framework, and the two payloads/topics/frames stay
//! independently evolvable). The board is `no_std` and HA's REST API is TLS-only,
//! so an HA automation publishes a display-ready grid payload RETAINED to
//! `smol/display/grid`; a gateway that connects for a burst is handed it
//! immediately (the broker is the cache) and re-broadcasts it as a **SMOLv1 GRID**
//! ESP-NOW frame so leaves fill their cache too. Transport-agnostic: it renders
//! whatever bytes the cache holds + how long ago they arrived.
//!
//! Unlike Batt, Grid is a **single page** — no SOC-style short-press toggle (the
//! grid payload has no optional second trio), so the short tap is a plain no-op.
//!
//! ## Payload format (LOCKED — HA automation + firmware agree byte-for-byte)
//!
//! The `smol/display/grid` payload is ASCII, ≤ 96 bytes:
//! `GRID|<line1>|<line2>|<line3>` — a `GRID|` marker then pipe-separated,
//! display-ready lines, each ≤ 12 chars, no trailing pipe. Default content:
//! `GRID|963W|L1 177W|L2 786W` (total grid power, then the two phase clamps). Any
//! source entity that is unavailable or stale (> 30 min by `last_reported`) renders
//! as `--` (HA's job, not ours).
//!
//! The cache stores this payload **verbatim, marker included** — so the SMOLv1
//! GRID frame is a byte-for-byte memcpy of the cache (frame = 12-B tag +
//! `bytes()`), and a received frame's payload is a memcpy straight back into the
//! cache. The plugin strips the `GRID|` marker only at render time.
//!
//! ## Split of responsibility
//!
//!   * [`GridCache`] — the verbatim payload bytes + a fetch timestamp. Owned by
//!     `main` (NOT the plugin): it is filled from a WiFi burst (MQTT downlink) or
//!     an inbound GRID frame while this screen may be inactive, and the plugin only
//!     ever READS it, borrowed through [`crate::app::Ctx::grid`] — mirroring
//!     `Ctx::batt`.
//!   * [`GridState`] — the [`Plugin`]: renders `Grid` + own fetch age on the title
//!     row and the three grid lines below it. Long press → Menu (uniform grammar).
//!     Short tap → a documented NO-OP (single page — see [`GridState::on_button`]).

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// The payload marker every grid payload begins with. Validated on [`store`]
/// (reject anything not starting with it) and stripped before rendering. Both the
/// retained MQTT payload and the SMOLv1 GRID frame carry it verbatim.
///
/// [`store`]: GridCache::store
const MARKER: &[u8] = b"GRID|";

/// Max bytes the cache retains — the payload is ≤ 96 bytes total (LOCKED). Stored
/// verbatim (marker included), so this is the exact wire size, never clipped.
const CACHE_CAP: usize = 96;
/// Screen width in FONT_5X8 glyphs: 72 px / 6 px-advance = 12 chars. Each line is
/// already ≤ 12 (LOCKED), but we clip defensively so a malformed/over-long line
/// can never draw off-panel.
const LINE_CHARS: usize = 12;

/// The HA grid-power cache: the retained payload's raw lines + when we last
/// fetched them. Owned by `main`, filled by the WiFi burst (`net/wifi.rs`), read
/// by [`GridState`]. `Copy`-free (it holds a byte buffer) and heap-free — it lives
/// as a single `main` local, borrowed to the plugin per tick.
pub struct GridCache {
    /// The grid payload VERBATIM, marker included (`GRID|line1|line2|line3`),
    /// exactly as received over MQTT or a GRID frame. `..len` is valid; the rest
    /// is stale/zero. Stored whole so the GRID frame is a pure memcpy of it.
    lines: [u8; CACHE_CAP],
    len: usize,
    /// Monotonic-ms stamp (`millis()`) of the last successful fetch, or `None` if
    /// we have never received a reply (title age then shows `--`).
    fetched_at_ms: Option<u64>,
}

impl GridCache {
    /// An empty cache (never fetched). `const` so `main` can seed it cheaply.
    pub const fn new() -> Self {
        Self {
            lines: [0; CACHE_CAP],
            len: 0,
            fetched_at_ms: None,
        }
    }

    /// Store a grid payload VERBATIM and stamp the fetch time. `payload` is the
    /// whole `GRID|…` byte string (MQTT retained downlink, or a received GRID
    /// frame's bytes). Rejects anything not starting with the `GRID|` [`MARKER`],
    /// leaving the prior cache intact — so a corrupt/foreign frame never wipes a
    /// good reading. Truncates to [`CACHE_CAP`] (the payload is ≤ 96 B by spec).
    pub fn store(&mut self, payload: &[u8], now_ms: u64) {
        if !payload.starts_with(MARKER) {
            return;
        }
        let n = payload.len().min(CACHE_CAP);
        self.lines[..n].copy_from_slice(&payload[..n]);
        self.len = n;
        self.fetched_at_ms = Some(now_ms);
    }

    /// The verbatim payload bytes (`GRID|…`), for the SMOLv1 GRID broadcast frame
    /// (`frame = tag + bytes()`). Empty until the first `store`. Only the espnow
    /// build broadcasts (gateway → leaves), so this is espnow-only.
    #[cfg(feature = "espnow")]
    pub fn bytes(&self) -> &[u8] {
        &self.lines[..self.len]
    }

    /// True until a payload has been cached (nothing to broadcast). espnow-only —
    /// it gates the gateway's periodic GRID re-broadcast.
    #[cfg(feature = "espnow")]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The stored payload as `&str` (`GRID|line1|line2|line3`), or `""` if never
    /// fetched / non-UTF-8. Lossy-free: the payload is ASCII, so valid UTF-8.
    fn payload(&self) -> &str {
        core::str::from_utf8(&self.lines[..self.len]).unwrap_or("")
    }
}

/// GRID screen state. Only render-dedup bookkeeping (the data lives in the cache):
/// repaint once/second so the fetch age ticks live, on a forced redraw (menu
/// entry), and the instant a fresh fetch lands — mirroring the CLOCK/ABOUT dedup.
pub struct GridState {
    /// Last uptime-second painted (drives the once/second age tick).
    last_s: Option<u32>,
    /// Last `fetched_at_ms` painted — so a new reply repaints immediately rather
    /// than waiting up to a second for the age tick.
    last_fetch: Option<u64>,
}

impl GridState {
    /// Fresh state (nothing painted yet). No args: the age is derived from the
    /// cache + `ctx.now_ms`, so there is no entry stamp to take.
    pub fn new() -> Self {
        Self {
            last_s: None,
            last_fetch: None,
        }
    }
}

impl Plugin for GridState {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // Short tap: intentional NO-OP. Grid is a SINGLE page (unlike Batt's
            // voltage↔SOC toggle — the grid payload has no optional second trio), so
            // there is nothing to page to. As with Batt, the fetch already
            // PIGGYBACKS every burst the node opens (a gateway on each ~30 s flush,
            // every build at boot; leaves get the gateway's GRID frame), so an
            // on-demand refresh would be redundant on a gateway and never fire on a
            // leaf — and a no-op guarantees a tap can never extend the mesh-deaf
            // window past the spec's 1.5 s button bound.
            Press::Short => Transition::Stay,
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Repaint iff forced (menu entry), the second rolled over (live age), or a
        // fresh fetch landed since we last painted — else leave the panel be.
        let sec = (ctx.now_ms / 1000) as u32;
        let fetched = ctx.grid.fetched_at_ms;
        if !(ctx.redraw || self.last_s != Some(sec) || self.last_fetch != fetched) {
            return;
        }
        self.last_s = Some(sec);
        self.last_fetch = fetched;

        // Age in whole seconds since the last fetch, or `None` if never fetched.
        let age_s = fetched.map(|f| ctx.now_ms.saturating_sub(f) / 1000);
        render(ctx, age_s);
    }
}

/// Paint the screen: `Grid` + fetch age on the title row, then the three grid
/// lines. Free fn (all inputs are the cache + age), reading disjoint `Ctx` fields
/// (`display` mut, `grid` shared).
fn render(ctx: &mut Ctx, age_s: Option<u64>) {
    let title_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let small = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    ctx.display.clear(BinaryColor::Off).ok();

    // Title: the screen name (matches the menu row + FONT_6X10 title elsewhere).
    Text::with_baseline("Grid", Point::new(2, 0), title_style, Baseline::Top)
        .draw(ctx.display)
        .ok();

    // Own fetch age, right of the title (`12s` / `5m` / `2h`, or `--` if never
    // fetched). This is OUR freshness (when the cache last changed) — distinct
    // from any `--` HA renders inside a line for a stale/unavailable source entity.
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

    // Rows 2-4: the three grid lines. The cache holds the payload verbatim
    // (`GRID|l1|l2|l3`), so strip the marker, then split on `|` and clip to the
    // panel width. Missing segments (never fetched, or a short/malformed payload)
    // leave that row blank — the `--` age already signals "no data".
    let lines = ctx.grid.payload().strip_prefix("GRID|").unwrap_or("");
    for (i, seg) in lines.split('|').take(3).enumerate() {
        let y = 12 + i as i32 * 9;
        Text::with_baseline(clip(seg, LINE_CHARS), Point::new(2, y), small, Baseline::Top)
            .draw(ctx.display)
            .ok();
    }

    ctx.display.flush().ok();
}

/// Clip `s` to at most `max` characters on a UTF-8 boundary (protocol is ASCII,
/// but this is boundary-safe regardless — never panics on a byte-slice).
fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Write a compact fetch age (`45s` / `12m` / `3h`) into `out`. Bounded to ≤ 4
/// glyphs so it always fits beside the title (48 px..72 px = 24 px = 4 chars).
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

/// Tiny heap-free line builder for the age string (mirrors `batt.rs`'s `AgeBuf`).
/// 8 bytes is ample for `NNNNh`.
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
