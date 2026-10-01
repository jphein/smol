//! Card taps at the shrine station: a tag's UID to the seat's move (tapstone 0009, 0032, 0036),
//! the same grammar as tapstone's scry bridge (`rust/tapstone-arena/tools/scry_bridge.py`), which
//! played real taps into an arena first. Pure, so the host tests drive whole prompt cycles
//! (`tests/shrine_taps.rs`); `tapstone_station/taps.rs` reads the seat for it and proposes what it
//! decides.
//!
//! **Registration is inline** (tapstone 0003: playtest stock is NTAG215 blanks with a UID
//! registry). The first fresh tag becomes the seat's castle, and each fresh tag after it is bound
//! to the next unbound copy of the deck list, then played like any other tap. Bindings live for
//! the power-up: the station writes nothing anywhere.
//!
//! **A tap means** (the scry bridge, plus the lead's mulligan ruling, 0032 item 13):
//! - the castle: pass. In the mulligan window it opens a 3 s prompt instead: a second castle tap
//!   mulligans, and expiry or any other tap keeps and passes;
//! - a copy not yet drawn, while a draw is owed: draw that copy (0036: a draw is a tap);
//! - a copy in hand, on the seat's move with no draw owed: the 3 s cast-or-charge window. A
//!   second tap of the same copy charges it; expiry or another tap casts it with 0009's defaults;
//! - anything else is refused locally, and nothing is sent.

/// 0009: "a second tap of the same card within 3 s means charge"; the lead's mulligan prompt is
/// the same 3 s.
pub const WINDOW_MS: u64 = 3_000;
/// A deck list's most copies (0040's decks are 30).
pub const MAX_COPIES: usize = 40;

/// A tag UID as the reader gives it: 4, 7 or 10 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uid {
    len: u8,
    bytes: [u8; 10],
}

impl Uid {
    /// `None` for a length a tag can't have.
    pub fn new(b: &[u8]) -> Option<Uid> {
        if !matches!(b.len(), 4 | 7 | 10) {
            return None;
        }
        let mut bytes = [0u8; 10];
        bytes[..b.len()].copy_from_slice(b);
        Some(Uid { len: b.len() as u8, bytes })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

impl core::fmt::Display for Uid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (i, b) in self.as_bytes().iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

/// What a bound tag is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Castle,
    /// Copy `k` of the deck list.
    Copy(u8),
}

/// The power-up's bindings: the castle, then copies in list order.
pub struct Registry {
    castle: Option<Uid>,
    copies: [Option<Uid>; MAX_COPIES],
    deck_len: usize,
}

/// A tag the registry has just seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    Known(Tag),
    /// Fresh, and now bound.
    Bound(Tag),
    /// Fresh, and every copy is bound already.
    Full,
}

impl Registry {
    pub const fn new(deck_len: usize) -> Self {
        Registry {
            castle: None,
            copies: [None; MAX_COPIES],
            deck_len: if deck_len < MAX_COPIES { deck_len } else { MAX_COPIES },
        }
    }

    /// Look a tag up, binding it if it is fresh (scry bridge `Registry.bind`).
    pub fn see(&mut self, uid: Uid) -> Seen {
        if self.castle == Some(uid) {
            return Seen::Known(Tag::Castle);
        }
        if let Some(k) = self.copies[..self.deck_len].iter().position(|&u| u == Some(uid)) {
            return Seen::Known(Tag::Copy(k as u8));
        }
        if self.castle.is_none() {
            self.castle = Some(uid);
            return Seen::Bound(Tag::Castle);
        }
        match self.copies[..self.deck_len].iter().position(|u| u.is_none()) {
            Some(k) => {
                self.copies[k] = Some(uid);
                Seen::Bound(Tag::Copy(k as u8))
            }
            None => Seen::Full,
        }
    }

    pub fn bound(&self) -> usize {
        usize::from(self.castle.is_some()) + self.copies.iter().filter(|u| u.is_some()).count()
    }
}

/// What the resolver reads of the seat. The station answers from the shrine's own state and its
/// engine (`tapstone_station/taps.rs`); the tests answer from a table.
pub trait Seat {
    /// The seat may act now: it owes a draw, or it is its turn in play.
    fn my_move(&self) -> bool;
    fn draw_owed(&self) -> bool;
    /// Copy `k` was drawn this shuffle and is still in hand.
    fn in_hand(&self, k: u8) -> bool;
    /// Copy `k` has been drawn since it was shuffled in (in hand, or played since).
    fn drawn(&self, k: u8) -> bool;
    /// A mulligan would be legal now.
    fn may_mulligan(&self) -> bool;
}

/// A move for the station to propose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Pass,
    Mulligan,
    Draw(u8),
    /// Cast copy `k` with 0009's defaults (the station picks the lane or target).
    Cast(u8),
    Charge(u8),
}

/// Why a tap did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    NotYourMove,
    /// A draw is owed, and this copy is not one that can be drawn.
    DrawFirst,
    /// This copy was played already.
    Played,
    /// Neither in hand nor drawable now.
    NotInHand,
    /// A fresh tag with every copy bound.
    Full,
}

/// One tap's (or tick's) result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    /// Propose these, in order (a lapsed window's move, then the new tap's).
    Moves([Option<Move>; 2]),
    /// A window opened: copy `k`'s cast-or-charge, or the castle's mulligan prompt.
    CastOrCharge(u8),
    MulliganPrompt,
    Refused(Why),
    Nothing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Card { k: u8, at: u64 },
    Castle { at: u64 },
}

/// The tap grammar's state: at most one open window.
pub struct Taps {
    pending: Option<Pending>,
}

impl Taps {
    pub const fn new() -> Self {
        Taps { pending: None }
    }

    /// Is a window open? (The host tests' probe; the station needs only the outputs.)
    #[cfg_attr(not(feature = "hostsim"), allow(dead_code))]
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// What an open window does when it closes without its second tap.
    fn lapse(&mut self) -> Option<Move> {
        match self.pending.take()? {
            Pending::Card { k, .. } => Some(Move::Cast(k)),
            Pending::Castle { .. } => Some(Move::Pass),
        }
    }

    /// Time passing: a window older than [`WINDOW_MS`] closes.
    pub fn tick(&mut self, now: u64) -> Out {
        let at = match self.pending {
            Some(Pending::Card { at, .. } | Pending::Castle { at }) => at,
            None => return Out::Nothing,
        };
        if now.saturating_sub(at) < WINDOW_MS {
            return Out::Nothing;
        }
        Out::Moves([self.lapse(), None])
    }

    /// A tap of `tag` (already through the registry) at `now`.
    pub fn tap(&mut self, now: u64, tag: Tag, seat: &impl Seat) -> Out {
        // The second tap of an open window.
        match (self.pending, tag) {
            (Some(Pending::Card { k, at }), Tag::Copy(t)) if t == k && now.saturating_sub(at) < WINDOW_MS => {
                self.pending = None;
                return Out::Moves([Some(Move::Charge(k)), None]);
            }
            (Some(Pending::Castle { at }), Tag::Castle) if now.saturating_sub(at) < WINDOW_MS => {
                self.pending = None;
                return Out::Moves([Some(Move::Mulligan), None]);
            }
            _ => {}
        }
        // Any other tap closes the window first, as its expiry would.
        let first = self.lapse();
        let then = |m: Move| Out::Moves([first, Some(m)]);
        let only = |o: Out| match first {
            Some(_) => Out::Moves([first, None]),
            None => o,
        };
        match tag {
            Tag::Castle => {
                if first.is_none() && seat.may_mulligan() {
                    self.pending = Some(Pending::Castle { at: now });
                    return Out::MulliganPrompt;
                }
                if !seat.my_move() {
                    return only(Out::Refused(Why::NotYourMove));
                }
                if seat.draw_owed() {
                    return only(Out::Refused(Why::DrawFirst));
                }
                then(Move::Pass)
            }
            Tag::Copy(k) => {
                if !seat.my_move() && first.is_none() {
                    return Out::Refused(Why::NotYourMove);
                }
                if seat.draw_owed() {
                    return if !seat.drawn(k) { then(Move::Draw(k)) } else { only(Out::Refused(Why::DrawFirst)) };
                }
                if seat.in_hand(k) {
                    if first.is_some() {
                        // The lapsed window's move goes first; this copy's window opens after it.
                        self.pending = Some(Pending::Card { k, at: now });
                        return Out::Moves([first, None]);
                    }
                    self.pending = Some(Pending::Card { k, at: now });
                    return Out::CastOrCharge(k);
                }
                only(Out::Refused(if seat.drawn(k) { Why::Played } else { Why::NotInHand }))
            }
        }
    }
}

impl Default for Taps {
    fn default() -> Self {
        Self::new()
    }
}
