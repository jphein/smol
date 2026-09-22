//! The 24-byte event record. Layout fixed by the phase 1 plan (2026-09-20); docs/protocol/tapstone-protocol-draft.md §3 is being brought in line with it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    ClaimSeat = 1,
    Mulligan = 2,
    Charge = 3,
    CastUnit = 4,
    CastSpell = 5,
    Advance = 6,
    Pass = 7,
    /// Reserved in v0: accepted by the decoder, always refused with `NotPlaying`.
    Leave = 8,
}

impl Kind {
    pub const fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::ClaimSeat,
            2 => Kind::Mulligan,
            3 => Kind::Charge,
            4 => Kind::CastUnit,
            5 => Kind::CastSpell,
            6 => Kind::Advance,
            7 => Kind::Pass,
            8 => Kind::Leave,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub seq: u16,
    pub seat: u8,
    pub kind: Kind,
    pub card: u16,
    pub lane: i8,
    pub target: u8,
    /// Meaning depends on `kind`. For `CastSpell` with a Shift effect: 0 = toward lane 0, 1 = toward lane 2.
    pub aux: u8,
    pub time_ms: u32,
    pub uid: [u8; 7],
    pub auth: u8,
}

impl Record {
    pub const LEN: usize = 24;

    /// Bytes 9, 22 and 23 are reserved: written as zero here, ignored by `decode`.
    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[0..2].copy_from_slice(&self.seq.to_le_bytes());
        b[2] = self.seat;
        b[3] = self.kind as u8;
        b[4..6].copy_from_slice(&self.card.to_le_bytes());
        b[6] = self.lane as u8;
        b[7] = self.target;
        b[8] = self.aux;
        b[10..14].copy_from_slice(&self.time_ms.to_le_bytes());
        b[14..21].copy_from_slice(&self.uid);
        b[21] = self.auth;
        b
    }

    /// A record is exactly 24 bytes on the wire. `decode` consumes the first 24 bytes of the
    /// buffer it is given and ignores the rest; fewer than 24 is an error.
    pub fn decode(b: &[u8]) -> Option<Record> {
        if b.len() < Self::LEN {
            return None;
        }
        Some(Record {
            seq: u16::from_le_bytes([b[0], b[1]]),
            seat: b[2],
            kind: Kind::from_u8(b[3])?,
            card: u16::from_le_bytes([b[4], b[5]]),
            lane: b[6] as i8,
            target: b[7],
            aux: b[8],
            time_ms: u32::from_le_bytes([b[10], b[11], b[12], b[13]]),
            uid: [b[14], b[15], b[16], b[17], b[18], b[19], b[20]],
            auth: b[21],
        })
    }
}
