//! The card-to-castle flourish: a card resolving from the pad into the castle it affects.
//!
//! Decision 0025 gives the shrine small animated pixel sprites — JP's phrasing was "like cards to
//! castles" — and decision 0010 budgets one animated band at a time at roughly 15 fps. This
//! renders the motion as a frame sequence through the same `embedded-graphics` path the firmware
//! will use, and reports the **dirty region per frame** so it can be costed rather than admired.
//!
//! # The design rule the cost model imposes
//!
//! Per-window overhead dominates (`cost.rs`), so the rule is **one contiguous dirty rectangle per
//! frame**. That is not a stylistic preference; it is the difference between a frame that costs
//! about 2 ms and one that costs 2× a full repaint. Every phase below is shaped to obey it, and
//! `dirty()` returns exactly one rectangle for every frame so the claim is checkable rather than
//! asserted.
//!
//! The naive optimisation — redraw only the moving sprite — is not the cheap choice here, and the
//! naive worry — "it crosses the whole screen, so it must be expensive" — is also wrong. What
//! matters is windows first and pixels second, and a lane column is only 106 px wide.
//!
//! # Art
//!
//! Decision 0014 wants hand-made pixel art that does not exist, so the travelling token is a
//! clearly-provisional card-shaped block at the size the real sprite will occupy. No style is
//! invented and nothing is generated.

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
};
use tapstone_rules::state::Game;

use crate::battlefield::{self, Opts, View, fill};
use crate::geom::{self, Layout};
use crate::palette as pal;

/// Token size: a card silhouette, proportioned like a CR80 card rather than square, so it reads
/// as "a card moving" instead of "a sprite moving".
pub const TOKEN_W: i32 = 22;
pub const TOKEN_H: i32 = 30;

/// Frames per phase, chosen against the UX doc's own motion table (summon drop is 6 frames /
/// 400 ms, hit flash 2, death dissolve 5) so this sits in the same vocabulary.
pub const RISE_FRAMES: usize = 7;
pub const IMPACT_FRAMES: usize = 3;
pub const TICK_FRAMES: usize = 2;
pub const TOTAL_FRAMES: usize = RISE_FRAMES + IMPACT_FRAMES + TICK_FRAMES;

/// What a frame is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The token travels up its lane toward the enemy castle.
    Rise,
    /// The wall takes the hit: flash and a ±2 px shake.
    Impact,
    /// The castle life readout ticks down.
    Tick,
}

/// One flourish, aimed at a lane.
#[derive(Clone, Copy, Debug)]
pub struct Flourish {
    pub lane: usize,
    /// Card being resolved, for the token's rim colour.
    pub design: u16,
    /// Damage dealt, shown by the tick.
    pub amount: u8,
}

impl Flourish {
    pub fn phase(&self, frame: usize) -> Phase {
        if frame < RISE_FRAMES {
            Phase::Rise
        } else if frame < RISE_FRAMES + IMPACT_FRAMES {
            Phase::Impact
        } else {
            Phase::Tick
        }
    }

    /// Top-left of the token on `frame`, travelling from my wall to the enemy wall.
    ///
    /// The path is the lane's own column, so the dirty region stays 106 px wide however far the
    /// token travels — which is why crossing the panel vertically is affordable.
    pub fn token_at(&self, frame: usize) -> Point {
        let start_y = geom::H - geom::HUD - geom::WALL - TOKEN_H; // sitting on my wall
        let end_y = geom::HUD + geom::WALL; // touching the enemy wall
        let t = (frame.min(RISE_FRAMES - 1)) as i32;
        let span = (RISE_FRAMES - 1) as i32;
        let y = start_y + (end_y - start_y) * t / span;
        let x = geom::lane_x(self.lane) + (geom::LANE_CROSS_V - TOKEN_W) / 2;
        Point::new(x, y)
    }

    /// The single contiguous rectangle this frame dirties.
    ///
    /// One rectangle, always — that is the whole point. During the rise it is the union of the
    /// token's previous and current positions, so the trail is erased in the same window that
    /// draws the token. During impact and tick it is the enemy wall and HUD, which are adjacent,
    /// so they merge into one band rather than costing two windows.
    pub fn dirty(&self, frame: usize) -> Rectangle {
        match self.phase(frame) {
            Phase::Rise => {
                let now = self.token_at(frame);
                let prev = self.token_at(frame.saturating_sub(1));
                let top = now.y.min(prev.y);
                let bottom = (now.y + TOKEN_H).max(prev.y + TOKEN_H);
                Rectangle::new(
                    Point::new(geom::lane_x(self.lane), top),
                    Size::new(geom::LANE_CROSS_V as u32, (bottom - top) as u32),
                )
            }
            // The enemy HUD (y 0..24) and wall (y 24..36) are contiguous, so one band covers the
            // flash, the shake and the life tick. Two px of slack absorbs the shake.
            Phase::Impact | Phase::Tick => Rectangle::new(
                Point::zero(),
                Size::new(geom::W as u32, (geom::HUD + geom::WALL + 2) as u32),
            ),
        }
    }

    /// Render one frame over the board `game`.
    ///
    /// The tick phase renders a board whose castle life has **already** dropped, so the sequence
    /// actually shows the consequence rather than flashing and leaving the number unchanged. An
    /// animation that depicts a hit with no result is worse than no animation: it teaches the
    /// player that the flourish means nothing.
    pub fn draw_frame<D: DrawTarget<Color = Rgb565>>(
        &self,
        d: &mut D,
        game: &Game,
        view: View,
        layout: &Layout,
        frame: usize,
    ) {
        let phase = self.phase(frame);
        let far = view.far() as usize;

        // The struck castle loses its life before the tick frames are drawn.
        let shown = if phase == Phase::Tick {
            let mut g = *game;
            g.seats[far].castle.life = g.seats[far].castle.life.saturating_sub(self.amount);
            g
        } else {
            *game
        };

        battlefield::draw(d, &shown, view, layout, &Opts::default());
        let rim = pal::faction(
            tapstone_rules::cards::design(self.design)
                .map(|c| c.faction)
                .unwrap_or(tapstone_rules::cards::Faction::Neutral),
        );
        // The struck wall keeps its OWN faction colour. Painting it the attacker's hue reads as
        // the wall changing hands, which is a different and much louder claim than "it was hit".
        let wall_colour = pal::faction(
            tapstone_rules::cards::design(shown.seats[far].castle_design)
                .map(|c| c.faction)
                .unwrap_or(tapstone_rules::cards::Faction::Neutral),
        );

        match phase {
            Phase::Rise => draw_token(d, self.token_at(frame), rim),
            Phase::Impact => {
                let i = frame - RISE_FRAMES;
                // Frame 0 is the hit flash; the rest are the shake, in the wall's own colour.
                // The doc's wall-shake is +/-2 px, and the band is redrawn offset rather than
                // moved, because a moved band would dirty two rectangles.
                let dx = [0i32, 2, -2][i.min(2)];
                let colour = if i == 0 { pal::WARN } else { wall_colour };
                fill(
                    d,
                    Rectangle::new(
                        Point::new(0, geom::HUD),
                        Size::new(geom::W as u32, geom::WALL as u32),
                    ),
                    pal::BG,
                );
                fill(
                    d,
                    Rectangle::new(
                        Point::new(dx, geom::HUD),
                        Size::new(geom::W as u32, geom::WALL as u32),
                    ),
                    colour,
                );
            }
            Phase::Tick => {
                // The number has already changed; one frame of warn behind it says why.
                //
                // The flash has to sit where the life value actually is, which the HUD derives
                // from the castle name's width. Putting it at a guessed x painted over the name
                // and left the value drawn twice - the render showed it immediately.
                if frame == RISE_FRAMES + IMPACT_FRAMES {
                    let name = battlefield::name_of(shown.seats[far].castle_design);
                    let life = crate::txt!("{}", shown.seats[far].castle.life);
                    let x = 4
                        + name.len() as i32 * battlefield::tier::STATUS.character_size.width as i32
                        + 8;
                    let w = life.len() as i32
                        * battlefield::tier::VALUE.character_size.width as i32
                        + 4;
                    fill(
                        d,
                        Rectangle::new(
                            Point::new(x - 2, 2),
                            Size::new(w as u32, (geom::HUD - 4) as u32),
                        ),
                        pal::WARN,
                    );
                    battlefield::text(
                        d,
                        &life,
                        Point::new(
                            x,
                            (geom::HUD - battlefield::tier::VALUE.character_size.height as i32) / 2,
                        ),
                        battlefield::tier::VALUE,
                        pal::BG,
                        embedded_graphics::text::Alignment::Left,
                    );
                }
            }
        }
    }
}

/// A deliberately provisional card token: silhouette and rim only, at the size the real art will
/// occupy. Decision 0014 wants hand-made pixel art and none exists, so none is invented here.
fn draw_token<D: DrawTarget<Color = Rgb565>>(d: &mut D, at: Point, rim: Rgb565) {
    let r = Rectangle::new(at, Size::new(TOKEN_W as u32, TOKEN_H as u32));
    fill(d, r, pal::PANEL);
    let _ = r.into_styled(PrimitiveStyle::with_stroke(rim, 2)).draw(d);
    // The same single diagonal the unit placeholders use, so "not art yet" reads consistently.
    let _ = Line::new(
        Point::new(at.x + 3, at.y + TOKEN_H - 4),
        Point::new(at.x + TOKEN_W - 4, at.y + 3),
    )
    .into_styled(PrimitiveStyle::with_stroke(pal::DIMMED, 1))
    .draw(d);
}
