//! Single-player Snake for the 72×40 SSD1306, `no_std` + heap-free.
//!
//! This is the SNAKE game mode of the unified firmware (see `src/menu.rs` for
//! the mode dispatcher). It is compiled into **every** build — it needs only
//! `embedded-graphics` + the display, no radio — so the default and `wifi`
//! builds are playable too; only BENCH is ESP-NOW-gated.
//!
//! ## Grid
//!
//! The panel is 72×40 px. We use **4×4 px cells**, giving an **18×10** grid.
//! The whole grid is offset down by [`Y_OFFSET`] px to leave the top row for a
//! compact `S:NN` score in `FONT_5X8`, so the playfield is 18×8 cells
//! (`PLAY_ROWS`). Cells are drawn as filled 4×4 rects; food is drawn hollow so
//! it reads differently from the snake body.
//!
//! ## Movement + input
//!
//! Movement is **millis-based**: the snake advances one cell every
//! [`STEP_MS`], independent of the (faster) render/poll rate, so speed is
//! constant regardless of frame timing. A **short tap** turns the snake
//! **clockwise** (Up→Right→Down→Left→Up) — a single-button control that can
//! reach any heading. Reversing directly into your own neck is impossible with
//! a clockwise-only turn from a 4-way heading, so no explicit anti-reverse guard
//! is needed.
//!
//! ## Rules
//!
//! Eating food grows the snake by one segment and spawns new food on a random
//! free cell; running into a wall or your own body ends the game. On death the
//! score is shown; a short tap restarts, and (handled by the dispatcher) a long
//! press exits to the Home menu.
//!
//! ## Randomness
//!
//! No RNG *peripheral* is used (it is consumed by esp-wifi in the radio builds),
//! so food placement uses a tiny software **xorshift32** PRNG seeded from the
//! monotonic millisecond clock at game start and re-stirred with the clock on
//! every food spawn. Not cryptographic — it doesn't need to be — just enough
//! spatial variety for food to feel random.

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

/// Cell size in pixels (square). 4 px divides 72 evenly (18 columns).
const CELL: i32 = 4;
/// Grid width in cells: 72 px / 4 = 18.
const COLS: u8 = 18;
/// Pixels reserved at the top for the score line (`FONT_5X8` is 8 px tall).
const Y_OFFSET: i32 = 8;
/// Playable rows below the score band: (40 − 8) px / 4 = 8.
const PLAY_ROWS: u8 = 8;
/// Maximum snake length = every playable cell (18 × 8 = 144). The body buffer is
/// sized to this so a perfect game can never overflow it.
const MAX_LEN: usize = (COLS as usize) * (PLAY_ROWS as usize);

/// Milliseconds per movement step. 220 ms ≈ 4.5 cells/s — brisk but playable
/// with a single turn button on a small grid.
const STEP_MS: u64 = 220;

/// A grid coordinate (col, row). Column 0..COLS, row 0..PLAY_ROWS.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    x: u8,
    y: u8,
}

/// The four headings, ordered CLOCKWISE so a "turn" is just `+1 (mod 4)`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Right,
    Down,
    Left,
}

impl Dir {
    /// Next heading when turning clockwise (the single-button control).
    fn clockwise(self) -> Dir {
        match self {
            Dir::Up => Dir::Right,
            Dir::Right => Dir::Down,
            Dir::Down => Dir::Left,
            Dir::Left => Dir::Up,
        }
    }

    /// (dx, dy) unit step for this heading in grid cells.
    fn delta(self) -> (i8, i8) {
        match self {
            Dir::Up => (0, -1),
            Dir::Right => (1, 0),
            Dir::Down => (0, 1),
            Dir::Left => (-1, 0),
        }
    }
}

/// Tiny non-cryptographic PRNG (xorshift32) for food placement.
struct Rng {
    state: u32,
}

impl Rng {
    /// Seed from the monotonic clock. Force non-zero (xorshift is stuck at 0).
    fn new(seed: u64) -> Self {
        let s = (seed as u32) ^ 0x9E37_79B9;
        Self {
            state: if s == 0 { 0xA5A5_A5A5 } else { s },
        }
    }

    /// Next pseudo-random u32.
    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Uniform-ish value in `0..bound` (bound > 0).
    fn below(&mut self, bound: u32) -> u32 {
        self.next_u32() % bound
    }
}

/// Snake game state. Fixed-capacity body ring (no heap): `body[..len]` holds the
/// live segments, `body[0]` is the head.
pub struct Snake {
    body: [Cell; MAX_LEN],
    len: usize,
    dir: Dir,
    /// A queued clockwise turn from a tap since the last step (applied at the
    /// next step so multiple taps between steps don't over-rotate confusingly).
    turn_queued: bool,
    food: Cell,
    score: u16,
    dead: bool,
    /// Monotonic time of the last movement step; next step at `+ STEP_MS`.
    last_step_ms: u64,
    rng: Rng,
}

impl Snake {
    /// Start a fresh game seeded from `now_ms` (used both to seed the PRNG and
    /// as the first step's time base). The snake starts length-3 near the middle
    /// heading right.
    pub fn new(now_ms: u64) -> Self {
        let mut rng = Rng::new(now_ms);
        let start = Cell {
            x: COLS / 2,
            y: PLAY_ROWS / 2,
        };
        let mut body = [start; MAX_LEN];
        // Three segments trailing to the LEFT of the head (we move right).
        body[0] = start;
        body[1] = Cell {
            x: start.x - 1,
            y: start.y,
        };
        body[2] = Cell {
            x: start.x - 2,
            y: start.y,
        };
        let len = 3;
        // Place the first food before moving `rng` into the struct.
        let food = random_free_cell(&body, len, &mut rng);
        Self {
            body,
            len,
            dir: Dir::Right,
            turn_queued: false,
            food,
            score: 0,
            dead: false,
            last_step_ms: now_ms,
            rng,
        }
    }

    /// Register a short-tap: queue one clockwise turn (ignored once dead — the
    /// dispatcher treats a tap on the death screen as "restart" instead).
    pub fn on_tap(&mut self) {
        if !self.dead {
            self.turn_queued = true;
        }
    }

    /// True once the snake has crashed (dispatcher shows score + waits for tap).
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Current score (food eaten), surfaced for the death screen.
    pub fn score(&self) -> u16 {
        self.score
    }

    /// Advance the simulation if a step is due at `now_ms`. Safe to call every
    /// render tick; it self-limits to one move per [`STEP_MS`]. No-op once dead.
    ///
    /// Returns `true` if the game state changed this call (a movement step
    /// happened — including the step that ends the game), so the caller only has
    /// to repaint the OLED when something actually moved rather than every tick.
    pub fn update(&mut self, now_ms: u64) -> bool {
        if self.dead {
            return false;
        }
        if now_ms.saturating_sub(self.last_step_ms) < STEP_MS {
            return false;
        }
        self.last_step_ms = now_ms;

        // Apply a queued turn (clockwise) exactly once per step.
        if self.turn_queued {
            self.dir = self.dir.clockwise();
            self.turn_queued = false;
        }

        // Compute the new head cell.
        let (dx, dy) = self.dir.delta();
        let head = self.body[0];
        let nx = head.x as i32 + dx as i32;
        let ny = head.y as i32 + dy as i32;

        // Wall collision -> death. (A step still occurred, so report `true`.)
        if nx < 0 || ny < 0 || nx >= COLS as i32 || ny >= PLAY_ROWS as i32 {
            self.dead = true;
            return true;
        }
        let new_head = Cell {
            x: nx as u8,
            y: ny as u8,
        };

        let ate = new_head == self.food;

        // Self collision: hitting any body cell EXCEPT the tail we're about to
        // vacate (unless we're growing, in which case the tail stays). Check
        // against the segments that will remain after the move.
        let occupied_len = if ate { self.len } else { self.len - 1 };
        for seg in &self.body[..occupied_len] {
            if *seg == new_head {
                self.dead = true;
                return true;
            }
        }

        // Shift body down by one (tail follows head) and insert the new head.
        // Growing appends a cell (len already < MAX_LEN because the grid can't
        // hold more segments than cells).
        if ate {
            self.len = (self.len + 1).min(MAX_LEN);
        }
        let mut i = self.len - 1;
        while i > 0 {
            self.body[i] = self.body[i - 1];
            i -= 1;
        }
        self.body[0] = new_head;

        if ate {
            self.score += 1;
            // Re-stir the PRNG with the clock so spawns don't fall into a fixed
            // sequence, then place food on a free cell. Split the borrow so the
            // occupancy scan (reads `body`/`len`) and the PRNG mutation don't
            // alias `self`: pull the state, spawn, write it back.
            self.rng.state ^= now_ms as u32;
            let mut rng = Rng {
                state: self.rng.state,
            };
            self.food = random_free_cell(&self.body, self.len, &mut rng);
            self.rng = rng;
        }
        true
    }

    /// Draw the current frame: score line, food (hollow), snake (filled).
    /// `display` is any `embedded-graphics` target (the SSD1306 buffer). Errors
    /// from individual draws are ignored — a dropped pixel on a 72×40 OLED is
    /// cosmetic and must never panic the firmware.
    pub fn draw<D>(&self, display: &mut D)
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let on = PrimitiveStyle::with_fill(BinaryColor::On);
        let outline = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
        let text = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::On)
            .build();

        // Score band (top 8 px): "S:NN".
        let mut buf = [0u8; 8];
        let s = fmt_score(self.score, &mut buf);
        Text::with_baseline(s, Point::new(1, 0), text, Baseline::Top)
            .draw(display)
            .ok();

        // Food: hollow 4×4 so it looks distinct from the solid body.
        Rectangle::new(cell_origin(self.food), Size::new(CELL as u32, CELL as u32))
            .into_styled(outline)
            .draw(display)
            .ok();

        // Snake body: filled 4×4 cells (head drawn same as body for simplicity).
        for seg in &self.body[..self.len] {
            Rectangle::new(cell_origin(*seg), Size::new(CELL as u32, CELL as u32))
                .into_styled(on)
                .draw(display)
                .ok();
        }
    }
}

/// Pick a random cell not occupied by `body[..len]`. Free function (not a
/// method) so the caller can borrow the PRNG mutably while the body is borrowed
/// immutably without aliasing `&mut Snake`. Falls back to a linear scan if random
/// probing keeps colliding (near-full board), so it always terminates.
fn random_free_cell(body: &[Cell; MAX_LEN], len: usize, rng: &mut Rng) -> Cell {
    // A handful of random probes first (fast in the common, sparse case).
    for _ in 0..32 {
        let c = Cell {
            x: rng.below(COLS as u32) as u8,
            y: rng.below(PLAY_ROWS as u32) as u8,
        };
        if !occupied(body, len, c) {
            return c;
        }
    }
    // Fallback: first free cell in scan order (board nearly full).
    for y in 0..PLAY_ROWS {
        for x in 0..COLS {
            let c = Cell { x, y };
            if !occupied(body, len, c) {
                return c;
            }
        }
    }
    // Board completely full (a win): keep food on the head; harmless.
    body[0]
}

/// Is `c` part of the live snake body `body[..len]`?
fn occupied(body: &[Cell; MAX_LEN], len: usize, c: Cell) -> bool {
    body[..len].contains(&c)
}

/// Top-left pixel of a grid cell, including the score-band vertical offset.
fn cell_origin(c: Cell) -> Point {
    Point::new(c.x as i32 * CELL, Y_OFFSET + c.y as i32 * CELL)
}

/// Format `"S:NN"` into `buf`, return the `&str`. Heap-free; score is small.
fn fmt_score(score: u16, buf: &mut [u8; 8]) -> &str {
    buf[0] = b'S';
    buf[1] = b':';
    // Up to 3 digits is plenty (max score 141); render without leading zeros.
    let mut n = score;
    if n > 999 {
        n = 999;
    }
    let mut tmp = [0u8; 3];
    let mut i = 0;
    if n == 0 {
        tmp[i] = b'0';
        i += 1;
    } else {
        // Build digits least-significant first, then reverse into buf.
        let mut m = n;
        while m > 0 {
            tmp[i] = b'0' + (m % 10) as u8;
            m /= 10;
            i += 1;
        }
    }
    let mut w = 2;
    for k in (0..i).rev() {
        buf[w] = tmp[k];
        w += 1;
    }
    core::str::from_utf8(&buf[..w]).unwrap_or("S:?")
}
