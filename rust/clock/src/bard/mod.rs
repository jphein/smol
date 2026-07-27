//! bard (#300): The Bard — an on-device tiny-LLM storyteller.
//!
//! Spec: `docs/superpowers/specs/2026-07-26-tiny-llm-story-design.md`. A 260K-parameter
//! TinyStories transformer, int8-quantized, running entirely on the ESP32-C3 with no radio and
//! no heap: the weights stay in flash (`.rodata`, XIP — never copied to RAM) and every scratch
//! buffer is a `static mut` in `.bss`.
//!
//! Everything big lives HERE, at module level, never in the `App` union: `App` is an enum whose
//! size is its largest variant, so a 96 KB screen state would make EVERY screen 96 KB.
//! [`BardApp`] therefore holds two bools and reaches the real state through these statics.
//!
//! This file is the firmware root of the bard tree; `lib.rs` reaches the same three source
//! files directly via `#[path]` for host tests, so nothing here may be needed by them.

pub mod nano_llm;
pub mod persona;
pub mod textflow;
pub mod tokenizer;

use embedded_graphics::{
    mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Plugin, Transition};
use crate::input::Press;
use nano_llm::{Bufs, Model, StepOut, Story};
use tokenizer::Tokenizer;

/// The 277 KB SBRD blob. `include_bytes!` puts it in `.rodata`, which on this chip is
/// execute-in-place flash — it is READ from flash on demand and never copied to RAM (spec §3),
/// which is the only reason a 277 KB model fits a 400 KB-RAM microcontroller at all.
static MODEL_BLOB: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/model/stories260K-q8.bin"
));

/// The forward pass's scratch — the single big RAM cost (spec §5). `.bss`, so it costs no flash.
static mut BUFS: Bufs = Bufs::INIT;
/// The story so far, as decoded bytes. Sized for a full 220-token story of short tokens.
static mut STORY_TEXT: [u8; 1024] = [0; 1024];
/// Bytes of [`STORY_TEXT`] in use.
static mut STORY_LEN: usize = 0;
/// Parsed model header + borrowed section views (cheap — the blob itself is not copied).
static mut MODEL: Option<Model<'static>> = None;
/// ~2.6 KB of offset/byte-index tables. A static, never a stack local: it would blow the
/// interrupt-sized stacks this firmware runs on.
static mut TOKENIZER: Option<Tokenizer<'static>> = None;
/// The generator, including its 1 KB sampler scratch (~1.1 KB total).
static mut STORY: Option<Story> = None;

/// Build the statics from the flash blob. `false` ⇒ the blob failed its integrity or geometry
/// checks and the screen must stay mute rather than render whatever the bytes happen to say.
///
/// # Safety
/// Single-threaded firmware, called only from [`BardApp::new`]; nothing else touches these
/// statics while this runs. Uses `addr_of_mut!` (the house idiom) rather than `&mut` on a
/// `static mut`, so no reference to the static is ever materialised.
unsafe fn init_statics() -> bool {
    let Ok(model) = Model::parse(MODEL_BLOB) else {
        return false;
    };
    let Some(tok) = Tokenizer::new(model.tok_table, model.cfg.vocab) else {
        return false;
    };
    core::ptr::addr_of_mut!(MODEL).write(Some(model));
    core::ptr::addr_of_mut!(TOKENIZER).write(Some(tok));
    core::ptr::addr_of_mut!(STORY_LEN).write(0);
    true
}

/// Start (or restart) a story for this node's protagonist, seeded from the clock so a second
/// telling differs.
///
/// # Safety
/// As [`init_statics`] — single-threaded, called only from the Bard screen's own handlers.
unsafe fn begin_story(node_id: u8, now_ms: u64) {
    let Some(tok) = (*core::ptr::addr_of!(TOKENIZER)).as_ref() else {
        return;
    };
    let mut buf = [0u8; 64];
    let n = persona::prompt(node_id, &mut buf);
    // The prompt is ASCII by construction (persona.rs), so this cannot fail; `unwrap_or` keeps
    // the screen panic-free even if that ever changes.
    let prompt = core::str::from_utf8(&buf[..n]).unwrap_or("Once upon a time");
    let story = Story::new(tok, prompt, now_ms as u32);
    core::ptr::addr_of_mut!(STORY).write(Some(story));
    core::ptr::addr_of_mut!(STORY_LEN).write(0);
    // Seed the visible text with the prompt itself, so the screen reads as a story from the
    // first frame instead of starting mid-sentence.
    let text = &mut *core::ptr::addr_of_mut!(STORY_TEXT);
    let len = n.min(text.len());
    text[..len].copy_from_slice(&buf[..len]);
    core::ptr::addr_of_mut!(STORY_LEN).write(len);
}

/// Append `extra` to the visible story text, truncating rather than wrapping.
///
/// # Safety
/// As [`init_statics`].
unsafe fn push_text(extra: &[u8]) -> bool {
    let text = &mut *core::ptr::addr_of_mut!(STORY_TEXT);
    let len = *core::ptr::addr_of!(STORY_LEN);
    let room = text.len().saturating_sub(len);
    let n = extra.len().min(room);
    text[len..len + n].copy_from_slice(&extra[..n]);
    core::ptr::addr_of_mut!(STORY_LEN).write(len + n);
    // Out of buffer is a stop condition too — never wrap, never truncate mid-token.
    room > extra.len()
}

/// Advance the story by ONE token, appending to [`STORY_TEXT`]. Returns `true` while more is
/// coming.
///
/// # Safety
/// As [`init_statics`].
unsafe fn step_story() -> bool {
    let (Some(model), Some(tok)) = (
        (*core::ptr::addr_of!(MODEL)).as_ref(),
        (*core::ptr::addr_of!(TOKENIZER)).as_ref(),
    ) else {
        return false;
    };
    let Some(story) = (*core::ptr::addr_of_mut!(STORY)).as_mut() else {
        return false;
    };
    let bufs = &mut *core::ptr::addr_of_mut!(BUFS);
    let more = match story.step(model, tok, bufs) {
        StepOut::Working => true,
        StepOut::Text(bytes) => push_text(bytes),
        // A cut is the usual ending at this model size (T8: ~19 of 20 seeds), so mark it —
        // trailing dots read as tailing off, where a bare mid-sentence stop reads as a crash.
        // ASCII dots, not U+2026: FONT_5X8 is an ASCII font and would draw the ellipsis as a
        // blank, silently losing the very signal we are adding.
        StepOut::Done { truncated } => {
            if truncated {
                push_text(b"...");
            }
            false
        }
    };
    // Belt and braces: the state machine's own view must agree that there is more to come.
    more && !story.is_done()
}

/// Reveal one character per this many ms (~6/s — spec §7's reading pace).
const REVEAL_MS: u64 = 160;
/// Quill blink half-period.
const BLINK_MS: u64 = 400;
/// FONT_5X8 columns across the 72 px panel (14 × 5 px = 70).
const COLS: usize = 14;
/// FONT_5X8 rows down the 40 px panel (5 × 8 px = 40).
const ROWS: usize = 5;
/// Pixel advance of one FONT_5X8 glyph.
const GLYPH_W: i32 = 5;
/// Pixel height of one text row.
const ROW_H: i32 = 8;

/// Where the screen is in a story's life.
///
/// No `Idle`: [`BardApp::new`] either arms a story (`Composing`) or fails the blob (`Mute`), so
/// an idle variant would be constructed nowhere and read as dead code.
enum Phase {
    /// Generating and typing out.
    Composing,
    /// Generation finished; may still be revealing the tail.
    Told,
    /// The blob failed its checks — render nothing but the notice.
    Mute,
}

/// The Bard screen. Small by design: `App` is a stack union sized to its largest variant, so the
/// model scratch, tokenizer, `Story` and text buffer all live in this module's statics.
///
/// (The plan sketched a `story: Option<Story>` field here. Deliberately not done: `Story` is
/// 1136 B — it carries the 1 KB sampler scratch — which would more than double the union, on the
/// very stack that overflowed when `SEQ_CAP` was 256.)
pub struct BardApp {
    phase: Phase,
    /// Bytes of [`STORY_TEXT`] revealed so far.
    shown: u16,
    /// When the next character is due.
    next_reveal_ms: u64,
    /// Quill state at the last paint (a repaint trigger).
    quill_on: bool,
    /// `shown` at the last paint (a repaint trigger).
    painted: u16,
    /// Set by a tap while composing: stop throttling and catch the reveal up (spec §9).
    fast: bool,
}

impl BardApp {
    /// Parse the blob and arm the first story. Called once per entry to the screen.
    pub fn new(ctx: &Ctx) -> Self {
        let ok = unsafe { init_statics() };
        let mut app = BardApp {
            phase: if ok { Phase::Composing } else { Phase::Mute },
            shown: 0,
            next_reveal_ms: ctx.now_ms,
            quill_on: false,
            painted: u16::MAX, // force the first paint
            fast: false,
        };
        if ok {
            app.restart(ctx.node_id, ctx.now_ms);
        }
        app
    }

    /// Begin a fresh story, seeded from the clock so successive tellings differ.
    fn restart(&mut self, node_id: u8, now_ms: u64) {
        unsafe { begin_story(node_id, now_ms) };
        self.phase = Phase::Composing;
        self.shown = 0;
        self.next_reveal_ms = now_ms;
        self.painted = u16::MAX;
        self.fast = false;
    }
}

impl Plugin for BardApp {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar across screens: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                match self.phase {
                    // Mid-story: skip the typewriter rather than start over — the story you are
                    // reading is the one you asked for.
                    Phase::Composing => self.fast = true,
                    // Finished: tell a new one.
                    Phase::Told => self.restart(ctx.node_id, ctx.now_ms),
                    Phase::Mute => {}
                }
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        if matches!(self.phase, Phase::Mute) {
            if ctx.redraw {
                draw_lines(ctx, &["the bard", "is mute"]);
            }
            return;
        }

        // Generate: ONE forward pass per tick, free-running. The REVEAL is what is paced — the
        // model is the slow part, so throttling generation too would only stall the screen.
        if matches!(self.phase, Phase::Composing) && !unsafe { step_story() } {
            self.phase = Phase::Told;
        }
        let text_len = unsafe { *core::ptr::addr_of!(STORY_LEN) } as u16;

        // Reveal on a wall-clock schedule (`next_reveal_ms` accumulates rather than resetting to
        // `now`), so a slow token or a missed tick catches up to ~6 chars/s instead of drifting.
        if self.fast {
            self.shown = text_len;
        } else {
            while self.shown < text_len && ctx.now_ms >= self.next_reveal_ms {
                self.shown += 1;
                self.next_reveal_ms = self.next_reveal_ms.saturating_add(REVEAL_MS);
            }
        }

        // Repaint only when something a viewer can see changed.
        let told = matches!(self.phase, Phase::Told) && self.shown >= text_len;
        let quill = !told && (ctx.now_ms / BLINK_MS).is_multiple_of(2);
        if !(ctx.redraw || self.shown != self.painted || quill != self.quill_on) {
            return;
        }
        self.painted = self.shown;
        self.quill_on = quill;
        draw_story(ctx, self.shown, told, quill);
    }
}

/// Draw the revealed text, word-wrapped, with the quill while composing and `~ fin ~` once the
/// story is fully told.
fn draw_story(ctx: &mut Ctx, shown: u16, told: bool, quill: bool) {
    let text = unsafe { &*core::ptr::addr_of!(STORY_TEXT) };
    let visible = &text[..(shown as usize).min(text.len())];
    // Reserve the bottom row for the ending, so `~ fin ~` never overwrites a line of story.
    let rows = if told { ROWS - 1 } else { ROWS };
    let mut spans = [(0u16, 0u16); ROWS];
    let n = textflow::wrap_tail(visible, COLS, rows, &mut spans[..rows]);

    ctx.display.clear(BinaryColor::Off).ok();
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    let mut last_end = 0i32;
    for (i, &(a, b)) in spans[..n].iter().enumerate() {
        // A line that is not valid UTF-8 (a raw byte-fallback token) is skipped rather than
        // panicked on; the rest of the story still reads.
        let line = core::str::from_utf8(&visible[a as usize..b as usize]).unwrap_or("");
        Text::with_baseline(
            line,
            Point::new(0, i as i32 * ROW_H),
            style,
            Baseline::Top,
        )
        .draw(ctx.display)
        .ok();
        last_end = line.chars().count() as i32;
    }
    if quill && n > 0 {
        // The nib sits after the last revealed character, clamped inside the panel.
        let x = (last_end * GLYPH_W).min(COLS as i32 * GLYPH_W - GLYPH_W);
        Text::with_baseline(
            "|",
            Point::new(x, (n as i32 - 1) * ROW_H),
            style,
            Baseline::Top,
        )
        .draw(ctx.display)
        .ok();
    }
    if told {
        const FIN: &str = "~ fin ~";
        let x = ((COLS as i32 * GLYPH_W) - FIN.len() as i32 * GLYPH_W) / 2;
        Text::with_baseline(
            FIN,
            Point::new(x.max(0), (ROWS as i32 - 1) * ROW_H),
            style,
            Baseline::Top,
        )
        .draw(ctx.display)
        .ok();
    }
    ctx.display.flush().ok();
}

/// Clear and draw up to [`ROWS`] left-aligned FONT_5X8 lines. Panic-free.
fn draw_lines(ctx: &mut Ctx, lines: &[&str]) {
    ctx.display.clear(BinaryColor::Off).ok();
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    for (i, line) in lines.iter().take(ROWS).enumerate() {
        Text::with_baseline(line, Point::new(0, i as i32 * ROW_H), style, Baseline::Top)
            .draw(ctx.display)
            .ok();
    }
    ctx.display.flush().ok();
}
