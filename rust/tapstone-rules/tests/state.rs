use tapstone_rules::state::Seat;
use tapstone_rules::{Game, HouseRules, Phase};

#[test]
fn new_game_deals_hands_from_seeded_decks() {
    let hr = HouseRules::default();
    let deck = [
        2u16, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6,
    ];
    let g = Game::new(hr, [0, 1], [&deck, &deck]);
    assert_eq!(g.phase, Phase::Lobby);
    let g = g.started();
    assert_eq!(g.seats[0].hand_len(), 5);
    // 6: decision 0035 restored second_player_bonus to 1, because the commander (0029) moved the
    // seat edge to seat 0. Seat 1 also draws before its first turn, so it opens two cards ahead.
    assert_eq!(
        g.seats[1].hand_len(),
        6,
        "one second-player bonus card since 0035"
    );
    assert_eq!(g.seats[0].castle.life, 20);
    assert_eq!(g.round, 1);
    assert_eq!(g.active, 0);
}

#[test]
fn house_rules_default_matches_decision_0035_and_147() {
    let hr = HouseRules::default();
    assert_eq!(
        (
            hr.deck_size,
            hr.hand,
            hr.second_player_bonus,
            hr.castle_life,
            hr.pressure_from,
            hr.pressure,
            hr.stop_round
        ),
        // deck_size 30 since #147 (2026-09-27): ten designs at three copies; the rest is 0035's.
        (30, 5, 1, 20, 8, 2, 12)
    );
}

#[test]
fn an_exhausted_list_owes_nothing_beyond_its_copies() {
    // 0036: owed draws never exceed the undrawn copies, the old "a drawn-out deck draws nothing".
    let mut s = Seat::empty();
    s.deck[0] = 5;
    s.deck_len = 1;
    s.owe(3);
    assert_eq!(s.owed_draws(), 1);
    assert!(s.undrawn(5) && !s.undrawn(6));
}

#[test]
fn remove_from_hand_removes_first_match_and_keeps_order() {
    let mut s = Seat::empty();
    s.hand[..4].copy_from_slice(&[3, 7, 3, 9]);
    s.hand_len = 4;
    assert!(s.remove_from_hand(3));
    assert_eq!(&s.hand[..s.hand_len()], &[7, 3, 9]);
    assert!(!s.remove_from_hand(42));
    assert_eq!(&s.hand[..s.hand_len()], &[7, 3, 9]);
}

#[test]
fn deck_len_is_clamped_by_deck_size_and_deck_max() {
    let thirty: [u16; 30] = core::array::from_fn(|i| (i % 12 + 2) as u16);
    let thirty_one: [u16; 31] = core::array::from_fn(|i| (i % 12 + 2) as u16);
    let small = HouseRules {
        deck_size: 25,
        ..HouseRules::default()
    };
    let g = Game::new(small, [0, 1], [&thirty, &thirty]);
    assert_eq!(
        g.seats[0].deck_len, 25,
        "deck_size 25 clamps a 30-card deck"
    );
    let g = Game::new(HouseRules::default(), [0, 1], [&thirty, &thirty]);
    assert_eq!(g.seats[0].deck_len, 30, "the default holds all 30 (#147)");
    let g = Game::new(HouseRules::default(), [0, 1], [&thirty_one, &thirty_one]);
    assert_eq!(g.seats[1].deck_len, 30, "DECK_MAX clamps a 31-card deck");
}

#[test]
fn house_rules_round_trip_through_their_hashed_bytes() {
    let r = tapstone_rules::HouseRules {
        castle_life: 17,
        commander_return: 4,
        ..Default::default()
    };
    assert_eq!(tapstone_rules::HouseRules::from_bytes(r.bytes()), r);
    let mut b = r.bytes();
    b[3] = 99;
    assert_eq!(
        tapstone_rules::HouseRules::from_bytes(b).castle_life,
        99,
        "byte 3 is castle_life"
    );
}
