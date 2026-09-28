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
    /// Castle life lost when the seat's commander dies (0029).
    pub commander_fall: u8,
    /// Rounds until a dead commander returns, at its owner's turn start (0029).
    pub commander_return: u8,
}

impl Default for HouseRules {
    fn default() -> Self {
        HouseRules {
            // 30 since #147 (2026-09-27, amending 0011's 25): ten designs at three copies each.
            deck_size: 30,
            hand: 5,
            // 1 since decision 0035. 0026 set it to 0 because the extra card compounded seat 1's
            // edge. The commander (0029) moved the edge to seat 0, and 1 now has the best worst
            // case on both mirrors (10.8 points from fair, against 12.9 at 0 and 15.8 at 2).
            second_player_bonus: 1,
            castle_life: 20,
            pressure_from: 8,
            pressure: 2,
            stop_round: 12,
            commander_fall: 3,
            commander_return: 2,
        }
    }
}

impl HouseRules {
    pub fn bytes(&self) -> [u8; 9] {
        [
            self.deck_size,
            self.hand,
            self.second_player_bonus,
            self.castle_life,
            self.pressure_from,
            self.pressure,
            self.stop_round,
            self.commander_fall,
            self.commander_return,
        ]
    }

    /// The inverse of `bytes()`, in the same order (the order genesis hashes). A journal or a CFG
    /// `M` value restores through here, never through a second list of field positions.
    pub fn from_bytes(b: [u8; 9]) -> HouseRules {
        HouseRules {
            deck_size: b[0],
            hand: b[1],
            second_player_bonus: b[2],
            castle_life: b[3],
            pressure_from: b[4],
            pressure: b[5],
            stop_round: b[6],
            commander_fall: b[7],
            commander_return: b[8],
        }
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

/// The design id a commander's cell carries (0029). Not a card: `design()` returns `None` for it,
/// and it is one below 0xFFFF, the canonical image's empty-cell marker.
pub const COMMANDER_DESIGN: u16 = 0xFFFE;

/// The only keywords a commander may carry (0034). Ranged and Shield 1 were measured far over
/// 0030's bound on a commander; Rush is inert on one, since it always enters the back cell (0029).
pub const COMMANDER_KEYWORDS: [Keyword; 2] = [Keyword::Haste, Keyword::Taunt];

/// The lane a commander enters at genesis: the middle one (0029).
pub const COMMANDER_LANE: u8 = 1;

/// A seat's commander (0029). The stats are final — the arena derives them from level (0030) and
/// gear (0031) — and are fixed at genesis; the engine never sees a level or an item.
/// `returns` is 0 while the commander is on the board, otherwise the round at whose owner-turn
/// start it comes back to the back cell of `lane`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commander {
    pub attack: u8,
    pub toughness: u8,
    pub keyword: Option<Keyword>,
    pub returns: u8,
    pub lane: u8,
}

impl Commander {
    /// 0029's level-1 base: 2 attack, 4 toughness, no keyword. The arena adds level bonuses and
    /// gear to this; the engine uses it only as the default for a seat no claim has set.
    pub const LEVEL_1: Commander = Commander::stats(2, 4, None);

    pub const fn stats(attack: u8, toughness: u8, keyword: Option<Keyword>) -> Commander {
        Commander {
            attack,
            toughness,
            keyword,
            returns: 0,
            lane: COMMANDER_LANE,
        }
    }

    /// The commander as a unit entering the back cell in `round`.
    pub fn unit(&self, entered_round: u8) -> Unit {
        Unit {
            design: COMMANDER_DESIGN,
            attack: self.attack,
            toughness: self.toughness,
            damage: 0,
            keyword: self.keyword,
            entered_round,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Castle {
    pub life: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seat {
    pub castle_design: u16,
    pub castle: Castle,
    /// The seat's undrawn copies (0036): a LIST, a multiset of designs whose order means nothing to
    /// the rules. A draw removes one copy and a mulligan puts the hand back. The physical deck is
    /// shuffled at the table, and the engine never learns its order.
    pub deck: [u16; DECK_MAX],
    pub deck_len: u8,
    /// Draws this seat owes (0036): the opening hand, one at each turn start, and a spell's count.
    /// Never more than the undrawn copies or the free hand slots. While it is non-zero the seat can
    /// only tap `Draw`. Takes the byte `deck_pos` held when the deck had an order.
    pub owed: u8,
    pub hand: [u16; HAND_MAX],
    pub hand_len: u8,
    pub charged: u8,
    pub spent: u8,
    /// Turn flags, one bit each, in the canonical image's own layout (it hashes this byte as is):
    /// bit 0 charged this round, bits 1..=3 lane k advanced this turn, bit 4 acted, bit 5
    /// mulliganed, bit 6 present. One byte instead of seven bools is what keeps `Game` inside
    /// its 350 B budget with a commander per seat (0029). Read and write through the accessors.
    pub flags: u8,
    /// cells[lane][cell]: cell 0 = back (next to my castle), 2 = front
    pub cells: [[Option<Unit>; CELLS]; LANES],
    pub commander: Commander,
}

impl Seat {
    pub const fn empty() -> Seat {
        Seat {
            castle_design: 0,
            castle: Castle { life: 0 },
            deck: [0; DECK_MAX],
            deck_len: 0,
            owed: 0,
            hand: [0; HAND_MAX],
            hand_len: 0,
            charged: 0,
            spent: 0,
            flags: 0,
            cells: [[None; CELLS]; LANES],
            commander: Commander::LEVEL_1,
        }
    }

    const CHARGED: u8 = 1 << 0;
    const ACTED: u8 = 1 << (LANES + 1);
    const MULLIGANED: u8 = 1 << (LANES + 2);
    const PRESENT: u8 = 1 << (LANES + 3);
    const fn lane_bit(lane: usize) -> u8 {
        1 << (lane + 1)
    }
    fn set(&mut self, bit: u8, on: bool) {
        if on {
            self.flags |= bit;
        } else {
            self.flags &= !bit;
        }
    }

    pub fn charged_this_round(&self) -> bool {
        self.flags & Self::CHARGED != 0
    }
    pub fn set_charged_this_round(&mut self, on: bool) {
        self.set(Self::CHARGED, on)
    }
    /// Lane `lane` advanced this turn; a lane past `LANES` reads false.
    pub fn lane_advanced(&self, lane: usize) -> bool {
        lane < LANES && self.flags & Self::lane_bit(lane) != 0
    }
    pub fn set_lane_advanced(&mut self, lane: usize, on: bool) {
        if lane < LANES {
            self.set(Self::lane_bit(lane), on)
        }
    }
    pub fn clear_lanes_advanced(&mut self) {
        for l in 0..LANES {
            self.set_lane_advanced(l, false);
        }
    }
    /// Set once this seat has applied any accepted non-Mulligan event while Playing; closes the mulligan.
    pub fn acted(&self) -> bool {
        self.flags & Self::ACTED != 0
    }
    pub fn set_acted(&mut self, on: bool) {
        self.set(Self::ACTED, on)
    }
    pub fn mulliganed(&self) -> bool {
        self.flags & Self::MULLIGANED != 0
    }
    pub fn set_mulliganed(&mut self, on: bool) {
        self.set(Self::MULLIGANED, on)
    }
    pub fn present(&self) -> bool {
        self.flags & Self::PRESENT != 0
    }
    pub fn set_present(&mut self, on: bool) {
        self.set(Self::PRESENT, on)
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

    pub fn owed_draws(&self) -> u8 {
        self.owed
    }

    /// Owe `n` more draws, then clamp (0036): an exhausted list owes nothing, and a draw with no
    /// free hand slot is dropped, as the digital draw dropped it before.
    pub fn owe(&mut self, n: u8) {
        self.owed = self.owed.saturating_add(n);
        self.clamp_owed();
    }

    pub(crate) fn clamp_owed(&mut self) {
        let room = (HAND_MAX - self.hand_len()) as u8;
        self.owed = self.owed.min(self.deck_len).min(room);
    }

    /// Whether an undrawn copy of `design` is in the list.
    pub fn undrawn(&self, design: u16) -> bool {
        self.deck[..self.deck_len as usize].contains(&design)
    }

    /// Move one copy of `design` from the list to the hand; `false` if none is left or the hand is
    /// full. The list keeps its remaining order, so identical lists evolve identically.
    pub(crate) fn draw_design(&mut self, design: u16) -> bool {
        let len = self.deck_len as usize;
        let Some(i) = self.deck[..len].iter().position(|&c| c == design) else {
            return false;
        };
        if self.hand_len() >= HAND_MAX {
            return false;
        }
        self.deck.copy_within(i + 1..len, i);
        self.deck_len -= 1;
        self.hand[self.hand_len()] = design;
        self.hand_len += 1;
        true
    }

    /// Return the whole hand to the list (a mulligan, 0036). Returns how many cards went back.
    pub(crate) fn return_hand(&mut self) -> u8 {
        let n = self.hand_len;
        for i in 0..n as usize {
            // The hand's cards came from this list, so the list has room for them.
            if (self.deck_len as usize) < DECK_MAX {
                self.deck[self.deck_len as usize] = self.hand[i];
                self.deck_len += 1;
            }
        }
        self.hand_len = 0;
        n
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

/// The shrine's RAM budget for one `Game` (smol issue 10; smol a1b4174 asserts the same bound in
/// the firmware). Asserted here too so every compiler that builds this crate — host, the
/// thumbv7em gate and the Xtensa build — evaluates it for its own target.
pub const GAME_BUDGET: usize = 350;
const _: () = assert!(
    core::mem::size_of::<Game>() <= GAME_BUDGET,
    "size_of::<Game>() exceeds the shrine's 350 B budget on this target; changing the budget is a ruling"
);

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
    /// Lobby state, nobody seated. Each deck is a LIST of designs (0036): its order is not a rule
    /// input, and genesis hashes it sorted. It is clamped to `DECK_MAX` and `rules.deck_size`. The
    /// castle designs are defaults until a `ClaimSeat` replaces them.
    pub fn new(rules: HouseRules, castles: [u16; SEATS], decks: [&[u16]; SEATS]) -> Game {
        let mut seats = [Seat::empty(); SEATS];
        for (seat, (castle, deck)) in seats.iter_mut().zip(castles.iter().zip(decks.iter())) {
            seat.castle_design = *castle;
            seat.castle.life = rules.castle_life;
            let n = deck.len().min(DECK_MAX).min(rules.deck_size as usize);
            seat.deck[..n].copy_from_slice(&deck[..n]);
            seat.deck_len = n as u8;
            seat.set_present(false);
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

    /// Sim and test shortcut: enters Playing without `ClaimSeat` records and pays both opening
    /// hands from the lists in list order, without `Draw` records. The shrine reaches Playing
    /// through two `ClaimSeat` taps and then taps its draws; a game started this way has no lobby
    /// records and no draws to replay. A no-op outside the Lobby.
    #[doc(hidden)]
    pub fn started(mut self) -> Game {
        if self.phase != Phase::Lobby {
            return self;
        }
        for seat in &mut self.seats {
            seat.set_present(true);
        }
        self.begin_play();
        for seat in &mut self.seats {
            while seat.owed > 0 {
                let top = seat.deck[0];
                seat.draw_design(top);
                seat.owed -= 1;
                seat.clamp_owed();
            }
        }
        self
    }

    /// The first undrawn copy in `seat`'s list, in list order: what the sim's and the tests'
    /// "draw from the top" means once the list is the physical deck's order. Not a rule.
    #[doc(hidden)]
    pub fn top_of_list(&self, seat: u8) -> Option<u16> {
        let s = self.seats.get(seat as usize)?;
        (s.deck_len > 0).then(|| s.deck[0])
    }

    /// Owe both opening hands (second-player bonus to seat 1, 0035) and start round 1 with seat 0
    /// active. Nothing is drawn here: every draw is a tap (0036).
    pub(crate) fn begin_play(&mut self) {
        for (s, seat) in self.seats.iter_mut().enumerate() {
            let bonus = if s == 1 {
                self.rules.second_player_bonus
            } else {
                0
            };
            seat.owe(self.rules.hand.saturating_add(bonus));
            // On the board before round 1, so entered_round 0: it may advance on its first turn.
            let c = seat.commander;
            seat.cells[COMMANDER_LANE as usize][0] = Some(c.unit(0));
        }
        self.phase = Phase::Playing;
        self.round = 1;
        self.active = 0;
    }

    /// Set a seat's commander stats while still in the Lobby (the `ClaimSeat` path and the sim's).
    /// Toughness 0 would die at genesis, and a keyword outside `COMMANDER_KEYWORDS` (0034) is not
    /// allowed on a commander, so both are `BadTarget`; anywhere but the Lobby → `LobbyClosed`.
    pub fn set_commander(&mut self, seat: u8, c: Commander) -> Result<(), Refusal> {
        if self.phase != Phase::Lobby {
            return Err(Refusal::LobbyClosed);
        }
        if seat as usize >= SEATS {
            return Err(Refusal::NotYourTurn);
        }
        if c.toughness == 0 || c.keyword.is_some_and(|k| !COMMANDER_KEYWORDS.contains(&k)) {
            return Err(Refusal::BadTarget);
        }
        self.seats[seat as usize].commander = Commander::stats(c.attack, c.toughness, c.keyword);
        Ok(())
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
