//! MMO Mesh Snake (issue #5) — the espnow game mode.
//!
//! `core.rs` is the **vendored pure oracle** (proto crate, 49 host tests): all
//! toroidal math, the fixed-cap `Snake` ring, the `Camera`, the `PeerTable`
//! dead-reckoning, the SMOLv1-SNK wire codec, phase jitter, and the
//! deterministic `food_cells`/`treasure_at` spawns. This `mod.rs` is the ONLY
//! hand-written firmware glue: it binds the core to the ESP-NOW radio
//! (`net::mode`), the SSD1306 display, the mesh Unix clock, and the powers /
//! leaderboard game rules (design §11/§12). No heap — all state is fixed arrays
//! on the caller's stack (see `main`'s `Option<MeshSnake>`).
//!
//! Authorities: `mmo-snake-design.md` §4/§10/§11/§12, `mmo-snake-netcode.md`.

pub mod snake_core;

use ::core::fmt::Write as _;
use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

use snake_core::{
    food_cells, power_name, treasure_at, Camera, Cell, Dir, IngestOutcome, PeerTable, SnkFrame,
    StepOutcome, POWER_HASTE, POWER_MIDAS, POWER_PHANTOM, POWER_PHOENIX, POWER_SHIELD,
};

// World is the ratified 256×256 (design §10.1). Hardcoded as the const-generic
// args; `snake_core::WORLD_W/H` document the same values and stay the lever.
const W: u16 = 256;
const H: u16 = 256;

/// Game-wide food/treasure seed. A COMPILE-TIME CONSTANT shared by every board
/// (NOT per-node) — only the mesh-clock `bucket` needs to converge for spawns to
/// agree (design §10.4 / §6 eventually-consistent).
const GAME_SEED: u32 = 0x5340_4B45; // "S@KE"

/// Food bucket period (s) — team-lead decision (design §10.4 range). Treasure
/// is rarer (§11.3).
const FOOD_PERIOD_S: u32 = 20;
const TREASURE_PERIOD_S: u32 = 45; // team-lead: 45 (demo-visible end of 45–60)

/// Base step (ms) — design §10.7. Zephyr Haste is ~1.75× faster.
const STEP_MS: u32 = snake_core::STEP_MS;
const HASTE_STEP_MS: u32 = 114; // 200 / 1.75 ≈ 114

/// Per-power durations (s), design §11.1 first-pass (tunable).
const DUR_PHANTOM_S: u32 = 6;
const DUR_HASTE_S: u32 = 5;
const DUR_SHIELD_S: u32 = 10;
const DUR_MIDAS_S: u32 = 8;
const DUR_REVEAL_S: u32 = 10;
const DUR_PHOENIX_S: u32 = 10;

const START_LEN: usize = 3;
const CELL_PX: i32 = 4;

/// Local alias so this file reads against the design's "Zephyr Rune" name while
/// the core exports the effect name `POWER_HASTE`.
const POWER_ZEPHYR: u8 = POWER_HASTE;

/// The mesh-snake game state. ~600 B, entirely on the stack via `Option` in main.
pub struct MeshSnake {
    id: u8,
    snake: snake_core::Snake,
    peers: PeerTable,
    /// Wrapping broadcast tick (wire ordering).
    tick: u8,
    /// Monotonic ms of the last movement step.
    last_step_ms: u32,
    dead: bool,
    /// Active own power (0 = none) + clock-based expiry (Unix s).
    power: u8,
    power_until_unix: u32,
    /// Aegis Ward: one buffered lethal-hit charge.
    aegis_charged: bool,
    /// Phoenix Ember: one respawn-keeping-length remaining while active.
    phoenix_ready: bool,
    /// Local "already ate this food cell this bucket" guard (design §… race).
    ate_bucket: u32,
    ate_cell: Option<Cell>,
    /// Local "already took this treasure bucket" guard.
    took_treasure_bucket: Option<u32>,
    /// Flicker phase for the phantom render tell.
    frame: u32,
}

impl MeshSnake {
    /// Fresh game: length-3 snake near the world centre, heading East.
    pub fn new(id: u8, now_ms: u32) -> Self {
        let head = Cell::new(W / 2, H / 2);
        Self {
            id,
            snake: snake_core::Snake::new(head, Dir::East, START_LEN),
            peers: PeerTable::new(),
            tick: 0,
            last_step_ms: now_ms,
            dead: false,
            power: 0,
            power_until_unix: 0,
            aegis_charged: false,
            phoenix_ready: false,
            ate_bucket: u32::MAX,
            ate_cell: None,
            took_treasure_bucket: None,
            frame: 0,
        }
    }

    /// Queue a clockwise turn (the single-button control). No-op while dead.
    pub fn turn(&mut self) {
        if !self.dead {
            self.snake.turn_cw();
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Score ≡ length (design §12).
    pub fn score(&self) -> u16 {
        self.snake.len() as u16
    }

    fn active_power(&self, unix_now: u32) -> u8 {
        if self.power != 0 && unix_now < self.power_until_unix {
            self.power
        } else {
            0
        }
    }

    /// Respawn keeping the world centre, length reset to 3 (design §… death UX).
    /// Phoenix keeps current length instead (caller decides).
    pub fn respawn(&mut self, now_ms: u32, keep_len: usize) {
        let head = Cell::new(W / 2, H / 2);
        let len = keep_len.clamp(START_LEN, snake_core::SNAKE_CAP);
        self.snake = snake_core::Snake::new(head, Dir::East, len);
        self.dead = false;
        self.last_step_ms = now_ms;
        self.power = 0;
        self.aegis_charged = false;
        self.phoenix_ready = false;
    }

    /// Ingest a decoded peer frame (wire→state), stamping local receive time.
    pub fn ingest(&mut self, f: &SnkFrame, now_ms: u32) -> IngestOutcome {
        // Ignore our own broadcast echoed back.
        if f.id == self.id {
            return IngestOutcome::StaleTick;
        }
        self.peers.ingest(f, now_ms)
    }

    /// The frame to broadcast for our own snake THIS tick (advances `tick`).
    pub fn make_frame(&mut self, unix_now: u32) -> SnkFrame {
        self.tick = self.tick.wrapping_add(1);
        SnkFrame::new(
            self.id,
            self.tick,
            !self.dead,
            self.snake.heading(),
            self.snake.head(),
            self.snake.len().min(255) as u8,
        )
        .with_power(self.active_power(unix_now))
    }

    /// Advance the game. Returns `true` if the board changed (repaint hint).
    /// `now_ms` = local monotonic ms; `unix_now` = mesh Unix clock (buckets).
    pub fn update(&mut self, now_ms: u32, unix_now: u32) -> bool {
        self.frame = self.frame.wrapping_add(1);
        // Despawn stale peers (single-tier, §10.6).
        self.peers.prune(now_ms, snake_core::PEER_STALE_MS);

        if self.dead {
            return false;
        }
        let step_ms = if self.active_power(unix_now) == POWER_ZEPHYR {
            HASTE_STEP_MS
        } else {
            STEP_MS
        };
        if now_ms.saturating_sub(self.last_step_ms) < step_ms {
            return false;
        }
        self.last_step_ms = now_ms;

        let phantom = self.active_power(unix_now) == POWER_PHANTOM;
        // Peer bodies block us — EXCEPT peers who are themselves phantom (they're
        // non-lethal). While WE are phantom we pass through everything.
        let outcome = if phantom {
            self.snake.step::<W, H>(|_| false)
        } else {
            // Snapshot peers for the closure (borrow split).
            let peers = &self.peers;
            self.snake
                .step::<W, H>(|c| peer_body_hits(peers, c, now_ms))
        };

        match outcome {
            StepOutcome::Moved => {
                self.handle_pickups(unix_now);
                // Expire a lapsed power.
                if self.power != 0 && unix_now >= self.power_until_unix {
                    self.power = 0;
                }
                true
            }
            StepOutcome::Blocked => {
                if phantom {
                    // Phantom stalls at a self-crossing but never dies.
                    return false;
                }
                // Aegis absorbs one lethal hit.
                if self.aegis_charged {
                    self.aegis_charged = false;
                    self.power = 0;
                    return true;
                }
                // Phoenix: instant respawn keeping length (1×).
                if self.phoenix_ready {
                    let keep = self.snake.len();
                    self.respawn(now_ms, keep);
                    return true;
                }
                self.dead = true;
                true
            }
        }
    }

    /// Food / treasure pickup at the new head (both-get-it; §11.4 / §… food).
    fn handle_pickups(&mut self, unix_now: u32) {
        let head = self.snake.head();

        // Treasure (rarer bucket). One alive at a time.
        let tbucket = unix_now / TREASURE_PERIOD_S;
        let (tcell, tpower) = treasure_at::<W, H>(GAME_SEED, tbucket);
        if head == tcell && self.took_treasure_bucket != Some(tbucket) {
            self.took_treasure_bucket = Some(tbucket);
            self.grant_power(tpower, unix_now);
        }

        // Food (K beacons). +1 length, or +3 under Midas (§11.1 Midas Sigil).
        let fbucket = unix_now / FOOD_PERIOD_S;
        let mut beacons = [Cell::default(); snake_core::FOOD_COUNT];
        let n = food_cells::<W, H>(GAME_SEED, fbucket, &mut beacons);
        let on_food = beacons[..n].contains(&head);
        let already = self.ate_bucket == fbucket && self.ate_cell == Some(head);
        if on_food && !already {
            self.ate_bucket = fbucket;
            self.ate_cell = Some(head);
            let grow = if self.active_power(unix_now) == POWER_MIDAS {
                3
            } else {
                1
            };
            self.snake.grow(grow);
        }
    }

    fn grant_power(&mut self, power: u8, unix_now: u32) {
        self.power = power;
        let dur = match power {
            POWER_PHANTOM => DUR_PHANTOM_S,
            POWER_ZEPHYR => DUR_HASTE_S,
            POWER_SHIELD => DUR_SHIELD_S,
            POWER_MIDAS => DUR_MIDAS_S,
            POWER_PHOENIX => DUR_PHOENIX_S,
            _ => DUR_REVEAL_S,
        };
        self.power_until_unix = unix_now + dur;
        if power == POWER_SHIELD {
            self.aegis_charged = true;
        }
        if power == POWER_PHOENIX {
            self.phoenix_ready = true;
        }
    }

    /// Our 1-based rank on the length leaderboard (desc length, ties id asc).
    fn rank(&self) -> u8 {
        let my_len = self.snake.len() as u16;
        let mut rank: u8 = 1;
        for p in self.peers.active() {
            if !p.alive {
                continue;
            }
            // A peer outranks us if longer, or equal length with a smaller id.
            if p.length > my_len || (p.length == my_len && p.id < self.id) {
                rank = rank.saturating_add(1);
            }
        }
        rank
    }

    // ---- render -----------------------------------------------------------

    /// Draw the full-bleed 18×10 world view + inverse-pill HUD (design §4/§10).
    pub fn draw<D>(&self, display: &mut D, now_ms: u32, unix_now: u32)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Death screen is a dedicated leaderboard view (design §12), not an
        // overlay on the world — clearer on the 72×40 mono OLED.
        if self.dead {
            self.draw_death(display);
            return;
        }
        let fill = PrimitiveStyle::with_fill(BinaryColor::On);
        let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

        let head = self.snake.head();
        let cam = Camera::centered_on::<W, H>(head);

        // --- Food: hollow 2×2 (design §4 "smaller than a body cell"). ---------
        let fbucket = unix_now / FOOD_PERIOD_S;
        let mut beacons = [Cell::default(); snake_core::FOOD_COUNT];
        let n = food_cells::<W, H>(GAME_SEED, fbucket, &mut beacons);
        for &c in &beacons[..n] {
            if let Some((px, py)) = cam.world_to_screen::<W, H>(c) {
                Rectangle::new(Point::new(px as i32, py as i32), Size::new(2, 2))
                    .into_styled(fill)
                    .draw(display)
                    .ok();
            }
        }

        // --- Treasure: a small plus/star glyph (design §4 star). --------------
        let tbucket = unix_now / TREASURE_PERIOD_S;
        if self.took_treasure_bucket != Some(tbucket) {
            let (tcell, _) = treasure_at::<W, H>(GAME_SEED, tbucket);
            if let Some((px, py)) = cam.world_to_screen::<W, H>(tcell) {
                draw_star(display, px as i32, py as i32, fill);
            }
        }

        // --- Peers: hollow body, solid head; phantom flickers (§11.2). --------
        let mut buf = [Cell::default(); snake_core::SNAKE_CAP];
        for p in self.peers.active() {
            if !p.alive {
                continue;
            }
            // Phantom peer: flicker ~3 Hz (skip drawing on alternate frames).
            if p.power == POWER_PHANTOM && (self.frame / 4).is_multiple_of(2) {
                continue;
            }
            let m = p.body_cells::<W, H>(now_ms, STEP_MS, &mut buf);
            for (i, &c) in buf[..m].iter().enumerate() {
                if let Some((px, py)) = cam.world_to_screen::<W, H>(c) {
                    let style = if i == 0 { fill } else { outline };
                    Rectangle::new(
                        Point::new(px as i32, py as i32),
                        Size::new(CELL_PX as u32, CELL_PX as u32),
                    )
                    .into_styled(style)
                    .draw(display)
                    .ok();
                }
            }
        }

        // --- Self: solid body + head (head gets a power tell). ----------------
        let phantom = self.active_power(unix_now) == POWER_PHANTOM;
        for (i, c) in self.snake.segments().enumerate() {
            // Phantom self flicker.
            if phantom && (self.frame / 4).is_multiple_of(2) {
                break;
            }
            if let Some((px, py)) = cam.world_to_screen::<W, H>(c) {
                Rectangle::new(
                    Point::new(px as i32, py as i32),
                    Size::new(CELL_PX as u32, CELL_PX as u32),
                )
                .into_styled(fill)
                .draw(display)
                .ok();
                if i == 0 {
                    // head power tell: a 1px halo ring for shield, else already solid.
                    if self.active_power(unix_now) == POWER_SHIELD {
                        Rectangle::new(
                            Point::new(px as i32 - 1, py as i32 - 1),
                            Size::new((CELL_PX + 2) as u32, (CELL_PX + 2) as u32),
                        )
                        .into_styled(outline)
                        .draw(display)
                        .ok();
                    }
                }
            }
        }

        // --- HUD: inverse-video pill "#R L:NN P:x" (design §10.2/§12). ---------
        self.draw_hud(display, unix_now);
    }

    /// Top-3 `(id, length)` on the leaderboard — length desc, ties id asc, over
    /// own + live peers (design §12). No alloc: gather into a fixed buffer,
    /// select the best 3. Returns the filled slots and the count (1..=3).
    fn leaderboard_top3(&self) -> ([(u8, u16); 3], usize) {
        const CAP: usize = 1 + snake_core::PEER_CAP;
        let mut cand = [(0u8, 0u16); CAP];
        let mut nc = 0;
        cand[nc] = (self.id, self.snake.len() as u16);
        nc += 1;
        for p in self.peers.active() {
            if p.alive && nc < CAP {
                cand[nc] = (p.id, p.length);
                nc += 1;
            }
        }
        let mut out = [(0u8, 0u16); 3];
        let mut used = [false; CAP];
        let take = nc.min(3);
        for slot in out.iter_mut().take(take) {
            let mut best: Option<usize> = None;
            for (i, &(cid, clen)) in cand[..nc].iter().enumerate() {
                if used[i] {
                    continue;
                }
                match best {
                    None => best = Some(i),
                    Some(b) => {
                        let (bid, blen) = cand[b];
                        if clen > blen || (clen == blen && cid < bid) {
                            best = Some(i);
                        }
                    }
                }
            }
            let bi = best.unwrap_or(0);
            used[bi] = true;
            *slot = cand[bi];
        }
        (out, take)
    }

    /// Death screen: "DEAD tap" + the top-3 leaderboard with magical nouns
    /// (design §12). Our own row shows even if we're not top-3? No — top-3 only,
    /// per §12; the HUD carried our live rank while playing.
    fn draw_death<D>(&self, display: &mut D)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        draw_center_banner(display, "DEAD  tap");
        let (top, n) = self.leaderboard_top3();
        let style = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::On)
            .build();
        for (i, &(id, len)) in top[..n].iter().enumerate() {
            let noun = crate::net::names::name_for_id(id).1;
            let mut line = heapless_line::Line::new();
            // "1.Noun NN" — a '*' marks our own entry.
            let me = if id == self.id { "*" } else { "" };
            let _ = write!(line, "{}.{}{} {}", i + 1, me, noun, len);
            let y = 12 + i as i32 * 9;
            Text::with_baseline(line.as_str(), Point::new(1, y), style, Baseline::Top)
                .draw(display)
                .ok();
        }
    }

    fn draw_hud<D>(&self, display: &mut D, unix_now: u32)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let mut s: heapless_line::Line = heapless_line::Line::new();
        let p = self.active_power(unix_now);
        // "#R L:NN" (score ≡ length, §12) + optional power initial.
        let _ = write!(s, "#{} L:{}", self.rank(), self.score());
        if p != 0 {
            let _ = write!(s, " {}", power_initial(p));
        }
        // Inverse pill: filled bar + off-text, top-left, ~ one 5x8 row.
        let text_len = s.as_str().len() as i32;
        let w = (text_len * 6 + 3).clamp(0, 72) as u32;
        Rectangle::new(Point::new(0, 0), Size::new(w, 9))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(display)
            .ok();
        let style = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::Off)
            .build();
        Text::with_baseline(s.as_str(), Point::new(2, 1), style, Baseline::Top)
            .draw(display)
            .ok();
    }
}

/// True if `c` lies on any NON-phantom live peer's dead-reckoned body.
fn peer_body_hits(peers: &PeerTable, c: Cell, now_ms: u32) -> bool {
    let mut buf = [Cell::default(); snake_core::SNAKE_CAP];
    for p in peers.active() {
        if !p.alive || p.power == POWER_PHANTOM {
            continue; // dead or phantom peers are non-lethal
        }
        let m = p.body_cells::<W, H>(now_ms, STEP_MS, &mut buf);
        if buf[..m].contains(&c) {
            return true;
        }
    }
    false
}

/// One-letter power tell for the HUD.
fn power_initial(power: u8) -> &'static str {
    // First letter of the design §11.1 name.
    match power_name(power).as_bytes().first() {
        Some(b'W') => "W", // Wraith Veil (Phantom)
        Some(b'Z') => "Z", // Zephyr Rune (Haste)
        Some(b'A') => "A", // Aegis Ward (Shield)
        Some(b'M') => "M", // Midas / Mothlight — disambiguated below
        Some(b'P') => "P", // Phoenix Ember
        _ => "?",
    }
}

/// A small 3×3 plus/star glyph at (x,y) for treasures.
fn draw_star<D>(display: &mut D, x: i32, y: i32, style: PrimitiveStyle<BinaryColor>)
where
    D: DrawTarget<Color = BinaryColor>,
{
    // vertical + horizontal 1px bars forming a plus inside a 4×4 cell.
    Rectangle::new(Point::new(x + 1, y), Size::new(1, 4))
        .into_styled(style)
        .draw(display)
        .ok();
    Rectangle::new(Point::new(x, y + 1), Size::new(4, 1))
        .into_styled(style)
        .draw(display)
        .ok();
}

/// Centre a short banner (death screen).
fn draw_center_banner<D>(display: &mut D, msg: &str)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let w = (msg.len() as i32 * 6 + 3).clamp(0, 72) as u32;
    let x = (72 - w as i32) / 2;
    Rectangle::new(Point::new(x, 15), Size::new(w, 10))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(display)
        .ok();
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::Off)
        .build();
    Text::with_baseline(msg, Point::new(x + 2, 16), style, Baseline::Top)
        .draw(display)
        .ok();
}

/// Tiny heap-free formatted line (the firmware uses `alloc`, but the game path
/// stays allocation-free per the DoD).
mod heapless_line {
    use ::core::fmt;

    pub struct Line {
        buf: [u8; 24],
        len: usize,
    }

    impl Line {
        pub fn new() -> Self {
            Self { buf: [0; 24], len: 0 }
        }
        pub fn as_str(&self) -> &str {
            ::core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
        }
    }

    impl fmt::Write for Line {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            for &b in s.as_bytes() {
                if self.len < self.buf.len() {
                    self.buf[self.len] = b;
                    self.len += 1;
                }
            }
            Ok(())
        }
    }
}
