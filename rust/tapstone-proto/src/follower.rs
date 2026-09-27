//! A reference follower (arena spec D3): what a shrine does with `C` frames, and how it arbitrates
//! while the arena is dark. `no_std`, no alloc, fixed capacity.
use tapstone_rules::state::{DECK_MAX, SEATS};
use tapstone_rules::{Applied, Chain, Game, HouseRules, Kind, Phase, Record, Refusal};

use crate::frame::{Ack, Begin, Commit, HANDBACK_RECORDS, Halt, Handback, Nak, halt_reason};
use crate::transcript::{RECORD_LEN, record_bytes};

/// 256 records × 32 B = 8 KB: a stop-round Duel with margin (goldens run 48–80 records).
pub const LOG_CAP: usize = 256;

/// The follower's RAM budget on the shrine: 8.5 KiB. Measured 8,584 B on xtensa-esp32s3 and
/// thumbv7em (8,592 on x86_64) on 2026-09-23; 8,588 B on both after the hand-back's per-seat lseq
/// (+4 B, plan Task 14); 8,616 B on both (8,624 on x86_64) after `B`'s match id, seat map and
/// genesis (+28 B, #67, 2026-09-25); 8,624 B (8,632 on x86_64) with the last-left match id (+8 B,
/// the stale-`B` guard, same day). Each by a failing `[(); 0] = [(); size_of::<Follower>()]`
/// probe on each target's compiler. Asserted here, so every target's compiler evaluates it
/// (verification.md's size rule, the same shape as `Game`'s 350 B).
pub const FOLLOWER_BUDGET: usize = 8704;
const _: () = assert!(
    core::mem::size_of::<Follower>() <= FOLLOWER_BUDGET,
    "size_of::<Follower>() exceeds the shrine's 8.5 KiB follower budget on this target"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnCommit {
    Applied {
        ack: Ack,
    },
    Duplicate {
        ack: Ack,
    },
    Gap {
        nak: Nak,
    },
    Halt(Halt),
    /// The log is full; the follower cannot keep the transcript it owes the arena.
    Full,
    /// No `B` yet (#67): the follower has no game to apply this to. The shrine asks the arbiter
    /// with `J role=seat`, which is answered with `B` and a full replay.
    Unbegun,
    /// The frame's header names a match other than the one this follower's game was built for:
    /// dropped. A follower whose game is still the last one's must not answer the next match's
    /// commits as duplicates (an ack the arena would take as proof the seat heard its `B`).
    OtherMatch,
}

/// What a follower did with one `H` chunk (#76: the interim arbiter's replay to a rebooted seat).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnHandback {
    /// Applied every record in it that was new; `next` is the mseq the follower now expects.
    Applied {
        next: u16,
    },
    /// The chunk starts past the next mseq: an earlier chunk is missing. Ask again from `next`.
    Gap {
        next: u16,
    },
    Halt(Halt),
    Full,
    Unbegun,
    OtherMatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnBegin {
    /// The game is built from this `B`; the follower now waits for the claim commits.
    Begun,
    /// A retransmit of the `B` already taken, or one for another match while this one is live.
    Ignored,
    /// This node holds no seat in it: a `B` for someone else's match.
    NotSeated,
    /// The match this follower left for a newer one: a late copy of its `B`, never re-taken.
    Stale,
}

pub struct Follower {
    pub game: Game,
    chain: Option<Chain>,
    log: [[u8; RECORD_LEN]; LOG_CAP],
    log_len: usize,
    halted: Option<Halt>,
    /// Each seat's highest committed lseq (its tap counter), for the hand-back: a revived arena
    /// dedupes retransmits by it (ruled 2026-09-23). 4 B, where a per-record lseq would cost 512 B.
    last_lseq: [u16; 2],
    /// The match whose `B` built `game`; `None` until one arrives (#67). `Follower::new` is begun
    /// by fiat, with match 0.
    begun: Option<u32>,
    /// The seat map from `B`.
    nodes: [u8; 2],
    /// The arbiter's `h_0` as `B` stated it (all zero: not stated).
    want_genesis: [u8; 8],
    /// This follower's own `h_0`, once the chain has started.
    genesis: Option<[u8; 8]>,
    /// The last match whose game this follower PLAYED (its chain left the Lobby) and then left for
    /// a newer `B`. Its `B` may still be in flight and must not pull the follower back. Only a
    /// started game counts: recording a match that never started let a stray `B` make the LIVE
    /// match "left", refusing its `B` until a reboot (Oracle on #86). (Not an ordering on ids:
    /// `(node << 24) ^ unix` is not monotone across the XOR, so "at or below" would be a guess.)
    left: Option<u32>,
    /// The begun match's RESULT was heard: its game is over whatever the local engine last applied
    /// (a follower that missed the final commit still reads Playing).
    ended: bool,
}

impl Follower {
    /// A follower whose game is given, not received: the sim's and the unit tests' shortcut.
    pub fn new(game: Game) -> Follower {
        Follower {
            game,
            chain: None,
            log: [[0; RECORD_LEN]; LOG_CAP],
            log_len: 0,
            halted: None,
            last_lseq: [0; 2],
            begun: Some(0),
            nodes: [0; 2],
            want_genesis: [0; 8],
            genesis: None,
            left: None,
            ended: false,
        }
    }

    /// A shrine's follower before its match's `B`: it knows no seat map, no rules and no lists,
    /// and applies nothing until `on_begin` (#67).
    pub fn awaiting() -> Follower {
        let mut f = Follower::new(Game::new(HouseRules::default(), [0; 2], [&[], &[]]));
        f.begun = None;
        f
    }

    /// Build the game from `B`, for the shrine at mesh node `me`. Taken once per match, and only
    /// for a match `me` is seated in. A `B` for another match replaces the game only while no game
    /// is being played, and never for the match this follower last left (Oracle on #86: a late
    /// `B(N)` after `B(N+1)` would otherwise pull it back and void N+1).
    pub fn on_begin(&mut self, match_id: u32, b: &Begin, me: u8) -> OnBegin {
        if b.seat_of(me).is_none() {
            return OnBegin::NotSeated;
        }
        let live = self.live();
        match self.begun {
            Some(id) if id == match_id => return OnBegin::Ignored,
            Some(_) if live => return OnBegin::Ignored,
            _ if self.left == Some(match_id) => return OnBegin::Stale,
            _ => {}
        }
        if self.game.phase != Phase::Lobby {
            self.left = self.begun;
        }
        // Reset in place: a `Follower` is 8.5 KB, too big to build a second one on a shrine's stack.
        self.game = b.game();
        self.chain = None;
        self.log_len = 0;
        self.halted = None;
        self.last_lseq = [0; 2];
        self.begun = Some(match_id);
        self.nodes = b.nodes;
        self.want_genesis = b.genesis;
        self.genesis = None;
        self.ended = false;
        OnBegin::Begun
    }

    /// Whether a game is in play that a `B` for another match must not reset. Halted is not in play
    /// (§5: it stops applying, while the engine still reads Playing), and neither is a game whose
    /// RESULT was heard. Before this, a halted follower, or one that missed its last commit,
    /// ignored every later match's `B` until a reboot.
    pub fn live(&self) -> bool {
        self.game.phase == Phase::Playing && self.halted.is_none() && !self.ended
    }

    /// The RESULT for `match_id` was heard. If it is the begun match, its game is over.
    pub fn on_result(&mut self, match_id: u32) {
        if self.begun == Some(match_id) {
            self.ended = true;
        }
    }

    /// The match this follower's game was built for, once a `B` (or `new`) gave it one.
    pub fn begun(&self) -> Option<u32> {
        self.begun
    }

    /// The seat `node` holds, from `B`'s seat map.
    pub fn seat_of(&self, node: u8) -> Option<u8> {
        self.begun?;
        self.nodes.iter().position(|&n| n == node).map(|s| s as u8)
    }

    /// This follower's own genesis hash, once the chain has started.
    pub fn genesis(&self) -> Option<[u8; 8]> {
        self.genesis
    }

    /// Each seat's highest committed lseq.
    pub fn last_lseq(&self) -> [u16; 2] {
        self.last_lseq
    }

    /// The next `mseq` this follower will apply: the engine's own `seq`.
    pub fn next_mseq(&self) -> u16 {
        self.game.seq
    }

    /// Chain head, zero before genesis (lobby records carry no hash).
    pub fn head_hash(&self) -> [u8; 8] {
        self.chain.map_or([0; 8], |c| c.head())
    }

    pub fn records(&self) -> &[[u8; RECORD_LEN]] {
        &self.log[..self.log_len]
    }

    pub fn halted(&self) -> Option<Halt> {
        self.halted
    }

    fn hash_at(&self, mseq: u16) -> [u8; 8] {
        let mut h = [0u8; 8];
        if let Some(r) = self
            .log
            .get(mseq as usize)
            .filter(|_| (mseq as usize) < self.log_len)
        {
            h.copy_from_slice(&r[Record::LEN..]);
        }
        h
    }

    /// Apply `r` to the engine and chain; returns the hash to record (zero for lobby records).
    fn step(&mut self, r: &Record) -> Result<[u8; 8], Refusal> {
        let applied = self.game.apply(r)?;
        Ok(if applied == Applied::Started {
            let c = Chain::genesis(&self.game);
            self.chain = Some(c);
            self.genesis = Some(c.head());
            [0; 8]
        } else if let Some(c) = self.chain.as_mut() {
            c.step(r, &self.game);
            c.head()
        } else {
            [0; 8]
        })
    }

    fn note_lseq(&mut self, seat: u8, lseq: u16) {
        let l = &mut self.last_lseq[(seat & 1) as usize];
        *l = (*l).max(lseq);
    }

    fn push(&mut self, r: &Record, hash: [u8; 8]) {
        let lobby = hash == [0; 8];
        self.log[self.log_len] = record_bytes(r, (!lobby).then_some(hash));
        self.log_len += 1;
    }

    /// `on_commit` for a `C` whose header names `match_id`: a commit from any other match than the
    /// one this follower's game was built for is `OtherMatch`, and changes nothing.
    pub fn on_commit_in(&mut self, match_id: u32, c: &Commit) -> OnCommit {
        match self.begun {
            None => OnCommit::Unbegun,
            Some(id) if id != match_id => OnCommit::OtherMatch,
            Some(_) => self.on_commit(c),
        }
    }

    pub fn on_commit(&mut self, c: &Commit) -> OnCommit {
        if let Some(x) = self.halted {
            return OnCommit::Halt(x);
        }
        if self.begun.is_none() {
            return OnCommit::Unbegun;
        }
        let next = self.next_mseq();
        if c.mseq < next {
            return OnCommit::Duplicate {
                ack: Ack {
                    mseq: c.mseq,
                    hash: self.hash_at(c.mseq),
                },
            };
        }
        if c.mseq > next {
            return OnCommit::Gap {
                nak: Nak {
                    from: next,
                    to: c.mseq - 1,
                },
            };
        }
        if self.log_len == LOG_CAP {
            return OnCommit::Full;
        }
        let mine = self.head_hash();
        let started = self.chain.is_none();
        match self.step(&c.record) {
            Err(_) => self.halt(c.mseq, halt_reason::REFUSED, mine, c.hash),
            Ok(h) if h != c.hash => self.halt(c.mseq, halt_reason::HASH, h, c.hash),
            // The record that started the chain: this engine's h_0 must be the one B stated (#67),
            // or it built a different game (a list, a rule or a seat) and halts before hashing on.
            Ok(_) if started && self.genesis_differs() => {
                let (mine, want) = (self.genesis.unwrap_or([0; 8]), self.want_genesis);
                self.halt(c.mseq, halt_reason::HASH, mine, want)
            }
            Ok(h) => {
                self.push(&c.record, h);
                self.note_lseq(c.record.seat, c.lseq);
                OnCommit::Applied {
                    ack: Ack {
                        mseq: c.mseq,
                        hash: h,
                    },
                }
            }
        }
    }

    fn genesis_differs(&self) -> bool {
        self.want_genesis != [0; 8] && self.genesis.is_some_and(|g| g != self.want_genesis)
    }

    fn halt(&mut self, at_mseq: u16, reason: u8, mine: [u8; 8], theirs: [u8; 8]) -> OnCommit {
        let x = Halt {
            at_mseq,
            reason,
            mine,
            theirs,
        };
        self.halted = Some(x);
        OnCommit::Halt(x)
    }

    /// Interim arbiter (arena dark, 0028/0029): stamp `seq` and `time_ms`, apply, and commit.
    pub fn arbitrate(&mut self, mut r: Record, lseq: u16, time_ms: u32) -> Result<Commit, Refusal> {
        if self.log_len == LOG_CAP {
            return Err(Refusal::GameOver);
        }
        r.seq = self.game.seq;
        r.time_ms = time_ms;
        let h = self.step(&r)?;
        self.push(&r, h);
        self.note_lseq(r.seat, lseq);
        Ok(Commit {
            mseq: r.seq,
            lseq,
            record: r,
            hash: h,
        })
    }

    /// The `B` this follower's game was built from, rebuilt from its own state (#76), so an interim
    /// arbiter can answer a rebooted seat's `J` without storing a 141 B frame it has no budget for.
    /// Each seat's original list is its undrawn list, plus its hand, plus every card its committed
    /// records took out of a hand: only `Charge`, `CastUnit` and `CastSpell` do, each exactly
    /// `record.card`, and only when applied. Draws and mulligans move cards between list and hand.
    /// The order differs from the original; the rules and genesis never read it (0036). `None`
    /// before a `B`, or if the arithmetic ever overflows a list (which would be an engine change).
    pub fn begin_frame(&self) -> Option<Begin> {
        self.begun?;
        let mut lists = [[0u16; DECK_MAX]; SEATS];
        let mut lens = [0usize; SEATS];
        let mut put = |seat: usize, card: u16| -> Option<()> {
            let n = lens.get_mut(seat)?;
            *lists[seat].get_mut(*n)? = card;
            *n += 1;
            Some(())
        };
        for (s, seat) in self.game.seats.iter().enumerate() {
            for &c in seat.deck[..seat.deck_len as usize]
                .iter()
                .chain(&seat.hand[..seat.hand_len as usize])
            {
                put(s, c)?;
            }
        }
        for bytes in self.records() {
            let r = Record::decode(bytes)?;
            if matches!(r.kind, Kind::Charge | Kind::CastUnit | Kind::CastSpell) {
                put((r.seat & 1) as usize, r.card)?;
            }
        }
        let genesis = self.genesis.unwrap_or(self.want_genesis);
        Some(Begin::new(
            &self.game.rules,
            self.nodes,
            genesis,
            [&lists[0][..lens[0]], &lists[1][..lens[1]]],
        ))
    }

    /// `on_handback` for an `H` whose header names `match_id`: another match's chunk is ignored.
    pub fn on_handback_in(&mut self, match_id: u32, hb: &Handback) -> OnHandback {
        match self.begun {
            None => OnHandback::Unbegun,
            Some(id) if id != match_id => OnHandback::OtherMatch,
            Some(_) => self.on_handback(hb),
        }
    }

    /// Apply one `H` chunk (#76): the interim arbiter's replay of its record list, the hand-back's
    /// shape because its records carry no lseq. Each new record goes through `on_commit`, so it is
    /// checked against its carried hash exactly as a `C` is; then the chunk's per-seat `last_lseq`
    /// is taken, which is where a rebooted seat's counter resumes (ruled 2026-09-23).
    pub fn on_handback(&mut self, hb: &Handback) -> OnHandback {
        if let Some(x) = self.halted {
            return OnHandback::Halt(x);
        }
        if self.begun.is_none() {
            return OnHandback::Unbegun;
        }
        let first = hb.from_mseq as usize + hb.idx as usize * HANDBACK_RECORDS;
        for (k, bytes) in hb.records[..(hb.n as usize).min(HANDBACK_RECORDS)]
            .iter()
            .enumerate()
        {
            let mseq = (first + k) as u16;
            let next = self.next_mseq();
            if mseq < next {
                continue; // already applied; a record past `next` is a gap, which on_commit reports
            }
            let Some(record) = Record::decode(bytes) else {
                return OnHandback::Gap { next };
            };
            let mut hash = [0u8; 8];
            hash.copy_from_slice(&bytes[Record::LEN..]);
            let c = Commit {
                mseq,
                lseq: 0,
                record,
                hash,
            };
            match self.on_commit(&c) {
                OnCommit::Applied { .. } => {}
                OnCommit::Halt(x) => return OnHandback::Halt(x),
                OnCommit::Full => return OnHandback::Full,
                _ => return OnHandback::Gap { next },
            }
        }
        for s in 0..SEATS {
            self.note_lseq(s as u8, hb.last_lseq[s]);
        }
        OnHandback::Applied {
            next: self.next_mseq(),
        }
    }

    /// Chunk `idx` of the records from `from_mseq` to the head, six per chunk (spec §6.3).
    pub fn handback(&self, from_mseq: u16, idx: u8) -> Option<Handback> {
        let tail = self.records().get(from_mseq as usize..)?;
        let count = tail.len().div_ceil(HANDBACK_RECORDS).max(1);
        if idx as usize >= count {
            return None;
        }
        let chunk = tail
            .chunks(HANDBACK_RECORDS)
            .nth(idx as usize)
            .unwrap_or(&[]);
        let mut records = [[0u8; 32]; HANDBACK_RECORDS];
        records[..chunk.len()].copy_from_slice(chunk);
        Some(Handback {
            from_mseq,
            idx,
            count: count as u8,
            n: chunk.len() as u8,
            last_lseq: self.last_lseq,
            records,
        })
    }
}
