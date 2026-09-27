//! TSX1: protocol §6's binary transcript. Header, then `mseq`-ordered 32 B records (24 B event
//! record + 8 B chain head, zero for lobby records). `transcript_sha` = SHA-256(header ‖ records),
//! the value `R` carries (§4.8).
use sha2::{Digest, Sha256};
use tapstone_rules::Record;

use crate::wire::{Reader, Writer};

pub const MAGIC: &[u8; 4] = b"TSX1";
pub const HEADER_LEN: usize = 36;
pub const RECORD_LEN: usize = Record::LEN + 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptHeader {
    pub match_id: u32,
    pub ruleset: u32,
    pub registry: u32,
    pub rules: u32,
    pub seat_nodes: [u8; 2],
    pub deck_sigils: [u32; 2],
    pub start_ts: u32,
}

impl TranscriptHeader {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        let mut w = Writer::new(&mut out);
        w.bytes(MAGIC);
        w.u32(self.match_id);
        w.u32(self.ruleset);
        w.u32(self.registry);
        w.u32(self.rules);
        w.u8(self.seat_nodes[0]);
        w.u8(self.seat_nodes[1]);
        w.u32(self.deck_sigils[0]);
        w.u32(self.deck_sigils[1]);
        w.u32(self.start_ts);
        w.u16(0);
        debug_assert_eq!(w.len(), HEADER_LEN);
        out
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut r = Reader::new(b);
        if r.take(4)? != MAGIC {
            return None;
        }
        let h = TranscriptHeader {
            match_id: r.u32()?,
            ruleset: r.u32()?,
            registry: r.u32()?,
            rules: r.u32()?,
            seat_nodes: [r.u8()?, r.u8()?],
            deck_sigils: [r.u32()?, r.u32()?],
            start_ts: r.u32()?,
        };
        r.u16()?;
        Some(h)
    }
}

pub fn record_bytes(r: &Record, hash: Option<[u8; 8]>) -> [u8; RECORD_LEN] {
    let mut out = [0u8; RECORD_LEN];
    out[..Record::LEN].copy_from_slice(&r.encode());
    out[Record::LEN..].copy_from_slice(&hash.unwrap_or([0; 8]));
    out
}

pub fn transcript_sha(header: &[u8; HEADER_LEN], records: &[[u8; RECORD_LEN]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(header);
    for r in records {
        h.update(r);
    }
    h.finalize().into()
}
