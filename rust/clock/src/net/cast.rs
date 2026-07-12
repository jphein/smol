//! #26 smol Cast — stream the gateway's OLED image to a network WLED matrix as
//! realtime UDP pixels.
//!
//! ## What this is
//! The gateway mirrors the 72×40 1-bit image on its own glass onto a network LED
//! matrix running WLED, over plain WiFi/UDP (NOT ESP-NOW — cleaner than #25's
//! control path). WLED exposes a realtime pixel protocol on UDP :21324; we
//! down-sample smol's mono framebuffer to the matrix's NxM grid, colour it (a
//! mono→RGB choice, since the OLED can't do colour), and stream one frame every
//! ~100 ms while the radio is associated. `feature = "cast"` (= `wifi`); the
//! default / wifi-only-without-cast / espnow / wled builds are byte-free of it.
//!
//! ## This file is PURE (no esp-hal / ssd1306 / embedded-graphics deps)
//! Everything here is plain integer/byte logic so it is host-unit-testable off the
//! target (see the scratch harness). The DrawTarget tee-wrapper that FEEDS the
//! [`Mirror`] from live draws lives in `net/cast_oled.rs` (it needs ssd1306), and
//! the UDP send site lives in `net/wifi.rs` (it needs smoltcp). This module owns
//! only: the shadow framebuffer, the down-sample, the serpentine LED mapping, and
//! the DNRGB packet encoder.
//!
//! ## Wire protocol — WLED DNRGB (realtime UDP, port 21324)
//! WLED realtime frame: `[proto][timeout][start_hi][start_lo][R,G,B]…`.
//!   * `proto = 4` (DNRGB — start-indexed RGB; DRGB=2 has no index and caps a whole
//!     frame at one packet, but a 16×16 = 256-LED DRGB frame is 770 B > the 512 B
//!     smoltcp UDP TX buffer, so we always use DNRGB and CHUNK — see [`MAX_LEDS_PER_PKT`]).
//!   * `timeout` = seconds WLED waits after the LAST packet before reverting to its
//!     normal (non-realtime) mode. A small value ([`DEFAULT_TIMEOUT_S`]) means the
//!     matrix self-releases a few seconds after cast stops — no explicit "off" frame
//!     needed. 255 would pin realtime until reboot (we deliberately do NOT use it).
//!   * `start` = 16-bit LED index the packet's first RGB triple applies to.
//!
//! WLED lays the linear LED stream onto its configured 2D grid; a **serpentine**
//! matrix wires odd rows right→left, so we emit physical-index order with the
//! odd-row reversal baked in (see [`cell_for_led`]) — "serpentine order matters".

/// Source framebuffer geometry — the smol OLED (must match `DisplaySize72x40`).
pub const SRC_W: usize = 72;
pub const SRC_H: usize = 40;
/// 72×40 = 2880 px, 1 bit each = 360 bytes. Row-major bit-packed.
const SRC_BYTES: usize = SRC_W * SRC_H / 8;

/// WLED realtime UDP port.
pub const WLED_PORT: u16 = 21324;

/// DNRGB protocol id (byte 0) + its 4-byte header `[id][timeout][hi][lo]`.
const PROTO_DNRGB: u8 = 4;
const DNRGB_HEADER: usize = 4;

/// Max LEDs packed into ONE UDP packet. Bounds a packet to `4 + 3*128 = 388 B`,
/// safely under the 512 B smoltcp UDP TX buffer smol already allocates (`net/wifi.rs`).
/// A 16×16 = 256-LED matrix therefore streams as 2 packets/frame; 8×8 = 64 as 1.
pub const MAX_LEDS_PER_PKT: usize = 128;

/// Default WLED revert timeout (byte 1), seconds. Short so the matrix returns to its
/// normal effect a couple seconds after casting stops, with no explicit off-frame.
pub const DEFAULT_TIMEOUT_S: u8 = 2;

/// A shadow copy of the gateway's 1-bit glass image, in logical (pre-rotation)
/// embedded-graphics coordinates — the SAME (x, y) the app draw code plots at.
/// Fed pixel-for-pixel by the [`crate::net::cast_oled::CastOled`] tee-wrapper as the
/// active screen renders, so it always holds exactly what was last flushed to the
/// OLED. Fixed 360 B, lives in the display owner's frame — no heap.
#[derive(Clone)]
pub struct Mirror {
    bits: [u8; SRC_BYTES],
}

impl Mirror {
    pub const fn new() -> Self {
        Self { bits: [0; SRC_BYTES] }
    }

    /// Set/clear one logical pixel. Out-of-bounds is a silent no-op (mirrors the
    /// ssd1306 `set_pixel` contract, so the tee never diverges from the panel).
    #[inline]
    pub fn plot(&mut self, x: usize, y: usize, on: bool) {
        if x >= SRC_W || y >= SRC_H {
            return;
        }
        let idx = y * SRC_W + x;
        let byte = idx / 8;
        let bit = idx % 8;
        if on {
            self.bits[byte] |= 1 << bit;
        } else {
            self.bits[byte] &= !(1 << bit);
        }
    }

    /// Read one logical pixel (false when out of bounds).
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> bool {
        if x >= SRC_W || y >= SRC_H {
            return false;
        }
        let idx = y * SRC_W + x;
        (self.bits[idx / 8] >> (idx % 8)) & 1 != 0
    }

    /// Clear (or fill) the whole shadow — mirrors `display.clear()`.
    #[inline]
    pub fn fill(&mut self, on: bool) {
        self.bits.fill(if on { 0xFF } else { 0 });
    }
}

impl Default for Mirror {
    fn default() -> Self {
        Self::new()
    }
}

/// Target-matrix description. Dimensions + wiring + colour come from the git-ignored
/// `secrets.rs` (the WLED host is a LAN IP, so it must never be committed to this
/// PUBLIC repo — same discipline as `WIFI_SSID` / `OTA_IMAGE_HOSTS`).
#[derive(Clone, Copy)]
pub struct MatrixCfg {
    /// Matrix width / height in LEDs (e.g. 16 × 16).
    pub w: usize,
    pub h: usize,
    /// True if the matrix is wired serpentine (odd rows run right→left).
    pub serpentine: bool,
    /// Rotate the source 180° before mapping — matches the OLED's `Rotate180`
    /// mount so the matrix shows what a viewer reads off the glass. Tune on-glass.
    pub flip180: bool,
    /// A matrix cell lights when ≥ this percent of its source block is lit. Low
    /// (~12) keeps sparse OLED text legible when down-sampled to a coarse grid.
    pub thresh_pct: u32,
    /// RGB for a lit / unlit cell (the mono→colour choice; OLED can't do colour).
    pub on: [u8; 3],
    pub off: [u8; 3],
    /// WLED revert timeout (byte 1), seconds.
    pub timeout_s: u8,
}

impl MatrixCfg {
    /// Total LED count = w × h.
    #[inline]
    pub fn total(&self) -> usize {
        self.w * self.h
    }
}

/// Inverse serpentine mapping: which source-grid cell `(mx, my)` a given PHYSICAL
/// LED index drives. DNRGB writes to CONSECUTIVE physical LEDs, so we walk physical
/// index order and reverse odd rows for a serpentine matrix. Row-major, LED 0 =
/// top-left, rows top→bottom.
#[inline]
pub fn cell_for_led(led: usize, cfg: &MatrixCfg) -> (usize, usize) {
    let row = led.checked_div(cfg.w).unwrap_or(0);
    let col_in_row = led.checked_rem(cfg.w).unwrap_or(0);
    let mx = if cfg.serpentine && (row & 1 == 1) {
        cfg.w.saturating_sub(1).saturating_sub(col_in_row)
    } else {
        col_in_row
    };
    (mx, row)
}

/// Down-sample one matrix cell to on/off: is the fraction of lit source pixels in
/// the cell's source block ≥ `thresh_pct`? Box-maps the NxM grid over 72×40 and
/// applies the optional 180° flip. Total-safe (empty block → off; div guards).
#[inline]
pub fn cell_on(m: &Mirror, mx: usize, my: usize, cfg: &MatrixCfg) -> bool {
    if cfg.w == 0 || cfg.h == 0 {
        return false;
    }
    let x0 = mx * SRC_W / cfg.w;
    let x1 = (((mx + 1) * SRC_W / cfg.w).max(x0 + 1)).min(SRC_W);
    let y0 = my * SRC_H / cfg.h;
    let y1 = (((my + 1) * SRC_H / cfg.h).max(y0 + 1)).min(SRC_H);
    let mut lit: u32 = 0;
    let mut tot: u32 = 0;
    let mut sy = y0;
    while sy < y1 {
        let mut sx = x0;
        while sx < x1 {
            let (rx, ry) = if cfg.flip180 {
                (SRC_W - 1 - sx, SRC_H - 1 - sy)
            } else {
                (sx, sy)
            };
            if m.get(rx, ry) {
                lit += 1;
            }
            tot += 1;
            sx += 1;
        }
        sy += 1;
    }
    tot > 0 && lit * 100 >= cfg.thresh_pct * tot
}

/// Encode ONE DNRGB packet covering LEDs `[start, start+count)` into `out`, where
/// `count` is bounded by both [`MAX_LEDS_PER_PKT`] and the room in `out`. Returns
/// `Some((bytes_written, next_start))`: send `out[..bytes_written]`, then if
/// `next_start < cfg.total()` call again with `next_start` to emit the rest of the
/// frame. Returns `None` when `start` is past the end or `out` can't hold a header
/// + one LED (never panics; total on every branch).
pub fn pack_dnrgb(m: &Mirror, cfg: &MatrixCfg, start: usize, out: &mut [u8]) -> Option<(usize, usize)> {
    let total = cfg.total();
    if start >= total || out.len() < DNRGB_HEADER + 3 {
        return None;
    }
    let room_leds = (out.len() - DNRGB_HEADER) / 3;
    let count = room_leds.min(MAX_LEDS_PER_PKT).min(total - start);
    out[0] = PROTO_DNRGB;
    out[1] = cfg.timeout_s;
    out[2] = ((start >> 8) & 0xFF) as u8;
    out[3] = (start & 0xFF) as u8;
    let mut i = 0;
    while i < count {
        let led = start + i;
        let (mx, my) = cell_for_led(led, cfg);
        let rgb = if cell_on(m, mx, my, cfg) { cfg.on } else { cfg.off };
        let o = DNRGB_HEADER + i * 3;
        out[o] = rgb[0];
        out[o + 1] = rgb[1];
        out[o + 2] = rgb[2];
        i += 1;
    }
    Some((DNRGB_HEADER + count * 3, start + count))
}

// =========================================================================
// Cross-module handoff (single-threaded, no ISR — the established smol pattern).
// =========================================================================
//
// The frame SOURCE (the live glass image) is produced in `main`'s render loop
// (via the [`crate::net::cast_oled::CastOled`] tee), but the UDP SEND happens deep
// inside a gateway flush (`net::wifi::run_mqtt_burst`, reached through the
// `RadioManager`). Rather than thread a `&Mirror` + flag through every layer, we
// hand them off through two `static mut`s — exactly the discipline `main`'s
// `NODE_ID_CACHE` and `net::mode`'s `GW_OTA_WINDOW` already use: written and read
// ONLY from the single-threaded boot/main path (never an ISR), so the accesses are
// race-free, and `addr_of[_mut]!` avoids ever forming a reference to the static
// (keeps the `static_mut_refs` lint quiet).

/// The most-recent glass image, published by `main` each render tick.
static mut CAST_MIRROR: Mirror = Mirror::new();
/// Whether HA has enabled casting (retained `smol/<id>/cast` = `ON`). Re-read from
/// the broker each gateway flush, so absence / a cleared topic reads as OFF.
static mut CAST_ENABLED: bool = false;

/// `main`: copy the freshly-rendered glass into the shared cast frame (every tick).
#[inline]
pub fn publish_frame(m: &Mirror) {
    // SAFETY: single-caller (main loop, never an ISR); no reference is formed.
    unsafe {
        core::ptr::addr_of_mut!(CAST_MIRROR).write(m.clone());
    }
}

/// Gateway flush: read the shared cast frame under a closure (no `&'static mut`
/// escapes → `static_mut_refs`-clean).
#[inline]
pub fn with_frame<R>(f: impl FnOnce(&Mirror) -> R) -> R {
    // SAFETY: single-caller (flush runs on the main path, never an ISR); the shared
    // ref does not outlive the closure.
    unsafe { f(&*core::ptr::addr_of!(CAST_MIRROR)) }
}

/// mqtt_session: latch the HA cast-enable flag from the retained topic.
#[inline]
pub fn set_enabled(on: bool) {
    // SAFETY: single-caller (flush/main path, never an ISR); no reference is formed.
    unsafe {
        core::ptr::addr_of_mut!(CAST_ENABLED).write(on);
    }
}

/// Gateway flush: is casting currently enabled?
#[inline]
pub fn is_enabled() -> bool {
    // SAFETY: single-caller (flush/main path, never an ISR); no reference is formed.
    unsafe { core::ptr::addr_of!(CAST_ENABLED).read() }
}
