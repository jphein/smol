//! The station's tap grammar (src/shrine_taps.rs): tapstone 0009, 0032 and 0036 as the scry
//! bridge plays them, driven through whole prompt cycles against a table seat.
use clock::shrine_taps::{Move, Out, Registry, Seat, Seen, Tag, Taps, Uid, WINDOW_MS, Why};

#[derive(Default)]
struct Table {
    my_move: bool,
    owed: bool,
    hand: Vec<u8>,
    drawn: Vec<u8>,
    mulligan: bool,
}

impl Seat for Table {
    fn my_move(&self) -> bool { self.my_move }
    fn draw_owed(&self) -> bool { self.owed }
    fn in_hand(&self, k: u8) -> bool { self.hand.contains(&k) }
    fn drawn(&self, k: u8) -> bool { self.drawn.contains(&k) }
    fn may_mulligan(&self) -> bool { self.mulligan }
}

fn uid(n: u8) -> Uid {
    Uid::new(&[4, n, 0x10, 0x20, 0x30, 0x40, 0x81]).unwrap()
}

fn moves(o: Out) -> Vec<Move> {
    match o {
        Out::Moves(m) => m.into_iter().flatten().collect(),
        other => panic!("expected moves, got {other:?}"),
    }
}

#[test]
fn fresh_tags_bind_the_castle_then_copies_in_list_order() {
    let mut r = Registry::new(3);
    assert_eq!(r.see(uid(1)), Seen::Bound(Tag::Castle));
    assert_eq!(r.see(uid(2)), Seen::Bound(Tag::Copy(0)));
    assert_eq!(r.see(uid(3)), Seen::Bound(Tag::Copy(1)));
    assert_eq!(r.see(uid(1)), Seen::Known(Tag::Castle));
    assert_eq!(r.see(uid(2)), Seen::Known(Tag::Copy(0)));
    assert_eq!(r.see(uid(4)), Seen::Bound(Tag::Copy(2)));
    assert_eq!(r.see(uid(5)), Seen::Full);
    assert_eq!(r.bound(), 4);
    assert!(Uid::new(&[1, 2, 3]).is_none(), "no 3-byte tags");
    assert_eq!(format!("{}", Uid::new(&[0x01, 0x53, 0x49, 0x64]).unwrap()), "01:53:49:64");
}

#[test]
fn castle_taps_alone_reach_the_mulligan() {
    // 0032: a person with only the castle figurine can mulligan.
    let mut t = Taps::new();
    let seat = Table { my_move: true, mulligan: true, ..Default::default() };
    assert_eq!(t.tap(0, Tag::Castle, &seat), Out::MulliganPrompt);
    assert_eq!(moves(t.tap(1_200, Tag::Castle, &seat)), vec![Move::Mulligan]);
    assert!(!t.pending());
}

#[test]
fn the_mulligan_prompt_keeps_and_passes_on_expiry_or_another_tap() {
    let seat = Table { my_move: true, mulligan: true, hand: vec![4], drawn: vec![4], ..Default::default() };
    let mut t = Taps::new();
    t.tap(0, Tag::Castle, &seat);
    assert_eq!(t.tick(WINDOW_MS - 1), Out::Nothing);
    assert_eq!(moves(t.tick(WINDOW_MS)), vec![Move::Pass]);
    // Another tap keeps too: the pass goes first, then that tap is read on its own.
    let mut t = Taps::new();
    t.tap(0, Tag::Castle, &seat);
    assert_eq!(moves(t.tap(500, Tag::Copy(4), &seat)), vec![Move::Pass]);
    assert!(t.pending(), "copy 4's cast-or-charge window opened after the pass");
    // A castle tap after the window lapsed is a new first tap, not a mulligan. The station ticks
    // before it reads a tap, so the lapse has passed the turn by then.
    let mut t = Taps::new();
    t.tap(0, Tag::Castle, &seat);
    assert_eq!(moves(t.tick(WINDOW_MS)), vec![Move::Pass]);
    let after = Table { my_move: false, ..Default::default() };
    assert_eq!(t.tap(WINDOW_MS + 1, Tag::Castle, &after), Out::Refused(Why::NotYourMove));
}

#[test]
fn an_owed_draw_draws_the_tapped_copy() {
    // 0036: a draw is a tap, of the copy drawn.
    let seat = Table { my_move: true, owed: true, drawn: vec![0], hand: vec![0], ..Default::default() };
    let mut t = Taps::new();
    assert_eq!(moves(t.tap(0, Tag::Copy(7), &seat)), vec![Move::Draw(7)]);
    assert_eq!(t.tap(10, Tag::Copy(0), &seat), Out::Refused(Why::DrawFirst), "already drawn");
    assert_eq!(t.tap(20, Tag::Castle, &seat), Out::Refused(Why::DrawFirst), "no pass while a draw is owed");
}

#[test]
fn a_second_tap_within_three_seconds_charges() {
    let seat = Table { my_move: true, hand: vec![3], drawn: vec![3], ..Default::default() };
    let mut t = Taps::new();
    assert_eq!(t.tap(0, Tag::Copy(3), &seat), Out::CastOrCharge(3));
    assert_eq!(moves(t.tap(WINDOW_MS - 1, Tag::Copy(3), &seat)), vec![Move::Charge(3)]);
}

#[test]
fn an_unanswered_window_casts_with_the_defaults() {
    let seat = Table { my_move: true, hand: vec![3, 5], drawn: vec![3, 5], ..Default::default() };
    let mut t = Taps::new();
    t.tap(0, Tag::Copy(3), &seat);
    assert_eq!(moves(t.tick(WINDOW_MS)), vec![Move::Cast(3)]);
    // Another card closes it the same way, and opens its own.
    let mut t = Taps::new();
    t.tap(0, Tag::Copy(3), &seat);
    assert_eq!(moves(t.tap(100, Tag::Copy(5), &seat)), vec![Move::Cast(3)]);
    assert_eq!(moves(t.tap(200, Tag::Copy(5), &seat)), vec![Move::Charge(5)]);
    // The castle closes it and passes.
    let mut t = Taps::new();
    t.tap(0, Tag::Copy(3), &seat);
    assert_eq!(moves(t.tap(100, Tag::Castle, &seat)), vec![Move::Cast(3), Move::Pass]);
    // The same card after the window: a fresh first tap, which casts the lapsed one first.
    let mut t = Taps::new();
    t.tap(0, Tag::Copy(3), &seat);
    assert_eq!(moves(t.tap(WINDOW_MS, Tag::Copy(3), &seat)), vec![Move::Cast(3)]);
}

#[test]
fn everything_else_is_refused_locally() {
    let off = Table { my_move: false, hand: vec![3], drawn: vec![3], ..Default::default() };
    let mut t = Taps::new();
    assert_eq!(t.tap(0, Tag::Copy(3), &off), Out::Refused(Why::NotYourMove));
    assert_eq!(t.tap(0, Tag::Castle, &off), Out::Refused(Why::NotYourMove));
    let on = Table { my_move: true, hand: vec![3], drawn: vec![3, 8], ..Default::default() };
    assert_eq!(t.tap(0, Tag::Copy(8), &on), Out::Refused(Why::Played));
    assert_eq!(t.tap(0, Tag::Copy(9), &on), Out::Refused(Why::NotInHand));
    assert!(!t.pending());
}
