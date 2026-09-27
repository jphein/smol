//! #548 Tapstone gateway — host guard for the `@TS1 ` line codec. `#[path]`-includes the REAL
//! `net/ts_lines.rs` and `net/wire.rs` (no drift). Panics on the first failure; prints the count
//! of checks that ran, because "ok" from a suite that silently ran nothing is not evidence.
//!
//! The byte-for-byte goldens below are the formats tapstone's `link/lines.rs` parses and emits
//! (`tx_line`, `ping_line`, the `serial_pty.rs` fixture). They are a copy of the other side's
//! contract, so `experiments/tapstone_lines_xcheck` additionally runs tapstone's real parser over
//! what this codec writes.

#[path = "../../../rust/clock/src/net/ts_lines.rs"]
#[allow(dead_code)]
mod ts_lines;

#[path = "../../../rust/clock/src/net/wire.rs"]
#[allow(dead_code)]
mod wire;

use std::cell::Cell;
use ts_lines::*;

thread_local!(static CHECKS: Cell<u32> = const { Cell::new(0) });

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {{
        CHECKS.with(|c| c.set(c.get() + 1));
        if !$cond {
            panic!("FAIL {}: {}", stringify!($cond), format!($($msg)+));
        }
    }};
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A MATCH frame of `len` bytes (≥ 13): the tag, then a counting body.
fn match_frame(len: usize) -> Vec<u8> {
    let mut f = MATCH_TAG.to_vec();
    f.extend((0..len.saturating_sub(MATCH_TAG.len())).map(|i| (i * 7 + 3) as u8));
    f.truncate(len);
    f
}

/// Exactly what tapstone `lines::tx_line` produces: `format!("{PREFIX}TX {tx_id} {dst} {}\n", hex)`.
fn arena_tx_line(id: u32, dst: u8, frame: &[u8]) -> String {
    format!("@TS1 TX {id} {dst} {}\n", hex(frame))
}

fn parse(line: &str) -> (Inbound, [u8; TX_FRAME_MAX]) {
    let mut buf = [0u8; TX_FRAME_MAX];
    let r = parse_inbound(line.as_bytes(), false, &mut buf);
    (r, buf)
}

fn refused(line: &str) -> Option<Reason> {
    match parse(line).0 {
        Inbound::TxRefused { reason, .. } => Some(reason),
        _ => None,
    }
}

/// Format through the SAME adapter the firmware prints through (`ts_lines::Show`).
fn line<F: Fn(&mut std::fmt::Formatter<'_>) -> std::fmt::Result>(f: F) -> String {
    format!("{}", Show(f))
}

fn encoders() {
    // The serial_pty.rs fixture, byte for byte.
    check!(
        line(|w| write_rx(w, 163, -40, true, b"SMOL")) == "@TS1 RX 163 -40 1 534d4f4c",
        "RX golden"
    );
    check!(line(|w| write_rx(w, 164, -50, false, b"MA")) == "@TS1 RX 164 -50 0 4d41", "RX mac_ok 0");
    check!(line(|w| write_rx(w, 0, -128, true, &[0xAB])) == "@TS1 RX 0 -128 1 ab", "RX extremes, lowercase");
    check!(line(|w| write_rx(w, 255, 0, true, &[])) == "@TS1 RX 255 0 1 ", "RX empty frame");
    check!(
        line(|w| write_hello(w, &[0xac, 0xa7, 0x04, 0xb9, 0x77, 0x14], 163, "1a2b3c4d", 129))
            == "@TS1 HELLO ac:a7:04:b9:77:14 163 1a2b3c4d 129",
        "HELLO golden"
    );
    check!(line(|w| write_txok(w, 4_294_967_295)) == "@TS1 TXOK 4294967295", "TXOK");
    check!(line(|w| write_txerr(w, 7, Reason::UnknownDst)) == "@TS1 TXERR 7 unknown-dst", "TXERR");
    let two = [(8u8, [1, 2, 3, 4, 5, 6], -40i8), (51, [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff], -81)];
    check!(
        line(|w| write_roster(w, &two).map(|_| ()))
            == "@TS1 ROSTER 8:01:02:03:04:05:06:-40,51:aa:bb:cc:dd:ee:ff:-81",
        "ROSTER golden"
    );
    let mut b = String::new();
    check!(write_roster(&mut b, &[]) == Ok(false) && b.is_empty(), "empty ROSTER writes nothing");
    check!(write_roster(&mut b, &two[..1]) == Ok(true), "non-empty ROSTER reports written");

    // Every reason is one token (TXERR's reason is the rest of the line; a space would be a second
    // word the arena joins back, but a single token keeps the log greppable) and kebab-case.
    for r in [
        Reason::BadDst, Reason::BadHex, Reason::TooLong, Reason::NotMatch, Reason::Short,
        Reason::ExtraField, Reason::UnknownDst, Reason::SelfDst, Reason::SendFailed, Reason::NoRadio,
    ] {
        let s = r.as_str();
        check!(!s.is_empty() && s.bytes().all(|c| c.is_ascii_lowercase() || c == b'-'), "reason {s:?}");
    }

    check!(fw_hash8("1a2b3c4d5e6f7a8b") == "1a2b3c4d", "fw_hash8 cuts at 8");
    check!(fw_hash8("abc1234-dirty") == "abc1234", "fw_hash8 cuts at non-alnum");
    check!(fw_hash8("nogit") == "nogit", "fw_hash8 short");
    check!(fw_hash8("") == "unknown" && fw_hash8("-x") == "unknown", "fw_hash8 empty");

    // Sizing: the longest line the arena can send is exactly what the reader holds.
    let tx_max = arena_tx_line(u32::MAX, 255, &match_frame(TX_FRAME_MAX));
    check!(tx_max.trim_end().len() == IN_LINE_MAX, "IN_LINE_MAX is the maximal TX ({} vs {})", tx_max.trim_end().len(), IN_LINE_MAX);
}

fn decoder() {
    let f = match_frame(38);
    let (r, buf) = parse(&arena_tx_line(7, 255, &f));
    check!(r == Inbound::Tx { id: 7, dst: 255, len: 38 } && buf[..38] == f[..], "arena tx_line round-trips: {r:?}");
    let (r, _) = parse(&arena_tx_line(7, 8, &f).replace('\n', "\r\n"));
    check!(r == Inbound::Tx { id: 7, dst: 8, len: 38 }, "CRLF tolerated");
    let (r, buf) = parse(&format!("@TS1 TX 1 9 {}", hex(&f).to_uppercase()));
    check!(r == Inbound::Tx { id: 1, dst: 9, len: 38 } && buf[..38] == f[..], "uppercase hex, no newline");
    let (r, _) = parse(&arena_tx_line(u32::MAX, 0, &match_frame(TX_FRAME_MAX)));
    check!(r == Inbound::Tx { id: u32::MAX, dst: 0, len: TX_FRAME_MAX }, "maximal TX accepted");
    let (r, _) = parse(&arena_tx_line(3, 1, &match_frame(MATCH_HEADER_LEN)));
    check!(r == Inbound::Tx { id: 3, dst: 1, len: MATCH_HEADER_LEN }, "exactly a header is enough");

    // PING — exactly tapstone's ping_line.
    check!(parse("@TS1 PING\n").0 == Inbound::Ping, "PING");
    check!(parse("@TS1 PING\r\n").0 == Inbound::Ping, "PING CRLF");
    check!(parse("@TS1 PING x").0 == Inbound::Ignored, "PING with an argument");
    check!(parse("@TS1 PING ").0 == Inbound::Ignored, "PING trailing space");

    // Not ours, or unanswerable.
    for l in [
        "", "\n", "I (12) boot: hello", "@TS1", "@TS1 ", "@ts1 PING", " @TS1 PING", "@TS1 FOO 1",
        "@TS1 TX", "@TS1 TX ", "@TS1 TX abc 1 00", "@TS1 TX -1 1 00", "@TS1 TX +1 1 00",
        "@TS1 TX 4294967296 1 00", "@TS1 TX 1.0 1 00", "@TS1  TX 1 1 00", "@TS1 RX 1 -40 1 00",
    ] {
        check!(parse(l).0 == Inbound::Ignored, "ignored: {l:?} -> {:?}", parse(l).0);
    }

    let h = hex(&f);
    check!(refused("@TS1 TX 5") == Some(Reason::BadDst), "dst missing");
    check!(refused(&format!("@TS1 TX 5 256 {h}")) == Some(Reason::BadDst), "dst 256");
    check!(refused(&format!("@TS1 TX 5 -1 {h}")) == Some(Reason::BadDst), "dst negative");
    check!(refused(&format!("@TS1 TX 5  {h}")) == Some(Reason::BadDst), "double space");
    check!(refused("@TS1 TX 5 255") == Some(Reason::BadHex), "hex missing");
    check!(refused("@TS1 TX 5 255 ") == Some(Reason::BadHex), "hex empty (tx_line of an empty frame)");
    check!(refused(&format!("@TS1 TX 5 255 {}", &h[1..])) == Some(Reason::BadHex), "odd hex");
    check!(refused(&format!("@TS1 TX 5 255 {}zz", &h[2..])) == Some(Reason::BadHex), "non-hex digit");
    check!(refused(&format!("@TS1 TX 5 255 {h} x")) == Some(Reason::ExtraField), "fifth field");
    check!(refused(&format!("@TS1 TX 5 255 {h} ")) == Some(Reason::ExtraField), "trailing space");
    check!(refused(&arena_tx_line(5, 255, &match_frame(TX_FRAME_MAX + 1))) == Some(Reason::TooLong), "242 B");
    check!(refused(&arena_tx_line(5, 255, b"SMOLv1 HELLO 007")) == Some(Reason::NotMatch), "not a MATCH frame");
    check!(refused(&arena_tx_line(5, 255, b"\x01\x02")) == Some(Reason::NotMatch), "serial_pty's b\"\\x01\\x02\"");
    check!(refused(&arena_tx_line(5, 255, &match_frame(MATCH_HEADER_LEN - 1))) == Some(Reason::Short), "19 B");
    check!(
        matches!(parse("@TS1 TX 77 255 zz").0, Inbound::TxRefused { id: 77, .. }),
        "a refusal names the line's own id"
    );

    // Truncation from the line reader.
    let mut buf = [0u8; TX_FRAME_MAX];
    check!(
        parse_inbound(b"@TS1 TX 9 255 0011", true, &mut buf) == Inbound::TxRefused { id: 9, reason: Reason::TooLong },
        "truncated TX with a readable id -> too-long"
    );
    check!(parse_inbound(b"@TS1 TX 9", true, &mut buf) == Inbound::TxRefused { id: 9, reason: Reason::TooLong }, "truncated after the id");
    check!(parse_inbound(b"@TS1 TX x", true, &mut buf) == Inbound::Ignored, "truncated, no id");
    check!(parse_inbound(b"@TS1 PING", true, &mut buf) == Inbound::Ignored, "truncated PING is not a PING");

    // Deterministic sweep: every length 20..=241, varied ids/dsts, all round-trip.
    let mut seed = 0x2545_f491_u32;
    for len in MATCH_HEADER_LEN..=TX_FRAME_MAX {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let mut fr = MATCH_TAG.to_vec();
        fr.extend((MATCH_TAG.len()..len).map(|i| (seed >> (i % 24)) as u8));
        let (id, dst) = (seed, (seed >> 8) as u8);
        let (r, b) = parse(&arena_tx_line(id, dst, &fr));
        check!(r == Inbound::Tx { id, dst, len } && b[..len] == fr[..], "sweep len {len}");
    }
}

fn line_reader() {
    let mut rd = LineReader::new();
    let mut got: Vec<(Vec<u8>, bool)> = Vec::new();
    for &b in b"@TS1 PING\nI (1) log\n@TS1 TX" {
        if let Some((l, t)) = rd.push(b) {
            got.push((l.to_vec(), t));
        }
    }
    check!(got == vec![(b"@TS1 PING".to_vec(), false), (b"I (1) log".to_vec(), false)], "{got:?}");
    check!(rd.in_progress(), "a partial line is held");
    got.clear();
    // An overlong TX: the reader keeps the head, flags the rest, and the parser still finds the id.
    let long = arena_tx_line(42, 255, &match_frame(TX_FRAME_MAX + 40));
    let mut rd = LineReader::new();
    for &b in long.as_bytes() {
        if let Some((l, t)) = rd.push(b) {
            got.push((l.to_vec(), t));
        }
    }
    check!(got.len() == 1 && got[0].1 && got[0].0.len() == IN_LINE_MAX, "overflow flagged, head kept");
    let mut buf = [0u8; TX_FRAME_MAX];
    check!(
        parse_inbound(&got[0].0, got[0].1, &mut buf) == Inbound::TxRefused { id: 42, reason: Reason::TooLong },
        "overlong TX answered too-long"
    );
    check!(!rd.in_progress(), "reset after the newline");
    // The next line is clean.
    let mut next = None;
    for &b in b"@TS1 PING\n" {
        if let Some((l, t)) = rd.push(b) {
            next = Some((l.to_vec(), t));
        }
    }
    check!(next == Some((b"@TS1 PING".to_vec(), false)), "no overflow carries into the next line");
}

fn trailer() {
    use wire::{append_group_mac, verify_group_mac, MacVerdict, ESP_NOW_MTU, MAC_TRAILER_LEN};
    check!(TX_FRAME_MAX == ESP_NOW_MTU - MAC_TRAILER_LEN, "TX_FRAME_MAX derives from wire.rs");
    check!(TRAILER_LEN == MAC_TRAILER_LEN, "TRAILER_LEN is wire's");
    check!(wire::should_group_mac(&match_frame(TX_FRAME_MAX)), "a maximal TX frame is MAC'd by send_to");
    check!(!wire::should_group_mac(&match_frame(TX_FRAME_MAX + 1)), "one byte more is sent raw (why TX caps at 241)");

    // The same mapping net/mode.rs applies to the live verdict.
    let map = |v: MacVerdict| match v {
        MacVerdict::Ok { .. } => Trailer::Verified,
        MacVerdict::BadTag => Trailer::BadTag,
        MacVerdict::Unkeyed => Trailer::Absent,
    };
    let key = [0x5au8; 32];
    let other = [0xa5u8; 32];
    let epoch = 0x81;
    let f = match_frame(60);
    let mut air = [0u8; ESP_NOW_MTU];
    air[..f.len()].copy_from_slice(&f);
    let n = append_group_mac(&mut air, f.len(), &key, epoch);

    let v = verify_group_mac(&air[..n], &[(epoch, &key)]);
    check!(matches!(v, MacVerdict::Ok { payload_len: 60 }), "{v:?}");
    check!(forward(&air[..n], map(v)) == (&f[..], true), "verified: trailer stripped, mac_ok 1");

    let v = verify_group_mac(&air[..n], &[(epoch, &other)]);
    check!(v == MacVerdict::BadTag, "{v:?}");
    check!(forward(&air[..n], map(v)) == (&f[..], false), "wrong key: trailer stripped, mac_ok 0");

    let v = verify_group_mac(&f, &[(epoch, &key)]);
    check!(v == MacVerdict::Unkeyed, "{v:?}");
    check!(forward(&f, map(v)) == (&f[..], false), "no trailer: forwarded whole, mac_ok 0");

    // A frame from another epoch still carries 9 trailing bytes we cannot identify; forwarded whole.
    let n2 = append_group_mac(&mut air, f.len(), &key, 0x82);
    let v = verify_group_mac(&air[..n2], &[(epoch, &key)]);
    check!(v == MacVerdict::Unkeyed && forward(&air[..n2], map(v)).0.len() == n2, "other epoch: whole");
}

fn main() {
    encoders();
    decoder();
    line_reader();
    trailer();
    let n = CHECKS.with(|c| c.get());
    // A floor on what ran (tapstone docs/verification.md: "a skip guard needs a floor").
    assert!(n >= 300, "only {n} checks ran");
    println!("tapstone_gw_verify: {n} checks passed");
}
