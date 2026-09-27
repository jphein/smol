//! tapstone#132 (c) — the shrine station's screens (tapstone decision 0032), on the S3's colour
//! panel through tapstone's `shrine-render`.
//!
//! `shrine-render` draws a whole 320×240 RGB565 screen into any `DrawTarget`. The firmware never
//! owns a full frame: [`Strip`] is a band of [`BAND_ROWS`] rows that presents itself as the whole
//! panel and keeps only its own rows, and each band goes out as one windowed `fill_contiguous`
//! (BOARD.md's strip-rasterise house rule). Rendering band by band is byte-identical to one pass;
//! tapstone's `shrine-preview/tests/banded.rs` holds that for every screen.
//!
//! What is drawn, from the seat's state (0032's screens):
//! 1. **Dark** while the arena has not been heard (screen 5, "the arena is gone").
//! 2. **Idle** in the lobby, the invitation (screen 1).
//! 3. **The station** in a match: paperdoll, wells, locator, and the voice band (screen 3). The
//!    band says an owed draw or a fallen commander's return, as `shrine-preview`'s `band_for`.
//! 4. **Result** once the game is over (screen 4, the game-level result).
//!
//! Redraws happen only when what a screen shows changes ([`Key`]); there is no animation yet.
// The S3's colour panel only; on any other chip this module is empty (main.rs gates it on the
// feature alone, which the tier-exclusion checker can model).
#![cfg(feature = "esp32s3")]

use core::fmt::Write as _;

use embedded_graphics::{pixelcolor::Rgb565, prelude::*, primitives::Rectangle};
use shrine_render::{
    commander::{Commander, Presence, Station},
    draws::DrawWhy,
    screens,
    sprite::Pose,
    station::{self, StationScreen},
    voice::{Dark, Voice},
};
use tapstone_rules::{Faction, Game, Phase};

use crate::s3_oled::Panel;

const W: u32 = 320;
const H: u32 = 240;
/// 24 rows × 320 px × 2 B = 15 KB of internal RAM; ten bands per screen.
pub const BAND_ROWS: u32 = 24;
/// A screen is repainted at most this often, so a burst of commits costs one paint.
const MIN_REPAINT_MS: u64 = 250;

/// One band of the panel, presented as the whole 320×240 surface.
struct Strip<'a> {
    buf: &'a mut [Rgb565; (W * BAND_ROWS) as usize],
    y0: i32,
}

impl OriginDimensions for Strip<'_> {
    fn size(&self) -> Size {
        Size::new(W, H)
    }
}

impl DrawTarget for Strip<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            let y = p.y - self.y0;
            if (0..W as i32).contains(&p.x) && (0..BAND_ROWS as i32).contains(&y) {
                self.buf[(y as u32 * W + p.x as u32) as usize] = c;
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let band = Rectangle::new(Point::new(0, self.y0), Size::new(W, BAND_ROWS));
        let a = area.intersection(&band);
        if a.size.width == 0 || a.size.height == 0 {
            return Ok(());
        }
        for y in a.top_left.y..a.top_left.y + a.size.height as i32 {
            let row = ((y - self.y0) as u32 * W) as usize;
            let x0 = a.top_left.x as usize;
            self.buf[row + x0..row + x0 + a.size.width as usize].fill(color);
        }
        Ok(())
    }
}

/// Which screen, and everything it shows: a repaint happens exactly when this changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Dark,
    Idle,
    Station { mseq: u16 },
    Result { match_id: u32 },
}

type Band = [Rgb565; (W * BAND_ROWS) as usize];

/// The one band buffer, in `.bss` rather than inside [`Screens`]: 15 KB built by value on the main
/// task's stack overflowed its guard (measured on glass, 2026-09-27: "write to the stack guard
/// value on ProCpu" at boot, both boards). Touched only by [`Screens::update`], which runs on the
/// single superloop task.
static mut BAND: Band = [Rgb565::BLACK; (W * BAND_ROWS) as usize];

pub struct Screens {
    shown: Option<Key>,
    last_paint: u64,
    faction: Faction,
    sigil: [u8; 24],
    sigil_len: usize,
}

impl Screens {
    pub fn new(node: u8, faction: Faction) -> Self {
        let mut sigil = [0u8; 24];
        let mut w = Buf { b: &mut sigil, n: 0 };
        let _ = write!(w, "shrine {}", node);
        let sigil_len = w.n;
        Self {
            shown: None,
            last_paint: 0,
            faction,
            sigil,
            sigil_len,
        }
    }

    /// Repaint if what the seat shows has changed. `arena_heard`: the station knows where the
    /// arena is. `seat`: this shrine's seat, once a `B` seats it.
    pub fn update(
        &mut self,
        panel: &mut Panel,
        now: u64,
        g: &Game,
        seat: Option<usize>,
        arena_heard: bool,
        mseq: u16,
        match_id: u32,
    ) {
        let key = match (arena_heard, seat, g.phase) {
            (false, _, _) => Key::Dark,
            (_, Some(_), Phase::Playing) => Key::Station { mseq },
            (_, Some(_), Phase::Over) => Key::Result { match_id },
            _ => Key::Idle,
        };
        if self.shown == Some(key) || now.saturating_sub(self.last_paint) < MIN_REPAINT_MS {
            return;
        }
        self.shown = Some(key);
        self.last_paint = now;
        let sigil_buf = self.sigil;
        let sigil = core::str::from_utf8(&sigil_buf[..self.sigil_len]).unwrap_or("shrine");
        let cmdr = Commander {
            name: sigil,
            faction: self.faction,
            xp: 0,
            loadout: [None; 3],
        };
        // SAFETY: the superloop task is the only caller, and nothing else names BAND.
        let band: &mut Band = unsafe { &mut *core::ptr::addr_of_mut!(BAND) };
        let t0 = esp_hal::time::Instant::now();
        let name = match key {
            Key::Dark => "dark",
            Key::Idle => "idle",
            Key::Station { .. } => "station",
            Key::Result { .. } => "result",
        };
        match key {
            Key::Dark => paint(band, panel, |d| station::dark(d, Some(&cmdr), Dark::ArenaGone)),
            Key::Idle => paint(band, panel, |d| station::idle(d, Some(&cmdr), sigil, Pose::default())),
            Key::Result { .. } => {
                let near = seat.unwrap_or(0) as u8;
                paint(band, panel, |d| screens::result(d, g, near, g.round, 0, sigil))
            }
            Key::Station { .. } => {
                let s = seat.unwrap_or(0) as u8;
                let Some(st) = Station::from_game(g, s) else {
                    return;
                };
                let sc = StationScreen {
                    cmdr,
                    st,
                    pose: Pose::default(),
                    voice: band_for(&st),
                    wells: None,
                };
                paint(band, panel, |d| station::station(d, &sc))
            }
        }
        esp_println::println!("[station] screen {} painted in {} ms", name, t0.elapsed().as_millis());
    }

}

/// Draw a whole screen band by band, one windowed write per band.
fn paint(band: &mut Band, panel: &mut Panel, draw: impl Fn(&mut Strip<'_>)) {
    for b in 0..H / BAND_ROWS {
        let y0 = (b * BAND_ROWS) as i32;
        {
            let mut strip = Strip { buf: band, y0 };
            draw(&mut strip);
        }
        let area = Rectangle::new(Point::new(0, y0), Size::new(W, BAND_ROWS));
        let _ = panel.fill_contiguous(&area, band.iter().copied());
    }
}

/// The band for an engine state (tapstone `shrine-preview`'s `band_for`): an owed draw first, since
/// the engine refuses everything else, then a fallen commander's return, then nothing.
fn band_for(st: &Station) -> Voice<'static> {
    if st.owed > 0 {
        let why = if st.round <= 1 {
            DrawWhy::Opening
        } else {
            DrawWhy::TurnStart
        };
        return Voice::Draw { n: st.owed, why };
    }
    match st.presence {
        Presence::Fallen { .. } => st.return_state().map(Voice::Return).unwrap_or(Voice::Silent),
        Presence::OnBoard { .. } => Voice::Silent,
    }
}

struct Buf<'a> {
    b: &'a mut [u8],
    n: usize,
}

impl core::fmt::Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let s = s.as_bytes();
        let k = s.len().min(self.b.len() - self.n);
        self.b[self.n..self.n + k].copy_from_slice(&s[..k]);
        self.n += k;
        Ok(())
    }
}
