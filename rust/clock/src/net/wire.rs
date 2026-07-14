//! #13 — the PURE SMOLv1 relay-family wire codec + its ASCII field helpers.
//!
//! Extracted verbatim from `net/mode.rs` (byte-identical — a pure move, no logic change) so the
//! frame formats are HOST-unit-testable off-target, mirroring the `net/flood.rs` pure split. The
//! bidirectional byte-compat guard in `experiments/relay_compat` `#[path]`-includes this module and
//! asserts (a) a NEW-code frame parses under a vendored pre-#13 parser and (b) an old-format
//! RELAYACK parses under this matcher — the permanent mixed-fleet / #124-migration insurance.
//!
//! `mode.rs` re-exports everything here via `use crate::net::wire::*;`, so its call sites are
//! unchanged. No `esp-hal`/`esp-wifi` deps — everything is `&[u8]` in / out.

// ---- codec-shared consts ---------------------------------------------------

/// Relay-bridge tags. RELAY carries a fragment of a leaf's telemetry uplink; RELAYACK is the
/// gateway's per-message received-fragment bitmap. The trailing space on RELAY disambiguates it
/// from RELAYACK at byte 12 (`' '` vs `'A'`), so match order is moot.
pub const RELAY_PREFIX: &[u8] = b"SMOLv1 RELAY "; // + "NNN MMMMM F C " + chunk
pub const RELAYACK_PREFIX: &[u8] = b"SMOLv1 RELAYACK "; // + "MMMMM BBB"
/// #13 multi-hop uplink tags (see the `#13` section in `mode.rs` + `net/flood.rs`). NEW tags — an
/// old firmware `classify()`s them to `None` (harmless). `RELAY2` diverges from `RELAY` at byte 12
/// (`'2'` vs `' '`) and `RELAYACK2` from `RELAYACK` at byte 15, so `strip_prefix` never confuses a
/// base tag with its variant regardless of order.
pub const RELAY2_PREFIX: &[u8] = b"SMOLv1 RELAY2 "; // + "OOO MMMMM H F C " + chunk
pub const RELAYACK2_PREFIX: &[u8] = b"SMOLv1 RELAYACK2 "; // + "TTT MMMMM BBB H"
/// #13 Stage B downlink freshness tags (10-digit `dl_seq` + verbatim `BATT|`/`GRID|` payload).
pub const BATT2_PREFIX: &[u8] = b"SMOLv1 BATT2 ";
pub const GRID2_PREFIX: &[u8] = b"SMOLv1 GRID2 ";
/// #124 generic uplink envelope tag (see the UP2 section below). Diverges from `RELAY`/`RELAY2` at
/// byte 7 (`'U'` vs `'R'`), so `strip_prefix` never confuses it. wip-unwired until Stage 1b.
#[allow(dead_code)]
pub const UP2_PREFIX: &[u8] = b"SMOLv1 UP2 "; // + "OOO MMMMM H " + <inner frame>

/// Max telemetry payload per RELAY fragment (bytes) — encoders truncate the chunk to this.
pub const RELAY_CHUNK: usize = 64;
/// Max BATT/GRID downlink payload (bytes) — `encode_dl` truncates to this (matches the caches).
pub const BATT_PAYLOAD_MAX: usize = 96;

// ---- fixed-width ASCII field helpers --------------------------------------

/// Write a 5-digit zero-padded decimal (value mod 100000) into `out[..5]`.
pub fn write_u5(v: u32, out: &mut [u8]) {
    let v = v % 100_000;
    out[0] = b'0' + ((v / 10_000) % 10) as u8;
    out[1] = b'0' + ((v / 1_000) % 10) as u8;
    out[2] = b'0' + ((v / 100) % 10) as u8;
    out[3] = b'0' + ((v / 10) % 10) as u8;
    out[4] = b'0' + (v % 10) as u8;
}

/// Write a 10-digit zero-padded decimal into `out[..10]`. 10 digits holds every u32, so no clamp
/// is needed — the full value round-trips through [`parse_u10`]. Filled least-significant-digit first.
pub fn write_u10(mut v: u32, out: &mut [u8]) {
    for i in (0..10).rev() {
        out[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

/// Parse a 3-digit ASCII id (`b"007"` -> 7). Rejects non-digits / short input.
pub fn parse_id(rest: &[u8]) -> Option<u8> {
    if rest.len() < 3 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in &rest[..3] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u16;
    }
    (val <= 255).then_some(val as u8)
}

/// Parse exactly 5 ASCII digits into a u32. Rejects short/non-digit input.
pub fn parse_u5(rest: &[u8]) -> Option<u32> {
    if rest.len() < 5 {
        return None;
    }
    let mut val: u32 = 0;
    for &b in &rest[..5] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u32;
    }
    Some(val)
}

/// Parse exactly 10 ASCII digits into a u32. Accumulates in u64 + range-checks on the way out, so a
/// garbled 10-digit field that exceeds `u32::MAX` is rejected rather than silently wrapping.
pub fn parse_u10(rest: &[u8]) -> Option<u32> {
    if rest.len() < 10 {
        return None;
    }
    let mut val: u64 = 0;
    for &b in &rest[..10] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u64;
    }
    u32::try_from(val).ok()
}

// ---- RELAY / RELAYACK (single-hop) ----------------------------------------

/// Encode a RELAY fragment `"SMOLv1 RELAY NNN MMMMM F C " + <chunk>` into `out`; returns total
/// length (27-byte header + chunk). `chunk` is truncated to [`RELAY_CHUNK`].
pub fn encode_relay(src_id: u8, msgid: u16, frag: u8, count: u8, chunk: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..RELAY_PREFIX.len()].copy_from_slice(RELAY_PREFIX);
    n += RELAY_PREFIX.len();
    out[n] = b'0' + (src_id / 100) % 10;
    out[n + 1] = b'0' + (src_id / 10) % 10;
    out[n + 2] = b'0' + src_id % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    write_u5(msgid as u32, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + frag;
    n += 1;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + count;
    n += 1;
    out[n] = b' ';
    n += 1;
    let len = chunk.len().min(RELAY_CHUNK);
    out[n..n + len].copy_from_slice(&chunk[..len]);
    n + len
}

/// Parse a RELAY frame into `(src_id, msgid, frag, count, chunk)`, or `None`. `chunk` borrows `data`.
pub fn parse_relay(data: &[u8]) -> Option<(u8, u16, u8, u8, &[u8])> {
    let rest = data.strip_prefix(RELAY_PREFIX)?;
    if rest.len() < 14 {
        return None;
    }
    let src_id = parse_id(&rest[0..3])?;
    let msgid = u16::try_from(parse_u5(&rest[4..9])?).ok()?;
    if !rest[10].is_ascii_digit() || !rest[12].is_ascii_digit() {
        return None;
    }
    let frag = rest[10] - b'0';
    let count = rest[12] - b'0';
    Some((src_id, msgid, frag, count, &rest[14..]))
}

/// Encode a `"SMOLv1 RELAYACK MMMMM BBB"` frame into `out`; returns length (25). `BBB` = the
/// 3-digit received-fragment bitmap (0..255).
pub fn encode_relayack(msgid: u16, bitmap: u8, out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..RELAYACK_PREFIX.len()].copy_from_slice(RELAYACK_PREFIX);
    n += RELAYACK_PREFIX.len();
    write_u5(msgid as u32, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + (bitmap / 100) % 10;
    out[n + 1] = b'0' + (bitmap / 10) % 10;
    out[n + 2] = b'0' + bitmap % 10;
    n += 3;
    n
}

/// Parse a RELAYACK frame into `(msgid, bitmap)`, or `None`.
pub fn parse_relayack(data: &[u8]) -> Option<(u16, u8)> {
    let rest = data.strip_prefix(RELAYACK_PREFIX)?;
    if rest.len() < 9 {
        return None;
    }
    let msgid = u16::try_from(parse_u5(&rest[0..5])?).ok()?;
    let bitmap = parse_id(&rest[6..9])?;
    Some((msgid, bitmap))
}

// ---- RELAY2 / RELAYACK2 (#13 multi-hop) -----------------------------------

/// Encode a RELAY2 fragment `"SMOLv1 RELAY2 OOO MMMMM H F C " + <chunk>` into `out`; returns total
/// length (30-byte header + chunk). `OOO` = ORIGIN id, `H` = 1-digit hop-limit.
pub fn encode_relay2(origin: u8, msgid: u16, hop: u8, frag: u8, count: u8, chunk: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..RELAY2_PREFIX.len()].copy_from_slice(RELAY2_PREFIX);
    n += RELAY2_PREFIX.len();
    out[n] = b'0' + (origin / 100) % 10;
    out[n + 1] = b'0' + (origin / 10) % 10;
    out[n + 2] = b'0' + origin % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    write_u5(msgid as u32, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + hop;
    n += 1;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + frag;
    n += 1;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + count;
    n += 1;
    out[n] = b' ';
    n += 1;
    let len = chunk.len().min(RELAY_CHUNK);
    out[n..n + len].copy_from_slice(&chunk[..len]);
    n + len
}

/// Parse a RELAY2 frame into `(origin, msgid, hop, frag, count, chunk)`, or `None`.
// Mirrors `parse_relay`'s tuple shape + the one `hop` field; the caller destructures immediately.
#[allow(clippy::type_complexity)]
pub fn parse_relay2(data: &[u8]) -> Option<(u8, u16, u8, u8, u8, &[u8])> {
    let rest = data.strip_prefix(RELAY2_PREFIX)?;
    if rest.len() < 16 {
        return None;
    }
    let origin = parse_id(&rest[0..3])?;
    let msgid = u16::try_from(parse_u5(&rest[4..9])?).ok()?;
    if !rest[10].is_ascii_digit() || !rest[12].is_ascii_digit() || !rest[14].is_ascii_digit() {
        return None;
    }
    let hop = rest[10] - b'0';
    let frag = rest[12] - b'0';
    let count = rest[14] - b'0';
    Some((origin, msgid, hop, frag, count, &rest[16..]))
}

/// Encode a `"SMOLv1 RELAYACK2 TTT MMMMM BBB H"` frame into `out`; returns length (32).
pub fn encode_relayack2(target: u8, msgid: u16, bitmap: u8, hop: u8, out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..RELAYACK2_PREFIX.len()].copy_from_slice(RELAYACK2_PREFIX);
    n += RELAYACK2_PREFIX.len();
    out[n] = b'0' + (target / 100) % 10;
    out[n + 1] = b'0' + (target / 10) % 10;
    out[n + 2] = b'0' + target % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    write_u5(msgid as u32, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + (bitmap / 100) % 10;
    out[n + 1] = b'0' + (bitmap / 10) % 10;
    out[n + 2] = b'0' + bitmap % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + hop;
    n += 1;
    n
}

/// Parse a RELAYACK2 frame into `(target, msgid, bitmap, hop)`, or `None`.
pub fn parse_relayack2(data: &[u8]) -> Option<(u8, u16, u8, u8)> {
    let rest = data.strip_prefix(RELAYACK2_PREFIX)?;
    if rest.len() < 15 {
        return None;
    }
    let target = parse_id(&rest[0..3])?;
    let msgid = u16::try_from(parse_u5(&rest[4..9])?).ok()?;
    let bitmap = parse_id(&rest[10..13])?;
    if !rest[14].is_ascii_digit() {
        return None;
    }
    let hop = rest[14] - b'0';
    Some((target, msgid, bitmap, hop))
}

/// The synthetic MAC a gateway keys a RELAY2 reassembly by — `00:00:00:00:00:<origin>`. A real
/// Espressif STA MAC is never all-zero, so this can't alias a single-hop leaf's real MAC.
/// (#124: reused verbatim for the UP2 gateway-reassembly of an inner RELAY, keyed by origin.)
pub fn synth_origin_mac(origin: u8) -> [u8; 6] {
    [0, 0, 0, 0, 0, origin]
}

// ---- UP2 generic uplink envelope (#124 — REPLACES RELAY2) ------------------
// wip (Stage 1a): the codec + host tests land here host-verified but UNWIRED in the clock crate
// (mode.rs still emits RELAY2). The allow(dead_code)s below DROP in Stage 1b when mode.rs migrates
// RELAY2→UP2 + removes the RELAY2 codec — then the -D warnings gate applies. relay_compat (a
// separate crate) exercises these NOW via #[path], so byte-format regressions are caught immediately.
//
// `SMOLv1 UP2 ` + `"OOO MMMMM H "` + <verbatim inner SMOLv1 frame (RELAY/STAT/DIAG/SCAN)>. A latched
// leaf wraps ANY uplink frame so a stranded leaf's observability (/stat /diag /scan) reaches the
// gateway via the relay — RELAY2 could only carry telemetry. `OOO` = origin id; `MMMMM` = the
// ENVELOPE msgid (per-origin rolling, the FLOOD-DEDUP key — DISTINCT from any inner RELAY msgid,
// which stays the reassembly/ACK key); `H` = hop-limit. Old firmware classify()s UP2 → None
// (harmless) + can't relay anyway → no flag-day (a leaf needing H≥2 was already stranded pre-#13).

/// Max ESP-NOW payload (bytes) — a frame MUST NOT exceed this on the wire.
#[allow(dead_code)]
pub const ESP_NOW_MTU: usize = 250;
/// UP2 envelope overhead: prefix `"SMOLv1 UP2 "` (11) + header `"OOO MMMMM H "` (12) = 23 B.
#[allow(dead_code)]
pub const UP2_OVERHEAD: usize = UP2_PREFIX.len() + 12;
/// Max inner frame a latched leaf may wrap so UP2 fits [`ESP_NOW_MTU`]: 250 − 23 = 227.
/// `encode_up2` CLAMPS the inner to this + never emits > MTU. For a prefix-tolerant inner
/// (DIAG/SCAN are `key=val`), clamping truncates the TAIL fields — the record still parses; the
/// tail that falls off (cfg=/io=/dlseq=/dfwd=, appended last) matters less than the record arriving.
#[allow(dead_code)]
pub const UP2_INNER_MAX: usize = ESP_NOW_MTU - UP2_OVERHEAD;

/// Encode a UP2 envelope: `"SMOLv1 UP2 " + "OOO MMMMM H " + <inner>`; returns total length (≤ MTU).
/// `env_msgid` is the envelope's own per-origin rolling counter (flood dedup). The inner is CLAMPED
/// to [`UP2_INNER_MAX`] so the frame never exceeds [`ESP_NOW_MTU`] (constraint: never emit > MTU).
#[allow(dead_code)]
pub fn encode_up2(origin: u8, env_msgid: u16, hop: u8, inner: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..UP2_PREFIX.len()].copy_from_slice(UP2_PREFIX);
    n += UP2_PREFIX.len();
    out[n] = b'0' + (origin / 100) % 10;
    out[n + 1] = b'0' + (origin / 10) % 10;
    out[n + 2] = b'0' + origin % 10;
    n += 3;
    out[n] = b' ';
    n += 1;
    write_u5(env_msgid as u32, &mut out[n..]);
    n += 5;
    out[n] = b' ';
    n += 1;
    out[n] = b'0' + hop;
    n += 1;
    out[n] = b' ';
    n += 1;
    let len = inner.len().min(UP2_INNER_MAX);
    out[n..n + len].copy_from_slice(&inner[..len]);
    n + len
}

/// Parse a UP2 envelope into `(origin, env_msgid, hop, inner)`, or `None`. `inner` borrows `data` —
/// the verbatim inner SMOLv1 frame, which the caller re-runs through `parse_frame` + dispatches.
#[allow(dead_code)]
pub fn parse_up2(data: &[u8]) -> Option<(u8, u16, u8, &[u8])> {
    let rest = data.strip_prefix(UP2_PREFIX)?;
    // "OOO MMMMM H " = 12 header bytes (origin 3, sp, env_msgid 5, sp, hop 1, sp), then the inner frame.
    if rest.len() < 12 {
        return None;
    }
    let origin = parse_id(&rest[0..3])?;
    let env_msgid = u16::try_from(parse_u5(&rest[4..9])?).ok()?;
    if !rest[10].is_ascii_digit() {
        return None;
    }
    let hop = rest[10] - b'0';
    Some((origin, env_msgid, hop, &rest[12..]))
}

// ---- BATT2 / GRID2 (#13 Stage B downlink freshness) -----------------------

/// Encode a downlink freshness frame `<prefix> + "SSSSSSSSSS " + <payload>` into `out`; returns
/// length. `prefix` is [`BATT2_PREFIX`]/[`GRID2_PREFIX`]; `seq` is the 10-digit freshness.
pub fn encode_dl(prefix: &[u8], seq: u32, payload: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..prefix.len()].copy_from_slice(prefix);
    n += prefix.len();
    write_u10(seq, &mut out[n..]);
    n += 10;
    out[n] = b' ';
    n += 1;
    let len = payload.len().min(BATT_PAYLOAD_MAX);
    out[n..n + len].copy_from_slice(&payload[..len]);
    n + len
}

/// Parse a downlink freshness frame with `prefix` into `(seq, payload)`, or `None`. `payload` borrows `data`.
pub fn parse_dl<'a>(prefix: &[u8], data: &'a [u8]) -> Option<(u32, &'a [u8])> {
    let rest = data.strip_prefix(prefix)?;
    if rest.len() < 11 {
        return None;
    }
    let seq = parse_u10(&rest[0..10])?;
    Some((seq, &rest[11..]))
}
