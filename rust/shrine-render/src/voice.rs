//! The voice: the one band that owns every sentence the shrine says (0032, spoken per 0033).
//!
//! **One line, one message, newest wins.** A message is a [`Voice`] value; the band draws
//! whichever one it was last handed and nothing else, so "newest wins" is the caller replacing a
//! value rather than a queue anyone can get wrong.
//!
//! # Why every sentence is a variant and not a string
//!
//! 0027 found the old prompt band needing 338 px of 320, and a second prompt clipped later because
//! the list of "every prompt" was written by hand beside the code that built them. The same fix
//! applies here, harder: every sentence the shrine can say is constructed in [`Voice::text`], every
//! variant is reachable from [`Voice::index`] (an exhaustive match), and refusals are the
//! **engine's own `Refusal` enum**, so a refusal added to the rules crate will not compile here
//! until it has a sentence — and the width test then measures it.
//!
//! Those same sentences are what 0033 pre-renders with Piper, so this enum is also the clip list:
//! one object for what is shown and what is spoken.
//!
//! # Draws are not silent (0036)
//!
//! 0036 makes every draw a tap: the shrine will say "draw N - tap it on the stone" and hold a
//! draw-owed state until the card is tapped. The engine has not landed it, so there is no variant
//! yet — but nothing here assumes a draw passes without a sentence. The band is newest-wins over
//! one value, so the draw prompt is one more `Voice` variant (and one more row in the width test),
//! and the station's hand well already reads the engine's `hand_len` rather than predicting it.

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::Alignment,
};
use tapstone_rules::rules::Refusal;

use crate::battlefield::{fill, fit, tier};
use crate::commander::{ReturnState, Slot, SlotExt};
use crate::draws::DrawWhy;
use crate::geom;
use crate::ink::{Ink, label};
use crate::palette as pal;

/// The band: the bottom 24 px, full width, on every station screen.
pub const BAND_H: i32 = 24;
pub const BAND_Y: i32 = geom::H - BAND_H;
/// The glyph at the band's left edge: speaker, or mic while listening.
pub const GLYPH_W: i32 = 12;
/// Where the sentence starts, after the glyph and a gap.
pub const TEXT_X: i32 = 4 + GLYPH_W + 6;
/// Right margin kept clear, so an overflow is measurable as pixels in it.
pub const MARGIN_R: i32 = 4;

/// When the battery is announced (the lead's ruling, 2026-09-23): at these levels only, highest
/// first. Each speaks once per crossing and charging above it resets it ([`BatteryAnnouncer`]).
pub const BATTERY_THRESHOLDS: [u8; 3] = [20, 10, 5];
/// The level at which the announcement adds "the shrine will sleep soon".
pub const SLEEP_WARNING_PCT: u8 = 5;

/// How far above a threshold the battery must climb before that threshold may speak again (the
/// lead's call): ADC jitter between 20 % and 21 % must not repeat "battery 20%".
pub const BATTERY_REARM_PCT: u8 = 2;

/// Speaks each battery threshold once per crossing, and re-arms it only when the battery charges
/// back to the threshold plus [`BATTERY_REARM_PCT`]. `update` returns the threshold to announce,
/// if any — the most urgent one crossed, so a drop from 25 % straight to 4 % says "5 %" once, not
/// three lines in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryAnnouncer {
    spoken: [bool; BATTERY_THRESHOLDS.len()],
    rearm: u8,
}

impl Default for BatteryAnnouncer {
    fn default() -> Self {
        Self::with_rearm(BATTERY_REARM_PCT)
    }
}

impl BatteryAnnouncer {
    /// An announcer that re-arms a threshold at `threshold + rearm` or above. `rearm` 1 is no
    /// hysteresis (21 % re-arms 20 %), kept as the jitter test's control; 0 is treated as 1.
    pub const fn with_rearm(rearm: u8) -> Self {
        BatteryAnnouncer {
            spoken: [false; BATTERY_THRESHOLDS.len()],
            rearm: if rearm == 0 { 1 } else { rearm },
        }
    }

    pub fn update(&mut self, pct: u8) -> Option<u8> {
        let mut say = None;
        for (i, t) in BATTERY_THRESHOLDS.iter().enumerate() {
            if pct >= t.saturating_add(self.rearm) {
                self.spoken[i] = false; // charged well above: this threshold may speak again
            } else if pct <= *t && !self.spoken[i] {
                self.spoken[i] = true;
                say = Some(*t); // thresholds run high to low, so the last one set is the lowest
            }
        }
        say
    }
}

/// 0009's countdown: "a 5-second countdown resolves to the rules' default". Prompts that name
/// it (target, lane, loadout) take it from here.
pub const COUNTDOWN_S: u8 = 5;

/// Characters the sentence may use: derived from the band geometry and the font, not typed.
pub const fn budget_chars() -> usize {
    ((geom::W - TEXT_X - MARGIN_R) / tier::STATUS.character_size.width as i32) as usize
}

/// 0033's push-to-talk surface: "touching and holding the voice band". **Optional:** 0033's
/// correction after review makes voice answers a phase-2 stretch goal, so the affordance (the mic
/// glyph and [`Voice::Listening`]) is drawn but a build may leave it off. The strip is reserved
/// either way, since keeping it clear costs nothing and adding it later would move touch targets.
pub const PTT_OPTIONAL: bool = true;

/// The push-to-talk hit area's top edge. The band is 24 px tall,
/// below the 44 px touch floor, so the **hit area** extends upward over readouts that are never
/// touch targets. Every screen with a band must keep this strip free of other touch targets.
pub const PTT_TOP: i32 = geom::H - geom::TOUCH_MIN;

pub fn band_rect() -> Rectangle {
    Rectangle::new(
        Point::new(0, BAND_Y),
        Size::new(geom::W as u32, BAND_H as u32),
    )
}

/// Why the station is dark (0032 screen 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dark {
    /// The arena is gone (0028); the match is paused, not lost.
    ArenaGone,
    /// The shrine went dormant (design spec §7: silent for N rounds).
    Dormant,
    BatteryLow {
        pct: u8,
    },
}

impl Dark {
    pub const ALL: [Dark; 3] = [
        Dark::ArenaGone,
        Dark::Dormant,
        Dark::BatteryLow {
            pct: SLEEP_WARNING_PCT,
        },
    ];
}

/// Every sentence the shrine says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Voice<'a> {
    /// Idle: 0032's invitation.
    Invite,
    /// A tapped card asks cast or charge (0007, 0009). 0009's default: cast.
    CastOrCharge {
        card: &'a str,
    },
    /// A unit wants a lane. 0009's default: the lane with the fewest of your units.
    Lane {
        card: &'a str,
        secs: u8,
    },
    /// A spell wants a target. 0009's default: the nearest legal target.
    Target {
        card: &'a str,
        secs: u8,
    },
    /// A refused tap: the engine's reason, in words.
    Refused(Refusal),
    /// A fallen commander's return, as the station derives it (0029).
    Return(ReturnState),
    /// The commander has re-entered its back cell.
    Returned,
    CastleAt {
        castle: &'a str,
        life: u8,
    },
    /// Lobby: touch or tap to equip; the 5 s default re-applies the last loadout (0031).
    Loadout {
        secs: u8,
    },
    Equipped {
        item: &'a str,
        slot: Slot,
    },
    /// A second keyword item, refused at equip time (0031).
    SecondKeyword,
    /// An item whose slot is not open until `level` (the third slot, `THIRD_SLOT_AT`) — the only
    /// level gate on equipping (the lead's ruling: `min_level` is loot-table only).
    TooLow {
        item: &'a str,
        level: u8,
    },
    Loot {
        item: &'a str,
    },
    /// A drop into a full grid melts into 1 XP (lead's call, 2026-09-23: the duplicate rule).
    Melted {
        item: &'a str,
    },
    LevelUp {
        level: u8,
        gain: &'a str,
    },
    Lobby,
    Dark(Dark),
    /// Push-to-talk held (0033). Optional: see [`PTT_OPTIONAL`].
    Listening,
    /// STT heard something outside the current prompt's grammar. `words` is dynamic.
    Heard {
        words: &'a str,
    },
    Silent,
    /// 0036: the seat owes `n` draws; nothing else is possible until they are tapped.
    Draw {
        n: u8,
        why: DrawWhy<'a>,
    },
    /// A draw tap acknowledged: the card's name (spoken, 0033), and how many are still owed.
    Drew {
        card: &'a str,
        left: u8,
    },
    /// The mulligan window (0036: the hand goes back and as many draws as it held are owed, #61).
    MulliganOffer,
    /// A turn start with no undrawn copies left: 0036's exhausted-deck rule owes nothing.
    DeckEmpty,
    /// A castle tap in the mulligan window opened the 3 s prompt (the lead's ruling): a second tap
    /// mulligans, expiry keeps and passes.
    MulliganPrompt {
        secs: u8,
    },
}

/// Number of variants; the second half of adding one (the first is `index`).
pub const VOICE_VARIANTS: usize = 25;

/// What a message is, for its colour and glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Prompt,
    Refusal,
    Whisper,
    Listening,
}

impl<'a> Voice<'a> {
    pub fn index(&self) -> usize {
        match self {
            Voice::Invite => 0,
            Voice::CastOrCharge { .. } => 1,
            Voice::Lane { .. } => 2,
            Voice::Target { .. } => 3,
            Voice::Refused(_) => 4,
            Voice::Return(_) => 5,
            Voice::CastleAt { .. } => 6,
            Voice::Loadout { .. } => 7,
            Voice::Equipped { .. } => 8,
            Voice::SecondKeyword => 9,
            Voice::Loot { .. } => 10,
            Voice::LevelUp { .. } => 11,
            Voice::Lobby => 12,
            Voice::Dark(_) => 13,
            Voice::Listening => 14,
            Voice::Heard { .. } => 15,
            Voice::Silent => 16,
            Voice::Returned => 17,
            Voice::Melted { .. } => 18,
            Voice::Draw { .. } => 19,
            Voice::Drew { .. } => 20,
            Voice::MulliganOffer => 21,
            Voice::DeckEmpty => 22,
            Voice::MulliganPrompt { .. } => 23,
            Voice::TooLow { .. } => 24,
        }
    }

    pub fn tone(&self) -> Tone {
        match self {
            Voice::Refused(_)
            | Voice::SecondKeyword
            | Voice::TooLow { .. }
            | Voice::Heard { .. } => Tone::Refusal,
            Voice::Return(_)
            | Voice::Returned
            | Voice::CastleAt { .. }
            | Voice::Equipped { .. }
            | Voice::Loot { .. }
            | Voice::Melted { .. }
            | Voice::Drew { .. }
            | Voice::DeckEmpty
            | Voice::LevelUp { .. }
            | Voice::Dark(_)
            | Voice::Silent => Tone::Whisper,
            Voice::Listening => Tone::Listening,
            Voice::Invite
            | Voice::CastOrCharge { .. }
            | Voice::Lane { .. }
            | Voice::Target { .. }
            | Voice::Loadout { .. }
            | Voice::Draw { .. }
            | Voice::MulliganOffer
            | Voice::MulliganPrompt { .. }
            | Voice::Lobby => Tone::Prompt,
        }
    }

    /// Does this sentence carry text the shrine did not choose (so it may be truncated)?
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Voice::Heard { .. })
    }

    pub fn text(&self) -> crate::fmt::Text {
        use crate::txt;
        match *self {
            Voice::Invite => txt!("set your castle on the stone"),
            Voice::CastOrCharge { card } => txt!("{card}: cast, or tap again to charge"),
            // 0009's defaults, as close to its words as 49 characters allow: "the lane with the
            // fewest of your units" and "the nearest legal target".
            Voice::Lane { card, secs } => txt!("{card} > lane  fewest of yours in {secs}s"),
            Voice::Target { card, secs } => txt!("{card} > target  default: nearest {secs}s"),
            Voice::Refused(r) => refusal(r),
            Voice::Return(ReturnState::ThisTurn) => txt!("your commander returns this turn"),
            Voice::Return(ReturnState::In(1)) => txt!("your commander returns next round"),
            Voice::Return(ReturnState::In(n)) => txt!("your commander returns in {n} rounds"),
            Voice::Return(ReturnState::Blocked) => {
                txt!("your commander waits - its back cell is taken")
            }
            Voice::Returned => txt!("your commander is back on the field"),
            Voice::CastleAt { castle, life } => txt!("the {castle} is at {life}"),
            Voice::Loadout { secs } => txt!("touch an item to equip - last loadout {secs}s"),
            Voice::Equipped { item, slot } => txt!("{item} worn as {}", slot.name()),
            Voice::SecondKeyword => txt!("refused: one keyword per commander"),
            Voice::TooLow { item, level } => txt!("refused: {item} needs level {level}"),
            Voice::Loot { item } => txt!("loot: {item}"),
            Voice::Melted { item } => txt!("{item}: no room - melts into 1 XP"),
            // A level that unlocks nothing says only the level; the clip manifest found the old
            // "level 4 - " with a dangling dash, which the band would have shown too.
            Voice::LevelUp { level, gain: "" } => txt!("level {level}"),
            Voice::LevelUp { level, gain } => txt!("level {level} - {gain}"),
            Voice::Lobby => txt!("tap your castle to return to the lobby"),
            Voice::Dark(Dark::ArenaGone) => txt!("the arena is dark - the match waits"),
            Voice::Dark(Dark::Dormant) => txt!("the shrine sleeps - tap a card to wake it"),
            // The lead's ruling (2026-09-23): 20 % and 10 % say the level; 5 % adds the warning.
            Voice::Dark(Dark::BatteryLow { pct }) if pct <= SLEEP_WARNING_PCT => {
                txt!("battery {pct}% - the shrine will sleep soon")
            }
            Voice::Dark(Dark::BatteryLow { pct }) => txt!("battery {pct}%"),
            Voice::Listening => txt!("listening - release to answer"),
            // The one sentence with text the shrine did not choose. The *words* are fitted, not
            // the sentence, so a long mishearing loses its tail and never the reason.
            Voice::Heard { words } => {
                let room = budget_chars().saturating_sub(HEARD_FRAME.len());
                txt!("heard \"{}\" - not an answer here", fit(words, room))
            }
            Voice::Silent => txt!(""),
            // 0036's wording, "draw 1 — tap it on the stone", with the dash the ASCII font has.
            Voice::Draw { n, why } => {
                let each = if n == 1 { "it" } else { "each" };
                match why {
                    DrawWhy::Opening => txt!("opening hand: draw {n} - tap {each} on the stone"),
                    DrawWhy::Mulligan => txt!("mulligan: draw {n} - tap {each} on the stone"),
                    DrawWhy::TurnStart => txt!("draw {n} - tap {each} on the stone"),
                    DrawWhy::Spell { card } => txt!("{card}: draw {n} - tap on the stone"),
                }
            }
            Voice::Drew { card, left: 0 } => txt!("drew {card}"),
            Voice::Drew { card, left } => txt!("drew {card} - {left} more to draw"),
            // The card path: a castle tap in the window opens the 3 s prompt (`MulliganPrompt`), a
            // second tap mulligans (the lead's ruling). Touch: the HAND well.
            Voice::MulliganOffer => txt!("keep: play a card. mulligan: castle twice"),
            Voice::DeckEmpty => txt!("your deck is empty - nothing to draw"),
            Voice::MulliganPrompt { secs } => {
                txt!("pass - tap the castle again to mulligan {secs}s")
            }
        }
    }
}

/// The fixed part of `Voice::Heard`, for fitting its dynamic part.
pub const HEARD_FRAME: &str = "heard \"\" - not an answer here";

/// A refusal in words. Exhaustive over the engine's enum: a new refusal will not compile here
/// until it has a sentence, and the width test measures that sentence.
pub fn refusal(r: Refusal) -> crate::fmt::Text {
    use crate::txt;
    match r {
        Refusal::NotYourTurn => txt!("refused: not your turn"),
        Refusal::NotInHand => txt!("refused: that card is not in your hand"),
        Refusal::NoMana { need, have } => txt!("refused: needs {need} mana, you have {have}"),
        Refusal::CellOccupied => txt!("refused: that cell is taken"),
        Refusal::AlreadyChargedThisRound => txt!("refused: you already charged this round"),
        Refusal::AlreadyAdvancedLane => txt!("refused: that lane already advanced"),
        Refusal::BadTarget => txt!("refused: not a legal target"),
        Refusal::UnknownCard => txt!("refused: this shrine does not know that card"),
        Refusal::GameOver => txt!("refused: the match is over"),
        Refusal::LaneOutOfRange => txt!("refused: no such lane"),
        Refusal::NotPlaying => txt!("refused: the match has not started"),
        Refusal::SeatTaken => txt!("refused: that seat is taken"),
        Refusal::MulliganClosed => txt!("refused: the mulligan has closed"),
        Refusal::LobbyClosed => txt!("refused: the lobby has closed"),
        // 0036: every draw is a tap.
        Refusal::DrawOwed => txt!("refused: draw first - tap the card"),
        Refusal::NoDrawOwed => txt!("refused: no draw is owed"),
        Refusal::NotInDeck => txt!("refused: that card is not in your deck"),
    }
}

/// Every engine refusal, for the width test. Kept beside `refusal` so both change together.
pub const REFUSALS: [Refusal; 17] = [
    Refusal::NotYourTurn,
    Refusal::NotInHand,
    Refusal::NoMana { need: 10, have: 10 },
    Refusal::CellOccupied,
    Refusal::AlreadyChargedThisRound,
    Refusal::AlreadyAdvancedLane,
    Refusal::BadTarget,
    Refusal::UnknownCard,
    Refusal::GameOver,
    Refusal::LaneOutOfRange,
    Refusal::NotPlaying,
    Refusal::SeatTaken,
    Refusal::MulliganClosed,
    Refusal::LobbyClosed,
    Refusal::DrawOwed,
    Refusal::NoDrawOwed,
    Refusal::NotInDeck,
];

/// The ink each tone speaks in. Every one sits on the band's `PANEL` ground.
pub const fn tone_ink(t: Tone) -> Ink {
    match t {
        Tone::Prompt => Ink::WellText,
        Tone::Refusal => Ink::Warn,
        Tone::Whisper => Ink::WellDim,
        Tone::Listening => Ink::Good,
    }
}

/// Draw the band. It never animates (0032): it is drawn once per message, as one window.
pub fn draw<D: DrawTarget<Color = Rgb565>>(d: &mut D, v: &Voice<'_>) {
    let band = band_rect();
    fill(d, band, pal::PANEL);
    let tone = v.tone();
    let ink = tone_ink(tone);
    let colour = ink.pair().0;
    // Top rule: the band's edge, in the message's colour for a refusal so it is findable at a
    // glance; otherwise a quiet line.
    let rule = if tone == Tone::Refusal {
        pal::WARN
    } else {
        pal::DIMMED
    };
    fill(
        d,
        Rectangle::new(Point::new(0, BAND_Y), Size::new(geom::W as u32, 1)),
        rule,
    );
    glyph(d, tone, colour);
    let s = fit(&v.text(), budget_chars());
    label(
        d,
        &s,
        Point::new(
            TEXT_X,
            BAND_Y + (BAND_H - tier::STATUS.character_size.height as i32) / 2 + 1,
        ),
        tier::STATUS,
        ink,
        Alignment::Left,
    );
}

/// Speaker (the shrine is talking) or mic (the shrine is listening), 12×12.
fn glyph<D: DrawTarget<Color = Rgb565>>(d: &mut D, tone: Tone, c: Rgb565) {
    let x = 4;
    let y = BAND_Y + (BAND_H - 12) / 2 + 1;
    let px = |d: &mut D, dx: i32, dy: i32, w: u32, h: u32| {
        fill(
            d,
            Rectangle::new(Point::new(x + dx, y + dy), Size::new(w, h)),
            c,
        )
    };
    if tone == Tone::Listening {
        px(d, 4, 0, 4, 7);
        px(d, 2, 5, 1, 3);
        px(d, 9, 5, 1, 3);
        px(d, 3, 8, 6, 1);
        px(d, 5, 9, 2, 3);
    } else {
        px(d, 0, 4, 3, 4);
        px(d, 3, 2, 2, 8);
        px(d, 5, 0, 1, 12);
        let _ = Rectangle::new(Point::new(x + 8, y + 3), Size::new(2, 6))
            .into_styled(PrimitiveStyle::with_fill(c))
            .draw(d);
        px(d, 11, 1, 1, 10);
    }
}
