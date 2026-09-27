//! The `SMOLv1 MATCH ` family. Header 20 B: tag 13 · ver 1 · kind 1 · match 4 (LE) · src 1.
use tapstone_rules::state::{DECK_MAX, SEATS};
use tapstone_rules::{Game, HouseRules, Record, Refusal};

use crate::wire::{Reader, Writer};

pub const TAG: &[u8; 13] = b"SMOLv1 MATCH ";
pub const VER: u8 = 1;
pub const HEADER_LEN: usize = 20;
/// smol `net/wire.rs`: `ESP_NOW_MTU = 250`; `send_to` appends a 9 B group-MAC trailer.
pub const ESP_NOW_MTU: usize = 250;
pub const MAC_TRAILER: usize = 9;
pub const PAYLOAD_MAX: usize = ESP_NOW_MTU - HEADER_LEN - MAC_TRAILER;
pub const FRAME_MAX: usize = HEADER_LEN + PAYLOAD_MAX;

/// The one budget predicate every size check uses, so its control (222 must fail) covers them all.
pub const fn fits_payload(n: usize) -> bool {
    n <= PAYLOAD_MAX
}
pub const BROADCAST: u8 = 255;
pub const SNAP_DATA_MAX: usize = 200;
pub const HANDBACK_RECORDS: usize = 6;
pub const GRID_MAX: usize = 12;
pub const NONE16: u16 = 0xFFFF;

/// `B` at its largest: rules 9 · seat nodes 2 · genesis 8, then per seat a list length and up to
/// `DECK_MAX` designs (#67). Derived from the engine's own bounds, so a bigger deck moves it.
pub const BEGIN_MAX: usize = 9 + SEATS + 8 + SEATS * (1 + 2 * DECK_MAX); // = 141
const _: () = assert!(
    fits_payload(BEGIN_MAX),
    "BEGIN no longer fits one frame: split it per seat (#67)"
);
pub const NONE8: u8 = 0xFF;

pub mod lobby_flags {
    pub const WANTS_MATCH: u8 = 1 << 0;
    pub const SPECTATORS: u8 = 1 << 1;
    pub const WIFI: u8 = 1 << 2;
    /// Arena spec §6.1: set only by the arena.
    pub const ARENA: u8 = 1 << 3;
}
pub mod join_role {
    pub const SEAT: u8 = 0;
    pub const SPECTATOR: u8 = 1;
    pub const ARENA: u8 = 2;
}
pub mod halt_reason {
    pub const HASH: u8 = 0;
    /// The follower's engine refused a record the arbiter committed.
    pub const REFUSED: u8 = 1;
    pub const HANDBACK: u8 = 2;
}
pub mod result_reason {
    pub const LETHAL: u8 = 0;
    pub const STOP: u8 = 1;
    pub const TIMEOUT: u8 = 2;
    pub const DESYNC: u8 = 3;
}
pub mod equip_op {
    pub const LOOT: u8 = 0;
    pub const CARD: u8 = 1;
    pub const CLEAR: u8 = 2;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub match_id: u32,
    pub src: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lobby {
    pub seat_pref: u8,
    pub deck_sigil: u32,
    pub ruleset: u32,
    pub registry: u32,
    pub rules: u32,
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tap {
    Propose { lseq: u16, record: Record },
    Reject { lseq: u16, reason: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    pub mseq: u16,
    pub lseq: u16,
    pub record: Record,
    /// Chain head after this record; all zero for lobby records (the chain starts at `Started`).
    pub hash: [u8; 8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    pub mseq: u16,
    pub hash: [u8; 8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nak {
    pub from: u16,
    /// `0xFFFF` = to head.
    pub to: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Join {
    pub role: u8,
    pub have_mseq: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snap {
    pub at_mseq: u16,
    pub idx: u8,
    pub count: u8,
    pub total_len: u16,
    pub len: u8,
    pub data: [u8; SNAP_DATA_MAX],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapNak {
    pub at_mseq: u16,
    pub bitmap: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Halt {
    pub at_mseq: u16,
    pub reason: u8,
    pub mine: [u8; 8],
    pub theirs: [u8; 8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchResult {
    pub final_mseq: u16,
    pub winner: u8,
    pub reason: u8,
    pub chain: [u8; 8],
    pub transcript_sha: [u8; 32],
    /// 0 unsigned · 1 HMAC-SHA256 group key (32 B) · 2 Ed25519 (64 B), protocol §7.
    pub sig_kind: u8,
    pub sig: [u8; 64],
}

impl MatchResult {
    pub fn unsigned(
        final_mseq: u16,
        winner: u8,
        reason: u8,
        chain: [u8; 8],
        sha: [u8; 32],
    ) -> Self {
        MatchResult {
            final_mseq,
            winner,
            reason,
            chain,
            transcript_sha: sha,
            sig_kind: 0,
            sig: [0; 64],
        }
    }
    pub fn sig_len(kind: u8) -> Option<usize> {
        match kind {
            0 => Some(0),
            1 => Some(32),
            2 => Some(64),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Doll {
    pub seat: u8,
    pub level: u8,
    pub xp: u16,
    pub xp_next: u16,
    pub slots: u8,
    pub loadout: [u16; 3],
    pub inv_len: u8,
    pub inv: [u16; GRID_MAX],
    pub keyword: u8,
    pub name_seed: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Equip {
    pub slot: u8,
    pub op: u8,
    pub design: u16,
    pub uid: [u8; 7],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handback {
    pub from_mseq: u16,
    pub idx: u8,
    pub count: u8,
    pub n: u8,
    /// Each seat's highest committed lseq at the interim arbiter, so a revived arena dedupes
    /// retransmits of taps it only learns of here (ruled 2026-09-23).
    pub last_lseq: [u16; 2],
    pub records: [[u8; 32]; HANDBACK_RECORDS],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandbackNak {
    pub from_mseq: u16,
    pub bitmap: u64,
}

/// BEGIN (#67): everything a shrine needs, beyond the committed records, to build the `Game` the
/// arbiter built and so reach its genesis hash: the house rules, the seat map and both deck lists.
/// The commanders and castles ride in the two `ClaimSeat` records, as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Begin {
    /// `HouseRules::bytes()`, the nine bytes genesis hashes.
    pub rules: [u8; 9],
    /// The mesh node in each seat, seat order (claim order, spec §5.1).
    pub nodes: [u8; SEATS],
    /// The arbiter's `h_0`, so a follower that built a different game halts at the record that
    /// starts the chain instead of at the first hashed one. All zero = not stated.
    pub genesis: [u8; 8],
    pub lens: [u8; SEATS],
    /// Each seat's list in the arbiter's order. The order means nothing to the rules (0036), but
    /// sending it as held keeps both engines' lists identical rather than merely equal as multisets.
    pub decks: [[u16; DECK_MAX]; SEATS],
}

impl Begin {
    /// `decks` longer than `DECK_MAX` are clamped, as `Game::new` clamps them.
    pub fn new(
        rules: &HouseRules,
        nodes: [u8; SEATS],
        genesis: [u8; 8],
        decks: [&[u16]; SEATS],
    ) -> Begin {
        let mut b = Begin {
            rules: rules.bytes(),
            nodes,
            genesis,
            lens: [0; SEATS],
            decks: [[0; DECK_MAX]; SEATS],
        };
        for (s, d) in decks.iter().enumerate() {
            let n = d.len().min(DECK_MAX);
            b.decks[s][..n].copy_from_slice(&d[..n]);
            b.lens[s] = n as u8;
        }
        b
    }

    pub fn deck(&self, seat: usize) -> &[u16] {
        &self.decks[seat][..self.lens[seat] as usize]
    }

    pub fn house_rules(&self) -> HouseRules {
        HouseRules::from_bytes(self.rules)
    }

    /// The seat `node` holds, if it holds one.
    pub fn seat_of(&self, node: u8) -> Option<u8> {
        self.nodes.iter().position(|&n| n == node).map(|s| s as u8)
    }

    /// The lobby game both engines start from. Castles are placeholders: each `ClaimSeat` sets its
    /// seat's castle design before anything is hashed.
    pub fn game(&self) -> Game {
        Game::new(self.house_rules(), [0; SEATS], [self.deck(0), self.deck(1)])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Begin(Begin),
    Lobby(Lobby),
    Tap(Tap),
    Commit(Commit),
    Ack(Ack),
    Nak(Nak),
    Join(Join),
    Snap(Snap),
    SnapNak(SnapNak),
    Halt(Halt),
    Result(MatchResult),
    Doll(Doll),
    Equip(Equip),
    Handback(Handback),
    HandbackNak(HandbackNak),
}

impl Frame {
    pub fn kind(&self) -> u8 {
        match self {
            Frame::Begin(_) => b'B',
            Frame::Lobby(_) => b'L',
            Frame::Tap(_) => b'T',
            Frame::Commit(_) => b'C',
            Frame::Ack(_) => b'A',
            Frame::Nak(_) => b'N',
            Frame::Join(_) => b'J',
            Frame::Snap(_) => b'S',
            Frame::SnapNak(_) => b'Q',
            Frame::Halt(_) => b'X',
            Frame::Result(_) => b'R',
            Frame::Doll(_) => b'D',
            Frame::Equip(_) => b'E',
            Frame::Handback(_) => b'H',
            Frame::HandbackNak(_) => b'K',
        }
    }

    /// Encode header + body into `out`; returns the frame length (≤ `FRAME_MAX`).
    pub fn encode(&self, h: &Header, out: &mut [u8; FRAME_MAX]) -> usize {
        let mut w = Writer::new(out);
        w.bytes(TAG);
        w.u8(VER);
        w.u8(self.kind());
        w.u32(h.match_id);
        w.u8(h.src);
        match self {
            Frame::Begin(b) => {
                w.bytes(&b.rules);
                w.bytes(&b.nodes);
                w.bytes(&b.genesis);
                for s in 0..SEATS {
                    w.u8(b.lens[s]);
                    for &c in b.deck(s) {
                        w.u16(c);
                    }
                }
            }
            Frame::Lobby(l) => {
                w.u8(l.seat_pref);
                w.u32(l.deck_sigil);
                w.u32(l.ruleset);
                w.u32(l.registry);
                w.u32(l.rules);
                w.u8(l.flags);
            }
            Frame::Tap(Tap::Propose { lseq, record }) => {
                w.u8(0);
                w.u16(*lseq);
                w.bytes(&record.encode());
            }
            Frame::Tap(Tap::Reject { lseq, reason }) => {
                w.u8(1);
                w.u16(*lseq);
                w.u8(*reason);
            }
            Frame::Commit(c) => {
                w.u16(c.mseq);
                w.u16(c.lseq);
                w.bytes(&c.record.encode());
                w.bytes(&c.hash);
            }
            Frame::Ack(a) => {
                w.u16(a.mseq);
                w.bytes(&a.hash);
            }
            Frame::Nak(n) => {
                w.u16(n.from);
                w.u16(n.to);
            }
            Frame::Join(j) => {
                w.u8(j.role);
                w.u16(j.have_mseq);
            }
            Frame::Snap(s) => {
                w.u16(s.at_mseq);
                w.u8(s.idx);
                w.u8(s.count);
                w.u16(s.total_len);
                w.bytes(&s.data[..s.len as usize]);
            }
            Frame::SnapNak(q) => {
                w.u16(q.at_mseq);
                w.u64(q.bitmap);
            }
            Frame::Halt(x) => {
                w.u16(x.at_mseq);
                w.u8(x.reason);
                w.bytes(&x.mine);
                w.bytes(&x.theirs);
            }
            Frame::Result(r) => {
                w.u16(r.final_mseq);
                w.u8(r.winner);
                w.u8(r.reason);
                w.bytes(&r.chain);
                w.bytes(&r.transcript_sha);
                w.u8(r.sig_kind);
                let n = MatchResult::sig_len(r.sig_kind).unwrap_or(0);
                w.bytes(&r.sig[..n]);
            }
            Frame::Doll(d) => {
                w.u8(d.seat);
                w.u8(d.level);
                w.u16(d.xp);
                w.u16(d.xp_next);
                w.u8(d.slots);
                for s in d.loadout {
                    w.u16(s);
                }
                w.u8(d.inv_len);
                for i in d.inv {
                    w.u16(i);
                }
                w.u8(d.keyword);
                w.u32(d.name_seed);
            }
            Frame::Equip(e) => {
                w.u8(e.slot);
                w.u8(e.op);
                w.u16(e.design);
                w.bytes(&e.uid);
            }
            Frame::Handback(hb) => {
                w.u16(hb.from_mseq);
                w.u8(hb.idx);
                w.u8(hb.count);
                w.u8(hb.n);
                w.u16(hb.last_lseq[0]);
                w.u16(hb.last_lseq[1]);
                for r in &hb.records[..hb.n as usize] {
                    w.bytes(r);
                }
            }
            Frame::HandbackNak(k) => {
                w.u16(k.from_mseq);
                w.u64(k.bitmap);
            }
        }
        w.len()
    }

    /// Decode one frame. `None` for a foreign prefix, a wrong version, an unknown kind, or a body
    /// shorter than its kind requires. Trailing bytes are ignored (SNK's length-tolerance rule).
    pub fn decode(b: &[u8]) -> Option<(Header, Frame)> {
        let mut r = Reader::new(b);
        if r.take(TAG.len())? != TAG || r.u8()? != VER {
            return None;
        }
        let kind = r.u8()?;
        let h = Header {
            match_id: r.u32()?,
            src: r.u8()?,
        };
        let f = match kind {
            b'B' => {
                let (rules, nodes, genesis) = (r.array()?, r.array()?, r.array()?);
                let mut b = Begin {
                    rules,
                    nodes,
                    genesis,
                    lens: [0; SEATS],
                    decks: [[0; DECK_MAX]; SEATS],
                };
                for s in 0..SEATS {
                    let n = r.u8()?;
                    if n as usize > DECK_MAX {
                        return None;
                    }
                    b.lens[s] = n;
                    for c in b.decks[s].iter_mut().take(n as usize) {
                        *c = r.u16()?;
                    }
                }
                Frame::Begin(b)
            }
            b'L' => Frame::Lobby(Lobby {
                seat_pref: r.u8()?,
                deck_sigil: r.u32()?,
                ruleset: r.u32()?,
                registry: r.u32()?,
                rules: r.u32()?,
                flags: r.u8()?,
            }),
            b'T' => match r.u8()? {
                0 => Frame::Tap(Tap::Propose {
                    lseq: r.u16()?,
                    record: Record::decode(r.take(Record::LEN)?)?,
                }),
                1 => Frame::Tap(Tap::Reject {
                    lseq: r.u16()?,
                    reason: r.u8()?,
                }),
                _ => return None,
            },
            b'C' => Frame::Commit(Commit {
                mseq: r.u16()?,
                lseq: r.u16()?,
                record: Record::decode(r.take(Record::LEN)?)?,
                hash: r.array()?,
            }),
            b'A' => Frame::Ack(Ack {
                mseq: r.u16()?,
                hash: r.array()?,
            }),
            b'N' => Frame::Nak(Nak {
                from: r.u16()?,
                to: r.u16()?,
            }),
            b'J' => Frame::Join(Join {
                role: r.u8()?,
                have_mseq: r.u16()?,
            }),
            b'S' => {
                let (at_mseq, idx, count, total_len) = (r.u16()?, r.u8()?, r.u8()?, r.u16()?);
                let rest = r.take(b.len().saturating_sub(HEADER_LEN + 6).min(SNAP_DATA_MAX))?;
                let mut data = [0u8; SNAP_DATA_MAX];
                data[..rest.len()].copy_from_slice(rest);
                Frame::Snap(Snap {
                    at_mseq,
                    idx,
                    count,
                    total_len,
                    len: rest.len() as u8,
                    data,
                })
            }
            b'Q' => Frame::SnapNak(SnapNak {
                at_mseq: r.u16()?,
                bitmap: r.u64()?,
            }),
            b'X' => Frame::Halt(Halt {
                at_mseq: r.u16()?,
                reason: r.u8()?,
                mine: r.array()?,
                theirs: r.array()?,
            }),
            b'R' => {
                let (final_mseq, winner, reason) = (r.u16()?, r.u8()?, r.u8()?);
                let (chain, transcript_sha) = (r.array()?, r.array()?);
                let sig_kind = r.u8()?;
                let n = MatchResult::sig_len(sig_kind)?;
                let mut sig = [0u8; 64];
                sig[..n].copy_from_slice(r.take(n)?);
                Frame::Result(MatchResult {
                    final_mseq,
                    winner,
                    reason,
                    chain,
                    transcript_sha,
                    sig_kind,
                    sig,
                })
            }
            b'D' => {
                let (seat, level, xp, xp_next, slots) =
                    (r.u8()?, r.u8()?, r.u16()?, r.u16()?, r.u8()?);
                let loadout = [r.u16()?, r.u16()?, r.u16()?];
                let inv_len = r.u8()?;
                if inv_len as usize > GRID_MAX {
                    return None;
                }
                let mut inv = [0u16; GRID_MAX];
                for slot in &mut inv {
                    *slot = r.u16()?;
                }
                Frame::Doll(Doll {
                    seat,
                    level,
                    xp,
                    xp_next,
                    slots,
                    loadout,
                    inv_len,
                    inv,
                    keyword: r.u8()?,
                    name_seed: r.u32()?,
                })
            }
            b'E' => Frame::Equip(Equip {
                slot: r.u8()?,
                op: r.u8()?,
                design: r.u16()?,
                uid: r.array()?,
            }),
            b'H' => {
                let (from_mseq, idx, count, n) = (r.u16()?, r.u8()?, r.u8()?, r.u8()?);
                let last_lseq = [r.u16()?, r.u16()?];
                if n as usize > HANDBACK_RECORDS {
                    return None;
                }
                let mut records = [[0u8; 32]; HANDBACK_RECORDS];
                for rec in records.iter_mut().take(n as usize) {
                    *rec = r.array()?;
                }
                Frame::Handback(Handback {
                    from_mseq,
                    idx,
                    count,
                    n,
                    last_lseq,
                    records,
                })
            }
            b'K' => Frame::HandbackNak(HandbackNak {
                from_mseq: r.u16()?,
                bitmap: r.u64()?,
            }),
            _ => return None,
        };
        Some((h, f))
    }
}

/// Wire code for an engine refusal in `T sub=1` (1..=17), in `Refusal`'s declaration order.
/// Arena-level refusals use 100 and up (`arena_refusal`).
pub fn refusal_code(r: &Refusal) -> u8 {
    match r {
        Refusal::NotYourTurn => 1,
        Refusal::NotInHand => 2,
        Refusal::NoMana { .. } => 3,
        Refusal::CellOccupied => 4,
        Refusal::AlreadyChargedThisRound => 5,
        Refusal::AlreadyAdvancedLane => 6,
        Refusal::BadTarget => 7,
        Refusal::UnknownCard => 8,
        Refusal::GameOver => 9,
        Refusal::LaneOutOfRange => 10,
        Refusal::NotPlaying => 11,
        Refusal::SeatTaken => 12,
        Refusal::MulliganClosed => 13,
        Refusal::LobbyClosed => 14,
        // 0036
        Refusal::DrawOwed => 15,
        Refusal::NoDrawOwed => 16,
        Refusal::NotInDeck => 17,
    }
}

pub mod arena_refusal {
    pub const UNKNOWN_UID: u8 = 100;
    pub const NOT_SEATED: u8 = 101;
    pub const UNKNOWN_DECK: u8 = 102;
    pub const TABLE_FULL: u8 = 103;
    pub const BAD_LOADOUT: u8 = 104;
    pub const NOT_IN_LOBBY: u8 = 105;
    /// 0036: this UID was already drawn since it was last shuffled in (the arena's rule; the
    /// engine never sees a UID).
    pub const COPY_DRAWN: u8 = 106;
    /// The lseq is already used for this seat and the proposal is not the tap committed under it (a
    /// rebooted shrine's counter): re-sync and resume above the seat's last committed lseq.
    pub const STALE_LSEQ: u8 = 107;
}
