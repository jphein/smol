//! #227/#228 — the PURE minimal DNS A-query codec (encode one question, parse one answer).
//!
//! ## Why this exists (the #228 part-2 decision)
//! smol is IP-only by design (`net/wifi.rs`: "we hardcode an anycast IP so we need no DNS
//! resolver in the smoltcp build") — every LAN target is a baked IP. The ONE path that must
//! resolve a real internet hostname is the #227 weather fetch (`api.open-meteo.com`), whose
//! frontend IPs rotate. Rather than enable smoltcp's `socket-dns` resolver (a new feature +
//! background machinery), this is a **hand-rolled one-shot A query** over the already-enabled
//! `socket-udp` — the same pattern as smol's hand-rolled MQTT 3.1.1 codec and the SNTP
//! one-datagram exchange (`step_sntp`). On ANY failure the caller falls back to the baked
//! `WEATHER_FALLBACK_IP` (git-ignored `board.rs`), so the failure mode degrades to exactly the
//! IP-only behavior smol has everywhere else.
//!
//! ## Pure + host-testable
//! No `esp-hal`/`esp-wifi`/smoltcp deps — `&[u8]` in/out, total (never panics on hostile
//! input). Host-tested in `experiments/dns_verify` (golden query bytes + a synthetic
//! CNAME-chain response + compression pointers + malformed/tampered rejection). The driver
//! (bind ephemeral port → send to resolver:53 → await one reply) lives in `net/wifi.rs`.

/// Max encoded query we ever build: 12-byte header + QNAME (≤255) + QTYPE/QCLASS.
pub const DNS_QUERY_MAX: usize = 12 + 255 + 4;
/// A practical response buffer size: one question + a short CNAME chain + a few A records.
/// Open-Meteo's answer is ~100-200 B; 512 gives ample margin (a truncated response fails
/// closed → fallback IP).
pub const DNS_RESPONSE_MAX: usize = 512;
/// The standard DNS port.
pub const DNS_PORT: u16 = 53;

/// Encode a recursion-desired A-record query for `host` into `out`; returns the total length,
/// or `None` if the host has an over-long label (>63) / total (>253) or `out` is too small.
/// Layout: `txid(2) | flags 0x0100 | QD=1 | AN/NS/AR=0 | qname | QTYPE=A(1) | QCLASS=IN(1)`.
pub fn encode_a_query(txid: u16, host: &str, out: &mut [u8]) -> Option<usize> {
    // 12-byte header.
    if out.len() < 12 || host.is_empty() || host.len() > 253 {
        return None;
    }
    out[0] = (txid >> 8) as u8;
    out[1] = txid as u8;
    out[2] = 0x01; // RD (recursion desired)
    out[3] = 0x00;
    out[4] = 0x00;
    out[5] = 0x01; // QDCOUNT = 1
    out[6..12].fill(0);
    let mut n = 12;
    for label in host.split('.') {
        let l = label.len();
        if l == 0 || l > 63 || n + 1 + l + 5 > out.len() {
            return None; // empty/over-long label, or no room for label + terminator + QTYPE/QCLASS
        }
        out[n] = l as u8;
        out[n + 1..n + 1 + l].copy_from_slice(label.as_bytes());
        n += 1 + l;
    }
    out[n] = 0; // root terminator
    n += 1;
    out[n..n + 4].copy_from_slice(&[0x00, 0x01, 0x00, 0x01]); // QTYPE=A, QCLASS=IN
    Some(n + 4)
}

/// Skip an (possibly compression-pointed) encoded name starting at `pos`; returns the index just
/// past it, or `None` on truncation/malformation. A pointer (`0b11......`) ends the name in 2
/// bytes; plain labels run to the 0 terminator. Bounded (≤ 128 labels) so a hostile packet can't
/// loop us.
fn skip_name(data: &[u8], mut pos: usize) -> Option<usize> {
    for _ in 0..128 {
        let b = *data.get(pos)?;
        if b == 0 {
            return Some(pos + 1);
        }
        if b & 0xC0 == 0xC0 {
            // Compression pointer: 2 bytes, name ends here (the target is elsewhere; we never
            // need to expand names — only to skip past them).
            return (pos + 2 <= data.len()).then_some(pos + 2);
        }
        if b & 0xC0 != 0 {
            return None; // reserved label type
        }
        pos = pos + 1 + b as usize;
        if pos > data.len() {
            return None;
        }
    }
    None
}

/// Parse a response to [`encode_a_query`]: match `txid`, require QR=1 + RCODE=0, skip the echoed
/// question(s), then return the FIRST `A`/`IN` answer's 4-byte address. CNAME records in the
/// answer chain are skipped (the resolver follows them; the A record is in the same message).
/// Total — any truncation/tamper/NXDOMAIN returns `None` (the caller falls back to the baked IP).
pub fn parse_a_response(txid: u16, data: &[u8]) -> Option<[u8; 4]> {
    if data.len() < 12 {
        return None;
    }
    if data[0] != (txid >> 8) as u8 || data[1] != txid as u8 {
        return None; // not our transaction
    }
    if data[2] & 0x80 == 0 {
        return None; // not a response
    }
    if data[3] & 0x0F != 0 {
        return None; // RCODE != NOERROR (NXDOMAIN/SERVFAIL/…)
    }
    let qdcount = u16::from_be_bytes([data[4], data[5]]) as usize;
    let ancount = u16::from_be_bytes([data[6], data[7]]) as usize;
    if ancount == 0 {
        return None;
    }
    // Skip the echoed question section: name + QTYPE(2) + QCLASS(2), qdcount times.
    let mut pos = 12;
    for _ in 0..qdcount.min(4) {
        pos = skip_name(data, pos)? + 4;
        if pos > data.len() {
            return None;
        }
    }
    // Walk the answers: name + TYPE(2) CLASS(2) TTL(4) RDLENGTH(2) + RDATA.
    for _ in 0..ancount.min(16) {
        pos = skip_name(data, pos)?;
        if pos + 10 > data.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let rclass = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
        let rdlen = u16::from_be_bytes([data[pos + 8], data[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > data.len() {
            return None;
        }
        if rtype == 1 && rclass == 1 && rdlen == 4 {
            return Some([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        }
        pos += rdlen; // CNAME / other record — skip its RDATA, keep walking
    }
    None
}
