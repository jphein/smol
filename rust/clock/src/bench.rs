//! BENCH mode — live ESP-NOW link statistics (the mesh test).
//!
//! Compiled only under `--features espnow` (it is the one mode that exercises
//! the ESP-NOW mesh). It renders a compact multi-line `FONT_5X8` readout of the
//! link stats gathered by [`crate::net::mode::RadioManager`]:
//!
//!   * **FPS**  — render rate, measured by `main`'s loop and passed in.
//!   * **TX/s** — BENCH BEACONs we broadcast per second.
//!   * **RX/s** — peer BEACONs received per second.
//!   * **RTT**  — round-trip time (ms) from an echoed seq (`--` until measured).
//!   * **LOSS** — packet-loss percent inferred from peer seq gaps.
//!   * **RSSI** — last peer BEACON RSSI in dBm (`--` until a peer is heard).
//!   * **LINK** — peer link state (IDLE / SEEN / LINK) from the LED handshake.
//!
//! The stats themselves live in `net::mode` (piggybacked on the ESP-NOW path);
//! this module is purely the on-OLED presentation. See that module for how RTT /
//! loss / RSSI are actually measured.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, MeshStatus, Plugin, TimeSource, Transition};
use crate::input::Press;
use crate::led::LedState;
use crate::net::mode::{BenchStats, NodeView, RosterView};
use crate::net::names::name_for_id;

/// Per-peer rows per NODES page (3 rows + 1 own-status line fills the 5×8 grid).
const PEERS_PER_PAGE: usize = 3;

/// Bench plugin state: which page is showing. Page 0 = the LINK stats (today's
/// readout); pages 1.. = the node roster (issue #8). A short tap cycles pages
/// (Bench ignored taps before — free input); a long press returns to the menu.
pub struct BenchState {
    page: u8,
}

impl BenchState {
    pub fn new() -> Self {
        Self { page: 0 }
    }

    /// Total pages = 1 LINK page + ⌈roster.count / PEERS_PER_PAGE⌉ node pages.
    fn page_count(count: usize) -> usize {
        1 + count.div_ceil(PEERS_PER_PAGE)
    }
}

/// Fixed-capacity heap-free line builder for one 72 px OLED row (~14 chars in
/// `FONT_5X8`). Overflow is silently dropped — every line we build is bounded
/// and short by construction, and a clipped stat is cosmetic, never fatal.
struct Line {
    buf: [u8; 24],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self {
            buf: [0; 24],
            len: 0,
        }
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

/// Short label for the current peer link state (fits the compact readout).
fn link_str(link: LedState) -> &'static str {
    match link {
        LedState::Connected => "LINK",
        LedState::PeerDetected => "SEEN",
        // WifiSync only occurs during the boot burst, before BENCH runs; treat
        // anything else as idle for display purposes.
        _ => "IDLE",
    }
}

/// Draw the BENCH LINK readout (page 0). `fps` is the render loop's measured
/// frames-per-second (the one stat this module's data source can't see); `s` is
/// everything else. Five `FONT_5X8` rows fit the 40 px height (8 px each).
/// UNCHANGED from the pre-plugin `bench::draw` — the page tag is a separate
/// overlay so the stats layout is byte-identical. Individual draw errors are
/// ignored (a dropped pixel must never panic the firmware).
fn draw<D>(display: &mut D, s: &BenchStats, fps: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();

    // Build the five lines into stack buffers (no heap).
    let mut l0 = Line::new();
    let _ = write!(l0, "FPS{} TX{}", fps, s.tx_per_s);

    let mut l1 = Line::new();
    let _ = write!(l1, "RX{} LOS{}%", s.rx_per_s, s.loss_pct);

    let mut l2 = Line::new();
    match s.rtt_ms {
        Some(rtt) => {
            let _ = write!(l2, "RTT{}ms", rtt);
        }
        None => {
            let _ = write!(l2, "RTT--");
        }
    }

    let mut l3 = Line::new();
    match s.rssi {
        Some(rssi) => {
            let _ = write!(l3, "RSSI{}", rssi);
        }
        None => {
            let _ = write!(l3, "RSSI--");
        }
    }

    let mut l4 = Line::new();
    let _ = write!(l4, "LINK {}", link_str(s.link));

    // Lay the rows out at 8 px pitch from the top.
    let lines = [l0.as_str(), l1.as_str(), l2.as_str(), l3.as_str(), l4.as_str()];
    for (i, text) in lines.iter().enumerate() {
        Text::with_baseline(
            text,
            Point::new(0, i as i32 * 8),
            style,
            Baseline::Top,
        )
        .draw(display)
        .ok();
    }
}

/// The `FONT_5X8` On text style shared by the paged views.
fn text_style() -> embedded_graphics::mono_font::MonoTextStyle<'static, BinaryColor> {
    MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build()
}

/// ASCII-safe left-truncate to `n` bytes (magical nouns are ASCII, so a byte
/// boundary is a char boundary — no panic).
fn clip(s: &str, n: usize) -> &str {
    &s[..s.len().min(n)]
}

/// The `p/N` page indicator, top-right corner (overlaid on any page so the LINK
/// page stays byte-identical otherwise). `page` is 1-based.
fn draw_page_tag<D>(display: &mut D, page: usize, n_pages: usize)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let mut t = Line::new();
    let _ = write!(t, "{}/{}", page, n_pages);
    Text::with_baseline(t.as_str(), Point::new(57, 0), text_style(), Baseline::Top)
        .draw(display)
        .ok();
}

/// One peer row: `noun` (≤8, from the id) at x=0, RSSI at x=42, then age + a
/// trailing marker (`*` = carries mesh time, `~` = linked to us but free-running,
/// blank = merely heard). Fixed columns so ragged nouns don't misalign RSSI.
fn draw_peer_row<D>(display: &mut D, y: i32, n: &NodeView)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = text_style();
    let noun = if n.id_known { name_for_id(n.id).1 } else { "?" };
    Text::with_baseline(clip(noun, 8), Point::new(0, y), style, Baseline::Top)
        .draw(display)
        .ok();

    let mut rssi = Line::new();
    let _ = write!(rssi, "{}", n.rssi);
    Text::with_baseline(rssi.as_str(), Point::new(42, y), style, Baseline::Top)
        .draw(display)
        .ok();

    let mut am = Line::new();
    if n.age_s > 9 {
        let _ = write!(am, "9+");
    } else {
        let _ = write!(am, "{}s", n.age_s);
    }
    if n.has_mesh_time {
        let _ = write!(am, "*");
    } else if n.connected {
        let _ = write!(am, "~");
    }
    Text::with_baseline(am.as_str(), Point::new(62, y), style, Baseline::Top)
        .draw(display)
        .ok();
}

/// The own-status line (bottom row of every NODES page): our noun, our mesh-time
/// SOURCE (`root` = NTP origin, `<Noun` = adopted from that peer, `free` = never
/// synced), and our relay ROLE (`GATE`/`LEAF`, own role only — a peer's role is
/// never on the wire). Role is a separate right-aligned draw so it stays visible
/// even if a long source noun clips.
fn draw_own_status<D>(display: &mut D, y: i32, node_id: u8, mesh: &MeshStatus)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = text_style();
    let noun = name_for_id(node_id).1;

    let mut left = Line::new();
    let _ = write!(left, "{} ", clip(noun, 6));
    match mesh.source {
        TimeSource::NtpRoot => {
            let _ = write!(left, "root");
        }
        TimeSource::Adopted(src) => {
            // Read the adoption source id → its noun (provenance: who we adopted
            // from). This is the spec's `adopt<Noun>` intent, compacted to `<Noun`.
            let _ = write!(left, "<{}", clip(name_for_id(src).1, 5));
        }
        TimeSource::None => {
            let _ = write!(left, "free");
        }
    }
    Text::with_baseline(left.as_str(), Point::new(0, y), style, Baseline::Top)
        .draw(display)
        .ok();

    let role = if mesh.is_gateway { "GATE" } else { "LEAF" };
    Text::with_baseline(role, Point::new(52, y), style, Baseline::Top)
        .draw(display)
        .ok();
}

/// Render a NODES page (`page` 1-based over the node pages): a header with the
/// live peer count, up to `PEERS_PER_PAGE` peer rows, and the own-status line.
fn draw_nodes_page<D>(
    display: &mut D,
    roster: &RosterView,
    node_page: usize,
    node_id: u8,
    mesh: &MeshStatus,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    let mut hdr = Line::new();
    let _ = write!(hdr, "NODES {}", roster.count);
    Text::with_baseline(hdr.as_str(), Point::new(0, 0), text_style(), Baseline::Top)
        .draw(display)
        .ok();

    let start = node_page * PEERS_PER_PAGE;
    for row in 0..PEERS_PER_PAGE {
        let idx = start + row;
        if idx >= roster.count {
            break;
        }
        draw_peer_row(display, 8 + row as i32 * 8, &roster.nodes[idx]);
    }

    draw_own_status(display, 32, node_id, mesh);
}

/// BENCH as a [`Plugin`] (issue #8 mesh-view). Page 0 = the LINK stats (rendered
/// by `draw`, byte-identical to before) + a page tag; pages 1.. = the node
/// roster. Short tap cycles pages (Bench ignored taps before), long press →
/// Menu. Repaints every tick (like the old Bench arm), so a page change shows
/// immediately. Reachable only under `espnow`, so `ctx.radio`/`fps`/`mesh` exist.
impl Plugin for BenchState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                let count = match ctx.radio.as_deref() {
                    Some(r) => r.roster(ctx.now_ms).count,
                    None => 0,
                };
                let n_pages = Self::page_count(count);
                self.page = ((self.page as usize + 1) % n_pages) as u8;
                ctx.redraw = true;
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        // Snapshot everything up front (Copy), releasing the radio borrow before
        // we touch the display. `bench_stats` refreshes the rate windows (&mut),
        // `roster` is a read — called sequentially so the borrows never overlap.
        let (stats, roster) = match ctx.radio.as_deref_mut() {
            Some(r) => (r.bench_stats(ctx.now_ms), r.roster(ctx.now_ms)),
            // No radio (bring-up failed): render nothing, exactly like the old arm.
            None => return,
        };
        let fps = ctx.fps;
        let mesh = ctx.mesh;
        let node_id = ctx.node_id;

        let n_pages = Self::page_count(roster.count);
        // The roster can shrink (peers age out) between ticks — clamp the page.
        if self.page as usize >= n_pages {
            self.page = 0;
        }

        ctx.display.clear(BinaryColor::Off).ok();
        if self.page == 0 {
            draw(ctx.display, &stats, fps);
        } else {
            draw_nodes_page(ctx.display, &roster, self.page as usize - 1, node_id, &mesh);
        }
        draw_page_tag(ctx.display, self.page as usize + 1, n_pages);
        ctx.display.flush().ok();
    }
}
