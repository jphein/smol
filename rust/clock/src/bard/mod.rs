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

// #335 P1.0 (edition 2024): `unsafe_op_in_unsafe_fn` goes warn-by-default in 2024 — an `unsafe fn`
// body is no longer implicitly an unsafe block. That is 34 sites across the 9 `unsafe fn`s below,
// and it is why `clippy -D warnings` went red on the `bard` and `stack-paint` tiers (and ONLY those
// two: this file is the only one in the tree with that shape). Note the tiers still `cargo check`
// clean — the lint is a warning, so only the `-D warnings` gate sees it.
//
// Allowed at module scope rather than wrapped, for the same reason main.rs defers its 38
// `collapsible_if` sites: an edition bump should not arrive as a 34-site reflow of a module this
// phase does not otherwise touch. Two things make that call stronger here than there — every one
// of these fns is a `static mut` singleton accessor whose SAFETY contract is already stated at the
// fn level (so per-op marking adds no information a reader lacks), and this is the #300 nano-LLM
// whose acceptance test is BIT-EXACT against an independent reference implementation. A mechanical
// 34-site edit through that is a real risk of perturbing a golden result, against no behavioural
// gain whatsoever.
//
// ⚠️ The cost, stated so it is not discovered later: new unsafe ops added inside these fns get no
// lint. Keep writing explicit `unsafe {}` blocks in new code here anyway.
// TODO(#335 follow-up): wrap the 34 sites and drop this allow — same cleanup commit as main.rs's
// `collapsible_if` collapse, which has the same shape and the same reason for waiting.
#![allow(unsafe_op_in_unsafe_fn)]

pub mod delivery;
pub mod nano_llm;
pub mod persona;
// #434: the stack instrument NO LONGER LIVES HERE — the file moved to `src/stack_paint.rs`. It
// was `pub mod stack_paint` inside this module, which is what welded `stack-paint` to `bard`, and
// post-#391 that composition stopped booting, taking the only way to measure a high-water with it.
// The FILE had to move, not just the module declaration: this repo encodes tier ownership in the
// path and `check_exclusions.py` enforces it, so a `#[path]` shim left the instrument "owned by
// bard" and failed the gate the moment a bard-less tier compiled it. This module reaches it as
// `crate::stack_paint`.
pub mod textflow;
pub mod tokenizer;

use embedded_graphics::{
    mono_font::{ascii::{FONT_5X8, FONT_6X10, FONT_9X15, FONT_10X20}, MonoFont, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};

use crate::app::{AppKind, Ctx, Oled, Plugin, Transition};
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

/// Bytes of story prompt a CFG `T` value may carry — `CFG_VALUE_MAX`, so the wire bound and the
/// buffer cannot drift apart.
const PROMPT_MAX: usize = 64;

/// #303 the operator's story opening, set at runtime over CFG key `T` (no reflash). Empty ⇒ the
/// node uses its built-in per-node persona prompt, so clearing the retained topic restores the
/// default — the same "empty = board default" convention as the `S` screen key (#21).
/// 64 B to match both the prompt buffer and `CFG_VALUE_MAX`; costs no flash (`.bss`).
///
/// The size is a `const` rather than being read back off the static: `PROMPT.len()` took a SHARED
/// REFERENCE to a mutable static, which the 2024 rules make UB and which warns today — a future
/// edition bump would turn it into an error. The house idiom is `addr_of!`/`addr_of_mut!` and never a
/// reference; a length needs neither.
static mut PROMPT: [u8; PROMPT_MAX] = [0; PROMPT_MAX];
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
    if value.len() > PROMPT_MAX {
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
/// Widest and tallest the panel can be in CHARACTERS, over every selectable font — the sizes the
/// span buffer and the page budget are cut to, since those cannot be runtime-sized without alloc.
/// `FONT_5X8` is the smallest face, so it sets both.
const MAX_COLS: usize = delivery::Font::F5x8.grid().0;
const MAX_ROWS: usize = delivery::Font::F5x8.grid().1;

/// The glyph table and the character grid it yields on this panel (#302 font slider).
///
/// A bigger font shows FEWER characters, not less story: the KV window is measured in TOKENS, so
/// generation is untouched — only how much of the scroll is visible changes. That is also why
/// `page` mode's "one screenful" shrinks with the font, which is correct (a page IS a screen) and
/// why [`page_bytes`] derives from the live geometry instead of being a constant.
fn panel_font(f: delivery::Font) -> (&'static MonoFont<'static>, usize, usize) {
    let font: &MonoFont = match f {
        delivery::Font::F5x8 => &FONT_5X8,
        delivery::Font::F6x10 => &FONT_6X10,
        delivery::Font::F9x15 => &FONT_9X15,
        delivery::Font::F10x20 => &FONT_10X20,
    };
    // The grid is computed in `delivery` (host-testable, no embedded-graphics); this is the one place
    // that can check the two agree, so a future upstream metric change fails a debug build here
    // instead of quietly wrapping text to the wrong width on the glass.
    debug_assert_eq!(
        (
            font.character_size.width as usize,
            font.character_size.height as usize
        ),
        f.glyph(),
        "delivery::Font::glyph disagrees with the embedded-graphics face"
    );
    let (cols, rows) = f.grid();
    (font, cols.min(MAX_COLS), rows.min(MAX_ROWS))
}
/// New characters one `page` delivers before waiting for a press: the story rows of a full panel at
/// the CURRENT font (the bottom row is the marker). Counted in BYTES of generated text, so word-wrap
/// makes it "about a screenful" rather than exactly one — the honest bound for a variable-width wrap,
/// and a page one word short would read as a bug where a slightly long one just scrolls.
fn page_bytes(cols: usize, rows: usize) -> u16 {
    (cols * rows.saturating_sub(1).max(1)) as u16
}

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

/// Marker shown while the narration is PAUSED, and it BLINKS — which is the whole point. A paused
/// endless story and a wedged one show identical text, so the only thing that can tell them apart on
/// a 72×40 panel is something moving: a blinking marker proves the tick loop is still running, and a
/// frozen one is a board to go and debug. (The quill is hidden while paused, because a blinking
/// quill means "the bard is writing" and it is not.)
const PAUSE_MARK: &str = "|| paused";

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
    /// `inf` mode: held by a press (JP, 2026-07-27). Generation AND the typewriter are stopped and
    /// the glass holds exactly what it showed; a press resumes mid-sentence from the same KV window.
    /// Nothing about the tale is reset — a pause is the absence of `step` calls, not a state the
    /// generator knows about, which is why resuming cannot perturb the token stream.
    Paused,
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
    /// Whether anything BLINKING was visible at the last paint — the quill while composing, the
    /// `|| paused` marker while paused (a repaint trigger). One flag because the two are mutually
    /// exclusive, and it deliberately stays `false` in the static states (`page` waiting) so those
    /// do not flush the panel 2.5 times a second forever.
    blink_on: bool,
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
            blink_on: false,
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

    /// Hold the narration where it is (`inf` mode). Generation stops because [`Self::wants_token`]
    /// only fires in `Composing`, so a paused board runs no forward pass, writes no KV slot and
    /// spends no cycles on the model — pause is the fleet's power saver as much as a reading
    /// control, which matters on a device where `inf` mode is otherwise near-continuous compute.
    fn pause(&mut self, now_ms: u64) {
        self.phase = Phase::Paused;
        // Unconditional, so the bench can always answer "did my press register?" — the perf flush
        // below is silent when no token has been generated since the last report, and silence is the
        // one thing this line must never be. (ESP_LOG is baked at compile time: a release fleet
        // image prints neither.)
        log::info!("smol #302: bard PAUSED — press to resume");
        // Close the window here so the tokens belong to the narration that just stopped, rather than
        // being averaged in with whatever comes after the pause.
        self.report_perf(now_ms, "at pause");
        // A pause ends any outstanding "hurry": the two would contradict each other, and `fast`
        // would keep dragging the reveal forward while the glass is supposed to be holding still.
        self.fast = false;
        self.painted = u16::MAX; // the marker has to appear on the next paint, not on the next token
    }

    /// Carry on from exactly where [`Self::pause`] stopped. Nothing is re-fed, re-primed or
    /// re-seeded — the `Story`, its `Session` and the KV window were never touched — so the next
    /// token is the one the pause interrupted.
    fn resume(&mut self, now_ms: u64) {
        self.phase = Phase::Composing;
        log::info!("smol #302: bard RESUMED");
        self.next_reveal_ms = now_ms;
        self.painted = u16::MAX; // clear the marker
    }

    /// Turn the page in `page` mode: another screenful of new text, at reading pace again.
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
            let region = crate::stack_paint::region_bytes();
            let used = crate::stack_paint::high_water();
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

    /// Render-only: advance the typewriter and repaint. NO generation, so this is the half of
    /// `update` that a WiFi-burst yield can afford (see [`Plugin::paint_burst`]) — one pass over
    /// ≤1 KB of text plus a panel flush, against a forward pass's measured 224-274 ms.
    fn reveal_and_paint(&mut self, display: &mut Oled, now: u64, redraw: bool, d: Delivery) {
        let text_len = unsafe { text_len() } as u16;

        // Reveal on a wall-clock schedule (`next_reveal_ms` accumulates rather than resetting to
        // `now`), so a slow token or a missed tick catches up instead of drifting.
        if matches!(self.phase, Phase::Paused) {
            // HOLD. Not just "stop advancing": park the schedule at `now` too, so a pause of any
            // length banks no reveal credit and the resume types on rather than dumping a burst.
            // Checked before `fast` because the two can meet (a `page`-mode hurry, then a switch to
            // `inf`, then a press) and holding still has to win.
            self.next_reveal_ms = now;
        } else if self.fast {
            self.shown = text_len;
        } else if self.shown >= text_len {
            // Caught up: park the schedule at `now`. Without this, any wait — a page turn, the
            // backpressure gate, a slow token — banks reveal credit and then dumps a burst of
            // characters the instant text arrives, which in `page` mode would mean every page after
            // the first appears all at once instead of typing itself out.
            self.next_reveal_ms = now;
        } else {
            while self.shown < text_len && now >= self.next_reveal_ms {
                self.shown += 1;
                self.next_reveal_ms = self.next_reveal_ms.saturating_add(d.reveal_ms());
            }
        }

        // Repaint only when something a viewer can see changed.
        let blink = (now / BLINK_MS).is_multiple_of(2);
        let held = matches!(self.phase, Phase::Paused);
        let page_done = matches!(self.phase, Phase::Paged) && self.shown >= text_len;
        // The quill means "the bard is writing", so it must not blink while paused — that is the
        // one state where a moving quill would be a lie.
        let quill = !held && !page_done && blink;
        let marker = if held {
            // Blinking, but the ROW STAYS RESERVED on both halves of the blink (an empty string
            // draws nothing and keeps `rows` the same), or the story would reflow twice a second.
            Some(if blink { PAUSE_MARK } else { "" })
        } else if page_done {
            Some(MORE_MARK)
        } else {
            None
        };
        // One flag for both blinking things (they are mutually exclusive), and `false` in the static
        // states so a `page` waiting for a press does not flush the panel forever.
        let blinking = quill || (held && blink);
        if !(redraw || self.shown != self.painted || blinking != self.blink_on) {
            return;
        }
        self.painted = self.shown;
        self.blink_on = blinking;
        draw_story(display, self.shown, quill, marker, d.font);
    }
}

impl Plugin for BardApp {
    fn on_button(&mut self, press: Press, ctx: &mut Ctx) -> Transition {
        match press {
            // Uniform grammar across screens: long press leaves to the menu.
            Press::Long => Transition::Switch(AppKind::Menu),
            Press::Short => {
                match self.phase {
                    // What a press MEANS depends on the delivery mode, and each mode gets exactly
                    // one meaning — a single button with three jobs would be a guessing game.
                    //   inf:  pause / play (JP's ask). An endless stream has nothing to restart and
                    //         nothing to skip to, so "hold it right there" is the useful gesture;
                    //         "skip the typewriter" is what it replaces, deliberately.
                    //   page: hurry, then turn (below) — the right pair for pages, unchanged.
                    Phase::Composing => match unsafe { delivery() }.mode {
                        delivery::Mode::Inf => self.pause(ctx.now_ms),
                        delivery::Mode::Page => self.fast = true,
                    },
                    // Paused: play. Deliberately mode-independent — if the mode changed to `page`
                    // while the board was held, a press still means "carry on" rather than silently
                    // meaning something new.
                    Phase::Paused => self.resume(ctx.now_ms),
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

    /// Keep the typewriter alive through a WiFi burst (JP: "I'm still getting UI freezes for wifi").
    /// A routine burst paints nothing by design (#153), which for THIS screen means a story that
    /// looks crashed — the same paused-vs-wedged confusion the `|| paused` blink exists to prevent,
    /// arriving from the other direction. Reveal + repaint only: generation is the expensive half and
    /// stays out of the radio's hot path entirely, so a burst slows the story down (it does not
    /// advance while the radio has the CPU) without ever freezing it.
    fn paint_burst(&mut self, display: &mut Oled, now_ms: u64) {
        if matches!(self.phase, Phase::Mute) {
            return;
        }
        // `redraw: false` — a burst repaint is a cadence repaint, never a forced one.
        self.reveal_and_paint(display, now_ms, false, unsafe { delivery() });
    }

    fn update(&mut self, ctx: &mut Ctx) {
        if matches!(self.phase, Phase::Mute) {
            if ctx.redraw {
                draw_lines(ctx.display, &["the bard", "is mute"]);
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
                    let (_, cols, rows) = panel_font(d.font);
                    if d.mode == delivery::Mode::Page && self.page_bytes >= page_bytes(cols, rows) {
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
        self.reveal_and_paint(ctx.display, ctx.now_ms, ctx.redraw, unsafe { delivery() });
    }
}

/// Draw the revealed text, word-wrapped, with the blinking quill while the bard is writing and
/// `marker` on the bottom row when it is waiting (`page` mode). The marker IS the affordance — the
/// panel has no room for instructions — so it appears only when a press will actually do something
/// (#302: there is no story-over marker any more, because a story is never over).
fn draw_story(
    display: &mut Oled,
    shown: u16,
    quill: bool,
    marker: Option<&str>,
    font: delivery::Font,
) {
    let (face, cols, panel_rows) = panel_font(font);
    let (glyph_w, row_h) = (
        face.character_size.width as i32,
        face.character_size.height as i32,
    );
    let text = unsafe { &*core::ptr::addr_of!(STORY_TEXT) };
    let visible = &text[..(shown as usize).min(text.len())];
    // Reserve the bottom row for the marker, so it never overwrites a line of story. At the two
    // biggest faces the panel is only 2 rows tall, so a marker would cost HALF the glass — there the
    // story keeps every row and the marker is dropped; the blinking quill still says "writing", and
    // `|| paused` gives up its row rather than the story giving up half of itself.
    let marker = if panel_rows > 2 { marker } else { None };
    let rows = if marker.is_some() {
        panel_rows - 1
    } else {
        panel_rows
    };
    let mut spans = [(0u16, 0u16); MAX_ROWS];
    let n = textflow::wrap_tail(visible, cols, rows, &mut spans[..rows]);

    display.clear(BinaryColor::Off).ok();
    let style = MonoTextStyleBuilder::new()
        .font(face)
        .text_color(BinaryColor::On)
        .build();
    let mut last_end = 0i32;
    for (i, &(a, b)) in spans[..n].iter().enumerate() {
        // A line that is not valid UTF-8 (a raw byte-fallback token) is skipped rather than
        // panicked on; the rest of the story still reads.
        let line = core::str::from_utf8(&visible[a as usize..b as usize]).unwrap_or("");
        Text::with_baseline(line, Point::new(0, i as i32 * row_h), style, Baseline::Top)
            .draw(display)
            .ok();
        last_end = line.chars().count() as i32;
    }
    if quill && n > 0 {
        // The nib sits after the last revealed character, clamped inside the panel.
        let x = (last_end * glyph_w).min(cols as i32 * glyph_w - glyph_w);
        Text::with_baseline("|", Point::new(x, (n as i32 - 1) * row_h), style, Baseline::Top)
            .draw(display)
            .ok();
    }
    if let Some(mark) = marker {
        let x = ((cols as i32 * glyph_w) - mark.len() as i32 * glyph_w) / 2;
        Text::with_baseline(
            mark,
            Point::new(x.max(0), (panel_rows as i32 - 1) * row_h),
            style,
            Baseline::Top,
        )
        .draw(display)
        .ok();
    }
    display.flush().ok();
}

/// Clear and draw up to a panelful of left-aligned lines in the SMALLEST face. Panic-free, and
/// deliberately font-independent: this renders the "bard is mute" notice, which must fit whatever
/// the operator set the story font to.
fn draw_lines(display: &mut Oled, lines: &[&str]) {
    display.clear(BinaryColor::Off).ok();
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    for (i, line) in lines.iter().take(MAX_ROWS).enumerate() {
        Text::with_baseline(line, Point::new(0, i as i32 * 8), style, Baseline::Top)
            .draw(display)
            .ok();
    }
    display.flush().ok();
}
