//! The battlefield, drawn from a real `Game`.
//!
//! # The advance axis
//!
//! Both orientations share one idea: cells are indexed along an **advance axis** that runs from
//! *my* castle to *theirs*. Index 0 is my back cell (`Seat::cells[lane][0]`, the one beside my own
//! castle); the last index is their back cell. Combat happens across the middle, where my front
//! cell meets theirs.
//!
//! Mapping that axis to pixels is the only place mirroring lives:
//!
//! * **Vertical** — my castle is at the bottom, so index 0 is the *bottom* row and the axis runs
//!   up the screen. Seat 0 and seat 1 therefore see mirrored boards from the same `Game`, which is
//!   the behaviour the UX doc asks for ("each shrine mirrors the shared state so 'mine' is always
//!   the bottom row").
//! * **Horizontal** — my castle is at the left, so index 0 is the *leftmost* cell.
//!
//! A spectator gets the arbiter's view: seat 0 is treated as "me" and nothing is mirrored.

use embedded_graphics::{
    mono_font::{MonoTextStyle, ascii},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use tapstone_rules::{
    cards::{self, Faction},
    state::{CELLS, Game, LANES, Unit},
};

use crate::geom::{self, Layout, Orientation};
use crate::palette as pal;

/// Which seat's shoulder we are looking over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// A player's own shrine: their units on the near side.
    Seat(u8),
    /// A third shrine watching: arbiter's view, unmirrored, both names shown.
    Spectator,
}

impl View {
    /// The seat treated as "near". A spectator borrows seat 0's frame without mirroring.
    pub fn near(&self) -> u8 {
        match self {
            View::Seat(s) => *s,
            View::Spectator => 0,
        }
    }
    pub fn far(&self) -> u8 {
        1 - self.near()
    }
    pub fn is_spectator(&self) -> bool {
        matches!(self, View::Spectator)
    }
}

/// One drawable position on the advance axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// Absolute seat index into `Game::seats`.
    pub seat: u8,
    /// Cell index within that seat's lane; 0 = back, `CELLS - 1` = front.
    pub cell: usize,
}

/// Map an advance-axis index to the seat and cell it shows.
///
/// `per_seat` is how many of each seat's cells the layout draws; when it is fewer than `CELLS`
/// the *front* cells are kept, because those are the ones combat reads.
pub fn slot(view: View, layout: &Layout, idx: usize) -> Slot {
    let n = layout.per_seat;
    debug_assert!(idx < n * 2, "index past the advance axis");
    let skipped = CELLS - n;
    if idx < n {
        // Near side: index rises away from my castle, so cell rises too.
        Slot {
            seat: view.near(),
            cell: idx + skipped,
        }
    } else {
        // Far side: we cross the middle, so their cells count back down toward their castle.
        Slot {
            seat: view.far(),
            cell: CELLS - 1 - (idx - n),
        }
    }
}

/// Pixel rect of one cell.
///
/// Slots are placed by accumulating the extents below them rather than by multiplying a single
/// row height, so a layout whose two halves differ (the asymmetric one) lands exactly.
pub fn slot_rect(layout: &Layout, lane: usize, idx: usize) -> Rectangle {
    let off = geom::slot_offset(layout, idx);
    let ext = geom::slot_extent(layout, idx);
    match layout.orientation {
        Orientation::Vertical => {
            // My wall is the bottom of the band; the axis runs up from it.
            let my_wall = geom::H - geom::HUD - geom::WALL;
            Rectangle::new(
                Point::new(geom::lane_x(lane), my_wall - off - ext),
                Size::new(layout.cell_cross as u32, ext as u32),
            )
        }
        Orientation::Horizontal => Rectangle::new(
            Point::new(geom::WALL + off, geom::lane_y(lane, layout.cell_cross)),
            Size::new(ext as u32, layout.cell_cross as u32),
        ),
    }
}

pub(crate) fn fill<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: Rectangle, c: Rgb565) {
    let _ = r.into_styled(PrimitiveStyle::with_fill(c)).draw(d);
}
pub(crate) fn stroke<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: Rectangle, c: Rgb565, w: u32) {
    let _ = r.into_styled(PrimitiveStyle::with_stroke(c, w)).draw(d);
}

/// Doc type tiers (13/14/17/22/26 px) mapped onto the mono fonts embedded-graphics ships.
/// The device will use its own glyph set; these are the nearest available stand-ins and are
/// labelled so the preview never implies a font decision it has not earned.
pub(crate) mod tier {
    use embedded_graphics::mono_font::{MonoFont, ascii};
    pub const STATUS: &MonoFont = &ascii::FONT_6X13; // doc 13 px
    pub const BODY: &MonoFont = &ascii::FONT_7X14; // doc 14 px
    pub const TURN: &MonoFont = &ascii::FONT_9X18; // doc 17 px
    pub const VALUE: &MonoFont = &ascii::FONT_10X20; // doc 22 px
}

/// Draw text with `at` as the **top-left** of the glyph box.
///
/// embedded-graphics defaults to an alphabetic baseline, which puts the glyphs *above* the given
/// point and silently clips them off the top of a band. Every call here anchors from the top so a
/// y-coordinate quoted from the UX doc means what the doc's wireframe means by it.
pub(crate) fn text<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    at: Point,
    font: &'static embedded_graphics::mono_font::MonoFont,
    c: Rgb565,
    align: Alignment,
) {
    let character_style = MonoTextStyle::new(font, c);
    let text_style = TextStyleBuilder::new()
        .alignment(align)
        .baseline(Baseline::Top)
        .build();
    let _ = Text::with_text_style(s, at, character_style, text_style).draw(d);
}

/// Height of one line in a tier, for the band-overflow audit.
pub(crate) fn line_h(font: &'static embedded_graphics::mono_font::MonoFont) -> i32 {
    font.character_size.height as i32
}

pub(crate) fn faction_of(design: u16) -> Faction {
    cards::design(design)
        .map(|d| d.faction)
        .unwrap_or(Faction::Neutral)
}

pub fn name_of(design: u16) -> &'static str {
    cards::design(design).map(|d| d.name).unwrap_or("?")
}

/// A deliberately unfinished sprite.
///
/// Decision 0014 calls for hand-made pixel art at 32 px base and 48 px heroes, and none exists.
/// Rather than invent art the game has not chosen, this draws a hatched block at exactly the size
/// and position the real sprite will occupy, so the layout is honest about what is undecided.
fn placeholder_sprite<D: DrawTarget<Color = Rgb565>>(d: &mut D, at: Point, size: i32, rim: Rgb565) {
    let r = Rectangle::new(at, Size::new(size as u32, size as u32));
    fill(d, r, pal::PANEL);
    stroke(d, r, rim, 1);
    // A single diagonal reads as "not art" at every size down to 12 px, where a cross would clot.
    let _ = Line::new(
        Point::new(at.x + 1, at.y + size - 2),
        Point::new(at.x + size - 2, at.y + 1),
    )
    .into_styled(PrimitiveStyle::with_stroke(pal::DIMMED, 1))
    .draw(d);
}

/// Health pips, capped at 5 as the doc requires ("pips <= 5, bar above").
fn pips<D: DrawTarget<Color = Rgb565>>(d: &mut D, at: Point, remaining: u8, max: u8, w: i32) {
    let c = pal::health(remaining, max);
    if max > 5 {
        // Bar, not pips.
        let full = Rectangle::new(at, Size::new(w as u32, 3));
        fill(d, full, pal::DIMMED);
        let n = if max == 0 {
            0
        } else {
            w * remaining as i32 / max as i32
        };
        if n > 0 {
            fill(d, Rectangle::new(at, Size::new(n as u32, 3)), c);
        }
        return;
    }
    for i in 0..max.min(5) {
        let x = at.x + i as i32 * 5;
        let r = Rectangle::new(Point::new(x, at.y), Size::new(4, 3));
        fill(d, r, if i < remaining { c } else { pal::DIMMED });
    }
}

/// Where a cell has room to put the attack/health readout.
///
/// The doc's wireframe always shows stats *beside* the sprite, which is true only when the cell is
/// much wider than the sprite. A 49 px cell holding a 48 px sprite has no such room, so the
/// placement has to be derived from the cell rather than assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatPlacement {
    /// To the right of the sprite, as the doc draws it.
    Beside,
    /// Under the sprite, inside the cell.
    Below,
    /// On top of the sprite; the last resort, and the doc's own stated fallback.
    Overlay,
}

/// Decide stat placement for a cell holding a sprite of `sprite` px.
pub fn stat_placement(cell_w: i32, cell_h: i32, sprite: i32) -> StatPlacement {
    if cell_w - sprite - 4 >= 26 {
        StatPlacement::Beside
    } else if cell_h - sprite - 2 >= 12 {
        StatPlacement::Below
    } else {
        StatPlacement::Overlay
    }
}

/// Below this extent a cell cannot hold a sprite plus a readable stat line, so it becomes a chip.
pub const CHIP_MAX: i32 = 28;

/// A read-only stat chip: no sprite, just the facts, for a cell the player never acts on.
///
/// Used by the asymmetric layout's far side. Carries what a glance needs — faction (the rim),
/// attack, current health and the keyword tag — in a band too short for pixel art.
fn draw_chip<D: DrawTarget<Color = Rgb565>>(d: &mut D, cell: Rectangle, u: &Unit, mark: Mark) {
    let rim = pal::faction(faction_of(u.design));
    let (cw, ch) = (cell.size.width as i32, cell.size.height as i32);
    let top = cell.top_left;
    let remaining = u.toughness.saturating_sub(u.damage);

    // Faction rim down the leading edge, then a panel ground.
    fill(
        d,
        Rectangle::new(
            Point::new(top.x + 1, top.y + 1),
            Size::new((cw - 2) as u32, (ch - 2) as u32),
        ),
        pal::PANEL,
    );
    fill(
        d,
        Rectangle::new(
            Point::new(top.x + 1, top.y + 1),
            Size::new(3, (ch - 2) as u32),
        ),
        rim,
    );

    let ty = top.y + (ch - line_h(tier::STATUS)) / 2;
    text(
        d,
        &crate::txt!("{}/{}", u.attack, remaining),
        Point::new(top.x + 7, ty),
        tier::STATUS,
        pal::TEXT,
        Alignment::Left,
    );
    // Health colour carries the damage state without a bar.
    fill(
        d,
        Rectangle::new(
            Point::new(top.x + 7 + 5 * 6, top.y + ch / 2 - 2),
            Size::new(4, 4),
        ),
        pal::health(remaining, u.toughness),
    );
    // "NEW" marks a unit summoned this round or last: the 44 px gap between the health dot and
    // the keyword tag is free, so this costs no space. Note it means newly *summoned*, not newly
    // arrived in the front cell - `entered_round` is set on entry and `advance` never touches it.
    // The 44 px between the health dot and the keyword tag is free, so a mark costs no space.
    if let Some((label, colour)) = match mark {
        Mark::Front => Some(("FRONT", pal::WARN)),
        Mark::New => Some(("NEW", pal::WARM)),
        Mark::None => None,
    } {
        text(
            d,
            label,
            Point::new(top.x + 46, ty),
            tier::STATUS,
            colour,
            Alignment::Left,
        );
    }
    if let Some(k) = u.keyword {
        text(
            d,
            kw(k),
            Point::new(top.x + cw - 3, ty),
            tier::STATUS,
            pal::TEXT_DIM,
            Alignment::Right,
        );
    }
}

/// What a cell is flagged with, if anything.
///
/// Ruling (0027): "just reached the front" is a **view** concern, not a rules one, so it does not
/// belong in the hashed image. The shrine applies every committed record, so it can keep the
/// previous `Game` — 350 bytes and `Copy`, one frame back costs nothing — and derive arrivals by
/// diffing. Nothing enters the canonical image, no golden moves, the protocol is untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    None,
    /// Summoned this round or last. Derivable from a single state.
    New,
    /// Reached the front cell since the previous frame. Requires two states, which is exactly why
    /// it is derived here rather than stored.
    Front,
}

/// Decide a cell's mark.
///
/// `prev` is the previous frame. A shrine joining mid-game has none, so it shows no arrival
/// markers until the next turn — the correct degradation, since inventing one would mean claiming
/// a unit just arrived when it may have stood there for three rounds.
pub fn mark_for(
    prev: Option<&Game>,
    seat: u8,
    lane: usize,
    cell: usize,
    u: &Unit,
    round: u8,
) -> Mark {
    if cell == CELLS - 1 && arrived_at_front(prev, seat, lane, u) {
        Mark::Front
    } else if is_new(u, round) {
        Mark::New
    } else {
        Mark::None
    }
}

/// Did this unit reach the front cell since `prev`?
///
/// Identity is `design` plus `entered_round`: two copies of the same card summoned on different
/// rounds are different units, and the same unit advancing keeps both fields.
pub fn arrived_at_front(prev: Option<&Game>, seat: u8, lane: usize, u: &Unit) -> bool {
    let Some(p) = prev else {
        return false;
    };
    match p.seats[seat as usize].cells[lane][CELLS - 1] {
        Some(before) => before.design != u.design || before.entered_round != u.entered_round,
        None => true,
    }
}

/// Was this unit summoned this round or the previous one?
///
/// This is "summoned", not "arrived": `Unit::entered_round` records entry into play and `advance`
/// never updates it, so a unit that walked to the front cell is not new by this test. What one
/// state can answer is "summoned this round"; "arrived at the front" needs two, and that is
/// `arrived_at_front` above.
pub fn is_new(u: &Unit, round: u8) -> bool {
    round.saturating_sub(u.entered_round) <= 1
}

/// Draw one unit into its cell.
fn draw_unit<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    cell: Rectangle,
    u: &Unit,
    layout: &Layout,
    mark: Mark,
) {
    let rim = pal::faction(faction_of(u.design));
    let (cw, ch) = (cell.size.width as i32, cell.size.height as i32);
    // A cell too short for a sprite becomes a stat chip rather than a squashed picture.
    let extent = match layout.orientation {
        Orientation::Vertical => ch,
        Orientation::Horizontal => cw,
    };
    if extent < CHIP_MAX {
        draw_chip(d, cell, u, mark);
        return;
    }
    let sprite = layout.sprite.min(cw - 2).min(ch - 2);
    let top = cell.top_left;
    let remaining = u.toughness.saturating_sub(u.damage);
    let stats = crate::txt!("{}/{}", u.attack, remaining);
    let place = stat_placement(cw, ch, sprite);

    // The mark has to be drawn on BOTH sides of the board. Chips draw it themselves; a full-size
    // cell has to as well, or every mark on the player's own units is silently dropped - which is
    // exactly what happened the first time, and only the render showed it.
    if let Some((label, colour)) = match mark {
        Mark::Front => Some(("FRONT", pal::WARN)),
        Mark::New => Some(("NEW", pal::WARM)),
        Mark::None => None,
    } {
        text(
            d,
            label,
            Point::new(top.x + cw - 2, top.y + 1),
            tier::STATUS,
            colour,
            Alignment::Right,
        );
    }

    // The sprite sits where the stats are not.
    let sy = match place {
        StatPlacement::Below => top.y + 1,
        _ => top.y + (ch - sprite) / 2,
    };
    placeholder_sprite(d, Point::new(top.x + 1, sy), sprite, rim);

    match place {
        StatPlacement::Beside => {
            // The doc draws value, pips and keyword as three stacked lines beside the sprite.
            // That stack needs about 34 px of height; a short cell has to shed lines from the
            // bottom up rather than overprint them.
            let sx = top.x + 1 + sprite + 3;
            let room = top.x + cw - sx;
            text(
                d,
                &stats,
                Point::new(sx, top.y + 3),
                tier::BODY,
                pal::TEXT,
                Alignment::Left,
            );
            let after_value = 3 + line_h(tier::BODY) + 2;
            if ch - after_value >= 4 {
                pips(
                    d,
                    Point::new(sx, top.y + after_value),
                    remaining,
                    u.toughness,
                    room - 2,
                );
                let after_pips = after_value + 6;
                if let Some(k) = u.keyword
                    && room >= 52
                    && ch - after_pips >= line_h(tier::STATUS)
                {
                    text(
                        d,
                        kw(k),
                        Point::new(sx, top.y + after_pips),
                        tier::STATUS,
                        pal::TEXT_DIM,
                        Alignment::Left,
                    );
                }
            }
        }
        StatPlacement::Below => {
            // A strip under the sprite, clamped so it cannot reach the next lane.
            let sy2 = (sy + sprite + 1).min(top.y + ch - line_h(tier::STATUS));
            text(
                d,
                &stats,
                Point::new(top.x + 2, sy2),
                tier::STATUS,
                pal::TEXT,
                Alignment::Left,
            );
            // Keywords lose their own line here; the rim colour and a 3-letter tag share the strip.
            if let Some(k) = u.keyword {
                text(
                    d,
                    kw(k),
                    Point::new(top.x + cw - 2, sy2),
                    tier::STATUS,
                    pal::TEXT_DIM,
                    Alignment::Right,
                );
            }
        }
        StatPlacement::Overlay => {
            // Sits on the sprite's lower edge, inside the cell by construction.
            let sy2 = top.y + ch - line_h(tier::STATUS) - 1;
            fill(
                d,
                Rectangle::new(
                    Point::new(top.x + 1, sy2),
                    Size::new((cw - 2) as u32, line_h(tier::STATUS) as u32),
                ),
                pal::BG,
            );
            text(
                d,
                &stats,
                Point::new(top.x + 2, sy2),
                tier::STATUS,
                pal::TEXT,
                Alignment::Left,
            );
        }
    }
}

fn kw(k: tapstone_rules::cards::Keyword) -> &'static str {
    use tapstone_rules::cards::Keyword::*;
    match k {
        Ranged => "RNG",
        Shield1 => "SHD",
        Haste => "HST",
        Rush => "RSH",
        Taunt => "TNT",
    }
}

/// Options that change what the battlefield says without changing where anything sits.
#[derive(Clone, Copy, Debug, Default)]
pub struct Opts<'a> {
    /// The previous frame, if this shrine has one. Used only to derive arrival marks.
    pub prev: Option<&'a Game>,
    /// Sudden death: both HUD bands go gold with a hazard rule.
    pub sudden_death: bool,
    /// Combat resolution: the prompt line locks and names the lane.
    pub resolving: Option<usize>,
    /// Targeting: legal cells keep full brightness, everything else drops a palette step.
    pub targeting: bool,
    /// An overlay message on the bottom band (disconnect, low battery).
    pub overlay: Option<&'static str>,
}

/// Draw the battlefield for `view` from `game` into `d`.
///
/// Takes the target rather than returning one: firmware owns its framebuffer, and a renderer
/// that allocates a surface cannot run on a device that has exactly one.
pub fn draw<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    game: &Game,
    view: View,
    layout: &Layout,
    opts: &Opts<'_>,
) {
    let d = &mut *d;
    fill(
        d,
        Rectangle::new(Point::zero(), Size::new(geom::W as u32, geom::H as u32)),
        pal::BG,
    );

    let near = view.near();
    let far = view.far();
    let near_f = pal::faction(faction_of(game.seats[near as usize].castle_design));
    let far_f = pal::faction(faction_of(game.seats[far as usize].castle_design));

    // --- HUD bands -------------------------------------------------------------------------
    let hud_far = Rectangle::new(Point::zero(), Size::new(geom::W as u32, geom::HUD as u32));
    let hud_near = Rectangle::new(
        Point::new(0, geom::H - geom::HUD),
        Size::new(geom::W as u32, geom::HUD as u32),
    );
    // Whose turn is told three ways; the tint is one of them.
    let active_is_near = game.active == near;
    let tint = |on: bool, f: Rgb565| {
        if opts.sudden_death {
            pal::WARM
        } else if on {
            f
        } else {
            pal::PANEL
        }
    };
    fill(d, hud_far, tint(!active_is_near, far_f));
    fill(d, hud_near, tint(active_is_near, near_f));
    if opts.sudden_death {
        // "both HUD bands turn the warm token (gold) with a thin hazard rule" - the tint is the
        // message, so the rule is one dashed line per band and nothing moves.
        crate::screens::hazard(d, geom::HUD - 1);
        crate::screens::hazard(d, geom::H - geom::HUD);
    }

    // The doc's wireframe puts each band's contents on ONE line:
    //   top:    NAME            <3 life  <>pips              T7 (timer)
    //   bottom: <3 life  <>pips  YOUR TURN - tap a card, touch a lane to aim
    // so a band overruns horizontally, never vertically. `hud_band` lays the row out left to
    // right and returns the x it needed, which `hud_overflow` turns into a measurement.
    let hud_band = |d: &mut D, y: i32, seat: u8, name: bool, tinted: bool| -> i32 {
        let s = &game.seats[seat as usize];
        // A tinted band is a light ground, and every foreground the HUD uses - the health ramp,
        // TEXT, the mana pips - measures under 2:1 against Ember, Tide or the sudden-death gold.
        // Measured, not guessed: HEALTH_MID on WARM is 1.13:1. So the readouts sit in a dark well
        // and the tint stays a band-level signal, which is what it is for: whose turn it is, read
        // from across the table. Tint and foreground can never both carry meaning in one place.
        if tinted {
            fill(
                d,
                Rectangle::new(
                    Point::new(2, y + 2),
                    Size::new((geom::W - 4) as u32, (geom::HUD - 4) as u32),
                ),
                pal::PANEL,
            );
        }
        let mut x = 4;
        if name {
            let n = name_of(s.castle_design);
            text(
                d,
                n,
                Point::new(x, y + (geom::HUD - line_h(tier::STATUS)) / 2),
                tier::STATUS,
                pal::TEXT,
                Alignment::Left,
            );
            x += n.len() as i32 * tier::STATUS.character_size.width as i32 + 8;
        }
        let life = crate::txt!("{}", s.castle.life);
        text(
            d,
            &life,
            Point::new(x, y + (geom::HUD - line_h(tier::VALUE)) / 2),
            tier::VALUE,
            pal::health(s.castle.life, game.rules.castle_life),
            Alignment::Left,
        );
        x += life.len() as i32 * tier::VALUE.character_size.width as i32 + 8;
        // Mana pips: charged, spent shown hollow.
        for i in 0..s.charged.min(8) {
            let r = Rectangle::new(
                Point::new(x + i as i32 * 7, y + geom::HUD / 2 - 2),
                Size::new(5, 5),
            );
            fill(
                d,
                r,
                if i < s.available_mana() {
                    pal::TEXT
                } else {
                    pal::DIMMED
                },
            );
        }
        x + s.charged.min(8) as i32 * 7 + 6
    };
    let far_tinted = !active_is_near || opts.sudden_death;
    let near_tinted = active_is_near || opts.sudden_death;
    hud_band(d, 0, far, true, far_tinted);
    let near_hud_end = hud_band(d, geom::H - geom::HUD, near, false, near_tinted);
    text(
        d,
        &crate::txt!("T{}", game.round),
        Point::new(geom::W - 4, (geom::HUD - line_h(tier::TURN)) / 2),
        tier::TURN,
        pal::TEXT,
        Alignment::Right,
    );

    // --- Walls -----------------------------------------------------------------------------
    match layout.orientation {
        Orientation::Vertical => {
            fill(
                d,
                Rectangle::new(
                    Point::new(0, geom::HUD),
                    Size::new(geom::W as u32, geom::WALL as u32),
                ),
                far_f,
            );
            fill(
                d,
                Rectangle::new(
                    Point::new(0, geom::H - geom::HUD - geom::WALL),
                    Size::new(geom::W as u32, geom::WALL as u32),
                ),
                near_f,
            );
        }
        Orientation::Horizontal => {
            fill(
                d,
                Rectangle::new(
                    Point::new(0, geom::HUD),
                    Size::new(geom::WALL as u32, geom::BETWEEN_HUD as u32),
                ),
                near_f,
            );
            fill(
                d,
                Rectangle::new(
                    Point::new(geom::W - geom::WALL, geom::HUD),
                    Size::new(geom::WALL as u32, geom::BETWEEN_HUD as u32),
                ),
                far_f,
            );
        }
    }

    // --- Cells -----------------------------------------------------------------------------
    let n2 = layout.per_seat * 2;
    for lane in 0..LANES {
        for idx in 0..n2 {
            let rect = slot_rect(layout, lane, idx);
            let s = slot(view, layout, idx);
            let occupant = game.seats[s.seat as usize].cells[lane][s.cell];

            // The midline: where my front cell faces theirs.
            let is_midline = idx == layout.per_seat;
            if is_midline {
                match layout.orientation {
                    Orientation::Vertical => {
                        let _ = Line::new(
                            Point::new(rect.top_left.x, rect.top_left.y + rect.size.height as i32),
                            Point::new(
                                rect.top_left.x + rect.size.width as i32,
                                rect.top_left.y + rect.size.height as i32,
                            ),
                        )
                        .into_styled(PrimitiveStyle::with_stroke(pal::DIMMED, 1))
                        .draw(d);
                    }
                    Orientation::Horizontal => {
                        let _ = Line::new(
                            rect.top_left,
                            Point::new(rect.top_left.x, rect.top_left.y + rect.size.height as i32),
                        )
                        .into_styled(PrimitiveStyle::with_stroke(pal::DIMMED, 1))
                        .draw(d);
                    }
                }
            }

            match occupant {
                Some(u) => {
                    let mark = mark_for(opts.prev, s.seat, lane, s.cell, &u, game.round);
                    draw_unit(d, rect, &u, layout, mark)
                }
                None => {
                    // Empty cell: the doc's three centred dots.
                    let cx = rect.top_left.x + rect.size.width as i32 / 2;
                    let cy = rect.top_left.y + rect.size.height as i32 / 2;
                    for k in -1..=1 {
                        fill(
                            d,
                            Rectangle::new(Point::new(cx + k * 6 - 1, cy - 1), Size::new(2, 2)),
                            pal::DIMMED,
                        );
                    }
                }
            }
        }

        // Lane separators, which is where the two unaccounted pixels of 320 go.
        if layout.orientation == Orientation::Vertical && lane + 1 < LANES {
            let x = geom::lane_x(lane) + geom::LANE_CROSS_V;
            let _ = Line::new(
                Point::new(x, geom::CELL_BAND_TOP),
                Point::new(x, geom::H - geom::HUD - geom::WALL),
            )
            .into_styled(PrimitiveStyle::with_stroke(pal::DIMMED, 1))
            .draw(d);
        }
    }

    // --- Lane labels ------------------------------------------------------------------------
    // Decision 0018 puts three pads side by side across the apron, so lane is a physical
    // left-to-right position. Labelling the lanes makes the mapping visible: a vertical layout
    // reads L1 L2 L3 across the glass exactly as the pads sit; the horizontal layout stacks them
    // top to bottom, so the player has to translate on every glance.
    for lane in 0..LANES {
        let tag = crate::txt!("L{}", lane + 1);
        match layout.orientation {
            Orientation::Vertical => {
                // Across my own wall, left to right — the same order as the pads.
                text(
                    d,
                    &tag,
                    Point::new(
                        geom::lane_x(lane) + geom::LANE_CROSS_V / 2,
                        geom::H - geom::HUD - geom::WALL + 1,
                    ),
                    tier::STATUS,
                    pal::BG,
                    Alignment::Center,
                );
            }
            Orientation::Horizontal => {
                // Down my wall, top to bottom — rotated from the pads.
                let y = geom::lane_y(lane, layout.cell_cross) + layout.cell_cross / 2
                    - line_h(tier::STATUS) / 2;
                text(
                    d,
                    &tag,
                    Point::new(1, y),
                    tier::STATUS,
                    pal::BG,
                    Alignment::Left,
                );
            }
        }
    }

    // --- Prompt line -----------------------------------------------------------------------
    let near_seat = &game.seats[near as usize];
    let p = if let Some(l) = opts.resolving {
        Prompt::Resolving(l)
    } else if let Some(o) = opts.overlay {
        Prompt::Overlay(o)
    } else if view.is_spectator() {
        Prompt::Spectator {
            a: name_of(game.seats[0].castle_design),
            b: name_of(game.seats[1].castle_design),
        }
    } else if opts.targeting {
        Prompt::Targeting {
            card: name_of(5),
            secs: 5,
        }
    } else if active_is_near {
        Prompt::YourTurn
    } else {
        Prompt::TheirTurn
    };
    // Fitted against what actually shares the band in THIS frame, not a tuned constant.
    let prompt = fit(
        &p.text(),
        prompt_budget_chars(near_seat.castle.life, near_seat.charged),
    );
    let prompt_colour = if opts.overlay.is_some() {
        pal::WARN
    } else {
        pal::TEXT
    };
    // The prompt shares the bottom band with life and mana, as the doc's wireframe shows, so it
    // starts where those finish. Anything past x=320 is the overflow the wireframe could not show.
    text(
        d,
        &prompt,
        Point::new(
            near_hud_end,
            geom::H - geom::HUD + (geom::HUD - line_h(tier::STATUS)) / 2,
        ),
        tier::STATUS,
        prompt_colour,
        Alignment::Left,
    );

    let _ = ascii::FONT_6X10;
}

/// Every prompt the battlefield can put in its bottom band.
///
/// This exists because a previous test claimed to check "every prompt the battlefield can show"
/// while listing four string literals, and the targeting prompt — built inline and 50 characters
/// long — was not among them. It clipped. The lesson is that an enumeration written by hand beside
/// the thing it enumerates will fall behind it, so the list now lives with the construction and
/// the compiler is made to care: `Prompt::index` is an exhaustive match, so a new variant will not
/// compile until it is handled, and `covers_every_variant` fails until `ALL` names it too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt<'a> {
    YourTurn,
    TheirTurn,
    /// End-of-turn combat, one lane at a time; touch is ignored.
    Resolving(usize),
    /// A tapped card wants a target. `card` is a real card name, which is why the budget matters.
    Targeting {
        card: &'a str,
        secs: u8,
    },
    /// The arbiter's view names both castles instead of prompting.
    Spectator {
        a: &'a str,
        b: &'a str,
    },
    /// Disconnect or low battery, overlaid on the band.
    Overlay(&'a str),
}

/// Number of variants. Raising this is the second half of adding one; the first half is
/// `Prompt::index`, which will not compile until the new variant is matched.
pub const PROMPT_VARIANTS: usize = 6;

impl<'a> Prompt<'a> {
    /// Exhaustive by construction: adding a variant breaks this match at compile time.
    pub fn index(&self) -> usize {
        match self {
            Prompt::YourTurn => 0,
            Prompt::TheirTurn => 1,
            Prompt::Resolving(_) => 2,
            Prompt::Targeting { .. } => 3,
            Prompt::Spectator { .. } => 4,
            Prompt::Overlay(_) => 5,
        }
    }

    /// The untruncated text.
    pub fn text(&self) -> crate::fmt::Text {
        match self {
            Prompt::YourTurn => crate::txt!("{PROMPT}"),
            Prompt::TheirTurn => crate::txt!("their turn"),
            Prompt::Resolving(lane) => crate::txt!("RESOLVING - lane {}", lane + 1),
            Prompt::Targeting { card, secs } => {
                crate::txt!("{card} > target  default: weakest {secs}s")
            }
            Prompt::Spectator { a, b } => crate::txt!("{a} vs {b}"),
            Prompt::Overlay(msg) => crate::txt!("{msg}"),
        }
    }

    /// The worst case of each variant, for tests: the longest real card and castle names, so a
    /// prompt cannot pass the check with a short name and clip with a long one.
    pub const WORST: [Prompt<'static>; PROMPT_VARIANTS] = [
        Prompt::YourTurn,
        Prompt::TheirTurn,
        Prompt::Resolving(LANES - 1),
        Prompt::Targeting {
            card: "Pearl Shieldbearer",
            secs: 5,
        },
        Prompt::Spectator {
            a: "Ember Castle",
            b: "Tide Castle",
        },
        Prompt::Overlay("shrines disagree - match void"),
    ];
}

/// Characters the prompt may use, given what else shares its 24 px line.
///
/// Derived from the same arithmetic as `bottom_band_width` rather than a tuned constant, so a
/// change to the life tier or the mana pips moves the budget instead of silently overflowing it.
pub fn prompt_budget_chars(life: u8, mana: u8) -> usize {
    let used = 4
        + crate::txt!("{life}").len() as i32 * tier::VALUE.character_size.width as i32
        + 8
        + mana.min(8) as i32 * 7
        + 6;
    let px = (geom::W - used).max(0);
    (px / tier::STATUS.character_size.width as i32) as usize
}

/// Truncate to `budget` characters, marking the cut so a clipped prompt is visibly clipped rather
/// than silently ending mid-word at the panel edge.
pub fn fit(s: &str, budget: usize) -> crate::fmt::Text {
    let mut out = crate::fmt::Text::new();
    if s.chars().count() <= budget {
        for c in s.chars() {
            let _ = out.push(c);
        }
        return out;
    }
    if budget <= 2 {
        for _ in 0..budget {
            let _ = out.push('.');
        }
        return out;
    }
    for c in s.chars().take(budget - 2) {
        let _ = out.push(c);
    }
    let _ = out.push('.');
    let _ = out.push('.');
    out
}

/// The bottom band as the UX doc specifies it: life at the 22 px value tier, mana pips, then the
/// full prompt sentence at the 13 px status tier — all on one 24 px line.
///
/// Returns the width that row actually needs. The panel is 320 px; anything above that is text the
/// wireframe's 64-character grid quietly absorbed.
pub fn bottom_band_width(life: u8, mana: u8, prompt: &str) -> i32 {
    let mut x = 4;
    x += crate::txt!("{life}").len() as i32 * tier::VALUE.character_size.width as i32 + 8;
    x += mana.min(8) as i32 * 7 + 6;
    x + prompt.len() as i32 * tier::STATUS.character_size.width as i32
}

/// The prompt the doc prints in its own battlefield wireframe.
pub const DOC_PROMPT: &str = "YOUR TURN - tap a card, touch a lane to aim";

/// The shortened prompt that fits. Ruling: shorten the sentence rather than drop a type tier -
/// life and mana are glanceable state that must survive at speed, while the prompt is read once.
pub const PROMPT: &str = "YOUR TURN - tap a card, touch to aim";

/// The status tier, for callers outside the crate (tests that plant a label to prove a check).
pub fn tier_status() -> &'static embedded_graphics::mono_font::MonoFont<'static> {
    tier::STATUS
}
