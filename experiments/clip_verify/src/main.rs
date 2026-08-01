//! #274 host guard. `#[path]`-includes the REAL `rssi.rs` (no drift) and asserts that `clip()`
//! cannot panic for any input at any budget, and that it still truncates the way callers expect.
//! `cargo run`.
//!
//! The bug this pins: `clip()` was `&s[..s.len().min(n)]`, a BYTE slice, documented as safe because
//! "magical nouns are ASCII". True, but unenforced — the corpus held the invariant, not the code.
//! The panic would have surfaced as a reboot on whichever board first rendered a non-ASCII string.

#[path = "../../../rust/clock/src/rssi.rs"]
mod rssi;

use rssi::clip;

/// Every prefix length of `s`, including 0 and past the end. If any budget panics, the harness dies
/// with the byte index that did it, which is the number you need to debug it.
fn sweep(s: &str, label: &str) {
    for n in 0..=s.len() + 4 {
        let out = clip(s, n);
        assert!(
            out.len() <= n.min(s.len()),
            "{label}: clip(_, {n}) returned {} bytes — over budget",
            out.len()
        );
        assert!(
            s.starts_with(out),
            "{label}: clip(_, {n}) = {out:?} is not a prefix of the input"
        );
        // The whole point: `out` is a &str, so if the boundary walk were wrong we'd have panicked
        // inside clip() before getting here. Re-validate anyway — cheap, and it catches an
        // implementation that reaches for `from_utf8_unchecked` later.
        assert!(
            core::str::from_utf8(out.as_bytes()).is_ok(),
            "{label}: clip(_, {n}) produced invalid UTF-8"
        );
    }
}

fn main() {
    // ---- the regression: multibyte input at every budget ------------------------------------
    // café — 'é' is 2 bytes, so a budget of 4 lands mid-character. The old code panicked here.
    sweep("café", "café (2-byte tail)");
    // Greek: every char is 2 bytes, so half of all budgets are mid-character.
    sweep("Ωμέγα", "greek (all 2-byte)");
    // Emoji: 4 bytes each — budgets 1..3 into the first char all split it.
    sweep("🐉🔥", "emoji (4-byte)");
    // Mixed widths, the nastiest shape: 1-, 2-, 3- and 4-byte chars in one string.
    sweep("a\u{00e9}\u{20ac}\u{1f409}z", "mixed 1/2/3/4-byte");
    // Degenerate inputs.
    sweep("", "empty");
    sweep("\u{1f409}", "single 4-byte char");

    // ---- the specific case the old implementation died on -----------------------------------
    // Budget 4 on "café": bytes are [c][a][f][0xC3][0xA9]. Byte index 4 is INSIDE 'é'.
    assert_eq!(clip("café", 4), "caf", "must drop the straddling char, not split it");
    assert_eq!(clip("café", 5), "café", "budget covers the whole string");
    assert_eq!(clip("café", 3), "caf");

    // A char wider than the entire budget yields empty, never a partial char.
    assert_eq!(clip("🐉", 1), "", "4-byte char, 1-byte budget → empty");
    assert_eq!(clip("🐉", 3), "", "4-byte char, 3-byte budget → empty");
    assert_eq!(clip("🐉", 4), "🐉");

    // ---- ASCII behaviour is UNCHANGED (this is a fleet-wide screen-layout guarantee) ---------
    // Every OLED call site sizes its budget in glyphs assuming 1 byte == 1 glyph. If the hardening
    // changed ASCII truncation by even one character, screens would reflow.
    for s in ["Ember", "Molten Engine", "smol", ""] {
        for n in 0..=s.len() + 4 {
            assert_eq!(
                clip(s, n),
                &s[..s.len().min(n)],
                "ASCII truncation changed for {s:?} at budget {n} — screens would reflow"
            );
        }
    }

    // ---- the Line buffer that feeds clip() at every real call site --------------------------
    // ota_screen.rs builds a Line, then clips it. Line truncates at 24 bytes on write, which can
    // itself tear a multibyte char; as_str() is from_utf8(..).unwrap_or(""), so clip() receives
    // either valid UTF-8 or "". Pin that, since it is half of why the old code survived.
    use core::fmt::Write;
    let mut line = rssi::Line::new();
    for _ in 0..12 {
        let _ = write!(line, "🐉");
    }
    let s = line.as_str();
    assert!(
        core::str::from_utf8(s.as_bytes()).is_ok(),
        "Line::as_str must never hand clip() invalid UTF-8"
    );
    sweep(s, "Line-truncated emoji run");

    println!("clip_verify: OK — no budget panics across multibyte, emoji, mixed-width and Line-torn input;");
    println!("             ASCII truncation byte-identical to the pre-#274 implementation.");
}
