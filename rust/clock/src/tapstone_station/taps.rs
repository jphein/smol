//! Card taps at the S3 station: the RC522 on the P3 jack, read through the shared SPI3
//! ([`super::spi3`]), drives the seat by [`crate::shrine_taps`]'s grammar (tapstone 0009, 0032,
//! 0036, as the scry bridge plays them). With no reader on P3 the seat keeps `Autoplay`.
//!
//! - **Detection, once at boot**: VersionReg read with the MISO pad pulled up, then down
//!   ([`Spi3::probe_reader`]). Both must agree on a byte that isn't 0x00 or 0xFF.
//! - **Polling**: a REQA every [`POLL_MS`]. A card answers, is selected for its UID, and is then
//!   halted (HLTA), so a card left on the reader is one tap. Lifting it and tapping again is the
//!   next. The RC522's no-card timeout is cut from the crate's 25 ms to 5 ms, so an empty poll
//!   costs the radio and the voice ring 5 ms.
//! - **A move** goes out through `Shrine::propose_to`. The seat is manual while a reader is
//!   present, and `act_to` then proposes nothing and re-sends nothing, so a proposal that gets no
//!   answer in [`RESEND_MS`] is proposed again here.
//! - **Design-level copies**: the engine and the arena check a draw's design and that the copy
//!   is undrawn, and the seat stamps the first undrawn copy of the design (0036). So "in hand"
//!   and "drawn" are answered by design, and two copies of a design are interchangeable.
use core::cell::RefCell;

use esp_println::println;
use mfrc522::comm::blocking::spi::{DummyDelay, SpiInterface};
use mfrc522::{Initialized, Mfrc522};
use tapstone_proto::shrine::{Autoplay, Shrine, tap};
use tapstone_rules::cards::design;
use tapstone_rules::state::{CELLS, LANES};
use tapstone_rules::{CardKind, Effect, Game, Kind, Phase, Record};

use super::spi3::{ReaderDev, Spi3};
use crate::shrine_taps::{Move, Out, Registry, Seat, Seen, Taps, Uid, Why};

/// How often the reader looks for a card.
pub const POLL_MS: u64 = 100;
/// A proposal with no commit or reject after this long is proposed again.
pub const RESEND_MS: u64 = 300;

pub struct Reader {
    rc: Mfrc522<SpiInterface<ReaderDev, DummyDelay>, Initialized>,
    next_poll: u64,
    pub registry: Registry,
    pub taps: Taps,
    /// Moves waiting for the seat's proposal in flight to be answered, oldest first (a tap yields
    /// at most two: a lapsed window's, then its own).
    queued: [Option<Move>; 2],
    pub taps_read: u32,
}

/// One RC522 register write: address byte `(reg << 1) & 0x7E` (MSB 0 = write), then the value.
fn write_reg(cell: &'static RefCell<Spi3>, reg: u8, val: u8) -> bool {
    use embedded_hal::spi::SpiDevice;
    Spi3::reader(cell).write(&[(reg << 1) & 0x7E, val]).is_ok()
}

impl Reader {
    /// The reader on P3, or `None` (and the seat keeps Autoplay). In a static, not built by value
    /// into the station: the main task's frame on the S3 has no room (see `Voice::new`).
    #[inline(never)]
    pub fn detect(cell: &'static RefCell<Spi3>, deck_len: usize) -> Option<&'static mut Reader> {
        let v = Spi3::probe_reader(cell);
        if v[0] != v[1] || matches!(v[0], 0x00 | 0xFF | 0xEE) {
            println!(
                "[taps] no reader on P3 (VersionReg 0x{:02X} pulled up, 0x{:02X} pulled down) - the seat plays by Autoplay",
                v[0], v[1]
            );
            return None;
        }
        let rc = match Mfrc522::new(SpiInterface::new(Spi3::reader(cell))).init() {
            Ok(rc) => rc,
            Err(e) => {
                println!("[taps] RC522 0x{:02X} did not init ({:?}) - the seat plays by Autoplay", v[0], e);
                return None;
            }
        };
        // TReloadReg (0x2C high, 0x2D low) = 200 ticks of the 25 us timer the crate set up: 5 ms.
        let fast = write_reg(cell, 0x2C, 0x00) && write_reg(cell, 0x2D, 0xC8);
        println!(
            "[taps] RC522 0x{:02X} on P3 - card taps drive the seat (no-card timeout {} ms)",
            v[0],
            if fast { 5 } else { 25 }
        );
        static READER: static_cell::StaticCell<Reader> = static_cell::StaticCell::new();
        Some(READER.init(Reader {
            rc,
            next_poll: 0,
            registry: Registry::new(deck_len),
            taps: Taps::new(),
            queued: [None, None],
            taps_read: 0,
        }))
    }

    /// A newly presented card's UID, if one answered this poll.
    fn poll(&mut self, now: u64) -> Option<Uid> {
        if now < self.next_poll {
            return None;
        }
        self.next_poll = now + POLL_MS;
        let atqa = self.rc.reqa().ok()?;
        let uid = self.rc.select(&atqa).ok().and_then(|u| Uid::new(u.as_bytes()));
        let _ = self.rc.hlta();
        uid
    }

    /// One service call: windows that lapsed, a tap if a card came, and the queued move. Returns
    /// the record to propose now (one in flight at a time) and the line to say, if any.
    pub fn service(&mut self, now: u64, shrine: &Shrine<Autoplay>) -> (Option<Record>, Option<Line>) {
        let seat = SeatView(shrine);
        let mut moves: [Option<Move>; 2] = [None, None];
        let mut line = None;
        match self.taps.tick(now) {
            Out::Moves(m) => moves = m,
            _ => {
                if let Some(uid) = self.poll(now) {
                    self.taps_read += 1;
                    let tag = match self.registry.see(uid) {
                        Seen::Known(t) => Some(t),
                        Seen::Bound(t) => {
                            println!("[taps] {} registered as {:?} ({} bound)", uid, t, self.registry.bound());
                            Some(t)
                        }
                        Seen::Full => {
                            println!("[taps] {}: every copy is bound already - refused", uid);
                            line = Some(Line::Refused(Why::Full));
                            None
                        }
                    };
                    if let Some(tag) = tag {
                        let out = self.taps.tap(now, tag, &seat);
                        println!("[taps] {} = {:?} -> {:?}", uid, tag, out);
                        match out {
                            Out::Moves(m) => moves = m,
                            Out::CastOrCharge(k) => line = Some(Line::CastOrCharge(shrine.deck[k as usize])),
                            Out::MulliganPrompt => line = Some(Line::MulliganPrompt),
                            Out::Refused(w) => line = Some(Line::Refused(w)),
                            Out::Nothing => {}
                        }
                    }
                }
            }
        }
        // One proposal in flight: moves queue in order, and the next goes once the seat's last
        // proposal is answered (committed or rejected).
        for m in moves.into_iter().flatten() {
            match self.queued.iter_mut().find(|q| q.is_none()) {
                Some(slot) => *slot = Some(m),
                None => println!("[taps] {:?}: two moves already waiting - dropped", m),
            }
        }
        let mut send = None;
        if shrine.pending.is_none()
            && let Some(m) = self.queued[0].take()
        {
            self.queued = [self.queued[1].take(), None];
            send = record(shrine, m);
            if send.is_none() {
                println!("[taps] {:?}: no legal form now - not sent", m);
            }
        }
        (send, line)
    }
}

/// What the station says for a tap (the voice band's sentences, by `shrine_render::voice`).
#[derive(Clone, Copy, Debug)]
pub enum Line {
    CastOrCharge(u16),
    MulliganPrompt,
    Refused(Why),
}

impl Line {
    pub fn text(&self) -> shrine_render::fmt::Text {
        use shrine_render::voice::Voice;
        use tapstone_rules::Refusal;
        match *self {
            Line::CastOrCharge(d) => Voice::CastOrCharge { card: design(d).map_or("that card", |c| c.name) }.text(),
            Line::MulliganPrompt => Voice::MulliganPrompt { secs: (crate::shrine_taps::WINDOW_MS / 1000) as u8 }.text(),
            Line::Refused(w) => Voice::Refused(match w {
                Why::NotYourMove => Refusal::NotYourTurn,
                Why::DrawFirst => Refusal::DrawOwed,
                Why::Played | Why::NotInHand => Refusal::NotInHand,
                Why::Full => Refusal::UnknownCard,
            })
            .text(),
        }
    }
}

/// A stale proposal (no commit or reject in [`RESEND_MS`]), to propose again: a manual seat's
/// `act_to` re-sends nothing itself.
pub fn stale(shrine: &Shrine<Autoplay>, now: u64) -> Option<Record> {
    let (_, sent) = shrine.pending?;
    (now.saturating_sub(sent) >= RESEND_MS).then_some(shrine.pending_record?)
}

/// The seat, as the tap grammar reads it.
struct SeatView<'a>(&'a Shrine<Autoplay>);

impl SeatView<'_> {
    fn design(&self, k: u8) -> Option<u16> {
        self.0.deck.get(k as usize).copied()
    }
}

impl Seat for SeatView<'_> {
    fn my_move(&self) -> bool {
        self.0.my_move()
    }

    fn draw_owed(&self) -> bool {
        let g = &self.0.follower.game;
        self.0
            .seat()
            .is_some_and(|s| g.phase == Phase::Playing && g.seats[s].owed_draws() > 0)
    }

    fn in_hand(&self, k: u8) -> bool {
        self.design(k).is_some_and(|d| self.0.hand.contains(&d))
    }

    fn drawn(&self, k: u8) -> bool {
        self.design(k).is_none_or(|d| self.0.undrawn_copy(d).is_none())
    }

    fn may_mulligan(&self) -> bool {
        let Some(s) = self.0.seat() else {
            return false;
        };
        legal(&self.0.follower.game, &tap(s as u8, Kind::Mulligan, 0, -1, 0, 0))
    }
}

fn legal(g: &Game, r: &Record) -> bool {
    let mut probe = *g;
    probe.apply(r).is_ok()
}

fn target_byte(seat: usize, lane: usize, cell: usize) -> u8 {
    ((seat as u8) << 4) | ((lane as u8) << 2) | cell as u8
}

/// The record for a move, with 0009's defaults for a cast: a unit into the legal lane with the
/// fewest of the seat's units (the lowest lane on a tie), a spell at its first legal target in
/// `Autoplay`'s order (a castle-capable damage spell at the castle, a heal at the seat's own
/// side, an untargeted draw, then each occupied cell). `None` when nothing legal fits.
fn record(shrine: &Shrine<Autoplay>, m: Move) -> Option<Record> {
    let seat = shrine.seat()? as u8;
    let g = &shrine.follower.game;
    let card = |k: u8| shrine.deck.get(k as usize).copied();
    Some(match m {
        Move::Pass => tap(seat, Kind::Pass, 0, -1, 0, 0),
        Move::Mulligan => tap(seat, Kind::Mulligan, 0, -1, 0, 0),
        Move::Draw(k) => tap(seat, Kind::Draw, card(k)?, -1, 0, 0),
        Move::Charge(k) => tap(seat, Kind::Charge, card(k)?, -1, 0, 0),
        Move::Cast(k) => {
            let c = card(k)?;
            let me = (seat & 1) as usize;
            let opp = 1 - me;
            match design(c)?.kind {
                CardKind::Spell(effect) => {
                    let side = if matches!(effect, Effect::Heal { .. }) { me } else { opp };
                    let castle = matches!(effect, Effect::Damage { castle_ok: true, .. });
                    let untargeted = matches!(effect, Effect::Draw { .. });
                    let cells = (0..LANES).flat_map(|l| (0..CELLS).map(move |cc| (l, cc)));
                    castle
                        .then(|| tap(seat, Kind::CastSpell, c, -1, 0xFF, 0))
                        .into_iter()
                        .chain(untargeted.then(|| tap(seat, Kind::CastSpell, c, -1, 0, 0)))
                        .chain(
                            cells
                                .filter(|&(l, cc)| g.seats[side].cells[l][cc].is_some())
                                .map(|(l, cc)| tap(seat, Kind::CastSpell, c, -1, target_byte(side, l, cc), 0)),
                        )
                        .find(|r| legal(g, r))?
                }
                _ => {
                    let mut lanes = [0usize, 1, 2];
                    let units = |l: usize| g.seats[me].cells[l].iter().filter(|x| x.is_some()).count();
                    lanes[..LANES].sort_by_key(|&l| (units(l), l));
                    lanes[..LANES]
                        .iter()
                        .map(|&l| tap(seat, Kind::CastUnit, c, l as i8, 0, 0))
                        .find(|r| legal(g, r))?
                }
            }
        }
    })
}
