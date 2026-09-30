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
    BROADCAST, Begin, FRAME_MAX, Frame, HANDBACK_RECORDS, Handback, Header, Join, Lobby, Nak, Tap,
    arena_refusal, join_role, lobby_flags, refusal_code,
};
use crate::ids::{deck_sigil, rules_id};

/// The arena's node id on the desk mesh. A shrine behind a gateway maps it to the arena gateway's
/// node when it sends (`tapstone-arena`'s `RadioShrine`).
pub const ARENA_NODE: u8 = 200;
/// The arena re-sends its head this often (`tapstone-arena` `core::play::HEAD_MS`; a test there
/// holds the two equal). A rejoining shrine waits one period caught up before it proposes.
pub const HEAD_MS: u64 = 1000;
/// Frames one base call can emit: a `J`, or a lobby beacon and a claim, or a tap, or a reply. The
/// routed entry points (`act_to`, `rx_to`, `propose_to`) emit through a sink instead, because an
/// interim's hand-back is as many chunks as the log needs.
pub const OUT_MAX: usize = 3;
/// Arena-dark detection (lead ruling 2026-09-27, arena spec §7 step 1): no frame from the arena for
/// this long. The arena re-broadcasts its head every [`HEAD_MS`] in play, so this is three missed
/// heads, the idiom of protocol §4.5's 3 s `PEER_STALE`.
pub const DARK_MS: u64 = 3_000;
/// How long the interim waits for seat 1 to catch it up before arbitrating anyway (#98).
pub const SYNC_BOUND_MS: u64 = 1_000;
/// The interim re-sends its catch-up `N` this often while it waits (#98).
pub const SYNC_RETRY_MS: u64 = 100;

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
    /// The time of the last `act_to` call: the clock `rx_to` runs on (commit `time_ms`, the
    /// catch-up's retry and bound, the arena's last-heard time).
    pub now: u64,
    /// The arena-dark state and the interim arbiter's (arena spec §7, #76, #91, #98).
    pub dark: Dark,
}

/// The arena-dark window, from this shrine's side (arena spec §7). When the arena goes dark, seat 0's
/// shrine (the first claimed, 0006/0029) is the **interim arbiter**: it arbitrates its own taps in
/// place and seat 1's, answers a seat's `J`/`N` from its own log (#76), catches up from seat 1 before
/// its first commit (#98), hands its log back to the revived arena (`H`, `K`), and returns to
/// follower on the arena's first commit (step 5). Seat 1 re-addresses what it sent the arena to seat
/// 0's node. This was `tapstone-arena/tests/harness.rs`'s `Net` until 2026-09-27; it lives here so
/// the shrine firmware runs the interim the tests run (spec D3).
pub struct Dark {
    /// The arena is dark, as this shrine sees it.
    pub on: bool,
    /// Detect dark from the arena's silence ([`DARK_MS`]). Off: only [`Shrine::go_dark`] starts a
    /// dark window (the test harness's god's-eye death of the arena).
    pub detect: bool,
    /// When this shrine last heard the arena.
    pub last_arena: Option<u64>,
    /// smol#558: a shrine with no seat (it rebooted and kept nothing, or never heard `B`) cannot
    /// detect dark from silence, having no match in play to be silent about. Its `J` for a `B` is
    /// the probe instead: one nobody answers for [`DARK_MS`] means the arena is dark. Off: the
    /// `J` goes to the dead arena and the seat stalls until it revives.
    pub discovers: bool,
    /// When this shrine, holding no `B` at all, first asked for one. Written and read only while
    /// `begin` is `None`, and a `B` once taken is never dropped (a reboot builds a fresh shrine),
    /// so the stamp a seatless rejoin leaves behind is never read again: a rematch's `J` for a
    /// lost `B` (#67) cannot find it stale and declare dark (`tests/dark_seatless.rs`).
    pub asked_begin: Option<u64>,
    /// While dark, the interim answers a seat's `J` and `N` (#76). Off: the deferral this replaced.
    pub answers: bool,
    /// #98: before its first commit in a dark window the interim asks seat 1 (`N`) for any record
    /// past its own head and adopts the `H` answer. Off: the interim arbitrates at once.
    pub syncs: bool,
    /// The interim has caught up with seat 1 this dark window (or gave up waiting).
    pub synced: bool,
    /// When the interim first and last asked seat 1 to catch it up, this dark window.
    pub sync_asked: Option<(u64, u64)>,
    /// Taps that reached the interim while it waited, one per seat in arrival order.
    pub sync_held: Vec<(u16, Record), 2>,
    /// The highest lseq the interim committed per seat, this match.
    pub lseq: [u16; 2],
    /// Test controls: flip one card of the rebuilt `B` (#76), zero `H.last_lseq` to a seat (#76),
    /// flip one byte of one carried hash in the hand-back to the arena.
    pub corrupt_begin: bool,
    pub zero_lseq: bool,
    pub corrupt_handback: bool,
    /// Instruments: `J`/`N` answers sent; rejects sent to seat 1; taps, and seats' `J`/`N`, that
    /// reached an interim with no game (#91); records adopted from seat 1's catch-up; frames the
    /// interim addressed to the (dark) arena anyway.
    pub replies: u32,
    pub rejects: u32,
    pub gameless_taps: u32,
    pub gameless_asks: u32,
    pub sync_adopted: u32,
    pub to_arena: u32,
}

impl Dark {
    pub const fn new() -> Dark {
        Dark {
            on: false,
            detect: true,
            last_arena: None,
            discovers: true,
            asked_begin: None,
            answers: true,
            syncs: true,
            synced: false,
            sync_asked: None,
            sync_held: Vec::new(),
            lseq: [0; 2],
            corrupt_begin: false,
            zero_lseq: false,
            corrupt_handback: false,
            replies: 0,
            rejects: 0,
            gameless_taps: 0,
            gameless_asks: 0,
            sync_adopted: 0,
            to_arena: 0,
        }
    }
}

impl Default for Dark {
    fn default() -> Dark {
        Dark::new()
    }
}

/// A frame sink: `(dst, bytes)`.
pub type Emit<'a> = dyn FnMut(u8, &[u8]) + 'a;

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
            now: 0,
            dark: Dark::new(),
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
    fn act_base(&mut self, now: u64, may_claim: bool, manual: bool) -> Out {
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
            if self.begin.is_none() {
                self.dark.asked_begin.get_or_insert(now);
            }
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
                if self.begin.is_none() {
                    self.dark.asked_begin.get_or_insert(now);
                }
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
    fn propose_base(&mut self, now: u64, mut tap: Record) -> Out {
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
    fn follow(&mut self, h: &Header, f: &Frame) -> Out {
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

/// The routed entry points and the interim arbiter (arena spec §7).
impl<C: Chooser> Shrine<C> {
    /// One scheduler tick, routed: [`Shrine::act`]'s frames, with the dark window applied (the
    /// interim arbitrates its own tap in place; seat 1 re-addresses to the interim).
    pub fn act_to(&mut self, now: u64, may_claim: bool, manual: bool, emit: &mut Emit<'_>) {
        self.now = now;
        self.detect_dark(now);
        let out = self.act_base(now, may_claim, manual);
        for (dst, bytes) in out.iter() {
            self.route(*dst, bytes, true, emit);
        }
    }

    /// A tap its person chose, routed as [`Shrine::act_to`] routes the scripted ones.
    pub fn propose_to(&mut self, now: u64, tap: Record, emit: &mut Emit<'_>) -> bool {
        self.now = now;
        let out = self.propose_base(now, tap);
        let sent = !out.is_empty();
        for (dst, bytes) in out.iter() {
            self.route(*dst, bytes, true, emit);
        }
        sent
    }

    /// A frame heard from the mesh, routed: the follower's replies, and the interim's side of a
    /// dark window. The branch order is the harness's, which the dark tests pinned.
    pub fn rx_to(&mut self, h: &Header, f: &Frame, emit: &mut Emit<'_>) {
        if self.arena_sent(h, f) {
            self.dark.last_arena = Some(self.now);
        }
        let seat = self.seat();
        let interim = self.interim_role();
        let dark = self.dark.on;
        let peer = self.peer_node();
        match f {
            Frame::Commit(_) => {
                // The first commit from the revived arena is the handover (step 5). Only a shrine
                // holding `B` can tell it from the interim's own commits (the seat map names the
                // interim's node); a seatless one stays dark until it can.
                if dark && self.interim_node().is_some_and(|n| h.src != n) {
                    self.dark.on = false;
                }
                self.follow_to(h, f, emit);
            }
            // Seat 1's tap reaches the interim arbiter while the arena is dark.
            Frame::Tap(Tap::Propose { lseq, record }) if interim && dark => {
                self.interim_commit(*lseq, *record, emit);
            }
            // ...or a would-be interim with no game, which arbitrates nothing (#91).
            Frame::Tap(Tap::Propose { .. }) if seat.is_none() && dark => {
                self.dark.gameless_taps += 1;
            }
            // The revived arena asks for the gap: every hand-back chunk.
            Frame::Join(j) if interim && j.role == join_role::ARENA => {
                self.send_handback(j.have_mseq, u64::MAX, ARENA_NODE, true, emit);
            }
            // It NAKs the chunks it is missing.
            Frame::HandbackNak(k) if interim => {
                self.send_handback(k.from_mseq, k.bitmap, ARENA_NODE, true, emit);
            }
            // #76: a seat's J (a reboot) is answered with B rebuilt from this state, then H chunks.
            Frame::Join(j)
                if interim
                    && dark
                    && self.dark.answers
                    && j.role == join_role::SEAT
                    && self.begin.is_some() =>
            {
                self.dark.replies += 1;
                let Some(mut b) = self.follower.begin_frame() else {
                    return;
                };
                if self.dark.corrupt_begin {
                    b.decks[1][0] ^= 0x0001;
                }
                let id = self.begin.map_or(0, |(id, _)| id);
                emit(h.src, &encode(self.node, id, &Frame::Begin(b)));
                self.send_handback(j.have_mseq, u64::MAX, h.src, true, emit);
            }
            // #76: a seat's N (a gap) gets the same chunks from `from`.
            Frame::Nak(n)
                if interim
                    && dark
                    && self.dark.answers
                    && Some(h.src) == peer
                    && self.begin.is_some() =>
            {
                self.dark.replies += 1;
                self.send_handback(n.from, u64::MAX, h.src, true, emit);
            }
            // #98: the interim's catch-up N: seat 1 answers with what it holds from `from`.
            Frame::Nak(n) if seat == Some(1) && dark && Some(h.src) == self.interim_node() => {
                if self.follower.handback(n.from, 0).is_some() {
                    self.send_handback(n.from, u64::MAX, h.src, false, emit);
                } else if self.follower.begun().is_some() {
                    // Seat 1 is behind the interim: nothing past `from`, said as one empty chunk.
                    let hb = Handback {
                        from_mseq: n.from,
                        idx: 0,
                        count: 1,
                        n: 0,
                        last_lseq: self.follower.last_lseq(),
                        records: [[0; 32]; HANDBACK_RECORDS],
                    };
                    let id = self.begin.map_or(0, |(id, _)| id);
                    emit(h.src, &encode(self.node, id, &Frame::Handback(hb)));
                }
            }
            // ...and the interim adopts the answer, then arbitrates from the common head.
            Frame::Handback(hb) if interim && dark && Some(h.src) == peer && !self.dark.synced => {
                let before = self.follower.next_mseq();
                self.follow_to(h, f, emit);
                let after = self.follower.next_mseq();
                self.dark.sync_adopted += u32::from(after - before);
                let end =
                    hb.from_mseq as usize + hb.idx as usize * HANDBACK_RECORDS + hb.n as usize;
                if hb.idx + 1 == hb.count && after as usize >= end {
                    self.dark.synced = true;
                    let held = core::mem::take(&mut self.dark.sync_held);
                    for (lseq, r) in held {
                        self.interim_commit(lseq, r, emit);
                    }
                }
            }
            // #91: a gameless would-be interim has nothing to answer a seat's J or N from.
            Frame::Join(_) | Frame::Nak(_)
                if seat.is_none() && dark && !self.arena_sent(h, f) && self.begin.is_none() =>
            {
                self.dark.gameless_asks += 1;
            }
            Frame::Tap(Tap::Reject { .. })
            | Frame::Result(_)
            | Frame::Begin(_)
            | Frame::Handback(_) => {
                self.follow_to(h, f, emit);
            }
            _ => {}
        }
    }

    /// The arena process died (the harness's god's-eye view, or [`DARK_MS`] of silence): a fresh
    /// dark window. Nothing is pending, so every seat's next tap goes out on the new route at once.
    pub fn go_dark(&mut self) {
        self.dark.on = true;
        self.dark.synced = false;
        self.dark.sync_asked = None;
        self.dark.sync_held.clear();
        self.pending = None;
    }

    /// The arena is back (its first commit, step 5).
    pub fn end_dark(&mut self) {
        self.dark.on = false;
    }

    /// Enter the dark window on [`DARK_MS`] of the arena's silence, in a match this shrine plays.
    /// A shrine with no `B` has no match in play to hear silence in, and it cannot tell the
    /// arena's commits from the interim's (it has no seat map), so its probe is its own `J`: one
    /// nobody has answered with a `B` for [`DARK_MS`] means the arena is dark (smol#558). Dark,
    /// its `J` goes out broadcast (`route`), the interim answers it with a rebuilt `B` (#76), and
    /// that `B` names its seat and the interim's node. If the arena was only slow, its answer
    /// arrives the same way, and its first commit ends the window (step 5).
    fn detect_dark(&mut self, now: u64) {
        if !self.dark.detect || self.dark.on {
            return;
        }
        let playing = self.follower.begun().is_some() && self.follower.game.phase == Phase::Playing;
        let silent = playing
            && self
                .dark
                .last_arena
                .is_some_and(|t| now.saturating_sub(t) >= DARK_MS);
        let unanswered = self.dark.discovers
            && self.begin.is_none()
            && self
                .dark
                .asked_begin
                .is_some_and(|t| now.saturating_sub(t) >= DARK_MS);
        if silent || unanswered {
            self.go_dark();
        }
    }

    /// Seat 0's shrine is the interim. A shrine with no seat (it never heard `B`, #91, or rebooted
    /// and kept nothing, #76) knows neither its own seat nor the interim's node: it arbitrates
    /// nothing, broadcasts what it meant for the arena (the interim, or a revived arena, hears it),
    /// and counts what reached it as a would-be interim.
    fn interim_role(&self) -> bool {
        self.seat() == Some(0)
    }

    /// Seat 0's node, from `B`'s seat map: where seat 1 sends what it meant for the arena.
    fn interim_node(&self) -> Option<u8> {
        self.follower.begun().map(|_| self.follower.nodes()[0])
    }

    /// The other seat's node.
    fn peer_node(&self) -> Option<u8> {
        let seat = self.seat()?;
        Some(self.follower.nodes()[1 - seat])
    }

    /// Did the arena send this? The arena heads its frames with its own node (200 on a desk
    /// mesh, its gateway's node over the radio), never a seat's, and only it sends these kinds.
    fn arena_sent(&self, h: &Header, f: &Frame) -> bool {
        let seat_node = self.follower.begun().is_some() && self.follower.nodes().contains(&h.src);
        if seat_node {
            return false;
        }
        match f {
            Frame::Lobby(l) => l.seat_pref == 0xFF,
            Frame::Join(j) => j.role == join_role::ARENA,
            Frame::Begin(_)
            | Frame::Commit(_)
            | Frame::Result(_)
            | Frame::HandbackNak(_)
            | Frame::Doll(_)
            | Frame::Tap(Tap::Reject { .. }) => true,
            _ => false,
        }
    }

    /// Where a frame this shrine addressed to the arena goes while the arena is dark.
    fn route(&mut self, dst: u8, bytes: &[u8], from_act: bool, emit: &mut Emit<'_>) {
        if !(self.dark.on && dst == ARENA_NODE) {
            return emit(dst, bytes);
        }
        if self.seat().is_none() {
            // Seatless: the interim's node is unknown (see `interim_role`).
            if from_act {
                self.dark.to_arena += 1;
            }
            return emit(BROADCAST, bytes);
        }
        if self.interim_role() {
            if !from_act {
                return emit(dst, bytes);
            }
            // Only a tap is the interim's to arbitrate. Anything else it addresses to the arena
            // (its `J` for a `B` it never received, #91) goes on the air to the arena as ever:
            // nobody hears it while the arena is dead, and the revived arena does.
            match Frame::decode(bytes) {
                Some((_, Frame::Tap(Tap::Propose { lseq, record }))) => {
                    self.interim_commit(lseq, record, emit)
                }
                _ => {
                    self.dark.to_arena += 1;
                    emit(dst, bytes);
                }
            }
        } else if let Some(n) = self.interim_node() {
            emit(n, bytes);
        }
    }

    /// The follower's own handling, its replies routed.
    fn follow_to(&mut self, h: &Header, f: &Frame, emit: &mut Emit<'_>) {
        let out = self.follow(h, f);
        for (dst, bytes) in out.iter() {
            self.route(*dst, bytes, false, emit);
        }
    }

    /// Arbitrate a tap as the interim: its own, or seat 1's (arena spec §7, #76, #98).
    fn interim_commit(&mut self, lseq: u16, r: Record, emit: &mut Emit<'_>) {
        if self.begin.is_none() {
            self.dark.gameless_taps += 1;
            return;
        }
        if !self.interim_caught_up(emit) {
            // Held, and arbitrated the moment seat 1's answer lands (the proposer's retransmit
            // would otherwise cost 100 ms); a retransmit meanwhile replaces its seat's.
            self.dark.sync_held.retain(|(_, h)| h.seat != r.seat);
            let _ = self.dark.sync_held.push((lseq, r));
            return;
        }
        let now = self.now as u32;
        let seat = r.seat;
        // Dedupe retransmits by (seat, lseq), exactly as the arena does, from every lseq seen
        // committed: the interim's own AND the ones it followed before the arena went dark
        // (a rebooted seat's restarted counter got through otherwise, #76).
        let seen = self.follower.last_lseq()[(seat & 1) as usize];
        if self.dark.lseq[(seat & 1) as usize].max(seen) >= lseq {
            return;
        }
        let id = self.begin.map_or(0, |(id, _)| id);
        match self.follower.arbitrate(r, lseq, now) {
            Ok(c) => {
                self.dark.lseq[(seat & 1) as usize] = lseq;
                emit(BROADCAST, &encode(self.node, id, &Frame::Commit(c)));
                if Some(seat as usize) == self.seat() {
                    // The interim never hears its own commits back, so it notes its own taps here:
                    // without it a copy drawn while dark was drawn again after the revival, and the
                    // arena refused it forever (COPY_DRAWN, #76).
                    self.pending = None;
                    self.note_own(&c.record);
                }
            }
            Err(e) => {
                let reject = Frame::Tap(Tap::Reject {
                    lseq,
                    reason: refusal_code(&e),
                });
                if Some(seat as usize) == self.seat() {
                    self.pending = None;
                    self.refused = self.refused.saturating_add(1);
                } else if let Some(p) = self.peer_node() {
                    self.dark.rejects += 1;
                    emit(p, &encode(self.node, 0, &reject));
                }
            }
        }
    }

    /// #98: whether the interim may arbitrate yet. Before its first commit in a dark window it sends
    /// seat 1 an `N` from its own next mseq, every [`SYNC_RETRY_MS`], and seat 1 answers with `H`
    /// chunks of whatever it holds past that (none: one empty chunk). A seat 1 that never answers
    /// is waited for [`SYNC_BOUND_MS`], then the interim arbitrates as before.
    fn interim_caught_up(&mut self, emit: &mut Emit<'_>) -> bool {
        if !self.dark.syncs || self.dark.synced {
            return true;
        }
        let now = self.now;
        let (first, last) = *self.dark.sync_asked.get_or_insert((now, 0));
        if now.saturating_sub(first) >= SYNC_BOUND_MS {
            self.dark.synced = true;
            return true;
        }
        if last == 0 || now.saturating_sub(last) >= SYNC_RETRY_MS {
            self.dark.sync_asked = Some((first, now.max(1)));
            let from = self.follower.next_mseq();
            let id = self.begin.map_or(0, |(id, _)| id);
            if let Some(p) = self.peer_node() {
                emit(
                    p,
                    &encode(self.node, id, &Frame::Nak(Nak { from, to: from })),
                );
            }
        }
        false
    }

    /// `H` chunks of this shrine's log from `from`, the chunks `bitmap` names, to `dst`. `as_interim`:
    /// the interim's hand-back (the test controls apply), or seat 1 answering a catch-up.
    fn send_handback(
        &mut self,
        from: u16,
        bitmap: u64,
        dst: u8,
        as_interim: bool,
        emit: &mut Emit<'_>,
    ) {
        let Some(first) = self.follower.handback(from, 0) else {
            return;
        };
        let id = self.begin.map_or(0, |(id, _)| id);
        for idx in 0..first.count {
            if idx < 64 && bitmap & (1 << idx) == 0 {
                continue;
            }
            let Some(mut hb) = self.follower.handback(from, idx) else {
                continue;
            };
            if as_interim && self.dark.corrupt_handback && idx == first.count - 1 {
                hb.records[0][30] ^= 0x01; // one byte of one carried hash
            }
            if as_interim && self.dark.zero_lseq && dst != ARENA_NODE {
                hb.last_lseq = [0; 2];
            }
            emit(dst, &encode(self.node, id, &Frame::Handback(hb)));
        }
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
