// VENDORED VERBATIM from scratch/smol/mmo-snake-proto/src/lib.rs (the 52-test oracle).
// Firmware copy: crate attr + #[cfg(test)] module stripped; logic byte-identical.
// Re-vendor on any proto change and diff to prove no drift. Do NOT hand-edit logic here.
#![allow(dead_code)] // not every core API is used by the firmware yet (powers/leaderboard)

//! # mmo_snake_core — pure game core for smol MMO mesh snake
//!
//! `no_std` + `core`-only (no `std`, no `alloc`). Every collection is a
//! fixed-capacity array with a documented cap, so this module drops verbatim
//! into the ESP32-C3 `no_std` firmware. The test harness below uses `std`
//! (host-only), which is why the crate attribute is
//! `#![cfg_attr(not(test), no_std)]` — `no_std` for the firmware build,
//! `std` back for `cargo test`.
//!
//! ## Scope
//! Pure logic ONLY: no radio, no display, no `esp-hal`. The firmware supplies
//! time (`millis()`), the ESP-NOW peer feed, and the SSD1306 blit; this module
//! owns the *rules*.
//!
//! ## What lives here
//! - [`World`] — toroidal grid + wrapping coordinate math (const-generic dims).
//! - [`Snake`] — fixed-cap segment ring buffer, single-button clockwise turn,
//!   [`Snake::step`] with self + external collision.
//! - [`Camera`] — world→72×40 screen mapping at 4 px/cell, seam-aware.
//! - [`PeerSnake`] / [`PeerTable`] — dead-reckoned remote snakes, stale despawn.
//! - [`food_at`] — deterministic food as a pure fn of `(seed, time_bucket)`.
//!
//! ## Assumptions (design/netcode specs were not yet written at build time)
//! - World is toroidal, u8-range coords. Default [`WORLD_W`]×[`WORLD_H`] is
//!   **128×128** per the latest directive (design doc §10.1 still says 256 —
//!   conflict flagged; it's a const-generic knob, tested at both sizes).
//! - Viewport is **72×40 px @ 4 px/cell** ⇒ [`VIEW_COLS`]×[`VIEW_ROWS`] = 18×10
//!   cells, camera **centered** on the local head (deadzone is a trivial
//!   extension — see [`Camera::centered_on`]).
//! - Peer reconstruction is **head + heading + length dead-reckoning** with a
//!   **straight-line body approximation** between updates (no turn history on
//!   the wire). Documented so the netcode author can upgrade the encoding.
//! - Food **eaten-race**: honest default — both eaters grow, the cell simply
//!   changes on the next `time_bucket`. No authority handshake needed because
//!   food position is a pure function every node computes identically.


// ===========================================================================
// Shipped constants (change these two to reshape the world; math is generic)
// ===========================================================================

/// Shipped world width (cells).
///
/// **256×256, RATIFIED** by design doc §10.1 (which completed its ratify pass
/// and kept the proto's original 256 — every `u8` coord valid, no masking,
/// matches netcode). The earlier 128 flip-flop is resolved → 256 stands. Still
/// const-generic over `World<W,H>`, so shrinking to 128/64 remains a one-line
/// lever (§10.1) — tests exercise both sizes so correctness is size-agnostic.
pub const WORLD_W: u16 = 256;
/// Shipped world height (cells). See [`WORLD_W`].
pub const WORLD_H: u16 = 256;

/// OLED usable width in pixels (0.42" SSD1306 visible area on this board).
pub const SCREEN_W_PX: u16 = 72;
/// OLED usable height in pixels.
pub const SCREEN_H_PX: u16 = 40;
/// Pixels per world cell when rendered.
pub const CELL_PX: u16 = 4;

/// Visible columns of cells (72 / 4 = 18).
pub const VIEW_COLS: u16 = SCREEN_W_PX / CELL_PX;
/// Visible rows of cells (40 / 4 = 10).
pub const VIEW_ROWS: u16 = SCREEN_H_PX / CELL_PX;

/// Max segments a snake body can hold (ring-buffer capacity).
pub const SNAKE_CAP: usize = 64;
/// Max simultaneously-tracked remote peers.
pub const PEER_CAP: usize = 16;

/// Local snake movement step period (ms). Dead-reckoning converts elapsed
/// `millis()` into whole steps with this. **Design §10.7 FINAL = 200** (aligns
/// one broadcast per step at 5 Hz; proto had 150, netcode 220 — 200 is final).
pub const STEP_MS: u32 = 200;
/// Broadcast period (ms) at R = 5 Hz — one state broadcast per step (== STEP_MS).
pub const BROADCAST_PERIOD_MS: u32 = 200;
/// No frame within this window (ms) ⇒ peer is **despawned** by
/// [`PeerTable::prune`]. Design §10.6 RATIFIES the proto's **single-tier 5000**
/// (netcode suggested a two-tier 3000/6000; design keeps one proven tunable).
pub const PEER_STALE_MS: u32 = 5_000;
/// OPTIONAL render-only dim threshold (ms) — cosmetic, NOT a protocol tier
/// (§10.6). A peer un-heard this long may be drawn dimmed/ghosted but stays
/// live until [`PEER_STALE_MS`]. Feed to [`PeerSnake::state`] if you want it.
pub const PEER_DIM_MS: u32 = 2_500;

// --- SMOLv1 SNK wire frame (netcode spec §1) -------------------------------
/// ASCII, sniffer-greppable frame prefix. Exactly 11 bytes.
pub const SNK_PREFIX: &[u8; 11] = b"SMOLv1 SNK ";
/// Version 1 frame: 18 B core, no score byte (leaderboard score == length).
pub const SNK_VER: u8 = 1;
/// Version 2 frame: 18 B core + 1 B explicit `score` (leaderboard feature).
pub const SNK_VER_SCORE: u8 = 2;
/// Total on-wire length of a v1 frame: 11 B prefix + 7 B binary core.
pub const SNK_FRAME_LEN: usize = 18;
/// Total on-wire length of a v2 frame: v1 core + 1 B score.
pub const SNK_FRAME_LEN_V2: usize = 19;

// --- flags byte bit layout (design v2: powers + leaderboard) ---------------
// Named masks so a re-carve (e.g. design v2 splitting off a respawn bit) is a
// one-line change, not a bit-twiddle hunt.
//
//   bit 0     alive
//   bits 1-2  heading (0=U/North, 1=R/East, 2=D/South, 3=L/West)
//   bits 3-7  active power (0 = none, 1..=31 = power id)  ← claims the netcode
//             spec's old bit3 "boost" + the design's "respawn/spare" bits.
/// `alive` occupies bit 0.
pub const FLAG_ALIVE_MASK: u8 = 0b0000_0001;
/// `heading` occupies bits 1-2.
pub const FLAG_HEADING_SHIFT: u8 = 1;
/// 2-bit heading mask (pre-shift).
pub const FLAG_HEADING_MASK: u8 = 0b11;
/// `power` occupies bits 3-7.
pub const FLAG_POWER_SHIFT: u8 = 3;
/// 5-bit power mask (pre-shift): values 0..=31.
pub const FLAG_POWER_MASK: u8 = 0b0001_1111;
/// Largest representable active-power id (5 bits).
pub const POWER_MAX: u8 = 31;

// --- Authoritative active-power ids (design §11.1; 0 = none, 1..=6 used) ----
/// No active power.
pub const POWER_NONE: u8 = 0;
/// Wraith Veil — phase through all bodies (non-lethal both ways). Wire-critical.
pub const POWER_PHANTOM: u8 = 1;
/// Zephyr Rune — ~1.75× step speed.
pub const POWER_HASTE: u8 = 2;
/// Aegis Ward — absorb the next lethal hit (1 charge).
pub const POWER_SHIELD: u8 = 3;
/// Midas Sigil — food yields +3 length (keeps score≡length).
pub const POWER_MIDAS: u8 = 4;
/// Mothlight Lantern — compass reveals all treasures + nearest peers.
pub const POWER_REVEAL: u8 = 5;
/// Phoenix Ember — die-while-active ⇒ instant respawn keeping length (1×).
pub const POWER_PHOENIX: u8 = 6;
/// Number of defined powers (ids 1..=6).
pub const POWER_COUNT: u8 = 6;

/// Human-readable name for an active-power id (design §11.1). Unknown/reserved
/// ids (7..=31) return `"reserved"`; forward-tolerant, never panics.
pub const fn power_name(power: u8) -> &'static str {
    match power {
        POWER_NONE => "none",
        POWER_PHANTOM => "Wraith Veil",
        POWER_HASTE => "Zephyr Rune",
        POWER_SHIELD => "Aegis Ward",
        POWER_MIDAS => "Midas Sigil",
        POWER_REVEAL => "Mothlight Lantern",
        POWER_PHOENIX => "Phoenix Ember",
        _ => "reserved",
    }
}

// --- Phase jitter (netcode spec §2 — mandatory above N≈8) ------------------
/// Design-target peer count; also the number of phase slots.
pub const PHASE_NMAX: u8 = 16;

// ===========================================================================
// Cell + Direction
// ===========================================================================

/// A world cell. Coordinates are always stored **canonically wrapped** into
/// `0..W` × `0..H` (every constructor that can move a cell goes through
/// [`World::add`]), so `==` is a valid same-cell test.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Cell {
    pub x: u16,
    pub y: u16,
}

impl Cell {
    #[inline]
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// Heading. The only control is a single button that rotates **clockwise**:
/// `North → East → South → West → North`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Dir {
    #[default]
    North,
    East,
    South,
    West,
}

impl Dir {
    /// The single-button control: rotate 90° clockwise.
    #[inline]
    pub const fn clockwise(self) -> Self {
        match self {
            Dir::North => Dir::East,
            Dir::East => Dir::South,
            Dir::South => Dir::West,
            Dir::West => Dir::North,
        }
    }

    /// 180° reversal (used to lay a straight body out behind a head).
    #[inline]
    pub const fn opposite(self) -> Self {
        self.clockwise().clockwise()
    }

    /// Per-step delta in (dx, dy). y grows downward (screen convention);
    /// `North` decreases y.
    #[inline]
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Dir::North => (0, -1),
            Dir::East => (1, 0),
            Dir::South => (0, 1),
            Dir::West => (-1, 0),
        }
    }

    /// Wire encoding for the `flags` field: `0=U(p) 1=R 2=D 3=L` (netcode
    /// spec §1). Written explicitly rather than relying on the enum
    /// discriminant so the wire contract can't drift if variants are reordered.
    #[inline]
    pub const fn to_bits(self) -> u8 {
        match self {
            Dir::North => 0, // U
            Dir::East => 1,  // R
            Dir::South => 2, // D
            Dir::West => 3,  // L
        }
    }

    /// Decode a 2-bit wire heading (masks to the low 2 bits, so it is total).
    #[inline]
    pub const fn from_bits(b: u8) -> Self {
        match b & 0b11 {
            0 => Dir::North,
            1 => Dir::East,
            2 => Dir::South,
            _ => Dir::West, // 3
        }
    }
}

// ===========================================================================
// World — toroidal grid + wrapping coordinate math (const-generic dims)
// ===========================================================================

/// Zero-sized namespace carrying the grid dimensions as const generics.
///
/// The firmware uses [`World`]`<`[`WORLD_W`]`, `[`WORLD_H`]`>`; tests use small
/// worlds (e.g. `World<8, 8>`) to exercise edge/seam behavior directly. No data
/// is stored — const generics don't need to appear in a field.
pub struct World<const W: u16, const H: u16>;

impl<const W: u16, const H: u16> World<W, H> {
    /// Wrap a (possibly negative / oversized) x back into `0..W`.
    #[inline]
    pub fn wrap_x(x: i32) -> u16 {
        x.rem_euclid(W as i32) as u16
    }

    /// Wrap a (possibly negative / oversized) y back into `0..H`.
    #[inline]
    pub fn wrap_y(y: i32) -> u16 {
        y.rem_euclid(H as i32) as u16
    }

    /// Move `c` by `(dx, dy)` with toroidal wrap. This is THE constructor for
    /// any moved cell, guaranteeing canonical coordinates.
    #[inline]
    pub fn add(c: Cell, dx: i32, dy: i32) -> Cell {
        Cell {
            x: Self::wrap_x(c.x as i32 + dx),
            y: Self::wrap_y(c.y as i32 + dy),
        }
    }

    /// Forward (non-negative) distance from `b` to `a` along +x, mod W.
    /// Equivalent to `(a - b) mod W`. This is the value a viewport uses to
    /// place a cell relative to the camera origin — it wraps across the seam.
    #[inline]
    pub fn forward_x(a: u16, b: u16) -> u16 {
        (a as i32 - b as i32).rem_euclid(W as i32) as u16
    }

    /// Forward (non-negative) distance from `b` to `a` along +y, mod H.
    #[inline]
    pub fn forward_y(a: u16, b: u16) -> u16 {
        (a as i32 - b as i32).rem_euclid(H as i32) as u16
    }

    /// Shortest wrapped distance along x (min of the two arcs). `0..=W/2`.
    #[inline]
    pub fn wrap_dist_x(a: u16, b: u16) -> u16 {
        let d = Self::forward_x(a, b);
        d.min(W - d)
    }

    /// Shortest wrapped distance along y (min of the two arcs). `0..=H/2`.
    #[inline]
    pub fn wrap_dist_y(a: u16, b: u16) -> u16 {
        let d = Self::forward_y(a, b);
        d.min(H - d)
    }

    /// Toroidal Manhattan distance between two cells.
    #[inline]
    pub fn manhattan(a: Cell, b: Cell) -> u32 {
        Self::wrap_dist_x(a.x, b.x) as u32 + Self::wrap_dist_y(a.y, b.y) as u32
    }
}

// ===========================================================================
// Snake — fixed-cap segment ring buffer
// ===========================================================================

/// Result of a [`Snake::step`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepOutcome {
    /// Head advanced one cell; no collision.
    Moved,
    /// Move was blocked (self-collision or an externally-occupied cell). The
    /// snake was **not** advanced — the caller decides death/respawn.
    Blocked,
}

/// A player snake as a ring buffer of body cells.
///
/// Segment 0 is the head. Body segment `i` (for `i` in `0..len`) lives at ring
/// index `(head_idx + SNAKE_CAP - i) % SNAKE_CAP`. `len` is the current body
/// length; `target_len` is what it's growing toward. Capacity [`SNAKE_CAP`].
#[derive(Clone, Copy)]
pub struct Snake {
    seg: [Cell; SNAKE_CAP],
    head_idx: usize,
    len: usize,
    target_len: usize,
    heading: Dir,
}

impl Snake {
    /// Spawn a coherent snake: `head` plus a straight body of `len` cells laid
    /// out *behind* the head (opposite `heading`). `len` is clamped to
    /// [`SNAKE_CAP`] and floored at 1.
    pub fn new(head: Cell, heading: Dir, len: usize) -> Self {
        let len = len.clamp(1, SNAKE_CAP);
        let mut seg = [head; SNAKE_CAP];
        let (bdx, bdy) = heading.opposite().delta();
        // Fill the ring so that reading `i` back from head_idx=0 yields the
        // straight body. Segment i sits at index (SNAKE_CAP - i) % SNAKE_CAP.
        for i in 0..len {
            let idx = (SNAKE_CAP - i) % SNAKE_CAP;
            seg[idx] = World::<WORLD_W, WORLD_H>::add(head, bdx * i as i32, bdy * i as i32);
        }
        Self {
            seg,
            head_idx: 0,
            len,
            target_len: len,
            heading,
        }
    }

    /// Current head cell.
    #[inline]
    pub fn head(&self) -> Cell {
        self.seg[self.head_idx]
    }

    /// Current heading.
    #[inline]
    pub fn heading(&self) -> Dir {
        self.heading
    }

    /// Current body length (number of occupied cells).
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Length the snake is growing toward.
    #[inline]
    pub fn target_len(&self) -> usize {
        self.target_len
    }

    /// True if the body is empty. (Present so clippy is happy alongside `len`;
    /// a live snake is never empty.)
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The single-button control: turn 90° clockwise.
    #[inline]
    pub fn turn_cw(&mut self) {
        self.heading = self.heading.clockwise();
    }

    /// Queue growth of `n` cells (clamped so `target_len ≤ SNAKE_CAP`).
    #[inline]
    pub fn grow(&mut self, n: usize) {
        self.target_len = (self.target_len + n).min(SNAKE_CAP);
    }

    /// Iterate body cells head→tail (segment 0 = head).
    pub fn segments(&self) -> impl Iterator<Item = Cell> + '_ {
        (0..self.len).map(move |i| self.seg[(self.head_idx + SNAKE_CAP - i) % SNAKE_CAP])
    }

    /// Iterate only the first `n` segments from the head.
    fn first_segments(&self, n: usize) -> impl Iterator<Item = Cell> + '_ {
        (0..n.min(self.len)).map(move |i| self.seg[(self.head_idx + SNAKE_CAP - i) % SNAKE_CAP])
    }

    /// Advance the head one cell in the current heading, on a `W`×`H` torus.
    ///
    /// `blocked(cell)` reports externally-occupied cells (peer bodies, walls);
    /// self-collision is checked internally. On a block the snake is left
    /// unchanged and [`StepOutcome::Blocked`] is returned. Moving into the cell
    /// the tail is vacating is allowed (classic snake rule) — unless the snake
    /// is growing this tick, in which case the tail stays and the cell is
    /// occupied.
    #[inline]
    pub fn step<const W: u16, const H: u16>(
        &mut self,
        blocked: impl Fn(Cell) -> bool,
    ) -> StepOutcome {
        self.step_impl::<W, H>(blocked, false)
    }

    /// Advance while **phasing through the snake's own body** — the Wraith Veil
    /// power (design §11.1 Phantom): self-collision is NOT lethal, so the head
    /// may pass over its own segments. The external `blocked` closure still
    /// applies (the game gates peer bodies there — passing `|_| false` also
    /// phases through peers, which is the phantom contract).
    ///
    /// **Phasing-expiry rule** (power ends while the head overlaps the body):
    /// nothing special is needed — only the cell you are ABOUT TO ENTER is ever
    /// tested (never the current head's overlap). So a plain [`Snake::step`]
    /// after the power lapses lets you survive on top of your own body until you
    /// would step INTO an occupied cell. See `phasing_expiry_only_entered_cell_kills`.
    #[inline]
    pub fn step_phasing<const W: u16, const H: u16>(
        &mut self,
        blocked: impl Fn(Cell) -> bool,
    ) -> StepOutcome {
        self.step_impl::<W, H>(blocked, true)
    }

    /// Shared step core. `phase_self` skips the self-collision check (Phantom).
    fn step_impl<const W: u16, const H: u16>(
        &mut self,
        blocked: impl Fn(Cell) -> bool,
        phase_self: bool,
    ) -> StepOutcome {
        let (dx, dy) = self.heading.delta();
        let new_head = World::<W, H>::add(self.head(), dx, dy);

        let growing = self.len < self.target_len;
        // Cells that remain occupied after the move: whole body if growing,
        // else all but the vacating tail.
        let remaining = if growing { self.len } else { self.len - 1 };

        let hit_self = !phase_self && self.first_segments(remaining).any(|c| c == new_head);
        if hit_self || blocked(new_head) {
            return StepOutcome::Blocked;
        }

        self.head_idx = (self.head_idx + 1) % SNAKE_CAP;
        self.seg[self.head_idx] = new_head;
        if growing {
            self.len += 1;
        }
        StepOutcome::Moved
    }

    /// True if `cell` is part of this snake's body.
    pub fn occupies(&self, cell: Cell) -> bool {
        self.segments().any(|c| c == cell)
    }
}

// ===========================================================================
// Camera / Viewport — world → 72×40 screen at 4 px/cell, seam-aware
// ===========================================================================

/// A camera window over the torus. `origin` is the world cell shown at the
/// top-left of the screen; the window spans [`VIEW_COLS`]×[`VIEW_ROWS`] cells
/// and wraps across the world seam.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Camera {
    pub origin: Cell,
}

impl Camera {
    /// Center the camera on `head` (the local player). Deadzone following is a
    /// drop-in replacement here: only `origin` changes, and every consumer goes
    /// through [`Camera::world_to_screen`], so the mapping stays correct.
    pub fn centered_on<const W: u16, const H: u16>(head: Cell) -> Self {
        Self {
            origin: Cell {
                x: World::<W, H>::wrap_x(head.x as i32 - (VIEW_COLS / 2) as i32),
                y: World::<W, H>::wrap_y(head.y as i32 - (VIEW_ROWS / 2) as i32),
            },
        }
    }

    /// Map a world cell to a **top-left pixel** on the 72×40 screen, or `None`
    /// if it's outside the visible window. Seam-aware: a cell on the far side
    /// of the world wrap still maps correctly because the offset is computed
    /// modulo the world size via [`World::forward_x`] / [`World::forward_y`].
    pub fn world_to_screen<const W: u16, const H: u16>(&self, c: Cell) -> Option<(u16, u16)> {
        let col = World::<W, H>::forward_x(c.x, self.origin.x);
        let row = World::<W, H>::forward_y(c.y, self.origin.y);
        if col < VIEW_COLS && row < VIEW_ROWS {
            Some((col * CELL_PX, row * CELL_PX))
        } else {
            None
        }
    }

    /// Convenience: is this world cell currently on-screen?
    pub fn is_visible<const W: u16, const H: u16>(&self, c: Cell) -> bool {
        self.world_to_screen::<W, H>(c).is_some()
    }
}

// ===========================================================================
// Wire codec — SMOLv1 SNK frame (netcode spec §1: 18 B, binary after prefix)
// ===========================================================================
//
// Layout (exactly SNK_FRAME_LEN = 18 bytes):
//   [0..11)  "SMOLv1 SNK "   ASCII prefix (sniffer-greppable)
//   [11]     ver   u8        format version (== SNK_VER)
//   [12]     id    u8        sender snake id (binary)
//   [13]     tick  u8        wrapping step counter — ORDERING + dead-reckon base
//   [14]     flags u8        bit0 alive · bits1-2 heading(0=U,1=R,2=D,3=L) · bits3-7 power(0..31)
//   [15]     head_x u8       world cell X (0..=255)
//   [16]     head_y u8       world cell Y (0..=255)
//   [17]     length u8       live segment count (body is dead-reckoned, not sent)
//   [18]     score  u8       ONLY in ver ≥ 2 (SNK_FRAME_LEN_V2 = 19); v1 implies score == length
//
// Coordinates are u8 (world ≤256/axis). The core stores cells as u16 for
// dimension-generality; encode narrows to u8, lossless for any world ≤256.
//
// Forward-compat policy: parse DEGRADES, it does not reject on version. Any
// frame with the SNK prefix and ≥ 18 B decodes its stable 18 B core regardless
// of `ver`; the `score` byte is read only when ver ≥ 2 and ≥ 19 B are present,
// else `score = length`. So old firmware keeps understanding newer frames
// (reads the fields it knows, ignores trailing additions), and an unknown
// power id decodes as its numeric value rather than erroring.

/// A decoded SMOLv1 SNK frame. Mirrors the on-wire fields 1:1 (with `score`
/// synthesized from `length` for v1 frames).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SnkFrame {
    pub ver: u8,
    pub id: u8,
    pub tick: u8,
    pub alive: bool,
    pub heading: Dir,
    /// Active treasure power, 0 = none, 1..=31 = power id (5 bits, flags[3..8]).
    pub power: u8,
    pub head: Cell,
    pub length: u8,
    /// Leaderboard score. v1 wire: implied == `length`. v2 wire: explicit byte.
    pub score: u8,
}

impl SnkFrame {
    /// A fresh `ver = SNK_VER` (v1) frame: `power = 0`, `score = length`.
    pub fn new(id: u8, tick: u8, alive: bool, heading: Dir, head: Cell, length: u8) -> Self {
        Self {
            ver: SNK_VER,
            id,
            tick,
            alive,
            heading,
            power: 0,
            head,
            length,
            score: length,
        }
    }

    /// Set the active power (clamped to [`POWER_MAX`]) and return `self` — a
    /// builder for the common `SnkFrame::new(..).with_power(p)` shape.
    #[inline]
    pub fn with_power(mut self, power: u8) -> Self {
        self.power = power & FLAG_POWER_MASK;
        self
    }

    /// Promote to a v2 frame carrying an explicit `score`.
    #[inline]
    pub fn with_score(mut self, score: u8) -> Self {
        self.ver = SNK_VER_SCORE;
        self.score = score;
        self
    }

    /// The active power id (0 = none). Getter mirroring [`SnkFrame::power`].
    #[inline]
    pub const fn power(&self) -> u8 {
        self.power & FLAG_POWER_MASK
    }

    /// Pack the `flags` byte:
    /// `bit0 alive | bits1-2 heading | bits3-7 power`.
    #[inline]
    pub const fn flags(&self) -> u8 {
        (self.alive as u8) & FLAG_ALIVE_MASK
            | ((self.heading.to_bits() & FLAG_HEADING_MASK) << FLAG_HEADING_SHIFT)
            | ((self.power & FLAG_POWER_MASK) << FLAG_POWER_SHIFT)
    }

    /// On-wire length for this frame's version (18 for v1, 19 for v2+).
    #[inline]
    pub const fn wire_len(&self) -> usize {
        if self.ver >= SNK_VER_SCORE {
            SNK_FRAME_LEN_V2
        } else {
            SNK_FRAME_LEN
        }
    }
}

/// Unpack the flags byte into `(alive, heading, power)`.
#[inline]
pub fn unpack_flags(flags: u8) -> (bool, Dir, u8) {
    let alive = flags & FLAG_ALIVE_MASK != 0;
    let heading = Dir::from_bits((flags >> FLAG_HEADING_SHIFT) & FLAG_HEADING_MASK);
    let power = (flags >> FLAG_POWER_SHIFT) & FLAG_POWER_MASK;
    (alive, heading, power)
}

/// Encode a [`SnkFrame`] into `out`, writing 18 B (v1) or 19 B (v2, with the
/// score byte) per `f.ver`. Returns the number of bytes written, or `None` if
/// `out` is too small. Mirrors the firmware's `encode_beacon` discipline:
/// fixed-size, length-returning, no allocation.
pub fn encode_snk(f: &SnkFrame, out: &mut [u8]) -> Option<usize> {
    let n = f.wire_len();
    if out.len() < n {
        return None;
    }
    out[..SNK_PREFIX.len()].copy_from_slice(SNK_PREFIX);
    out[11] = f.ver;
    out[12] = f.id;
    out[13] = f.tick;
    out[14] = f.flags();
    // Narrow to u8: lossless for a ≤256-cell world.
    out[15] = (f.head.x & 0xff) as u8;
    out[16] = (f.head.y & 0xff) as u8;
    out[17] = f.length;
    if n == SNK_FRAME_LEN_V2 {
        out[18] = f.score;
    }
    Some(n)
}

/// Parse a SMOLv1 SNK frame, or `None` only if it is too short (< 18 B) or has
/// the wrong prefix (every foreign SMOLv1 tag — HELLO/ACK/BEACON/TIME/RELAY —
/// is rejected here). **Version-degrading, not version-rejecting:** the stable
/// 18 B core decodes for any `ver ≥ 1`; the `score` byte is read only when
/// `ver ≥ SNK_VER_SCORE` and ≥ 19 B are present, otherwise `score = length`.
/// Total, rejects garbage, never panics.
pub fn parse_snk(buf: &[u8]) -> Option<SnkFrame> {
    if buf.len() < SNK_FRAME_LEN {
        return None; // truncated
    }
    if &buf[..SNK_PREFIX.len()] != SNK_PREFIX.as_slice() {
        return None; // foreign tag / garbage
    }
    let ver = buf[11];
    if ver == 0 {
        return None; // 0 = uninitialized/garbage, never a real frame
    }
    let (alive, heading, power) = unpack_flags(buf[14]);
    let length = buf[17];
    // score: explicit in v2+ when the byte is present, else implied == length.
    let score = if ver >= SNK_VER_SCORE && buf.len() >= SNK_FRAME_LEN_V2 {
        buf[18]
    } else {
        length
    };
    Some(SnkFrame {
        ver,
        id: buf[12],
        tick: buf[13],
        alive,
        heading,
        power,
        head: Cell::new(buf[15] as u16, buf[16] as u16),
        length,
        score,
    })
}

/// Wrap-aware (RFC 1982) "is `a` newer than `b`" for the `tick:u8` counter.
/// True iff the forward distance `a − b (mod 256)` is in `1..=127`. Handles the
/// 255→0 wrap for free and rejects late stragglers. The `< 128` half-window is
/// unambiguous here because at R = 5 Hz over a 3 s staleness window ticks
/// advance ~15 — nowhere near 128.
#[inline]
pub fn tick_is_newer(a: u8, b: u8) -> bool {
    let d = a.wrapping_sub(b);
    d != 0 && d < 128
}

// --- Phase jitter (netcode spec §2) ----------------------------------------

/// Deterministic broadcast phase slot for a snake id: `id % nmax`. Every board
/// derives the same schedule from the id alone (no negotiation), spreading
/// broadcasts across the tick period so synced clocks don't fire them all into
/// one 20 ms RX window. See [`phase_offset_ms`].
#[inline]
pub fn phase_slot(id: u8, nmax: u8) -> u8 {
    id % nmax.max(1)
}

/// Per-id broadcast offset (ms) within a `period_ms` window:
/// `(id % nmax) * (period_ms / nmax)` (spec §2). Distinct for any ids that are
/// distinct **modulo `nmax`** — guaranteed for the design's contiguous 0..nmax
/// id space (and this project's ids 7/8/9). Ids differing by exactly `nmax`
/// would share a slot; that is inherent to a per-id deterministic scheme
/// (a collision-free assignment for arbitrary ids needs global rank the mesh
/// doesn't have). See reconciliation note in the report.
#[inline]
pub fn phase_offset_ms(id: u8, nmax: u8, period_ms: u32) -> u32 {
    let nmax = nmax.max(1);
    phase_slot(id, nmax) as u32 * (period_ms / nmax as u32)
}

// ===========================================================================
// Peers — dead-reckoned remote snakes with tick-ordering + two-level staleness
// ===========================================================================

/// An already-decoded update fed into the [`PeerTable`].
///
/// Reconciliation with the 18 B wire frame ([`SnkFrame`]): the wire carries no
/// millisecond timestamp — only `tick:u8`. So `recv_ms` is stamped by the
/// **receiver** at arrival (its own `millis()`), which is exactly what the
/// staleness idiom needs (“how long since *I* last heard this peer”). `tick`
/// drives ordering; `recv_ms` drives staleness. Build one with
/// [`PeerUpdate::from_frame`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PeerUpdate {
    pub id: u8,
    /// Wrapping step counter from the wire (ordering).
    pub tick: u8,
    /// Local `millis()` at the moment this frame was received (staleness).
    pub recv_ms: u32,
    pub head: Cell,
    pub heading: Dir,
    pub length: u16,
    pub alive: bool,
    /// Active treasure power (0 = none).
    pub power: u8,
    /// Leaderboard score (v1: == length; v2: explicit).
    pub score: u8,
}

impl PeerUpdate {
    /// Adapt a decoded wire frame into an update, stamping the local receive
    /// time. This is THE wire→state ingestion boundary.
    pub fn from_frame(f: &SnkFrame, recv_ms: u32) -> Self {
        Self {
            id: f.id,
            tick: f.tick,
            recv_ms,
            head: f.head,
            heading: f.heading,
            length: f.length as u16,
            alive: f.alive,
            power: f.power(),
            score: f.score,
        }
    }
}

/// Liveness of a tracked peer relative to the staleness window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeerState {
    /// Fresh frame within `PEER_STALE_MS` — dead-reckon normally.
    Live,
    /// No frame for `PEER_STALE_MS..2×` — stop reckoning, ghost/dim it.
    Stale,
}

/// Outcome of feeding an update to [`PeerTable::upsert`] / [`PeerTable::ingest`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IngestOutcome {
    /// Stored (new peer or newer-tick update to an existing one).
    Accepted,
    /// Existing peer, but the frame's tick was not newer — dropped (ordering).
    StaleTick,
    /// Table full of other live peers — dropped.
    Overflow,
}

/// A remote snake reconstructed from its last [`PeerUpdate`].
///
/// **Reconstruction model (spec §3):** absolute head + heading + length +
/// `tick`; body dead-reckoned. Between updates the head advances in a straight
/// line at [`STEP_MS`] and the body is approximated as a straight line trailing
/// the head opposite the heading. Any single received frame **fully refreshes**
/// the peer (absolute state, not deltas), so loss self-heals. When a
/// turn-history encoding lands, swap [`PeerSnake::body_cells`] for a turn-replay
/// without touching callers.
#[derive(Clone, Copy, Debug, Default)]
pub struct PeerSnake {
    pub id: u8,
    /// Last accepted wrapping tick (for ordering).
    pub tick: u8,
    /// Local receive time of the last accepted frame (for staleness).
    pub last_ms: u32,
    pub head0: Cell,
    pub heading: Dir,
    pub length: u16,
    /// Game-state alive flag from the wire (bit0). A dead peer stays in the
    /// table (so it can respawn) but should not be reckoned as moving.
    pub alive: bool,
    /// Active treasure power from the wire (flags bits3-7, 0 = none) — advisory
    /// for the renderer (e.g. draw an aura/marker).
    pub power: u8,
    /// Leaderboard score (v1: == length; v2: explicit byte).
    pub score: u8,
    /// Slot occupancy (distinct from `alive`): true once filled, false after
    /// despawn frees the slot.
    active: bool,
}

impl PeerSnake {
    /// Dead-reckon the head forward to `now_ms` using `step_ms` per cell.
    pub fn dead_reckon_head<const W: u16, const H: u16>(&self, now_ms: u32, step_ms: u32) -> Cell {
        let step_ms = step_ms.max(1);
        let steps = (now_ms.saturating_sub(self.last_ms) / step_ms) as i32;
        let (dx, dy) = self.heading.delta();
        World::<W, H>::add(self.head0, dx * steps, dy * steps)
    }

    /// Liveness relative to `stale_ms`: [`PeerState::Live`] if a frame arrived
    /// within the window, else [`PeerState::Stale`]. (Despawn — beyond
    /// `2×stale_ms` — is handled by [`PeerTable::prune`], which frees the slot.)
    pub fn state(&self, now_ms: u32, stale_ms: u32) -> PeerState {
        if now_ms.saturating_sub(self.last_ms) <= stale_ms {
            PeerState::Live
        } else {
            PeerState::Stale
        }
    }

    /// Fill `out` with the peer's body cells (head first), straight-line
    /// approximation. Returns the number written = `min(length, out.len())`.
    pub fn body_cells<const W: u16, const H: u16>(
        &self,
        now_ms: u32,
        step_ms: u32,
        out: &mut [Cell],
    ) -> usize {
        let head = self.dead_reckon_head::<W, H>(now_ms, step_ms);
        let (bdx, bdy) = self.heading.opposite().delta();
        let n = (self.length as usize).min(out.len());
        for (i, slot) in out.iter_mut().enumerate().take(n) {
            *slot = World::<W, H>::add(head, bdx * i as i32, bdy * i as i32);
        }
        n
    }

    /// Whether this slot currently holds a live peer.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active
    }
}

/// Fixed-capacity peer table ([`PEER_CAP`] slots), keyed by peer `id`.
#[derive(Clone, Copy)]
pub struct PeerTable {
    peers: [PeerSnake; PEER_CAP],
}

impl Default for PeerTable {
    fn default() -> Self {
        Self {
            peers: [PeerSnake::default(); PEER_CAP],
        }
    }
}

impl PeerTable {
    /// New empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode a wire frame and ingest it, stamping the local receive time.
    /// The one-call wire→state path for the firmware RX loop.
    pub fn ingest(&mut self, f: &SnkFrame, recv_ms: u32) -> IngestOutcome {
        self.upsert(PeerUpdate::from_frame(f, recv_ms))
    }

    /// Insert or update a peer by id, applying **tick-wrap ordering**: an
    /// update to an existing peer is [`IngestOutcome::StaleTick`] (dropped) if
    /// its `tick` is not newer than the last accepted (see [`tick_is_newer`]),
    /// so a late straggler can't rubber-band a peer. A new id takes a free slot
    /// or is [`IngestOutcome::Overflow`] if the table is full of other peers.
    pub fn upsert(&mut self, u: PeerUpdate) -> IngestOutcome {
        // 1) existing id? apply ordering.
        for p in self.peers.iter_mut() {
            if p.active && p.id == u.id {
                if !tick_is_newer(u.tick, p.tick) {
                    return IngestOutcome::StaleTick;
                }
                Self::apply(p, u);
                return IngestOutcome::Accepted;
            }
        }
        // 2) first free slot?
        for p in self.peers.iter_mut() {
            if !p.active {
                Self::apply(p, u);
                return IngestOutcome::Accepted;
            }
        }
        IngestOutcome::Overflow
    }

    fn apply(p: &mut PeerSnake, u: PeerUpdate) {
        p.id = u.id;
        p.tick = u.tick;
        p.last_ms = u.recv_ms;
        p.head0 = u.head;
        p.heading = u.heading;
        p.length = u.length;
        p.alive = u.alive;
        p.power = u.power;
        p.score = u.score;
        p.active = true;
    }

    /// Despawn peers whose last accepted frame is older than `despawn_ms`
    /// relative to `now_ms` (firmware passes [`PEER_STALE_MS`] — single-tier per
    /// §10.6). Returns the number despawned. The optional render-only dim
    /// ([`PEER_DIM_MS`] via [`PeerSnake::state`]) is cosmetic and does NOT
    /// despawn.
    pub fn prune(&mut self, now_ms: u32, despawn_ms: u32) -> usize {
        let mut n = 0;
        for p in self.peers.iter_mut() {
            if p.active && now_ms.saturating_sub(p.last_ms) > despawn_ms {
                p.active = false;
                n += 1;
            }
        }
        n
    }

    /// Iterate live peers.
    pub fn active(&self) -> impl Iterator<Item = &PeerSnake> + '_ {
        self.peers.iter().filter(|p| p.active)
    }

    /// Count of live peers.
    pub fn active_count(&self) -> usize {
        self.peers.iter().filter(|p| p.active).count()
    }
}

// ===========================================================================
// Food — deterministic spawn as a pure fn of (seed, time_bucket)
// ===========================================================================

/// SplitMix64 finalizer — a well-distributed integer hash. Pure, allocation
/// free, identical on every node. (Public so the netcode layer can reuse the
/// same mixing if it wants correlated randomness.)
#[inline]
pub fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The food cell for a given `(seed, time_bucket)`.
///
/// Pure and deterministic: every node that agrees on `seed` and the current
/// `time_bucket` (derived from the mesh-synced clock) computes the **same**
/// cell with no messaging. Food changes each bucket; the honest eaten-race
/// rule is "both eaters grow, the cell moves next bucket" — no authority needed
/// precisely because this is a shared pure function.
pub fn food_at<const W: u16, const H: u16>(seed: u32, time_bucket: u32) -> Cell {
    let h = splitmix64(((seed as u64) << 32) | time_bucket as u64);
    Cell {
        x: World::<W, H>::wrap_x((h & 0xFFFF) as i32),
        y: World::<W, H>::wrap_y(((h >> 16) & 0xFFFF) as i32),
    }
}

/// Default beacon count for [`food_cells`] (design §10.4 wants K≈8–16 so a 2–16
/// snake game in a 256² world actually meets food; the raw [`food_at`] returns
/// just 1). 12 sits mid-range.
pub const FOOD_COUNT: usize = 12;

/// Fill `out` with up to `min(out.len(), FOOD_COUNT)` deterministic food cells
/// for a bucket — the design §10.4 "several beacons" wrapper over [`food_at`].
/// Each beacon derives from the same `(seed, bucket)` with a per-index salt, so
/// every node computes the identical set with zero messaging (cells may
/// coincidentally coincide — that just shows fewer beacons, harmless). Returns
/// the count written.
pub fn food_cells<const W: u16, const H: u16>(
    seed: u32,
    time_bucket: u32,
    out: &mut [Cell],
) -> usize {
    let n = out.len().min(FOOD_COUNT);
    for (i, slot) in out.iter_mut().enumerate().take(n) {
        // Distinct salt per beacon; multiply-by-odd keeps the stream well spread.
        let s = seed ^ (i as u32).wrapping_mul(0x9E37_79B1);
        *slot = food_at::<W, H>(s, time_bucket);
    }
    n
}

/// Salt distinguishing the treasure stream from the food stream (same
/// `(seed, bucket)`, different cells/schedule). Any fixed nonzero constant.
pub const TREASURE_KSALT: u32 = 0x5EED_7EA5;

/// Deterministic treasure spawn (design §11.3): the cell AND which power, as a
/// pure fn of `(seed, treasure_bucket)`. Node-agnostic — every board computes
/// the same treasure with no messaging (like [`food_at`], but a rarer bucket
/// cadence, e.g. `TREASURE_PERIOD ≈ 45–60 s`). Returns `(cell, power)` where
/// `power ∈ 1..=POWER_COUNT` per `1 + h % 6`.
pub fn treasure_at<const W: u16, const H: u16>(seed: u32, treasure_bucket: u32) -> (Cell, u8) {
    let h = splitmix64(((seed ^ TREASURE_KSALT) as u64) << 32 | treasure_bucket as u64);
    let cell = Cell {
        x: World::<W, H>::wrap_x((h & 0xFFFF) as i32),
        y: World::<W, H>::wrap_y(((h >> 16) & 0xFFFF) as i32),
    };
    let power = 1 + (h % POWER_COUNT as u64) as u8; // 1..=6
    (cell, power)
}

