//! The in-board SD provisioner's wire format (tapstone 0033: the shrine's voice pack, written to
//! the card in the board's own slot). The S3 firmware is `targets/s3-cyd/sd-provision`, and the
//! host side is tapstone's `tools/sd_provision.py`, which tests against the same vectors as
//! `tests/frames.rs`.
//!
//! One frame, either direction, over the USB-Serial-JTAG link:
//!
//! ```text
//! 'S' 'P' | kind u8 | seq u16 LE | len u16 LE | payload[len] | crc32 LE
//! ```
//!
//! The CRC32 (IEEE, the one zlib and Python's `zlib.crc32` compute) covers kind, seq, len and the
//! payload. The board answers every request with the request's `seq`. A frame whose CRC fails is
//! answered with [`NAK`] (if its header was readable), and the host sends it again; every
//! request is idempotent (a block written twice is the same block), so a lost answer is safe
//! to retry too.
#![no_std]

pub const MAGIC: [u8; 2] = *b"SP";
/// Header: magic, kind, seq, len.
pub const HEADER: usize = 7;
/// Blocks per write request.
pub const MAX_BLOCKS: usize = 8;
/// The largest payload: a write's LBA and eight 512 B blocks.
pub const MAX_PAYLOAD: usize = 4 + MAX_BLOCKS * 512;
pub const FRAME_MAX: usize = HEADER + MAX_PAYLOAD + 4;

// Requests (host -> board).
/// Card facts: answered with OK + card bytes (u64 LE).
pub const INFO: u8 = b'I';
/// Allow writes to THIS card: payload is the card size INFO gave (u64 LE). Writes are refused
/// until the board is armed with the size of the card it holds.
pub const ARM: u8 = b'A';
/// Write blocks: LBA (u32 LE), then 1..=8 blocks of 512 B.
pub const WRITE: u8 = b'W';
/// Zero blocks: LBA (u32 LE), count (u32 LE).
pub const ZERO: u8 = b'Z';
/// Read blocks (no ARM needed: it writes nothing): LBA (u32 LE), count (u8, 1..=8). Answered with
/// OK + count x 512 B. The host's blank check reads the card this way before anything is written.
pub const READ: u8 = b'R';
/// Hash one file through the FAT (read-only mount): payload is its path, e.g.
/// `TAPSTONE/VOICE/SET1/MANIFEST.TSV`. Answered with OK + size (u32 LE) + sha256.
pub const FILE: u8 = b'F';

// Answers (board -> host).
pub const OK: u8 = b'k';
pub const NAK: u8 = b'n';
/// Refused: payload is one status byte below.
pub const ERR: u8 = b'e';

// ERR status bytes.
pub const E_NOT_ARMED: u8 = 1;
pub const E_CARD: u8 = 2;
pub const E_ARGS: u8 = 3;
pub const E_NOT_FOUND: u8 = 4;
pub const E_ARM_MISMATCH: u8 = 5;
pub const E_UNKNOWN: u8 = 6;

/// CRC-32/IEEE (reflected, poly 0xEDB88320), bit by bit: no table, 300 bytes of code.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

fn crc32_update(mut c: u32, data: &[u8]) -> u32 {
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    c
}

/// Encode a frame into `out`; returns its length. `None` if the payload is too long or `out`
/// too short.
pub fn encode(kind: u8, seq: u16, payload: &[u8], out: &mut [u8]) -> Option<usize> {
    let n = HEADER + payload.len() + 4;
    if payload.len() > MAX_PAYLOAD || out.len() < n {
        return None;
    }
    out[..2].copy_from_slice(&MAGIC);
    out[2] = kind;
    out[3..5].copy_from_slice(&seq.to_le_bytes());
    out[5..7].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    out[HEADER..HEADER + payload.len()].copy_from_slice(payload);
    let crc = crc32(&out[2..HEADER + payload.len()]);
    out[HEADER + payload.len()..n].copy_from_slice(&crc.to_le_bytes());
    Some(n)
}

/// What the decoder found.
#[derive(Debug, PartialEq, Eq)]
pub enum Event<'a> {
    Frame { kind: u8, seq: u16, payload: &'a [u8] },
    /// A whole frame whose CRC failed: answer NAK with this seq.
    Corrupt { seq: u16 },
}

/// A streaming decoder: feed it bytes as they arrive, and take frames out. Bytes before a magic,
/// and a header claiming more than [`MAX_PAYLOAD`], are skipped a byte at a time until it finds
/// the next magic.
pub struct Decoder {
    buf: [u8; FRAME_MAX],
    n: usize,
    /// Bytes of `buf` the last event handed out, dropped on the next call.
    consumed: usize,
}

impl Decoder {
    pub const fn new() -> Self {
        Decoder { buf: [0; FRAME_MAX], n: 0, consumed: 0 }
    }

    /// Append bytes; returns how many were taken (fewer when the buffer is full: call
    /// [`Decoder::next_event`] and offer the rest again).
    pub fn push(&mut self, bytes: &[u8]) -> usize {
        self.drop_consumed();
        let k = bytes.len().min(FRAME_MAX - self.n);
        self.buf[self.n..self.n + k].copy_from_slice(&bytes[..k]);
        self.n += k;
        k
    }

    fn drop_consumed(&mut self) {
        if self.consumed > 0 {
            self.buf.copy_within(self.consumed..self.n, 0);
            self.n -= self.consumed;
            self.consumed = 0;
        }
    }

    /// The next frame (or corrupt frame) in what has arrived, if a whole one has.
    pub fn next_event(&mut self) -> Option<Event<'_>> {
        self.drop_consumed();
        loop {
            // Resync to a magic.
            let start = (0..self.n).find(|&i| self.buf[i] == MAGIC[0] && (i + 1 == self.n || self.buf[i + 1] == MAGIC[1]));
            match start {
                None => {
                    self.n = 0;
                    return None;
                }
                Some(s) if s > 0 => {
                    self.buf.copy_within(s..self.n, 0);
                    self.n -= s;
                }
                _ => {}
            }
            if self.n < HEADER {
                return None;
            }
            let len = u16::from_le_bytes([self.buf[5], self.buf[6]]) as usize;
            if len > MAX_PAYLOAD {
                // Not a frame: skip this magic and look again.
                self.buf.copy_within(1..self.n, 0);
                self.n -= 1;
                continue;
            }
            let total = HEADER + len + 4;
            if self.n < total {
                return None;
            }
            let kind = self.buf[2];
            let seq = u16::from_le_bytes([self.buf[3], self.buf[4]]);
            let want = u32::from_le_bytes([
                self.buf[HEADER + len],
                self.buf[HEADER + len + 1],
                self.buf[HEADER + len + 2],
                self.buf[HEADER + len + 3],
            ]);
            self.consumed = total;
            if crc32(&self.buf[2..HEADER + len]) != want {
                return Some(Event::Corrupt { seq });
            }
            return Some(Event::Frame { kind, seq, payload: &self.buf[HEADER..HEADER + len] });
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}
