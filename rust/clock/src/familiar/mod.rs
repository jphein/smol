//! The Mesh Familiar (issue #57 — the flagship) — a single living creature that
//! inhabits **exactly one board at a time** and MIGRATES across the ESP-NOW mesh,
//! visibly. Unplug the node it's on → it hops to a neighbour within seconds (it
//! never dies). It grows with uptime, is fed with the BOOT button, and reacts to
//! real mesh + clock events. This is smol's soul made visible.
//!
//! # Architecture (wisp's spec §1 + §7)
//! The familiar rides two **already-shipped** mesh patterns, inventing nothing new:
//! - the World-Snake **broadcast/dead-reckon discipline** (a tiny fixed frame, a
//!   bounded RX inbox drained by `main`), and
//! - the MC **single-owner arbitration** (`seq` + id tiebreak; RSSI-weighted,
//!   staggered dead-owner takeover — here computed mesh-locally from the roster).
//!
//! Split of responsibility (wisp §7 "Decision point for the fw author"): the
//! **holder / heartbeat / election** is ALWAYS-ON INFRASTRUCTURE — [`FamState`]
//! lives inside [`crate::net::mode::RadioManager`] (beside the roster it elects
//! from) and is ticked every `main` loop via `RadioManager::fam_tick`, so the pet
//! keeps living even when its screen isn't the active plugin. The **render + feed**
//! is the [`FamiliarState`] PLUGIN, which only reaches a `Copy` [`FamView`] snapshot
//! and the `fam_feed`/`fam_call` hooks through `ctx.radio` (exactly as MeshSnake
//! reaches the radio for its SNK frames). One creature, one owner, one source of truth.
//!
//! Everything here is `#[cfg(feature = "espnow")]` (it needs the radio) → the
//! default/wifi builds link NONE of it (the default-build invariant is preserved
//! by module-gating, not byte-comparison).

use ::core::fmt::Write as _;
use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle, Triangle},
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;
use crate::mesh_snake::snake_core::phase_offset_ms;
use crate::net::names::{name_for_id, name_for_seed, FANTASY};

// ===========================================================================
// Tuning (all wall-clock; wisp §1.2/§3/§6). Kept together as the single lever.
// ===========================================================================

/// Heartbeat cadence (ms). Only the holder beats; every node caches the freshest
/// `(holder, seq, creature)` it hears. Phase-jittered per-id so synced boards
/// don't fire into one RX window (reuses `phase_offset_ms`).
const HEARTBEAT_MS: u64 = 1_500;
/// Broadcast phase spread — matches the snake netcode's `PHASE_NMAX` so the
/// per-id offset math is identical.
const PHASE_NMAX: u8 = 16;
/// A botched handoff self-heals: if the chosen destination doesn't take up the
/// heartbeat within this window, the old holder resumes (the creature is never
/// lost by a dropped handoff frame).
const HANDOFF_TIMEOUT_MS: u64 = 4_000;
/// No heartbeat heard for this long ⇒ the holder is presumed dead ⇒ a survivor
/// takes over (several missed beats). This is the unplug-migration trigger.
const FAM_LOST_MS: u64 = 12_000;
/// Cold-mesh grace: a node that has NEVER heard a familiar waits this long from
/// boot before it may mint a NEW creature — long enough to hear an existing
/// holder first (so a joining board adopts, it doesn't spawn a rival).
const FIRST_BIRTH_GRACE_MS: u64 = 8_000;
/// Per-`id % 8` claim stagger (ms) — the final tiebreak so two equally-near
/// survivors don't claim in the same instant (lowest id effectively wins).
const ID_STAGGER_MS: u64 = 200;
/// Per-RSSI-bucket claim stagger (ms). A survivor that heard the dead holder
/// STRONGLY (physically near it) waits fewer buckets → adopts first, so the
/// creature "hops to a neighbour" rather than teleporting across the house.
/// Kept small (+ the bucket capped at 6, see `rssi_stagger`) so even a far-only
/// survivor adopts well within a few seconds of the `FAM_LOST_MS` trigger.
const RSSI_STAGGER_MS: u64 = 500;
/// Base wander period (ms) + a per-id jitter span, so the fleet's handoffs don't
/// synchronise. The holder "walks" the creature to a neighbour every ~2.5–5 min.
const WANDER_BASE_MS: u64 = 150_000;
const WANDER_JITTER_SPAN_MS: u64 = 150_000;
/// When a new node joins, the holder biases its next wander to "go say hi" within
/// this window (§6 node-join greeting).
const GREET_BIAS_MS: u64 = 15_000;

/// Growth-stage thresholds (age = `unix_now − birth_unix`, everyone agrees).
/// Short early stages so a demo shows growth in-session (§3).
const EGG_MAX_S: u32 = 300; // < 5 min
const HATCHLING_MAX_S: u32 = 7_200; // < 2 h
const JUVENILE_MAX_S: u32 = 86_400; // < 24 h

/// Hunger thresholds (`now − last_fed_unix`). Feeding (BOOT tap) resets it.
const FULL_MAX_S: u32 = 600; // < 10 min
const PECKISH_MAX_S: u32 = 3_600; // < 1 h

/// A recent feed / migration / greet shows as `Happy` for this long (a short
/// wiggle, then back to Idle). Feed keeps hunger `Full` far longer than this.
const HAPPY_WINDOW_S: u32 = 15;

/// Local "night" window (holder derives the hour from `unix_now + TZ`, the same
/// math `clock.rs` uses) → the creature sleeps.
const NIGHT_START_H: u32 = 23;
const NIGHT_END_H: u32 = 6;

// ===========================================================================
// Wire frame — SMOLv1 FAM (wisp §1.1). Fixed binary after an ASCII prefix.
// ===========================================================================
//
// Byte 7 (0-indexed) of the prefix is 'F' — free (HELLO='H' BEACON/BATT='B'
// ACK='A' TIME='T' RELAY='R' CFG='C' STAT/SNK='S' GRID='G' DIAG='D' OTA='O') — so
// `strip_prefix` never confuses FAM with any other SMOLv1 tag.
//
// Layout (exactly FAM_FRAME_LEN = 29 bytes):
//   [0..11)  "SMOLv1 FAM "   ASCII prefix (sniffer-greppable)
//   [11]     kind   u8       'H' heartbeat · 'X' handoff · 'C' call
//   [12]     holder u8       the node currently hosting the familiar
//   [13]     target u8       handoff dest (X) / caller id (C); else 0
//   [14..16] seq    u16 LE   monotonic authority counter (dual-holder arbitration)
//   [16..20] seed   u32 LE   the creature's identity seed (→ name + species)
//   [20..24] birth  u32 LE   mesh-Unix birth time → age / growth stage
//   [24..28] fed    u32 LE   last-fed mesh-Unix → hunger
//   [28]     mood   u8       holder-computed mood (cosmetic; see MOOD_*)

/// The 11-byte tag (trailing space), diverging from every other SMOLv1 prefix at
/// byte 7 = `'F'`.
pub const FAM_PREFIX: &[u8; 11] = b"SMOLv1 FAM ";
/// Exact on-wire length. Well under the ~250 B frame budget.
pub const FAM_FRAME_LEN: usize = 29;

/// Frame kind (byte 11).
pub const FAM_HEARTBEAT: u8 = b'H';
pub const FAM_HANDOFF: u8 = b'X';
pub const FAM_CALL: u8 = b'C';

/// A decoded SMOLv1 FAM frame. `Copy` scalar-only → lives in `.bss`, no heap.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FamFrame {
    pub kind: u8,
    pub holder: u8,
    pub target: u8,
    pub seq: u16,
    pub seed: u32,
    pub birth: u32,
    pub fed: u32,
    pub mood: u8,
}

/// Encode a [`FamFrame`] into `out` (29 B), returning the length, or `None` if
/// `out` is too small. Mirrors `encode_snk`: fixed-size, length-returning, no
/// allocation, panic-free.
pub fn encode_fam(f: &FamFrame, out: &mut [u8]) -> Option<usize> {
    if out.len() < FAM_FRAME_LEN {
        return None;
    }
    out[..FAM_PREFIX.len()].copy_from_slice(FAM_PREFIX);
    out[11] = f.kind;
    out[12] = f.holder;
    out[13] = f.target;
    out[14..16].copy_from_slice(&f.seq.to_le_bytes());
    out[16..20].copy_from_slice(&f.seed.to_le_bytes());
    out[20..24].copy_from_slice(&f.birth.to_le_bytes());
    out[24..28].copy_from_slice(&f.fed.to_le_bytes());
    out[28] = f.mood;
    Some(FAM_FRAME_LEN)
}

/// Parse a SMOLv1 FAM frame, or `None` if too short / wrong prefix / an
/// unrecognised kind. Total, rejects garbage, never panics. A `seed == 0` frame
/// is rejected (0 = uninitialised, never a real creature).
pub fn parse_fam(buf: &[u8]) -> Option<FamFrame> {
    if buf.len() < FAM_FRAME_LEN {
        return None;
    }
    if &buf[..FAM_PREFIX.len()] != FAM_PREFIX.as_slice() {
        return None;
    }
    let kind = buf[11];
    if kind != FAM_HEARTBEAT && kind != FAM_HANDOFF && kind != FAM_CALL {
        return None;
    }
    let seq = u16::from_le_bytes([buf[14], buf[15]]);
    let seed = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
    if seed == 0 {
        return None;
    }
    let birth = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
    let fed = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    Some(FamFrame {
        kind,
        holder: buf[12],
        target: buf[13],
        seq,
        seed,
        birth,
        fed,
        mood: buf[28],
    })
}

/// Wrap-aware (RFC 1982) "is `a` newer than `b`" for the `seq:u16` authority
/// counter — the forward distance `a − b (mod 2^16)` in `1..=0x7FFF`. Handles the
/// 65535→0 wrap for free and rejects stragglers. The 32 767-wide half-window is
/// unambiguous: at one beat / 1.5 s a wrap takes >27 h, so live seqs never span it.
#[inline]
fn seq_newer(a: u16, b: u16) -> bool {
    let d = a.wrapping_sub(b);
    d != 0 && d < 0x8000
}

// ===========================================================================
// Creature identity + derived state (deterministic from the 4-byte seed + age).
// ===========================================================================

/// The three v1 species, chosen by `seed % 3` (§2). Each picks a sprite routine.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Species {
    /// Floating orb + trailing tail that sways.
    Wisp,
    /// Winged, horned silhouette that flaps.
    Drake,
    /// Legged, ear-tufted little creature.
    Sprite,
}

impl Species {
    fn from_seed(seed: u32) -> Species {
        match seed % 3 {
            0 => Species::Wisp,
            1 => Species::Drake,
            _ => Species::Sprite,
        }
    }
}

/// Growth stage from age (§3). Everyone computes the same stage from `birth_unix`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Egg,
    Hatchling,
    Juvenile,
    Adult,
}

impl Stage {
    /// Compact one-char label for the top row ("L0".."L3" reads as a level).
    fn level(self) -> u8 {
        match self {
            Stage::Egg => 0,
            Stage::Hatchling => 1,
            Stage::Juvenile => 2,
            Stage::Adult => 3,
        }
    }
}

/// Hunger from time-since-fed (§3). Feeding resets it to `Full`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hunger {
    Full,
    Peckish,
    Hungry,
}

/// Cosmetic mood the HOLDER computes each beat and carries on the wire, so every
/// OLED (holder + pointers) agrees. TIER-1 only for v1 (clock + feed + events);
/// `Basking`/`Startled` (solar/grid) are the TIER-2 fast-follow (§3 house-tiers).
pub const MOOD_IDLE: u8 = 0;
pub const MOOD_HAPPY: u8 = 1;
pub const MOOD_HUNGRY: u8 = 2;
pub const MOOD_SLEEPING: u8 = 3;

/// A creature's identity + counters — the fields that TRAVEL (survive migration).
/// All derivation (name, species, stage, hunger) is pure over these + `unix_now`.
#[derive(Clone, Copy)]
pub struct Creature {
    pub seed: u32,
    pub birth_unix: u32,
    pub last_fed_unix: u32,
}

impl Creature {
    fn species(&self) -> Species {
        Species::from_seed(self.seed)
    }

    /// The creature's own magical noun (fantasy realm), distinct from any node's
    /// name — derived from the seed exactly like `name_for_id` derives node names.
    fn noun(&self) -> &'static str {
        name_for_seed(self.seed, &FANTASY).1
    }

    fn stage(&self, unix_now: u32) -> Stage {
        let age = unix_now.saturating_sub(self.birth_unix);
        if age < EGG_MAX_S {
            Stage::Egg
        } else if age < HATCHLING_MAX_S {
            Stage::Hatchling
        } else if age < JUVENILE_MAX_S {
            Stage::Juvenile
        } else {
            Stage::Adult
        }
    }

    fn hunger(&self, unix_now: u32) -> Hunger {
        let since = unix_now.saturating_sub(self.last_fed_unix);
        if since < FULL_MAX_S {
            Hunger::Full
        } else if since < PECKISH_MAX_S {
            Hunger::Peckish
        } else {
            Hunger::Hungry
        }
    }
}

/// True if the mesh-Unix time falls in the local night window (holder sleeps).
fn is_night(unix_now: u32) -> bool {
    let sod = ((unix_now as i64) + crate::TZ_OFFSET_SECONDS).rem_euclid(86_400) as u32;
    let hour = sod / 3_600;
    // Night = the wrap-around window [23:00, 06:00) = NOT within the daytime range.
    !(NIGHT_END_H..NIGHT_START_H).contains(&hour)
}

// ===========================================================================
// FamState — the always-on holder / arbitration / migration state machine.
// Owned by RadioManager; ticked every main loop. This is the technical heart.
// ===========================================================================

/// The living familiar's authoritative state on THIS node. Exactly one node in the
/// mesh is `is_holder` at steady state (§1.2 the exactly-one invariant).
pub struct FamState {
    /// This node's logical id (holder comparisons + election).
    node_id: u8,
    /// Are WE the current holder (the one that beats + renders the live creature)?
    is_holder: bool,
    /// The authoritative sequence counter (monotonic; ++ each beat/handoff/claim).
    seq: u16,
    /// Latest known holder id (== `node_id` while `is_holder`).
    holder_id: u8,
    /// Whether we've ever heard/hosted a familiar (false = cold mesh, no creature).
    known: bool,
    /// The creature's travelling identity + counters (valid once `known`).
    creature: Creature,
    /// The mood the holder last computed / we last heard (drives every render).
    mood: u8,
    /// Monotonic ms of the last heartbeat we HEARD (drives orphan-takeover timing).
    last_beat_ms: u64,
    /// RSSI (dBm) of the last holder frame we heard — nearer holder ⇒ we adopt
    /// sooner on its death (the "hops to a neighbour" weighting).
    last_holder_rssi: i8,
    /// Our phase-jittered heartbeat slot within `HEARTBEAT_MS`.
    hb_phase: u64,
    /// A handoff we initiated: the deadline (monotonic ms) by which the chosen
    /// destination must have taken up the heartbeat, else we resume (keep the pet).
    /// `None` when not handing off.
    handoff_until_ms: Option<u64>,
    /// One-shot: emit a heartbeat on the very next tick (immediate become-holder).
    pending_beat: bool,
    /// Monotonic ms of the next scheduled wander (holder migrates the creature).
    next_wander_ms: u64,
    /// mesh-Unix of the last time we became holder (arrival greet → Happy).
    last_arrival_unix: u32,
    /// mesh-Unix of the last node-join greeting we played (holder → Happy).
    last_greet_unix: u32,
    /// A pending wander bias toward a specific id (a caller, or a newcomer to greet).
    bias_to: Option<u8>,
    /// 256-bit set of node ids we've already seen in the roster (node-join detect).
    seen_ids: [u8; 32],
    /// Whether the seen-set has been primed (first holder tick records silently,
    /// so we don't greet-storm the whole roster the instant we become holder).
    greet_primed: bool,
}

impl FamState {
    /// A fresh, creature-less state. `node_id` seeds the heartbeat phase + the
    /// arbitration id. No familiar exists until one is heard or first-birthed.
    pub fn new(node_id: u8) -> Self {
        Self {
            node_id,
            is_holder: false,
            seq: 0,
            holder_id: 0,
            known: false,
            creature: Creature { seed: 0, birth_unix: 0, last_fed_unix: 0 },
            mood: MOOD_IDLE,
            last_beat_ms: 0,
            last_holder_rssi: -90,
            hb_phase: phase_offset_ms(node_id, PHASE_NMAX, HEARTBEAT_MS as u32) as u64,
            handoff_until_ms: None,
            pending_beat: false,
            next_wander_ms: 0,
            last_arrival_unix: 0,
            last_greet_unix: 0,
            bias_to: None,
            seen_ids: [0; 32],
            greet_primed: false,
        }
    }

    pub fn is_holder(&self) -> bool {
        self.is_holder
    }

    /// A `Copy` snapshot for the render plugin (no borrow of the radio).
    pub fn view(&self, now_ms: u64) -> FamView {
        FamView {
            known: self.known,
            is_holder: self.is_holder,
            holder_id: self.holder_id,
            creature: self.creature,
            mood: self.mood,
            last_heard_s: (now_ms.saturating_sub(self.last_beat_ms) / 1_000) as u32,
        }
    }

    /// FEED (holder BOOT tap): reset hunger + greet. The next heartbeat carries the
    /// fresh `last_fed_unix` + `Happy` mood to the whole fleet.
    pub fn feed(&mut self, unix_now: u32) {
        if self.is_holder {
            self.creature.last_fed_unix = unix_now;
            self.last_greet_unix = unix_now; // a happy wiggle right away
            self.mood = MOOD_HAPPY;
            self.pending_beat = true; // propagate the joy promptly
        }
    }

    /// The id we'd address a CALL to (the current holder), for a non-holder tap.
    /// `None` if no familiar is known yet.
    pub fn call_target(&self) -> Option<u8> {
        if self.known && !self.is_holder {
            Some(self.holder_id)
        } else {
            None
        }
    }

    /// Build a type-`C` call frame naming the current holder + us as caller. The
    /// holder biases its next wander toward us ("come here!").
    pub fn call_frame(&self) -> Option<FamFrame> {
        let holder = self.call_target()?;
        Some(FamFrame {
            kind: FAM_CALL,
            holder,
            target: self.node_id,
            seq: self.seq,
            seed: self.creature.seed,
            birth: self.creature.birth_unix,
            fed: self.creature.last_fed_unix,
            mood: self.mood,
        })
    }

    // ---- id-set helpers (node-join detection) -----------------------------

    fn seen(&self, id: u8) -> bool {
        self.seen_ids[(id >> 3) as usize] & (1 << (id & 7)) != 0
    }

    fn mark_seen(&mut self, id: u8) {
        self.seen_ids[(id >> 3) as usize] |= 1 << (id & 7);
    }

    // ---- frame constructors ----------------------------------------------

    /// The current-state heartbeat/handoff frame. Pure — the caller bumps `seq`
    /// and computes `mood` first.
    fn state_frame(&self, kind: u8, target: u8) -> FamFrame {
        FamFrame {
            kind,
            holder: self.node_id,
            target,
            seq: self.seq,
            seed: self.creature.seed,
            birth: self.creature.birth_unix,
            fed: self.creature.last_fed_unix,
            mood: self.mood,
        }
    }

    /// Recompute the holder's mood from real state (§3, priority-ordered). TIER-1:
    /// Sleeping (clock) > Happy (fresh feed/greet/arrival) > Hungry > Idle.
    fn compute_mood(&self, unix_now: u32) -> u8 {
        if is_night(unix_now) {
            return MOOD_SLEEPING;
        }
        let happy = unix_now.saturating_sub(self.creature.last_fed_unix) < HAPPY_WINDOW_S
            || unix_now.saturating_sub(self.last_greet_unix) < HAPPY_WINDOW_S
            || unix_now.saturating_sub(self.last_arrival_unix) < HAPPY_WINDOW_S;
        if happy {
            return MOOD_HAPPY;
        }
        if matches!(self.creature.hunger(unix_now), Hunger::Hungry) {
            return MOOD_HUNGRY;
        }
        MOOD_IDLE
    }

    /// Emit a heartbeat NOW: recompute mood, bump `seq`, return the frame.
    fn beat(&mut self, unix_now: u32) -> FamFrame {
        self.mood = self.compute_mood(unix_now);
        self.seq = self.seq.wrapping_add(1);
        self.state_frame(FAM_HEARTBEAT, 0)
    }

    /// Schedule the next wander (per-id jittered so the fleet desynchronises).
    fn reschedule_wander(&mut self, now_ms: u64) {
        let jitter = (self.node_id as u64).wrapping_mul(7_919) % WANDER_JITTER_SPAN_MS;
        self.next_wander_ms = now_ms + WANDER_BASE_MS + jitter;
    }

    /// Adopt a heard creature's identity/state (a non-holder tracking "where +
    /// who", or a yielding holder taking the winner's creature).
    fn adopt(&mut self, f: &FamFrame) {
        self.holder_id = f.holder;
        self.seq = f.seq;
        self.creature = Creature {
            seed: f.seed,
            birth_unix: f.birth,
            last_fed_unix: f.fed,
        };
        self.mood = f.mood;
        self.known = true;
    }

    /// Become the holder of a (possibly newly-minted) creature: reset the wander
    /// timer + prime the greet-set so we don't greet the existing roster.
    fn take_holdership(&mut self, now_ms: u64, unix_now: u32) {
        self.is_holder = true;
        self.holder_id = self.node_id;
        self.handoff_until_ms = None;
        self.last_arrival_unix = unix_now;
        self.greet_primed = false;
        self.reschedule_wander(now_ms);
    }

    /// Mint a brand-new creature (cold-mesh first-birth). Seed is arbitrary but
    /// frozen for life: `now_ms` low bits spread through the golden ratio + our id,
    /// forced non-zero (0 = the "no creature" sentinel).
    fn mint(&mut self, now_ms: u64, unix_now: u32) {
        let mixed = (now_ms as u32)
            .wrapping_mul(2_654_435_761)
            ^ (self.node_id as u32).wrapping_mul(40_503).rotate_left(16);
        let seed = mixed | 1;
        self.creature = Creature { seed, birth_unix: unix_now, last_fed_unix: unix_now };
        self.seq = 0;
        self.known = true;
    }

    // ---- inbound frame handling (drained by RadioManager::fam_tick) -------

    /// Ingest a decoded FAM frame heard from a peer at `rssi`. Runs the exactly-one
    /// arbitration (dual-holder collapse), handoff take-up, orphan-view tracking,
    /// and call-bias capture. Never emits — any resulting beat rides the next tick.
    pub fn ingest(&mut self, f: &FamFrame, rssi: i32, now_ms: u64, unix_now: u32) {
        // A frame claiming our OWN id is a stray/echo (self-frames are MAC-filtered
        // upstream; unique ids make this impossible in normal operation) — ignore.
        if f.holder == self.node_id && f.kind != FAM_CALL {
            return;
        }

        match f.kind {
            FAM_CALL => {
                // "Come here!" addressed to us as holder → bias next wander to the
                // caller, and expedite it so the pet actually walks over soon.
                if self.is_holder && f.holder == self.node_id {
                    self.bias_to = Some(f.target);
                    self.next_wander_ms = self.next_wander_ms.min(now_ms + GREET_BIAS_MS);
                }
            }
            FAM_HEARTBEAT | FAM_HANDOFF => {
                if f.seed == 0 {
                    return;
                }
                // Freshness/liveness bookkeeping for every valid holder frame.
                self.last_beat_ms = now_ms;
                self.last_holder_rssi = rssi.clamp(-127, 0) as i8;

                let fresher = seq_newer(f.seq, self.seq)
                    || (f.seq == self.seq && f.holder < self.node_id);

                if self.is_holder {
                    // Dual-holder collapse (§1.2): a strictly-fresher authority (or
                    // equal-seq + lower id) wins → we yield + adopt its creature.
                    if fresher {
                        self.is_holder = false;
                        self.handoff_until_ms = None;
                        self.adopt(f);
                    }
                    // else: we're fresher-or-equal-higher-id → keep holding; the
                    // other node yields when it hears our next beat.
                } else {
                    // Non-holder: track the freshest view of where + who the pet is.
                    if !self.known || fresher || f.seq == self.seq {
                        self.adopt(f);
                    }
                }

                // A handoff addressed to US → become the new holder + beat at once
                // (the old holder confirms on hearing our +1 beat; §1.2 migration).
                if f.kind == FAM_HANDOFF && f.target == self.node_id {
                    self.adopt(f); // take the exact travelling state
                    self.take_holdership(now_ms, unix_now);
                    self.pending_beat = true;
                }
            }
            _ => {}
        }
    }

    // ---- the per-loop tick (infra; may emit one frame to broadcast) -------

    /// Advance the state machine one main-loop subtick. `roster` is the live peer
    /// snapshot (RSSI-desc). Returns a frame to broadcast this tick, if any. This
    /// runs EVERY loop regardless of the active screen — the pet is always alive.
    pub fn tick(
        &mut self,
        roster: &crate::net::mode::RosterView,
        now_ms: u64,
        unix_now: u32,
    ) -> Option<FamFrame> {
        // An immediate become-holder beat (handoff take-up) fires first.
        if self.pending_beat {
            self.pending_beat = false;
            if self.is_holder {
                return Some(self.beat(unix_now));
            }
        }

        // A handoff in flight: wait for the destination, else resume as holder.
        if let Some(deadline) = self.handoff_until_ms {
            if now_ms >= deadline {
                // Timed out (destination asleep / lost the frame) → keep the pet:
                // resume heartbeating (§1.2 "resumes at seq+2, may retry another D").
                self.handoff_until_ms = None;
                self.is_holder = true;
                self.holder_id = self.node_id;
                self.last_arrival_unix = unix_now; // "it came back"
                self.reschedule_wander(now_ms);
                return Some(self.beat(unix_now));
            }
            return None; // still waiting — no beat during a handoff
        }

        if self.is_holder {
            return self.holder_tick(roster, now_ms, unix_now);
        }
        self.claim_tick(roster, now_ms, unix_now)
    }

    /// Holder path: detect newcomers (greet), maybe migrate, else beat on cadence.
    fn holder_tick(
        &mut self,
        roster: &crate::net::mode::RosterView,
        now_ms: u64,
        unix_now: u32,
    ) -> Option<FamFrame> {
        self.detect_newcomers(roster, now_ms, unix_now);

        // Wander: hand the creature off to a neighbour when the timer elapses.
        if now_ms >= self.next_wander_ms {
            if let Some(dest) = self.pick_dest(roster) {
                self.seq = self.seq.wrapping_add(1);
                self.mood = self.compute_mood(unix_now);
                self.handoff_until_ms = Some(now_ms + HANDOFF_TIMEOUT_MS);
                self.bias_to = None;
                self.reschedule_wander(now_ms); // next attempt scheduled either way
                return Some(self.state_frame(FAM_HANDOFF, dest));
            }
            // Alone (empty roster) → stay put, try again later.
            self.reschedule_wander(now_ms);
        }

        // Heartbeat on our phase-jittered slot (the SNK edge-detector idiom).
        if self.beat_due(now_ms) {
            return Some(self.beat(unix_now));
        }
        None
    }

    /// Non-holder path: take over a dead holder's creature, or first-birth a new
    /// one on a cold mesh — both via a staggered, RSSI-weighted claim window.
    fn claim_tick(
        &mut self,
        _roster: &crate::net::mode::RosterView,
        now_ms: u64,
        unix_now: u32,
    ) -> Option<FamFrame> {
        if !self.known {
            // Cold mesh: nobody has a familiar. First-birth after a boot grace so a
            // late-heard existing holder wins first. Staggered by id (no holder RSSI
            // to weight by yet) → the lowest id mints, others hear it + adopt.
            let wait = FIRST_BIRTH_GRACE_MS + self.id_stagger();
            if now_ms < wait {
                return None;
            }
            self.mint(now_ms, unix_now);
            self.take_holdership(now_ms, unix_now);
            return Some(self.beat(unix_now)); // seq 0 → 1: the first heartbeat
        }

        // Orphan takeover: no beat for FAM_LOST_MS ⇒ the holder is dead. Claim on a
        // staggered window weighted by how strongly we heard the (now-dead) holder
        // — the nearest survivor claims first, so the pet hops to a neighbour.
        if now_ms.saturating_sub(self.last_beat_ms) < FAM_LOST_MS {
            return None;
        }
        let wait = self.last_beat_ms + FAM_LOST_MS + self.rssi_stagger() + self.id_stagger();
        if now_ms < wait {
            return None;
        }
        // Re-birth from the CACHED state (same seed/birth ⇒ same creature, same age
        // — continuity preserved). `beat()` bumps `seq` to `cached_seq + 1`, so the
        // claim out-ranks any stale non-holder still at `cached_seq`; a simultaneous
        // rival claimer at the same seq is then settled by the id tiebreak.
        self.take_holdership(now_ms, unix_now);
        Some(self.beat(unix_now))
    }

    /// Detect a node id newly present in the roster → the holder greets it (Happy)
    /// and biases its next wander to "go say hi" soon (§6 node-join greeting).
    fn detect_newcomers(
        &mut self,
        roster: &crate::net::mode::RosterView,
        now_ms: u64,
        unix_now: u32,
    ) {
        for n in &roster.nodes[..roster.count] {
            if !n.id_known || n.id == self.node_id {
                continue;
            }
            if !self.seen(n.id) {
                self.mark_seen(n.id);
                if self.greet_primed {
                    self.last_greet_unix = unix_now; // Happy greet
                    self.bias_to = Some(n.id); // go say hi
                    // Expedite the greeting wander (but never earlier than now).
                    self.next_wander_ms = self.next_wander_ms.min(now_ms + GREET_BIAS_MS);
                }
            }
        }
        self.greet_primed = true;
    }

    /// Pick a wander destination from the fresh roster, weighted toward strong-RSSI
    /// (near) peers so the creature "walks" to a neighbour. A pending bias (caller /
    /// newcomer) wins if that peer is still audible.
    fn pick_dest(&self, roster: &crate::net::mode::RosterView) -> Option<u8> {
        // Gather audible, id-known peers (roster is already RSSI-desc).
        let mut cand = [0u8; crate::net::mode::ROSTER_VIEW_CAP];
        let mut n = 0;
        for node in &roster.nodes[..roster.count] {
            if node.id_known && node.id != self.node_id {
                cand[n] = node.id;
                n += 1;
            }
        }
        if n == 0 {
            return None;
        }
        // Honour a bias (call / greet) when that peer is still present.
        if let Some(b) = self.bias_to {
            if cand[..n].contains(&b) {
                return Some(b);
            }
        }
        // Otherwise pick among the nearer half, rotated by seq so it varies (it
        // "wanders" rather than pinning to the single strongest peer).
        let strong = n.div_ceil(2);
        let pick = (self.seq as usize) % strong;
        Some(cand[pick])
    }

    /// Heartbeat-due edge detector on our phase-jittered slot (identical idiom to
    /// the snake SNK broadcast edge: a `now/period` boundary crossed this subtick).
    fn beat_due(&self, now_ms: u64) -> bool {
        let period = HEARTBEAT_MS;
        let base = now_ms.saturating_sub(self.hb_phase);
        let cur = base / period;
        let prev = base.saturating_sub(crate::SUBTICK_MS as u64) / period;
        cur != prev
    }

    /// Per-`id % 8` stagger term (ms) — the final claim tiebreak.
    fn id_stagger(&self) -> u64 {
        (self.node_id as u64 % 8) * ID_STAGGER_MS
    }

    /// RSSI-bucketed stagger term (ms): a stronger (nearer) last-heard holder ⇒
    /// fewer buckets ⇒ we claim sooner. Buckets ~10 dB wide, capped at 6 so even a
    /// far-only survivor's total wait stays within a few seconds of `FAM_LOST_MS`.
    fn rssi_stagger(&self) -> u64 {
        let mag = (-(self.last_holder_rssi as i32)).clamp(0, 99) as u64; // 0..99 dBm
        (mag / 10).min(6) * RSSI_STAGGER_MS
    }
}

// ===========================================================================
// FamView — the Copy snapshot the render plugin reads through ctx.radio.
// ===========================================================================

/// A `Copy` render snapshot (no live borrow of the radio / FamState).
#[derive(Clone, Copy)]
pub struct FamView {
    pub known: bool,
    pub is_holder: bool,
    pub holder_id: u8,
    pub creature: Creature,
    pub mood: u8,
    /// Seconds since we last heard the holder (non-holder "last seen" line).
    pub last_heard_s: u32,
}

impl Default for FamView {
    fn default() -> Self {
        Self {
            known: false,
            is_holder: false,
            holder_id: 0,
            creature: Creature { seed: 0, birth_unix: 0, last_fed_unix: 0 },
            mood: MOOD_IDLE,
            last_heard_s: 0,
        }
    }
}

// ===========================================================================
// FamiliarState — the render + input PLUGIN (holder view / away pointer).
// ===========================================================================

/// How many subticks between animation frames (~120 ms at 20 ms/subtick) — smooth
/// enough for a bob/flap without hammering the I²C flush.
const ANIM_DIV: u32 = 6;

/// The Familiar SCREEN. Holds ONLY render state — the living creature is infra
/// (RadioManager's `FamState`), reached via `ctx.radio` for the view + feed/call,
/// exactly as MeshSnake reaches the radio for its frames.
pub struct FamiliarState {
    /// Animation phase (bob / blink / flap / tail sway).
    frame: u32,
    /// This node's id (the away-view can't derive it without the radio, and the
    /// pointer needs it to say "@ here" vs "@ <Noun>").
    node_id: u8,
}

impl FamiliarState {
    pub fn new(node_id: u8) -> Self {
        Self { frame: 0, node_id }
    }
}

impl Plugin for FamiliarState {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                if let Some(r) = ctx.radio.as_deref_mut() {
                    if r.fam_is_holder() {
                        // FEED: reset hunger, a happy wiggle propagates on the next beat.
                        r.fam_feed(ctx.unix_now);
                    } else if let Some(frame) = r.fam_call_frame() {
                        // CALL: "come here!" — bias the holder's wander toward us.
                        r.broadcast_fam(&frame);
                    }
                }
                ctx.redraw = true;
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        self.frame = self.frame.wrapping_add(1);
        let anim_due = self.frame.is_multiple_of(ANIM_DIV);
        if !(anim_due || ctx.redraw) {
            return;
        }
        let now_ms = ctx.now_ms;
        let view = ctx
            .radio
            .as_deref()
            .map(|r| r.fam_view(now_ms))
            .unwrap_or_default();
        ctx.display.clear(BinaryColor::Off).ok();
        draw_familiar(ctx.display, &view, self.frame, ctx.unix_now, self.node_id);
        ctx.display.flush().ok();
    }
}

// ===========================================================================
// Rendering — procedural, 1-bit, 72×40 (wisp §4). No asset pipeline: the creature
// is composed from embedded-graphics primitives, keyed off the seed + frame.
// ===========================================================================

/// Draw the Familiar screen: the live creature if we host it, else the
/// Weasley-clock "@ <Noun>" pointer to wherever it currently lives.
fn draw_familiar<D>(display: &mut D, v: &FamView, frame: u32, unix_now: u32, node_id: u8)
where
    D: DrawTarget<Color = BinaryColor>,
{
    if !v.known {
        draw_searching(display, frame);
        return;
    }
    if v.is_holder {
        draw_holder(display, v, frame, unix_now);
    } else {
        draw_pointer(display, v, node_id);
    }
}

/// Cold-mesh / not-yet-heard: a hatching egg silhouette + "seeking…".
fn draw_searching<D>(display: &mut D, frame: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    // A pulsing egg dead-centre.
    let r = 8 + ((frame / 8) % 2) as i32;
    Circle::new(Point::new(36 - r, 18 - r), (r * 2) as u32)
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(display)
        .ok();
    text(display, "seeking", 16, 32);
}

/// The holder view: name + level on top, the animated creature centred, mood +
/// "@here" at the bottom.
fn draw_holder<D>(display: &mut D, v: &FamView, frame: u32, unix_now: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let c = v.creature;
    let stage = c.stage(unix_now);

    // --- top row: "<Noun>            L<stage>" ---
    let mut line: TextLine = TextLine::new();
    let _ = write!(line, "{} L{}", c.noun(), stage.level());
    text(display, line.as_str(), 0, 0);

    // --- centre: the creature ---
    // A slow ±1 px vertical bob (slower when sleeping / hungry).
    let bob_div = match v.mood {
        MOOD_SLEEPING => 20,
        MOOD_HUNGRY => 14,
        _ => 8,
    };
    let bob = if (frame / bob_div).is_multiple_of(2) { 0i32 } else { 1i32 };
    let cx = 36;
    let cy = 20 + bob;

    if matches!(stage, Stage::Egg) {
        draw_egg(display, cx, cy, frame);
    } else {
        draw_creature(display, c.species(), stage, v.mood, cx, cy, frame);
    }

    // --- mood overlay glyph ---
    draw_mood_overlay(display, v.mood, cx, cy, frame);

    // --- bottom row: mood word + "@here" ---
    let mood_word = match v.mood {
        MOOD_SLEEPING => "sleep",
        MOOD_HAPPY => "happy",
        MOOD_HUNGRY => "hungry",
        _ => "awake",
    };
    text(display, mood_word, 0, 32);
    text(display, "@here", 44, 32);
}

/// The non-holder "away" view (the Weasley-clock hand): a small idle silhouette +
/// big "@ <HolderNoun>" pointing to the node currently hosting the creature, plus
/// the creature's own name + level + how long since we last heard it.
fn draw_pointer<D>(display: &mut D, v: &FamView, node_id: u8)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let c = v.creature;
    // Top: the creature's own identity so the fleet agrees who it is.
    let mut top: TextLine = TextLine::new();
    let _ = write!(top, "{}", c.noun());
    text(display, top.as_str(), 0, 0);

    // A tiny idle silhouette to the left (it's elsewhere, so just a hint).
    draw_silhouette(display, 10, 22);

    // The pointer: "@ <Noun>" of the holder node (or "@here" if that's us — a
    // transient during a handoff we just initiated).
    let mut ptr: TextLine = TextLine::new();
    if v.holder_id == node_id {
        let _ = write!(ptr, "@here");
    } else {
        let _ = write!(ptr, "@ {}", name_for_id(v.holder_id).1);
    }
    text(display, ptr.as_str(), 26, 16);

    // Bottom: last-heard freshness so a stale pointer is obvious.
    let mut foot: TextLine = TextLine::new();
    if v.last_heard_s < 3 {
        let _ = write!(foot, "here now");
    } else {
        let _ = write!(foot, "{}s ago", v.last_heard_s.min(999));
    }
    text(display, foot.as_str(), 26, 30);
}

/// The egg stage: a pulsing filled ovoid with a hairline crack that grows.
fn draw_egg<D>(display: &mut D, cx: i32, cy: i32, frame: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);
    let pulse = ((frame / 10) % 2) as i32;
    let w = 12 + pulse;
    let h = 16 + pulse;
    // Ovoid ≈ a filled circle with a couple of trim rows (cheap "egg" shape).
    Circle::new(Point::new(cx - w / 2, cy - h / 2 + 2), w as u32)
        .into_styled(fill)
        .draw(display)
        .ok();
    // A faint crack (off pixels) across the middle.
    Line::new(Point::new(cx - 3, cy - 2), Point::new(cx + 2, cy + 1))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::Off, 1))
        .draw(display)
        .ok();
}

/// Compose the creature from primitives: a body scaled by stage, two eyes (blink
/// via `frame`), and the species feature (Wisp tail / Drake wings / Sprite tufts).
fn draw_creature<D>(
    display: &mut D,
    species: Species,
    stage: Stage,
    mood: u8,
    cx: i32,
    cy: i32,
    frame: u32,
) where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);

    // Body radius grows with stage.
    let br = match stage {
        Stage::Hatchling => 7,
        Stage::Juvenile => 9,
        _ => 11, // Adult
    };

    // Species feature drawn BEHIND / AROUND the body first.
    match species {
        Species::Wisp => draw_wisp_tail(display, cx, cy, br, frame),
        Species::Drake => draw_drake_wings(display, cx, cy, br, frame),
        Species::Sprite => draw_sprite_features(display, cx, cy, br, stage),
    }

    // Body.
    Circle::new(Point::new(cx - br, cy - br), (br * 2) as u32)
        .into_styled(fill)
        .draw(display)
        .ok();

    // Eyes (drawn as OFF holes in the filled body so they read at 1-bit).
    let eye_dx = br / 2;
    let eye_y = cy - br / 3;
    let sleeping = mood == MOOD_SLEEPING;
    // Blink: closed for a couple of frames every ~18 (or always, when sleeping).
    let blink = sleeping || (frame % 18) < 2;
    let hungry_droop = if mood == MOOD_HUNGRY { 1 } else { 0 };
    for &ex in &[cx - eye_dx, cx + eye_dx] {
        if blink {
            // A 1-px closed-eye line (a content/sleepy look).
            Line::new(
                Point::new(ex - 1, eye_y + 1 + hungry_droop),
                Point::new(ex + 1, eye_y + 1 + hungry_droop),
            )
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::Off, 1))
            .draw(display)
            .ok();
        } else {
            Rectangle::new(Point::new(ex - 1, eye_y + hungry_droop), Size::new(2, 2))
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(display)
                .ok();
        }
    }
}

/// Wisp: a 3-segment trailing tail below the orb that sways with `frame`.
fn draw_wisp_tail<D>(display: &mut D, cx: i32, cy: i32, br: i32, frame: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);
    let sway = match (frame / 5) % 4 {
        0 => -2,
        1 => 0,
        2 => 2,
        _ => 0,
    };
    let mut y = cy + br;
    let mut x = cx;
    let mut s = 3i32;
    for i in 0..3 {
        x += if i == 0 { sway / 2 } else { sway };
        Circle::new(Point::new(x - s / 2, y), s as u32)
            .into_styled(fill)
            .draw(display)
            .ok();
        y += s;
        s -= 1;
    }
}

/// Drake: two triangular wings that flap on a 2-frame cycle, + a small horn.
fn draw_drake_wings<D>(display: &mut D, cx: i32, cy: i32, br: i32, frame: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);
    // Flap: wingtip rises/falls every ~4 anim frames.
    let up = (frame / 4).is_multiple_of(2);
    let tip_dy = if up { -br } else { -br / 3 };
    let span = br + 6;
    // Left wing.
    Triangle::new(
        Point::new(cx - br, cy),
        Point::new(cx - span, cy + tip_dy),
        Point::new(cx - br, cy + br / 2),
    )
    .into_styled(fill)
    .draw(display)
    .ok();
    // Right wing.
    Triangle::new(
        Point::new(cx + br, cy),
        Point::new(cx + span, cy + tip_dy),
        Point::new(cx + br, cy + br / 2),
    )
    .into_styled(fill)
    .draw(display)
    .ok();
    // Horn.
    Triangle::new(
        Point::new(cx - 2, cy - br),
        Point::new(cx + 2, cy - br),
        Point::new(cx, cy - br - 4),
    )
    .into_styled(fill)
    .draw(display)
    .ok();
}

/// Sprite: two ear tufts on top + little legs below (legs appear from Juvenile).
fn draw_sprite_features<D>(display: &mut D, cx: i32, cy: i32, br: i32, stage: Stage)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);
    // Ear tufts.
    for &ex in &[cx - br / 2, cx + br / 2] {
        Triangle::new(
            Point::new(ex - 2, cy - br),
            Point::new(ex + 2, cy - br),
            Point::new(ex, cy - br - 5),
        )
        .into_styled(fill)
        .draw(display)
        .ok();
    }
    // Legs (Juvenile+).
    if matches!(stage, Stage::Juvenile | Stage::Adult) {
        for &lx in &[cx - br / 2, cx + br / 2] {
            Line::new(Point::new(lx, cy + br), Point::new(lx, cy + br + 4))
                .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
                .draw(display)
                .ok();
        }
    }
}

/// A minimal 8×6 idle silhouette for the away view (it lives elsewhere).
fn draw_silhouette<D>(display: &mut D, cx: i32, cy: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    Circle::new(Point::new(cx - 4, cy - 4), 8)
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(display)
        .ok();
}

/// Mood overlay glyphs above the creature (Zzz asleep, heart happy). Hungry/idle
/// are conveyed by the body (droopy eyes / plain), so no extra glyph.
fn draw_mood_overlay<D>(display: &mut D, mood: u8, cx: i32, cy: i32, frame: u32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    match mood {
        MOOD_SLEEPING => {
            // Rising "z z" that drifts up on a slow cycle.
            let rise = ((frame / 10) % 3) as i32;
            text(display, "z", cx + 10, cy - 12 - rise);
            text(display, "z", cx + 14, cy - 18 - rise);
        }
        // A small 3×3 heart pixel-cluster that pops on a fast cycle.
        MOOD_HAPPY if (frame / 4).is_multiple_of(2) => {
            draw_heart(display, cx + 10, cy - 12);
        }
        _ => {}
    }
}

/// A tiny heart: two top pixels + a filled body wedge.
fn draw_heart<D>(display: &mut D, x: i32, y: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let fill = PrimitiveStyle::with_fill(BinaryColor::On);
    Rectangle::new(Point::new(x, y), Size::new(1, 1)).into_styled(fill).draw(display).ok();
    Rectangle::new(Point::new(x + 2, y), Size::new(1, 1)).into_styled(fill).draw(display).ok();
    Rectangle::new(Point::new(x, y + 1), Size::new(3, 1)).into_styled(fill).draw(display).ok();
    Rectangle::new(Point::new(x + 1, y + 2), Size::new(1, 1)).into_styled(fill).draw(display).ok();
}

/// Draw a short string at (x,y) in FONT_5X8, top baseline (the module's one text
/// helper — keeps every call site terse).
fn text<D>(display: &mut D, s: &str, x: i32, y: i32)
where
    D: DrawTarget<Color = BinaryColor>,
{
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    Text::with_baseline(s, Point::new(x, y), style, Baseline::Top)
        .draw(display)
        .ok();
}

/// A tiny heap-free formatted line (the game/render paths stay allocation-free).
struct TextLine {
    buf: [u8; 24],
    len: usize,
}

impl TextLine {
    fn new() -> Self {
        Self { buf: [0; 24], len: 0 }
    }
    fn as_str(&self) -> &str {
        ::core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl ::core::fmt::Write for TextLine {
    fn write_str(&mut self, s: &str) -> ::core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}
