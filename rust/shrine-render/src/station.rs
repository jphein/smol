//! Decision 0032's shrine screens: the commander's station.
//!
//! Five screens and no others — idle, lobby/loadout, the in-match station, result, dark — all
//! drawn at 320×240 RGB565 into any `DrawTarget`, positioned absolutely so they keep the crate's
//! band contract (`band.rs`).
//!
//! # One skeleton for every screen
//!
//! The paperdoll sits in the **same 96×96 box** on lobby, station and result, and the voice band
//! is the **same bottom 24 px** everywhere. A transition between them therefore never moves the
//! figure, and "where do I look" has one answer per question: the figure is you, the band is
//! what the shrine is saying.
//!
//! ```text
//!  x 4        100 108   156 172                        316
//!  ┌─ name  Lv ─┐ ┌─────┐   ┌──────┬──────┬──────┐  y 4
//!  │            │ │ wpn │   │      │      │      │
//!  │  paperdoll │ └─────┘   ├──────┼──────┼──────┤
//!  │   96×96    │ ┌─────┐   │  inventory 3×4    │   (lobby / result)
//!  └────────────┘ │ arm │   │  of 48 px         │
//!   xp / hp bar   └─────┘   ├──────┼──────┼──────┤
//!   stats         ┌─────┐   │      │      │      │
//!   keyword       │ trk │   ├──────┼──────┼──────┤
//!                 └─────┘   │      │      │      │  y 196
//!  ─ ─ ─ ─ push-to-talk hit area (44 px) ─ ─ ─ ─ ─ ─ ─ ─   y 196
//!  ▌voice band (24 px drawn)                               y 216..240
//! ```
//!
//! The station replaces the sockets and grid with the glanceable wells (castle, mana, hand and
//! round) and puts the commander's health bar and lane locator under the figure.
//!
//! # The two geometry facts this layout rests on
//!
//! * **The inventory fits only as 3 wide × 4 tall, and only with the header moved.** 0032's
//!   "3×4 of 48 px" is 144×192 or 192×144. The 192-wide reading leaves 128 px for a 96 px doll
//!   plus a 48 px socket column (144): it does not fit. The 144-wide reading needs 192 of the
//!   196 px above the push-to-talk strip, so the grid runs y 4..196 and there is **no room for a
//!   header above it** — the name, level and XP live in the left column instead.
//! * **The push-to-talk strip is 44 px, not 24.** 0033 makes the band a touch-and-hold target;
//!   the band is drawn 24 px tall, under the 44 px touch floor. The hit area is therefore
//!   y 196..240, and no other touch target may enter it. See [`voice::PTT_TOP`].

use embedded_graphics::{
    mono_font::MonoFont,
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::Alignment,
};

use crate::battlefield::{fill, fit, name_of, stroke, tier};
use crate::commander::{
    self, Commander, INVENTORY, Ledger, Presence, ReturnState, SLOTS, Slot, SlotExt, Station,
    keyword_name, xp_to_next,
};
use crate::geom;
use crate::ink::{Ink, label};
use crate::palette as pal;
use crate::sprite::{self, DOLL_SCALE, HERO, Look, Pose, Scaled};
use crate::voice::{self, Voice};

/// Outer margin.
pub const M: i32 = 4;
/// Paperdoll box: 0014's 48 px hero at 0032's 2×.
pub const DOLL_PX: i32 = HERO * DOLL_SCALE;
pub const DOLL: Rectangle =
    Rectangle::new(Point::new(M, 24), Size::new(DOLL_PX as u32, DOLL_PX as u32));
/// Name / level / result word line, above the doll.
pub const TITLE_Y: i32 = M;
/// The left column's right edge: the doll's.
pub const COL_R: i32 = M + DOLL_PX;

/// Lobby/result: XP bar and the text lines under the doll.
pub const XP_BAR: Rectangle = Rectangle::new(Point::new(M, 126), Size::new(DOLL_PX as u32, 6));
pub const XP_TEXT_Y: i32 = 136;
pub const DESK_STATS_Y: i32 = 152;
pub const DESK_NOTE_Y: i32 = 166;

/// Station: commander health bar, stats line, locator.
pub const HP_BAR: Rectangle = Rectangle::new(Point::new(M, 126), Size::new(DOLL_PX as u32, 8));
pub const STATION_STATS_Y: i32 = 138;
pub const LOC_X: i32 = M;
pub const LOC_Y: i32 = 158;
pub const LOC_CELL: i32 = 10;
pub const LOC_CGAP: i32 = 2;
pub const LOC_LGAP: i32 = 5;
pub const LOC_LABEL_X: i32 = 56;

/// Gear sockets: one column of three, beside the doll.
pub const SOCKET: i32 = 48;
pub const SOCKET_X: i32 = COL_R + 8;
pub const SOCKET_GAP: i32 = 4;
pub fn socket_rect(s: Slot) -> Rectangle {
    Rectangle::new(
        Point::new(
            SOCKET_X,
            DOLL.top_left.y + s.index() as i32 * (SOCKET + SOCKET_GAP),
        ),
        Size::new(SOCKET as u32, SOCKET as u32),
    )
}

/// Inventory: 3 wide × 4 tall at 48 px (0032), right-aligned to the margin.
pub const CELL: i32 = 48;
pub const GRID_COLS: i32 = 3;
pub const GRID_ROWS: i32 = 4;
pub const GRID_X: i32 = geom::W - M - GRID_COLS * CELL;
pub const GRID_Y: i32 = M;
pub fn cell_rect(i: usize) -> Rectangle {
    let (c, r) = (i as i32 % GRID_COLS, i as i32 / GRID_COLS);
    Rectangle::new(
        Point::new(GRID_X + c * CELL, GRID_Y + r * CELL),
        Size::new(CELL as u32, CELL as u32),
    )
}

// The inventory geometry, evaluated by the compiler of whatever target builds this crate — the
// firmware's included — rather than by a host test (`docs/verification.md`: a layout claim is only
// evidence if the target's compiler evaluated it). Each was checked by breaking it: a fourth
// column, or a 24 px header above the grid, stops the build.
//
// 0032's "3×4 of 48 px" fits 3 wide beside the doll and its sockets...
const _: () = assert!(SOCKET_X + SOCKET + 8 + GRID_COLS * CELL + M <= geom::W);
// ...and would not fit 4 wide.
const _: () = assert!(SOCKET_X + SOCKET + 8 + (GRID_COLS + 1) * CELL + M > geom::W);
// Four rows clear the push-to-talk strip...
const _: () = assert!(GRID_Y + GRID_ROWS * CELL <= crate::voice::PTT_TOP);
// ...with no room for a 24 px header above them: the name and XP must live in the left column.
const _: () = assert!(GRID_Y + GRID_ROWS * CELL + 24 > crate::voice::PTT_TOP);
// The grid starts where the sockets end, plus a gutter.
const _: () = assert!(GRID_X >= SOCKET_X + SOCKET + 8);
// Every cell and socket clears the 44 px touch floor.
const _: () = assert!(CELL >= geom::TOUCH_MIN && SOCKET >= geom::TOUCH_MIN);

/// Station wells, right of the left column.
pub const WELL_X: i32 = COL_R + 12;
pub const WELL_W: i32 = geom::W - M - WELL_X;
pub const CASTLE_WELL: Rectangle =
    Rectangle::new(Point::new(WELL_X, M), Size::new(WELL_W as u32, 52));
pub const MANA_WELL: Rectangle =
    Rectangle::new(Point::new(WELL_X, 62), Size::new(WELL_W as u32, 48));
pub const HAND_WELL: Rectangle =
    Rectangle::new(Point::new(WELL_X, 116), Size::new(WELL_W as u32, 44));
pub const PRESSURE_WELL: Rectangle =
    Rectangle::new(Point::new(WELL_X, 166), Size::new(WELL_W as u32, 22));

/// Level-up light stages; 0 is no light.
pub const LIGHT_STAGES: u8 = 5;
/// The fallen figure lies this many base pixels lower (the end of the fall).
pub const FALL_DROP: i32 = 2;

fn blank<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
    fill(
        d,
        Rectangle::new(Point::zero(), Size::new(geom::W as u32, geom::H as u32)),
        pal::BG,
    );
}

fn lh(font: &MonoFont) -> i32 {
    font.character_size.height as i32
}

/// The paperdoll at 2× in `at` (a 96×96 box), with an optional level-up light behind it.
pub fn doll<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    at: Rectangle,
    c: Option<&Commander<'_>>,
    pose: Pose,
    light: u8,
) {
    fill(d, at, pal::BG);
    if light > 0 {
        light_column(d, at, light);
    }
    let mut s = Scaled {
        inner: d,
        origin: at.top_left,
        k: DOLL_SCALE,
    };
    sprite::doll(&mut s, c, pose);
}

/// The level-up column: rises from the figure's feet to the top of the panel, `stage` of
/// `LIGHT_STAGES`. Confined to the doll's own x-range so it is one rectangle with the figure.
pub const LIGHT: Rgb565 = Rgb565::new(0xff >> 3, 0xe6 >> 2, 0x99 >> 3);
pub fn light_column<D: DrawTarget<Color = Rgb565>>(d: &mut D, at: Rectangle, stage: u8) {
    let stage = stage.min(LIGHT_STAGES) as i32;
    let bottom = at.top_left.y + at.size.height as i32;
    let h = bottom * stage / LIGHT_STAGES as i32;
    let w = at.size.width as i32 / 2;
    let x = at.top_left.x + (at.size.width as i32 - w) / 2;
    // Core and a softer halo either side; both palette, no blending needed on the device.
    fill(
        d,
        Rectangle::new(
            Point::new(x - 8, bottom - h),
            Size::new((w + 16) as u32, h as u32),
        ),
        sprite::mix(LIGHT, pal::BG, 3),
    );
    fill(
        d,
        Rectangle::new(Point::new(x, bottom - h), Size::new(w as u32, h as u32)),
        sprite::mix(LIGHT, pal::BG, 1),
    );
}

/// Name and level on one line, fitted to the left column.
fn title_line<D: DrawTarget<Color = Rgb565>>(d: &mut D, c: &Commander<'_>) {
    let lv = crate::txt!(" Lv{}", c.level());
    let room = (DOLL_PX / tier::STATUS.character_size.width as i32) as usize - lv.len();
    let name = fit(c.name, room);
    label(
        d,
        &name,
        Point::new(M, TITLE_Y),
        tier::STATUS,
        Ink::Text,
        Alignment::Left,
    );
    let x = M + name.len() as i32 * tier::STATUS.character_size.width as i32;
    label(
        d,
        &lv,
        Point::new(x, TITLE_Y),
        tier::STATUS,
        Ink::Dim,
        Alignment::Left,
    );
}

fn stats_line<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    y: i32,
    attack: u8,
    hp: u8,
    max: u8,
    full: bool,
) {
    let s = if full {
        crate::txt!("ATK {attack}  HP {max}")
    } else {
        crate::txt!("ATK {attack}  HP {hp}/{max}")
    };
    label(
        d,
        &s,
        Point::new(M, y),
        tier::STATUS,
        Ink::Text,
        Alignment::Left,
    );
}

// ---------------------------------------------------------------------------------------------
// 1. Idle
// ---------------------------------------------------------------------------------------------

/// Idle doll box: centred, same 96 px.
pub const IDLE_DOLL: Rectangle = Rectangle::new(
    Point::new((geom::W - DOLL_PX) / 2, 28),
    Size::new(DOLL_PX as u32, DOLL_PX as u32),
);

/// Screen 1 — Idle (amends 0020): the last commander seen here, breathing; or the hooded silhouette.
pub fn idle<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    c: Option<&Commander<'_>>,
    sigil: &str,
    pose: Pose,
) {
    blank(d);
    doll(d, IDLE_DOLL, c, pose, 0);
    let cx = geom::W / 2;
    match c {
        Some(c) => {
            let line = crate::txt!("{}  Lv {}", fit(c.name, 20), c.level());
            label(
                d,
                &line,
                Point::new(cx, 132),
                tier::TURN,
                Ink::Text,
                Alignment::Center,
            );
        }
        None => label(
            d,
            "no commander yet",
            Point::new(cx, 132),
            tier::TURN,
            Ink::Dim,
            Alignment::Center,
        ),
    }
    let s = crate::txt!("shrine {}", sigil);
    label(
        d,
        &s,
        Point::new(cx, 156),
        tier::STATUS,
        Ink::Dim,
        Alignment::Center,
    );
    voice::draw(d, &Voice::Invite);
}

// ---------------------------------------------------------------------------------------------
// 2 and 4. The desk: lobby / loadout, and result (same skeleton)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Socket {
    Locked,
    Empty,
    Worn(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Empty,
    Item {
        id: u16,
        worn: bool,
        new: bool,
    },
    /// A loot chest opening, 0..=3.
    Chest(u8),
    /// A drop with nowhere to go, melting into 1 XP over the cell it borrowed.
    Melting(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Title<'a> {
    /// Name and level.
    Lobby,
    /// The result word, e.g. "VICTORY".
    Result { word: &'a str, won: Option<bool> },
}

/// Everything the lobby and result screens draw, region by region.
///
/// Regions are separate fields on purpose: an animation changes one region per frame (one
/// dirty rectangle), so it needs to be able to hold the others at their old value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Desk<'a> {
    pub title: Title<'a>,
    /// Drives the figure and the stats lines.
    pub cmdr: Commander<'a>,
    pub pose: Pose,
    pub light: u8,
    /// The level shown, which lags the XP while a level-up is animated: the total can cross into
    /// the next level before the light lands it. Static screens show `cmdr.level()`.
    pub level: u8,
    /// Total XP shown (may lag `cmdr.xp` while the bar fills); the bar draws what lies inside
    /// `level`.
    pub xp_shown: u32,
    pub xp_gain: Option<u8>,
    /// The level-up line, when one was earned.
    pub levelled: bool,
    pub sockets: [Socket; 3],
    pub grid: [Cell; INVENTORY],
    /// An item falling into its socket: (item id, top y in the socket column).
    pub drop: Option<(u16, i32)>,
    pub voice: Voice<'a>,
}

impl<'a> Desk<'a> {
    /// The static lobby for a commander and its ledger.
    pub fn lobby(cmdr: Commander<'a>, ledger: &Ledger, voice: Voice<'a>) -> Desk<'a> {
        Desk {
            title: Title::Lobby,
            cmdr,
            pose: Pose::default(),
            light: 0,
            level: cmdr.level(),
            xp_shown: cmdr.xp,
            xp_gain: None,
            levelled: false,
            sockets: sockets_of(&cmdr, cmdr.level()),
            grid: grid_of(&cmdr, ledger),
            drop: None,
            voice,
        }
    }
}

/// The sockets a commander shows at `level` (the displayed one, which a level-up may lag).
pub fn sockets_of(c: &Commander<'_>, level: u8) -> [Socket; 3] {
    SLOTS.map(|s| {
        if !commander::slot_open_at(level, s) {
            Socket::Locked
        } else {
            match c.worn(s) {
                Some(it) => Socket::Worn(it.id),
                None => Socket::Empty,
            }
        }
    })
}

pub fn grid_of(c: &Commander<'_>, l: &Ledger) -> [Cell; INVENTORY] {
    l.inventory.map(|slot| match slot {
        None => Cell::Empty,
        Some(id) => Cell::Item {
            id,
            worn: c.loadout.contains(&Some(id)),
            new: false,
        },
    })
}

pub fn draw_socket<D: DrawTarget<Color = Rgb565>>(d: &mut D, slot: Slot, s: Socket, at: Rectangle) {
    fill(d, at, pal::PANEL);
    match s {
        Socket::Locked => {
            stroke(d, at, pal::DIMMED, 1);
            let cx = at.top_left.x + SOCKET / 2;
            // A padlock: shackle and body.
            stroke(
                d,
                Rectangle::new(Point::new(cx - 5, at.top_left.y + 8), Size::new(10, 10)),
                pal::DIMMED,
                2,
            );
            fill(
                d,
                Rectangle::new(Point::new(cx - 8, at.top_left.y + 16), Size::new(16, 12)),
                pal::DIMMED,
            );
            let lv = crate::txt!("Lv {}", commander::TRINKET_LEVEL);
            label(
                d,
                &lv,
                Point::new(cx, at.top_left.y + 32),
                tier::STATUS,
                Ink::WellDim,
                Alignment::Center,
            );
        }
        Socket::Empty => {
            stroke(d, at, pal::TEXT_DIM, 1);
            label(
                d,
                slot.name(),
                Point::new(
                    at.top_left.x + SOCKET / 2,
                    at.top_left.y + (SOCKET - lh(tier::STATUS)) / 2,
                ),
                tier::STATUS,
                Ink::WellDim,
                Alignment::Center,
            );
        }
        Socket::Worn(id) => {
            stroke(d, at, pal::TEXT, 1);
            if let Some(it) = commander::item(id) {
                sprite::icon(d, it, centre(at), 36, Look::Normal);
            }
        }
    }
}

fn centre(r: Rectangle) -> Point {
    Point::new(
        r.top_left.x + r.size.width as i32 / 2,
        r.top_left.y + r.size.height as i32 / 2,
    )
}

pub fn draw_cell<D: DrawTarget<Color = Rgb565>>(d: &mut D, c: Cell, at: Rectangle) {
    // The 1 px frame is inside the cell, so 48 px cells tile with no gutter.
    fill(d, at, pal::BG);
    let inner = Rectangle::new(at.top_left + Point::new(1, 1), at.size - Size::new(2, 2));
    fill(d, inner, pal::PANEL);
    match c {
        Cell::Empty => stroke(d, inner, pal::DIMMED, 1),
        Cell::Item { id, worn, new } => {
            let frame = if new { pal::WARM } else { pal::DIMMED };
            stroke(d, inner, frame, if new { 2 } else { 1 });
            if let Some(it) = commander::item(id) {
                // A worn item stays in the grid, dimmed, with a dot saying where it went.
                let look = if worn { Look::Fade(2) } else { Look::Normal };
                sprite::icon(d, it, centre(at), 36, look);
                if worn {
                    fill(
                        d,
                        Rectangle::new(at.top_left + Point::new(CELL - 9, 4), Size::new(5, 5)),
                        pal::TEXT,
                    );
                }
            }
        }
        Cell::Melting(id) => {
            stroke(d, inner, pal::WARM, 1);
            if let Some(it) = commander::item(id) {
                sprite::icon(d, it, centre(at), 36, Look::Fade(3));
            }
            label(
                d,
                "+1 XP",
                Point::new(at.top_left.x + CELL / 2, at.top_left.y + CELL - 16),
                tier::STATUS,
                Ink::Warm,
                Alignment::Center,
            );
        }
        Cell::Chest(stage) => {
            stroke(d, inner, pal::WARM, 1);
            sprite::chest(d, at.top_left + Point::new(4, 4), stage);
        }
    }
}

/// Draw the item falling into its socket column at `y`, exactly as it will look once seated.
pub fn draw_drop<D: DrawTarget<Color = Rgb565>>(d: &mut D, id: u16, y: i32) {
    let Some(it) = commander::item(id) else {
        return;
    };
    let at = Rectangle::new(
        Point::new(SOCKET_X, y),
        Size::new(SOCKET as u32, SOCKET as u32),
    );
    draw_socket(d, it.slot, Socket::Worn(id), at);
}

/// The desk's left column under the doll: XP bar, XP line, stats, keyword or level-up line.
fn desk_column<D: DrawTarget<Color = Rgb565>>(d: &mut D, k: &Desk<'_>) {
    let c = &k.cmdr;
    // XP bar: the total shown, measured inside the level shown.
    fill(d, XP_BAR, pal::PANEL);
    let next = xp_to_next(k.level);
    let into = k.xp_shown.saturating_sub(commander::level_start(k.level)) as u16;
    let w = match next {
        Some(n) if n > 0 => (into.min(n) as i32 * DOLL_PX / n as i32).max(0),
        _ => DOLL_PX,
    };
    fill(
        d,
        Rectangle::new(XP_BAR.top_left, Size::new(w as u32, XP_BAR.size.height)),
        pal::WARM,
    );
    let xp = match (k.xp_gain, next) {
        // A bar that has filled past its level shows the level coming, not "5/5 at Lv6" — a
        // state that cannot exist under 0030's flat 5 (it is already Lv7).
        (Some(g), Some(n)) if into >= n => crate::txt!("+{g} XP  level up!"),
        (Some(g), Some(n)) => crate::txt!("+{g} XP  {into}/{n}"),
        (Some(g), None) => crate::txt!("+{g} XP  max"),
        (None, Some(n)) => crate::txt!("XP {}/{n} to Lv{}", into.min(n), k.level + 1),
        (None, None) => crate::txt!("max level"),
    };
    label(
        d,
        &xp,
        Point::new(M, XP_TEXT_Y),
        tier::STATUS,
        Ink::Dim,
        Alignment::Left,
    );

    // The lobby shows the claim the arena will send (0029): progression's `derive_commander`. An
    // illegal loadout shows the base with a warning rather than numbers ClaimSeat would refuse.
    let claim = c.claim();
    let st = claim.unwrap_or(tapstone_rules::Commander::LEVEL_1);
    stats_line(d, DESK_STATS_Y, st.attack, st.toughness, st.toughness, true);
    if claim.is_err() {
        label(
            d,
            "loadout refused",
            Point::new(M, DESK_NOTE_Y),
            tier::STATUS,
            Ink::Warn,
            Alignment::Left,
        );
    } else if k.levelled {
        let g = commander::level_gain(k.level).unwrap_or("");
        let s = crate::txt!("Lv{}! {}", k.level, g);
        label(
            d,
            &s,
            Point::new(M, DESK_NOTE_Y),
            tier::STATUS,
            Ink::Warm,
            Alignment::Left,
        );
    } else if let Some(kw) = st.keyword {
        label(
            d,
            keyword_name(kw),
            Point::new(M, DESK_NOTE_Y),
            tier::STATUS,
            Ink::Dim,
            Alignment::Left,
        );
    }
}

fn desk<D: DrawTarget<Color = Rgb565>>(d: &mut D, k: &Desk<'_>) {
    blank(d);
    doll(d, DOLL, Some(&k.cmdr), k.pose, k.light);
    match k.title {
        Title::Lobby => title_line(d, &k.cmdr),
        Title::Result { word, won } => {
            let ink = match won {
                Some(true) => Ink::Good,
                Some(false) => Ink::Warn,
                None => Ink::WellText,
            };
            // A well, not bare text: the level-up light rises through this line.
            label(
                d,
                word,
                Point::new(M + 1, TITLE_Y),
                tier::STATUS,
                ink,
                Alignment::Left,
            );
            let lv = crate::txt!("Lv{}", k.level);
            label(
                d,
                &lv,
                Point::new(COL_R, TITLE_Y),
                tier::STATUS,
                Ink::WellDim,
                Alignment::Right,
            );
        }
    }
    desk_column(d, k);
    for s in SLOTS {
        draw_socket(d, s, k.sockets[s.index()], socket_rect(s));
    }
    if let Some((id, y)) = k.drop {
        draw_drop(d, id, y);
    }
    for (i, c) in k.grid.iter().enumerate() {
        draw_cell(d, *c, cell_rect(i));
    }
    voice::draw(d, &k.voice);
}

/// Screen 2 — Lobby / loadout.
pub fn lobby<D: DrawTarget<Color = Rgb565>>(d: &mut D, k: &Desk<'_>) {
    desk(d, k)
}

/// Screen 4 — Result: the lobby's skeleton, with the result word, the XP bar filling, the level-up line
/// and the loot chest in the inventory's first free cell.
pub fn result<D: DrawTarget<Color = Rgb565>>(d: &mut D, k: &Desk<'_>) {
    desk(d, k)
}

// ---------------------------------------------------------------------------------------------
// 3. The station, in match
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StationScreen<'a> {
    pub cmdr: Commander<'a>,
    pub st: Station,
    pub pose: Pose,
    pub voice: Voice<'a>,
    /// Draw each glanceable well from its own state (castle, mana, hand/round, sudden death), or
    /// all of them from `st` when `None`. A real engine record can change the commander *and*
    /// several wells at once (a death in end-of-turn combat arrives with the round change), and
    /// one rectangle per frame means each well updates on its own frame.
    pub wells: Option<[Station; WELL_COUNT]>,
}

/// The glanceable wells, in the order a motion settles them.
pub const WELL_COUNT: usize = 4;
pub const WELLS: [Rectangle; WELL_COUNT] = [CASTLE_WELL, MANA_WELL, HAND_WELL, PRESSURE_WELL];

/// Does well `i` draw differently for `a` than for `b`? Keyed on exactly what each well shows.
pub fn well_changed(a: &Station, b: &Station, i: usize) -> bool {
    match i {
        0 => {
            (a.castle_life, a.castle_design, a.castle_max)
                != (b.castle_life, b.castle_design, b.castle_max)
        }
        1 => (a.charged, a.spent, a.mana) != (b.charged, b.spent, b.mana),
        2 => (a.hand, a.round, a.stop_round) != (b.hand, b.round, b.stop_round),
        _ => {
            (a.round >= a.pressure_from, a.pressure_from)
                != (b.round >= b.pressure_from, b.pressure_from)
        }
    }
}

impl StationScreen<'_> {
    /// The figure's look and drop implied by its presence, before any animation overrides it.
    pub fn resting_pose(&self) -> Pose {
        match self.st.presence {
            // Owing a draw raises the free hand with a face-down card (0036), unless a motion is
            // already holding something else there.
            Presence::OnBoard { .. } if self.st.owed > 0 && self.pose.reach.is_none() => Pose {
                reach: Some(sprite::Held::Back),
                ..self.pose
            },
            Presence::OnBoard { .. } => self.pose,
            Presence::Fallen { .. } => Pose {
                look: Look::Ghost,
                dy: FALL_DROP,
                ..self.pose
            },
        }
    }
}

/// The commander's column: health bar, stats line, lane locator.
fn commander_column<D: DrawTarget<Color = Rgb565>>(d: &mut D, s: &StationScreen<'_>) {
    // The engine's commander, never the ledger's: what the station shows is what is in play.
    let st = s.st;
    let max = st.toughness.max(1);
    let hp = match s.st.presence {
        Presence::OnBoard { hp, .. } => hp.min(max),
        Presence::Fallen { .. } => 0,
    };
    // Segmented bar: one segment per point of toughness, so damage is countable at a glance.
    let n = max as i32;
    let gap = 2;
    let seg = (DOLL_PX - (n - 1) * gap) / n;
    let used = n * seg + (n - 1) * gap;
    let x0 = HP_BAR.top_left.x + (DOLL_PX - used) / 2;
    fill(d, HP_BAR, pal::BG);
    let lit = pal::health(hp, max);
    for i in 0..n {
        let c = if i < hp as i32 { lit } else { pal::DIMMED };
        fill(
            d,
            Rectangle::new(
                Point::new(x0 + i * (seg + gap), HP_BAR.top_left.y),
                Size::new(seg as u32, HP_BAR.size.height),
            ),
            c,
        );
    }

    match s.st.presence {
        Presence::OnBoard { .. } => {
            stats_line(d, STATION_STATS_Y, st.attack, hp, max, false);
            // 0034's one keyword, as a chip on the figure (inside the doll box, so any motion
            // that redraws the figure redraws the chip in the same rectangle).
            if let Some(kw) = st.keyword {
                label(
                    d,
                    keyword_name(kw),
                    Point::new(COL_R - 2, DOLL.top_left.y + 2),
                    tier::STATUS,
                    Ink::WellText,
                    Alignment::Right,
                );
            }
        }
        Presence::Fallen { .. } => {
            // Derived from the round, never stored: a blocked commander's return round is in the
            // past, and the station must say "waits", not "returns in 0" (or in 250).
            let (t, ink) = match s.st.return_state() {
                Some(ReturnState::In(n)) => (crate::txt!("returns in {n}"), Ink::Warm),
                Some(ReturnState::ThisTurn) => (crate::txt!("returns now"), Ink::Warm),
                _ => (crate::txt!("waits for cell"), Ink::Warn),
            };
            label(
                d,
                &t,
                Point::new(M, STATION_STATS_Y),
                tier::STATUS,
                ink,
                Alignment::Left,
            );
            // The "FALLEN" tag sits on the figure, inside the doll box.
            label(
                d,
                "FALLEN",
                Point::new(DOLL.top_left.x + DOLL_PX / 2, DOLL.top_left.y + 2),
                tier::STATUS,
                Ink::Warn,
                Alignment::Center,
            );
        }
    }
    locator(d, &s.st, s.cmdr.faction);
}

/// Top-left of locator cell (`lane`, `cell`); front (cell 2) is at the top, toward the enemy,
/// as the mat and the arena draw it.
pub fn loc_cell(lane: usize, cell: usize) -> Rectangle {
    let x = LOC_X + lane as i32 * (LOC_CELL + LOC_LGAP);
    let y = LOC_Y + (tapstone_rules::state::CELLS as i32 - 1 - cell as i32) * (LOC_CELL + LOC_CGAP);
    Rectangle::new(
        Point::new(x, y),
        Size::new(LOC_CELL as u32, LOC_CELL as u32),
    )
}

/// The locator's whole extent, including its label.
pub fn locator_rect() -> Rectangle {
    let bottom = loc_cell(0, 0);
    Rectangle::new(
        Point::new(LOC_X, LOC_Y),
        Size::new(
            (COL_R - LOC_X) as u32,
            (bottom.top_left.y + LOC_CELL - LOC_Y) as u32,
        ),
    )
}

fn locator<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    st: &Station,
    f: tapstone_rules::cards::Faction,
) {
    fill(d, locator_rect(), pal::BG);
    let blocked = st.return_state() == Some(ReturnState::Blocked);
    let (lane, cell, fallen) = match st.presence {
        Presence::OnBoard { lane, cell, .. } => (lane as usize, cell as usize, false),
        // A fallen commander returns to the back cell of the same lane (0029): show where.
        Presence::Fallen { lane, .. } => (lane as usize, 0, true),
    };
    for l in 0..tapstone_rules::state::LANES {
        for c in 0..tapstone_rules::state::CELLS {
            let r = loc_cell(l, c);
            if l == lane && c == cell {
                if fallen && blocked {
                    // The cell it needs is taken: filled in warn, the occupant it waits on.
                    fill(d, r, pal::WARN);
                } else if fallen {
                    stroke(d, r, pal::WARM, 1);
                } else {
                    fill(d, r, pal::faction(f));
                }
            } else {
                stroke(d, r, pal::DIMMED, 1);
            }
        }
    }
    let lane_s = crate::txt!("lane {}", lane + 1);
    label(
        d,
        &lane_s,
        Point::new(LOC_LABEL_X, LOC_Y + 1),
        tier::STATUS,
        Ink::Dim,
        Alignment::Left,
    );
    let where_s = if blocked {
        "waits"
    } else if fallen {
        "returns"
    } else {
        ["back", "middle", "front"][cell.min(2)]
    };
    label(
        d,
        where_s,
        Point::new(LOC_LABEL_X, LOC_Y + 17),
        tier::STATUS,
        Ink::Dim,
        Alignment::Left,
    );
}

fn well<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: Rectangle) {
    fill(d, r, pal::PANEL);
}

/// The glanceable wells (0032: castle life, mana charged/spent, hand count, round).
fn wells<D: DrawTarget<Color = Rgb565>>(d: &mut D, src: &[Station; WELL_COUNT]) {
    let x = WELL_X + 6;
    let xr = WELL_X + WELL_W - 6;

    // Castle.
    let st = &src[0];
    well(d, CASTLE_WELL);
    let y = CASTLE_WELL.top_left.y;
    label(
        d,
        "CASTLE",
        Point::new(x, y + 4),
        tier::STATUS,
        Ink::WellDim,
        Alignment::Left,
    );
    label(
        d,
        name_of(st.castle_design),
        Point::new(xr, y + 4),
        tier::STATUS,
        Ink::WellDim,
        Alignment::Right,
    );
    let life = crate::txt!("{}", st.castle_life);
    label(
        d,
        &life,
        Point::new(x, y + 24),
        tier::VALUE,
        Ink::health(st.castle_life, st.castle_max),
        Alignment::Left,
    );
    let of = crate::txt!("/{}", st.castle_max);
    let lx = x + life.len() as i32 * tier::VALUE.character_size.width as i32 + 3;
    label(
        d,
        &of,
        Point::new(lx, y + 30),
        tier::STATUS,
        Ink::WellDim,
        Alignment::Left,
    );

    // Mana: filled = available now, ring = spent this round (0007: charged is permanent).
    let st = &src[1];
    well(d, MANA_WELL);
    let y = MANA_WELL.top_left.y;
    label(
        d,
        "MANA",
        Point::new(x, y + 4),
        tier::STATUS,
        Ink::WellDim,
        Alignment::Left,
    );
    let m = crate::txt!("{} of {}", st.mana(), st.charged);
    label(
        d,
        &m,
        Point::new(xr, y + 4),
        tier::STATUS,
        Ink::WellText,
        Alignment::Right,
    );
    // Through inks, so the contrast test sees the pips too. The spent ring is 2 px: at 1 px it
    // was the station's likeliest 3:1 failure and the hardest mark to tell from "no pip".
    let shown = st.charged.min(MANA_PIPS_MAX as u8) as i32;
    for i in 0..shown {
        let c = Circle::new(Point::new(x + i * 16, y + 24), 12);
        if i < st.mana() as i32 {
            let _ = c
                .into_styled(PrimitiveStyle::with_fill(Ink::Mana.pair().0))
                .draw(d);
        } else {
            let _ = c
                .into_styled(PrimitiveStyle::with_stroke(Ink::Spent.pair().0, 2))
                .draw(d);
        }
    }

    // Hand and round.
    let st = &src[2];
    well(d, HAND_WELL);
    let y = HAND_WELL.top_left.y;
    let mid = WELL_X + WELL_W / 2;
    // In the mulligan window the HAND half of the well is a touch target (0009's touch path),
    // outlined so it reads as one; it is exactly 44 px tall and above the push-to-talk strip.
    let offer = st.mulligan_open && st.owed == 0;
    if offer {
        // Inside alignment: embedded-graphics centres a stroke on the edge by default, and a 2 px
        // centred stroke paints 1 px outside the well — outside the rectangle a motion pushes. The
        // frame-diff test caught it (240 px at x 111, y 115) before any picture did.
        let _ = mulligan_target()
            .into_styled(
                embedded_graphics::primitives::PrimitiveStyleBuilder::new()
                    .stroke_color(pal::WARM)
                    .stroke_width(2)
                    .stroke_alignment(embedded_graphics::primitives::StrokeAlignment::Inside)
                    .build(),
            )
            .draw(d);
    }
    label(
        d,
        if offer { "HAND: mulligan?" } else { "HAND" },
        Point::new(x, y + 4),
        tier::STATUS,
        if offer { Ink::Warm } else { Ink::WellDim },
        Alignment::Left,
    );
    let h = crate::txt!("{}", st.hand);
    label(
        d,
        &h,
        Point::new(x, y + 20),
        tier::VALUE,
        Ink::WellText,
        Alignment::Left,
    );
    // Owed draws beside the count: "+2" in warm, the cards the hand is still waiting for (0036).
    if st.owed > 0 {
        let owed = crate::txt!("+{}", st.owed);
        label(
            d,
            &owed,
            Point::new(
                x + h.len() as i32 * tier::VALUE.character_size.width as i32 + 4,
                y + 26,
            ),
            tier::STATUS,
            Ink::Warm,
            Alignment::Left,
        );
    }
    label(
        d,
        "ROUND",
        Point::new(mid, y + 4),
        tier::STATUS,
        Ink::WellDim,
        Alignment::Left,
    );
    let r = crate::txt!("{}/{}", st.round, st.stop_round);
    label(
        d,
        &r,
        Point::new(mid, y + 20),
        tier::VALUE,
        Ink::WellText,
        Alignment::Left,
    );

    // Sudden death: announced before it bites, gold once it does.
    let st = &src[3];
    well(d, PRESSURE_WELL);
    let y = PRESSURE_WELL.top_left.y + (PRESSURE_WELL.size.height as i32 - lh(tier::STATUS)) / 2;
    if st.round >= st.pressure_from {
        label(
            d,
            "SUDDEN DEATH",
            Point::new(x, y),
            tier::STATUS,
            Ink::Warm,
            Alignment::Left,
        );
    } else {
        let s = crate::txt!("sudden death from R{}", st.pressure_from);
        label(
            d,
            &s,
            Point::new(x, y),
            tier::STATUS,
            Ink::WellDim,
            Alignment::Left,
        );
    }
}

/// The number of owed draws, written on the face-down card the figure holds (0036). It rides the
/// card through the breath lift and any stagger, so it is always on the card and inside the doll
/// box — the rectangle every figure motion already pushes.
fn owed_numeral<D: DrawTarget<Color = Rgb565>>(d: &mut D, pose: Pose, owed: u8) {
    if owed == 0 || owed > 9 || pose.reach != Some(sprite::Held::Back) {
        return;
    }
    let k = DOLL_SCALE;
    let l = sprite::lift(pose.breath);
    // The card's inner face spans base x 6..11 and y 5..13 (lifted); centre the digit on it.
    let cx = DOLL.top_left.x + (6 + pose.dx) * k + 5 * k / 2;
    let top = DOLL.top_left.y + (5 - l + pose.dy) * k + 2;
    let n = crate::txt!("{owed}");
    label(
        d,
        &n,
        Point::new(cx, top),
        tier::STATUS,
        Ink::OnCard,
        Alignment::Center,
    );
}

/// The mulligan touch target: the HAND half of the hand/round well (0009's touch path).
///
/// It stops 6 px short of the well's midpoint, where the ROUND label starts: the first render ran
/// the outline through the "R".
pub fn mulligan_target() -> Rectangle {
    Rectangle::new(
        HAND_WELL.top_left,
        Size::new(WELL_W as u32 / 2 - 6, HAND_WELL.size.height),
    )
}

/// Mana pips a 16 px pitch fits in the well.
pub const MANA_PIPS_MAX: i32 = (WELL_W - 12) / 16;
/// Mana colour: off the faction axis and off the health ramp, so a pip never reads as either.
pub const MANA: Rgb565 = Rgb565::new(0x8f >> 3, 0xb8 >> 2, 0xff >> 3);

/// Screen 3 — In match — the station.
pub fn station<D: DrawTarget<Color = Rgb565>>(d: &mut D, s: &StationScreen<'_>) {
    blank(d);
    title_line(d, &s.cmdr);
    let pose = s.resting_pose();
    doll(d, DOLL, Some(&s.cmdr), pose, 0);
    owed_numeral(d, pose, s.st.owed);
    commander_column(d, s);
    wells(d, &s.wells.unwrap_or([s.st; WELL_COUNT]));
    voice::draw(d, &s.voice);
}

// ---------------------------------------------------------------------------------------------
// 5. Dark / waiting
// ---------------------------------------------------------------------------------------------

/// Screen 5 — Dark: the arena is gone, the shrine is dormant, or the battery is low. Mostly black on
/// purpose — a dark shrine should cost the panel and the eye as little as possible.
pub fn dark<D: DrawTarget<Color = Rgb565>>(d: &mut D, c: Option<&Commander<'_>>, why: voice::Dark) {
    blank(d);
    doll(
        d,
        IDLE_DOLL,
        c,
        Pose {
            look: Look::Ghost,
            ..Pose::default()
        },
        0,
    );
    if let voice::Dark::BatteryLow { pct } = why {
        // Battery glyph, top right.
        let r = Rectangle::new(Point::new(geom::W - M - 28, M + 2), Size::new(24, 12));
        stroke(d, r, pal::WARN, 1);
        fill(
            d,
            Rectangle::new(Point::new(geom::W - M - 4, M + 6), Size::new(2, 4)),
            pal::WARN,
        );
        let w = (22 * pct.min(100) as i32 / 100).max(1);
        fill(
            d,
            Rectangle::new(r.top_left + Point::new(1, 1), Size::new(w as u32, 10)),
            pal::WARN,
        );
    }
    voice::draw(d, &Voice::Dark(why));
}

/// Does the band's sentence agree with the state drawn beside it?
///
/// 0027's class: a readout and a sentence on one screen that say different things. The first
/// refusal fixture said "you have 1" beside a well reading "4 of 4". Every station frame is
/// checked against this in the preview tests, and a planted contradiction must fail it.
pub fn voice_agrees(v: &Voice<'_>, st: &Station) -> bool {
    // While a draw is owed the engine refuses everything else (0036), so the band must ask for
    // the draw (or acknowledge one, or relay the refusal): a whisper there hides the only thing
    // the player can do. The real engine fall at a round turn showed "returns next round" over an
    // owed turn-start draw.
    if st.owed > 0
        && !matches!(
            v,
            Voice::Draw { .. }
                | Voice::Drew { .. }
                | Voice::Refused(tapstone_rules::rules::Refusal::DrawOwed)
                | Voice::Listening
                | Voice::Heard { .. }
        )
    {
        return false;
    }
    match *v {
        Voice::Refused(tapstone_rules::rules::Refusal::NoMana { have, need }) => {
            have == st.mana() && need > have
        }
        Voice::Return(r) => st.return_state() == Some(r),
        Voice::Returned => matches!(st.presence, Presence::OnBoard { cell: 0, .. }),
        // 0036: the band's count is the station's count, and a draw prompt only while owing.
        Voice::Draw { n, .. } => n == st.owed && n > 0,
        Voice::Drew { left, .. } => left == st.owed,
        // The engine refuses anything but a draw while one is owed (0036, #63).
        Voice::Refused(tapstone_rules::rules::Refusal::DrawOwed) => st.owed > 0,
        Voice::Refused(tapstone_rules::rules::Refusal::NoDrawOwed) => st.owed == 0,
        Voice::MulliganOffer => st.mulligan_open && st.owed == 0,
        Voice::MulliganPrompt { .. } => st.mulligan_open && st.owed == 0,
        _ => true,
    }
}
