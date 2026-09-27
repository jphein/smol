//! Decision 0036: every draw is a tap; the engine knows a deck's list, not its order.
use tapstone_rules::hash::{CANON, Chain, canonical};
use tapstone_rules::state::HAND_MAX;
use tapstone_rules::{Applied, Game, HouseRules, Kind, Phase, Record, Refusal};

fn deck() -> [u16; 25] {
    [
        2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6,
    ]
}
fn rec(seat: u8, kind: Kind, card: u16) -> Record {
    Record {
        seq: 0,
        seat,
        kind,
        card,
        lane: -1,
        // A claim carries the commander's final stats (0029): target = attack, aux = toughness.
        target: if kind == Kind::ClaimSeat { 2 } else { 0 },
        aux: if kind == Kind::ClaimSeat { 4 } else { 0 },
        time_ms: 0,
        uid: [0; 7],
        auth: 0,
    }
}
/// The real start: two claims, no shortcut. Both seats then owe their opening hands.
fn claimed() -> Game {
    let mut g = Game::new(HouseRules::default(), [0, 1], [&deck(), &deck()]);
    g.apply(&rec(1, Kind::ClaimSeat, 1)).unwrap();
    assert_eq!(g.apply(&rec(0, Kind::ClaimSeat, 0)), Ok(Applied::Started));
    g
}
fn draw(g: &mut Game, seat: u8, card: u16) -> Result<Applied, Refusal> {
    g.apply(&rec(seat, Kind::Draw, card))
}
/// Pay every draw `seat` owes, taking the first undrawn copy in list order.
fn pay(g: &mut Game, seat: u8) {
    while g.seats[seat as usize].owed_draws() > 0 {
        let c = g
            .top_of_list(seat)
            .expect("a seat that owes has an undrawn copy");
        draw(g, seat, c).unwrap();
    }
}

#[test]
fn the_game_starts_with_empty_hands_and_opening_draws_owed() {
    let g = claimed();
    assert_eq!(g.phase, Phase::Playing);
    assert_eq!((g.seats[0].hand_len(), g.seats[1].hand_len()), (0, 0));
    let r = HouseRules::default();
    assert_eq!(
        g.seats[0].owed_draws(),
        r.hand,
        "seat 0 owes its opening hand"
    );
    assert_eq!(
        g.seats[1].owed_draws(),
        r.hand + r.second_player_bonus,
        "seat 1 also owes the 0035 bonus card"
    );
}

#[test]
fn a_draw_moves_one_copy_from_the_list_to_the_hand_and_pays_one() {
    let mut g = claimed();
    let before = g.seats[0].deck_len;
    assert_eq!(draw(&mut g, 0, 7), Ok(Applied::Drew { owed: 4 }));
    assert_eq!(&g.seats[0].hand[..1], &[7]);
    assert_eq!(g.seats[0].deck_len, before - 1);
    assert_eq!(g.seats[0].owed_draws(), 4);
    assert_eq!(
        g.seats[0].deck[..g.seats[0].deck_len as usize]
            .iter()
            .filter(|&&c| c == 7)
            .count(),
        1,
        "the list held two Tidecallers; one is left"
    );
}

#[test]
fn only_an_undrawn_copy_can_be_drawn() {
    let mut g = claimed();
    assert_eq!(
        draw(&mut g, 0, 12),
        Err(Refusal::NotInDeck),
        "Mend is not in this list"
    );
    draw(&mut g, 0, 11).unwrap();
    draw(&mut g, 0, 11).unwrap();
    assert_eq!(
        draw(&mut g, 0, 11),
        Err(Refusal::NotInDeck),
        "both Deep Breaths are drawn"
    );
    assert_eq!(g.seats[0].owed_draws(), 3, "a refused draw pays nothing");
}

#[test]
fn a_seat_that_owes_draws_can_do_nothing_else() {
    let mut g = claimed();
    for kind in [Kind::Pass, Kind::Charge, Kind::Mulligan, Kind::Advance] {
        assert_eq!(
            g.apply(&rec(0, kind, 2)),
            Err(Refusal::DrawOwed),
            "{kind:?}"
        );
    }
    pay(&mut g, 0);
    assert!(
        g.apply(&rec(0, Kind::Pass, 0)).is_ok(),
        "paid up, seat 0 may act"
    );
}

#[test]
fn both_seats_draw_their_opening_hands_at_once() {
    let mut g = claimed();
    assert_eq!(g.active, 0);
    // Seat 1 draws during seat 0's turn: only opening draws can be owed off-turn.
    assert!(draw(&mut g, 1, 3).is_ok());
    pay(&mut g, 1);
    assert_eq!(g.seats[1].hand_len(), 6);
    assert_eq!(
        g.apply(&rec(1, Kind::Pass, 0)),
        Err(Refusal::NotYourTurn),
        "drawing off-turn does not make it seat 1's turn"
    );
    assert_eq!(
        draw(&mut g, 1, 3),
        Err(Refusal::NoDrawOwed),
        "nothing more is owed"
    );
}

#[test]
fn each_turn_start_owes_one_draw() {
    let mut g = claimed();
    pay(&mut g, 0);
    pay(&mut g, 1);
    g.apply(&rec(0, Kind::Pass, 0)).unwrap();
    assert_eq!(
        (g.active, g.seats[1].owed_draws()),
        (1, 1),
        "seat 1's turn starts owing one"
    );
    assert_eq!(g.apply(&rec(1, Kind::Pass, 0)), Err(Refusal::DrawOwed));
    pay(&mut g, 1);
    assert_eq!(g.seats[1].hand_len(), 7);
    g.apply(&rec(1, Kind::Pass, 0)).unwrap();
    assert_eq!((g.round, g.active, g.seats[0].owed_draws()), (2, 0, 1));
}

#[test]
fn a_draw_spell_owes_its_count() {
    let mut g = claimed();
    // Put Deep Breath (draw 2, cost 1) in seat 0's hand by drawing it, and give seat 0 the mana.
    draw(&mut g, 0, 11).unwrap();
    pay(&mut g, 0);
    pay(&mut g, 1);
    g.seats[0].charged = 1;
    assert_eq!(g.apply(&rec(0, Kind::CastSpell, 11)), Ok(Applied::Spell));
    assert_eq!(g.seats[0].owed_draws(), 2);
    assert_eq!(g.apply(&rec(0, Kind::Pass, 0)), Err(Refusal::DrawOwed));
}

#[test]
fn mulligan_returns_the_hand_to_the_list_and_owes_it_again() {
    let mut g = claimed();
    pay(&mut g, 0);
    pay(&mut g, 1);
    let list_before = g.seats[0].deck_len;
    let hand: Vec<u16> = g.seats[0].hand[..5].to_vec();
    assert_eq!(
        g.apply(&rec(0, Kind::Mulligan, 0)),
        Ok(Applied::Mulliganed { returned: 5 })
    );
    assert_eq!(g.seats[0].hand_len(), 0);
    assert_eq!(
        g.seats[0].deck_len,
        list_before + 5,
        "the hand went back into the list"
    );
    for c in hand {
        assert!(g.seats[0].deck[..g.seats[0].deck_len as usize].contains(&c));
    }
    assert_eq!(
        g.seats[0].owed_draws(),
        5,
        "a fresh hand of the same size is owed"
    );
    assert!(g.seats[0].mulliganed());
    pay(&mut g, 0);
    assert_eq!(
        g.apply(&rec(0, Kind::Mulligan, 0)),
        Err(Refusal::MulliganClosed),
        "once only"
    );
}

#[test]
fn drawing_does_not_close_the_mulligan() {
    let mut g = claimed();
    pay(&mut g, 0);
    assert!(
        !g.seats[0].acted(),
        "draw taps are not actions for the mulligan rule"
    );
}

#[test]
fn an_exhausted_list_owes_nothing_and_a_full_hand_owes_no_more() {
    let short = [2u16, 3];
    let mut g = Game::new(HouseRules::default(), [0, 1], [&short, &deck()]);
    g.apply(&rec(1, Kind::ClaimSeat, 1)).unwrap();
    g.apply(&rec(0, Kind::ClaimSeat, 0)).unwrap();
    assert_eq!(
        g.seats[0].owed_draws(),
        2,
        "a two-card list owes two, not five"
    );
    pay(&mut g, 0);
    assert_eq!((g.seats[0].hand_len(), g.seats[0].owed_draws()), (2, 0));
    // A full hand: a draw with no free slot is dropped, as the old digital draw dropped it.
    let mut g = claimed();
    pay(&mut g, 0);
    pay(&mut g, 1);
    g.seats[1].hand_len = HAND_MAX as u8;
    g.apply(&rec(0, Kind::Pass, 0)).unwrap();
    assert_eq!(
        (g.active, g.seats[1].owed_draws()),
        (1, 0),
        "a full hand owes no turn draw"
    );
    assert!(
        g.apply(&rec(1, Kind::Pass, 0)).is_ok(),
        "and nothing blocks the seat"
    );
}

#[test]
fn genesis_hashes_the_list_and_not_its_order() {
    let mut rev = deck();
    rev.reverse();
    let start = |d0: &[u16]| {
        let mut g = Game::new(HouseRules::default(), [0, 1], [d0, &deck()]);
        g.apply(&rec(1, Kind::ClaimSeat, 1)).unwrap();
        g.apply(&rec(0, Kind::ClaimSeat, 0)).unwrap();
        Chain::genesis(&g).head()
    };
    assert_eq!(
        start(&deck()),
        start(&rev),
        "shuffling the paper must not move genesis"
    );
    let mut other = deck();
    other[0] = 13;
    assert_ne!(
        start(&deck()),
        start(&other),
        "a different list is a different game"
    );
}

#[test]
fn the_image_carries_the_owed_draws() {
    let g = claimed();
    let image = |g: &Game| {
        let mut out = [0u8; CANON];
        canonical(g, &mut out);
        out
    };
    let mut h = g;
    h.seats[1].owed -= 1;
    assert_ne!(
        image(&g),
        image(&h),
        "owed draws change what the engine accepts next"
    );
}

#[test]
fn draw_is_kind_nine_on_the_wire() {
    assert_eq!(Kind::Draw as u8, 9);
    assert_eq!(Kind::from_u8(9), Some(Kind::Draw));
    assert_eq!(Kind::from_u8(10), None);
}

#[test]
fn the_slot_a_draw_spell_frees_counts_toward_its_draws() {
    // A full hand of ten, one of them Deep Breath (draw 2). Casting it frees one slot, so one draw
    // is owed; had the draws been owed before the spell left the hand, there would be no room and
    // none would be owed.
    let mut g = claimed();
    pay(&mut g, 0);
    pay(&mut g, 1);
    g.seats[0].hand_len = HAND_MAX as u8;
    g.seats[0].hand[0] = 11;
    g.seats[0].charged = 1;
    assert_eq!(g.apply(&rec(0, Kind::CastSpell, 11)), Ok(Applied::Spell));
    assert_eq!(g.seats[0].hand_len(), HAND_MAX - 1);
    assert_eq!(
        g.seats[0].owed_draws(),
        1,
        "the freed slot takes one of the two draws"
    );
}

/// The protocol draft quotes the new kind's wire code; compare it with the code (verification.md's
/// document-quoting rule).
#[test]
fn the_protocol_draft_names_draw_by_its_code() {
    let p = format!(
        "{}/../../docs/protocol/tapstone-protocol-draft.md",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc = std::fs::read_to_string(&p).unwrap();
    // The kind list itself, not any later mention: a loose "9 Draw" also matched the prose after it
    // and let this test pass with the list wrong (caught by perturbing it).
    let want = format!("{} Leave · {} Draw;", Kind::Leave as u8, Kind::Draw as u8);
    assert!(doc.contains(&want), "protocol draft lacks {want:?}");
}
