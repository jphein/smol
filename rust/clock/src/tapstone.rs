//! TAPSTONE — the shrine app slot (smol issue 10, tapstone decision 0010).
//!
//! What this file is: the **seam**. It owns the three things a shrine must own — a [`Game`], a
//! [`Chain`], and the single place a [`Record`] is allowed to change either — and it draws a
//! deliberately ugly status page. What it is NOT is the battlefield UI: that layout is unresolved
//! (see the geometry note below), and Luna is designing it in tapstone. Anything pretty drawn here
//! now would have to be deleted, and would meanwhile look like a decision had been made.
//!
//! ── WHY THE STATE OWNERSHIP IS THE WHOLE POINT ────────────────────────────────────────────────
//! Two shrines on a table share a battlefield with no phone and no server (decision 0004). They do
//! not exchange game state — only tap events and an 8-byte chain head — and each recomputes the
//! state itself. That makes [`commit`](TapstoneApp::commit) the most load-bearing function in the
//! Tapstone firmware: it is the ONLY path from a `Record` to the game, so it is the only place the
//! chain can advance, and therefore the only place the two shrines can start to disagree. Every
//! future event source (mesh frame, card reader, touch) funnels through it. Do not add a second
//! one — a `Record` applied anywhere else advances the game WITHOUT advancing the chain, and the
//! two boards then differ with nothing to notice.
//!
//! ── ⚠️ GEOMETRY: DECISION 0010's BATTLEFIELD DOES NOT FIT THROUGH THIS SEAM ───────────────────
//! Decision 0010 specifies "3 vertical lanes, 48 px sprites, ~15 fps" and card faces as runtime
//! data in a flash partition. That cannot be drawn from a `clock` app, and the reason is
//! structural rather than a matter of effort: `app::Oled` on the S3 is [`crate::s3_oled::S3Oled`],
//! a **72×40 one-bit** logical framebuffer (360 bytes) that is scaled 4× and letterboxed to
//! 288×160 inside the 320×240 panel (#398, and the #152 "zero forked render code" gate is why it
//! is that shape). A 48 px sprite is taller than the entire 40 px logical surface, and there is no
//! colour: `BinaryColor`, one bit.
//!
//! So decision 0010 is right that the shrine is a framebuffer app and not a Slint scene — the
//! measurement behind it (74–79 ms per Slint page flip) stands — but the framebuffer it assumes
//! does not exist in `rust/clock`. Closing that needs an explicit ruling, and it is NOT this
//! issue's to make. The two shapes it could take:
//!   (a) the shrine renders in the **GUI flavor** (`targets/c6-watch`, board-esp32s3-cyd), which
//!       already drives the panel at full 320×240 RGB565 through mipidsi — where smol#540's scry
//!       station lives, and the flavor PARITY.md already calls right for a held-association kiosk;
//!   (b) `clock` gains a second, full-resolution colour display seam for the S3, which is a real
//!       architecture change (a 320×240 RGB565 image is 150 KB — it would have to live in the
//!       8 MB PSRAM, and #445/#447's "PSRAM registered FIRST" boot order becomes load-bearing for
//!       an app rather than just for the GUI flavor).
//! Until then this app draws 1-bit text, which is all the seam it is plugged into can express.
//!
//! ── WHAT IS STILL MISSING, AND WHOSE ISSUE IT IS ──────────────────────────────────────────────
//! - the `SMOLv1 MATCH ` frame family + `on_app_frame` → **issue 1**. [`TapstoneApp::on_match_frame`]
//!   is the seam it calls; it has no caller yet, hence the `dead_code` allow on it.
//! - group-MAC enforcement (`SealedMatch`) → **issue 2**. Until it lands, a `Record` arriving off
//!   the radio is UNAUTHENTICATED, and a forged commit is a forged match result. This app must not
//!   be wired to a live radio before that.
//! - house rules over CFG key `M` → **issue 4**. Until then [`HouseRules::default`] is used, and
//!   `Game::with_rules` is the (lobby-only) door it will come through.
//! - card reader presence + touch → **issue 8**. No seam is invented here for them on purpose:
//!   guessing the shape now would just be something for issue 8 to contradict.
//! - real decks and card data → not yet an issue. The game therefore sits in `Phase::Lobby` with
//!   two empty decks, which is the honest state of a shrine that has never been dealt a card.

use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{MonoTextStyleBuilder, ascii::FONT_5X8, ascii::FONT_6X10},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use tapstone_rules::state::SEATS;
use tapstone_rules::{Applied, Chain, Game, HouseRules, Phase, Record, Refusal, Winner};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;

/// Issue 10's `Game` <= 350 B budget, asserted **for the target this actually ships to**.
///
/// There is already a host test for this (`tapstone-rules`'s `game_fits_its_ram_budget`), and on
/// its own it is not quite evidence: it measures x86_64's layout, and the claim is about
/// `xtensa-esp32s3-none-elf`. `Game` contains no pointers and nothing wider than a `u16`, so the
/// two SHOULD agree — but "should agree" is an inference about the thing being measured, and this
/// file is compiled by the Xtensa build, so the inference can simply be replaced with a check.
///
/// `const _: () = assert!(...)` and not a runtime check or a test: it is evaluated by the compiler
/// that is targeting the chip, so a layout difference fails the FIRMWARE build with this message
/// rather than passing a host suite and shipping. Same construction as `budget.rs`'s chip asserts,
/// and for the same reason.
///
/// Measured exactly 350 B on both x86_64 and xtensa-esp32s3 (2026-09-21) — AT the bound, with zero
/// headroom. One more `u8` field in `Game` or `Seat` breaks this, which is the point of having it.
const _: () = assert!(
    core::mem::size_of::<Game>() <= 350,
    "size_of::<Game>() exceeds issue 10's 350 B budget ON THIS TARGET. Game is what every shrine \
     holds and what app.rs's `App` union is sized against, so this is a firmware-wide RAM cost. \
     Fix it in tapstone and re-vendor (tools/tapstone_vendor.sh --sync); do not raise the bound \
     without saying what the extra bytes buy and re-checking App's largest variant."
);

/// The shrine's game state. Owned by the [`crate::app::App`] union, so it lives on the stack with
/// every other screen's state and allocates nothing.
///
/// Size: `Game` is **350 B measured** (`tapstone-rules`'s `game_fits_its_ram_budget`, which is
/// also issue 10's budget and has exactly zero headroom left), plus an `Option<Chain>` and five
/// bytes of bookkeeping. `app.rs` puts `MeshSnake` at ~0.5 KB, so this should sit under the
/// variant the union is already sized to and cost no additional RAM — stated as an expectation,
/// not a measurement: nothing asserts `size_of::<App>()` today, on any tier. If a future card-set
/// cache pushes past MeshSnake the union grows for EVERY screen at once, so measure before
/// growing this struct.
pub struct TapstoneApp {
    game: Game,
    /// `None` until the game leaves the Lobby. `hash.rs`'s genesis contract: the chain starts when
    /// house rules are final, and lobby records are transcript-only and never enter it. Modelling
    /// that as an `Option` rather than an eagerly-built chain is what keeps this in step with the
    /// host engine's replay — see tapstone-sim's `replay()`.
    chain: Option<Chain>,
    committed: u16,
    refused: u16,
    /// Set by anything that changes what the page would show, so `update` repaints on a real
    /// change instead of once per tick. Same dedup shape as the other screens' `last_s`.
    dirty: bool,
}

impl TapstoneApp {
    pub fn new() -> Self {
        // Two empty decks and default house rules: a shrine that has been powered on and dealt
        // nothing. `Game::new` clamps a deck to `DECK_MAX` and `rules.deck_size`, so an empty one
        // is legal input, not a special case. Castles are `Game::new`'s defaults and are replaced
        // by whatever `ClaimSeat` carries (`rules.rs::claim_seat`), which is how a real seating
        // will set them — so there is nothing to invent here either.
        const NO_CARDS: &[u16] = &[];
        Self {
            game: Game::new(HouseRules::default(), [0; SEATS], [NO_CARDS; SEATS]),
            chain: None,
            committed: 0,
            refused: 0,
            dirty: true,
        }
    }

    /// **The arbiter loop's one step, and the only door into the game state.**
    ///
    /// Applies `r`, and on acceptance advances the chain in the same breath. The two are not
    /// separable: a record that changes the game without stepping the chain is a silent
    /// divergence, and one that steps the chain without changing the game is the same bug
    /// mirrored. Refusals do NOT advance the chain (they never happened, as far as the transcript
    /// is concerned) — matching the host arbiter, which records only accepted events.
    ///
    /// Held deliberately identical in shape to `tapstone-sim`'s `replay()`, because "the firmware
    /// engine and the host engine agree" is only true if the WALK over the records is the same
    /// walk. `rust/tapstone-rules/tests/vendor_golden_replay.rs` is the proof, and it replays a
    /// recorded match through exactly this sequence of calls.
    #[allow(dead_code)] // caller arrives with issue 1 (`on_app_frame`) and issue 8 (reader/touch).
    pub fn commit(&mut self, r: &Record) -> Result<Applied, Refusal> {
        let out = self.game.apply(r);
        self.dirty = true;
        match out {
            Ok(applied) => {
                if applied == Applied::Started {
                    // Genesis, taken once, AFTER the rules are final and BEFORE any state step.
                    self.chain = Some(Chain::genesis(&self.game.rules));
                } else if let Some(c) = self.chain.as_mut() {
                    c.step(r, &self.game);
                }
                self.committed = self.committed.saturating_add(1);
                Ok(applied)
            }
            Err(refusal) => {
                self.refused = self.refused.saturating_add(1);
                Err(refusal)
            }
        }
    }

    /// This shrine's chain head, or `None` while still in the Lobby. Live: `update` paints it.
    pub fn head(&self) -> Option<[u8; 8]> {
        self.chain.map(|c| c.head())
    }

    /// Does a peer's head agree with ours?
    ///
    /// `Some(true)` = the two shrines computed the same state from the same taps. `Some(false)` =
    /// **they have diverged**, which is unrecoverable by replay and is the condition the whole
    /// chained-hash design exists to make detectable — a divergent table must stop and say so, not
    /// keep playing two different games. `None` = we have no chain yet, so there is nothing to
    /// compare and no claim to make (deliberately not `false`: "I cannot tell" and "we disagree"
    /// are different answers and only one of them should abort a match).
    #[allow(dead_code)] // consumed by the pairing/commit handler (issues 1 + 5).
    pub fn agrees_with(&self, peer_head: [u8; 8]) -> Option<bool> {
        self.chain.map(|c| c.head() == peer_head)
    }

    /// Seam for issue 1's `on_app_frame`: a `MATCH` frame body carrying one wire record.
    ///
    /// Returns `None` if the body is not a decodable record — a 24-byte `Record` is the unit, and
    /// `Record::decode` already tolerates a LONGER body (SNK's length-tolerance rule, so a future
    /// field cannot break an old parser) while rejecting a short one. A malformed frame is dropped,
    /// never applied and never a panic: this is untrusted radio input.
    ///
    /// ⚠️ NOT SAFE TO WIRE YET. Issue 2 (`SealedMatch`) is what makes an inbound `MATCH` frame
    /// trustworthy; until it lands, anyone within ESP-NOW range can author a commit. The decode is
    /// here so issue 1 has something to call; the authorisation is issue 2's and is not stubbed
    /// permissively here, because a permissive stub is how a missing check ships.
    #[allow(dead_code)] // caller arrives with issue 1.
    pub fn on_match_frame(&mut self, body: &[u8]) -> Option<Result<Applied, Refusal>> {
        let r = Record::decode(body)?;
        Some(self.commit(&r))
    }
}

impl Plugin for TapstoneApp {
    fn on_button(&mut self, press: Press, _ctx: &mut Ctx) -> Transition {
        match press {
            // The uniform grammar (app.rs): long = change level, so long leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // Short has no meaning yet and is NOT given a placeholder one. The shrine's real
            // input is a card on the reader and a finger on the glass (issue 8); inventing a
            // button gesture now would be a third event path into a state machine whose whole
            // invariant is that there is exactly one (`commit`).
            Press::Short => Transition::Stay,
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        if !(ctx.redraw || self.dirty) {
            return;
        }
        self.dirty = false;

        let title = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(BinaryColor::On)
            .build();
        let small = MonoTextStyleBuilder::new()
            .font(&FONT_5X8)
            .text_color(BinaryColor::On)
            .build();

        ctx.display.clear(BinaryColor::Off).ok();

        // PROVISIONAL, and it should look it. 72×40 of 1-bit text is not a battlefield and is not
        // trying to be one — it is the bring-up instrument for the state machine above: which
        // phase, whose turn, both castles, and the chain head that has to match the other shrine.
        // The head is the field that matters: two shrines showing different hex here is the
        // divergence this design is built to catch, readable off the glass with no serial cable.
        Text::with_baseline("TAPSTONE?", Point::new(2, 0), title, Baseline::Top)
            .draw(ctx.display)
            .ok();

        let mut l1 = Line::new();
        match self.game.winner {
            Some(Winner::Seat(s)) => {
                let _ = write!(l1, "seat{s} wins r{}", self.game.round);
            }
            Some(Winner::Draw) => {
                let _ = write!(l1, "draw r{}", self.game.round);
            }
            None => {
                let _ = write!(
                    l1,
                    "{} r{} s{}",
                    phase_tag(self.game.phase),
                    self.game.round,
                    self.game.active
                );
            }
        }
        Text::with_baseline(l1.as_str(), Point::new(2, 11), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        let mut l2 = Line::new();
        let _ = write!(
            l2,
            "L{}/{} n{}",
            self.game.seats[0].castle.life, self.game.seats[1].castle.life, self.committed
        );
        if self.refused > 0 {
            let _ = write!(l2, " x{}", self.refused);
        }
        Text::with_baseline(l2.as_str(), Point::new(2, 21), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        // The chain head, or a visibly-absent marker. Four bytes is what fits and is plenty to
        // spot a disagreement by eye; the full 8 go on the wire.
        let mut l3 = Line::new();
        match self.head() {
            Some(h) => {
                let _ = write!(l3, "#{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3]);
            }
            // Not "#00000000": a zero that could be mistaken for a real head is the wrong kind of
            // placeholder. See budget.rs's poison row for the same reasoning at a larger scale.
            None => {
                let _ = write!(l3, "#lobby");
            }
        }
        Text::with_baseline(l3.as_str(), Point::new(2, 31), small, Baseline::Top)
            .draw(ctx.display)
            .ok();

        ctx.display.flush().ok();
    }
}

fn phase_tag(p: Phase) -> &'static str {
    match p {
        Phase::Lobby => "lobby",
        Phase::Playing => "play",
        Phase::Over => "over",
    }
}

/// Tiny heap-free line builder. A local copy for the same reason `about.rs` has one: the only
/// `pub` `Line` in the tree is `rssi::Line`, which is `espnow`-gated, and this app compiles in a
/// radio-less build (the rules engine needs no radio — see the `tapstone` feature in Cargo.toml).
struct Line {
    buf: [u8; 20],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self { buf: [0; 20], len: 0 }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}
