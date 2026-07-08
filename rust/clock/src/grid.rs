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
//!   * [`GridState`] — the [`Plugin`]: page 0 (entry) = the BREAKDOWN (total / L1 /
//!     L2, three lines); a SHORT TAP reveals page 1 = the big TOTAL power filling the
//!     window (FONT_10X20, mirrors the CLOCK — JP's "just the power, whole window,
//!     when you click the button"). Long press → Menu (uniform grammar).

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
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

    /// The TOTAL segment (segment 1 after the `GRID|` marker, e.g. `878W` / `1.2kW`)
    /// for the big-number view, or `"--"` when never fetched / empty. The `<L1>`/`<L2>`
    /// breakdown lives in the later segments (shown on the breakdown page).
    fn total(&self) -> &str {
        match self.payload().strip_prefix("GRID|") {
            Some(body) => {
                let t = body.split('|').next().unwrap_or("");
                if t.is_empty() { "--" } else { t }
            }
            None => "--",
        }
    }
}

/// Grid has two pages: 0 = breakdown (entry), 1 = big total power.
const PAGE_COUNT: u8 = 2;

/// GRID screen state: render-dedup bookkeeping (data lives in the cache) + the
/// current page INDEX. Page 0 = the BREAKDOWN (total / L1 / L2, three-line) — the
/// entry view; page 1 = the big TOTAL power filling the window (JP's "just the power
/// … when you click the button"). Repaint once/second (live age), on a forced
/// redraw, on a fresh fetch, and on a page flip.
pub struct GridState {
    /// Last uptime-second painted (drives the once/second age tick).
    last_s: Option<u32>,
    /// Last `fetched_at_ms` painted — a new reply repaints at once.
    last_fetch: Option<u64>,
    /// Selected page INDEX (0 = breakdown, 1 = big total). Stored raw; the
    /// renderer/cycler take it `% PAGE_COUNT`, so an out-of-range boot page (from
    /// `DEFAULT_PAGE`) safely resolves to a valid page.
    page: u8,
    /// Last page painted — so a tap-driven flip repaints immediately.
    last_page: Option<u8>,
}

impl GridState {
    /// Fresh state — page 0, the breakdown overview (the entry view).
    pub fn new() -> Self {
        Self {
            last_s: None,
            last_fetch: None,
            page: 0,
            last_page: None,
        }
    }

    /// Seed the boot page (`board::DEFAULT_PAGE`) — stored raw, clamped at render.
    /// Only the boot one-shot calls this; Menu entry keeps page 0.
    pub fn set_page(&mut self, page: u8) {
        self.page = page;
    }
}

impl Plugin for GridState {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // Short tap: flip BREAKDOWN (0) ↔ big TOTAL power (1) — JP's "show just
            // the power, whole window, when you click the button". Pure `page`
            // mutation (no WiFi burst, no `Ctx` touch), so a tap can never extend the
            // mesh-deaf window past the spec's 1.5 s button bound.
            Press::Short => {
                self.page = self.page.wrapping_add(1) % PAGE_COUNT;
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Repaint iff forced (menu entry), the second rolled over (live age), a
        // fresh fetch landed, or a tap flipped the page — else leave the panel be.
        let sec = (ctx.now_ms / 1000) as u32;
        let fetched = ctx.grid.fetched_at_ms;
        if !(ctx.redraw
            || self.last_s != Some(sec)
            || self.last_fetch != fetched
            || self.last_page != Some(self.page))
        {
            return;
        }
        self.last_s = Some(sec);
        self.last_fetch = fetched;
        self.last_page = Some(self.page);

        // Age in whole seconds since the last fetch, or `None` if never fetched.
        let age_s = fetched.map(|f| ctx.now_ms.saturating_sub(f) / 1000);
        render(ctx, age_s, self.page);
    }
}

/// Paint the screen. Page 0 = BREAKDOWN (`Grid` + age title row, then total/L1/L2,
/// three-line) — the entry view. Page 1 = the big TOTAL power (FONT_10X20, mirroring
/// the CLOCK's big digits) filling the window + a small age. `page` is taken
/// `% PAGE_COUNT`, so an out-of-range boot page resolves to a valid one. Reads
/// disjoint `Ctx` fields (`display` mut, `grid` shared).
fn render(ctx: &mut Ctx, age_s: Option<u64>, page: u8) {
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

    // Own fetch age (`12s`/`5m`/`2h`, or `--` if never fetched) — OUR freshness.
    let mut age = AgeBuf::new();
    match age_s {
        Some(s) => write_age(&mut age, s),
        None => {
            let _ = age.write_str("--");
        }
    }

    if page.is_multiple_of(PAGE_COUNT) {
        // BREAKDOWN (entry view): title + age + the three payload lines.
        Text::with_baseline("Grid", Point::new(2, 0), title_style, Baseline::Top)
            .draw(ctx.display)
            .ok();
        Text::with_baseline(age.as_str(), Point::new(48, 1), small, Baseline::Top)
            .draw(ctx.display)
            .ok();
        let lines = ctx.grid.payload().strip_prefix("GRID|").unwrap_or("");
        for (i, seg) in lines.split('|').take(3).enumerate() {
            let y = 12 + i as i32 * 9;
            Text::with_baseline(clip(seg, LINE_CHARS), Point::new(2, y), small, Baseline::Top)
                .draw(ctx.display)
                .ok();
        }
    } else {
        // JP's "just the power, whole window": the TOTAL as a big centred number.
        let total = clip(ctx.grid.total(), 7); // ≤ 7 glyphs @ 10 px advance fits 72 px
        let w = total.chars().count() as i32 * 10;
        let x = ((72 - w) / 2).max(1); // centre like the CLOCK (10 px/char)
        Text::with_baseline(total, Point::new(x, 11), big, Baseline::Top)
            .draw(ctx.display)
            .ok();
        // Tiny age, top-right (like the CLOCK's AM/PM) — unobtrusive freshness.
        Text::with_baseline(age.as_str(), Point::new(56, 0), small, Baseline::Top)
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
