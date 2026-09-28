#![no_std]
#![forbid(unsafe_code)]
//! Commander progression, arena-side (0029: the engine never sees a level, XP or an item).
//! The arena derives `ClaimSeat` stats from these tables, and `tapstone-sim`'s bound run measures
//! the same tables, so the ≤60% bound (0030) is a claim about what the arena actually does.

pub mod items;

use tapstone_rules::state::COMMANDER_KEYWORDS;
use tapstone_rules::{Commander, Faction, Keyword};

pub use items::ITEMS;

pub const LEVEL_MAX: u8 = 10;
/// 0030 ruling: flat 5 XP a level.
pub const XP_PER_LEVEL: u32 = 5;
/// 0030: win 3, loss 1. 0031: a melted duplicate is 1.
pub const XP_WIN: u32 = 3;
pub const XP_LOSS: u32 = 1;
pub const XP_MELT: u32 = 1;
/// 0031 ruling: a drop on every third consecutive loss.
pub const DROP_EVERY_LOSSES: u32 = 3;
/// 0030: nothing for a match abandoned before round 3.
pub const ABANDON_BEFORE_ROUND: u8 = 3;
/// 0032: the loot grid is 3×4.
pub const GRID: usize = 12;
/// 0034 supersedes 0030's level bonuses: levels give no attack and no toughness.
pub const ATTACK_AT: &[u8] = &[];
pub const TOUGHNESS_AT: &[u8] = &[];
/// 0030/0031: two slots from level 1, the third (a look slot under 0034) at level 7.
pub const THIRD_SLOT_AT: u8 = 7;
/// 0034: "cosmetic tiers for the paperdoll (0032) at levels 3, 5 and 10". Levels that unlock a new
/// look and nothing else; the station's level-up line reads them from here. Added by luna-station
/// (the lead's authorisation) so the station holds no copy.
pub const COSMETIC_TIERS: [u8; 3] = [3, 5, 10];
const _: () = assert!(COSMETIC_TIERS[COSMETIC_TIERS.len() - 1] == LEVEL_MAX);
// No tier may share the third slot's level: the station's level-up line names the slot there, and a
// cosmetic tier at 7 would be silently hidden behind it. (`contains` is not const, hence the loop.)
const _: () = {
    let mut i = 0;
    while i < COSMETIC_TIERS.len() {
        assert!(
            COSMETIC_TIERS[i] != THIRD_SLOT_AT,
            "a cosmetic tier shares THIRD_SLOT_AT"
        );
        i += 1;
    }
};

/// 0034: the only item effects are the engine's commander keywords.
pub const ITEM_KEYWORDS: [Keyword; 2] = COMMANDER_KEYWORDS;

/// Level for an XP total: 1 + xp/5, capped at 10. Derived, never stored (spec §8.1).
pub fn level_for_xp(xp: u32) -> u8 {
    (1 + xp / XP_PER_LEVEL).min(LEVEL_MAX as u32) as u8
}

/// Total XP at which `level + 1` begins, or `None` at the cap.
pub fn xp_next(level: u8) -> Option<u32> {
    (level < LEVEL_MAX).then(|| u32::from(level) * XP_PER_LEVEL)
}

/// (attack, toughness) a level adds over level 1: zero at every level under 0034.
pub fn level_bonus(level: u8) -> (u8, u8) {
    let n = |at: &[u8]| at.iter().filter(|&&l| level >= l).count() as u8;
    (n(ATTACK_AT), n(TOUGHNESS_AT))
}

pub fn slots(level: u8) -> usize {
    if level >= THIRD_SLOT_AT { 3 } else { 2 }
}

/// One item's effect (0031 as amended by 0034): a commander keyword, or a look with no rule effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Keyword(Keyword),
    Look,
}

/// 0031's three slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Slot {
    Weapon = 0,
    Armour = 1,
    Trinket = 2,
}

/// An item design record (`game/items/<set>/itN-NNN.toml`, spec §8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemDesign {
    pub id: u16,
    pub name: &'static str,
    pub faction: Faction,
    pub slot: Slot,
    pub effect: Item,
    /// The level at which this item joins the loot table (0034: "a wider loot table").
    pub min_level: u8,
    /// Whether it can drop as loot; `false` = item-card only.
    pub loot: bool,
}

pub fn item(id: u16) -> Option<&'static ItemDesign> {
    ITEMS.get(id as usize).filter(|d| d.id == id)
}

/// Final stats for `level` wearing `gear`, or `None` if the kit is illegal: more items than
/// slots, a keyword outside `ITEM_KEYWORDS`, or a second keyword (refused at equip time, 0031).
pub fn try_commander_at(level: u8, gear: &[Item]) -> Option<Commander> {
    if gear.len() > slots(level) {
        return None;
    }
    let (la, lt) = level_bonus(level);
    let base = Commander::LEVEL_1;
    let mut k = base.keyword;
    for item in gear {
        if let Item::Keyword(kw) = *item {
            if k.is_some() || !ITEM_KEYWORDS.contains(&kw) {
                return None;
            }
            k = Some(kw);
        }
    }
    Some(Commander::stats(base.attack + la, base.toughness + lt, k))
}

/// As `try_commander_at`, panicking on an illegal kit.
pub fn commander_at(level: u8, gear: &[Item]) -> Commander {
    try_commander_at(level, gear).expect("illegal loadout")
}

/// A commander's worn gear: item design ids by slot (weapon, armour, trinket).
pub type Loadout = [Option<u16>; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadoutError {
    /// An item sits in a slot this level has not opened.
    SlotLocked(u8),
    /// The item's design belongs to another slot.
    WrongSlot {
        slot: u8,
        design: u16,
    },
    UnknownItem(u16),
    /// 0031: one keyword per commander; a second keyword item is refused.
    SecondKeyword,
}

/// The final `ClaimSeat` stats for a commander of `level` wearing `loadout` (0029, 0034).
pub fn derive_commander(level: u8, loadout: &Loadout) -> Result<Commander, LoadoutError> {
    let open = slots(level);
    let mut gear = [Item::Look; 3];
    let mut n = 0;
    for (slot, worn) in loadout.iter().enumerate() {
        let Some(design) = *worn else { continue };
        if slot >= open {
            return Err(LoadoutError::SlotLocked(slot as u8));
        }
        let d = item(design).ok_or(LoadoutError::UnknownItem(design))?;
        if d.slot as usize != slot {
            return Err(LoadoutError::WrongSlot {
                slot: slot as u8,
                design,
            });
        }
        gear[n] = d.effect;
        n += 1;
    }
    try_commander_at(level, &gear[..n]).ok_or(LoadoutError::SecondKeyword)
}

/// `flat` mode (0030 ruling): both commanders play at the lower level's slot count; slots that
/// level has not opened are emptied.
pub fn flat_loadout(level: u8, loadout: &Loadout) -> Loadout {
    let mut out = *loadout;
    for slot in out.iter_mut().skip(slots(level)) {
        *slot = None;
    }
    out
}
