//! #274 follow-up — the fleet's display-text truncators, in one place and **named for their
//! unit**.
//!
//! # Why two functions and not one
//!
//! #274 left the crate with two correct-but-different truncators, both called `clip`:
//!
//! | where | unit | callers |
//! |---|---|---|
//! | `rssi.rs` | **bytes** | `bench.rs`, `ota_screen.rs` |
//! | `batt.rs`, `grid.rs` | **characters** | their own line layout |
//!
//! Both are boundary-safe, so neither can panic; the hazard is subtler than that. Two functions
//! with the same name and different units, in one crate, is a trap for whoever next moves a call
//! site between them — the code compiles, the ASCII output is identical, and the difference only
//! appears the day a non-ASCII string reaches a screen.
//!
//! Unifying them would have been the wrong fix. The units are not an accident:
//!
//! * `ota_screen.rs` budgets against a fixed pixel width and a `Line` buffer measured in bytes;
//! * `batt.rs`/`grid.rs` budget in `LINE_CHARS`, a glyph count.
//!
//! Neither is truly "display width" — a wide or combining character is neither one byte nor one
//! cell — and pretending otherwise would need font metrics, not string surgery. So both units
//! survive, each keeps its exact current behaviour, and the names now say which is which.
//!
//! **No semantic change.** Every call site keeps the function it already had; only the name and
//! the home move. `experiments/clip_verify` asserts byte-identical output against the previous
//! implementations across all-ASCII input, which is what makes that claim checkable rather than
//! asserted.
//!
//! # Placement
//!
//! `wifi`-gated, because the consumers span two tiers: `batt`/`grid` are `wifi`, while
//! `bench`/`rssi`/`ota_screen` are `espnow` (⊃ `wifi`). The previous shared home was `rssi.rs`,
//! which is **espnow**-only — so `batt`/`grid` structurally could not have used it, which is
//! precisely why the duplication survived #274.

/// Truncate to at most `n` **bytes**, never splitting a UTF-8 character.
///
/// Walks backwards from `n`, so a character straddling the budget is dropped rather than
/// half-emitted and the result is always within the caller's byte budget. Was `rssi::clip`.
///
/// Use this where the budget comes from a buffer size or a byte-measured layout.
///
/// Both consumers (`bench`, `ota_screen`) are `espnow`, but this module is `wifi`-gated because
/// `clip_chars`'s consumers are — so on a wifi-without-espnow build this is genuinely unused.
/// Narrow `cfg_attr` rather than a blanket `allow`, matching `net.rs::assert_max_tx_power`: a
/// blanket allow would also hide it going unused on the tiers that DO ship it.
#[cfg_attr(not(feature = "espnow"), allow(dead_code))]
pub fn clip_bytes(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate to at most `max` **characters** (Unicode scalar values). Was the `clip` in
/// `batt.rs` / `grid.rs`.
///
/// Use this where the budget is a glyph count — a fixed-width font's columns, say.
pub fn clip_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
