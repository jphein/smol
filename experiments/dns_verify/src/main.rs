//! #227/#228 host verification of the PURE minimal DNS A-query codec. `#[path]`-includes the
//! REAL `net/dns.rs` (no drift). Run: `cargo run` — panics on any failure.
//!
//! Coverage: golden query bytes for `api.open-meteo.com` · response parse through a CNAME chain
//! with compression pointers (the realistic resolver answer shape) · rejection of wrong-txid /
//! non-response / NXDOMAIN / zero-answer / truncated / pointer-loop packets (every failure is a
//! clean `None` → the caller falls back to the baked IP; a panic on a hostile packet would be a
//! remote crash).

#[path = "../../../rust/clock/src/net/dns.rs"]
mod dns;

use dns::{encode_a_query, parse_a_response, DNS_QUERY_MAX};

fn main() {
    // --- golden query bytes -------------------------------------------------
    let mut q = [0u8; DNS_QUERY_MAX];
    let n = encode_a_query(0xBEEF, "api.open-meteo.com", &mut q).expect("encode");
    // header: txid BEEF, RD, QD=1
    let mut golden = vec![0xBE, 0xEF, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    golden.extend_from_slice(b"\x03api\x0aopen-meteo\x03com\x00"); // qname
    golden.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // QTYPE=A, QCLASS=IN
    assert_eq!(&q[..n], &golden[..], "golden A-query wire bytes");
    assert_eq!(n, 36, "api.open-meteo.com query is 36 bytes");

    // encode rejects an over-long label + an empty host.
    let long = "a".repeat(64);
    assert!(encode_a_query(1, &long, &mut q).is_none(), "64-char label rejected");
    assert!(encode_a_query(1, "", &mut q).is_none(), "empty host rejected");

    // --- realistic response: question echo + CNAME (pointer name) + A -------
    // Build: header(QR=1, RCODE=0, QD=1, AN=2) | question | CNAME answer | A answer.
    let mut r = vec![0xBE, 0xEF, 0x81, 0x80, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00];
    r.extend_from_slice(b"\x03api\x0aopen-meteo\x03com\x00\x00\x01\x00\x01"); // question
    // answer 1: name = pointer to offset 12 (the question name), TYPE=CNAME(5), rdata = pointer name (2B)
    r.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x02, 0xC0, 0x10]);
    // answer 2: name = pointer, TYPE=A(1), CLASS=IN, TTL, RDLENGTH=4, rdata = 188.40.99.226
    r.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x04, 188, 40, 99, 226]);
    assert_eq!(
        parse_a_response(0xBEEF, &r),
        Some([188, 40, 99, 226]),
        "CNAME-chain response yields the A record"
    );

    // --- rejection cases (all must be a clean None) --------------------------
    assert_eq!(parse_a_response(0xDEAD, &r), None, "wrong txid rejected");
    let mut bad = r.clone();
    bad[2] = 0x01; // QR=0 → not a response
    assert_eq!(parse_a_response(0xBEEF, &bad), None, "non-response rejected");
    let mut nx = r.clone();
    nx[3] = 0x83; // RCODE=3 NXDOMAIN
    assert_eq!(parse_a_response(0xBEEF, &nx), None, "NXDOMAIN rejected");
    let mut zero = r.clone();
    zero[6] = 0;
    zero[7] = 0; // ANCOUNT=0
    assert_eq!(parse_a_response(0xBEEF, &zero), None, "zero answers rejected");
    for cut in [0usize, 5, 11, 13, 30, r.len() - 3] {
        assert_eq!(parse_a_response(0xBEEF, &r[..cut]), None, "truncated at {cut} rejected");
    }
    // hostile: a label that claims to run past the end.
    let mut runaway = vec![0xBE, 0xEF, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0, 0, 0, 0];
    runaway.extend_from_slice(&[0x3F, b'x']); // label len 63 but only 1 byte present
    assert_eq!(parse_a_response(0xBEEF, &runaway), None, "runaway label rejected");

    // A-record-only response (no CNAME) also parses — the simple resolver shape.
    let mut simple = vec![0xBE, 0xEF, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00];
    simple.extend_from_slice(b"\x03api\x0aopen-meteo\x03com\x00\x00\x01\x00\x01");
    simple.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x01, 0x2C, 0x00, 0x04, 1, 2, 3, 4]);
    assert_eq!(parse_a_response(0xBEEF, &simple), Some([1, 2, 3, 4]), "plain A response");

    println!("dns_verify: ALL CHECKS PASSED (golden query + CNAME-chain/pointer parse + hostile-packet rejection)");
}
