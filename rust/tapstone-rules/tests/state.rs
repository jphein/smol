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
    assert_eq!(g.seats[1].hand_len(), 6, "second player bonus");
    assert_eq!(g.seats[0].castle.life, 20);
    assert_eq!(g.round, 1);
    assert_eq!(g.active, 0);
}

#[test]
fn house_rules_default_matches_decision_0011() {
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
        (25, 5, 1, 20, 8, 2, 12)
    );
}

#[test]
fn draw_on_exhausted_deck_returns_false_and_leaves_hand_unchanged() {
    let mut s = Seat::empty();
    s.deck[0] = 5;
    s.deck_len = 1;
    assert!(s.draw());
    assert_eq!(s.hand_len(), 1);
    let before = s;
    assert!(!s.draw());
    assert_eq!(s, before);
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
    let g = Game::new(HouseRules::default(), [0, 1], [&thirty, &thirty]);
    assert_eq!(
        g.seats[0].deck_len, 25,
        "default deck_size clamps a 30-card deck"
    );
    let big = HouseRules {
        deck_size: 30,
        ..HouseRules::default()
    };
    let g = Game::new(big, [0, 1], [&thirty, &thirty]);
    assert_eq!(g.seats[0].deck_len, 30);
    let g = Game::new(big, [0, 1], [&thirty_one, &thirty_one]);
    assert_eq!(g.seats[1].deck_len, 30, "DECK_MAX clamps a 31-card deck");
}
