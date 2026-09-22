use tapstone_rules::{Applied, Game, HouseRules, Kind, Phase, Record, Refusal};

fn deck() -> [u16; 25] {
    [
        2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6,
    ]
}
fn lobby() -> Game {
    Game::new(HouseRules::default(), [0, 1], [&deck(), &deck()])
}
fn game() -> Game {
    lobby().started()
}
fn tap(seat: u8, kind: Kind, card: u16) -> Record {
    Record {
        seq: 0,
        seat,
        kind,
        card,
        lane: -1,
        target: 0,
        aux: 0,
        time_ms: 0,
        uid: [0; 7],
        auth: 0,
    }
}
fn hand(g: &Game, seat: usize) -> &[u16] {
    &g.seats[seat].hand[..g.seats[seat].hand_len()]
}

#[test]
fn claiming_two_seats_starts_the_game() {
    let mut g = lobby();
    assert_eq!(g.phase, Phase::Lobby);
    assert!(!g.seats[0].present && !g.seats[1].present);
    assert_eq!(
        g.apply(&tap(1, Kind::ClaimSeat, 1)),
        Ok(Applied::SeatClaimed { seat: 1 })
    );
    assert!(g.seats[1].present);
    assert_eq!(g.seats[1].castle_design, 1);
    assert_eq!(g.phase, Phase::Lobby);
    assert_eq!(g.seq, 1);
    assert_eq!(
        g.apply(&tap(1, Kind::ClaimSeat, 1)),
        Err(Refusal::SeatTaken)
    );
    assert_eq!(
        g.apply(&tap(0, Kind::ClaimSeat, 5)),
        Err(Refusal::BadTarget),
        "Flare is not a castle"
    );
    assert_eq!(
        g.apply(&tap(0, Kind::ClaimSeat, 99)),
        Err(Refusal::UnknownCard)
    );
    assert_eq!(
        g.apply(&tap(2, Kind::ClaimSeat, 0)),
        Err(Refusal::NotYourTurn),
        "no third seat"
    );
    assert_eq!(g.apply(&tap(0, Kind::ClaimSeat, 0)), Ok(Applied::Started));
    assert_eq!(g.phase, Phase::Playing);
    assert_eq!((g.round, g.active), (1, 0));
    assert_eq!((g.seats[0].hand_len(), g.seats[1].hand_len()), (5, 6));
    assert_eq!(g.seats[0].castle_design, 0);
    assert_eq!(g.seq, 2);
}

#[test]
fn lobby_refuses_play_events_and_playing_refuses_claims() {
    let mut g = lobby();
    assert_eq!(g.apply(&tap(0, Kind::Charge, 2)), Err(Refusal::NotPlaying));
    assert_eq!(g.apply(&tap(0, Kind::Pass, 0)), Err(Refusal::NotPlaying));
    assert_eq!(g.seq, 0);
    let mut g = game();
    assert_eq!(
        g.apply(&tap(0, Kind::ClaimSeat, 0)),
        Err(Refusal::NotPlaying)
    );
    assert_eq!(g.apply(&tap(0, Kind::Leave, 0)), Err(Refusal::NotPlaying));
}

#[test]
fn mulligan_redraws_from_the_remaining_deck_once_and_only_before_acting() {
    let mut g = game();
    assert_eq!(hand(&g, 0), &[2, 3, 4, 5, 6]);
    assert_eq!(
        g.apply(&tap(0, Kind::Mulligan, 0)),
        Ok(Applied::Mulliganed { drawn: 5 })
    );
    assert_eq!(hand(&g, 0), &[7, 8, 9, 10, 11]);
    assert_eq!(g.seats[0].deck_pos, 10);
    assert_eq!(
        g.apply(&tap(0, Kind::Mulligan, 0)),
        Err(Refusal::MulliganClosed),
        "once only"
    );
    g.apply(&tap(0, Kind::Pass, 0)).unwrap();
    assert_eq!(g.seats[1].hand_len(), 7);
    assert_eq!(
        g.apply(&tap(1, Kind::Mulligan, 0)),
        Ok(Applied::Mulliganed { drawn: 7 })
    );
    assert_eq!(
        hand(&g, 1),
        &[9, 10, 11, 2, 3, 4, 5],
        "the next seven deck cards"
    );
    assert_eq!(g.seats[1].deck_pos, 14);

    let mut g = game();
    g.apply(&tap(0, Kind::Charge, 6)).unwrap();
    assert_eq!(
        g.apply(&tap(0, Kind::Mulligan, 0)),
        Err(Refusal::MulliganClosed),
        "acted already"
    );

    let mut g = game();
    assert_eq!(
        g.apply(&tap(1, Kind::Mulligan, 0)),
        Err(Refusal::NotYourTurn)
    );
}

#[test]
fn with_rules_applies_in_lobby_only() {
    let mut g = lobby();
    let hr = HouseRules {
        castle_life: 25,
        deck_size: 20,
        ..HouseRules::default()
    };
    assert_eq!(g.with_rules(hr), Ok(()));
    assert_eq!(g.rules, hr);
    assert_eq!((g.seats[0].castle.life, g.seats[1].castle.life), (25, 25));
    assert_eq!((g.seats[0].deck_len, g.seats[1].deck_len), (20, 20));
    let mut g = g.started();
    assert_eq!(g.seats[0].hand_len(), 5);
    let before = g;
    assert_eq!(
        g.with_rules(HouseRules::default()),
        Err(Refusal::LobbyClosed)
    );
    assert_eq!(g, before);
}

#[test]
fn started_is_a_no_op_outside_the_lobby() {
    let g = game();
    let again = g.started();
    assert_eq!(again, g);
}
