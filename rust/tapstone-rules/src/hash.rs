//! Canonical state bytes and the chained hash (spec §3, decision 0016).
//! h_0 = SHA256(b"tapstone:v0" ‖ house_rules_bytes)[..8]; h_n = SHA256(h_{n-1} ‖ record ‖ canonical_state)[..8].
//!
//! What the image covers, and why:
//! - Cells are lane-major: lane 0 cells 0, 1, 2, then lane 1, then lane 2. Each cell is design (LE u16),
//!   damage, entered_round; an empty cell is FF FF 00 00.
//! - Fields derived from `design` (attack, toughness, keyword) are omitted. The image must grow the
//!   day any effect mutates a unit's stats.
//! - Hand *contents* are hashed deliberately: state is fully replicated on both shrines, there is no
//!   hidden information, and cards leave hands only through committed events.
//! - Per-seat flags byte: bit 0 charged_this_round, bits 1..=3 lanes_advanced[k], bit 4 acted,
//!   bit 5 mulliganed, bit 6 present.
//!
//! Genesis contract: `Chain::genesis` is taken when the game leaves the Lobby, after house rules are
//! final. Lobby records before that point are transcript-only and never enter the chain.
use sha2::{Digest, Sha256};

use crate::event::Record;
use crate::state::{CELLS, Game, HAND_MAX, HouseRules, LANES, Phase, SEATS, Winner};

/// round, active, phase, winner, seq(2); per seat: castle_design(2), life, charged, spent, deck_pos, flags,
/// cells×4, hand_len, hand×2.
pub const CANON: usize = 6 + SEATS * (2 + 5 + LANES * CELLS * 4 + 1 + HAND_MAX * 2); // = 134

/// Stable accessor for `CANON`, so callers sizing a buffer do not depend on the constant's visibility.
pub const fn canonical_len() -> usize {
    CANON
}

/// Deterministic byte image of everything that affects play. Hand *contents* are included (the
/// arbiter knows them via the tap stream: cards leave hands only through committed events).
pub fn canonical(g: &Game, out: &mut [u8; CANON]) {
    let mut i = 0;
    let mut put = |b: &[u8], i: &mut usize| {
        out[*i..*i + b.len()].copy_from_slice(b);
        *i += b.len();
    };
    let phase = match g.phase {
        Phase::Lobby => 0,
        Phase::Playing => 1,
        Phase::Over => 2,
    };
    let winner = match g.winner {
        None => 0xFF,
        Some(Winner::Seat(s)) => s,
        Some(Winner::Draw) => 2,
    };
    put(&[g.round, g.active, phase, winner], &mut i);
    put(&g.seq.to_le_bytes(), &mut i);
    for s in &g.seats {
        put(&s.castle_design.to_le_bytes(), &mut i);
        // bit 0: charged this round; bits 1..=LANES: lane k advanced this turn; then acted, mulliganed, present.
        let mut flags = u8::from(s.charged_this_round);
        for (k, &advanced) in s.lanes_advanced.iter().enumerate() {
            flags |= u8::from(advanced) << (k + 1);
        }
        flags |= u8::from(s.acted) << (LANES + 1);
        flags |= u8::from(s.mulliganed) << (LANES + 2);
        flags |= u8::from(s.present) << (LANES + 3);
        put(
            &[s.castle.life, s.charged, s.spent, s.deck_pos, flags],
            &mut i,
        );
        for cell in s.cells.iter().flatten() {
            match cell {
                Some(u) => {
                    let [d0, d1] = u.design.to_le_bytes();
                    put(&[d0, d1, u.damage, u.entered_round], &mut i);
                }
                None => put(&[0xFF, 0xFF, 0, 0], &mut i),
            }
        }
        put(&[s.hand_len], &mut i);
        for (h, &card) in s.hand.iter().enumerate() {
            let v = if h < s.hand_len as usize {
                card
            } else {
                0xFFFF
            };
            put(&v.to_le_bytes(), &mut i);
        }
    }
    debug_assert_eq!(i, CANON);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chain {
    head: [u8; 8],
    pub len: u32,
}

impl Chain {
    pub fn genesis(rules: &HouseRules) -> Chain {
        let mut h = Sha256::new();
        h.update(b"tapstone:v0");
        h.update(rules.bytes());
        let d = h.finalize();
        let mut head = [0u8; 8];
        head.copy_from_slice(&d[..8]);
        Chain { head, len: 0 }
    }

    pub fn step(&mut self, r: &Record, g: &Game) {
        let mut canon = [0u8; CANON];
        canonical(g, &mut canon);
        let mut h = Sha256::new();
        h.update(self.head);
        h.update(r.encode());
        h.update(canon);
        let d = h.finalize();
        self.head.copy_from_slice(&d[..8]);
        self.len = self.len.saturating_add(1);
    }

    pub fn head(&self) -> [u8; 8] {
        self.head
    }
}
