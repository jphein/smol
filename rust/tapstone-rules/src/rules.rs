//! Applying tap events to the game state: the rules. Every failure is a `Refusal`, never a panic.
use crate::cards::{CardKind, Effect, Keyword, design};
use crate::event::{Kind, Record};
use crate::state::{CELLS, Game, LANES, Phase, SEATS, Seat, Unit, Winner};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotYourTurn,
    NotInHand,
    NoMana { need: u8, have: u8 },
    CellOccupied,
    AlreadyChargedThisRound,
    AlreadyAdvancedLane,
    BadTarget,
    UnknownCard,
    GameOver,
    LaneOutOfRange,
    NotPlaying,
    SeatTaken,
    MulliganClosed,
    LobbyClosed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Charged,
    Summoned {
        lane: u8,
        cell: u8,
    },
    Spell,
    Advanced {
        lane: u8,
        moved: u8,
    },
    TurnEnded {
        combat_damage: [u8; SEATS],
    },
    GameEnded(Winner),
    SeatClaimed {
        seat: u8,
    },
    /// The second seat was claimed: hands dealt, round 1, seat 0 active.
    Started,
    Mulliganed {
        drawn: u8,
    },
}

/// The enemy castle as a spell target.
const CASTLE_TARGET: u8 = 0xFF;

impl Game {
    /// Apply one tap. `seq` advances only when the event is accepted (lobby events included).
    pub fn apply(&mut self, r: &Record) -> Result<Applied, Refusal> {
        let out = match self.phase {
            Phase::Over => Err(Refusal::GameOver),
            Phase::Lobby => match r.kind {
                Kind::ClaimSeat => self.claim_seat(r),
                _ => Err(Refusal::NotPlaying),
            },
            Phase::Playing => {
                if r.seat as usize >= SEATS || r.seat != self.active {
                    return Err(Refusal::NotYourTurn);
                }
                let out = match r.kind {
                    Kind::Mulligan => self.mulligan(),
                    Kind::Charge => self.charge(r),
                    Kind::CastUnit => self.cast_unit(r),
                    Kind::CastSpell => self.cast_spell(r),
                    Kind::Advance => self.advance(r),
                    Kind::Pass => Ok(self.end_turn()),
                    // Leave is out of scope for v0; ClaimSeat belongs to the Lobby.
                    Kind::ClaimSeat | Kind::Leave => Err(Refusal::NotPlaying),
                }?;
                if r.kind != Kind::Mulligan {
                    self.seats[(r.seat & 1) as usize].acted = true;
                }
                Ok(out)
            }
        }?;
        self.seq = self.seq.wrapping_add(1);
        Ok(out)
    }

    /// Lobby: `r.seat` claims a seat with castle design `r.card`. The second claim starts the game.
    fn claim_seat(&mut self, r: &Record) -> Result<Applied, Refusal> {
        if r.seat as usize >= SEATS {
            return Err(Refusal::NotYourTurn);
        }
        let d = design(r.card).ok_or(Refusal::UnknownCard)?;
        if d.kind != CardKind::Castle {
            return Err(Refusal::BadTarget);
        }
        let seat = &mut self.seats[(r.seat & 1) as usize];
        if seat.present {
            return Err(Refusal::SeatTaken);
        }
        seat.castle_design = r.card;
        seat.present = true;
        if self.seats.iter().all(|s| s.present) {
            self.begin_play();
            return Ok(Applied::Started);
        }
        Ok(Applied::SeatClaimed { seat: r.seat })
    }

    /// Once, on the active seat's first turn before any other action: discard the hand and draw
    /// as many from the remaining deck order.
    fn mulligan(&mut self) -> Result<Applied, Refusal> {
        let s = self.me();
        if s.acted || s.mulliganed {
            return Err(Refusal::MulliganClosed);
        }
        let n = s.hand_len;
        s.hand_len = 0;
        let mut drawn = 0;
        for _ in 0..n {
            if s.draw() {
                drawn += 1;
            }
        }
        s.mulliganed = true;
        Ok(Applied::Mulliganed { drawn })
    }

    fn me(&mut self) -> &mut Seat {
        &mut self.seats[(self.active & 1) as usize]
    }

    fn charge(&mut self, r: &Record) -> Result<Applied, Refusal> {
        let s = self.me();
        if s.charged_this_round {
            return Err(Refusal::AlreadyChargedThisRound);
        }
        if !s.remove_from_hand(r.card) {
            return Err(Refusal::NotInHand);
        }
        s.charged = s.charged.saturating_add(1);
        s.charged_this_round = true;
        Ok(Applied::Charged)
    }

    fn cast_unit(&mut self, r: &Record) -> Result<Applied, Refusal> {
        let d = design(r.card).ok_or(Refusal::UnknownCard)?;
        let CardKind::Unit {
            attack,
            toughness,
            keyword,
        } = d.kind
        else {
            return Err(Refusal::BadTarget);
        };
        let lane = lane_index(r.lane)?;
        let cell = if keyword == Some(Keyword::Rush) { 1 } else { 0 };
        let round = self.round;
        let s = self.me();
        if !s.hand[..s.hand_len()].contains(&r.card) {
            return Err(Refusal::NotInHand);
        }
        if s.cells[lane][cell].is_some() {
            return Err(Refusal::CellOccupied);
        }
        let need = d.cost;
        let have = s.available_mana();
        if have < need {
            return Err(Refusal::NoMana { need, have });
        }
        s.spent = s.spent.saturating_add(need);
        s.remove_from_hand(r.card);
        s.cells[lane][cell] = Some(Unit {
            design: d.id,
            attack,
            toughness,
            damage: 0,
            keyword,
            entered_round: round,
        });
        Ok(Applied::Summoned {
            lane: lane as u8,
            cell: cell as u8,
        })
    }

    /// Check mana and hand, resolve the effect, then spend, discard and sweep.
    /// A refused target costs nothing; a refused mana check changes nothing.
    fn cast_spell(&mut self, r: &Record) -> Result<Applied, Refusal> {
        let d = design(r.card).ok_or(Refusal::UnknownCard)?;
        let CardKind::Spell(effect) = d.kind else {
            return Err(Refusal::BadTarget);
        };
        let need = d.cost;
        {
            let s = self.me();
            if !s.hand[..s.hand_len()].contains(&r.card) {
                return Err(Refusal::NotInHand);
            }
            let have = s.available_mana();
            if have < need {
                return Err(Refusal::NoMana { need, have });
            }
        }
        let opp = 1 - (self.active & 1) as usize;
        match effect {
            Effect::Damage { amount, castle_ok } => {
                if r.target == CASTLE_TARGET {
                    if !castle_ok {
                        return Err(Refusal::BadTarget);
                    }
                    let castle = &mut self.seats[opp].castle;
                    castle.life = castle.life.saturating_sub(amount);
                } else {
                    let (s, l, c) = unpack(r.target)?;
                    let u = self.seats[s].cells[l][c]
                        .as_mut()
                        .ok_or(Refusal::BadTarget)?;
                    u.damage = u.damage.saturating_add(amount);
                }
            }
            Effect::Heal { amount } => {
                let (s, l, c) = unpack(r.target)?;
                let u = self.seats[s].cells[l][c]
                    .as_mut()
                    .ok_or(Refusal::BadTarget)?;
                u.damage = u.damage.saturating_sub(amount);
            }
            Effect::Destroy { max_toughness } => {
                let (s, l, c) = unpack(r.target)?;
                let u = self.seats[s].cells[l][c].ok_or(Refusal::BadTarget)?;
                if u.toughness > max_toughness {
                    return Err(Refusal::BadTarget);
                }
                self.seats[s].cells[l][c] = None;
            }
            Effect::Shift => {
                let (s, l, c) = unpack(r.target)?;
                let dir: i8 = if r.aux == 0 { -1 } else { 1 };
                let nl = lane_index(l as i8 + dir)?;
                // Check source before destination so a refused shift changes nothing.
                if self.seats[s].cells[l][c].is_none() {
                    return Err(Refusal::BadTarget);
                }
                if self.seats[s].cells[nl][c].is_some() {
                    return Err(Refusal::CellOccupied);
                }
                let u = self.seats[s].cells[l][c].take();
                self.seats[s].cells[nl][c] = u;
            }
            Effect::Draw { count } => {
                for _ in 0..count {
                    self.me().draw();
                }
            }
        }
        let s = self.me();
        s.spent = s.spent.saturating_add(need);
        s.remove_from_hand(r.card);
        self.sweep_dead();
        // A castle at 0 during a live turn is never a displayable state: finish now.
        if self.any_castle_fallen() {
            return Ok(self.finish());
        }
        Ok(Applied::Spell)
    }

    fn advance(&mut self, r: &Record) -> Result<Applied, Refusal> {
        let lane = lane_index(r.lane)?;
        let round = self.round;
        let s = self.me();
        if s.lanes_advanced[lane] {
            return Err(Refusal::AlreadyAdvancedLane);
        }
        let mut moved = 0;
        // Front-most first so a column shuffles forward.
        for cell in (0..CELLS - 1).rev() {
            if let Some(u) = s.cells[lane][cell] {
                let may = u.entered_round < round || u.keyword == Some(Keyword::Haste);
                if may && s.cells[lane][cell + 1].is_none() {
                    s.cells[lane][cell + 1] = Some(u);
                    s.cells[lane][cell] = None;
                    moved += 1;
                }
            }
        }
        s.lanes_advanced[lane] = true;
        Ok(Applied::Advanced {
            lane: lane as u8,
            moved,
        })
    }

    fn end_turn(&mut self) -> Applied {
        let dmg = self.combat();
        self.sweep_dead();
        if self.any_castle_fallen() {
            return self.finish();
        }
        // Round `stop_round` completed by both seats: finish before wrapping so `round` stays == stop_round.
        if self.active == 1 && self.round >= self.rules.stop_round {
            return self.finish();
        }
        let next = 1 - (self.active & 1) as usize;
        if next == 0 {
            self.round = self.round.saturating_add(1);
        }
        self.active = next as u8;
        let round = self.round;
        let rules = self.rules;
        let s = &mut self.seats[next];
        s.spent = 0;
        s.charged_this_round = false;
        s.lanes_advanced = [false; LANES];
        if round >= rules.pressure_from {
            s.castle.life = s.castle.life.saturating_sub(rules.pressure);
        }
        s.draw();
        if self.any_castle_fallen() {
            return self.finish();
        }
        Applied::TurnEnded { combat_damage: dmg }
    }

    fn any_castle_fallen(&self) -> bool {
        self.seats.iter().any(|s| s.castle.life == 0)
    }

    /// Higher castle life wins; tie → more units on board; tie → seat 1.
    fn finish(&mut self) -> Applied {
        self.phase = Phase::Over;
        let (a, b) = (&self.seats[0], &self.seats[1]);
        let w = if a.castle.life != b.castle.life {
            Winner::Seat(if a.castle.life > b.castle.life { 0 } else { 1 })
        } else if a.units() != b.units() {
            Winner::Seat(if a.units() > b.units() { 0 } else { 1 })
        } else {
            Winner::Seat(1)
        };
        self.winner = Some(w);
        Applied::GameEnded(w)
    }

    /// Simultaneous combat at a turn end: both seats' units strike, damage is tallied then applied together.
    fn combat(&mut self) -> [u8; SEATS] {
        let mut castle_dmg = [0u8; SEATS];
        let mut hits = [[[0u8; CELLS]; LANES]; SEATS];
        for me in 0..SEATS {
            let opp = 1 - me;
            for (lane, cells) in self.seats[me].cells.iter().enumerate() {
                for (cell, slot) in cells.iter().enumerate() {
                    let Some(u) = slot else { continue };
                    let is_front = cell == CELLS - 1;
                    let ranged = u.keyword == Some(Keyword::Ranged);
                    if !is_front && !ranged {
                        continue;
                    }
                    match target_in_lane(&self.seats[opp].cells[lane], ranged) {
                        Some(tc) => {
                            hits[opp][lane][tc] = hits[opp][lane][tc].saturating_add(u.attack)
                        }
                        None => castle_dmg[opp] = castle_dmg[opp].saturating_add(u.attack),
                    }
                }
            }
        }
        for (s, seat) in self.seats.iter_mut().enumerate() {
            seat.castle.life = seat.castle.life.saturating_sub(castle_dmg[s]);
            for (lane, cells) in seat.cells.iter_mut().enumerate() {
                for (cell, slot) in cells.iter_mut().enumerate() {
                    if let Some(u) = slot {
                        let mut h = hits[s][lane][cell];
                        if u.keyword == Some(Keyword::Shield1) && h > 0 {
                            h -= 1;
                        }
                        u.damage = u.damage.saturating_add(h);
                    }
                }
            }
        }
        castle_dmg
    }

    fn sweep_dead(&mut self) {
        for slot in self
            .seats
            .iter_mut()
            .flat_map(|s| s.cells.iter_mut().flatten())
        {
            if matches!(*slot, Some(u) if u.damage >= u.toughness) {
                *slot = None;
            }
        }
    }
}

fn lane_index(lane: i8) -> Result<usize, Refusal> {
    if (0..LANES as i8).contains(&lane) {
        Ok(lane as usize)
    } else {
        Err(Refusal::LaneOutOfRange)
    }
}

/// Target cell for one attacker. A Taunt anywhere in the enemy lane draws every attacker (nearest Taunt, front→back).
/// Otherwise a Ranged attacker hits the nearest enemy (front→back); a melee attacker hits only the enemy front cell.
/// None = hit the castle.
fn target_in_lane(enemy: &[Option<Unit>; CELLS], ranged: bool) -> Option<usize> {
    let order = [CELLS - 1, 1, 0];
    if let Some(&t) = order
        .iter()
        .find(|&&c| matches!(enemy[c], Some(u) if u.keyword == Some(Keyword::Taunt)))
    {
        return Some(t);
    }
    if ranged {
        order.iter().copied().find(|&c| enemy[c].is_some())
    } else if enemy[CELLS - 1].is_some() {
        Some(CELLS - 1)
    } else {
        None
    }
}

/// Target byte = seat<<4 | lane<<2 | cell; lane/cell out of range → BadTarget (no index panic).
fn unpack(t: u8) -> Result<(usize, usize, usize), Refusal> {
    let (s, l, c) = (
        ((t >> 4) & 1) as usize,
        ((t >> 2) & 3) as usize,
        (t & 3) as usize,
    );
    if l >= LANES || c >= CELLS {
        return Err(Refusal::BadTarget);
    }
    Ok((s, l, c))
}
