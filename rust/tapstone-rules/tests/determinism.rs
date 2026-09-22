use tapstone_rules::hash::{Chain, canonical_len};
use tapstone_rules::{Game, HouseRules, Kind, Record};

#[test]
fn same_records_same_chain_and_a_changed_record_changes_it() {
    let deck = [
        2u16, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6,
    ];
    let recs = [
        Record {
            seq: 0,
            seat: 0,
            kind: Kind::Charge,
            card: 6,
            lane: -1,
            target: 0,
            aux: 0,
            time_ms: 1,
            uid: [0; 7],
            auth: 0,
        },
        Record {
            seq: 1,
            seat: 0,
            kind: Kind::CastUnit,
            card: 2,
            lane: 0,
            target: 0,
            aux: 0,
            time_ms: 2,
            uid: [0; 7],
            auth: 0,
        },
        Record {
            seq: 2,
            seat: 0,
            kind: Kind::Pass,
            card: 0,
            lane: -1,
            target: 0,
            aux: 0,
            time_ms: 3,
            uid: [0; 7],
            auth: 0,
        },
    ];
    let run = |recs: &[Record]| {
        let mut g = Game::new(HouseRules::default(), [0, 1], [&deck, &deck]).started();
        let mut chain = Chain::genesis(&g.rules);
        for r in recs {
            g.apply(r).unwrap();
            chain.step(r, &g);
        }
        chain.head()
    };
    assert_eq!(run(&recs), run(&recs));
    let mut other = recs;
    other[1].lane = 1;
    assert_ne!(run(&recs), run(&other));
    assert!(
        canonical_len() <= 512,
        "canonical state must fit a small fixed buffer on the shrine"
    );
}

#[test]
fn genesis_depends_on_house_rules_and_head_changes_per_step() {
    let a = Chain::genesis(&HouseRules::default());
    let hr = HouseRules {
        castle_life: 25,
        ..HouseRules::default()
    };
    assert_ne!(a.head(), Chain::genesis(&hr).head());
    assert_eq!(a.len, 0);
}

fn record(seat: u8, kind: Kind, card: u16) -> Record {
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

#[test]
fn genesis_changes_when_rules_change_in_lobby() {
    let deck = [
        2u16, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6,
    ];
    let mut g = Game::new(HouseRules::default(), [0, 1], [&deck, &deck]);
    let before = Chain::genesis(&g.rules);
    g.with_rules(HouseRules {
        castle_life: 25,
        ..HouseRules::default()
    })
    .unwrap();
    assert_ne!(before.head(), Chain::genesis(&g.rules).head());
}

#[test]
fn mulligan_changes_the_chain() {
    let deck = [
        2u16, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 2, 3, 4, 5, 6,
    ];
    let run = |recs: &[Record]| {
        let mut g = Game::new(HouseRules::default(), [0, 1], [&deck, &deck]).started();
        let mut chain = Chain::genesis(&g.rules);
        for r in recs {
            g.apply(r).unwrap();
            chain.step(r, &g);
        }
        chain.head()
    };
    let with = [record(0, Kind::Mulligan, 0), record(0, Kind::Pass, 0)];
    let without = [record(0, Kind::Pass, 0)];
    assert_ne!(run(&with), run(&without));
}
