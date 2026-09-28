//! 0032's small motions, each a frame sequence with **one contiguous dirty rectangle per frame**.
//!
//! # The contract every motion here keeps, and how it is checked
//!
//! A motion is a *before* screen, `frames()` full-screen frames, and an *after* screen. Each frame
//! names one rectangle, [`Motion::dirty`], which is all the device will push that frame. So the
//! motion is correct only if **every pixel that changes between consecutive frames lies inside
//! the later frame's rectangle** — otherwise the glass keeps a stale pixel the host render hides.
//! And the last frame must be byte-identical to the static *after* screen, or the motion leaves
//! a residue the next static redraw would have to clean up with a second window.
//!
//! Both are checked in `shrine-preview/tests/motion.rs` by diffing real renders, which is an
//! instrument that can disagree with this module rather than one that restates it.
//!
//! # Why some motions spend a frame on the voice band
//!
//! A state change often touches the figure *and* the band ("your commander returns in 2 rounds").
//! Those are 90 px apart vertically, so pushing both at once is two windows or one window over
//! most of the panel. The rule taken here: **the band's update gets its own frame**, after the
//! motion. The band still never animates (0032); it is simply drawn once, on its own turn.
//!
//! # Why every frame is costed rather than admired
//!
//! Cost on this panel is windows first and pixels second (`cost.rs`, 0025). Every frame here is
//! one window by construction, and its pixel count is small; the per-motion budget share is
//! reported by `shrine-preview station-cost` and pinned by the manifest test.

use embedded_graphics::{pixelcolor::Rgb565, prelude::*, primitives::Rectangle};

use crate::battlefield::tier;
use crate::commander::{self, Commander, Ledger, Presence, Slot, SlotExt};
use crate::palette as pal;
use crate::sprite::{BREATH, Look, Pose};
use crate::station::{
    self, Cell, DESK_NOTE_Y, DOLL, Desk, FALL_DROP, IDLE_DOLL, LIGHT_STAGES, STATION_STATS_Y,
    Socket, StationScreen, XP_BAR, XP_TEXT_Y, cell_rect, locator_rect, socket_rect,
};
use crate::voice::{self, Voice};

/// Union of two rectangles (the smallest one containing both).
pub fn union(a: Rectangle, b: Rectangle) -> Rectangle {
    let x0 = a.top_left.x.min(b.top_left.x);
    let y0 = a.top_left.y.min(b.top_left.y);
    let x1 = (a.top_left.x + a.size.width as i32).max(b.top_left.x + b.size.width as i32);
    let y1 = (a.top_left.y + a.size.height as i32).max(b.top_left.y + b.size.height as i32);
    Rectangle::new(
        Point::new(x0, y0),
        Size::new((x1 - x0) as u32, (y1 - y0) as u32),
    )
}

fn rows(top: i32, bottom: i32) -> Rectangle {
    // The left column: the doll's x-range, from `top` to `bottom`, widened by the one pixel of
    // padding `ink::label` paints either side of a glyph box. The first version used the doll's
    // own x-range and a label at the margin changed a 1 px sliver outside it on every fall — the
    // frame-diff test caught it; the render could not show it.
    Rectangle::new(
        Point::new(DOLL.top_left.x - 1, top),
        Size::new(DOLL.size.width + 2, (bottom - top) as u32),
    )
}

fn line_bottom(y: i32) -> i32 {
    // A text line's box as `ink::label` paints it: one px of padding either side.
    y + tier::STATUS.character_size.height as i32 + 1
}

/// The figure, its health bar and its stats line: what a hit or a heal can change.
pub fn hit_rect() -> Rectangle {
    rows(DOLL.top_left.y, line_bottom(STATION_STATS_Y))
}

/// The whole commander column including the locator: what a fall or a return can change.
pub fn column_rect() -> Rectangle {
    union(
        rows(DOLL.top_left.y, line_bottom(STATION_STATS_Y)),
        locator_rect(),
    )
}

/// The desk's figure and every line under it: what equipping or levelling can change.
/// Starts at y 0 because the level-up light rises through the title line.
pub fn desk_column_rect() -> Rectangle {
    rows(0, line_bottom(DESK_NOTE_Y))
}

/// The XP bar and its line.
pub fn xp_rect() -> Rectangle {
    rows(XP_BAR.top_left.y, line_bottom(XP_TEXT_Y))
}

/// What a motion is showing, frame by frame.
#[derive(Clone, Copy, Debug)]
pub enum Scene<'a> {
    Idle {
        cmdr: Option<Commander<'a>>,
        sigil: &'a str,
        pose: Pose,
    },
    Station(StationScreen<'a>),
    Desk(Desk<'a>),
}

impl Scene<'_> {
    pub fn draw<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        match self {
            Scene::Idle { cmdr, sigil, pose } => station::idle(d, cmdr.as_ref(), sigil, *pose),
            Scene::Station(s) => station::station(d, s),
            Scene::Desk(k) => station::lobby(d, k),
        }
    }
}

/// Every motion 0032 names.
#[derive(Clone, Copy, Debug)]
pub enum Motion<'a> {
    /// 0014's 1-2-3-2-1, on the station (or idle) figure.
    Breath(Scene<'a>),
    /// Struck (hp drops) or healed (hp rises): a flash, a stagger if struck, then the bar; then
    /// each well the record also changed (a heal is a cast: mana and hand move too), then the band.
    ///
    /// `after` is the engine's state after the record, or [`Motion::hit`]'s view-side derivation.
    Hit {
        before: StationScreen<'a>,
        after: StationScreen<'a>,
    },
    /// Toughness reaches 0: a flash, a fade and a drop, then each well the death changed (the
    /// castle always pays 0029's penalty; a death in end-of-turn combat also turns the round, the
    /// hand and the mana), then the whisper.
    ///
    /// `after` is the state to end on — the **engine's** after a real death (see
    /// `shrine-preview`'s `real_fall`), or [`Motion::fall`]'s view-side derivation.
    Fall {
        before: StationScreen<'a>,
        after: StationScreen<'a>,
    },
    /// The fallen commander rises in the back cell of its lane at full toughness, then the wells
    /// the return's record changed settle, then the whisper.
    Return {
        before: StationScreen<'a>,
        after: StationScreen<'a>,
    },
    /// An item drops into its socket, then the figure wears it, then the grid marks it worn.
    Equip { before: Desk<'a>, cell: usize },
    /// The XP bar fills by `gain`.
    XpFill { before: Desk<'a>, gain: u8 },
    /// A column of light over the figure; the level and its gain appear.
    LevelUp { before: Desk<'a> },
    /// A chest opens in the inventory's first free cell and becomes the item.
    Loot {
        before: Desk<'a>,
        item: u16,
        cell: usize,
    },
    /// A drop into a full grid (lead's call, 2026-09-23): the chest opens over the last cell, shows
    /// the item, and it melts into 1 XP. The borrowed cell is restored; nothing is discarded.
    Melt { before: Desk<'a>, item: u16 },
    /// 0036: a draw tap acknowledged. The card in the figure's raised hand turns face-up in the
    /// drawn card's colour, the figure settles (next face-down card, or the arm comes down), the
    /// HAND well ticks, and the band speaks the card's name.
    DrawAck {
        before: StationScreen<'a>,
        card: u16,
        name: &'a str,
    },
    /// 0036: a mulligan. The hand goes back to the deck and as many draws as it held are owed (#61).
    Mulligan { before: StationScreen<'a> },
}

/// The cell a melting drop borrows for its reveal: the grid's last.
pub const MELT_CELL: usize = commander::INVENTORY - 1;

/// The fall's figure frames: flash, four fade steps, the fallen figure.
pub const FALL_FIGURE: usize = 6;
/// The return's: three steps back from the ghost, a bright frame, full colour.
pub const RETURN_FIGURE: usize = 5;
/// A struck figure: a flash, a stagger each way, the new toughness.
pub const STRUCK_FIGURE: usize = 4;
/// A healed figure: two flashes, the new toughness.
pub const HEAL_FIGURE: usize = 3;

/// How many wells differ between two states.
pub fn changed(a: &commander::Station, b: &commander::Station) -> usize {
    (0..station::WELL_COUNT)
        .filter(|i| station::well_changed(a, b, *i))
        .count()
}

/// The index of the `k`-th changed well.
pub fn nth_changed(a: &commander::Station, b: &commander::Station, k: usize) -> usize {
    (0..station::WELL_COUNT)
        .filter(|i| station::well_changed(a, b, *i))
        .nth(k)
        .unwrap_or(0)
}

/// The after-state with only the first `k + 1` changed wells settled, the old whisper kept.
fn settling<'a>(
    before: StationScreen<'a>,
    after: StationScreen<'a>,
    k: usize,
) -> StationScreen<'a> {
    let settled = nth_changed(&before.st, &after.st, k);
    let src = core::array::from_fn(|i| {
        let done = station::well_changed(&before.st, &after.st, i) && i <= settled;
        if done || !station::well_changed(&before.st, &after.st, i) {
            after.st
        } else {
            before.st
        }
    });
    StationScreen {
        voice: before.voice,
        wells: Some(src),
        ..after
    }
}

/// Hold frames for a pose, so breath reads as breath and not a twitch: 15 fps / 4 = 3.75 poses a
/// second. Frames where the pose does not change push nothing and cost nothing.
pub const BREATH_HOLD: usize = 4;
pub const DROP_FRAMES: usize = 6;
pub const XP_FRAMES: usize = 6;

impl<'a> Motion<'a> {
    pub fn name(&self) -> &'static str {
        match self {
            Motion::Breath(Scene::Idle { .. }) => "breath (idle)",
            Motion::Breath(_) => "breath",
            Motion::Hit { before, after } => {
                if hp_of(before) > hp_of(after) {
                    "struck"
                } else {
                    "heal"
                }
            }
            Motion::Fall { .. } => "fall",
            Motion::Return { .. } => "return",
            Motion::Equip { .. } => "equip",
            Motion::XpFill { .. } => "xp fill",
            Motion::LevelUp { .. } => "level-up",
            Motion::Loot { .. } => "loot",
            Motion::Melt { .. } => "loot (full)",
            // Distinct names: the filmstrip files and the manifest rows are keyed on them, and two
            // motions sharing one silently overwrote the first strip.
            Motion::DrawAck { before, .. } if before.st.owed <= 1 => "draw (last)",
            Motion::DrawAck { .. } => "draw",
            Motion::Mulligan { .. } => "mulligan",
        }
    }

    /// The static screen before the motion starts.
    pub fn before(&self) -> Scene<'a> {
        match *self {
            Motion::Breath(s) => s,
            Motion::Hit { before, .. }
            | Motion::Fall { before, .. }
            | Motion::Return { before, .. }
            | Motion::DrawAck { before, .. }
            | Motion::Mulligan { before } => Scene::Station(before),
            Motion::Equip { before, .. }
            | Motion::XpFill { before, .. }
            | Motion::LevelUp { before }
            | Motion::Loot { before, .. }
            | Motion::Melt { before, .. } => Scene::Desk(before),
        }
    }

    /// The static screen the motion must end on, exactly.
    pub fn after(&self) -> Scene<'a> {
        match *self {
            Motion::Breath(s) => s,
            Motion::Hit { after, .. } => Scene::Station(after),
            Motion::Fall { after, .. } | Motion::Return { after, .. } => Scene::Station(after),
            Motion::DrawAck { before, name, .. } => Scene::Station(drawn(before, name)),
            Motion::Mulligan { before } => Scene::Station(mulliganed(before)),
            Motion::Equip { before, cell } => Scene::Desk(equipped(before, cell)),
            Motion::XpFill { before, gain } => Scene::Desk(xp_filled(before, gain)),
            Motion::LevelUp { before } => Scene::Desk(levelled(before)),
            Motion::Loot { before, item, cell } => Scene::Desk(looted(before, item, cell)),
            Motion::Melt { before, item } => Scene::Desk(melted(before, item)),
        }
    }

    pub fn frames(&self) -> usize {
        match self {
            Motion::Breath(_) => BREATH.len() * BREATH_HOLD,
            Motion::Hit { before, after } => {
                self.figure_frames()
                    + changed(&before.st, &after.st)
                    + (before.voice != after.voice) as usize
            }
            // flash, four fade steps, the fallen figure; a frame per changed well; the band.
            Motion::Fall { before, after } => FALL_FIGURE + changed(&before.st, &after.st) + 1,
            // three steps back from the ghost, a bright frame, full colour; wells; the band.
            Motion::Return { before, after } => RETURN_FIGURE + changed(&before.st, &after.st) + 1,
            // drop frames, the figure, the grid cell, the swapped-out cell if any, the band.
            Motion::Equip { before, cell } => {
                DROP_FRAMES + 3 + swapped_cell(before, *cell).is_some() as usize
            }
            Motion::XpFill { .. } => XP_FRAMES,
            // light rises, the level lands under full light, the light goes, a socket unlocks
            // if this level opens one, then the band.
            Motion::LevelUp { before } => LIGHT_STAGES as usize + 3 + unlocks(before) as usize,
            // closed, ajar, open, the item, then the band.
            Motion::Loot { .. } => 5,
            // closed, ajar, open, the item, melting, the cell restored, the XP, the band.
            Motion::Melt { .. } => 8,
            // the card turns face-up, the figure settles, the HAND well, the band.
            Motion::DrawAck { .. } => 4,
            // the figure raises the empty hand's card, the HAND well, the band.
            Motion::Mulligan { .. } => 3,
        }
    }

    /// The one rectangle frame `n` pushes.
    pub fn dirty(&self, n: usize) -> Rectangle {
        let band = voice::band_rect();
        let last = self.frames() - 1;
        match *self {
            Motion::Breath(Scene::Idle { .. }) => IDLE_DOLL,
            Motion::Breath(_) => DOLL,
            Motion::Hit { before, after } => {
                let fig = self.figure_frames();
                let wells = changed(&before.st, &after.st);
                if n < fig {
                    hit_rect()
                } else if n < fig + wells {
                    station::WELLS[nth_changed(&before.st, &after.st, n - fig)]
                } else {
                    band
                }
            }
            Motion::Fall { before, after } | Motion::Return { before, after } => {
                let fig = self.figure_frames();
                if n == last {
                    band
                } else if n < fig {
                    column_rect()
                } else {
                    station::WELLS[nth_changed(&before.st, &after.st, n - fig)]
                }
            }
            Motion::Equip { before, cell } => {
                let slot = equip_slot(&before, cell);
                if n < DROP_FRAMES {
                    let now = drop_y(slot, n);
                    let prev = if n == 0 { now } else { drop_y(slot, n - 1) };
                    let top = now.min(prev);
                    let bottom = now.max(prev) + station::SOCKET;
                    Rectangle::new(
                        Point::new(station::SOCKET_X, top),
                        Size::new(station::SOCKET as u32, (bottom - top) as u32),
                    )
                } else if n == DROP_FRAMES {
                    desk_column_rect()
                } else if n == DROP_FRAMES + 1 {
                    cell_rect(cell)
                } else if n == last {
                    band
                } else {
                    // The item this one replaced is no longer worn: its own cell, its own frame.
                    cell_rect(swapped_cell(&before, cell).unwrap_or(cell))
                }
            }
            Motion::XpFill { .. } => xp_rect(),
            Motion::LevelUp { before } => {
                if n == last {
                    band
                } else if unlocks(&before) && n == last - 1 {
                    socket_rect(Slot::Trinket)
                } else {
                    desk_column_rect()
                }
            }
            Motion::Loot { cell, .. } => {
                if n == last {
                    band
                } else {
                    cell_rect(cell)
                }
            }
            Motion::Melt { .. } => match n {
                0..=5 => cell_rect(MELT_CELL),
                6 => xp_rect(),
                _ => band,
            },
            Motion::DrawAck { .. } => match n {
                0 | 1 => DOLL,
                2 => station::HAND_WELL,
                _ => band,
            },
            Motion::Mulligan { .. } => match n {
                0 => DOLL,
                1 => station::HAND_WELL,
                _ => band,
            },
        }
    }

    /// The whole screen as it stands at frame `n`.
    pub fn scene(&self, n: usize) -> Scene<'a> {
        let last = self.frames() - 1;
        match *self {
            Motion::Breath(s) => {
                let pose = BREATH[(n / BREATH_HOLD).min(BREATH.len() - 1)];
                with_breath(s, pose)
            }
            Motion::Hit { before, after } => {
                let struck = hp_of(&before) > hp_of(&after);
                let fig = self.figure_frames();
                if n == last {
                    return Scene::Station(after);
                }
                if n >= fig {
                    // Settling: the wells the record changed, one frame each, old whisper kept.
                    return Scene::Station(settling(before, after, n - fig));
                }
                if n == fig - 1 {
                    // The figure and its bar land on the new toughness over the old wells.
                    return Scene::Station(StationScreen {
                        voice: before.voice,
                        wells: Some([before.st; station::WELL_COUNT]),
                        ..after
                    });
                }
                let flash = if struck { pal::WARN } else { pal::HEALTH_OK };
                let (look, dx) = match (struck, n) {
                    (_, 0) => (Look::Flash(flash), 0),
                    (true, 1) => (Look::Normal, -2),
                    (true, _) => (Look::Normal, 2),
                    (false, _) => (Look::Flash(flash), 0),
                };
                Scene::Station(StationScreen {
                    pose: Pose {
                        look,
                        dx,
                        ..before.pose
                    },
                    ..before
                })
            }
            Motion::Fall { before, after } | Motion::Return { before, after } => {
                let fig = self.figure_frames();
                if n == last {
                    return Scene::Station(after);
                }
                if n >= fig {
                    // Settling: wells up to this frame's show the new state, the rest the old.
                    return Scene::Station(settling(before, after, n - fig));
                }
                // The figure's frames: the new presence (or the old one emptying) over the old wells
                // and the old whisper.
                let old = Some([before.st; station::WELL_COUNT]);
                if matches!(self, Motion::Fall { .. }) {
                    if n == fig - 1 {
                        return Scene::Station(StationScreen {
                            voice: before.voice,
                            wells: old,
                            ..after
                        });
                    }
                    let look = if n == 0 {
                        Look::Flash(pal::WARN)
                    } else {
                        Look::Fade(n as u8)
                    };
                    let dy = (n as i32).min(FALL_DROP);
                    // The bar empties on the first frame: the consequence lands with the blow.
                    return Scene::Station(StationScreen {
                        pose: Pose {
                            look,
                            dy,
                            ..before.pose
                        },
                        st: commander::Station {
                            presence: match before.st.presence {
                                Presence::OnBoard { lane, cell, .. } => {
                                    Presence::OnBoard { lane, cell, hp: 0 }
                                }
                                p => p,
                            },
                            ..before.st
                        },
                        ..before
                    });
                }
                let at_rest = StationScreen {
                    voice: before.voice,
                    wells: old,
                    ..after
                };
                match n {
                    0..=2 => Scene::Station(StationScreen {
                        pose: Pose {
                            look: Look::Fade(3 - n as u8),
                            dy: FALL_DROP - (n as i32).min(FALL_DROP),
                            ..at_rest.pose
                        },
                        ..at_rest
                    }),
                    3 => Scene::Station(StationScreen {
                        pose: Pose {
                            look: Look::Flash(pal::TEXT),
                            ..at_rest.pose
                        },
                        ..at_rest
                    }),
                    _ => Scene::Station(at_rest),
                }
            }
            Motion::DrawAck { before, card, name } => {
                let after = drawn(before, name);
                let old = Some([before.st; station::WELL_COUNT]);
                match n {
                    0 => {
                        let face = pal::faction(
                            tapstone_rules::cards::design(card)
                                .map(|d| d.faction)
                                .unwrap_or(tapstone_rules::cards::Faction::Neutral),
                        );
                        Scene::Station(StationScreen {
                            pose: Pose {
                                reach: Some(crate::sprite::Held::Face(face)),
                                ..before.pose
                            },
                            ..before
                        })
                    }
                    1 => Scene::Station(StationScreen {
                        voice: before.voice,
                        wells: old,
                        ..after
                    }),
                    2 => Scene::Station(StationScreen {
                        voice: before.voice,
                        ..after
                    }),
                    _ => Scene::Station(after),
                }
            }
            Motion::Mulligan { before } => {
                let after = mulliganed(before);
                match n {
                    0 => Scene::Station(StationScreen {
                        voice: before.voice,
                        wells: Some([before.st; station::WELL_COUNT]),
                        ..after
                    }),
                    1 => Scene::Station(StationScreen {
                        voice: before.voice,
                        ..after
                    }),
                    _ => Scene::Station(after),
                }
            }
            Motion::Equip { before, cell } => {
                let after = equipped(before, cell);
                let slot = equip_slot(&before, cell);
                let id = item_at(&before, cell);
                if n < DROP_FRAMES {
                    // The socket is still empty underneath; the item is in the air above it.
                    Scene::Desk(Desk {
                        drop: Some((id, drop_y(slot, n))),
                        ..before
                    })
                } else if n == DROP_FRAMES {
                    // Seated, and the figure wears it. The grid still shows it unworn.
                    Scene::Desk(Desk {
                        grid: before.grid,
                        voice: before.voice,
                        ..after
                    })
                } else if n == DROP_FRAMES + 1 {
                    // This cell marked worn; a swapped-out cell, if any, still shows worn.
                    let mut grid = after.grid;
                    if let Some(old) = swapped_cell(&before, cell) {
                        grid[old] = before.grid[old];
                    }
                    Scene::Desk(Desk {
                        grid,
                        voice: before.voice,
                        ..after
                    })
                } else if n == last {
                    Scene::Desk(after)
                } else {
                    Scene::Desk(Desk {
                        voice: before.voice,
                        ..after
                    })
                }
            }
            Motion::XpFill { before, gain } => {
                let after = xp_filled(before, gain);
                if n == last {
                    return Scene::Desk(after);
                }
                let step = u32::from(gain) * (n as u32 + 1) / XP_FRAMES as u32;
                Scene::Desk(Desk {
                    xp_shown: before.xp_shown + step,
                    xp_gain: Some(gain),
                    ..before
                })
            }
            Motion::LevelUp { before } => {
                let after = levelled(before);
                let stages = LIGHT_STAGES as usize;
                if n < stages {
                    Scene::Desk(Desk {
                        light: n as u8 + 1,
                        ..before
                    })
                } else if n == stages {
                    // The socket a level opens is held locked here: it is outside this frame's
                    // rectangle. (The first version unlocked it here, and the render showed it
                    // unlocked, then locked, then unlocked again.)
                    Scene::Desk(Desk {
                        light: LIGHT_STAGES,
                        sockets: before.sockets,
                        voice: before.voice,
                        ..after
                    })
                } else if n == last {
                    Scene::Desk(after)
                } else if n == stages + 1 && unlocks(&before) {
                    // Light gone; the socket it opens still shows locked until its own frame.
                    Scene::Desk(Desk {
                        sockets: before.sockets,
                        voice: before.voice,
                        ..after
                    })
                } else {
                    Scene::Desk(Desk {
                        voice: before.voice,
                        ..after
                    })
                }
            }
            Motion::Loot { before, item, cell } => {
                let after = looted(before, item, cell);
                if n == last {
                    return Scene::Desk(after);
                }
                if n == last - 1 {
                    return Scene::Desk(Desk {
                        voice: before.voice,
                        ..after
                    });
                }
                let mut grid = before.grid;
                grid[cell] = Cell::Chest(n as u8);
                Scene::Desk(Desk { grid, ..before })
            }
            Motion::Melt { before, item } => {
                let after = melted(before, item);
                let mut grid = before.grid;
                match n {
                    0..=2 => grid[MELT_CELL] = Cell::Chest(n as u8),
                    3 => {
                        grid[MELT_CELL] = Cell::Item {
                            id: item,
                            worn: false,
                            new: true,
                        }
                    }
                    4 => grid[MELT_CELL] = Cell::Melting(item),
                    5 => {}
                    6 => {
                        return Scene::Desk(Desk {
                            voice: before.voice,
                            ..after
                        });
                    }
                    _ => return Scene::Desk(after),
                }
                Scene::Desk(Desk { grid, ..before })
            }
        }
    }

    /// Frames before the wells settle.
    fn figure_frames(&self) -> usize {
        match self {
            Motion::Return { .. } => RETURN_FIGURE,
            Motion::Hit { before, after } if hp_of(before) > hp_of(after) => STRUCK_FIGURE,
            Motion::Hit { .. } => HEAL_FIGURE,
            _ => FALL_FIGURE,
        }
    }

    /// A hit driven by the view: the same state with only the toughness changed. Use the engine's
    /// after-state instead whenever one exists.
    pub fn hit(before: StationScreen<'a>, to_hp: u8) -> Motion<'a> {
        Motion::Hit {
            before,
            after: with_hp(before, to_hp),
        }
    }

    /// A fall driven by the view: the castle pays the game's own penalty, the return round is the
    /// game's delay on. Use the engine's after-state instead whenever one exists.
    pub fn fall(before: StationScreen<'a>) -> Motion<'a> {
        Motion::Fall {
            before,
            after: fallen(before),
        }
    }

    /// A return driven by the view, for a before-state the engine game does not reach.
    pub fn ret(before: StationScreen<'a>) -> Motion<'a> {
        Motion::Return {
            before,
            after: returned(before),
        }
    }

    pub fn draw_frame<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, n: usize) {
        self.scene(n).draw(d)
    }
}

fn with_breath(s: Scene<'_>, breath: u8) -> Scene<'_> {
    match s {
        Scene::Idle { cmdr, sigil, pose } => Scene::Idle {
            cmdr,
            sigil,
            pose: Pose { breath, ..pose },
        },
        Scene::Station(st) => Scene::Station(StationScreen {
            pose: Pose { breath, ..st.pose },
            ..st
        }),
        Scene::Desk(k) => Scene::Desk(Desk {
            pose: Pose { breath, ..k.pose },
            ..k
        }),
    }
}

fn hp_of(s: &StationScreen<'_>) -> u8 {
    match s.st.presence {
        Presence::OnBoard { hp, .. } => hp,
        Presence::Fallen { .. } => 0,
    }
}

fn with_hp<'a>(s: StationScreen<'a>, hp: u8) -> StationScreen<'a> {
    let presence = match s.st.presence {
        Presence::OnBoard { lane, cell, .. } => Presence::OnBoard { lane, cell, hp },
        p => p,
    };
    StationScreen {
        st: commander::Station { presence, ..s.st },
        ..s
    }
}

/// The state after a fall, by 0029's rulings: the castle loses the fall penalty, and the return
/// round is this round plus the return delay, in the lane it died in.
fn fallen<'a>(s: StationScreen<'a>) -> StationScreen<'a> {
    let lane = match s.st.presence {
        Presence::OnBoard { lane, .. } | Presence::Fallen { lane, .. } => lane,
    };
    let st = commander::Station {
        presence: Presence::Fallen {
            lane,
            return_round: s.st.round.saturating_add(s.st.return_delay),
        },
        castle_life: s.st.castle_life.saturating_sub(s.st.fall_penalty),
        ..s.st
    };
    StationScreen {
        st,
        voice: st
            .return_state()
            .map(Voice::Return)
            .unwrap_or(Voice::Silent),
        pose: Pose::default(),
        ..s
    }
}

fn returned<'a>(s: StationScreen<'a>) -> StationScreen<'a> {
    let lane = match s.st.presence {
        Presence::OnBoard { lane, .. } | Presence::Fallen { lane, .. } => lane,
    };
    let hp = s.st.toughness;
    StationScreen {
        st: commander::Station {
            presence: Presence::OnBoard { lane, cell: 0, hp },
            ..s.st
        },
        voice: Voice::Returned,
        pose: Pose::default(),
        wells: None,
        ..s
    }
}

fn item_at(k: &Desk<'_>, cell: usize) -> u16 {
    match k.grid[cell] {
        Cell::Item { id, .. } => id,
        _ => panic!("equip from a cell with no item"),
    }
}

fn equip_slot(k: &Desk<'_>, cell: usize) -> Slot {
    commander::item(item_at(k, cell))
        .map(|i| i.slot)
        .unwrap_or(Slot::Weapon)
}

/// The grid cell holding whatever `cell`'s item displaces from its slot, if anything is worn there.
pub fn swapped_cell(k: &Desk<'_>, cell: usize) -> Option<usize> {
    let slot = equip_slot(k, cell);
    let old = k.cmdr.loadout[slot.index()]?;
    k.grid
        .iter()
        .position(|c| matches!(c, Cell::Item { id, .. } if *id == old))
}

/// The level the XP shown has reached — what a level-up lands.
fn reached(k: &Desk<'_>) -> u8 {
    tapstone_progression::level_for_xp(k.xp_shown)
}

/// Does landing the level the XP has reached open a socket?
pub fn unlocks(k: &Desk<'_>) -> bool {
    station::sockets_of(&k.cmdr, reached(k)) != k.sockets
}

/// Top y of the falling item at drop frame `n`: from the top of the panel into its socket.
pub fn drop_y(slot: Slot, n: usize) -> i32 {
    let end = socket_rect(slot).top_left.y;
    let start = 0;
    let t = n.min(DROP_FRAMES - 1) as i32;
    // Ease in: gravity, not a lift.
    let span = (DROP_FRAMES - 1) as i32;
    start + (end - start) * t * t / (span * span)
}

fn equipped<'a>(k: Desk<'a>, cell: usize) -> Desk<'a> {
    let id = item_at(&k, cell);
    let slot = equip_slot(&k, cell);
    let mut cmdr = k.cmdr;
    cmdr.loadout[slot.index()] = Some(id);
    let mut grid = k.grid;
    // The previously worn item in this slot, if any, is no longer worn.
    for c in grid.iter_mut() {
        if let Cell::Item {
            id: other, worn, ..
        } = c
        {
            *worn = cmdr.loadout.contains(&Some(*other));
        }
    }
    let mut sockets = k.sockets;
    sockets[slot.index()] = Socket::Worn(id);
    let name = commander::item(id).map(|i| i.name).unwrap_or("?");
    Desk {
        cmdr,
        grid,
        sockets,
        voice: Voice::Equipped { item: name, slot },
        ..k
    }
}

/// The award lands in the ledger (total XP) and on the bar; the level shown waits for its light.
fn xp_filled<'a>(k: Desk<'a>, gain: u8) -> Desk<'a> {
    let mut cmdr = k.cmdr;
    cmdr.xp += u32::from(gain);
    Desk {
        cmdr,
        xp_shown: k.xp_shown + u32::from(gain),
        xp_gain: Some(gain),
        ..k
    }
}

/// The level the XP has reached is landed: progression's `level_for_xp`, not "+1".
fn levelled<'a>(k: Desk<'a>) -> Desk<'a> {
    let level = reached(&k);
    let gain = commander::level_gain(level).unwrap_or("");
    Desk {
        level,
        levelled: true,
        sockets: station::sockets_of(&k.cmdr, level),
        light: 0,
        voice: Voice::LevelUp { level, gain },
        ..k
    }
}

fn looted<'a>(k: Desk<'a>, item: u16, cell: usize) -> Desk<'a> {
    let mut grid = k.grid;
    grid[cell] = Cell::Item {
        id: item,
        worn: false,
        new: true,
    };
    let name = commander::item(item).map(|i| i.name).unwrap_or("?");
    Desk {
        grid,
        voice: Voice::Loot { item: name },
        ..k
    }
}

/// After one draw tap: one fewer owed, one more in hand, the name spoken (0036, 0033).
fn drawn<'a>(s: StationScreen<'a>, name: &'a str) -> StationScreen<'a> {
    let owed = s.st.owed.saturating_sub(1);
    StationScreen {
        st: commander::Station {
            owed,
            hand: s.st.hand + 1,
            ..s.st
        },
        voice: Voice::Drew {
            card: name,
            left: owed,
        },
        pose: Pose {
            reach: None,
            ..s.pose
        },
        wells: None,
        ..s
    }
}

/// After a mulligan: the hand is back in the deck and the seat owes **as many draws as the hand it
/// returned** — 0036 as clarified on 2026-09-23 (#61; rules v0's mulligan, the one every balance
/// number was measured under). An opening-hand count would have cost seat 1 its turn-start card.
fn mulliganed<'a>(s: StationScreen<'a>) -> StationScreen<'a> {
    let n = s.st.hand;
    let st = commander::Station {
        owed: n,
        hand: 0,
        mulligan_open: false,
        ..s.st
    };
    StationScreen {
        st,
        voice: Voice::Draw {
            n,
            why: crate::draws::DrawWhy::Mulligan,
        },
        pose: Pose {
            reach: None,
            ..s.pose
        },
        wells: None,
        ..s
    }
}

fn melted<'a>(k: Desk<'a>, item: u16) -> Desk<'a> {
    let mut cmdr = k.cmdr;
    cmdr.xp += tapstone_progression::XP_MELT;
    let name = commander::item(item).map(|i| i.name).unwrap_or("?");
    Desk {
        cmdr,
        xp_shown: k.xp_shown + tapstone_progression::XP_MELT,
        voice: Voice::Melted { item: name },
        ..k
    }
}

/// Where a loot chest may open, or `None` if the grid is full (see the findings: 0032 does not
/// say what happens then).
pub fn loot_cell(l: &Ledger) -> Option<usize> {
    l.first_free()
}
