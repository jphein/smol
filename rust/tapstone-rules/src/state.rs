//! Fixed-size game state: no allocation, every collection is an array with a length.
use crate::cards::Keyword;
use crate::rules::Refusal;

pub const LANES: usize = 3;
pub const CELLS: usize = 3;
pub const DECK_MAX: usize = 30;
pub const HAND_MAX: usize = 10;
pub const SEATS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HouseRules {
    pub deck_size: u8,
    pub hand: u8,
    pub second_player_bonus: u8,
    pub castle_life: u8,
    pub pressure_from: u8,
    pub pressure: u8,
    pub stop_round: u8,
}

impl Default for HouseRules {
    fn default() -> Self {
        HouseRules {
            deck_size: 25,
            hand: 5,
            second_player_bonus: 1,
            castle_life: 20,
            pressure_from: 8,
            pressure: 2,
            stop_round: 12,
        }
    }
}

impl HouseRules {
    pub fn bytes(&self) -> [u8; 7] {
        [
            self.deck_size,
            self.hand,
            self.second_player_bonus,
            self.castle_life,
            self.pressure_from,
            self.pressure,
            self.stop_round,
        ]
    }
}

/// A unit in play.
///
/// Layout: `design` (2 B) plus five one-byte fields — `attack`, `toughness`, `damage`,
/// `Option<Keyword>` (1 B by niche) and `entered_round` — is 7 B, rounded to 8 at align 2, so the
/// struct carries one byte of slack. Dropping one more `u8` is therefore free; dropping two would
/// take it to 6 and move `Option<Unit>` and every cell offset in the canonical image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    pub design: u16,
    pub attack: u8,
    pub toughness: u8,
    pub damage: u8,
    pub keyword: Option<Keyword>,
    pub entered_round: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Castle {
    pub life: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seat {
    pub castle_design: u16,
    pub castle: Castle,
    pub deck: [u16; DECK_MAX],
    pub deck_len: u8,
    pub deck_pos: u8,
    pub hand: [u16; HAND_MAX],
    pub hand_len: u8,
    pub charged: u8,
    pub spent: u8,
    pub charged_this_round: bool,
    pub lanes_advanced: [bool; LANES],
    /// cells[lane][cell]: cell 0 = back (next to my castle), 2 = front
    pub cells: [[Option<Unit>; CELLS]; LANES],
    pub present: bool,
    /// Set once this seat has applied any accepted non-Mulligan event while Playing; closes the mulligan.
    pub acted: bool,
    pub mulliganed: bool,
}

impl Seat {
    pub const fn empty() -> Seat {
        Seat {
            castle_design: 0,
            castle: Castle { life: 0 },
            deck: [0; DECK_MAX],
            deck_len: 0,
            deck_pos: 0,
            hand: [0; HAND_MAX],
            hand_len: 0,
            charged: 0,
            spent: 0,
            charged_this_round: false,
            lanes_advanced: [false; LANES],
            cells: [[None; CELLS]; LANES],
            present: false,
            acted: false,
            mulliganed: false,
        }
    }

    pub fn hand_len(&self) -> usize {
        self.hand_len as usize
    }

    pub fn available_mana(&self) -> u8 {
        debug_assert!(self.spent <= self.charged, "spent exceeds charged");
        self.charged.saturating_sub(self.spent)
    }

    pub fn units(&self) -> usize {
        self.cells.iter().flatten().flatten().count()
    }

    /// Draw one; a drawn-out deck draws nothing (no penalty in v0).
    pub fn draw(&mut self) -> bool {
        if self.deck_pos >= self.deck_len || (self.hand_len as usize) >= HAND_MAX {
            return false;
        }
        self.hand[self.hand_len as usize] = self.deck[self.deck_pos as usize];
        self.hand_len += 1;
        self.deck_pos += 1;
        true
    }

    /// Remove the first card matching `design`; the rest keep their order.
    pub fn remove_from_hand(&mut self, design: u16) -> bool {
        let len = self.hand_len as usize;
        if let Some(i) = self.hand[..len].iter().position(|&c| c == design) {
            self.hand.copy_within(i + 1..len, i);
            self.hand_len -= 1;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Lobby,
    Playing,
    Over,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Winner {
    Seat(u8),
    /// Reserved. v0 never produces a draw: `finish` breaks ties on units, then on seat 1.
    Draw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Game {
    pub rules: HouseRules,
    pub phase: Phase,
    pub round: u8,
    pub active: u8,
    pub seats: [Seat; SEATS],
    pub winner: Option<Winner>,
    pub seq: u16,
}

impl Game {
    /// Lobby state, nobody seated. Decks are in play order (the arbiter shuffled; the physical deck
    /// is what the player shuffled) and are clamped to `DECK_MAX` and `rules.deck_size`. The castle
    /// designs are defaults until a `ClaimSeat` replaces them.
    pub fn new(rules: HouseRules, castles: [u16; SEATS], decks: [&[u16]; SEATS]) -> Game {
        let mut seats = [Seat::empty(); SEATS];
        for (seat, (castle, deck)) in seats.iter_mut().zip(castles.iter().zip(decks.iter())) {
            seat.castle_design = *castle;
            seat.castle.life = rules.castle_life;
            let n = deck.len().min(DECK_MAX).min(rules.deck_size as usize);
            seat.deck[..n].copy_from_slice(&deck[..n]);
            seat.deck_len = n as u8;
            seat.present = false;
        }
        Game {
            rules,
            phase: Phase::Lobby,
            round: 0,
            active: 0,
            seats,
            winner: None,
            seq: 0,
        }
    }

    /// Sim and test shortcut: deals hands and enters Playing without `ClaimSeat` records. The shrine
    /// reaches Playing through two `ClaimSeat` taps; a game started this way has no lobby records and
    /// no claims to replay. A no-op outside the Lobby.
    #[doc(hidden)]
    pub fn started(mut self) -> Game {
        if self.phase != Phase::Lobby {
            return self;
        }
        for seat in &mut self.seats {
            seat.present = true;
        }
        self.begin_play();
        self
    }

    /// Deal opening hands (second player bonus) and start round 1 with seat 0 active.
    pub(crate) fn begin_play(&mut self) {
        for (s, seat) in self.seats.iter_mut().enumerate() {
            let bonus = if s == 1 {
                self.rules.second_player_bonus
            } else {
                0
            };
            for _ in 0..self.rules.hand.saturating_add(bonus) {
                seat.draw();
            }
        }
        self.phase = Phase::Playing;
        self.round = 1;
        self.active = 0;
    }

    /// Replace the house rules while still in the Lobby: castles take the new life, decks re-clamp
    /// to the new `deck_size`. Anywhere else → `Refusal::LobbyClosed`.
    pub fn with_rules(&mut self, rules: HouseRules) -> Result<(), Refusal> {
        if self.phase != Phase::Lobby {
            return Err(Refusal::LobbyClosed);
        }
        self.rules = rules;
        for seat in &mut self.seats {
            seat.castle.life = rules.castle_life;
            seat.deck_len = seat.deck_len.min(rules.deck_size);
        }
        Ok(())
    }
}
