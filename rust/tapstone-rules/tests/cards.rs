use tapstone_rules::cards::design;
use tapstone_rules::{CardKind, Effect, Faction, Keyword, SET1};

#[test]
fn set1_has_fourteen_designs_with_unique_ids() {
    assert_eq!(SET1.len(), 14);
    for (i, c) in SET1.iter().enumerate() {
        assert_eq!(c.id as usize, i, "design index must equal its id");
        assert!(!c.name.is_empty());
    }
}

#[test]
fn ember_vanguard_is_a_rush_unit() {
    let c = SET1.iter().find(|c| c.name == "Ashen Vanguard").unwrap();
    assert_eq!(c.faction, Faction::Ember);
    assert_eq!(
        c.kind,
        CardKind::Unit {
            attack: 3,
            toughness: 2,
            keyword: Some(Keyword::Rush)
        }
    );
    assert_eq!(c.cost, 3);
}

#[test]
fn tide_bolt_is_a_damage_spell() {
    let c = SET1.iter().find(|c| c.name == "Tidal Lash").unwrap();
    assert_eq!(
        c.kind,
        CardKind::Spell(Effect::Damage {
            amount: 3,
            castle_ok: false
        })
    );
}

#[test]
fn design_lookup_is_bounds_checked() {
    assert_eq!(design(13).map(|d| d.name), Some("Riptide"));
    assert!(design(14).is_none());
}

#[test]
fn mend_heals_and_riptide_destroys() {
    let mend = SET1.iter().find(|c| c.name == "Mend").unwrap();
    assert_eq!(mend.faction, Faction::Neutral);
    assert_eq!(mend.cost, 1);
    assert_eq!(mend.kind, CardKind::Spell(Effect::Heal { amount: 2 }));
    let riptide = SET1.iter().find(|c| c.name == "Riptide").unwrap();
    assert_eq!(riptide.faction, Faction::Tide);
    assert_eq!(riptide.cost, 3);
    assert_eq!(
        riptide.kind,
        CardKind::Spell(Effect::Destroy { max_toughness: 2 })
    );
}
