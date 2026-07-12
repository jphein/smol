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

// =========================================================================
// #74 stage-2 — display mirror: the SAME shadow glass, encoded as a tiny 1-bit
// BMP for HA to render as an `mqtt image`. Reuses the Cast tee (no new draw-path
// tap — the invasive per-plugin text approach was deliberately NOT taken), so a
// `cast` build gets the HA screen mirror for free alongside the WLED stream.
// =========================================================================
//
// Why a DOWNSAMPLED BMP (not the full 72×40, not raw bits):
//   * The gateway MQTT publish path is bounded to a 512 B packet buffer (`pkt` in
//     net/wifi.rs) — a full 72×40 1-bit BMP is 542 B raw / 724 B base64, too big.
//     A 64×32 BMP is 318 B raw / 424 B base64 → fits the packet with margin.
//   * BMP (not PBM/XBM) because browsers render `image/bmp` inline — HA serves the
//     bytes through its image proxy and a `<img>` in the dashboard shows it.
//   * base64 (not raw binary) keeps `smol/<id>/screen` a text topic (greppable via
//     mosquitto_sub, no binary-payload edge cases) — HA decodes with `image_encoding: b64`.
// The down-sample + 180° flip reuse [`cell_on`] (the WLED path's box-sampler), so the
// mirror matches the glass a viewer reads (the panel is mounted `Rotate180`).

/// Mirror image geometry — chosen so the 1-bit BMP fits the 512 B MQTT packet.
pub const SCREEN_W: usize = 64;
pub const SCREEN_H: usize = 32;
/// A target cell lights when ≥ this % of its (near-1:1) source block is lit. Low so
/// thin OLED text survives the mild 72×40→64×32 down-sample.
const SCREEN_THRESH_PCT: u32 = 25;
/// 1-bit BMP: 14 (file hdr) + 40 (info hdr) + 8 (2-colour palette) + rows. Each row is
/// `SCREEN_W` bits padded to a 4-byte boundary = 8 B for 64 px; 32 rows = 256 B.
const SCREEN_ROW_BYTES: usize = SCREEN_W.div_ceil(32) * 4; // 8
const SCREEN_BMP_LEN: usize = 62 + SCREEN_ROW_BYTES * SCREEN_H; // 318
/// base64 of the BMP: ceil(318/3)*4 = 424.
pub const SCREEN_B64_LEN: usize = SCREEN_BMP_LEN.div_ceil(3) * 4; // 424

/// Encode the shadow glass as a 64×32 1-bit BMP into `out` (must be ≥ [`SCREEN_BMP_LEN`]),
/// returning the byte length. PURE (no HAL / heap) — host-testable like [`pack_dnrgb`].
/// Bottom-up rows (positive `biHeight`) + `flip180` reproduce what a viewer reads off the
/// `Rotate180`-mounted glass. Down-sample via [`cell_on`] (shared with the WLED path).
pub fn pack_bmp1(m: &Mirror, out: &mut [u8]) -> usize {
    if out.len() < SCREEN_BMP_LEN {
        return 0;
    }
    for b in out[..SCREEN_BMP_LEN].iter_mut() {
        *b = 0;
    }
    // --- BITMAPFILEHEADER (14) ---
    out[0] = b'B';
    out[1] = b'M';
    out[2..6].copy_from_slice(&(SCREEN_BMP_LEN as u32).to_le_bytes());
    out[10..14].copy_from_slice(&62u32.to_le_bytes()); // pixel data offset
    // --- BITMAPINFOHEADER (40) ---
    out[14..18].copy_from_slice(&40u32.to_le_bytes());
    out[18..22].copy_from_slice(&(SCREEN_W as i32).to_le_bytes());
    out[22..26].copy_from_slice(&(SCREEN_H as i32).to_le_bytes()); // +H = bottom-up
    out[26..28].copy_from_slice(&1u16.to_le_bytes()); // planes
    out[28..30].copy_from_slice(&1u16.to_le_bytes()); // 1 bpp
    out[34..38].copy_from_slice(&((SCREEN_ROW_BYTES * SCREEN_H) as u32).to_le_bytes()); // biSizeImage
    out[46..50].copy_from_slice(&2u32.to_le_bytes()); // biClrUsed
    out[50..54].copy_from_slice(&2u32.to_le_bytes()); // biClrImportant
    // --- palette (8): idx0 = black (off), idx1 = white (lit). BMP order is B,G,R,0. ---
    out[58] = 0xFF;
    out[59] = 0xFF;
    out[60] = 0xFF;
    // --- pixels: 1 = lit. Bottom-up: file row r ↔ image row (H-1-r); MSB = leftmost px. ---
    let cfg = MatrixCfg {
        w: SCREEN_W,
        h: SCREEN_H,
        serpentine: false,
        flip180: true,
        thresh_pct: SCREEN_THRESH_PCT,
        on: [0, 0, 0],
        off: [0, 0, 0],
        timeout_s: 0,
    };
    for r in 0..SCREEN_H {
        let img_y = SCREEN_H - 1 - r;
        let row0 = 62 + r * SCREEN_ROW_BYTES;
        for x in 0..SCREEN_W {
            if cell_on(m, x, img_y, &cfg) {
                out[row0 + x / 8] |= 0x80u8 >> (x % 8);
            }
        }
    }
    SCREEN_BMP_LEN
}

/// Standard base64 (RFC 4648, `+`/`/`, `=` padded) of `src` into `out`, returning the
/// byte length written. PURE + panic-free (writes at most `out.len()` bytes; stops if
/// `out` is too small). Host-testable.
pub fn base64_into(src: &[u8], out: &mut [u8]) -> usize {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = 0;
    let mut i = 0;
    while i < src.len() {
        if o + 4 > out.len() {
            break;
        }
        let b0 = src[i];
        let b1 = if i + 1 < src.len() { src[i + 1] } else { 0 };
        let b2 = if i + 2 < src.len() { src[i + 2] } else { 0 };
        out[o] = A[(b0 >> 2) as usize];
        out[o + 1] = A[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize];
        out[o + 2] = if i + 1 < src.len() {
            A[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize]
        } else {
            b'='
        };
        out[o + 3] = if i + 2 < src.len() {
            A[(b2 & 0x3F) as usize]
        } else {
            b'='
        };
        o += 4;
        i += 3;
    }
    o
}

/// Fixed scratch for the mirror BMP + its base64, kept off the (bounded) MQTT-flush stack
/// in `.bss` — the same single-threaded main-path discipline as [`CAST_MIRROR`] (written
/// and read only from the flush, never an ISR; `addr_of[_mut]!` keeps `static_mut_refs` quiet).
static mut SCREEN_BMP: [u8; SCREEN_BMP_LEN] = [0; SCREEN_BMP_LEN];
static mut SCREEN_B64: [u8; SCREEN_B64_LEN] = [0; SCREEN_B64_LEN];

/// Gateway flush: build the current glass into a base64 BMP and hand it to `f` as a byte
/// slice (the retained `smol/<id>/screen` payload). Closure form so no `&'static mut`
/// escapes (`static_mut_refs`-clean), mirroring [`with_frame`].
pub fn with_screen_b64<R>(f: impl FnOnce(&[u8]) -> R) -> R {
    // SAFETY: single-caller (gateway flush on the main path, never an ISR); the three
    // statics are distinct (no aliasing) and no reference to them outlives this call.
    unsafe {
        let m = &*core::ptr::addr_of!(CAST_MIRROR);
        let bmp = &mut *core::ptr::addr_of_mut!(SCREEN_BMP);
        let blen = pack_bmp1(m, bmp);
        let b64 = &mut *core::ptr::addr_of_mut!(SCREEN_B64);
        let n = base64_into(&bmp[..blen], b64);
        f(&b64[..n])
    }
}
