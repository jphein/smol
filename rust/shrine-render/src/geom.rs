//! Battlefield geometry for the 320x240 shrine panel.
//!
//! Every constant here is either quoted from `docs/design/shrine-ux.md` or derived from it, and
//! the derivation is written next to the number. The doc's own measured table sets two hard
//! floors that the layouts below are checked against:
//!
//! * **sprite 48 px** (`## Sprite and type budget`) — 8.5 mm on the glass, the size at which a
//!   silhouette reads at arm's length.
//! * **touch target 44 px** (`## The device, measured`, PARITY.md house rule) — below this a cell
//!   cannot be a touch target and the screen is purist-mode-only.
//!
//! The doc draws the battlefield as three rows per lane. The shipped engine gives *each seat* its
//! own three-cell track per lane (`tapstone_rules::state::Seat::cells`, cell 0 = back next to my
//! castle, cell 2 = front; `rules.rs` resolves melee against `enemy[CELLS - 1]`). A lane therefore
//! holds **six** cells. That is the whole reason this module carries more than one layout.

use tapstone_rules::state::{CELLS, LANES};

/// Panel size, landscape. `DISPLAY-PACKAGE §2`.
pub const W: i32 = 320;
pub const H: i32 = 240;

/// HUD band per seat, top and bottom. Battlefield wireframe: `y0-24` and `y216-240`.
pub const HUD: i32 = 24;
/// Castle wall band per seat. Wireframe: `y24-36` and `y204-216`.
pub const WALL: i32 = 12;

/// The doc's own floors, kept as constants so the layout audit can cite them.
pub const SPRITE_SPEC: i32 = 48;
pub const TOUCH_MIN: i32 = 44;
/// Decision 0014's *base* sprite size. The 48 px figure is the hero size; 32 px is the floor at
/// which the planned pixel art still reads as a shape.
pub const SPRITE_BASE: i32 = 32;

/// Cells belonging to one lane once both seats are counted: 3 mine + 3 theirs.
pub const CELLS_PER_LANE: usize = CELLS * 2;

/// Which way the lanes run across the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// Lanes are columns; "toward the enemy" is up. Matches the mat. The doc's choice.
    Vertical,
    /// Lanes are rows; "toward the enemy" is right. Rotates the board 90 deg from the mat.
    Horizontal,
}

/// How many of each seat's three cells the layout actually draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// All three cells per seat: six per lane. The only honest option.
    Full,
    /// The front two cells per seat; the back cell collapses to a badge on the wall.
    FrontTwo,
}

/// A candidate battlefield layout, resolved to pixels.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub name: &'static str,
    pub orientation: Orientation,
    pub depth: Depth,
    /// Cells drawn per seat (1..=CELLS).
    pub per_seat: usize,
    /// Cell extent along the advance axis for MY cells (vertical: height; horizontal: width).
    pub cell_adv: i32,
    /// Same, for the opponent's cells. Equal to `cell_adv` in every symmetric layout; smaller in
    /// the asymmetric one, where their side is a read-only stat chip rather than a touch target.
    pub far_adv: i32,
    /// Cell extent across lanes (vertical: width; horizontal: height).
    pub cell_cross: i32,
    /// Largest square sprite that fits, after leaving a 1 px margin.
    pub sprite: i32,
}

impl Layout {
    /// Does the cell clear the doc's 44 px minimum touch target on both axes?
    pub fn touchable(&self) -> bool {
        self.cell_adv >= TOUCH_MIN && self.cell_cross >= TOUCH_MIN
    }

    /// Does a 48 px hero sprite fit as the UX doc specifies?
    pub fn sprite_fits(&self) -> bool {
        self.sprite >= SPRITE_SPEC
    }

    /// Does the sprite clear decision 0014's 32 px base size?
    pub fn sprite_base_ok(&self) -> bool {
        self.sprite >= SPRITE_BASE
    }

    /// Does the on-screen lane index run the same way as the apron's three pads?
    ///
    /// Decision 0018 puts three pads side by side across the apron, so lane is a physical
    /// left-to-right position. A vertical layout draws lanes as columns and matches it; the
    /// horizontal layout draws lane 0 as the top row, which the player must learn rather than see.
    pub fn lane_index_matches_apron(&self) -> bool {
        matches!(self.orientation, Orientation::Vertical)
    }

    /// Touch viability is judged on MY cells: those are the ones a finger acts on.
    pub fn touchable_near(&self) -> bool {
        self.cell_adv >= TOUCH_MIN && self.cell_cross >= TOUCH_MIN
    }

    /// Cells drawn per lane across both seats.
    pub fn cells_per_lane(&self) -> usize {
        self.per_seat * 2
    }

    /// Does this layout show the whole board the engine tracks?
    pub fn shows_whole_board(&self) -> bool {
        self.per_seat == CELLS
    }
}

/// Space between the two HUD bands: 240 - 2*24 = 192 px. The doc states this figure directly.
pub const BETWEEN_HUD: i32 = H - 2 * HUD;
/// Space left for cells once both castle walls are taken: 192 - 2*12 = 168 px.
pub const CELL_BAND: i32 = BETWEEN_HUD - 2 * WALL;

/// Lane columns for the vertical layouts. 320 px does not divide by 3, so the doc's stated
/// 106 px column leaves 2 px unaccounted. They are spent on the two 1 px lane separators the
/// doc's own wireframe draws, giving 106 + 1 + 106 + 1 + 106 = 320 exactly.
pub const LANE_CROSS_V: i32 = 106;
pub const LANE_SEP: i32 = 1;

/// The doc's layout as written: three rows per lane. Cannot display both seats' tracks, and is
/// kept only so the preview can show what the wireframe actually specified.
pub fn doc_three_row() -> Layout {
    Layout {
        name: "doc-3row",
        orientation: Orientation::Vertical,
        depth: Depth::FrontTwo,
        // The doc gives no per-seat split at all; three rows for a six-cell lane.
        per_seat: 1,
        cell_adv: CELL_BAND / 3,
        far_adv: CELL_BAND / 3,
        cell_cross: LANE_CROSS_V,
        sprite: SPRITE_SPEC,
    }
}

/// (a) Honest six rows: every cell the engine tracks, at whatever height is left.
pub fn vertical_full() -> Layout {
    let adv = CELL_BAND / CELLS_PER_LANE as i32; // 168 / 6 = 28
    Layout {
        name: "v6-full",
        orientation: Orientation::Vertical,
        depth: Depth::Full,
        per_seat: CELLS,
        cell_adv: adv,
        far_adv: adv,
        cell_cross: LANE_CROSS_V,
        sprite: (adv - 2).min(LANE_CROSS_V - 2),
    }
}

/// (b) Four rows at 42 px: the front two cells per seat, back cell as a wall badge.
pub fn vertical_front_two() -> Layout {
    let adv = CELL_BAND / 4; // 168 / 4 = 42
    Layout {
        name: "v4-front2",
        orientation: Orientation::Vertical,
        depth: Depth::FrontTwo,
        per_seat: 2,
        cell_adv: adv,
        far_adv: adv,
        cell_cross: LANE_CROSS_V,
        sprite: (adv - 2).min(LANE_CROSS_V - 2),
    }
}

/// (d) Lanes as rows, cells running left to right, castles at the edges.
///
/// Vertical budget: 240 - 2*24 HUD = 192 for three lanes = 64 px per lane.
/// Horizontal budget: 320 - 2*12 walls = 296 for six cells = 49 px per cell.
/// Both axes clear 44 px, and a 48 px sprite fits. This is the only candidate that satisfies
/// every constraint the doc sets for itself.
pub fn horizontal_full() -> Layout {
    let cross = BETWEEN_HUD / LANES as i32; // 192 / 3 = 64
    let adv = (W - 2 * WALL) / CELLS_PER_LANE as i32; // 296 / 6 = 49
    Layout {
        name: "h6-full",
        orientation: Orientation::Horizontal,
        depth: Depth::Full,
        per_seat: CELLS,
        cell_adv: adv,
        far_adv: adv,
        cell_cross: cross,
        sprite: (adv - 1).min(cross - 1).min(SPRITE_SPEC),
    }
}

/// (e) Asymmetric vertical: my three cells at 36 px, theirs at 20 px.
///
/// `3*36 + 3*20 = 168`, so the budget is spent exactly. The halves are not symmetric in what the
/// player *does* with them: I summon, advance and target my own units, so they need a sprite and a
/// touch target; I only *read* theirs, so they can be a compact stat chip. 36 px clears decision
/// 0014's 32 px base sprite, which no symmetric vertical layout manages.
pub fn vertical_asymmetric() -> Layout {
    Layout {
        name: "v6-asym",
        orientation: Orientation::Vertical,
        depth: Depth::Full,
        per_seat: CELLS,
        cell_adv: 36,
        far_adv: 20,
        cell_cross: LANE_CROSS_V,
        sprite: 34,
    }
}

/// Number of candidate layouts.
pub const LAYOUTS: usize = 5;

/// Every candidate, in the order the report discusses them. A fixed array: this crate has no
/// allocator, and the candidate list is a constant rather than runtime data.
pub fn all() -> [Layout; LAYOUTS] {
    [
        doc_three_row(),
        vertical_full(),
        vertical_front_two(),
        horizontal_full(),
        vertical_asymmetric(),
    ]
}

/// Extent of one slot along the advance axis: mine use `cell_adv`, theirs `far_adv`.
pub fn slot_extent(layout: &Layout, idx: usize) -> i32 {
    if idx < layout.per_seat {
        layout.cell_adv
    } else {
        layout.far_adv
    }
}

/// Distance from my own wall to the near edge of slot `idx`, summing the slots below it.
pub fn slot_offset(layout: &Layout, idx: usize) -> i32 {
    (0..idx).map(|i| slot_extent(layout, i)).sum()
}

/// Total extent of every drawn slot in a lane.
pub fn advance_span(layout: &Layout) -> i32 {
    slot_offset(layout, layout.per_seat * 2)
}

/// Top edge of the vertical cell band (below the enemy wall).
pub const CELL_BAND_TOP: i32 = HUD + WALL; // 36
/// x of lane `l` under the vertical layouts.
pub fn lane_x(l: usize) -> i32 {
    l as i32 * (LANE_CROSS_V + LANE_SEP)
}
/// y of lane `l` under the horizontal layout.
pub fn lane_y(l: usize, cross: i32) -> i32 {
    HUD + l as i32 * cross
}
