//! OTA fetch-leg HTTP parser host guard. `#[path]`-includes the REAL `net/http.rs` (no drift) and
//! pins the #gateway-election byte-0 fetch bug + robustness. `cargo run`.

#[path = "../../../rust/clock/src/net/http.rs"]
mod http;

use http::*;

fn main() {
    // ---- THE byte-0 bug: header + start of BINARY body coalesced into ONE TCP segment -----------
    // rangeserver.py (Python BaseHTTPServer) answers "HTTP/1.0 206 …"; the response head + the first
    // body bytes arrived together (rx=956B on HW). The OLD parser fed the WHOLE buffer to from_utf8,
    // which fails on the binary body → status parsed None → the fetch aborted at byte 0.
    let mut resp: Vec<u8> = Vec::new();
    resp.extend_from_slice(
        b"HTTP/1.0 206 Partial Content\r\n\
          Server: SimpleHTTP/0.6 Python/3.12.3\r\n\
          Content-Range: bytes 0-49151/1164864\r\n\
          Content-Length: 49152\r\n\r\n",
    );
    let hdr_only = resp.len();
    // …then the binary image body (invalid UTF-8: PNG-ish magic + high bytes).
    resp.extend_from_slice(&[0x00, 0xFF, 0xFE, 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A]);

    let be = header_end(&resp).expect("header_end must find \\r\\n\\r\\n");
    assert_eq!(be, hdr_only, "body starts one past the terminator");

    // The regression: status parses to 206 EVEN when the buffer carries trailing binary body.
    assert_eq!(
        status_code(&resp),
        Some(206),
        "HTTP/1.0 206 must parse with a trailing BINARY body in the same buffer (the byte-0 bug)"
    );
    // And on the header slice the fixed call site actually passes.
    assert_eq!(status_code(&resp[..be]), Some(206), "206 on the header slice");
    assert_eq!(content_length(&resp[..be]), Some(49152), "content-length on the header slice");
    assert_eq!(
        content_length(&resp),
        Some(49152),
        "content-length robust to trailing binary body (stops at the blank line)"
    );

    // ---- version-agnostic + other paths ---------------------------------------------------------
    assert_eq!(status_code(b"HTTP/1.1 200 OK\r\n\r\n"), Some(200), "HTTP/1.1 200 (fallback path)");
    assert_eq!(status_code(b"HTTP/1.0 206 Partial Content\r\n\r\n"), Some(206), "clean 1.0 206");
    assert_eq!(status_code(b"HTTP/1.0 416 Range Not Satisfiable\r\n\r\n"), Some(416), "416 parses (caller rejects)");
    assert_eq!(status_code(b"HTTP/1.0 404 Not Found\r\n\r\n"), Some(404), "404 parses");
    // incomplete header block → header_end None (keep accumulating).
    assert_eq!(header_end(b"HTTP/1.0 206 Partial\r\nServer: x\r\n"), None, "incomplete headers → None");
    // content-length absent → None.
    assert_eq!(content_length(b"HTTP/1.0 206 Partial\r\nServer: x\r\n\r\n"), None, "no content-length → None");
    // case-insensitive header name.
    assert_eq!(content_length(b"HTTP/1.0 200 OK\r\ncontent-length: 7\r\n\r\n"), Some(7), "case-insensitive");

    println!("ota_http_verify: ALL CHECKS PASSED (HTTP/1.0 206 + coalesced binary body + content-length + version-agnostic + non-206 + incomplete)");
}
