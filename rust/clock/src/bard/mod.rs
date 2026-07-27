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
        // A cut is the usual ending at this model size (~19 of 20 seeds), so mark it: an
        // ellipsis reads as trailing off, where a bare mid-sentence stop reads as a crash.
        StepOut::Done { truncated } => {
            if truncated {
                push_text(b" \xe2\x80\xa6");
            }
            false
        }
    };
    // Belt and braces: the state machine's own view must agree that there is more to come.
    more && !story.is_done()
}

/// The Bard screen. Two bools by design: see the module doc on the `App` union's size.
pub struct BardApp {
    /// The blob did not parse — refuse to render anything rather than show garbage.
    mute: bool,
    /// More tokens are coming (drives the per-tick step).
    telling: bool,
}

impl BardApp {
    /// Parse the blob and arm the generator. Called once per entry to the screen.
    pub fn new(ctx: &Ctx) -> Self {
        let ok = unsafe { init_statics() };
        if ok {
            unsafe { begin_story(ctx.node_id, ctx.now_ms) };
        }
        BardApp {
            mute: !ok,
            telling: ok,
        }
    }
}

impl Plugin for BardApp {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar across screens: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            // A tap tells a NEW story (a different seed) — the one interaction this screen has.
            Press::Short => {
                if !self.mute {
                    unsafe { begin_story(ctx.node_id, ctx.now_ms) };
                    self.telling = true;
                    ctx.redraw = true;
                }
                Transition::Stay
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        if self.mute {
            // Static content: paint once per entry/redraw.
            if ctx.redraw {
                draw_lines(ctx, &["the bard", "is mute"]);
            }
            return;
        }
        // ONE forward pass per tick — the loop must stay cooperative, so the story advances a
        // token at a time rather than blocking for seconds.
        if self.telling {
            self.telling = unsafe { step_story() };
        } else if !ctx.redraw {
            return;
        }
        draw_story(ctx);
    }
}

/// Render the tail of the story, unwrapped. Deliberately plain: Task 10 replaces this with the
/// word-wrapped typewriter, the quill cursor and the `~ fin ~` ending.
fn draw_story(ctx: &mut Ctx) {
    let (text, len) = unsafe {
        (
            &*core::ptr::addr_of!(STORY_TEXT),
            *core::ptr::addr_of!(STORY_LEN),
        )
    };
    // 14 columns of FONT_5X8 across 72 px; show the last 4 lines' worth of characters.
    const COLS: usize = 14;
    const ROWS: usize = 4;
    let shown = &text[len.saturating_sub(COLS * ROWS)..len];
    let mut rows: [&str; ROWS] = [""; ROWS];
    for (i, row) in rows.iter_mut().enumerate() {
        let a = (i * COLS).min(shown.len());
        let b = ((i + 1) * COLS).min(shown.len());
        // from_utf8 can fail mid-multi-byte-token; skip that row rather than panic.
        *row = core::str::from_utf8(&shown[a..b]).unwrap_or("");
    }
    draw_lines(ctx, &rows);
}

/// Clear and draw up to 5 left-aligned FONT_5X8 lines. Panic-free.
fn draw_lines(ctx: &mut Ctx, lines: &[&str]) {
    ctx.display.clear(BinaryColor::Off).ok();
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    let mut y = 0i32;
    for line in lines.iter().take(5) {
        Text::with_baseline(line, Point::new(0, y), style, Baseline::Top)
            .draw(ctx.display)
            .ok();
        y += 8;
    }
    ctx.display.flush().ok();
}
