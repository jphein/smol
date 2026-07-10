//! BATT screen — Home-Assistant battery voltages on the 72×40 OLED (`cfg(wifi)`).
//!
//! The board is `no_std` (smoltcp) and HA's REST API is TLS-only, so the firmware
//! cannot call HA directly. Instead it speaks **MQTT** to HA's Mosquitto broker
//! over plain TCP (`net/mqtt.rs`): an HA automation publishes a display-ready
//! battery payload RETAINED to `smol/display/batt`, and a gateway that connects
//! for a ~2 s burst is handed it immediately (the broker is the cache). The
//! gateway stores it here and re-broadcasts it as a **SMOLv1 BATT** ESP-NOW frame
//! so leaves fill their cache too. This module is transport-agnostic: it just
//! renders whatever bytes the cache holds + how long ago they arrived.
//!
//! ## Payload format (LOCKED — HA automation + firmware agree byte-for-byte)
//!
//! The `smol/display/batt` payload is ASCII, ≤ 96 bytes:
//! `BATT|<line1>|<line2>|<line3>[|<soc1>|<soc2>|<soc3>]` — a `BATT|` marker then
//! pipe-separated, display-ready lines, each ≤ 12 chars, no trailing pipe.
//! Segments 1-3 are the VOLTAGE page (default content
//! `BATT|48V 52.8V|HV 391.9V|d 43mV`); the OPTIONAL segments 4-6 are the
//! state-of-charge (SOC) page (issue #17) — present iff HA appends them, still
//! ≤ 96 B total. Any source entity that is unavailable or stale (> 30 min by
//! `last_reported`) renders as `--` (HA's job, not ours).
//!
//! The cache stores this payload **verbatim, marker included** — so the SMOLv1
//! BATT frame is a byte-for-byte memcpy of the cache (frame = 12-B tag +
//! `bytes()`), and a received frame's payload is a memcpy straight back into the
//! cache. The plugin strips the `BATT|` marker only at render time.
//!
//! ## Split of responsibility
//!
//!   * [`BattCache`] — the verbatim payload bytes + a fetch timestamp. Owned by
//!     `main` (NOT the plugin): it is filled from a WiFi burst (MQTT downlink) or
//!     an inbound BATT frame while this screen may be inactive, and the plugin only
//!     ever READS it, borrowed through [`crate::app::Ctx::batt`] — mirroring the
//!     cfg'd `Ctx` fields (`label`, `mesh`, `radio`) `main` already hands plugins.
//!   * [`BattState`] — the [`Plugin`]: renders `Batt` + own fetch age on the title
//!     row and the three battery lines below it. Long press → Menu (uniform
//!     grammar). Short tap → CYCLE the pages: 0 = the three-line voltage overview,
//!     1..N = one BIG full-window page per non-empty detail segment (Task 3).

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// The payload marker every battery payload begins with. Validated on [`store`]
/// (reject anything not starting with it) and stripped before rendering. Both the
/// retained MQTT payload and the SMOLv1 BATT frame carry it verbatim.
///
/// [`store`]: BattCache::store
const MARKER: &[u8] = b"BATT|";

/// Max bytes the cache retains — the payload is ≤ 96 bytes total (LOCKED). Stored
/// verbatim (marker included), so this is the exact wire size, never clipped.
const CACHE_CAP: usize = 96;
/// Screen width in FONT_5X8 glyphs: 72 px / 6 px-advance = 12 chars. Each
/// collector line is already ≤ 12 (LOCKED), but we clip defensively so a
/// malformed/over-long line can never draw off-panel.
const LINE_CHARS: usize = 12;

/// The HA battery-voltage cache: the collector's raw reply lines + when we last
/// fetched them. Owned by `main`, filled by the WiFi burst (`net/wifi.rs`), read
/// by [`BattState`]. `Copy`-free (it holds a byte buffer) and heap-free — it lives
/// as a single `main` local, borrowed to the plugin per tick.
pub struct BattCache {
    /// The battery payload VERBATIM, marker included (`BATT|line1|line2|line3`),
    /// exactly as received over MQTT or a BATT frame. `..len` is valid; the rest
    /// is stale/zero. Stored whole so the BATT frame is a pure memcpy of it.
    lines: [u8; CACHE_CAP],
    len: usize,
    /// Monotonic-ms stamp (`millis()`) of the last successful fetch, or `None` if
    /// we have never received a reply (title age then shows `--`).
    fetched_at_ms: Option<u64>,
}

impl BattCache {
    /// An empty cache (never fetched). `const` so `main` can seed it cheaply.
    pub const fn new() -> Self {
        Self {
            lines: [0; CACHE_CAP],
            len: 0,
            fetched_at_ms: None,
        }
    }

    /// Store a battery payload VERBATIM and stamp the fetch time. `payload` is the
    /// whole `BATT|…` byte string (MQTT retained downlink, or a received BATT
    /// frame's bytes). Rejects anything not starting with the `BATT|` [`MARKER`],
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

    /// The verbatim payload bytes (`BATT|…`), for the SMOLv1 BATT broadcast frame
    /// (`frame = tag + bytes()`). Empty until the first `store`. Only the espnow
    /// build broadcasts (gateway → leaves), so this is espnow-only.
    #[cfg(feature = "espnow")]
    pub fn bytes(&self) -> &[u8] {
        &self.lines[..self.len]
    }

    /// True until a payload has been cached (nothing to broadcast). espnow-only —
    /// it gates the gateway's periodic BATT re-broadcast.
    #[cfg(feature = "espnow")]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The stored payload as `&str` (`BATT|line1|line2|line3`), or `""` if never
    /// fetched / non-UTF-8. Lossy-free: the payload is ASCII, so valid UTF-8.
    fn payload(&self) -> &str {
        core::str::from_utf8(&self.lines[..self.len]).unwrap_or("")
    }

    /// The count of NON-EMPTY detail segments — payload segments 4+ (after the three
    /// voltage lines) that carry real data (non-empty, not the `--` sentinel). Each
    /// drives one BIG full-window SOC/charge page (Task 3). `0` for a 3-segment
    /// (voltage-only) payload → no detail pages (backward-compatible no-op tap).
    fn detail_count(&self) -> usize {
        match self.payload().strip_prefix("BATT|") {
            Some(body) => body
                .split('|')
                .skip(3)
                .filter(|s| !s.is_empty() && *s != "--")
                .count(),
            None => 0,
        }
    }

    /// The `k`-th (0-based) non-empty detail segment (e.g. `48V 69%` / `Chg 16.5A`),
    /// or `None`. `--`/empty segments are skipped so a big page never goes blank.
    fn detail_segment(&self, k: usize) -> Option<&str> {
        self.payload()
            .strip_prefix("BATT|")?
            .split('|')
            .skip(3)
            .filter(|s| !s.is_empty() && *s != "--")
            .nth(k)
    }
}

/// BATT screen state. Render-dedup bookkeeping (data lives in the cache) + the
/// current page INDEX. Page 0 = the VOLTAGE overview (three-line); pages 1..N = one
/// BIG full-window page per non-empty detail segment (Task 3 — SOC% per bank, charge
/// A). Repaint once/second (live age), on a forced redraw, on a fresh fetch, and on
/// a page flip — mirroring the CLOCK/ABOUT dedup.
pub struct BattState {
    /// Last uptime-second painted (drives the once/second age tick).
    last_s: Option<u32>,
    /// Last `fetched_at_ms` painted — a new reply repaints at once.
    last_fetch: Option<u64>,
    /// Selected page INDEX (0 = voltage overview; 1.. = big detail pages). Stored
    /// raw; the renderer/cycler take it `% page_count`, so an out-of-range boot page
    /// (from `DEFAULT_PAGE`) or a shrunk payload safely resolves to a valid page.
    page: u8,
    /// Last page painted — so a tap-driven flip repaints immediately.
    last_page: Option<u8>,
}

impl BattState {
    /// Fresh state (page 0 = the voltage overview). The age derives from the cache,
    /// so unlike Snake/About there is no entry stamp to take.
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

    /// #50: the live page index (raw; the renderer clamps `% page_count`). Read for
    /// the `smol/<id>/status` readback of the ACTUAL screen state.
    pub fn page(&self) -> u8 {
        self.page
    }
}

impl Plugin for BattState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // Short tap: CYCLE the pages 0→1→…→N→0 (Task 3). Page 0 = voltage
            // overview; pages 1..N = one big page per non-empty detail segment. A
            // voltage-only (3-segment) payload has exactly ONE page, so the tap is a
            // no-op there — backward-compatible. Pure `page` mutation (no WiFi burst,
            // no `Ctx` transport touch), so a tap can NEVER open `run_mqtt_burst` nor
            // extend the spec's hard 1.5 s button bound.
            Press::Short => {
                let n = 1 + ctx.batt.detail_count() as u8;
                if n > 1 {
                    self.page = self.page.wrapping_add(1) % n;
                }
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Repaint iff forced (menu entry), the second rolled over (live age), a
        // fresh fetch landed, or a tap flipped the page since we last painted —
        // else leave the panel be.
        let sec = (ctx.now_ms / 1000) as u32;
        let fetched = ctx.batt.fetched_at_ms;
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

/// Paint the screen. Page 0 = VOLTAGE overview (`Batt` + age title row, then the
/// three voltage lines). Pages 1..N = one BIG full-window detail page (Task 3): the
/// segment's short label (before the first space) small on top + its value (after)
/// LARGE (FONT_10X20). `page` is taken `% page_count`, so an out-of-range boot page
/// or a shrunk payload resolves to a valid page. Reads disjoint `Ctx` fields.
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

    // Own fetch age (`12s`/`5m`/`2h`, or `--` if never fetched) — OUR freshness,
    // distinct from any `--` HA renders inside a line for a stale source entity.
    let mut age = AgeBuf::new();
    match age_s {
        Some(s) => write_age(&mut age, s),
        None => {
            let _ = age.write_str("--");
        }
    }

    // Pages: 0 = voltage overview, 1..N = one big page per non-empty detail segment.
    let n = 1 + ctx.batt.detail_count() as u8;
    let p = page % n.max(1);

    if p == 0 {
        // Voltage overview: title + age + the three voltage lines (segments 1-3).
        Text::with_baseline("Batt", Point::new(2, 0), title_style, Baseline::Top)
            .draw(ctx.display)
            .ok();
        Text::with_baseline(age.as_str(), Point::new(48, 1), small, Baseline::Top)
            .draw(ctx.display)
            .ok();
        let body = ctx.batt.payload().strip_prefix("BATT|").unwrap_or("");
        for (i, seg) in body.split('|').take(3).enumerate() {
            let y = 12 + i as i32 * 9;
            Text::with_baseline(clip(seg, LINE_CHARS), Point::new(2, y), small, Baseline::Top)
                .draw(ctx.display)
                .ok();
        }
    } else if let Some(seg) = ctx.batt.detail_segment((p - 1) as usize) {
        // Big detail page: `<label> <value>` → small label on top, BIG value below.
        let (label, value) = seg.split_once(' ').unwrap_or(("", seg));
        Text::with_baseline(clip(label, LINE_CHARS), Point::new(2, 1), small, Baseline::Top)
            .draw(ctx.display)
            .ok();
        // Tiny age top-right so freshness stays visible on the big page too.
        Text::with_baseline(age.as_str(), Point::new(56, 1), small, Baseline::Top)
            .draw(ctx.display)
            .ok();
        let v = clip(value, 7); // ≤ 7 glyphs @ 10 px advance fits 72 px
        let w = v.chars().count() as i32 * 10;
        let x = ((72 - w) / 2).max(1);
        Text::with_baseline(v, Point::new(x, 14), big, Baseline::Top)
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

/// Tiny heap-free line builder for the age string (Batt is a wifi build, which
/// has an allocator, but a fixed buffer keeps the render alloc-free and mirrors
/// `about.rs`'s `Line`). 8 bytes is ample for `NNNNh`.
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
