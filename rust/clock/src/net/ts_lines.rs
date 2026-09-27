//! #548 Tapstone gateway — the `@TS1 ` USB line codec (tapstone arena spec §4, D6).
//!
//! The gateway (`--features tapstone-gw`, `src/ts_gw.rs`) and the arena laptop talk over the
//! USB-Serial-JTAG in newline-delimited text. Every line the arena cares about starts `@TS1 `;
//! anything else is a log line, which is why smol's ordinary `log::` output can keep printing on
//! the same wire. Frames travel as hex.
//!
//! | direction | line |
//! |---|---|
//! | gateway → arena | `@TS1 HELLO <mac> <node_id> <fw_hash8> <group_epoch>` |
//! | gateway → arena | `@TS1 RX <src_id> <rssi> <mac_ok 0\|1> <hex>` |
//! | gateway → arena | `@TS1 TXOK <tx_id>` / `@TS1 TXERR <tx_id> <reason>` |
//! | gateway → arena | `@TS1 ROSTER <id>:<mac>:<rssi>,…` |
//! | arena → gateway | `@TS1 TX <tx_id> <dst_id\|255> <hex>` |
//! | arena → gateway | `@TS1 PING` |
//!
//! **The other end is the contract.** The arena's parser is tapstone
//! `rust/tapstone-arena/src/link/lines.rs`; it splits on single spaces (`split(' ')`), reads
//! `node`/`epoch`/`src` as decimal `u8`, `rssi` as `i8`, `mac_ok` as `== "1"`, the tx id as `u32`,
//! and needs at least one ROSTER entry. So every encoder here emits single spaces, no trailing
//! space, decimal numbers and lowercase hex, and the decoder is as strict as that parser is.
//! `experiments/tapstone_gw_verify` pins the bytes; `experiments/tapstone_lines_xcheck` feeds them
//! through tapstone's real `lines.rs`.
//!
//! Pure, `no_std`, no alloc and no `crate::` paths, so the host verifiers can `#[path]`-include
//! this exact file (the `wire.rs` / `mesh_elect.rs` convention).

use core::fmt::{self, Write};

/// Line prefix. Byte-identical to tapstone's `lines::PREFIX`.
pub const PREFIX: &str = "@TS1 ";

/// The MATCH frame family tag (tapstone protocol draft §2). The gateway forwards these and only
/// these; it does not interpret what follows.
pub const MATCH_TAG: &[u8] = b"SMOLv1 MATCH ";

/// The MATCH common header: tag 13 · ver 1 · kind 1 · match u32 LE 4 · src 1 (draft §2).
pub const MATCH_HEADER_LEN: usize = MATCH_TAG.len() + 1 + 1 + 4 + 1;

/// Offset of the header's `src` byte (the sender's claimed node id).
pub const MATCH_SRC_OFFSET: usize = MATCH_HEADER_LEN - 1;

/// Largest frame the gateway accepts from `TX`: the ESP-NOW MTU (250) less the #190 trailer (9),
/// so `send_to` always has room to append the MAC. The two constants live in `net/wire.rs`, which
/// this file may not name (host-includable); `net/mode.rs` const-asserts the equality so the
/// number here cannot drift from them.
pub const TX_FRAME_MAX: usize = 241;

/// Longest inbound line the gateway must hold to parse a maximal `TX`:
/// `@TS1 TX 4294967295 255 ` + two hex digits per frame byte.
pub const IN_LINE_MAX: usize = PREFIX.len() + "TX 4294967295 255 ".len() + 2 * TX_FRAME_MAX;

/// Is this received ESP-NOW payload a MATCH frame?
pub fn is_match(frame: &[u8]) -> bool {
    frame.starts_with(MATCH_TAG)
}

/// The #190 group-MAC trailer length (`wire::MAC_TRAILER_LEN`; const-asserted equal in
/// `net/mode.rs`, same reason as [`TX_FRAME_MAX`]).
pub const TRAILER_LEN: usize = 9;

/// What `wire::verify_group_mac` said about a received frame, reduced to what the gateway needs.
/// `net/mode.rs` maps `MacVerdict` onto this one-to-one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trailer {
    /// `MacVerdict::Ok`: a trailer, and it verified.
    Verified,
    /// `MacVerdict::BadTag`: the frame claimed an accepted epoch, so it carries a trailer, and the
    /// tag did not verify (wrong key, corruption, forgery).
    BadTag,
    /// `MacVerdict::Unkeyed`: nothing claimed an accepted epoch, so there is no trailer we know of
    /// (a frame sent raw, or another epoch).
    Absent,
}

/// The bytes the gateway forwards as `RX <hex>`, and its `mac_ok`.
///
/// The trailer is stripped whenever the verdict says one is there — `Verified` **and** `BadTag` —
/// so the arena gets the frame as its sender built it, with `mac_ok` telling the two apart. Every
/// MATCH sender goes through `send_to`, which appends the trailer (protocol draft §7), so a MATCH
/// frame that claimed our epoch and failed is a trailered frame with the wrong key, not a raw frame
/// whose ninth-from-last byte happens to look like an epoch. `Absent` is forwarded whole: there is
/// nothing to strip. Only `Verified` is `mac_ok = 1`. The fleet parser keeps a `BadTag` frame whole
/// in observe mode (`service()`); it can afford to, since its parsers are length-tolerant, whereas
/// the arena is told "trailer stripped" (spec D6).
pub fn forward(raw: &[u8], t: Trailer) -> (&[u8], bool) {
    match t {
        Trailer::Verified => (&raw[..raw.len().saturating_sub(TRAILER_LEN)], true),
        Trailer::BadTag => (&raw[..raw.len().saturating_sub(TRAILER_LEN)], false),
        Trailer::Absent => (raw, false),
    }
}

/// Why a `TX` was refused. `as_str` is the single token after `TXERR <id> `; the arena logs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// `dst` missing or not a decimal `0..=255`.
    BadDst,
    /// hex missing, empty, odd-length or not hex.
    BadHex,
    /// the frame is longer than [`TX_FRAME_MAX`], or the line overran [`IN_LINE_MAX`].
    TooLong,
    /// the frame does not start `SMOLv1 MATCH ` — the gateway forwards MATCH frames only (D7).
    NotMatch,
    /// shorter than the 20-byte MATCH header.
    Short,
    /// a fifth field after the hex.
    ExtraField,
    /// `dst` is not in the roster (no HELLO heard from it yet), so there is no MAC to send to.
    UnknownDst,
    /// `dst` is the gateway's own id.
    SelfDst,
    /// the radio refused the frame (`esp_now.send` returned an error).
    SendFailed,
    /// the radio never came up at boot.
    NoRadio,
}

impl Reason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Reason::BadDst => "bad-dst",
            Reason::BadHex => "bad-hex",
            Reason::TooLong => "too-long",
            Reason::NotMatch => "not-match",
            Reason::Short => "short",
            Reason::ExtraField => "extra-field",
            Reason::UnknownDst => "unknown-dst",
            Reason::SelfDst => "self-dst",
            Reason::SendFailed => "send-failed",
            Reason::NoRadio => "no-radio",
        }
    }
}

/// One parsed inbound line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inbound {
    /// A well-formed MATCH frame to send: `frame[..len]` of the caller's buffer. `dst` 255 is
    /// broadcast.
    Tx { id: u32, dst: u8, len: usize },
    /// A `TX` whose id parsed but whose body did not: answer `TXERR <id> <reason>`.
    TxRefused { id: u32, reason: Reason },
    /// `@TS1 PING`: answer with `HELLO`.
    Ping,
    /// Not addressed to the gateway, or unanswerable (no prefix, an unknown verb, a `TX` whose id
    /// does not parse so no `TXERR` can name it). The caller logs and drops it.
    Ignored,
}

/// Strict decimal: ASCII digits only (no sign, no space), non-empty, no overflow. Rust's
/// `str::parse` would also take `+5`; the arena never sends that, so neither does this accept it.
fn dec_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    s.iter()
        .try_fold(0u32, |acc, b| acc.checked_mul(10)?.checked_add((b - b'0') as u32))
}

fn dec_u8(s: &[u8]) -> Option<u8> {
    dec_u32(s).and_then(|v| u8::try_from(v).ok())
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Parse one inbound line (without or with its trailing `\n` / `\r\n`).
///
/// `truncated` is the line reader's overflow flag: the line was longer than its buffer and `line`
/// holds only the first [`IN_LINE_MAX`] bytes. A truncated `TX` whose id is still readable is
/// refused `too-long` rather than dropped, so the arena hears why.
///
/// On `Inbound::Tx` the decoded frame is in `frame[..len]`.
pub fn parse_inbound(line: &[u8], truncated: bool, frame: &mut [u8; TX_FRAME_MAX]) -> Inbound {
    let mut line = line;
    while let [rest @ .., b'\r' | b'\n'] = line {
        line = rest;
    }
    let Some(body) = line.strip_prefix(PREFIX.as_bytes()) else {
        return Inbound::Ignored;
    };
    let mut w = body.split(|&b| b == b' ');
    match w.next() {
        Some(b"PING") if !truncated && w.next().is_none() => Inbound::Ping,
        Some(b"TX") => {
            let Some(id) = w.next().and_then(dec_u32) else {
                return Inbound::Ignored;
            };
            let refuse = |reason| Inbound::TxRefused { id, reason };
            if truncated {
                return refuse(Reason::TooLong);
            }
            let Some(dst) = w.next().and_then(dec_u8) else {
                return refuse(Reason::BadDst);
            };
            let hex = match w.next() {
                Some(h) if !h.is_empty() => h,
                _ => return refuse(Reason::BadHex),
            };
            if w.next().is_some() {
                return refuse(Reason::ExtraField);
            }
            if hex.len() % 2 != 0 {
                return refuse(Reason::BadHex);
            }
            let len = hex.len() / 2;
            if len > TX_FRAME_MAX {
                return refuse(Reason::TooLong);
            }
            for (i, pair) in hex.chunks_exact(2).enumerate() {
                match (nibble(pair[0]), nibble(pair[1])) {
                    (Some(hi), Some(lo)) => frame[i] = hi << 4 | lo,
                    _ => return refuse(Reason::BadHex),
                }
            }
            let f = &frame[..len];
            if !is_match(f) {
                return refuse(Reason::NotMatch);
            }
            if len < MATCH_HEADER_LEN {
                return refuse(Reason::Short);
            }
            Inbound::Tx { id, dst, len }
        }
        _ => Inbound::Ignored,
    }
}

fn write_hex(w: &mut impl Write, bytes: &[u8]) -> fmt::Result {
    for b in bytes {
        write!(w, "{:02x}", b)?;
    }
    Ok(())
}

fn write_mac(w: &mut impl Write, mac: &[u8; 6]) -> fmt::Result {
    write!(
        w,
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// `@TS1 RX <src> <rssi> <0|1> <hex>` — one received MATCH frame, trailer already stripped.
pub fn write_rx(w: &mut impl Write, src: u8, rssi: i8, mac_ok: bool, frame: &[u8]) -> fmt::Result {
    write!(w, "{}RX {} {} {} ", PREFIX, src, rssi, mac_ok as u8)?;
    write_hex(w, frame)
}

/// `@TS1 HELLO <mac> <node_id> <fw_hash8> <group_epoch>`. `fw` must be one token; pass it through
/// [`fw_hash8`].
pub fn write_hello(w: &mut impl Write, mac: &[u8; 6], node: u8, fw: &str, epoch: u8) -> fmt::Result {
    write!(w, "{}HELLO ", PREFIX)?;
    write_mac(w, mac)?;
    write!(w, " {} {} {}", node, fw, epoch)
}

/// `@TS1 TXOK <id>`.
pub fn write_txok(w: &mut impl Write, id: u32) -> fmt::Result {
    write!(w, "{}TXOK {}", PREFIX, id)
}

/// `@TS1 TXERR <id> <reason>`.
pub fn write_txerr(w: &mut impl Write, id: u32, reason: Reason) -> fmt::Result {
    write!(w, "{}TXERR {} {}", PREFIX, id, reason.as_str())
}

/// `@TS1 ROSTER <id>:<mac>:<rssi>,…`.
///
/// **An empty roster writes nothing and returns `false`.** The arena's parser needs at least one
/// entry — `@TS1 ROSTER` and `@TS1 ROSTER ` both come out as log lines there — so an empty ROSTER
/// is not a roster the arena can read, and the gateway skips that beat instead of sending one.
pub fn write_roster(w: &mut impl Write, entries: &[(u8, [u8; 6], i8)]) -> Result<bool, fmt::Error> {
    if entries.is_empty() {
        return Ok(false);
    }
    write!(w, "{}ROSTER ", PREFIX)?;
    for (i, (id, mac, rssi)) in entries.iter().enumerate() {
        if i > 0 {
            w.write_char(',')?;
        }
        write!(w, "{}:", id)?;
        write_mac(w, mac)?;
        write!(w, ":{}", rssi)?;
    }
    Ok(true)
}

/// The HELLO `fw_hash8` token from the build's `BUILD_HASH`: its first eight characters, cut at
/// the first one that is not alphanumeric (a `-dirty` suffix or a space would split the token);
/// `unknown` if nothing is left.
pub fn fw_hash8(build_hash: &str) -> &str {
    // Only ASCII is kept, so a byte count is a char count and `end` is always a char boundary.
    let end = build_hash
        .bytes()
        .take(8)
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    if end == 0 { "unknown" } else { &build_hash[..end] }
}

/// Display adapter for the encoders, so a line streams straight into `println!` without a
/// buffer: `println!("{}", Show(|f| write_rx(f, …)))`. esp-println holds its lock for the whole
/// `println!` and appends the `\n`, so a log line from elsewhere cannot land inside an `@TS1` line.
/// The host verifiers format through this same type.
pub struct Show<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result>(pub F);

impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Display for Show<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(f)
    }
}

/// Accumulates USB bytes into lines. Holds the first [`IN_LINE_MAX`] bytes of a line and flags
/// the rest as overflow, so a too-long `TX` still yields its id (see [`parse_inbound`]).
pub struct LineReader {
    buf: [u8; IN_LINE_MAX],
    len: usize,
    overflow: bool,
}

impl LineReader {
    pub const fn new() -> Self {
        Self { buf: [0; IN_LINE_MAX], len: 0, overflow: false }
    }

    /// True while a line has started and not yet ended.
    pub fn in_progress(&self) -> bool {
        self.len > 0 || self.overflow
    }

    /// Feed one byte. On `\n` returns `Some((line, truncated))` — the line without its `\n` —
    /// and resets for the next one.
    pub fn push(&mut self, b: u8) -> Option<(&[u8], bool)> {
        if b == b'\n' {
            let (len, overflow) = (self.len, self.overflow);
            self.len = 0;
            self.overflow = false;
            return Some((&self.buf[..len], overflow));
        }
        if self.len < IN_LINE_MAX {
            self.buf[self.len] = b;
            self.len += 1;
        } else {
            self.overflow = true;
        }
        None
    }
}

impl Default for LineReader {
    fn default() -> Self {
        Self::new()
    }
}
