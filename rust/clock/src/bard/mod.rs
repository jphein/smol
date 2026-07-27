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

pub mod delivery;
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
use delivery::Delivery;
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

/// #302 how the narration is delivered (CFG key `V`): reveal pace + `inf`/`page` mode. 4 B of
/// `.bss`. Read live by the screen every tick, so a change lands with no reboot and without
/// interrupting the tale in progress.
static mut DELIVERY: Delivery = Delivery::DEFAULT;

/// The live delivery setting.
///
/// # Safety
/// As [`init_statics`].
unsafe fn delivery() -> Delivery {
    core::ptr::addr_of!(DELIVERY).read()
}

/// The last `V` value this node acted on — see [`delivery::LastOffer`] for why the dedupe is on
/// bytes and what the two quiet hazards are. 19 B of `.bss`.
#[cfg(any(feature = "espnow", feature = "hostsim"))]
static mut LAST_V: delivery::LastOffer = delivery::LastOffer::NONE;

/// Offer a delivery setting (CFG `V`), e.g. `160:inf` or `80:page`; empty restores the defaults.
/// A malformed value is REFUSED and the previous setting kept — the same discipline as `T`, for the
/// same reason: this is a value from the air, and the failure mode of accepting half of it is a
/// board that reveals at 0 ms/char or stops narrating.
///
/// `Ok(None)` means this exact value was already handled (a retained re-arm): nothing changed and
/// the caller should say nothing. `Ok(Some(a))` is a fresh apply, worth a line — including whether
/// the speed had to be clamped. `Err` is a fresh refusal, worth a warning.
///
/// # Safety
/// As [`init_statics`] — single-threaded; called only from `main`'s config-apply path.
#[cfg(any(feature = "espnow", feature = "hostsim"))]
pub unsafe fn set_delivery(
    value: &[u8],
) -> Result<Option<delivery::Accepted>, delivery::DeliveryErr> {
    // Records the offer as it checks, so a refused value is deduplicated too (a retained bad value
    // warns once; a DIFFERENT bad value warns again).
    if !(*core::ptr::addr_of_mut!(LAST_V)).is_new(value) {
        return Ok(None);
    }
    let accepted = Delivery::parse(value, delivery())?;
    core::ptr::addr_of_mut!(DELIVERY).write(accepted.delivery);
    Ok(Some(accepted))
}

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

/// Open a tale for this node's protagonist, seeded from the clock so successive tales differ.
///
/// `fresh` clears the panel (entering the screen); otherwise the tale is appended to the text
/// already scrolling — a paragraph break, then its opening line — which is how the endless
/// narrator crosses a tale boundary (#302). Returns bytes rolled off the HEAD of the buffer, which
/// the caller must subtract from its reveal cursor.
///
/// # Safety
/// As [`init_statics`] — single-threaded, called only from the Bard screen's own handlers.
unsafe fn begin_story(node_id: u8, now_ms: u64, fresh: bool, revealed: usize) -> usize {
    let Some(tok) = (*core::ptr::addr_of!(TOKENIZER)).as_ref() else {
        return 0;
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
    if fresh {
        core::ptr::addr_of_mut!(STORY_LEN).write(0);
    }
    // Seed the visible text with the opening itself, so the screen reads as a story from the first
    // frame rather than sitting blank through ~15 priming passes (~3 s) — and, mid-scroll, so a new
    // tale announces itself instead of appearing to resume the old one mid-sentence. A newline is
    // the break: `textflow::wrap_tail` treats it as a hard break, which costs no glyph row.
    let mut dropped = 0usize;
    if !fresh {
        dropped += push_text(b"\n", revealed);
    }
    dropped + push_text(&buf[..n], revealed.saturating_sub(dropped))
}

/// Bytes of story text kept when the buffer has to roll (#302). The panel renders only the last
/// four or five wrapped lines (~70 chars), so everything above this is scrollback nobody can scroll
/// back to; it is generous rather than tight because the buffer is 1 KB either way — this bounds
/// the CONTENT, not an allocation — and a bigger keep means a rarer memmove.
const TEXT_KEEP: usize = 512;

/// Append `extra` to the visible story text, ROLLING the oldest text out when the buffer is full
/// (#302). Returns how many bytes were dropped from the head — the caller's reveal cursor is a byte
/// offset into this buffer and MUST move back by that much.
///
/// An endless narrator overruns any fixed buffer by definition, so "truncate and stop" (the
/// pre-#302 behaviour) would have ended the story after 1 KB. The policy and its two hazards —
/// never dropping unrevealed text, never splitting a UTF-8 character — live in
/// [`textflow::append_rolling`] where the host tests drive the real code.
///
/// # Safety
/// As [`init_statics`].
unsafe fn push_text(extra: &[u8], revealed: usize) -> usize {
    let text = &mut *core::ptr::addr_of_mut!(STORY_TEXT);
    let len = *core::ptr::addr_of!(STORY_LEN);
    let (len, dropped) = textflow::append_rolling(text, len, extra, TEXT_KEEP, revealed);
    core::ptr::addr_of_mut!(STORY_LEN).write(len);
    dropped
}

/// Length of the visible story text.
///
/// # Safety
/// As [`init_statics`].
unsafe fn text_len() -> usize {
    *core::ptr::addr_of!(STORY_LEN)
}

/// What one [`step_story`] call did — enough to tell a prompt-priming pass (same cost, but not
/// a story token) from a generated one, so the perf stats only count what they claim to, and to
/// carry the rolling buffer's head-drop back to the caller's reveal cursor.
enum Advance {
    /// A prompt token was fed; nothing generated yet.
    Primed,
    /// A generated token's bytes were appended; `dropped` bytes rolled off the head.
    Wrote { dropped: usize },
    /// This TALE ended. NOT the end of the narration — the screen opens the next tale (#302).
    /// `cursor` distinguishes the two causes for the log: the position cursor recycling (essentially
    /// never) versus the model choosing to finish (the normal case).
    Ended { cursor: bool },
}

/// Advance the tale by ONE token, appending to [`STORY_TEXT`].
///
/// # Safety
/// As [`init_statics`].
unsafe fn step_story(revealed: usize) -> Advance {
    let (Some(model), Some(tok)) = (
        (*core::ptr::addr_of!(MODEL)).as_ref(),
        (*core::ptr::addr_of!(TOKENIZER)).as_ref(),
    ) else {
        return Advance::Ended { cursor: false };
    };
    let Some(story) = (*core::ptr::addr_of_mut!(STORY)).as_mut() else {
        return Advance::Ended { cursor: false };
    };
    let bufs = &mut *core::ptr::addr_of_mut!(BUFS);
    match story.step(model, tok, bufs) {
        StepOut::Working => Advance::Primed,
        // The buffer ROLLS now instead of filling up, so a long tale is not an ending — the only
        // endings left are the model's own and the cursor's, both of which arrive as `Done`.
        StepOut::Text(bytes) => Advance::Wrote {
            dropped: push_text(bytes, revealed),
        },
        StepOut::Done { truncated } => Advance::Ended { cursor: truncated },
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

/// New characters one `page` delivers before waiting for a press: the story rows of a full panel
/// (the bottom row is the `~ more ~` marker). Counted in BYTES of generated text, so word-wrap
/// makes it "about a screenful" rather than exactly one — the honest bound for a variable-width
/// wrap, and a page that came up one word short would read as a bug where a slightly long one
/// just scrolls.
const PAGE_BYTES: u16 = (COLS * (ROWS - 1)) as u16;

/// How far generation may run AHEAD of the reveal in `inf` mode before it waits (#302).
///
/// This is the load-bearing number of infinite mode. Generation makes ~13 chars/s on the C3
/// (202 ms/token, ~2.7 chars/token) while the default reveal consumes 6.25 chars/s, so an
/// unthrottled narrator outruns its reader forever: the buffer fills with text nobody has seen and
/// every roll then has to choose between dropping unread words and stalling. Bounding the unrevealed
/// backlog turns the pair into a producer/consumer with a small queue — the reveal speed becomes
/// the generation rate, and the board's compute duty cycle follows the setting instead of pinning
/// the CPU. ~1.5 screenfuls: deep enough that the typewriter never starves waiting for a token
/// (one token feeds ~430 ms of reading), shallow enough that a speed change is felt at once.
const BACKLOG_MAX: usize = 96;

/// Perf-report cadence, whichever comes first (#302). An endless narrator has no natural moment to
/// report at: the old "one line per story" fired on a transition that no longer exists, and a tale
/// boundary is now minutes apart at best (backpressure paces generation to ~2.3 tok/s, so a
/// ~300-token tale takes >2 min) or never — a tale that does not sample end-of-text runs to the
/// position cursor, hours away. So the metric is periodic instead, and each line is self-contained
/// (pace and mode included) because the config can change mid-soak.
///
/// 64 tokens is directly comparable to T13's 67-token story measurement, and the 60 s ceiling means
/// a paused `page` reader still produces a line rather than silence. Serial-only either way: ESP_LOG
/// is baked at compile time, so a release fleet image prints none of this.
const REPORT_TOKENS: u16 = 64;
/// Longest a report may be withheld, even mid-page.
const REPORT_MS: u64 = 60_000;

/// Marker shown when a `page` is fully revealed and a press will turn it. ASCII only — FONT_5X8
/// has no glyph beyond it, and a missing glyph draws as a blank.
const MORE_MARK: &str = "~ more ~";

/// Where the screen is in the narration.
///
/// No `Idle` and no `Told` (#302): [`BardApp::new`] either starts narrating or fails the blob, and
/// the narration has no end — a tale finishing is answered by opening the next one, so the only
/// "not generating" state is a `page` mode pause, which a press ends.
enum Phase {
    /// Generating and typing out.
    Composing,
    /// `page` mode: this page's text is written; waiting for a press to turn it.
    Paged,
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
    /// Set by a tap: stop throttling and catch the reveal up (spec §9). Expires at the next tale
    /// or page turn, so one impatient tap does not disable the typewriter for ever.
    fast: bool,
    /// New text bytes written into the current `page` (`page` mode only).
    page_bytes: u16,
    /// Generated tokens this tale (prompt-priming passes excluded — see [`Advance`]).
    tok_count: u16,
    /// Total ms spent in those passes. u32 holds ~49 days; a tale is seconds to minutes.
    tok_ms_sum: u32,
    /// Slowest single pass, ms — the number that decides whether the UI can stay responsive.
    tok_ms_max: u16,
    /// When the next perf report is due (see [`REPORT_MS`]).
    next_report_ms: u64,
}

impl BardApp {
    /// Parse the blob and open the first tale. Called once per entry to the screen.
    pub fn new(ctx: &Ctx) -> Self {
        let ok = unsafe { init_statics() };
        let app = BardApp {
            phase: if ok { Phase::Composing } else { Phase::Mute },
            shown: 0,
            next_reveal_ms: ctx.now_ms,
            quill_on: false,
            painted: u16::MAX, // force the first paint
            fast: false,
            page_bytes: 0,
            tok_count: 0,
            tok_ms_sum: 0,
            tok_ms_max: 0,
            next_report_ms: ctx.now_ms.saturating_add(REPORT_MS),
        };
        if ok {
            unsafe { begin_story(ctx.node_id, ctx.now_ms, true, 0) };
        }
        app
    }

    /// Open the next tale in the same endless scroll (#302): the old text keeps scrolling up, a
    /// paragraph break and the new opening are appended, and the reveal carries straight on. Called
    /// when the MODEL ends a tale — the reader sees the bard finish and start another, not a screen
    /// that reset.
    fn next_tale(&mut self, node_id: u8, now_ms: u64) {
        let before = unsafe { text_len() };
        let dropped = unsafe { begin_story(node_id, now_ms, false, self.shown as usize) };
        // The break and the new opening are new text on the panel, so they count against the page
        // quota like any other — otherwise a page that happens to contain a tale boundary would run
        // ~40 characters long.
        let written = unsafe { text_len() } + dropped - before;
        self.page_bytes = self.page_bytes.saturating_add(written as u16);
        self.rewind(dropped);
        self.fast = false;
    }

    /// Turn the page in `page` mode: another [`PAGE_BYTES`] of new text, at reading pace again.
    fn turn_page(&mut self, now_ms: u64) {
        self.phase = Phase::Composing;
        self.page_bytes = 0;
        self.fast = false;
        self.next_reveal_ms = now_ms;
        self.painted = u16::MAX; // the marker has to come off the panel
    }

    /// Re-anchor the reveal cursor after the rolling buffer dropped `dropped` bytes off the head:
    /// `shown` is a byte offset into a buffer that just moved. Clamped to the new length so a
    /// tail trim can never leave the cursor past the end.
    fn rewind(&mut self, dropped: usize) {
        if dropped == 0 {
            return;
        }
        let len = unsafe { text_len() } as u16;
        self.shown = self.shown.saturating_sub(dropped as u16).min(len);
        self.painted = u16::MAX;
    }

    /// Emit the accumulated generation metrics and start a fresh window. `why` names the occasion
    /// (periodic, or a tale boundary) so one grep covers both.
    ///
    /// This is the ONLY place the numbers are printed, so they can never be double-counted or —
    /// as they were until the bench caught it — reported at a transition an endless narrator never
    /// makes. Silent when nothing was generated: a paused `page` reader should produce no line
    /// rather than a row of zeroes.
    fn report_perf(&mut self, now_ms: u64, why: &str) {
        self.next_report_ms = now_ms.saturating_add(REPORT_MS);
        if self.tok_count == 0 {
            return;
        }
        let d = unsafe { delivery() };
        log::info!(
            "smol #300: bard {} — {} tok, avg {} ms/tok, max {} ms @ {} ms/char {}",
            why,
            self.tok_count,
            self.tok_ms_sum / self.tok_count as u32,
            self.tok_ms_max,
            d.ms_per_char,
            match d.mode {
                delivery::Mode::Inf => "inf",
                delivery::Mode::Page => "page",
            }
        );
        // Bench builds only: how much of the (floor-gated) stack the narration actually used. In
        // the periodic line rather than at a tale end, because the question it answers — does the
        // ring leak stack as it wraps? — is about elapsed narration, not about tales.
        #[cfg(feature = "stack-paint")]
        {
            let region = stack_paint::region_bytes();
            let used = stack_paint::high_water();
            // checked_div, not a zero test: a region of 0 means the linker symbols were nonsense,
            // and reporting 0% is the honest answer rather than dividing.
            let pct = (used * 100).checked_div(region).unwrap_or(0);
            log::info!("smol #300: stack high-water {} of {} B ({}%)", used, region, pct);
        }
        self.reset_stats();
    }

    /// Zero the perf counters.
    fn reset_stats(&mut self) {
        self.tok_count = 0;
        self.tok_ms_sum = 0;
        self.tok_ms_max = 0;
    }

    /// Whether to spend this tick on a forward pass.
    ///
    /// The narrator is endless, so something has to decide when NOT to generate; this is that
    /// something. See [`BACKLOG_MAX`] — generation waits for the reader rather than racing ahead,
    /// which is what makes the speed setting a pacing control rather than a cosmetic one.
    fn wants_token(&self, len: usize) -> bool {
        matches!(self.phase, Phase::Composing) && len.saturating_sub(self.shown as usize) < BACKLOG_MAX
    }
}

impl Plugin for BardApp {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar across screens: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                match self.phase {
                    // Composing: skip the typewriter. There is nothing to "restart" any more —
                    // the narration never ended — so the impatient tap is the only meaning left.
                    Phase::Composing => self.fast = true,
                    // Page turn, with the courtesy of finishing THIS page first: a press while the
                    // typewriter is still catching up means "hurry", a press once it has caught up
                    // means "next page". Same button, and neither reading is ever surprising.
                    Phase::Paged => {
                        if (self.shown as usize) < unsafe { text_len() } {
                            self.fast = true;
                        } else {
                            self.turn_page(ctx.now_ms);
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
        // Read the delivery setting LIVE, so a CFG `V` change lands on the next tick with no
        // reboot and no story restart (#302).
        let d = unsafe { delivery() };
        // Switching to `inf` un-pauses immediately; switching to `page` lets the current page
        // finish, because `page_bytes` is already counted and cutting mid-page would look like a
        // dropped word.
        if matches!(self.phase, Phase::Paged) && d.mode == delivery::Mode::Inf {
            self.turn_page(ctx.now_ms);
        }

        // Generate: at most ONE forward pass per tick, and only when the reader is not already
        // behind (see `wants_token`). The REVEAL is what is paced; generation is what is gated.
        let len = unsafe { text_len() };
        if self.wants_token(len) {
            let t0 = now_ms_live();
            let advance = unsafe { step_story(self.shown as usize) };
            // Millisecond resolution is plenty at the expected 200 ms per pass.
            let dt = now_ms_live().saturating_sub(t0).min(u16::MAX as u64) as u16;
            match advance {
                Advance::Wrote { dropped } => {
                    self.tok_count = self.tok_count.saturating_add(1);
                    self.tok_ms_sum = self.tok_ms_sum.saturating_add(dt as u32);
                    self.tok_ms_max = self.tok_ms_max.max(dt);
                    // Count what the tale actually appended, so a multi-byte token fills the page
                    // by the width it will occupy.
                    let written = unsafe { text_len() } + dropped - len;
                    self.page_bytes = self.page_bytes.saturating_add(written as u16);
                    self.rewind(dropped);
                    if d.mode == delivery::Mode::Page && self.page_bytes >= PAGE_BYTES {
                        self.phase = Phase::Paged;
                    }
                }
                Advance::Primed => {}
                Advance::Ended { cursor } => {
                    // A tale boundary FLUSHES the window rather than owning the metric, so the
                    // numbers are attributable to a tale when one ends and still arrive when none
                    // does. `page` mode gets its per-tale line here exactly as before.
                    self.report_perf(
                        ctx.now_ms,
                        if cursor {
                            "tale done (position cursor recycled), opening the next"
                        } else {
                            "tale done (the model ended it), opening the next"
                        },
                    );
                    self.next_tale(ctx.node_id, ctx.now_ms);
                }
            }
        }
        // Periodic report: whichever of the two bounds trips first (see REPORT_TOKENS). Outside the
        // generate block so a `page` pause still closes out its window on the time bound.
        if self.tok_count >= REPORT_TOKENS || ctx.now_ms >= self.next_report_ms {
            self.report_perf(ctx.now_ms, "narrating");
        }
        let text_len = unsafe { text_len() } as u16;

        // Reveal on a wall-clock schedule (`next_reveal_ms` accumulates rather than resetting to
        // `now`), so a slow token or a missed tick catches up instead of drifting.
        if self.fast {
            self.shown = text_len;
        } else if self.shown >= text_len {
            // Caught up: park the schedule at `now`. Without this, any wait — a page turn, the
            // backpressure gate, a slow token — banks reveal credit and then dumps a burst of
            // characters the instant text arrives, which in `page` mode would mean every page after
            // the first appears all at once instead of typing itself out.
            self.next_reveal_ms = ctx.now_ms;
        } else {
            while self.shown < text_len && ctx.now_ms >= self.next_reveal_ms {
                self.shown += 1;
                self.next_reveal_ms = self.next_reveal_ms.saturating_add(d.reveal_ms());
            }
        }

        // Repaint only when something a viewer can see changed.
        let paused = matches!(self.phase, Phase::Paged) && self.shown >= text_len;
        let quill = !paused && (ctx.now_ms / BLINK_MS).is_multiple_of(2);
        if !(ctx.redraw || self.shown != self.painted || quill != self.quill_on) {
            return;
        }
        self.painted = self.shown;
        self.quill_on = quill;
        draw_story(ctx, self.shown, quill, paused.then_some(MORE_MARK));
    }
}

/// Draw the revealed text, word-wrapped, with the blinking quill while the bard is writing and
/// `marker` on the bottom row when it is waiting (`page` mode). The marker IS the affordance — the
/// panel has no room for instructions — so it appears only when a press will actually do something
/// (#302: there is no story-over marker any more, because a story is never over).
fn draw_story(ctx: &mut Ctx, shown: u16, quill: bool, marker: Option<&str>) {
    let text = unsafe { &*core::ptr::addr_of!(STORY_TEXT) };
    let visible = &text[..(shown as usize).min(text.len())];
    // Reserve the bottom row for the marker, so it never overwrites a line of story.
    let rows = if marker.is_some() { ROWS - 1 } else { ROWS };
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
    if let Some(mark) = marker {
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
