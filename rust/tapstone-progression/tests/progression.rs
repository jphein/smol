use tapstone_progression::{
    ATTACK_AT, LEVEL_MAX, THIRD_SLOT_AT, TOUGHNESS_AT, XP_PER_LEVEL, level_bonus, level_for_xp,
    slots, xp_next,
};

#[test]
fn level_is_one_plus_xp_over_five_capped_at_ten() {
    // 0030 ruling: "XP per level is flat: 5 XP a level. Level 10 is 45 XP".
    assert_eq!(XP_PER_LEVEL, 5);
    assert_eq!(level_for_xp(0), 1);
    assert_eq!(level_for_xp(4), 1);
    assert_eq!(level_for_xp(5), 2);
    assert_eq!(level_for_xp(44), 9);
    assert_eq!(level_for_xp(45), LEVEL_MAX);
    assert_eq!(level_for_xp(u32::MAX), LEVEL_MAX);
}

#[test]
fn xp_needed_for_the_next_level() {
    assert_eq!(xp_next(1), Some(5));
    assert_eq!(xp_next(9), Some(45));
    assert_eq!(xp_next(LEVEL_MAX), None);
}

#[test]
fn levels_give_no_stats_and_the_third_slot_opens_at_seven() {
    // 0034: "Levels give no attack and no toughness"; 0030/0031: third slot at 7.
    assert!(ATTACK_AT.is_empty() && TOUGHNESS_AT.is_empty());
    for level in 1..=LEVEL_MAX {
        assert_eq!(level_bonus(level), (0, 0), "level {level}");
    }
    assert_eq!(THIRD_SLOT_AT, 7);
    assert_eq!((slots(1), slots(6), slots(7), slots(10)), (2, 2, 3, 3));
}

use tapstone_progression::{ITEM_KEYWORDS, ITEMS, Item, Slot, item};

#[test]
fn the_item_table_is_contiguous_and_carries_both_keywords() {
    assert!(!ITEMS.is_empty(), "compile_items.py has not been run");
    for (i, d) in ITEMS.iter().enumerate() {
        assert_eq!(d.id as usize, i, "{} is out of place", d.name);
        assert_eq!(item(d.id), Some(d));
        if let Item::Keyword(k) = d.effect {
            assert!(
                ITEM_KEYWORDS.contains(&k),
                "{} grants {k:?}, outside 0034",
                d.name
            );
        }
        if d.slot == Slot::Trinket {
            assert!(d.min_level >= 7, "{} drops before its slot opens", d.name);
        }
    }
    for k in ITEM_KEYWORDS {
        assert!(
            ITEMS.iter().any(|d| d.effect == Item::Keyword(k)),
            "no {k:?} item: a loadout could never carry it"
        );
    }
    assert_eq!(item(ITEMS.len() as u16), None);
}

use tapstone_progression::{Loadout, LoadoutError, derive_commander, flat_loadout};
use tapstone_rules::{Commander, Game, HouseRules, Keyword};

const SABRE: u16 = 0; // weapon, haste
const TRIDENT: u16 = 1; // weapon, look
const PLATE: u16 = 2; // armour, taunt
const CLOAK: u16 = 3; // armour, look
const LOCKET: u16 = 4; // trinket, look

#[test]
fn derivation_is_two_four_plus_at_most_one_keyword() {
    let bare: Loadout = [None; 3];
    assert_eq!(derive_commander(1, &bare), Ok(Commander::LEVEL_1));
    assert_eq!(
        derive_commander(1, &[Some(SABRE), Some(CLOAK), None]),
        Ok(Commander::stats(2, 4, Some(Keyword::Haste)))
    );
    assert_eq!(
        derive_commander(10, &[Some(TRIDENT), Some(PLATE), Some(LOCKET)]),
        Ok(Commander::stats(2, 4, Some(Keyword::Taunt)))
    );
}

#[test]
fn derivation_refuses_what_0031_refuses_at_equip() {
    assert_eq!(
        derive_commander(6, &[None, None, Some(LOCKET)]),
        Err(LoadoutError::SlotLocked(2)),
        "the trinket slot is closed below level 7"
    );
    assert_eq!(
        derive_commander(10, &[Some(PLATE), None, None]),
        Err(LoadoutError::WrongSlot {
            slot: 0,
            design: PLATE
        })
    );
    assert_eq!(
        derive_commander(10, &[Some(99), None, None]),
        Err(LoadoutError::UnknownItem(99))
    );
    assert_eq!(
        derive_commander(10, &[Some(SABRE), Some(PLATE), None]),
        Err(LoadoutError::SecondKeyword)
    );
}

#[test]
fn every_derivable_commander_is_one_the_engine_accepts() {
    // One object (verification.md): the arena derives, the engine refuses anything else (PR #49).
    let weapons = [None, Some(SABRE), Some(TRIDENT)];
    let armours = [None, Some(PLATE), Some(CLOAK)];
    let trinkets = [None, Some(LOCKET)];
    let mut n = 0;
    for w in weapons {
        for a in armours {
            for t in trinkets {
                let Ok(c) = derive_commander(10, &[w, a, t]) else {
                    continue;
                };
                let mut g = Game::new(HouseRules::default(), [0, 1], [&[2u16; 25], &[2u16; 25]]);
                assert_eq!(g.set_commander(0, c), Ok(()), "{w:?}/{a:?}/{t:?}");
                n += 1;
            }
        }
    }
    assert!(n >= 12, "only {n} legal kits enumerated");
}

#[test]
fn flat_mode_drops_the_trinket_when_the_lower_level_has_two_slots() {
    let kit: Loadout = [Some(TRIDENT), Some(PLATE), Some(LOCKET)];
    assert_eq!(flat_loadout(6, &kit), [Some(TRIDENT), Some(PLATE), None]);
    assert_eq!(flat_loadout(7, &kit), kit);
}
