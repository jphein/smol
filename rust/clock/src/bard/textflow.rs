//! bard (#300): word-wrap for the 72×40 panel — the one piece of the UI that is pure logic,
//! so it is host-tested instead of eyeballed on hardware.
//!
//! Pure, `no_std`, zero alloc: [`wrap_tail`] returns byte SPANS into the caller's text rather
//! than copied lines, so the 1 KB story buffer is never duplicated. That frugality is not
//! stylistic — on the canonical fleet tier the bard's `.bss` comes out of the RUNTIME STACK,
//! which the linker shrinks silently, leaving 14240 B (see `nano_llm::SEQ_CAP` for all three
//! measured points). `tools/repro_build.sh` refuses to package an image with less than 12288 B,
//! so a second copy of the story buffer here could turn into a failed release build.

/// Word-wrap `text` to `cols` columns and write the LAST `rows` line spans into `out`,
/// returning how many were written.
///
/// Each span is `(start, end)` byte offsets into `text`. Breaking rules, in order:
///   * `\n` is a hard break (the model emits them, and a story's paragraphs should survive).
///   * A line that would exceed `cols` breaks at the last space, which is CONSUMED — so the
///     next line starts with the word, not with a leading blank.
///   * A word longer than `cols` is hard-broken at the column edge rather than dropped.
///
/// Columns are counted in BYTES. That is exact for the ASCII these stories are made of, and a
/// multi-byte token would only mis-measure one line's width — never index outside `text`, and
/// never split a span the renderer can't decode (it skips a line it cannot read as UTF-8).
///
/// Wrapping runs FORWARD from the start of `text`, keeping the last `rows` spans in a small
/// rotating window. That matters for how it looks: break positions depend on where a line
/// begins, so wrapping the tail backwards would make already-visible words re-flow as the story
/// grows. Forward wrapping keeps every line stable once it appears — the text scrolls, it does
/// not shuffle. One pass over ≤1 KB per frame, no line buffer beyond `rows` spans.
pub fn wrap_tail(text: &[u8], cols: usize, rows: usize, out: &mut [(u16, u16)]) -> usize {
    if rows == 0 || out.is_empty() || cols == 0 {
        return 0;
    }
    let rows = rows.min(out.len());
    // Rotating window: line N lands in out[N % rows], so `out` only ever holds the newest rows.
    let mut total = 0usize;
    let mut emit = |start: usize, end: usize, total: &mut usize| {
        out[*total % rows] = (start as u16, end as u16);
        *total += 1;
    };

    let (mut line_start, mut col, mut last_space) = (0usize, 0usize, None::<usize>);
    let mut i = 0usize;
    while i < text.len() {
        if text[i] == b'\n' {
            emit(line_start, i, &mut total);
            i += 1;
            (line_start, col, last_space) = (i, 0, None);
            continue;
        }
        if col == cols {
            // The overflowing character is itself a space: the line fits EXACTLY, so break here
            // and swallow that space. (Without this, "aaa bbb" at 7 cols would backtrack to the
            // earlier space and emit "aaa" — every line one word narrower than it should be.)
            if text[i] == b' ' {
                emit(line_start, i, &mut total);
                line_start = i + 1;
                i = line_start;
                (col, last_space) = (0, None);
                continue;
            }
            match last_space {
                // Break at the space and swallow it.
                Some(sp) if sp > line_start => {
                    emit(line_start, sp, &mut total);
                    line_start = sp + 1;
                }
                // No space to break at: hard-break mid-word.
                _ => {
                    emit(line_start, i, &mut total);
                    line_start = i;
                }
            }
            i = line_start;
            (col, last_space) = (0, None);
            continue;
        }
        if text[i] == b' ' {
            last_space = Some(i);
        }
        col += 1;
        i += 1;
    }
    if line_start < text.len() {
        emit(line_start, text.len(), &mut total);
    }

    // Un-rotate IN PLACE: the window holds the newest `rows` lines starting at `total % rows`,
    // so one rotate puts them in reading order — no temporary, and no cap on `rows`.
    if total > rows {
        out[..rows].rotate_left(total % rows);
        return rows;
    }
    total
}
