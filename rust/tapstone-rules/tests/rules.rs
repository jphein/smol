use tapstone_rules::{Game, HouseRules, Kind, Phase, Record, Refusal, Winner};

fn deck() -> [u16; 25] {
    [
        2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6,
    ]
}
fn game() -> Game {
    Game::new(HouseRules::default(), [0, 1], [&deck(), &deck()]).started()
}
fn tap(seat: u8, kind: Kind, card: u16, lane: i8, target: u8) -> Record {
    Record {
        seq: 0,
        seat,
        kind,
        card,
        lane,
        target,
        aux: 0,
        time_ms: 0,
        uid: [0; 7],
        auth: 0,
    }
}
fn pass_round(g: &mut Game) {
    g.apply(&tap(g.active, Kind::Pass, 0, -1, 0)).unwrap();
    g.apply(&tap(g.active, Kind::Pass, 0, -1, 0)).unwrap();
}
/// Charge the last card in hand (never the Vanguard at hand[1]).
fn charge_last(g: &mut Game) {
    let s = &g.seats[g.active as usize];
    let card = s.hand[s.hand_len() - 1];
    g.apply(&tap(g.active, Kind::Charge, card, -1, 0)).unwrap();
}

#[test]
fn charge_gives_permanent_mana_once_per_round() {
    let mut g = game();
    assert!(g.apply(&tap(0, Kind::Charge, 2, -1, 0)).is_ok());
    assert_eq!(g.seats[0].charged, 1);
    assert_eq!(g.seats[0].hand_len(), 4);
    assert_eq!(
        g.apply(&tap(0, Kind::Charge, 3, -1, 0)),
        Err(Refusal::AlreadyChargedThisRound)
    );
    assert_eq!(g.seq, 1, "seq advances only on accepted events");
}

#[test]
fn casting_needs_mana_and_the_card_in_hand() {
    let mut g = game();
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 2, 0, 0)),
        Err(Refusal::NoMana { need: 1, have: 0 })
    );
    g.apply(&tap(0, Kind::Charge, 6, -1, 0)).unwrap(); // hand [2,3,4,5]
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 4, 0, 0)),
        Err(Refusal::NoMana { need: 2, have: 1 })
    ); // Hearth Warden costs 2
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 7, 0, 0)),
        Err(Refusal::NotInHand)
    );
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 5, 0, 0)),
        Err(Refusal::BadTarget),
        "Flare is a spell"
    );
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 99, 0, 0)),
        Err(Refusal::UnknownCard)
    );
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 2, 3, 0)),
        Err(Refusal::LaneOutOfRange)
    );
    g.apply(&tap(0, Kind::CastUnit, 2, 0, 0)).unwrap(); // Cinder Whelp, cost 1, lane 0 back
    assert!(g.seats[0].cells[0][0].is_some());
    assert_eq!(g.seats[0].available_mana(), 0);
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 2, 0, 0)),
        Err(Refusal::NotInHand),
        "only one Whelp in the opening hand"
    );
}

#[test]
fn rush_enters_mid_and_occupied_cell_refuses() {
    let mut g = game();
    for _ in 0..4 {
        charge_last(&mut g);
        pass_round(&mut g);
    } // round 5, 4 mana, hand [2,3,4,5,10]
    assert_eq!(g.seats[0].charged, 4);
    assert!(g.seats[0].hand[..g.seats[0].hand_len()].contains(&3));
    g.apply(&tap(0, Kind::CastUnit, 3, 1, 0)).unwrap(); // Ashen Vanguard, Rush
    assert!(g.seats[0].cells[1][1].is_some(), "Rush enters the mid cell");
    assert!(g.seats[0].cells[1][0].is_none());
    g.apply(&tap(0, Kind::CastUnit, 2, 1, 0)).unwrap(); // Whelp into lane 1 back
    assert_eq!(
        g.apply(&tap(0, Kind::CastUnit, 4, 1, 0)),
        Err(Refusal::CellOccupied)
    );
}

#[test]
fn not_your_turn_and_pass_advances_turn_and_round() {
    let mut g = game();
    assert_eq!(
        g.apply(&tap(1, Kind::Charge, 2, -1, 0)),
        Err(Refusal::NotYourTurn)
    );
    assert_eq!(
        g.apply(&tap(2, Kind::Pass, 0, -1, 0)),
        Err(Refusal::NotYourTurn)
    );
    g.apply(&tap(0, Kind::Pass, 0, -1, 0)).unwrap();
    assert_eq!((g.active, g.round), (1, 1));
    assert_eq!(
        g.seats[1].hand_len(),
        7,
        "seat 1 draws at the start of its turn"
    );
    g.apply(&tap(1, Kind::Pass, 0, -1, 0)).unwrap();
    assert_eq!((g.active, g.round), (0, 2));
    assert_eq!(g.seats[0].hand_len(), 6, "draw at the start of your turn");
}

#[test]
fn front_units_hit_the_castle_when_unopposed() {
    let mut g = game();
    g.apply(&tap(0, Kind::Charge, 6, -1, 0)).unwrap();
    g.apply(&tap(0, Kind::CastUnit, 2, 0, 0)).unwrap(); // Whelp 2/1 Haste in lane 0 back
    g.apply(&tap(0, Kind::Advance, 0, 0, 0)).unwrap(); // Haste: may advance the turn it enters → mid
    assert!(g.seats[0].cells[0][1].is_some());
    assert_eq!(
        g.apply(&tap(0, Kind::Advance, 0, 0, 0)),
        Err(Refusal::AlreadyAdvancedLane)
    );
    g.apply(&tap(0, Kind::Pass, 0, -1, 0)).unwrap(); // mid unit does not attack
    assert_eq!(g.seats[1].castle.life, 20);
    g.apply(&tap(1, Kind::Pass, 0, -1, 0)).unwrap();
    g.apply(&tap(0, Kind::Advance, 0, 0, 0)).unwrap(); // → front
    g.apply(&tap(0, Kind::Pass, 0, -1, 0)).unwrap(); // combat: front Whelp hits castle for 2
    assert_eq!(g.seats[1].castle.life, 18);
}

#[test]
fn advance_refuses_a_unit_that_entered_this_round_without_haste() {
    let mut g = game();
    for _ in 0..2 {
        charge_last(&mut g);
        pass_round(&mut g);
    } // round 3, 2 mana, hand [2,3,4,5,8]
    g.apply(&tap(0, Kind::CastUnit, 4, 2, 0)).unwrap(); // Hearth Warden 1/3 Taunt, no Haste
    g.apply(&tap(0, Kind::Advance, 0, 2, 0)).unwrap(); // accepted, but nothing moves
    assert!(g.seats[0].cells[2][0].is_some());
    assert!(g.seats[0].cells[2][1].is_none());
}

#[test]
fn combat_is_simultaneous_and_shield_absorbs_one() {
    // Seat 0: Whelp 2/1 Haste in front of lane 0. Seat 1: Pearl Shieldbearer 1/2 Shield1 in front of lane 0.
    let mut g = game();
    for _ in 0..2 {
        charge_last(&mut g);
        pass_round(&mut g);
    } // round 3, seat 0 has 2 mana
    // seat 0 casts Whelp, advances to mid (Haste)
    g.apply(&tap(0, Kind::CastUnit, 2, 0, 0)).unwrap();
    g.apply(&tap(0, Kind::Advance, 0, 0, 0)).unwrap();
    g.apply(&tap(0, Kind::Pass, 0, -1, 0)).unwrap();
    // seat 1: charge twice over two turns is not possible in one turn; give seat 1 mana via its own charges
    // seat 1 hand at this point: opening [2,3,4,5,6,7] + draws 8,9,10 → charge_last takes 10, then 9 …
    charge_last(&mut g); // seat 1 charged 1
    g.apply(&tap(1, Kind::Pass, 0, -1, 0)).unwrap();
    g.apply(&tap(0, Kind::Advance, 0, 0, 0)).unwrap(); // Whelp → front (round 4)
    g.apply(&tap(0, Kind::Pass, 0, -1, 0)).unwrap(); // Whelp hits castle: 18
    assert_eq!(g.seats[1].castle.life, 18);
    charge_last(&mut g); // seat 1 charged 2
    g.apply(&tap(1, Kind::CastUnit, 8, 0, 0)).unwrap(); // Shieldbearer, lane 0 back (seat 1 side)
    g.apply(&tap(1, Kind::Pass, 0, -1, 0)).unwrap(); // Whelp hits castle again: 16
    assert_eq!(g.seats[1].castle.life, 16);
    // seat 0 passes twice while seat 1 advances the Shieldbearer to the front
    g.apply(&tap(0, Kind::Pass, 0, -1, 0)).unwrap(); // Whelp: 14
    g.apply(&tap(1, Kind::Advance, 0, 0, 0)).unwrap(); // Shieldbearer → mid
    g.apply(&tap(1, Kind::Pass, 0, -1, 0)).unwrap(); // Whelp: 12 (Shieldbearer in mid, melee ignores it)
    assert_eq!(g.seats[1].castle.life, 12);
    g.apply(&tap(0, Kind::Pass, 0, -1, 0)).unwrap(); // Whelp: 10
    g.apply(&tap(1, Kind::Advance, 0, 0, 0)).unwrap(); // Shieldbearer → front
    g.apply(&tap(1, Kind::Pass, 0, -1, 0)).unwrap(); // Whelp 2 vs Shield1 → 1 damage; Shieldbearer 1 vs Whelp 1 toughness → Whelp dies
    assert_eq!(g.seats[1].castle.life, 10, "front unit absorbs the hit");
    assert!(
        g.seats[0].cells[0][2].is_none(),
        "Whelp died simultaneously"
    );
    let sb = g.seats[1].cells[0][2].unwrap();
    assert_eq!(sb.damage, 1, "Shield 1 absorbed one of the Whelp's 2");
}

#[test]
fn spells_check_mana_before_resolving_and_targets_are_bounds_checked() {
    let mut g = game();
    // hand [2,3,4,5,6]; Flare (5) costs 1, Damage 2 castle_ok
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 5, -1, 0xFF)),
        Err(Refusal::NoMana { need: 1, have: 0 })
    );
    assert_eq!(
        g.seats[1].castle.life, 20,
        "a refused spell changes nothing"
    );
    g.apply(&tap(0, Kind::Charge, 6, -1, 0)).unwrap();
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 5, -1, 0b0001_1111)),
        Err(Refusal::BadTarget),
        "lane/cell 3 is out of range, no panic"
    );
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 5, -1, 0b0001_0000)),
        Err(Refusal::BadTarget),
        "empty cell"
    );
    g.apply(&tap(0, Kind::CastSpell, 5, -1, 0xFF)).unwrap();
    assert_eq!(g.seats[1].castle.life, 18);
    assert_eq!(g.seats[0].available_mana(), 0);
    assert!(!g.seats[0].hand[..g.seats[0].hand_len()].contains(&5));
}

#[test]
fn pressure_from_round_8_and_stop_at_12() {
    let mut g = game();
    while g.round < 8 {
        pass_round(&mut g);
    }
    let before = g.seats[0].castle.life;
    assert_eq!(before, 18, "seat 0 took pressure at the start of round 8");
    pass_round(&mut g);
    assert_eq!(g.seats[0].castle.life, before - 2);
    while g.phase == Phase::Playing {
        pass_round(&mut g);
    }
    assert_eq!(g.round, 12);
    assert_eq!(g.seats[0].castle.life, 10);
    assert_eq!(
        g.winner,
        Some(Winner::Seat(1)),
        "equal life, equal units → second player"
    );
    assert_eq!(
        g.apply(&tap(0, Kind::Pass, 0, -1, 0)),
        Err(Refusal::GameOver)
    );
}

// ---- Task 4 follow-up: targeted combat and spell tests -------------------------------------

use tapstone_rules::state::Unit;
use tapstone_rules::{Applied, Keyword};

fn unit(a: u8, t: u8, kw: Option<Keyword>) -> Unit {
    Unit {
        design: 7,
        attack: a,
        toughness: t,
        damage: 0,
        keyword: kw,
        entered_round: 0,
    }
}
fn tgt(s: u8, l: u8, c: u8) -> u8 {
    (s << 4) | (l << 2) | c
}
/// Give seat 0 `mana` and put `card` at hand[0] (hand_len stays 5).
fn arm(g: &mut Game, mana: u8, card: u16) {
    g.seats[0].charged = mana;
    g.seats[0].spent = 0;
    g.seats[0].hand[0] = card;
}
fn tap_aux(seat: u8, kind: Kind, card: u16, target: u8, aux: u8) -> Record {
    let mut r = tap(seat, kind, card, -1, target);
    r.aux = aux;
    r
}

#[test]
fn taunt_draws_melee_and_ranged_attackers() {
    let mut g = game();
    g.seats[0].cells[0][2] = Some(unit(3, 9, None));
    g.seats[1].cells[0][1] = Some(unit(1, 9, Some(Keyword::Taunt)));
    g.seats[1].cells[0][2] = Some(unit(2, 9, None));
    g.seats[0].cells[1][0] = Some(unit(1, 9, Some(Keyword::Ranged)));
    g.seats[1].cells[1][1] = Some(unit(1, 9, Some(Keyword::Taunt)));
    g.seats[1].cells[1][2] = Some(unit(2, 9, None));
    let res = g.apply(&tap(0, Kind::Pass, 0, -1, 0));
    assert_eq!(
        g.seats[1].cells[0][1].unwrap().damage,
        3,
        "melee front unit is drawn to the Taunt"
    );
    assert_eq!(g.seats[1].cells[0][2].unwrap().damage, 0);
    assert_eq!(
        g.seats[1].cells[1][1].unwrap().damage,
        1,
        "ranged unit is drawn to the Taunt"
    );
    assert_eq!(g.seats[1].cells[1][2].unwrap().damage, 0);
    assert_eq!(
        g.seats[0].cells[0][2].unwrap().damage,
        2,
        "enemy front melee hits my front unit"
    );
    // seat 1's [1][2] is a front melee unit facing an empty seat-0 front cell: it hits seat 0's castle.
    assert_eq!(
        res,
        Ok(Applied::TurnEnded {
            combat_damage: [2, 0]
        })
    );
    assert_eq!(g.seats[1].castle.life, 20);
    assert_eq!(g.seats[0].castle.life, 18);
}

#[test]
fn ranged_hits_nearest_or_castle_from_any_cell() {
    let mut g = game();
    g.seats[0].cells[0][0] = Some(unit(1, 9, Some(Keyword::Ranged)));
    g.seats[1].cells[0][0] = Some(unit(2, 9, None));
    g.seats[1].cells[0][1] = Some(unit(2, 9, None));
    g.seats[0].cells[2][0] = Some(unit(1, 9, Some(Keyword::Ranged)));
    let res = g.apply(&tap(0, Kind::Pass, 0, -1, 0));
    assert_eq!(
        g.seats[1].cells[0][1].unwrap().damage,
        1,
        "nearest enemy, front to back"
    );
    assert_eq!(g.seats[1].cells[0][0].unwrap().damage, 0);
    assert_eq!(
        g.seats[1].castle.life, 19,
        "unopposed ranged hits the castle"
    );
    assert_eq!(
        res,
        Ok(Applied::TurnEnded {
            combat_damage: [0, 1]
        })
    );
    assert_eq!(
        g.seats[0].cells[0][0].unwrap().damage,
        0,
        "enemy back melee is idle"
    );
}

#[test]
fn shift_moves_a_unit_and_refuses_cleanly() {
    let mut g = game();
    arm(&mut g, 2, 10); // Undertow: cost 2, Shift
    g.seats[1].cells[1][1] = Some(unit(2, 3, None));
    let before = g;

    // (a) aux 0: toward lane 0
    assert_eq!(
        g.apply(&tap_aux(0, Kind::CastSpell, 10, tgt(1, 1, 1), 0)),
        Ok(Applied::Spell)
    );
    assert!(g.seats[1].cells[0][1].is_some());
    assert!(g.seats[1].cells[1][1].is_none());

    // (b) aux 1: toward lane 2
    let mut g = before;
    g.apply(&tap_aux(0, Kind::CastSpell, 10, tgt(1, 1, 1), 1))
        .unwrap();
    assert!(g.seats[1].cells[2][1].is_some());

    // (c) destination occupied
    let mut g = before;
    g.seats[1].cells[2][1] = Some(unit(1, 1, None));
    let snapshot = g;
    assert_eq!(
        g.apply(&tap_aux(0, Kind::CastSpell, 10, tgt(1, 1, 1), 1)),
        Err(Refusal::CellOccupied)
    );
    assert_eq!(g, snapshot);

    // (d) empty source
    let mut g = before;
    assert_eq!(
        g.apply(&tap_aux(0, Kind::CastSpell, 10, tgt(1, 2, 1), 0)),
        Err(Refusal::BadTarget)
    );
    assert_eq!(g, before);

    // (e) off the board
    let mut g = before;
    g.seats[1].cells[1][1] = None;
    g.seats[1].cells[0][1] = Some(unit(2, 3, None));
    let snapshot = g;
    assert_eq!(
        g.apply(&tap_aux(0, Kind::CastSpell, 10, tgt(1, 0, 1), 0)),
        Err(Refusal::LaneOutOfRange)
    );
    assert_eq!(g, snapshot);
}

#[test]
fn damage_spell_kills_and_sweeps_and_castle_target_needs_castle_ok() {
    let mut g = game();
    arm(&mut g, 2, 9); // Tidal Lash: cost 2, Damage 3, not castle_ok
    g.seats[1].cells[0][2] = Some(unit(2, 3, None));
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 9, -1, tgt(1, 0, 2))),
        Ok(Applied::Spell)
    );
    assert!(
        g.seats[1].cells[0][2].is_none(),
        "3 damage on toughness 3 is swept"
    );
    assert_eq!(g.seats[0].available_mana(), 0);

    let mut g = game();
    arm(&mut g, 2, 9);
    let before = g;
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 9, -1, 0xFF)),
        Err(Refusal::BadTarget)
    );
    assert_eq!(g, before);
}

#[test]
fn draw_spell_draws_count() {
    let mut g = game();
    arm(&mut g, 1, 11); // Deep Breath: cost 1, Draw 2
    let hand = g.seats[0].hand_len();
    let pos = g.seats[0].deck_pos;
    g.apply(&tap(0, Kind::CastSpell, 11, -1, 0)).unwrap();
    assert_eq!(
        g.seats[0].hand_len(),
        hand + 1,
        "two drawn, one card left the hand"
    );
    assert_eq!(g.seats[0].deck_pos, pos + 2);
}

#[test]
fn stop_round_tie_breaks_on_units() {
    let mut g = game();
    g.round = 12;
    g.active = 1;
    g.seats[0].cells[0][0] = Some(unit(2, 3, None));
    assert_eq!(
        g.apply(&tap(1, Kind::Pass, 0, -1, 0)),
        Ok(Applied::GameEnded(Winner::Seat(0)))
    );
    assert_eq!(g.phase, Phase::Over);
    assert_eq!(g.round, 12);
}

#[test]
fn lethal_spell_ends_the_game_immediately() {
    let mut g = game();
    arm(&mut g, 1, 5); // Flare: cost 1, Damage 2, castle_ok
    g.seats[1].castle.life = 2;
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 5, -1, 0xFF)),
        Ok(Applied::GameEnded(Winner::Seat(0)))
    );
    assert_eq!(g.phase, Phase::Over);
    assert_eq!(g.seats[1].castle.life, 0);
    assert_eq!(
        g.apply(&tap(0, Kind::Pass, 0, -1, 0)),
        Err(Refusal::GameOver)
    );
}

#[test]
fn heal_spell_reduces_damage() {
    let mut g = game();
    arm(&mut g, 1, 12); // Mend: cost 1, Heal 2
    g.seats[0].cells[0][0] = Some(Unit {
        damage: 2,
        ..unit(1, 3, None)
    });
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 12, -1, tgt(0, 0, 0))),
        Ok(Applied::Spell)
    );
    assert_eq!(g.seats[0].cells[0][0].unwrap().damage, 0);
    assert_eq!(g.seats[0].available_mana(), 0);

    let mut g = game();
    arm(&mut g, 1, 12);
    let before = g;
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 12, -1, tgt(0, 0, 0))),
        Err(Refusal::BadTarget),
        "empty cell"
    );
    assert_eq!(g, before);
}

#[test]
fn destroy_spell_respects_max_toughness() {
    let mut g = game();
    arm(&mut g, 3, 13); // Riptide: cost 3, Destroy toughness <= 2
    g.seats[1].cells[0][2] = Some(unit(2, 2, None));
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 13, -1, tgt(1, 0, 2))),
        Ok(Applied::Spell)
    );
    assert!(g.seats[1].cells[0][2].is_none());

    let mut g = game();
    arm(&mut g, 3, 13);
    g.seats[1].cells[0][2] = Some(unit(2, 3, None));
    let before = g;
    assert_eq!(
        g.apply(&tap(0, Kind::CastSpell, 13, -1, tgt(1, 0, 2))),
        Err(Refusal::BadTarget),
        "too tough"
    );
    assert_eq!(g, before);
}
