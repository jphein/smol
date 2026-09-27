//! The shrine's side of a match, `no_std` and allocation-free: one object for the arena's desk
//! shrines, its tests, and the shrine firmware (tapstone#132 item c).
//!
//! This is `tapstone-arena`'s `DeskShrine` moved here unchanged in behaviour, so the firmware runs
//! the very seat the arena's tests run (verification.md, "one object"). What a seat *chooses* is a
//! [`Chooser`]: the desk plugs in `tapstone-sim`'s `ScriptedSeat`, the firmware its own picker or a
//! person's taps. Frames go out as [`Out`], `(dst, bytes)` pairs, the shape `DeskShrine` returned
//! as `Vec`s.
use heapless::Vec;
use tapstone_rules::state::DECK_MAX;
use tapstone_rules::{Game, HouseRules, Kind, Phase, Record};

use crate::follower::{Follower, OnBegin, OnCommit, OnHandback};
use crate::frame::{
    BROADCAST, Begin, FRAME_MAX, Frame, HANDBACK_RECORDS, Header, Join, Lobby, Tap, arena_refusal,
    join_role, lobby_flags,
};
use crate::ids::{deck_sigil, rules_id};

/// The arena's node id on the desk mesh. A shrine behind a gateway maps it to the arena gateway's
/// node when it sends (`tapstone-arena`'s `RadioShrine`).
pub const ARENA_NODE: u8 = 200;
/// The arena re-sends its head this often (`tapstone-arena` `core::play::HEAD_MS`; a test there
/// holds the two equal). A rejoining shrine waits one period caught up before it proposes.
pub const HEAD_MS: u64 = 1000;
/// Frames one call can emit: a `J`, or a lobby beacon and a claim, or a tap, or a reply.
pub const OUT_MAX: usize = 3;

pub type Bytes = Vec<u8, FRAME_MAX>;
/// Frames to send, as `(dst, bytes)`.
pub type Out = Vec<(u8, Bytes), OUT_MAX>;

/// Encode `f` from `src` in match `match_id`.
pub fn encode(src: u8, match_id: u32, f: &Frame) -> Bytes {
    let mut buf = [0u8; FRAME_MAX];
    let n = f.encode(&Header { match_id, src }, &mut buf);
    Vec::from_slice(&buf[..n]).expect("n <= FRAME_MAX")
}

fn push(out: &mut Out, dst: u8, bytes: Bytes) {
    // OUT_MAX is the most any path emits; a full Out is a bug in this file, not a runtime case.
    out.push((dst, bytes)).expect("OUT_MAX frames per call");
}

/// Shrine `shrine`'s synthetic tag UID for copy `k` of its deck list (desk-style, spec §5.2: "the
/// check runs on whatever UIDs the desk shrines send"). Byte 1 keeps these apart from the castle
/// figurines' `[4, 0, 0, 0, 0, 0, i]`.
pub fn copy_uid(shrine: usize, k: usize) -> [u8; 7] {
    [4, 0xC0 | shrine as u8, 0, 0, 0, (k >> 8) as u8, k as u8]
}

/// A tap by `seat`, everything the shrine does not choose left zero (`tapstone_sim::tap`).
pub fn tap(seat: u8, kind: Kind, card: u16, lane: i8, target: u8, aux: u8) -> Record {
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

/// What a seat plays when it is its move and no draw is owed.
pub trait Chooser {
    /// A `B` seated this shrine at `seat`.
    fn seated(&mut self, seat: u8);
    /// The tap to propose now. The engine arbitrates it; a refused tap costs a retry.
    fn next_tap(&mut self, g: &Game, seat: u8) -> Record;
}

/// Whether committed record `c` is the tap `p` proposed (every field the shrine chose).
pub fn same_tap(c: &Record, p: &Record) -> bool {
    c.seat == p.seat
        && c.kind == p.kind
        && c.uid == p.uid
        && c.lane == p.lane
        && c.target == p.target
        && c.aux == p.aux
        && (c.card == p.card || c.uid != [0; 7])
}

/// One shrine at a table: the reference follower plus a [`Chooser`] picking its taps. It holds its
/// own deck and nothing else: the seat map, the rules and the other list reach it only in the
/// arena's `B` (#67), exactly as they reach a real shrine.
pub struct Shrine<C> {
    /// This shrine's index: its node, its figurine and its copies' UIDs. Its SEAT comes from `B`.
    pub index: usize,
    pub seed: u64,
    pub node: u8,
    pub chooser: C,
    pub follower: Follower,
    pub deck: Vec<u16, DECK_MAX>,
    pub lseq: u16,
    pub pending: Option<(u16, u64)>,
    pub refused: u8,
    /// The castle design this shrine's player claims a seat with.
    pub castle: u16,
    /// Synthetic UIDs of this shrine's copies it has seen committed as drawn and not yet
    /// mulliganed back (0036: a copy is drawn once per shuffle-in, and the arena checks it).
    pub drawn: Vec<[u8; 7], DECK_MAX>,
    /// The designs in this seat's hand, in draw order, from its own committed records: a draw
    /// adds its card, a cast or a charge removes one copy of its card, a mulligan empties it.
    pub hand: Vec<u16, DECK_MAX>,
    /// The record of the pending proposal, to tell a true confirmation from a false one.
    pub pending_record: Option<Record>,
    /// STALE_LSEQ refusals received: a counter that went backwards, caught by the arena.
    pub stale_rejects: u32,
    /// Commits that cleared the pending tap while recording a DIFFERENT tap (#74), counted.
    pub false_confirms: u32,
    /// Rebooted and not yet re-synced: it proposes nothing until it has applied every mseq up to
    /// the highest it has heard in any commit.
    pub rejoining: bool,
    /// Rejoin by JOIN (a full replay from the arena) instead of by NAKing the gap.
    pub join: bool,
    /// The highest mseq heard in any commit.
    pub seen_head: Option<u16>,
    /// When the last JOIN went out.
    pub join_sent: Option<u64>,
    /// Resume above the seat's last committed lseq after a re-sync (ruled 2026-09-23). Off only in
    /// a test that shows the arena catching a counter that went backwards.
    pub resume_from_resync: bool,
    /// Play-tap proposals made (a test instrument; `DeskShrine` kept the lseqs themselves).
    pub proposals: u32,
    /// Since when a rejoining shrine has been caught up with the highest mseq it has heard.
    pub caught_up_since: Option<u64>,
    /// A rejoining shrine heard R: the match is over, and its head is final.
    pub heard_result: bool,
    /// How long a lobby claim waits before it is re-sent (100 ms).
    pub claim_retry_ms: u64,
    /// The match's `B` as received (#67), with its match id.
    pub begin: Option<(u32, Begin)>,
    /// Heard a commit before any `B`: the shrine asks for one with `J role=seat`.
    pub heard_unbegun: bool,
    /// Ask for a missing `B` (default on). Off only to show the arena's own retransmit suffices.
    pub ask_begin: bool,
    /// `J`s sent for a missing `B` (a test instrument).
    pub begin_asks: u32,
    /// Go back to the lobby and claim again once the match's RESULT is heard (a second match).
    pub rematch: bool,
    /// The finished match this shrine went back to the lobby from.
    pub lobby_after: Option<u32>,
    /// Name the copy on every cast and charge (`stamp`), as a real shrine does by tapping the card.
    /// A strict registry (gateway mode) resolves those taps by UID; a trusting one (desk mode)
    /// reads the design, so this is off there and every desk fixture keeps its bytes.
    pub stamp_uids: bool,
    /// The UIDs of this seat's copies in hand, in draw order: `hand`, by copy.
    pub hand_uids: Vec<[u8; 7], DECK_MAX>,
}

impl<C: Chooser> Shrine<C> {
    /// A shrine on `node` with figurine/copy index `index`, claiming with `castle`, holding `deck`.
    pub fn new(seed: u64, index: usize, node: u8, castle: u16, deck: &[u16], chooser: C) -> Self {
        Shrine {
            index,
            seed,
            node,
            chooser,
            follower: Follower::awaiting(),
            deck: Vec::from_slice(deck).expect("a deck of at most DECK_MAX"),
            lseq: 0,
            pending: None,
            refused: 0,
            castle,
            drawn: Vec::new(),
            hand: Vec::new(),
            pending_record: None,
            stale_rejects: 0,
            false_confirms: 0,
            rejoining: false,
            join: false,
            seen_head: None,
            join_sent: None,
            resume_from_resync: true,
            proposals: 0,
            caught_up_since: None,
            heard_result: false,
            claim_retry_ms: 100,
            begin: None,
            heard_unbegun: false,
            ask_begin: true,
            begin_asks: 0,
            rematch: false,
            lobby_after: None,
            stamp_uids: false,
            hand_uids: Vec::new(),
        }
    }

    /// The castle figurine this shrine's player taps to claim a seat (desk-synthetic).
    pub fn figurine(&self) -> [u8; 7] {
        [4, 0, 0, 0, 0, 0, self.index as u8]
    }

    /// This shrine's seat, from `B`'s seat map; `None` until a `B` that seats it arrives.
    pub fn seat(&self) -> Option<usize> {
        self.follower.seat_of(self.node).map(usize::from)
    }

    /// Resume from resync (ruled 2026-09-23): the counter never sits below the seat's last
    /// committed lseq, so the next tap is at least last + 1.
    fn resume(&mut self) {
        if let (true, Some(seat)) = (self.resume_from_resync, self.seat()) {
            self.lseq = self.lseq.max(self.follower.last_lseq()[seat]);
        }
    }

    /// Track this shrine's own copies through an applied record: drawn, or shuffled back.
    pub fn note_own(&mut self, r: &Record) {
        if self.seat() != Some(r.seat as usize) {
            return;
        }
        match r.kind {
            Kind::Draw => {
                if !self.drawn.contains(&r.uid) {
                    let _ = self.drawn.push(r.uid);
                }
                let _ = self.hand.push(r.card);
                let _ = self.hand_uids.push(r.uid);
            }
            // No seat mulligans after acting, so its hand is everything drawn.
            Kind::Mulligan => {
                self.drawn.clear();
                self.hand.clear();
                self.hand_uids.clear();
            }
            Kind::CastUnit | Kind::CastSpell | Kind::Charge => {
                if let Some(k) = self.hand.iter().position(|&c| c == r.card) {
                    self.hand.remove(k);
                }
                // The copy the record names, else (an unstamped tap) the first of its design.
                let k = self.hand_uids.iter().position(|&u| u == r.uid).or_else(|| {
                    self.hand_uids
                        .iter()
                        .position(|&u| self.design_of(u) == Some(r.card))
                });
                if let Some(k) = k {
                    self.hand_uids.remove(k);
                }
            }
            _ => {}
        }
    }

    /// The match is over: back to the lobby with a fresh per-match state (lseq, copies), keeping
    /// the follower, whose `B` guards still name the finished match.
    fn back_to_lobby(&mut self, match_id: u32) {
        self.lobby_after = Some(match_id);
        self.lseq = 0;
        self.pending = None;
        self.pending_record = None;
        self.refused = 0;
        self.drawn.clear();
        self.hand.clear();
        self.hand_uids.clear();
        self.seen_head = None;
        self.heard_result = false;
        self.heard_unbegun = false;
        self.rejoining = false;
    }

    /// The design of this shrine's copy `uid` (`copy_uid(index, k)` is `deck[k]`).
    pub fn design_of(&self, uid: [u8; 7]) -> Option<u16> {
        if uid[..5] != copy_uid(self.index, 0)[..5] {
            return None;
        }
        self.deck
            .get(usize::from(uid[5]) << 8 | usize::from(uid[6]))
            .copied()
    }

    /// The first copy of design `card` this shrine has not drawn since it was shuffled in.
    pub fn undrawn_copy(&self, card: u16) -> Option<usize> {
        (0..self.deck.len())
            .find(|&k| self.deck[k] == card && !self.drawn.contains(&copy_uid(self.index, k)))
    }

    /// With `stamp_uids`, give a cast or charge the UID of a copy of its design in hand.
    pub fn stamp(&self, tap: &mut Record) {
        let needs_card = matches!(tap.kind, Kind::CastUnit | Kind::CastSpell | Kind::Charge);
        if !self.stamp_uids || !needs_card || tap.uid != [0; 7] {
            return;
        }
        if let Some(&u) = self
            .hand_uids
            .iter()
            .find(|&&u| self.design_of(u) == Some(tap.card))
        {
            tap.uid = u;
        }
    }

    /// Take a match's `B`: build the game from it and play the seat it names.
    pub fn take_begin(&mut self, match_id: u32, b: &Begin) {
        if self.follower.on_begin(match_id, b, self.node) == OnBegin::Begun
            && let Some(seat) = self.seat()
        {
            self.begin = Some((match_id, *b));
            self.heard_unbegun = false;
            self.chooser.seated(seat as u8);
        }
    }

    /// Whether this seat may act now: it owes a draw (0036), or it is its turn in play.
    pub fn my_move(&self) -> bool {
        let Some(seat) = self.seat() else {
            return false;
        };
        let g = &self.follower.game;
        let owes = g.phase == Phase::Playing && g.seats[seat].owed_draws() > 0;
        let turn = g.phase == Phase::Playing && g.active == seat as u8;
        self.pending.is_none() && (owes || turn)
    }

    /// One scheduler tick. In the lobby: beacon the deck, and (when `may_claim`) tap the castle
    /// figurine, re-sending that claim every 100 ms with the same lseq until the match starts, so
    /// a lost claim cannot strand the lobby. In a match: the owed draws first (0036), then this
    /// seat's turn, one tap at a time with a 100 ms retransmit (protocol §4.3). `manual` makes no
    /// play taps. Returns the frames to send, as `(dst, bytes)`.
    pub fn act(&mut self, now: u64, may_claim: bool, manual: bool) -> Out {
        let i = self.index;
        let seat = self.seat();
        let node = self.node;
        let mut out = Out::new();
        // #101 (ruled 2026-09-27, option 2): a game that reached `Phase::Over` through a committed
        // record is over, so the shrine goes back to the lobby on that alone. `R` only carries the
        // result's details: the arena's linger closes once both seats ACK the final commit, which
        // does not prove either heard `R`.
        if self.rematch
            && self.follower.game.phase == Phase::Over
            && let Some(id) = self.follower.begun()
            && self.lobby_after != Some(id)
        {
            self.back_to_lobby(id);
        }
        if self.rejoining {
            // Caught up with the highest mseq heard is not caught up with the head: the first
            // commits heard may be an old retransmit (Oracle, on #75). So rejoin ends only once the
            // shrine has STAYED caught up for a head re-broadcast period, or once it has heard R.
            // A higher head heard in the meantime restarts the wait.
            let caught_up = self
                .seen_head
                .is_some_and(|h| self.follower.next_mseq() > h);
            if !caught_up {
                self.caught_up_since = None;
            } else {
                let since = *self.caught_up_since.get_or_insert(now);
                if self.heard_result || now.saturating_sub(since) >= HEAD_MS {
                    self.rejoining = false;
                }
            }
        }
        // A match is running that this shrine has no B for (#67; lost, or never heard): ask for it
        // the rejoin way, once a second. The answer is B and a full replay.
        let ask = self.heard_unbegun && self.ask_begin && !self.rejoining;
        if ask && self.join_sent.is_none_or(|t| now - t >= 1_000) {
            self.join_sent = Some(now);
            self.begin_asks += 1;
            let j = Frame::Join(Join {
                role: join_role::SEAT,
                have_mseq: 0,
            });
            push(&mut out, ARENA_NODE, encode(node, 0, &j));
        }
        if self.rejoining {
            // A rebooted shrine proposes nothing until it has re-synced. On the JOIN path it asks
            // the arena for a full replay, repeating every second until one arrives.
            if self.join && self.join_sent.is_none_or(|t| now - t >= 1_000) {
                self.join_sent = Some(now);
                // From what it already holds, so a repeated J resumes where the last one left off.
                let j = Frame::Join(Join {
                    role: join_role::SEAT,
                    have_mseq: self.follower.next_mseq(),
                });
                push(&mut out, ARENA_NODE, encode(node, 0, &j));
            }
            return out;
        }
        let back_in_lobby = self.lobby_after.is_some() && self.lobby_after == self.follower.begun();
        if self.follower.game.phase == Phase::Lobby || back_in_lobby {
            let lobby = Frame::Lobby(Lobby {
                seat_pref: i as u8,
                deck_sigil: deck_sigil(&self.deck),
                ruleset: 1,
                registry: 2,
                rules: rules_id(&HouseRules::default()),
                flags: lobby_flags::WANTS_MATCH,
            });
            let claim = Record {
                seq: 0,
                seat: i as u8,
                kind: Kind::ClaimSeat,
                card: self.castle,
                lane: -1,
                target: 0,
                aux: 0,
                time_ms: 0,
                uid: self.figurine(),
                auth: 0,
            };
            let due = self
                .pending
                .is_none_or(|(_, sent)| now - sent >= self.claim_retry_ms);
            let send_claim = may_claim && due;
            if send_claim {
                self.lseq = 1;
                self.pending = Some((1, now));
            }
            push(&mut out, BROADCAST, encode(node, 0, &lobby));
            if send_claim {
                let tap = Frame::Tap(Tap::Propose {
                    lseq: 1,
                    record: claim,
                });
                push(&mut out, ARENA_NODE, encode(node, 0, &tap));
            }
            return out;
        }
        if self.pending.is_some_and(|(l, _)| l == 1) && self.lseq == 1 {
            self.pending = None; // the claim was answered by the match starting
        }
        // Halted (§5): the shrine shows the split and proposes nothing more.
        if manual || self.follower.halted().is_some() {
            return out;
        }
        let Some(seat) = seat else {
            return out; // no B seats this shrine: it plays no seat
        };
        let g = &self.follower.game;
        // 0036: a seat that owes draws taps them first, from the top of its list. An opening hand
        // is owed off-turn too, so this comes before the turn check.
        let owes_draw = g.phase == Phase::Playing && g.seats[seat].owed_draws() > 0;
        if !owes_draw && (g.phase != Phase::Playing || g.active != (seat as u8)) {
            return out;
        }
        if let Some((_, sent)) = self.pending
            && now - sent < 100
        {
            return out; // retransmit at 100 ms (protocol §4.3)
        }
        let tap = if owes_draw {
            let c = g
                .top_of_list(seat as u8)
                .expect("a seat that owes has an undrawn copy");
            let k = self
                .undrawn_copy(c)
                .expect("the list's undrawn copy is a copy this shrine holds");
            let mut r = tap(seat as u8, Kind::Draw, c, -1, 0, 0);
            r.uid = copy_uid(i, k);
            r
        } else if self.refused >= 3 {
            self.refused = 0;
            tap(seat as u8, Kind::Pass, 0, -1, 0, 0)
        } else {
            let g = self.follower.game;
            let mut t = self.chooser.next_tap(&g, seat as u8);
            self.stamp(&mut t);
            t
        };
        self.send_tap(now, tap, &mut out);
        out
    }

    fn send_tap(&mut self, now: u64, tap: Record, out: &mut Out) {
        self.lseq += 1;
        self.pending = Some((self.lseq, now));
        self.pending_record = Some(tap);
        self.proposals += 1;
        let f = Frame::Tap(Tap::Propose {
            lseq: self.lseq,
            record: tap,
        });
        // A play tap names its match (protocol §2 header), so the arena drops one from any other.
        let id = self.follower.begun().unwrap_or(0);
        push(out, ARENA_NODE, encode(self.node, id, &f));
    }

    /// A tap its person chose (a manual seat), stamped and sent the way `act` sends its own. A draw
    /// gets the UID of the first copy of that design not yet drawn (0036), as in `act`. Sends
    /// nothing with no seat, with a tap still pending, or for a draw of a design this shrine holds
    /// no undrawn copy of.
    pub fn propose(&mut self, now: u64, mut tap: Record) -> Out {
        let mut out = Out::new();
        if self.seat().is_none() || self.pending.is_some() {
            return out;
        }
        if tap.kind == Kind::Draw {
            let Some(k) = self.undrawn_copy(tap.card) else {
                return out;
            };
            tap.uid = copy_uid(self.index, k);
        }
        self.stamp(&mut tap);
        self.send_tap(now, tap, &mut out);
        out
    }

    /// A commit or a tap reject heard from the mesh (anything else is ignored). Returns the
    /// replies to send: an ACK, a NAK for a gap, or an X for a hash split.
    pub fn rx(&mut self, h: &Header, f: &Frame) -> Out {
        let mut out = Out::new();
        let i = self.seat();
        match f {
            Frame::Begin(b) => self.take_begin(h.match_id, b),
            Frame::Commit(c) => {
                if self.follower.begun().is_some_and(|id| id != h.match_id) {
                    // Another match's commit (Oracle on #86): never acked as a duplicate. If this
                    // shrine's game is not in play, a new match may be running without its B: ask.
                    if self.follower.game.phase != Phase::Playing {
                        self.heard_unbegun = true;
                    }
                    return out;
                }
                self.seen_head = Some(self.seen_head.map_or(c.mseq, |h| h.max(c.mseq)));
                // Before B, this shrine's seat is unknown, but its own claim is still recognisable
                // by what it physically holds: the figurine it tapped.
                let mine = match i {
                    Some(s) => s == c.record.seat as usize,
                    None => c.record.kind == Kind::ClaimSeat && c.record.uid == self.figurine(),
                };
                if self.pending.is_some_and(|(l, _)| l == c.lseq) && mine {
                    if self
                        .pending_record
                        .is_some_and(|p| !same_tap(&c.record, &p))
                    {
                        self.false_confirms += 1;
                    }
                    self.pending = None;
                    self.pending_record = None;
                    self.refused = 0;
                }
                let reply = match self.follower.on_commit_in(h.match_id, c) {
                    OnCommit::Applied { ack } => {
                        self.resume();
                        self.note_own(&c.record);
                        Frame::Ack(ack)
                    }
                    OnCommit::Duplicate { ack } => Frame::Ack(ack),
                    // On the JOIN path the replay is asked for once, by J; the gap is not NAKed.
                    OnCommit::Gap { .. } if self.rejoining && self.join => return out,
                    OnCommit::Gap { nak } => Frame::Nak(nak),
                    OnCommit::Halt(x) => Frame::Halt(x),
                    OnCommit::Full => return out,
                    OnCommit::Unbegun => {
                        self.heard_unbegun = true;
                        return out;
                    }
                    OnCommit::OtherMatch => return out,
                };
                push(&mut out, ARENA_NODE, encode(self.node, h.match_id, &reply));
            }
            // The interim arbiter's replay (#76): records with their hashes and each seat's last lseq.
            Frame::Handback(hb) => {
                let before = self.follower.next_mseq() as usize;
                let first = hb.from_mseq as usize + hb.idx as usize * HANDBACK_RECORDS;
                if hb.idx + 1 == hb.count {
                    // The last chunk ends at the interim's head.
                    let head = (first + hb.n as usize).saturating_sub(1) as u16;
                    self.seen_head = Some(self.seen_head.map_or(head, |h| h.max(head)));
                }
                let result = self.follower.on_handback_in(h.match_id, hb);
                let after = self.follower.next_mseq() as usize;
                for k in before.max(first)..after.min(first + hb.n as usize) {
                    if let Some(r) = Record::decode(&hb.records[k - first]) {
                        self.note_own(&r);
                    }
                }
                match result {
                    OnHandback::Applied { .. } => self.resume(),
                    OnHandback::Halt(x) => push(
                        &mut out,
                        ARENA_NODE,
                        encode(self.node, h.match_id, &Frame::Halt(x)),
                    ),
                    _ => {} // a gap: the next J (once a second) asks from `next`
                }
            }
            Frame::Result(_) => {
                self.follower.on_result(h.match_id);
                // A match this shrine already left, on its final commit (#101): the details only.
                // Going back again would reset the claim in flight for the next match.
                if self.lobby_after == Some(h.match_id) {
                    return out;
                }
                self.heard_result = true;
                let for_mine = self.follower.begun() == Some(h.match_id);
                if self.rematch && for_mine {
                    self.back_to_lobby(h.match_id);
                }
            }
            Frame::Tap(Tap::Reject { lseq, reason })
                if self.pending.is_some_and(|(l, _)| l == *lseq) =>
            {
                self.pending = None;
                self.pending_record = None;
                if *reason == arena_refusal::STALE_LSEQ {
                    // The counter is behind the arena's: move past the refused lseq.
                    self.stale_rejects += 1;
                    self.lseq = self.lseq.max(*lseq);
                } else {
                    self.refused = self.refused.saturating_add(1);
                }
            }
            _ => {}
        }
        out
    }
}

/// A `no_std` seat that plays by itself: the shrine firmware's stand-in for a person until card
/// taps drive it (and the table's autoplay control). Every candidate is trial-applied to a COPY of
/// the game, as `tapstone-sim`'s menu does, so the engine, not this picker, decides what is legal.
/// Among the categories with a legal move it picks by `tapstone-sim`'s play-out weights (charge 40,
/// unit 30, spell 10, advance 10) and passes only when nothing else is legal. It never mulligans.
pub struct Autoplay {
    rng: u64,
}

impl Autoplay {
    pub fn new(seed: u64) -> Autoplay {
        Autoplay {
            rng: seed | 1, // xorshift must not start at 0
        }
    }

    fn roll(&mut self, n: u32) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x % u64::from(n)) as u32
    }
}

/// The first of `cands` the engine accepts from `g`.
fn first_legal(g: &Game, cands: impl IntoIterator<Item = Record>) -> Option<Record> {
    cands.into_iter().find(|r| {
        let mut probe = *g;
        probe.apply(r).is_ok()
    })
}

fn target_byte(seat: usize, lane: usize, cell: usize) -> u8 {
    ((seat as u8) << 4) | ((lane as u8) << 2) | cell as u8
}

impl Chooser for Autoplay {
    fn seated(&mut self, seat: u8) {
        self.rng ^= u64::from(seat) << 32 | 0x9E37;
    }

    fn next_tap(&mut self, g: &Game, seat: u8) -> Record {
        use tapstone_rules::state::{CELLS, LANES};
        use tapstone_rules::{CardKind, Effect, cards::design};
        let me_i = (seat & 1) as usize;
        let opp_i = 1 - me_i;
        let me = &g.seats[me_i];
        let hand = &me.hand[..me.hand_len()];
        let cells = |si: usize| (0..LANES).flat_map(move |l| (0..CELLS).map(move |c| (si, l, c)));
        let charge = first_legal(
            g,
            hand.iter().map(|&c| tap(seat, Kind::Charge, c, -1, 0, 0)),
        );
        let unit = first_legal(
            g,
            hand.iter().flat_map(|&c| {
                (0..LANES as i8).map(move |l| tap(seat, Kind::CastUnit, c, l, 0, 0))
            }),
        );
        let spell = first_legal(
            g,
            hand.iter().flat_map(|&c| {
                let kind = design(c).map(|d| d.kind);
                // Aim at the other side (heal: at your own), castle first where a spell may hit it.
                let castle = matches!(
                    kind,
                    Some(CardKind::Spell(Effect::Damage {
                        castle_ok: true,
                        ..
                    }))
                );
                let side = match kind {
                    Some(CardKind::Spell(Effect::Heal { .. })) => me_i,
                    _ => opp_i,
                };
                let untargeted = matches!(kind, Some(CardKind::Spell(Effect::Draw { .. })));
                castle
                    .then(|| tap(seat, Kind::CastSpell, c, -1, 0xFF, 0))
                    .into_iter()
                    .chain(untargeted.then(|| tap(seat, Kind::CastSpell, c, -1, 0, 0)))
                    .chain(
                        cells(side)
                            .filter(move |&(si, l, cc)| g.seats[si].cells[l][cc].is_some())
                            .map(move |(si, l, cc)| {
                                tap(seat, Kind::CastSpell, c, -1, target_byte(si, l, cc), 0)
                            }),
                    )
            }),
        );
        let advance = first_legal(
            g,
            (0..LANES as i8).map(|l| tap(seat, Kind::Advance, 0, l, 0, 0)),
        );
        let live = [(40u32, charge), (30, unit), (10, spell), (10, advance)];
        let total: u32 = live
            .iter()
            .filter(|(_, r)| r.is_some())
            .map(|(w, _)| w)
            .sum();
        if total == 0 {
            return tap(seat, Kind::Pass, 0, -1, 0, 0);
        }
        let mut roll = self.roll(total);
        for (w, r) in live {
            if let Some(r) = r {
                if roll < w {
                    return r;
                }
                roll -= w;
            }
        }
        tap(seat, Kind::Pass, 0, -1, 0, 0)
    }
}
