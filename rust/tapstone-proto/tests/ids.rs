use tapstone_proto::ids::{deck_sigil, rules_id};
use tapstone_rules::HouseRules;

#[test]
fn a_deck_sigil_names_the_list_not_the_order() {
    let a = [2u16, 3, 4, 2, 11];
    let b = [11u16, 2, 4, 3, 2];
    assert_eq!(
        deck_sigil(&a),
        deck_sigil(&b),
        "shuffling must not change the deck's name"
    );
    assert_ne!(deck_sigil(&a), deck_sigil(&[2, 3, 4, 2, 12]));
    assert_ne!(
        deck_sigil(&a),
        deck_sigil(&[2, 3, 4, 11]),
        "copy counts are part of the list"
    );
}

#[test]
fn rules_id_moves_with_every_rule_byte() {
    let d = HouseRules::default();
    let base = rules_id(&d);
    for i in 0..d.bytes().len() {
        let mut b = d.bytes();
        b[i] = b[i].wrapping_add(1);
        assert_ne!(
            tapstone_proto::ids::rules_id_bytes(&b),
            base,
            "rule byte {i}"
        );
    }
}
