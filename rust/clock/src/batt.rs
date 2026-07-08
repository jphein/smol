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
//!     grammar). Short tap → flip the VOLTAGE ↔ SOC page when the payload carries
//!     the optional SOC trio, else a no-op (see [`BattState::on_button`], #17).

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, ascii::FONT_6X10, MonoTextStyleBuilder},
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

    /// Number of pipe-separated segments in the cached payload, AFTER the `BATT|`
    /// marker (issue #17). `3` = a VOLTAGE-only payload (backward-compatible, no
    /// SOC page); `> 3` = the optional SOC trio (segments 4-6) is present, so the
    /// Batt screen offers a second page. `0` when never fetched / no marker.
    fn segment_count(&self) -> usize {
        match self.payload().strip_prefix("BATT|") {
            Some(body) => body.split('|').count(),
            None => 0,
        }
    }
}

/// Which page the Batt screen shows (issue #17). The retained payload optionally
/// carries a second trio of segments: 1-3 are battery VOLTAGES, 4-6 are state-of-
/// charge. A short tap flips between them when the SOC trio is present.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    /// Segments 1-3 — title row reads `Batt`.
    Voltage,
    /// Segments 4-6 — title row reads `SOC`.
    Soc,
}

impl Page {
    /// The other page. Short-tap toggles voltage ↔ SOC.
    fn toggle(self) -> Self {
        match self {
            Page::Voltage => Page::Soc,
            Page::Soc => Page::Voltage,
        }
    }
}

/// BATT screen state. Render-dedup bookkeeping (the data lives in the cache) plus
/// the selected page: repaint once/second so the fetch age ticks live, on a forced
/// redraw (menu entry), the instant a fresh fetch lands, AND the instant a tap
/// flips the page — mirroring the CLOCK/ABOUT dedup.
pub struct BattState {
    /// Last uptime-second painted (drives the once/second age tick).
    last_s: Option<u32>,
    /// Last `fetched_at_ms` painted — so a new reply repaints immediately rather
    /// than waiting up to a second for the age tick.
    last_fetch: Option<u64>,
    /// The page short-tap has selected (issue #17). Clamped at render to what the
    /// payload actually carries — a 3-segment payload always shows `Voltage`.
    page: Page,
    /// Last page painted — so a tap-driven page flip repaints immediately.
    last_page: Option<Page>,
}

impl BattState {
    /// Fresh state (nothing painted yet). No args: the age is derived from the
    /// cache + `ctx.now_ms`, so unlike Snake/About there is no entry stamp to take.
    pub fn new() -> Self {
        Self {
            last_s: None,
            last_fetch: None,
            page: Page::Voltage,
            last_page: None,
        }
    }
}

impl Plugin for BattState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // Short tap: flip the VOLTAGE ↔ SOC page (issue #17) — but ONLY when
            // the retained payload carries the optional SOC trio (> 3 segments).
            // A voltage-only (3-segment) payload has no second page, so the tap
            // stays a no-op there — backward-compatible with pre-#17 payloads and
            // boards (this SUPERSEDES the earlier "short tap is always a no-op"
            // ruling). The flip is the WHOLE action: it mutates only this plugin's
            // `page` field, opens no WiFi burst and touches no `Ctx` transport, so
            // a tap still can NEVER open the mesh-deaf `run_mqtt_burst` path nor
            // extend the spec's hard 1.5 s button bound. (An on-demand refresh is
            // still deliberately absent: the downlink already piggybacks every
            // burst the node opens — a gateway flush ~30 s, every build at boot —
            // so a "refresh now" flag would be redundant on a gateway and never
            // fire on a leaf, which opens no post-boot bursts.)
            Press::Short => {
                if ctx.batt.segment_count() > 3 {
                    self.page = self.page.toggle();
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

/// Paint the screen: the page title (`Batt`/`SOC`) + fetch age on the title row,
/// then the three lines of the CURRENT page. Free fn (all inputs are the cache +
/// age + page), reading disjoint `Ctx` fields (`display` mut, `batt` shared).
fn render(ctx: &mut Ctx, age_s: Option<u64>, page: Page) {
    let title_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let small = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    ctx.display.clear(BinaryColor::Off).ok();

    // Which page can actually be shown: the SOC page exists only when the payload
    // carries segments 4-6 (> 3 total; issue #17). Otherwise clamp to Voltage, so
    // a 3-segment payload (or an empty cache) always renders as `Batt` — even if
    // `page` still remembers a SOC selection from a richer earlier payload.
    let show_soc = page == Page::Soc && ctx.batt.segment_count() > 3;

    // Title: the current page name (matches the menu row + FONT_6X10 title style).
    let title = if show_soc { "SOC" } else { "Batt" };
    Text::with_baseline(title, Point::new(2, 0), title_style, Baseline::Top)
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

    // Rows 2-4: the three lines of the current page. The cache holds the payload
    // verbatim (`BATT|v1|v2|v3[|s1|s2|s3]`), so strip the marker, split on `|`,
    // then skip to this page's window (Voltage = segments 1-3; SOC = 4-6) and clip
    // to the panel width. Missing/short segments leave that row blank — the `--`
    // age already signals "no data".
    let body = ctx.batt.payload().strip_prefix("BATT|").unwrap_or("");
    let skip = if show_soc { 3 } else { 0 };
    for (i, seg) in body.split('|').skip(skip).take(3).enumerate() {
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
