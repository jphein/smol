//! Minimal HTTP/1.x response-head parsers for the OTA fetch leg — PURE (no deps, no cfg, no alloc),
//! so the firmware and `experiments/ota_http_verify` share ONE definition (no drift), mirroring the
//! `coexist`/`election` pattern.
//!
//! #gateway-election byte-0 fetch BUG these fix: a valid `HTTP/1.0 206 Partial Content` response
//! whose header AND the start of the (binary) image body arrive in ONE TCP segment was fed WHOLE to
//! `from_utf8` — which fails on the binary body → the status parsed as `None` → the fetch aborted at
//! byte 0 with `bad=true`, even though the link was fine (rx=956B, valid 206 head on serial). It was
//! segmentation-dependent (passed when the header arrived in its own segment), which is why it hid.
//! FIX: every parser slices to the relevant ASCII span on BYTES *before* any `from_utf8`, so a
//! coalesced binary body can never break header parsing; and the status parser accepts ANY minor
//! version (1.0/1.1). Callers additionally pass only the header slice (`..header_end`).

/// Index one-past the header terminator (`\r\n\r\n`) — the body start. `None` if incomplete.
pub fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Status code from an HTTP/1.x status line, e.g. `HTTP/1.0 206 Partial Content` → `206`. Version-
/// agnostic (the code is the 2nd whitespace token). ROBUST when `buf` also carries trailing header /
/// BODY bytes (incl. binary): only the FIRST line is considered, sliced on BYTES before `from_utf8`,
/// so a coalesced binary body can never make it return `None` (the byte-0 bug).
pub fn status_code(buf: &[u8]) -> Option<u16> {
    let end = buf
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(buf.len());
    let line = core::str::from_utf8(&buf[..end]).ok()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// `Content-Length` (case-insensitive) from the header block. ROBUST: walks line-by-line on BYTES
/// (a stray non-UTF-8 / body byte in a later line can't abort the search) and UTF-8-decodes only
/// each candidate line; stops at the blank line (end of headers). `None` if absent/unparseable.
pub fn content_length(buf: &[u8]) -> Option<u32> {
    let mut start = 0;
    while start < buf.len() {
        let nl = buf[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| start + i)
            .unwrap_or(buf.len());
        let mut end = nl;
        if end > start && buf[end - 1] == b'\r' {
            end -= 1; // strip the trailing CR
        }
        if end == start {
            break; // blank line → end of headers
        }
        if let Ok(line) = core::str::from_utf8(&buf[start..end]) {
            if let Some((name, val)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    return val.trim().parse().ok();
                }
            }
        }
        if nl >= buf.len() {
            break;
        }
        start = nl + 1;
    }
    None
}
