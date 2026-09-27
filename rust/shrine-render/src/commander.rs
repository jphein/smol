//! The view-side commander: what the station needs to draw a paperdoll, a loadout and a ledger.
//!
//! # Why this lives here and not in the rules crate
//!
//! Decisions 0029–0031 give every seat a commander, but `tapstone-rules` has none yet (a sibling
//! lane is adding them on `feat/commander-rules`). The screens cannot wait for it, so this module
//! defines the **smallest display model** the station needs. It is deliberately shaped so wiring it
//! to the engine later is a set of `From` impls, not a redesign:
//!
//! * [`Commander`] is the ledger's view (0030): name, faction, level, XP, loadout. The engine never
//!   sees any of it — genesis hashes only the derived attack/toughness/keyword (0029) — so this
//!   struct stays view-side forever. Only its *source* changes: today a literal, later a ledger
//!   read over the mesh.
//! * **Stats are the engine's.** The lobby shows [`Commander::claim`], which is the claim the
//!   arena sends at `ClaimSeat` (0029): the engine's own `Commander::LEVEL_1` base plus the one
//!   keyword the gear grants (0034). The station reads the seat's `tapstone_rules::Commander`
//!   from the live `Game`. No stat table lives here, so there is nothing to drift.
//! * [`Presence`] is derived from the engine seat ([`Presence::from_seat`]): the commander's cell
//!   (design `COMMANDER_DESIGN`) while it is on the board, or its `returns` round and `lane` while
//!   it is dead. The fall penalty and return delay are read from the game's house rules.
//! * Items, slots, the level table and the ClaimSeat derivation are **progression's**
//!   (`tapstone-progression`, #65): the generated [`ITEMS`] from `game/items/set1/*.toml`,
//!   `derive_commander` behind [`Commander::claim`], and `level_for_xp` behind
//!   [`Commander::level`]. This module holds none of their numbers.

use tapstone_rules::cards::{Faction, Keyword};
use tapstone_rules::state::{CELLS, COMMANDER_DESIGN, Game, LANES, Seat};

// Items are progression's generated designs (`game/items/set1/*.toml` via tools/compile_items.py,
// #65): one table for the arena, the sim and the station. The station's names for the types:
/// Gear slots (0031): weapon, armour, trinket — progression's own enum.
pub use tapstone_progression::Slot;
/// An item design record (id, name, faction, slot, effect, `min_level`, `loot`).
pub type Item = tapstone_progression::ItemDesign;
/// An item's effect (0034): a commander keyword, or a look with no rule effect.
pub use tapstone_progression::Item as Effect;
/// A commander's worn gear: design ids by slot.
pub use tapstone_progression::Loadout;
/// The item table, and lookup by id.
pub use tapstone_progression::{ITEMS, item};

/// The three slots in their loadout order.
pub const SLOTS: [Slot; 3] = [Slot::Weapon, Slot::Armour, Slot::Trinket];

/// The station's view of a slot: its loadout index and the word the band uses for it.
pub trait SlotExt {
    fn index(self) -> usize;
    fn name(self) -> &'static str;
}

impl SlotExt for Slot {
    fn index(self) -> usize {
        self as usize
    }
    fn name(self) -> &'static str {
        match self {
            Slot::Weapon => "weapon",
            Slot::Armour => "armour",
            Slot::Trinket => "trinket",
        }
    }
}

/// Keywords an item may grant: progression's `ITEM_KEYWORDS`, which is the engine's
/// `COMMANDER_KEYWORDS` (0034: Haste and Taunt), so a keyword the engine would refuse at
/// `ClaimSeat` can never be offered in the lobby.
pub fn item_keyword_allowed(k: Keyword) -> bool {
    tapstone_progression::ITEM_KEYWORDS.contains(&k)
}

/// The band's sentence for a refused equip. Exhaustive over progression's `LoadoutError`, so a new
/// reason will not compile here until it has a sentence.
pub fn equip_voice<'a>(r: EquipRefusal, item_name: &'a str) -> crate::voice::Voice<'a> {
    use crate::voice::Voice;
    use tapstone_progression::LoadoutError;
    match r {
        EquipRefusal::Loadout(LoadoutError::SecondKeyword) => Voice::SecondKeyword,
        EquipRefusal::Loadout(LoadoutError::SlotLocked(_)) => Voice::TooLow {
            item: item_name,
            level: TRINKET_LEVEL,
        },
        // Unreachable from the grid (an item goes to its own slot, and the grid holds only known
        // designs), but named: the shrine does not know that item.
        EquipRefusal::Loadout(LoadoutError::WrongSlot { .. } | LoadoutError::UnknownItem(_)) => {
            Voice::Refused(tapstone_rules::rules::Refusal::UnknownCard)
        }
    }
}

/// Why the station refuses to put an item on: progression's own `derive_commander` error, and
/// nothing else. Equipping is **not** level-gated (the lead's ruling, 2026-09-23): `min_level` is
/// a loot-table rule — the level at which an item can drop — and the only level gate is the third
/// slot (`THIRD_SLOT_AT`), which `derive_commander` reports as `SlotLocked`. A station-only gate
/// would refuse in the lobby what ClaimSeat accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquipRefusal {
    Loadout(tapstone_progression::LoadoutError),
}

// Progression's numbers have one home, `tapstone-progression` (#65), which the arena derives
// ClaimSeat from and the sim's balance bound measures. Re-exported under the station's names so the
// screens read the same values the arena plays by; a copy here would be a second opinion.
/// Inventory cells the screen shows at once (0032's grid of 12).
pub use tapstone_progression::GRID as INVENTORY;
pub use tapstone_progression::LEVEL_MAX;
/// The trinket slot — a look slot under 0034 — opens at this level (0030, 0031).
pub use tapstone_progression::THIRD_SLOT_AT as TRINKET_LEVEL;
pub use tapstone_progression::XP_PER_LEVEL;

/// What reaching `level` unlocks, for the level-up line (0034: levels buy **options**, never
/// stats): the third gear slot (`TRINKET_LEVEL`, a look slot) and progression's `COSMETIC_TIERS`.
/// `None` if the level unlocks nothing the station shows (0034's wider loot table is the arena's).
///
/// Kept short enough to share the 96 px left column with a level number; the gutter test holds it.
pub fn level_gain(level: u8) -> Option<&'static str> {
    if level == TRINKET_LEVEL {
        Some("3rd slot")
    } else if tapstone_progression::COSMETIC_TIERS.contains(&level) {
        Some("new look")
    } else {
        None
    }
}

/// XP needed to go from `level` to `level + 1`, or `None` at the cap.
///
/// Derived from progression's `xp_next`, the **cumulative** XP at which the next level begins: the
/// bar's denominator is the gap between this level's start and the next's. (The ledger stores total
/// XP, and so does `Commander::xp`; the bar draws the XP into the level.)
pub fn xp_to_next(level: u8) -> Option<u16> {
    let start = if level <= 1 {
        0
    } else {
        tapstone_progression::xp_next(level - 1)?
    };
    tapstone_progression::xp_next(level).map(|next| (next - start) as u16)
}

/// The ledger's view of one commander (0030).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commander<'a> {
    pub name: &'a str,
    pub faction: Faction,
    /// **Total** XP, as the ledger stores it; the level is derived, never stored (spec §8.1).
    pub xp: u32,
    pub loadout: Loadout,
}

impl Commander<'_> {
    /// The level, from progression's own `level_for_xp`.
    pub fn level(&self) -> u8 {
        tapstone_progression::level_for_xp(self.xp)
    }

    /// XP into the current level, for the bar.
    pub fn xp_into_level(&self) -> u32 {
        xp_into(self.xp)
    }

    /// Is `s` open at this commander's level? Progression's `slots`.
    pub fn slot_open(&self, s: Slot) -> bool {
        slot_open_at(self.level(), s)
    }

    pub fn worn(&self, s: Slot) -> Option<&'static Item> {
        if !self.slot_open(s) {
            return None;
        }
        self.loadout[s.index()].and_then(item)
    }

    /// The claim the arena sends at `ClaimSeat` (0029): **progression's `derive_commander`**, the
    /// derivation the arena runs, so the lobby shows exactly what ClaimSeat will carry. An illegal
    /// loadout is the arena's error, not a guess.
    pub fn claim(&self) -> Result<tapstone_rules::Commander, tapstone_progression::LoadoutError> {
        tapstone_progression::derive_commander(self.level(), &self.loadout)
    }

    /// The keyword the claim carries, if any (0034: at most one, Haste or Taunt).
    pub fn keyword(&self) -> Option<Keyword> {
        self.claim().ok().and_then(|c| c.keyword)
    }

    /// Could this commander put `id` on now? Exactly progression's derivation on the loadout with
    /// the item in its slot — one source with ClaimSeat.
    pub fn can_equip(&self, id: u16) -> Result<(), EquipRefusal> {
        let d = item(id).ok_or(EquipRefusal::Loadout(
            tapstone_progression::LoadoutError::UnknownItem(id),
        ))?;
        let mut next = self.loadout;
        next[d.slot.index()] = Some(id);
        tapstone_progression::derive_commander(self.level(), &next)
            .map(|_| ())
            .map_err(EquipRefusal::Loadout)
    }
}

/// The total XP at which `level` begins (0 for level 1), from progression's `xp_next`.
pub fn level_start(level: u8) -> u32 {
    if level <= 1 {
        0
    } else {
        tapstone_progression::xp_next(level - 1).unwrap_or(0)
    }
}

/// XP into the level a total belongs to.
pub fn xp_into(total: u32) -> u32 {
    total - level_start(tapstone_progression::level_for_xp(total))
}

/// Is `s` open at `level`? Progression's slot count.
pub fn slot_open_at(level: u8, s: Slot) -> bool {
    s.index() < tapstone_progression::slots(level)
}

/// The ledger's inventory (0031): earned loot plus item cards tapped in this lobby.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ledger {
    pub inventory: [Option<u16>; INVENTORY],
}

impl Ledger {
    /// Where a loot chest opens (0032). `None` when all twelve cells are full — a case 0032 does
    /// not cover; see the findings.
    pub fn first_free(&self) -> Option<usize> {
        self.inventory.iter().position(|c| c.is_none())
    }
}

/// The in-match commander, derived from the engine seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    /// On the board: `cell` 0 = back (next to my castle), 2 = front, as `Seat::cells`.
    OnBoard { lane: u8, cell: u8, hp: u8 },
    /// Dead. It re-enters the back cell of `lane` at the start of its owner's turn in
    /// `return_round` or later — later if that cell is occupied, and the engine then leaves
    /// `return_round` in the past while it waits (0029).
    Fallen { lane: u8, return_round: u8 },
}

impl Presence {
    /// Read the commander from the engine: `returns > 0` means dead (0029), otherwise it is the
    /// unit carrying `COMMANDER_DESIGN`. `None` only for a seat with no commander on the board and
    /// none returning, which the engine does not produce once play has begun.
    pub fn from_seat(s: &Seat) -> Option<Presence> {
        let c = s.commander;
        if c.returns > 0 {
            return Some(Presence::Fallen {
                lane: c.lane,
                return_round: c.returns,
            });
        }
        for (lane, cells) in s.cells.iter().enumerate() {
            for (cell, u) in cells.iter().enumerate() {
                if let Some(u) = u.filter(|u| u.design == COMMANDER_DESIGN) {
                    return Some(Presence::OnBoard {
                        lane: lane as u8,
                        cell: cell as u8,
                        hp: u.toughness.saturating_sub(u.damage),
                    });
                }
            }
        }
        None
    }
}

/// What a fallen commander is doing, derived from the round rather than stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnState {
    /// `n` rounds to go.
    In(u8),
    /// The round has arrived and the back cell is free: it enters at the owner's turn start.
    ThisTurn,
    /// The round has arrived (or passed) and the back cell is occupied: it waits (0029).
    Blocked,
}

/// Rounds until return, saturating: a blocked commander's return round is in the past, and an
/// unsigned subtraction there would wrap to 250-odd rounds.
pub const fn rounds_until(return_round: u8, round: u8) -> u8 {
    return_round.saturating_sub(round)
}

/// Everything the in-match station shows, as a value, **all of it read from the engine**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Station {
    pub castle_design: u16,
    pub castle_life: u8,
    pub castle_max: u8,
    pub charged: u8,
    pub spent: u8,
    /// Mana available now, as the engine computes it (`Seat::available_mana`), not re-derived.
    pub mana: u8,
    pub hand: u8,
    pub round: u8,
    pub stop_round: u8,
    pub pressure_from: u8,
    /// Is each lane's back cell free of anything but this seat's own commander? A returning
    /// commander needs it (0029).
    pub back_free: [bool; LANES],
    /// The seat's commander as the engine holds it: final attack, toughness, keyword (0029).
    pub attack: u8,
    pub toughness: u8,
    pub keyword: Option<Keyword>,
    /// 0029's house rules, from the game: castle life lost on a fall, rounds until return.
    pub fall_penalty: u8,
    pub return_delay: u8,
    pub presence: Presence,
    /// Draws this seat owes (0036), the engine's own per-seat counter (`Seat::owed_draws`).
    pub owed: u8,
    /// The seat's mulligan window, from the engine's own flags (`draws::mulligan_open`).
    pub mulligan_open: bool,
    /// The seat's opening-hand size, from the rules (`draws::opening_hand`): what it owes after the
    /// claims (0036). A mulligan owes the size of the hand returned instead (#61), which can differ.
    pub opening: u8,
}

impl Station {
    /// Read the whole station from a real engine `Game`. `None` before genesis (no commander).
    pub fn from_game(g: &Game, seat: u8) -> Option<Station> {
        let s = &g.seats[seat as usize];
        let presence = Presence::from_seat(s)?;
        if let Presence::OnBoard { lane, cell, .. } = presence {
            debug_assert!((lane as usize) < LANES && (cell as usize) < CELLS);
        }
        Some(Station {
            castle_design: s.castle_design,
            castle_life: s.castle.life,
            castle_max: g.rules.castle_life,
            charged: s.charged,
            spent: s.spent,
            mana: s.available_mana(),
            hand: s.hand_len,
            round: g.round,
            stop_round: g.rules.stop_round,
            pressure_from: g.rules.pressure_from,
            back_free: core::array::from_fn(|l| {
                s.cells[l][0].is_none_or(|u| u.design == COMMANDER_DESIGN)
            }),
            attack: s.commander.attack,
            toughness: s.commander.toughness,
            keyword: s.commander.keyword,
            fall_penalty: g.rules.commander_fall,
            return_delay: g.rules.commander_return,
            presence,
            owed: s.owed_draws(),
            mulligan_open: crate::draws::mulligan_open(g, seat),
            opening: crate::draws::opening_hand(&g.rules, seat),
        })
    }

    /// Mana available this round.
    pub fn mana(&self) -> u8 {
        self.mana
    }

    /// A fallen commander's return, or `None` if it is on the board.
    pub fn return_state(&self) -> Option<ReturnState> {
        match self.presence {
            Presence::OnBoard { .. } => None,
            Presence::Fallen { lane, return_round } => {
                Some(match rounds_until(return_round, self.round) {
                    0 if self.back_free[lane as usize] => ReturnState::ThisTurn,
                    0 => ReturnState::Blocked,
                    n => ReturnState::In(n),
                })
            }
        }
    }
}

pub const fn keyword_name(k: Keyword) -> &'static str {
    match k {
        Keyword::Ranged => "Ranged",
        Keyword::Shield1 => "Shield 1",
        Keyword::Haste => "Haste",
        Keyword::Rush => "Rush",
        Keyword::Taunt => "Taunt",
    }
}
