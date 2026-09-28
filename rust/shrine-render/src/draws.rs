//! Decision 0036 on the station: every draw is a tap, and the shrine asks for each one.
//!
//! # All of it is the engine's
//!
//! The opening-hand size ([`opening_hand`]: `rules.hand` plus 0035's `second_player_bonus` for
//! seat 1, the arithmetic the engine owes at genesis), the owed-draws count (`Seat::owed_draws`,
//! 0036 in the engine since #63), the hand, and whether the mulligan window is open
//! ([`mulligan_open`]: the engine's `acted`/`mulliganed` flags). An earlier version of this module
//! stood the owed count in by hand until the engine carried it; that stand-in is gone.
//!
//! # The experience, in one paragraph
//!
//! While a seat owes draws it can do nothing else (0036), so the station makes owing a draw
//! **unmistakable and calm**: the voice band says how many and where ("draw 2 - tap each on the
//! stone"), the paperdoll raises its free hand holding a face-down card, a `draw N` chip sits on
//! the figure, and the HAND well shows `+N` beside the count. Each tapped card is acknowledged by
//! the card in the figure's hand turning face-up in the drawn card's faction colour, the count
//! ticking, and the band speaking the card's name (a pre-rendered clip, 0033). When nothing is
//! owed, the hand comes down.

use tapstone_rules::state::{Game, HouseRules, Phase};

/// Why a seat owes draws — it changes only the sentence, never the mechanics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawWhy<'a> {
    /// The opening hand, after both claims (0036).
    Opening,
    /// Redrawing after a mulligan: as many as the hand returned (0036 as clarified, #61).
    Mulligan,
    /// One at each turn start (0036).
    TurnStart,
    /// A spell says so ("draw two").
    Spell { card: &'a str },
}

/// The opening hand a seat owes: 0036's "5, plus the second-player bonus per 0035".
///
/// Derived from the rules exactly as `Game::begin_play` deals it, so a table with a different hand
/// size or bonus shows its own number.
pub const fn opening_hand(rules: &HouseRules, seat: u8) -> u8 {
    let bonus = if seat == 1 {
        rules.second_player_bonus
    } else {
        0
    };
    rules.hand.saturating_add(bonus)
}

/// Is `seat`'s mulligan window open? The engine allows one mulligan, on the seat's first turn,
/// before any other action (`rules.rs`); this reads the engine's own flags rather than guessing.
pub fn mulligan_open(g: &Game, seat: u8) -> bool {
    let s = &g.seats[seat as usize];
    g.phase == Phase::Playing && g.active == seat && !s.acted() && !s.mulliganed()
}

/// 0009's second-tap window ("tap again within 3 s"), which the mulligan prompt reuses.
pub const SECOND_TAP_MS: u32 = 3_000;

/// How a castle tap is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastleRule {
    /// 0009 as written: a castle tap passes at once. Kept as the control that shows a card-only
    /// mulligan is impossible under it.
    Immediate,
    /// The lead's ruling (2026-09-23): inside the mulligan window a castle tap opens a 3 s prompt;
    /// a second tap within it mulligans, expiry keeps and passes. Outside the window, immediate.
    PromptInWindow,
}

/// What the station sends to the engine once a castle tap is resolved. Nothing is sent while a
/// prompt is open ("ambiguity is resolved before the frame exists").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    Pass,
    Mulligan,
    /// The tap arrived when it could not act (the turn had already passed): `NotYourTurn`.
    Refused,
}

/// What one castle tap resolved to: a prompt that had already expired (a late tap reads as that
/// expiry first), and then what this tap itself sends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub expired: Option<Sent>,
    pub now: Option<Sent>,
}

/// The station's castle-tap resolver for one seat's turn.
///
/// It stores no copy of the mulligan window: every tap is given the engine's **current** state,
/// because the window closes the moment the seat acts (charges, casts), and a snapshot taken when
/// the turn began would open a prompt the engine then refuses to honour (Oracle, #59).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CastleTaps {
    pub rule: CastleRule,
    /// Still this seat's turn.
    pub my_turn: bool,
    /// A prompt opened at this time (ms), awaiting a second tap or expiry.
    pub pending: Option<u32>,
}

impl CastleTaps {
    pub fn new(rule: CastleRule) -> CastleTaps {
        CastleTaps {
            rule,
            my_turn: true,
            pending: None,
        }
    }

    /// Is the 3 s prompt showing? The band says so while it is.
    pub fn prompting(&self) -> bool {
        self.pending.is_some()
    }

    /// A castle tap at `t_ms`, with the engine's mulligan window as it stands **now**.
    ///
    /// Expiry is evaluated first, so the outcome depends only on the taps' times and never on how
    /// often `tick` happened to run: a tap after the prompt lapsed reads as that lapse (keep and
    /// pass), and is then judged afresh — by which time the turn has passed.
    pub fn tap(&mut self, t_ms: u32, window_open: bool) -> Resolved {
        let expired = self.tick(t_ms);
        Resolved {
            expired,
            now: self.fresh_tap(t_ms, window_open),
        }
    }

    fn fresh_tap(&mut self, t_ms: u32, window_open: bool) -> Option<Sent> {
        if !self.my_turn {
            return Some(Sent::Refused);
        }
        if self.rule == CastleRule::PromptInWindow && window_open {
            return match self.pending.take() {
                // Within 3 s: `tick` has already cleared any prompt that lapsed.
                Some(_) => Some(Sent::Mulligan),
                None => {
                    self.pending = Some(t_ms);
                    None
                }
            };
        }
        // Outside the window — including a window that closed while a prompt was open — a castle
        // tap passes at once (0009).
        self.pending = None;
        self.pass()
    }

    /// Time passing. An expired prompt keeps the hand and passes.
    pub fn tick(&mut self, t_ms: u32) -> Option<Sent> {
        match self.pending {
            Some(t0) if t_ms.saturating_sub(t0) > SECOND_TAP_MS => {
                self.pending = None;
                self.pass()
            }
            _ => None,
        }
    }

    fn pass(&mut self) -> Option<Sent> {
        self.my_turn = false;
        Some(Sent::Pass)
    }
}
