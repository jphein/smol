//! tapstone#132 (c) — the Tapstone shrine **station** (`--features tapstone-station`).
//!
//! A real shrine at the table: the seat is `tapstone_proto::shrine::Shrine`, the very state
//! machine the arena's tests run as `DeskShrine` (tapstone verification.md, "one object"), and
//! this file only carries its frames. Until an RC522 is wired to P3 and taps drive it, the seat
//! chooses with `shrine::Autoplay`, which trial-applies every candidate to the engine.
//!
//! ## One image for both boards
//!
//! `tapstone-station` implies `tapstone-gw`, so the station rides the gateway's radio posture
//! (WiFi off, channel pinned, never the crown) and its USB `@TS1` bridge. Where the arena is is
//! learned, not configured: the arena is whoever sends its frames (a lobby beacon naming no seat,
//! `B`, commits, `R`, rejects), and the station replies the way those frames reached it. The seat
//! addresses the arena as [`ARENA_NODE`]; this file maps that to the learned route.
//!
//! - **Arena on this board's USB** (id61, the arena's radio and seat A): the arena's `@TS1 TX`
//!   lines addressed to this node (or broadcast) are delivered here by `ts_send`, and the
//!   station's arena-bound frames go up as `@TS1 RX` lines from this node, exactly as if heard
//!   from the air. Nothing crosses the air between the arena and its own board's seat.
//! - **Arena across the air** (id62, seat B): frames come in off ESP-NOW from the arena's
//!   gateway node, and go back to that node through `send_to` with the #190 trailer.
//!
//! ## Console
//!
//! `[station]` lines (not `@TS1`, so the arena's line parser files them as logs): the seat, phase
//! and chain head every 2 s, and the final head once the match is over, which is what
//! tapstone's `radio_verify.py` compares against the arena's ledger.
//!
//! ## The interim arbiter (arena spec §7)
//!
//! The seat's `Dark` state (tapstone_proto, rules-v0.2.2) does it all; this file carries frames.
//! After 3 s with no arena frame the station goes dark; seat 0 (seat A, the arena's own board)
//! arbitrates as the interim, and seat 1 re-addresses to seat 0's node. That node is virtual on
//! the table (`TAPSTONE_STATION_NODE`, 161, behind gateway 61), so [`Tx`] learns header src →
//! link node from air frames and sends through the link. The revived arena's first commit ends it.
//! `TAPSTONE_NO_INTERIM=1` builds the control: no dark detection, so a dead arena stalls the match.
//!
//! ## The voice (tapstone 0033, S3 only)
//!
//! [`voice`] mounts the SD card read-only at boot and speaks the band from the pack
//! (`/TAPSTONE/VOICE/SET1`). Each line is found by the sha256 of its text. After a repaint,
//! `draw` asks for the band's line; while a clip plays, repaints wait (up to `VOICE_HOLD_MS`),
//! because a repaint outlasts the I²S ring. With no card, no pack or no codec, the band stays
//! text, and the boot log says which.
//!
//! ## Owed (tapstone#132)
//!
//! - **A seatless station in a dark window.** One that rebooted and kept nothing has no `B`, so no
//!   seat map: it broadcasts what it meant for the arena (proto's rule), but it only knows the
//!   arena is dark if something tells it; the silence detector runs in a match it plays.
//! - **Tag bindings that outlive a power-up.** [`taps`] binds fresh tags inline (the scry bridge's
//!   rule) and keeps them in RAM; nothing is written.
//! - **Voice from a card that is inserted after boot.** The card is mounted once, at boot.
use esp_println::println;

/// 0032's screens: the S3's colour panel only (nested so the tier-exclusion checker, #351, reads
/// the gate as `esp32s3` + `tapstone-station`).
#[cfg(feature = "esp32s3")]
pub mod screen;
/// SPI3's one owner: the SD slot and the P3 card reader (S3 only).
#[cfg(feature = "esp32s3")]
mod spi3;
/// 0033's voice from SD: the card, the pack, the codec and the I²S ring (S3 only).
#[cfg(feature = "esp32s3")]
pub mod voice;
/// Card taps: the RC522 on P3 drives the seat (S3 only; without one, `Autoplay`).
#[cfg(feature = "esp32s3")]
pub mod taps;
/// `stackfree N` on the status line: the main stack's untouched bytes (tapstone#132, all chips).
mod stackfree;
/// SPI3, shared by the SD slot and the reader (main builds it).
#[cfg(feature = "esp32s3")]
pub use spi3::{ReaderPins, Spi3};
use tapstone_proto::frame::{BROADCAST, FRAME_MAX, Frame, Lobby, Tap};
use tapstone_proto::shrine::{ARENA_NODE, Autoplay, Shrine};
use tapstone_rules::Phase;

use crate::net::mode::RadioManager;
use crate::net::ts_lines::{self, Show};

/// Frames queued between the radio drain / USB pump and the station's service.
const INBOX_CAP: usize = 8;
/// The lobby beacon's floor (tapstone `RadioShrine::BEACON_MS`); the arena's own is every 2 s.
const BEACON_MS: u64 = 250;
const STATUS_MS: u64 = 2_000;
/// After a match, the station holds the result this long before claiming the next seat (0032's
/// Result screen; and a table's second match starts only once its people have seen the first).
const RESULT_HOLD_MS: u64 = 10_000;
/// The arena's own lobby beacon names no seat (tapstone `core::lobby`); a shrine's names its index.
const ARENA_SEAT_PREF: u8 = 0xFF;
/// A repaint waits for a clip at most this long (set 1's longest clip is under 5 s). A repaint
/// needs the band buffer the I²S ring is playing from (screen::lend_band).
#[cfg(feature = "esp32s3")]
const VOICE_HOLD_MS: u64 = 8_000;

mod decks {
    include!(concat!(env!("OUT_DIR"), "/tapstone_decks.rs"));
}

/// A deck the station can hold: its castle and its list, in list order (copy `k` is `cards[k]`,
/// and its UID is `copy_uid(index, k)`). From tapstone's `decks/*.toml`, by build.rs.
struct DeckDef {
    name: &'static str,
    castle: u16,
    cards: &'static [u16],
}

/// The seat's deck and figurine index, baked at build time: `TAPSTONE_DECK` (`ember-neutral`,
/// the default, or `tide-neutral`) and `TAPSTONE_INDEX` (default 0). The arena's registry must
/// hold the same rows: `radio_shrine registry --deck <deck> --index <index>`.
fn deck_def() -> DeckDef {
    let want = option_env!("TAPSTONE_DECK").unwrap_or("ember-neutral");
    let &(name, castle, cards) = decks::DECKS
        .iter()
        .find(|d| d.0 == want)
        .unwrap_or(&decks::DECKS[0]);
    DeckDef { name, castle, cards }
}

/// How many copies the seat's deck list holds (the S3 reader's tag registry size).
#[cfg(feature = "esp32s3")]
pub fn deck_len() -> usize {
    deck_def().cards.len()
}

/// The seat's node id. On a board whose USB holds the arena it MUST differ from the mesh id: the
/// arena drops every frame from its own gateway's node as an echo (tapstone `core::mod`, `src ==
/// cfg.node`), so seat A on the arena's own board is `TAPSTONE_STATION_NODE` (161 at the table)
/// and the gateway delivers the arena's frames for that id locally. Elsewhere it defaults to the
/// mesh id, the id the arena's gateway resolves to this board's MAC over the air.
pub fn station_node(mesh_id: u8) -> u8 {
    match option_env!("TAPSTONE_STATION_NODE") {
        Some(s) => s.parse().unwrap_or(mesh_id),
        None => mesh_id,
    }
}

fn index() -> usize {
    match option_env!("TAPSTONE_INDEX") {
        Some(s) => s.parse().unwrap_or(0),
        None => 0,
    }
}

/// One queued MATCH frame: the link-layer sender's node id, whether it came from this board's
/// own USB (the arena is local), and the frame without its trailer.
#[derive(Clone, Copy)]
struct Queued {
    src: u8,
    local: bool,
    len: usize,
    buf: [u8; FRAME_MAX],
}

/// The radio's side of the station: `RadioManager` owns one, fills it from the RX drain and from
/// `ts_send`'s local delivery, and the station drains it.
pub struct Inbox {
    q: [Queued; INBOX_CAP],
    head: usize,
    len: usize,
    /// Frames dropped because the station fell behind (oldest first).
    pub dropped: u32,
}

impl Inbox {
    pub const fn new() -> Self {
        Self {
            q: [Queued {
                src: 0,
                local: false,
                len: 0,
                buf: [0; FRAME_MAX],
            }; INBOX_CAP],
            head: 0,
            len: 0,
            dropped: 0,
        }
    }

    /// Queue `frame` (trailer already stripped). A frame longer than `FRAME_MAX` is not a MATCH
    /// frame anyone sent and is dropped.
    pub fn push(&mut self, src: u8, local: bool, frame: &[u8]) {
        if frame.len() > FRAME_MAX {
            return;
        }
        if self.len == INBOX_CAP {
            self.head = (self.head + 1) % INBOX_CAP;
            self.len -= 1;
            self.dropped = self.dropped.saturating_add(1);
        }
        let slot = &mut self.q[(self.head + self.len) % INBOX_CAP];
        slot.src = src;
        slot.local = local;
        slot.len = frame.len();
        slot.buf[..frame.len()].copy_from_slice(frame);
        self.len += 1;
    }

    fn pop(&mut self) -> Option<Queued> {
        if self.len == 0 {
            return None;
        }
        let q = self.q[self.head];
        self.head = (self.head + 1) % INBOX_CAP;
        self.len -= 1;
        Some(q)
    }
}

/// Where the arena is, as learned from its own frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Route {
    Unknown,
    /// On this board's USB.
    Local,
    /// Across the air, behind the gateway on this node id.
    Air(u8),
}

/// Header src → link-layer node, learned from air frames: the table's seat A is a virtual node
/// (`TAPSTONE_STATION_NODE`, 161) behind the arena's gateway (61), and seat 1 addresses it by
/// its seat node in a dark window.
const LINKS: usize = 4;

/// The station's send side: where the arena is, and how to reach a node.
struct Tx {
    node: u8,
    route: Route,
    last_beacon: Option<u64>,
    links: [(u8, u8); LINKS],
    sent: u32,
    send_errors: u32,
}

impl Tx {
    fn learn(&mut self, hdr: u8, link: u8) {
        if hdr == link {
            return;
        }
        if let Some(e) = self.links.iter_mut().find(|e| e.0 == hdr || e.0 == 0) {
            *e = (hdr, link);
        }
    }

    fn link_for(&self, node: u8) -> u8 {
        self.links
            .iter()
            .find(|e| e.0 == node && node != 0)
            .map_or(node, |e| e.1)
    }

    fn send(&mut self, radio: &mut RadioManager, now: u64, dst: u8, bytes: &[u8]) {
        if matches!(Frame::decode(bytes), Some((_, Frame::Lobby(Lobby { .. })))) {
            if self.last_beacon.is_some_and(|t| now.saturating_sub(t) < BEACON_MS) {
                return;
            }
            self.last_beacon = Some(now);
        }
        let to_arena = dst == ARENA_NODE || dst == BROADCAST;
        // Up the USB when the arena is this board's own host.
        if to_arena && self.route == Route::Local {
            println!("{}", Show(|f| ts_lines::write_rx(f, self.node, 0, true, bytes)));
            self.sent += 1;
            if dst == ARENA_NODE {
                return;
            }
        }
        let air_dst = match (dst, self.route) {
            (BROADCAST, _) => BROADCAST,
            (ARENA_NODE, Route::Air(n)) => n,
            // The arena's whereabouts are unknown until it beacons (every 2 s): a claim sent
            // before then is re-sent every 100 ms by the seat, so dropping one costs nothing.
            (ARENA_NODE, _) => return,
            (d, _) => self.link_for(d),
        };
        match radio.ts_send(air_dst, bytes) {
            Ok(()) => self.sent += 1,
            Err(_) => self.send_errors = self.send_errors.saturating_add(1),
        }
    }
}

pub struct Station {
    shrine: Shrine<Autoplay>,
    /// 0032's screens: the S3's colour panel only. A C3 station is a headless seat.
    #[cfg(feature = "esp32s3")]
    screens: screen::Screens,
    /// 0033: the band, spoken from the SD pack (text only without a card, pack or codec).
    #[cfg(feature = "esp32s3")]
    voice: &'static mut voice::Voice,
    /// When the current clip began holding repaints back.
    #[cfg(feature = "esp32s3")]
    held_since: Option<u64>,
    /// The RC522 on P3, if one answered at boot: then taps drive the seat, not `Autoplay`.
    #[cfg(feature = "esp32s3")]
    reader: Option<&'static mut taps::Reader>,
    tx: Tx,
    next_status: u64,
    reported_over: Option<u32>,
    /// The last match whose `R` this station heard (the seat's own flag resets with the lobby).
    heard_result: Option<u32>,
    /// When this station first saw its match over (the result hold starts there).
    over_at: Option<u64>,
    /// `TAPSTONE_NO_PROPOSE=1` at build time: the stall control, a seat that proposes nothing.
    no_propose: bool,
}

impl Station {
    /// The station for this board's node id `node`. On the S3, `voice` is the hardware the band
    /// speaks through.
    ///
    /// The station lives in a static and `run` holds a reference. Built by value, the whole seat
    /// (the follower's game, chain and logs) sat in the main task's poll frame on the S3, which
    /// measured 37,952 B before the voice. That left the boot paint's callees ~13 KB of a 51 KB
    /// stack region, and the voice's `.bss` (~12 KB) took the rest: a stack-guard panic at boot
    /// on board 61, 2026-09-30. Not inlined, so its temporaries stay out of `run`'s frame too.
    #[inline(never)]
    pub fn new(
        node: u8,
        #[cfg(feature = "esp32s3")] voice: &'static mut voice::Voice,
        #[cfg(feature = "esp32s3")] reader: Option<&'static mut taps::Reader>,
    ) -> &'static mut Self {
        static STATION: static_cell::StaticCell<Station> = static_cell::StaticCell::new();
        // After the radio's boot peak (build_radio) and before the station's service loop: what
        // the status line reports is the headroom of the running station.
        stackfree::paint();
        let d = deck_def();
        let seed = u64::from(node) << 8 | index() as u64;
        let mut shrine = Shrine::new(seed, index(), node, d.castle, d.cards, Autoplay::new(seed));
        // The arena behind a gateway resolves every cast and charge by UID (strict registry).
        shrine.stamp_uids = true;
        // A real shrine stays at the table: back to the lobby after each match.
        shrine.rematch = true;
        // The interim role (arena spec §7): on unless built `TAPSTONE_NO_INTERIM=1`, the control
        // for the arena kill/restart run (a dark window nobody arbitrates).
        shrine.dark.detect = !matches!(option_env!("TAPSTONE_NO_INTERIM"), Some("1"));
        println!(
            "[station] node {} index {} deck {} castle {} ({} cards)",
            node,
            index(),
            d.name,
            d.castle,
            d.cards.len()
        );
        if matches!(option_env!("TAPSTONE_NO_PROPOSE"), Some("1")) {
            println!("[station] NO-PROPOSE build: the stall control, this seat plays nothing");
        }
        if matches!(option_env!("TAPSTONE_NO_INTERIM"), Some("1")) {
            println!("[station] NO-INTERIM build: the arena-dark control, no dark detection");
        }
        #[cfg(feature = "esp32s3")]
        let faction = if d.name.starts_with("tide") {
            tapstone_rules::Faction::Tide
        } else {
            tapstone_rules::Faction::Ember
        };
        STATION.init(Self {
            shrine,
            #[cfg(feature = "esp32s3")]
            screens: screen::Screens::new(node, faction),
            #[cfg(feature = "esp32s3")]
            voice,
            #[cfg(feature = "esp32s3")]
            held_since: None,
            #[cfg(feature = "esp32s3")]
            reader,
            tx: Tx {
                node,
                route: Route::Unknown,
                last_beacon: None,
                links: [(0, 0); LINKS],
                sent: 0,
                send_errors: 0,
            },
            next_status: 0,
            reported_over: None,
            heard_result: None,
            over_at: None,
            no_propose: matches!(option_env!("TAPSTONE_NO_PROPOSE"), Some("1")),
        })
    }

    /// Drain the inbox into the seat, then tick it; send whatever it says.
    pub fn service(&mut self, radio: &mut RadioManager, now: u64) {
        while let Some(q) = radio.ts_inbox.pop() {
            let Some((h, f)) = Frame::decode(&q.buf[..q.len]) else {
                continue;
            };
            if !q.local {
                self.tx.learn(h.src, q.src);
            }
            // The arena is recognised by what it sends, not by its header `src`: behind a gateway
            // it speaks as the gateway's node (61 at this table), and 200 only on a desk mesh. A
            // seat's node is never the arena's (the interim's commits come from seat 0's).
            let seat_node = self.shrine.follower.begun().is_some()
                && self.shrine.follower.nodes().contains(&h.src);
            let from_arena = !seat_node
                && match &f {
                    Frame::Lobby(l) => l.seat_pref == ARENA_SEAT_PREF,
                    Frame::Begin(_) | Frame::Commit(_) | Frame::Result(_) => true,
                    Frame::Tap(Tap::Reject { .. }) => true,
                    _ => false,
                };
            if from_arena {
                let route = if q.local { Route::Local } else { Route::Air(q.src) };
                if route != self.tx.route {
                    println!("[station] arena heard: {:?}", route);
                    self.tx.route = route;
                }
            }
            if let Frame::Result(_) = f
                && self.heard_result != Some(h.match_id)
            {
                self.heard_result = Some(h.match_id);
                println!("[station] RESULT match {:08x}", h.match_id);
            }
            let dark_before = self.shrine.dark.on;
            let tx = &mut self.tx;
            self.shrine
                .rx_to(&h, &f, &mut |dst, bytes| tx.send(radio, now, dst, bytes));
            if dark_before && !self.shrine.dark.on {
                println!("[station] dark ENDS: the arena's commit (handover)");
            }
        }
        // Feed the voice between the long steps too: one service() call measured up to 129 ms
        // on the table, near the 139 ms ring.
        #[cfg(feature = "esp32s3")]
        self.voice.service(crate::millis());
        // The result hold: no claim for RESULT_HOLD_MS after the match is seen over. The lobby
        // beacon still goes out, so the arena keeps the shrine on its table.
        if self.shrine.follower.game.phase == Phase::Over {
            self.over_at.get_or_insert(now);
        } else if self.shrine.follower.game.phase == Phase::Playing {
            self.over_at = None;
        }
        let may_claim = self
            .over_at
            .is_none_or(|t| now.saturating_sub(t) >= RESULT_HOLD_MS);
        let dark_before = self.shrine.dark.on;
        // A reader makes the seat manual: its taps are the moves, so `act_to` proposes nothing.
        #[cfg(feature = "esp32s3")]
        let manual = self.no_propose || self.reader.is_some();
        #[cfg(not(feature = "esp32s3"))]
        let manual = self.no_propose;
        let tx = &mut self.tx;
        self.shrine.act_to(now, may_claim, manual, &mut |dst, bytes| {
            tx.send(radio, now, dst, bytes)
        });
        #[cfg(feature = "esp32s3")]
        self.voice.service(crate::millis());
        #[cfg(feature = "esp32s3")]
        self.service_taps(radio, now);
        if !dark_before && self.shrine.dark.on {
            println!(
                "[station] dark BEGINS: no arena frame for {} ms; interim={}",
                tapstone_proto::shrine::DARK_MS,
                self.shrine.seat() == Some(0)
            );
        }
        #[cfg(feature = "esp32s3")]
        self.voice.service(crate::millis());
        self.status(now);
    }

    /// Top the voice's I2S ring up (between the superloop's subtick slices, as well as in
    /// `service`).
    #[cfg(feature = "esp32s3")]
    pub fn feed_voice(&mut self, now: u64) {
        self.voice.service(now);
    }

    /// The reader's tap, if one came, proposed; and a manual seat's stale proposal, again.
    #[cfg(feature = "esp32s3")]
    fn service_taps(&mut self, radio: &mut RadioManager, now: u64) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let (send, line) = reader.service(now, &self.shrine);
        let send = send.or_else(|| taps::stale(&self.shrine, now).inspect(|_| self.shrine.pending = None));
        if let Some(line) = line {
            self.voice.say(line.text().as_str(), now);
        }
        if let Some(r) = send {
            let tx = &mut self.tx;
            let sent = self.shrine.propose_to(now, r, &mut |dst, bytes| tx.send(radio, now, dst, bytes));
            println!("[taps] propose {:?} card {} lane {} target {} -> {}", r.kind, r.card, r.lane, r.target, if sent { "sent" } else { "not sent" });
        }
    }

    /// Repaint 0032's screen if what it shows changed (the S3's colour panel), then say the band.
    /// The text shows first and the voice follows it; while a clip plays, repaints wait (up to
    /// [`VOICE_HOLD_MS`]) so the ring never runs dry under a paint.
    #[cfg(feature = "esp32s3")]
    pub fn draw(&mut self, panel: &mut crate::s3_oled::Panel, now: u64) {
        if self.voice.playing() {
            let since = *self.held_since.get_or_insert(now);
            if now.saturating_sub(since) < VOICE_HOLD_MS {
                return;
            }
            // Waited long enough: the paint needs the band buffer the clip is playing from.
            self.voice.cut();
        }
        self.held_since = None;
        let s = &self.shrine;
        self.screens.update(
            panel,
            now,
            &s.follower.game,
            s.seat(),
            self.tx.route != Route::Unknown,
            s.follower.next_mseq(),
            s.follower.begun().unwrap_or(0),
        );
        let text = screen::band_text(&s.follower.game, s.seat(), s.dark.on);
        self.voice.say(text.as_str(), now);
    }

    fn status(&mut self, now: u64) {
        let s = &self.shrine;
        let g = &s.follower.game;
        let over = g.phase == Phase::Over;
        let final_now = over && s.follower.begun().is_some() && self.reported_over != s.follower.begun();
        if now < self.next_status && !final_now {
            return;
        }
        self.next_status = now + STATUS_MS;
        let head = s.follower.head_hash();
        println!(
            "[station] {} match {:08x} seat {:?} phase {:?} round {} mseq {} head {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} route {:?} sent {} err {} dark {} stackfree {}",
            if final_now { "FINAL" } else { "status" },
            s.follower.begun().unwrap_or(0),
            s.seat(),
            g.phase,
            g.round,
            s.follower.next_mseq(),
            head[0], head[1], head[2], head[3], head[4], head[5], head[6], head[7],
            self.tx.route,
            self.tx.sent,
            self.tx.send_errors,
            u32::from(self.shrine.dark.on),
            // -1 = never painted (cannot happen: `new` paints). Bytes, not words.
            stackfree::free().map_or(-1, i64::from),
        );
        if final_now {
            self.reported_over = s.follower.begun();
        }
    }
}
