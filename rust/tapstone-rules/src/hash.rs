//! Canonical state bytes and the chained hash (spec §3, decision 0016).
//! h_0 = SHA256(b"tapstone:v0" ‖ house_rules_bytes ‖ per seat: commander attack, toughness,
//! keyword ‖ per seat: list length, then the sorted deck list (0036))[..8]; h_n = SHA256(h_{n-1} ‖ record ‖ canonical_state)[..8].
//!
//! What the image covers, and why:
//! - Cells are lane-major: lane 0 cells 0, 1, 2, then lane 1, then lane 2. Each cell is design (LE u16),
//!   damage, entered_round; an empty cell is FF FF 00 00.
//! - Fields derived from `design` (attack, toughness, keyword) are omitted for cards. A commander's
//!   stats come from its level and gear, not a design, so per seat the image carries the
//!   commander's attack, toughness, keyword code (FF = none), return round and lane (0029). The
//!   commander's cell uses the ordinary cell encoding with `COMMANDER_DESIGN`.
//! - Hand *contents* are hashed deliberately: state is fully replicated on both shrines, there is no
//!   hidden information, and cards leave hands only through committed events.
//! - Per-seat flags byte: bit 0 charged_this_round, bits 1..=3 lanes_advanced[k], bit 4 acted,
//!   bit 5 mulliganed, bit 6 present.
//!
//! Genesis contract: `Chain::genesis` is taken when the game leaves the Lobby, after house rules are
//! final. Lobby records before that point are transcript-only and never enter the chain.
use sha2::{Digest, Sha256};

use crate::cards::Keyword;
use crate::event::Record;
use crate::state::{CELLS, Commander, DECK_MAX, Game, HAND_MAX, LANES, Phase, SEATS, Winner};

/// Commander bytes per seat: attack, toughness, keyword code, return round, lane.
const COMMANDER_BYTES: usize = 5;

/// round, active, phase, winner, seq(2); per seat: castle_design(2), life, charged, spent, owed draws, flags,
/// commander×5, cells×4, hand_len, hand×2.
pub const CANON: usize =
    6 + SEATS * (2 + 5 + COMMANDER_BYTES + LANES * CELLS * 4 + 1 + HAND_MAX * 2); // = 144

fn keyword_byte(k: Option<Keyword>) -> u8 {
    k.map_or(0xFF, Keyword::code)
}

/// The bytes genesis hashes for one commander: its final stats, fixed from that point on.
fn commander_stats(c: &Commander) -> [u8; 3] {
    [c.attack, c.toughness, keyword_byte(c.keyword)]
}

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
        // The state's own flags byte (bit layout on `Seat::flags`), hashed as stored.
        put(
            &[s.castle.life, s.charged, s.spent, s.owed, s.flags],
            &mut i,
        );
        let c = &s.commander;
        let [a, t, k] = commander_stats(c);
        put(&[a, t, k, c.returns, c.lane], &mut i);
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
    /// Taken as the game leaves the Lobby: the house rules and both commanders' stats are final.
    /// Takes the whole `Game` so no caller can hash the rules and forget the commanders.
    pub fn genesis(g: &Game) -> Chain {
        let mut h = Sha256::new();
        h.update(b"tapstone:v0");
        h.update(g.rules.bytes());
        for s in &g.seats {
            h.update(commander_stats(&s.commander));
        }
        // 0036: each seat's deck LIST, sorted, so the physical shuffle never moves genesis. Taken
        // as the game leaves the Lobby, before any draw, so it is the whole list.
        for s in &g.seats {
            let n = s.deck_len as usize;
            let mut list = [0u16; DECK_MAX];
            list[..n].copy_from_slice(&s.deck[..n]);
            list[..n].sort_unstable();
            h.update([s.deck_len]);
            for c in &list[..n] {
                h.update(c.to_le_bytes());
            }
        }
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
