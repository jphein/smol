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
// Bench-only stack measurement. Gated on the feature in the FIRMWARE tree: with `stack-paint`
// off nothing calls it, and an uncompiled module beats nine `dead_code` allows. The host lib
// exports the same file separately (lib.rs) so its pure scanner stays testable regardless.
#[cfg(feature = "stack-paint")]
pub mod stack_paint;
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
/// The story so far, as decoded bytes. Sized for a full 220-token story of short tokens — and
/// since #302 a story can run for as many chapters as the reader presses for, so this is a ROLLING
/// window: each continuation drops the oldest text ([`compact_text`]) rather than growing the
/// buffer. The panel only ever renders the tail, so the cap is on scrollback nobody can reach.
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

/// #303 the operator's story opening, set at runtime over CFG key `T` (no reflash). Empty ⇒ the
/// node uses its built-in per-node persona prompt, so clearing the retained topic restores the
/// default — the same "empty = board default" convention as the `S` screen key (#21).
/// 64 B to match both the prompt buffer and `CFG_VALUE_MAX`; costs no flash (`.bss`).
static mut PROMPT: [u8; 64] = [0; 64];
/// Bytes of [`PROMPT`] in use; 0 ⇒ no override.
static mut PROMPT_LEN: usize = 0;
/// Whether [`PROMPT`] has been through the vocabulary check. A prompt can arrive before the model
/// is built (retained config lands on the first gateway burst; the model loads lazily on first
/// screen entry), so an unchecked prompt is validated at first use instead of being thrown away.
/// Only the tiers that can RECEIVE a prompt have anything to stage (see `checked_staged_prompt`).
#[cfg(any(feature = "espnow", feature = "hostsim"))]
static mut PROMPT_CHECKED: bool = true;

/// Offer a runtime story prompt (CFG `T`). Validated against the MODEL'S OWN vocabulary before
/// it is stored, so a prompt that would derail generation is refused and the previous value
/// kept — see `persona::validate_prompt` for the two hazards and which one is fatal.
///
/// An empty value CLEARS the override (back to the per-node default). Returns the accepted
/// token count, or the reason it was refused, so the caller can log something actionable.
///
/// # Safety
/// As [`init_statics`] — single-threaded; called only from `main`'s config-apply path.
#[cfg(any(feature = "espnow", feature = "hostsim"))]
pub unsafe fn set_prompt(value: &[u8]) -> Result<Option<usize>, persona::PromptErr> {
    if value.is_empty() {
        core::ptr::addr_of_mut!(PROMPT_LEN).write(0);
        core::ptr::addr_of_mut!(PROMPT_CHECKED).write(true);
        return Ok(Some(0));
    }
    // Cheap checks first — these need no model, so they give a straight answer even at boot.
    if value.len() > PROMPT.len() {
        return Err(persona::PromptErr::TooLong { got: value.len() });
    }
    if core::str::from_utf8(value).is_err() {
        return Err(persona::PromptErr::NotUtf8);
    }
    // The vocabulary check needs the tokenizer, and the config can arrive BEFORE the Bard screen
    // has ever been opened (retained MQTT is delivered on the first gateway burst, while the
    // model is built lazily on first entry — measured on id8: the prompt landed at boot). So
    // when the model is not up yet, STAGE the bytes and let `begin_story` validate them once the
    // tokenizer exists. Refusing here instead would drop a perfectly good prompt for good, since
    // a retained value is only re-offered when it CHANGES.
    let staged = match (*core::ptr::addr_of!(TOKENIZER)).as_ref() {
        Some(tok) => {
            // Model is up: validate BEFORE storing, so a refusal leaves the previous prompt intact.
            Some(persona::validate_prompt(tok, value)?)
        }
        None => None,
    };
    let buf = &mut *core::ptr::addr_of_mut!(PROMPT);
    let len = value.len().min(buf.len());
    buf[..len].copy_from_slice(&value[..len]);
    core::ptr::addr_of_mut!(PROMPT_LEN).write(len);
    core::ptr::addr_of_mut!(PROMPT_CHECKED).write(staged.is_some());
    Ok(staged)
}

/// Vocabulary-check a prompt that was STAGED before the model existed, returning the length to
/// use (0 ⇒ refused, fall back to this node's persona). Split out of [`begin_story`] because
/// `persona::validate_prompt` only exists on the tiers that can receive a CFG offer — inlined, it
/// made the `bard`-only tier fail to compile, which is how it shipped in #303.
#[cfg(any(feature = "espnow", feature = "hostsim"))]
unsafe fn checked_staged_prompt(tok: &Tokenizer<'_>, over: usize) -> usize {
    if over == 0 || core::ptr::addr_of!(PROMPT_CHECKED).read() {
        return over;
    }
    let src = &*core::ptr::addr_of!(PROMPT);
    let kept = match persona::validate_prompt(tok, &src[..over]) {
        Ok(n) => {
            log::info!("smol #303: staged story prompt accepted ({} tokens)", n);
            over
        }
        Err(e) => {
            log::warn!(
                "smol #303: staged story prompt REFUSED ({:?}) — using this node's default",
                e
            );
            core::ptr::addr_of_mut!(PROMPT_LEN).write(0);
            0
        }
    };
    core::ptr::addr_of_mut!(PROMPT_CHECKED).write(true);
    kept
}

/// No CFG plumbing on this tier, so nothing can ever be staged — the length passes through.
#[cfg(not(any(feature = "espnow", feature = "hostsim")))]
unsafe fn checked_staged_prompt(_tok: &Tokenizer<'_>, over: usize) -> usize {
    over
}

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
    // #303 the operator's prompt wins when set (it was validated at accept time); otherwise this
    // node's built-in persona. Read per story, not cached, so a CFG change takes effect on the
    // very next story with no reboot.
    // A prompt staged before the model was up gets its vocabulary check HERE — the one place the
    // tokenizer is guaranteed. Refused ⇒ drop it and fall back to the persona, saying why.
    let over = checked_staged_prompt(tok, core::ptr::addr_of!(PROMPT_LEN).read());
    let n = if over > 0 {
        let src = &*core::ptr::addr_of!(PROMPT);
        buf[..over].copy_from_slice(&src[..over]);
        over
    } else {
        persona::prompt(node_id, &mut buf)
    };
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

/// Bytes of story text a continuation keeps (#302). The panel renders only the last four or five
/// wrapped lines (~70 chars), so this is several screens of scrollback nobody can scroll back to;
/// it is deliberately generous so the head-drop is invisible rather than tight to save `.bss`
/// (the buffer is 1 KB either way — this is a bound on the CONTENT, not an allocation).
const TEXT_KEEP: usize = 256;

/// Drop the oldest story text to make room for another chapter, returning how many bytes went.
/// The arithmetic (and its two hazards) lives in [`textflow::roll`], where the host tests can
/// reach it; this is only the `static mut` plumbing.
///
/// The panel shows the TAIL ([`textflow::wrap_tail`]), so losing the head is invisible — but the
/// caller's reveal cursor is a byte offset into this buffer and MUST be moved back by the return
/// value.
///
/// # Safety
/// As [`init_statics`].
unsafe fn compact_text(keep: usize, revealed: usize) -> usize {
    let text = &mut *core::ptr::addr_of_mut!(STORY_TEXT);
    let len = *core::ptr::addr_of!(STORY_LEN);
    let cut = textflow::roll(text, len, keep, revealed);
    core::ptr::addr_of_mut!(STORY_LEN).write(len - cut);
    cut
}

/// What a cut chapter appends so the text reads as trailing off rather than as a crash. ASCII
/// dots, not U+2026: FONT_5X8 is an ASCII font and would draw the ellipsis as a blank, silently
/// losing the very signal we are adding.
const CUT_MARK: &[u8] = b"...";

/// Take back the [`CUT_MARK`] a paused chapter left: it says "the story stops here", and it is
/// about not to. Without this a continued story reads `went insid... e and went`.
///
/// # Safety
/// As [`init_statics`].
unsafe fn unmark_truncation() {
    let text = &*core::ptr::addr_of!(STORY_TEXT);
    let len = *core::ptr::addr_of!(STORY_LEN);
    let n = CUT_MARK.len();
    if len >= n && &text[len - n..len] == CUT_MARK {
        core::ptr::addr_of_mut!(STORY_LEN).write(len - n);
    }
}

/// Continue the paused story instead of starting a new one (#302): trim the ellipsis, make room,
/// and let the generator pick up mid-sentence. Returns the bytes the reveal cursor must move back,
/// or `None` when the story is genuinely over (the model chose to stop) and the caller should
/// begin a new one.
///
/// # Safety
/// As [`init_statics`].
unsafe fn continue_story(revealed: usize) -> Option<usize> {
    let story = (*core::ptr::addr_of_mut!(STORY)).as_mut()?;
    if !story.resume() {
        return None;
    }
    unmark_truncation();
    Some(compact_text(TEXT_KEEP, revealed))
}

/// Whether a press on the finished screen should CONTINUE this story (see `Story::can_continue`).
/// Read from the generator rather than mirrored into [`BardApp`], so the marker on the panel and
/// the behaviour of the button can never disagree.
///
/// # Safety
/// As [`init_statics`].
unsafe fn story_can_continue() -> bool {
    (*core::ptr::addr_of!(STORY))
        .as_ref()
        .is_some_and(Story::can_continue)
}

/// Absolute length of the story so far, in tokens (prompt included) — for the per-chapter log.
///
/// # Safety
/// As [`init_statics`].
unsafe fn story_pos() -> u16 {
    (*core::ptr::addr_of!(STORY))
        .as_ref()
        .map_or(0, Story::pos)
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

/// What one [`step_story`] call did — enough to tell a prompt-priming pass (same cost, but not
/// a story token) from a generated one, so the perf stats only count what they claim to.
enum Advance {
    /// A prompt token was fed; nothing generated yet.
    Primed,
    /// A generated token's bytes were appended.
    Wrote,
    /// The story ended.
    Ended,
}

/// Advance the story by ONE token, appending to [`STORY_TEXT`].
///
/// # Safety
/// As [`init_statics`].
unsafe fn step_story() -> Advance {
    let (Some(model), Some(tok)) = (
        (*core::ptr::addr_of!(MODEL)).as_ref(),
        (*core::ptr::addr_of!(TOKENIZER)).as_ref(),
    ) else {
        return Advance::Ended;
    };
    let Some(story) = (*core::ptr::addr_of_mut!(STORY)).as_mut() else {
        return Advance::Ended;
    };
    let bufs = &mut *core::ptr::addr_of_mut!(BUFS);
    let advance = match story.step(model, tok, bufs) {
        StepOut::Working => Advance::Primed,
        StepOut::Text(bytes) => {
            if push_text(bytes) {
                Advance::Wrote
            } else {
                // The text buffer is full — the last token landed, but this is the end.
                Advance::Ended
            }
        }
        // A cut is the usual ending at this model size (T8: ~19 of 20 seeds), so mark it —
        // trailing dots read as tailing off, where a bare mid-sentence stop reads as a crash.
        // Since #302 a press takes the mark back off again (`unmark_truncation`) and keeps going.
        StepOut::Done { truncated } => {
            if truncated {
                push_text(CUT_MARK);
            }
            Advance::Ended
        }
    };
    // Belt and braces: the state machine's own view must agree that there is more to come.
    if story.is_done() {
        Advance::Ended
    } else {
        advance
    }
}

/// Milliseconds since boot, read LIVE.
///
/// `ctx.now_ms` cannot be used to time a forward pass: it is a snapshot `main` takes once per
/// tick, so before/after readings inside one `update()` are identical and every measurement
/// would be 0. `main::millis()` is private and `net::mode::now_ms` is radio-gated, so this
/// takes the same underlying clock directly — the third copy of a one-line call the crate
/// already keeps two of, by the same reasoning its comment gives.
#[inline]
fn now_ms_live() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_millis()
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
///
/// `Told` covers both endings a story can have (#302). Which one it is lives in the generator, not
/// here — see [`story_can_continue`].
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
    /// Generated tokens this story (prompt-priming passes excluded — see [`Advance`]).
    tok_count: u16,
    /// Total ms spent in those passes. u32 holds ~49 days; a story is seconds.
    tok_ms_sum: u32,
    /// Slowest single pass, ms — the number that decides whether the UI can stay responsive.
    tok_ms_max: u16,
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
            tok_count: 0,
            tok_ms_sum: 0,
            tok_ms_max: 0,
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
        self.reset_stats();
    }

    /// Keep telling the SAME story (#302): one press buys another chapter, generated from the KV
    /// state the last one left behind. `false` ⇒ the model ended this story, so the caller should
    /// start a new one instead.
    ///
    /// The reveal is NOT reset — it is re-anchored. `compact_text` may have dropped bytes off the
    /// head of the buffer, and `shown` is a byte offset into it, so it moves back by exactly as
    /// many; the reader sees the typewriter carry on mid-sentence, which is the whole point.
    /// `fast` survives too: a reader who asked for no throttle asked about the story, not the
    /// chapter.
    fn keep_going(&mut self, now_ms: u64) -> bool {
        let continued = unsafe { continue_story(self.shown as usize) };
        let Some(dropped) = continued else {
            return false;
        };
        let len = unsafe { *core::ptr::addr_of!(STORY_LEN) } as u16;
        self.shown = self.shown.saturating_sub(dropped as u16).min(len);
        self.phase = Phase::Composing;
        self.next_reveal_ms = now_ms;
        self.painted = u16::MAX; // the `~ more ~` marker has to come off the panel
        // Per-CHAPTER timing, so the numbers stay comparable to the T13 bench (which measured
        // ~65-token runs) instead of averaging a story that may now run for hours.
        self.reset_stats();
        true
    }

    /// Zero the per-chapter perf counters.
    fn reset_stats(&mut self) {
        self.tok_count = 0;
        self.tok_ms_sum = 0;
        self.tok_ms_max = 0;
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
                    // Paused (#302): keep this story going. A press only starts a NEW story when
                    // the model itself ended this one (`~ fin ~`); the way to ask for a different
                    // story is the gesture that already exists — long-press out to the menu and
                    // pick the Bard again, which builds a fresh `BardApp`. That keeps the grammar
                    // to two gestures and makes the common case (more of this story) the cheap one.
                    Phase::Told => {
                        if !self.keep_going(ctx.now_ms) {
                            self.restart(ctx.node_id, ctx.now_ms);
                        }
                    }
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
        if matches!(self.phase, Phase::Composing) {
            let t0 = now_ms_live();
            let advance = unsafe { step_story() };
            // Millisecond resolution is plenty at the expected 20-100 ms per pass.
            let dt = now_ms_live().saturating_sub(t0).min(u16::MAX as u64) as u16;
            if matches!(advance, Advance::Wrote) {
                self.tok_count = self.tok_count.saturating_add(1);
                self.tok_ms_sum = self.tok_ms_sum.saturating_add(dt as u32);
                self.tok_ms_max = self.tok_ms_max.max(dt);
            }
            if matches!(advance, Advance::Ended) {
                self.phase = Phase::Told;
                // ONE line per story, on the transition only. Serial-only by nature: ESP_LOG is
                // baked at compile time, so a release image is silent here and an ESP_LOG=info
                // build is what surfaces it (Task 13's bench run).
                let avg = if self.tok_count == 0 {
                    0
                } else {
                    self.tok_ms_sum / self.tok_count as u32
                };
                // #302: one line per CHAPTER now, with the story's running length and whether a
                // press will continue it — the two things a bench run needs to interpret the rest.
                log::info!(
                    "smol #300: bard chapter done — {} tok, avg {} ms/tok, max {} ms; story {} tok, {}",
                    self.tok_count,
                    avg,
                    self.tok_ms_max,
                    unsafe { story_pos() },
                    if unsafe { story_can_continue() } {
                        "press to continue"
                    } else {
                        "the end"
                    }
                );
                // Bench builds only: how much of the (floor-gated) stack the story actually used.
                #[cfg(feature = "stack-paint")]
                {
                    let region = stack_paint::region_bytes();
                    let used = stack_paint::high_water();
                    // checked_div, not a zero test: a region of 0 means the linker symbols were
                    // nonsense, and reporting 0% is the honest answer rather than dividing.
                    let pct = (used * 100).checked_div(region).unwrap_or(0);
                    log::info!(
                        "smol #300: stack high-water {} of {} B ({}%)",
                        used,
                        region,
                        pct
                    );
                }
            }
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
        // Ask the generator, not the phase: the marker must say what the button will actually do.
        draw_story(ctx, self.shown, told, quill, told && unsafe { story_can_continue() });
    }
}

/// Draw the revealed text, word-wrapped, with the quill while composing and an ending marker once
/// the story is fully told: `~ more ~` when a press will continue it, `~ fin ~` when the model
/// ended it and a press starts a new one (#302). The marker IS the affordance — the panel has no
/// room for instructions, so the two endings have to look different.
fn draw_story(ctx: &mut Ctx, shown: u16, told: bool, quill: bool, more: bool) {
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
        // ASCII only — FONT_5X8 has no glyph beyond it, and a missing glyph draws as a blank.
        let mark = if more { "~ more ~" } else { "~ fin ~" };
        let x = ((COLS as i32 * GLYPH_W) - mark.len() as i32 * GLYPH_W) / 2;
        Text::with_baseline(
            mark,
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
