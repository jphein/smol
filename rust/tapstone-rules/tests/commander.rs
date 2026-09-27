//! Decision 0029: each seat fields a commander that fights on the board.
use tapstone_rules::hash::{CANON, Chain, canonical};
use tapstone_rules::state::{COMMANDER_DESIGN, Commander, Unit};
use tapstone_rules::{Applied, Game, HouseRules, Keyword, Kind, Phase, Record, Refusal, Winner};

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
fn rec(seat: u8, kind: Kind, card: u16, lane: i8, target: u8, aux: u8) -> Record {
    Record {
        seq: 0,
        seat,
        kind,
        card,
        lane,
        target,
        aux,
        time_ms: 0,
        uid: [0; 7],
        auth: 0,
    }
}
/// Pass, then pay the draw the next seat owes at its turn start (0036), from the top of its list.
fn pass(g: &mut Game) -> Applied {
    let out = g.apply(&rec(g.active, Kind::Pass, 0, -1, 0, 0)).unwrap();
    let a = g.active;
    while g.phase == Phase::Playing && g.seats[a as usize].owed_draws() > 0 {
        let c = g.top_of_list(a).unwrap();
        g.apply(&rec(a, Kind::Draw, c, -1, 0, 0)).unwrap();
    }
    out
}
fn target(seat: u8, lane: u8, cell: u8) -> u8 {
    (seat << 4) | (lane << 2) | cell
}
/// Give the active seat a Flare (1 mana, 2 damage) and the mana to cast it.
fn arm_flare(g: &mut Game) {
    let s = &mut g.seats[g.active as usize];
    s.hand[0] = 5;
    s.hand_len = s.hand_len.max(1);
    s.charged = s.spent + 1;
}
fn commander_at(g: &Game, seat: usize, lane: usize, cell: usize) -> Option<Unit> {
    g.seats[seat].cells[lane][cell].filter(|u| u.design == COMMANDER_DESIGN)
}

#[test]
fn genesis_places_a_level_one_commander_in_the_middle_back_cell() {
    let g = game();
    for s in 0..2 {
        assert_eq!(
            g.seats[s].cells[1][0],
            Some(Unit {
                design: COMMANDER_DESIGN,
                attack: Commander::LEVEL_1.attack,
                toughness: Commander::LEVEL_1.toughness,
                damage: 0,
                keyword: None,
                entered_round: 0,
            }),
            "seat {s}"
        );
        assert_eq!(g.seats[s].units(), 1, "the commander is a unit");
    }
    assert_eq!(
        (Commander::LEVEL_1.attack, Commander::LEVEL_1.toughness),
        (2, 4),
        "0029's level-1 base"
    );
}

#[test]
fn the_reserved_commander_design_is_no_card() {
    assert!(tapstone_rules::cards::design(COMMANDER_DESIGN).is_none());
    assert_ne!(COMMANDER_DESIGN, 0xFFFF, "0xFFFF is the empty-cell marker");
}

#[test]
fn claim_seat_carries_the_commanders_final_stats() {
    let mut g = lobby();
    // target = attack, aux = toughness, lane = keyword code (-1 none).
    let taunt = Keyword::Taunt.code() as i8;
    g.apply(&rec(1, Kind::ClaimSeat, 1, taunt, 3, 7)).unwrap();
    assert_eq!(
        g.apply(&rec(0, Kind::ClaimSeat, 0, -1, 2, 4)),
        Ok(Applied::Started)
    );
    let u = commander_at(&g, 1, 1, 0).expect("seat 1 commander on board");
    assert_eq!(
        (u.attack, u.toughness, u.keyword),
        (3, 7, Some(Keyword::Taunt))
    );
    let u = commander_at(&g, 0, 1, 0).expect("seat 0 commander on board");
    assert_eq!((u.attack, u.toughness, u.keyword), (2, 4, None));
}

#[test]
fn a_claim_with_no_toughness_or_an_unknown_keyword_is_refused_and_changes_nothing() {
    let mut g = lobby();
    let before = g;
    assert_eq!(
        g.apply(&rec(0, Kind::ClaimSeat, 0, -1, 2, 0)),
        Err(Refusal::BadTarget),
        "a commander with toughness 0 would die at genesis"
    );
    assert_eq!(
        g.apply(&rec(0, Kind::ClaimSeat, 0, 5, 2, 4)),
        Err(Refusal::BadTarget),
        "keyword code 5 is past the closed list"
    );
    assert_eq!(g, before);
}

#[test]
fn keyword_codes_round_trip_and_stop_at_the_closed_list() {
    for k in [
        Keyword::Ranged,
        Keyword::Shield1,
        Keyword::Haste,
        Keyword::Rush,
        Keyword::Taunt,
    ] {
        assert_eq!(Keyword::from_code(k.code()), Some(k));
    }
    assert_eq!(Keyword::from_code(5), None);
}

#[test]
fn set_commander_is_lobby_only() {
    let mut g = lobby();
    g.set_commander(0, Commander::stats(4, 6, Some(Keyword::Haste)))
        .unwrap();
    let mut g = g.started();
    let u = commander_at(&g, 0, 1, 0).unwrap();
    assert_eq!(
        (u.attack, u.toughness, u.keyword),
        (4, 6, Some(Keyword::Haste))
    );
    assert_eq!(
        g.set_commander(0, Commander::LEVEL_1),
        Err(Refusal::LobbyClosed)
    );
    assert_eq!(
        lobby().set_commander(0, Commander::stats(1, 0, None)),
        Err(Refusal::BadTarget)
    );
}

#[test]
fn a_commander_blocks_its_cell_like_any_unit() {
    let mut g = game();
    let s = &mut g.seats[0];
    s.hand[0] = 2; // Cinder Whelp, cost 1
    s.charged = 1;
    assert_eq!(
        g.apply(&rec(0, Kind::CastUnit, 2, 1, 0, 0)),
        Err(Refusal::CellOccupied)
    );
}

#[test]
fn a_killed_commander_costs_its_castle_and_leaves_the_board() {
    let mut g = game();
    g.seats[1].cells[1][0].as_mut().unwrap().damage = 2;
    arm_flare(&mut g);
    assert_eq!(
        g.apply(&rec(0, Kind::CastSpell, 5, -1, target(1, 1, 0), 0)),
        Ok(Applied::Spell)
    );
    assert_eq!(g.seats[1].cells[1][0], None);
    let fall = HouseRules::default().commander_fall;
    assert_eq!(fall, 3, "0029's v0 number");
    assert_eq!(g.seats[1].castle.life, 20 - fall);
    assert_eq!(
        g.seats[0].castle.life, 20,
        "the killer's castle is untouched"
    );
    assert_eq!(g.seats[1].units(), 0, "a dead commander is not a unit");
    let c = g.seats[1].commander;
    assert_eq!(c.returns, 1 + HouseRules::default().commander_return);
    assert_eq!(c.lane, 1);
}

#[test]
fn it_returns_at_its_owners_turn_start_two_rounds_later_at_full_toughness() {
    let mut g = game();
    g.seats[1].cells[1][0].as_mut().unwrap().damage = 2;
    arm_flare(&mut g);
    g.apply(&rec(0, Kind::CastSpell, 5, -1, target(1, 1, 0), 0))
        .unwrap(); // dies in round 1
    assert_eq!(HouseRules::default().commander_return, 2);
    pass(&mut g); // seat 1, round 1
    assert_eq!((g.round, g.active), (1, 1));
    assert!(commander_at(&g, 1, 1, 0).is_none());
    pass(&mut g);
    pass(&mut g); // seat 1, round 2
    assert_eq!((g.round, g.active), (2, 1));
    assert!(
        commander_at(&g, 1, 1, 0).is_none(),
        "one round is too early"
    );
    pass(&mut g);
    assert_eq!((g.round, g.active), (3, 0));
    assert!(
        commander_at(&g, 1, 1, 0).is_none(),
        "not at the opponent's turn start"
    );
    pass(&mut g); // seat 1, round 3
    assert_eq!((g.round, g.active), (3, 1));
    let u = commander_at(&g, 1, 1, 0).expect("back at round 3");
    assert_eq!((u.damage, u.toughness, u.entered_round), (0, 4, 3));
    assert_eq!(g.seats[1].commander.returns, 0, "no longer down");
}

#[test]
fn it_returns_to_the_lane_it_died_in() {
    let mut g = game();
    // Move seat 1's commander to lane 2 by hand, then kill it there.
    let u = g.seats[1].cells[1][0].take();
    g.seats[1].cells[2][0] = u;
    g.seats[1].cells[2][0].as_mut().unwrap().damage = 2;
    arm_flare(&mut g);
    g.apply(&rec(0, Kind::CastSpell, 5, -1, target(1, 2, 0), 0))
        .unwrap();
    assert_eq!(g.seats[1].commander.lane, 2);
    for _ in 0..5 {
        pass(&mut g);
    }
    assert_eq!((g.round, g.active), (3, 1));
    assert!(commander_at(&g, 1, 2, 0).is_some());
    assert!(commander_at(&g, 1, 1, 0).is_none());
}

#[test]
fn it_waits_while_its_back_cell_is_occupied() {
    let mut g = game();
    g.seats[1].cells[1][0].as_mut().unwrap().damage = 2;
    arm_flare(&mut g);
    g.apply(&rec(0, Kind::CastSpell, 5, -1, target(1, 1, 0), 0))
        .unwrap();
    let squatter = Unit {
        design: 7,
        attack: 2,
        toughness: 3,
        damage: 0,
        keyword: None,
        entered_round: 1,
    };
    g.seats[1].cells[1][0] = Some(squatter);
    for _ in 0..5 {
        pass(&mut g);
    }
    assert_eq!((g.round, g.active), (3, 1));
    assert_eq!(g.seats[1].cells[1][0], Some(squatter), "still waiting");
    assert_ne!(g.seats[1].commander.returns, 0);
    g.seats[1].cells[1][0] = None;
    pass(&mut g);
    pass(&mut g); // seat 1, round 4
    assert_eq!((g.round, g.active), (4, 1));
    assert!(commander_at(&g, 1, 1, 0).is_some(), "the cell came free");
}

#[test]
fn commanders_that_die_in_combat_cost_their_castles_too() {
    let mut g = game();
    for s in 0..2 {
        let mut u = g.seats[s].cells[1][0].take().unwrap();
        u.damage = 2;
        g.seats[s].cells[1][2] = Some(u); // front cells face each other
    }
    assert_eq!(
        pass(&mut g),
        Applied::TurnEnded {
            combat_damage: [0, 0]
        }
    );
    for s in 0..2 {
        assert_eq!(g.seats[s].cells[1][2], None, "seat {s}");
        assert_eq!(g.seats[s].castle.life, 17, "seat {s}");
        assert_eq!(g.seats[s].commander.returns, 3, "seat {s}");
    }
}

#[test]
fn a_destroy_spell_is_a_death_too() {
    let mut g = lobby();
    g.set_commander(1, Commander::stats(1, 2, None)).unwrap();
    let mut g = g.started();
    let s = &mut g.seats[0];
    s.hand[0] = 13; // Riptide: 3 mana, destroy toughness <= 2
    s.charged = 3;
    g.apply(&rec(0, Kind::CastSpell, 13, -1, target(1, 1, 0), 0))
        .unwrap();
    assert_eq!(g.seats[1].cells[1][0], None);
    assert_eq!(g.seats[1].castle.life, 17);
    assert_ne!(g.seats[1].commander.returns, 0);
}

#[test]
fn losing_the_commander_can_lose_the_game() {
    let mut g = game();
    g.seats[1].castle.life = 3;
    g.seats[1].cells[1][0].as_mut().unwrap().damage = 2;
    arm_flare(&mut g);
    assert_eq!(
        g.apply(&rec(0, Kind::CastSpell, 5, -1, target(1, 1, 0), 0)),
        Ok(Applied::GameEnded(Winner::Seat(0)))
    );
    assert_eq!(g.phase, Phase::Over);
}

#[test]
fn the_commander_counts_for_the_units_tiebreak() {
    let mut g = game();
    // Seat 1's commander is down; both castles equal; seat 1 completes the stop round.
    g.seats[1].cells[1][0] = None;
    g.seats[1].commander.returns = 99;
    g.round = g.rules.stop_round;
    g.active = 1;
    assert_eq!(pass(&mut g), Applied::GameEnded(Winner::Seat(0)));
}

#[test]
fn the_image_covers_every_commander_byte() {
    let base = game();
    let image = |g: &Game| {
        let mut out = [0u8; CANON];
        canonical(g, &mut out);
        out
    };
    let a = image(&base);
    let perturbs: [fn(&mut Commander); 5] = [
        |c| c.attack += 1,
        |c| c.toughness += 1,
        |c| c.keyword = Some(Keyword::Haste),
        |c| c.returns = 7,
        |c| c.lane = 2,
    ];
    for (i, f) in perturbs.iter().enumerate() {
        for s in 0..2 {
            let mut g = base;
            f(&mut g.seats[s].commander);
            assert_ne!(
                image(&g),
                a,
                "commander field {i} of seat {s} is not hashed"
            );
        }
    }
}

#[test]
fn genesis_hashes_the_commanders_and_the_new_house_rules() {
    let base = Chain::genesis(&game()).head();
    for s in 0..2u8 {
        for c in [
            Commander::stats(3, 4, None),
            Commander::stats(2, 5, None),
            Commander::stats(2, 4, Some(Keyword::Taunt)),
        ] {
            let mut g = lobby();
            g.set_commander(s, c).unwrap();
            assert_ne!(Chain::genesis(&g.started()).head(), base, "seat {s} {c:?}");
        }
    }
    for rules in [
        HouseRules {
            commander_fall: 4,
            ..HouseRules::default()
        },
        HouseRules {
            commander_return: 3,
            ..HouseRules::default()
        },
    ] {
        let g = Game::new(rules, [0, 1], [&deck(), &deck()]).started();
        assert_ne!(Chain::genesis(&g).head(), base, "{rules:?}");
    }
}

#[test]
fn a_rush_commander_is_refused_because_rush_would_be_inert() {
    // 0029/0031 rulings: a commander always enters the back cell, so Rush does nothing on it and
    // is excluded rather than silently accepted.
    let mut g = lobby();
    let rush = Keyword::Rush.code() as i8;
    assert_eq!(
        g.apply(&rec(0, Kind::ClaimSeat, 0, rush, 2, 4)),
        Err(Refusal::BadTarget)
    );
    assert_eq!(
        g.set_commander(0, Commander::stats(2, 4, Some(Keyword::Rush))),
        Err(Refusal::BadTarget)
    );
    assert!(!g.seats[0].present());
}

#[test]
fn only_haste_and_taunt_may_ride_on_a_commander() {
    // 0034: Ranged (~100% in every opening) and Shield 1 (68–82%) never go on a commander; Rush is
    // inert on one (0029). Each is refused at ClaimSeat and in set_commander; Haste and Taunt pass.
    use tapstone_rules::state::COMMANDER_KEYWORDS;
    assert_eq!(COMMANDER_KEYWORDS, [Keyword::Haste, Keyword::Taunt]);
    for code in 0..5u8 {
        let k = Keyword::from_code(code).unwrap();
        let allowed = matches!(k, Keyword::Haste | Keyword::Taunt);
        let mut g = lobby();
        let want = if allowed {
            Ok(Applied::SeatClaimed { seat: 0 })
        } else {
            Err(Refusal::BadTarget)
        };
        assert_eq!(
            g.apply(&rec(0, Kind::ClaimSeat, 0, code as i8, 2, 4)),
            want,
            "{k:?} at ClaimSeat"
        );
        assert_eq!(
            g.seats[0].present(),
            allowed,
            "{k:?}: a refused claim seats nobody"
        );
        let want = if allowed {
            Ok(())
        } else {
            Err(Refusal::BadTarget)
        };
        assert_eq!(
            lobby().set_commander(1, Commander::stats(2, 4, Some(k))),
            want,
            "{k:?} in set_commander"
        );
    }
    assert_eq!(
        lobby().set_commander(1, Commander::stats(2, 4, None)),
        Ok(()),
        "no keyword is fine"
    );
}

/// Two commanders facing each other at the front, each one hit from death.
fn front_duel() -> Game {
    let mut g = game();
    for s in 0..2 {
        let mut u = g.seats[s].cells[1][0].take().unwrap();
        u.damage = 2;
        g.seats[s].cells[1][2] = Some(u);
    }
    g
}

#[test]
fn a_commander_death_in_combat_can_be_the_lethal_blow() {
    let mut g = front_duel();
    g.seats[1].castle.life = 3; // only the fall penalty can finish it
    g.seats[0].castle.life = 10;
    assert_eq!(pass(&mut g), Applied::GameEnded(Winner::Seat(0)));
    assert_eq!(g.seats[1].castle.life, 0);
}

#[test]
fn simultaneous_zeros_from_the_fall_resolve_as_rules_v0_does() {
    // Both commanders trade and both castles fall to 0 in the same combat: equal life, equal
    // units (none), so v0's last tiebreak gives it to seat 1.
    let mut g = front_duel();
    g.seats[0].castle.life = 3;
    g.seats[1].castle.life = 3;
    assert_eq!(pass(&mut g), Applied::GameEnded(Winner::Seat(1)));
    assert_eq!((g.seats[0].castle.life, g.seats[1].castle.life), (0, 0));
}

/// The protocol draft and 0022 quote the commander's wire codes and byte counts. A hand-typed
/// number in a document is the class docs/verification.md warns about, so compare them to code.
#[test]
fn the_documents_quote_the_codes_and_sizes_the_code_uses() {
    let read = |rel: &str| {
        let p = format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
    };
    let proto = read("docs/protocol/tapstone-protocol-draft.md");
    let want = format!(
        "keyword code ({} Haste · {} Taunt; {} Ranged, {} Shield 1 and {} Rush are refused, 0034)",
        Keyword::Haste.code(),
        Keyword::Taunt.code(),
        Keyword::Ranged.code(),
        Keyword::Shield1.code(),
        Keyword::Rush.code()
    );
    assert!(proto.contains(&want), "protocol draft lacks {want:?}");
    assert!(
        proto.contains("deck:hand:bonus:life:from:pressure:stop:fall:return:v"),
        "CFG M must list the rule bytes in HouseRules::bytes() order"
    );
    assert_eq!(HouseRules::default().bytes().len(), 9);
    let d0022 = read("docs/decisions/0022-canonical-state-and-genesis.md");
    let want = format!("The image is now **{CANON} bytes**");
    assert!(d0022.contains(&want), "0022 lacks {want:?}");
}
