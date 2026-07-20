//! #40 leaf-mesh-OTA — signed firmware delivery to ESP-NOW-only leaves over the mesh.
//!
//! Compiled ONLY in `espnow` builds (`mod ota_mesh;` is `#[cfg(feature = "espnow")]`
//! in `main.rs`). The DEFAULT build links NONE of this (proven by cfg-gating, not
//! ELF byte-equality — `build.rs` embeds a per-commit git stamp).
//!
//! # What this is
//! A **credential-less leaf** never opens WiFi/MQTT, so it cannot fetch an image.
//! The GATEWAY is its MQTT proxy: it fetches the fleet-staged, ed25519-signed image
//! (the SAME `smol/ota/staged` line the whole fleet uses) to its own inactive slot,
//! then relays it chunk-by-chunk over ESP-NOW to ONE leaf (canary-one-leaf). The leaf
//! verifies the signature BEFORE it flashes, reassembles into its inactive slot, and
//! activates only on a full size+sha match. Design: `scratch/smol-ha-batt/issue-40-
//! leaf-mesh-ota-design.md` (§E LOCKED wire format) + `leaf-mesh-ota-design.md` (§0–6).
//!
//! # The brick-critical invariants (this whole file exists to honor them)
//! 1. **verify-sig-BEFORE-flash** — the mesh is unauthenticated; the ed25519 sig over
//!    the manifest `M = "build|size|sha256hex"` (reused byte-identical from #32) is the
//!    SOLE thing preventing any RF device from flashing a leaf. Verified at OTAM receipt,
//!    before any slot erase/write (HOLE-2 closed by the real baked key).
//! 2. **HOLE-3 — every chunk is bounds-checked against the SIGNED manifest** BEFORE any
//!    write (`seq < total_chunks` ∧ `seq*231 + len ≤ size`), AND every write goes through
//!    a partition-scoped writer that physically errors on an out-of-region address
//!    ([`crate::ota::LeafImageWriter`], HOLE-3b). An OOB `seq` cannot reach the active
//!    slot / otadata → no mid-transfer brick.
//! 3. **signed-freshness floor** — accept iff `sig ok ∧ build > running ∧ build >
//!    fresh_floor ∧ size/sha ok` (closes the signed-intermediate / rolled-back-build
//!    replay; `fresh_floor` in NVS, see [`crate::ota`]).
//! 4. **canary-one-leaf** — the gateway targets exactly ONE leaf id; there is NEVER a
//!    broadcast-to-all image push in this file. Load-bearing safety.
//!
//! # Wire format (LOCKED §E — all multi-byte ints LE, SMOLv1 family, 12-byte tags)
//! Every parser here is panic-free and bounded-copy (it runs on untrusted mesh bytes).

use crate::ota;

// ---------------------------------------------------------------------------
// Constants (LOCKED §E)
// ---------------------------------------------------------------------------

/// Image bytes per `OTAD` chunk: 250 − 12 (tag) − 3 (target) − 2 (session) − 2 (seq).
pub const CHUNK_PAYLOAD: usize = 231;

/// Chunks per windowed-NAK window. 64 → an 8-byte missing-bitmap (`u64`). The leaf
/// tracks a per-window received bitmap; the gateway retransmits only the set bits.
pub const WINDOW_CHUNKS: usize = 64;

/// Bytes in one full window buffer (`WINDOW_CHUNKS * CHUNK_PAYLOAD`). The reassembly
/// buffer holds exactly ONE window (windows complete in order → the on-the-wire image
/// is fed to flash sequentially; no whole-image RAM buffer). 64 * 231 = 14 784.
pub const WINDOW_BYTES: usize = WINDOW_CHUNKS * CHUNK_PAYLOAD;

/// Bytes of the missing-bitmap in an `OTAN` (one bit per chunk in the window).
pub const OTAN_BITMAP_BYTES: usize = WINDOW_CHUNKS / 8;

/// 12-byte frame tags (mirror the SMOLv1 family; full-prefix strip like CFG/STAT).
pub const OTAM_PREFIX: &[u8] = b"SMOLv1 OTAM "; // gateway→leaf: signed manifest / announce
pub const OTAD_PREFIX: &[u8] = b"SMOLv1 OTAD "; // gateway→leaf: one image chunk
pub const OTAN_PREFIX: &[u8] = b"SMOLv1 OTAN "; // leaf→gateway (UNICAST): windowed NAK
/// #40 #3: leaf→(broadcast) OTA RX-diag beacon: `LDBG ` + id[3] + heard[2 LE] + verdict[1] +
/// sent[2 LE]. Broadcast on the HELLO cadence so the gateway's relay RX loop CAPTURES it while
/// the leaf is provably online (rx>0), naming WHY a `relay-failed` had `otan=0` (see `RelayDiag`).
pub const LDBG_PREFIX: &[u8] = b"SMOLv1 LDBG "; // leaf→broadcast: OTA receive-side self-report
// #237 peer-sourced relay — crown↔holder ARBITRATION (unicast; NOT on the receiver hot path).
pub const ODEL_PREFIX: &[u8] = b"SMOLv1 ODEL "; // crown→holder (UNICAST): delegate-to-serve
pub const ODON_PREFIX: &[u8] = b"SMOLv1 ODON "; // holder→crown (UNICAST): serve outcome
/// `LDBG` payload: id[3 ASCII] + otam_heard[2 LE] + verdict[1] + otan_sent[2 LE] + ch[1].
/// #3b `ch` = the leaf's `current_channel()` at beacon time (0 = SCANNING/unlocked, else the
/// locked channel 1/6/11). Decisive for the H0 fork: ch=6 means the leaf was ON ch6 yet still
/// heard no OTAM (RX issue); ch≠6 means it drifted off ch6 during the gateway's fetch window.
pub const LDBG_FRAME_LEN: usize = 12 + 3 + 2 + 1 + 2 + 1;

/// Parse a leaf `LDBG` beacon → `(otam_heard, verdict, otan_sent, leaf_ch)` iff it is well-formed
/// AND addressed-from `want_id` (the 3-ASCII id field). `None` otherwise.
pub fn parse_ldbg(data: &[u8], want_id: u8) -> Option<(u16, u8, u16, u8)> {
    let rest = data.strip_prefix(LDBG_PREFIX)?;
    if rest.len() < 3 + 2 + 1 + 2 + 1 {
        return None;
    }
    if parse_id3(&rest[0..3])? != want_id {
        return None;
    }
    let heard = u16::from_le_bytes([rest[3], rest[4]]);
    let verdict = rest[5];
    let sent = u16::from_le_bytes([rest[6], rest[7]]);
    let leaf_ch = rest[8];
    Some((heard, verdict, sent, leaf_ch))
}

/// Max `OTAM` frame: 12 + 3 + 2 + 1 + `SIGNED_MSG_MAX` (M) + 64 (sig).
pub const OTAM_FRAME_MAX: usize = 12 + 3 + 2 + 1 + ota::SIGNED_MSG_MAX + 64;
/// Max `OTAD` frame: 12 + 3 + 2 + 2 + 231 = 250 (the full ESP-NOW MTU).
pub const OTAD_FRAME_MAX: usize = 12 + 3 + 2 + 2 + CHUNK_PAYLOAD;
/// Max `OTAN` frame: 12 + 3 + 2 + 2 + 8.
pub const OTAN_FRAME_MAX: usize = 12 + 3 + 2 + 2 + OTAN_BITMAP_BYTES;

// ---------------------------------------------------------------------------
// Parsed frames (borrow the RX buffer; used immediately in `service()`)
// ---------------------------------------------------------------------------

/// A decoded #40 OTA frame. `Meta`/`Data` are gateway→leaf; `Nak` is leaf→gateway.
pub enum OtaFrame<'a> {
    /// Signed session announce: the manifest `M` bytes + the 64-byte ed25519 sig.
    /// The leaf verifies `sig` over `m` BEFORE trusting any field parsed from `m`.
    Meta {
        target: u8,
        session: u16,
        m: &'a [u8],
        sig: &'a [u8; 64],
    },
    /// One image chunk: `payload` are the image bytes at offset `seq * CHUNK_PAYLOAD`.
    Data {
        target: u8,
        session: u16,
        seq: u16,
        payload: &'a [u8],
    },
    /// Windowed NAK: bit `i` set in `bitmap` ⇒ chunk `window_base + i` is still missing.
    /// An all-zero bitmap = "window complete, advance" (the only positive ack).
    Nak {
        origin: u8,
        session: u16,
        window_base: u16,
        bitmap: &'a [u8],
    },
}

/// #237 ODON serve outcome (wire `result[1]`). Pure u8 ⇄ enum; host-testable.
#[allow(dead_code)] // #237 INC1: wired by the crown/holder dispatch in a later slice-1 increment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeResult {
    /// All windows served — last-window exhaustion IS the #40 confirm.
    Ok,
    /// The target never answered (no NAKs) — crown re-delegates / falls back.
    TargetUnreachable,
    /// Serve aborted mid-flight (e.g. the holder drifted off ch6).
    Aborted,
    /// The holder's own `slot[..size]` readback-sha did not match the manifest → it must not serve;
    /// the crown falls back to the gateway fetch (spec §5.2 pre-flight catch).
    SelfSlotVerifyFailed,
}

impl ServeResult {
    #[allow(dead_code)] // #237 INC1: wired later
    pub fn as_u8(self) -> u8 {
        match self {
            ServeResult::Ok => 0,
            ServeResult::TargetUnreachable => 1,
            ServeResult::Aborted => 2,
            ServeResult::SelfSlotVerifyFailed => 3,
        }
    }
    #[allow(dead_code)] // #237 INC1: wired later
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ServeResult::Ok),
            1 => Some(ServeResult::TargetUnreachable),
            2 => Some(ServeResult::Aborted),
            3 => Some(ServeResult::SelfSlotVerifyFailed),
            _ => None,
        }
    }
}

/// #237 crown↔holder ARBITRATION frames. Distinct from [`OtaFrame`] (the receiver demux, which
/// stays `#[esp_hal::ram]`/lean and UNTOUCHED per the spec invariant): a holder handles `Odel`, the
/// crown handles `Odon`. Parsed by [`parse_arb_frame`], off the per-chunk hot path.
#[allow(dead_code)] // #237 INC1: wired by the crown/holder dispatch in a later slice-1 increment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbFrame {
    /// crown→holder: serve `build` to leaf `target` under `session`, gated by `term` (a holder
    /// rejects a `term` older than the highest it has seen → a dethroned crown cannot delegate).
    /// ODEL only STARTS a serve of an already-signed image — it cannot cause a flash (the leaf still
    /// verifies the image sig) — so it needs replay/stale protection, not a signature.
    Odel { target: u8, build: u32, session: u16, term: u16 },
    /// holder→crown: outcome of the delegated serve (so the crown advances or falls back).
    Odon { target: u8, build: u32, session: u16, result: ServeResult },
}

/// Parse one #40 OTA frame. Returns `None` on ANY malformed input (never panics,
/// never indexes past the slice) — the caller treats `None` as "not an OTA frame".
// #69 IRAM: on the per-chunk RX hot path (parsed for every OTAD during a leaf mesh-OTA). Placed in
// IRAM so it runs at full speed right after each flash write invalidates the XIP cache — fewer
// stalled drain iterations → fewer dropped chunks → fewer NAK repairs. Small + pure → cheap IRAM.
#[esp_hal::ram]
pub fn parse_ota_frame(data: &[u8]) -> Option<OtaFrame<'_>> {
    if let Some(rest) = data.strip_prefix(OTAM_PREFIX) {
        // target[3] session[2] M_len[1] M[M_len] sig[64]
        if rest.len() < 3 + 2 + 1 {
            return None;
        }
        let target = parse_id3(&rest[0..3])?;
        let session = u16::from_le_bytes([rest[3], rest[4]]);
        let m_len = rest[5] as usize;
        // Bound M by the shared cap so a hostile M_len can't over-read or blow buffers.
        if m_len == 0 || m_len > ota::SIGNED_MSG_MAX {
            return None;
        }
        let m_start = 6;
        let sig_start = m_start + m_len;
        let end = sig_start + 64;
        if rest.len() < end {
            return None;
        }
        let m = &rest[m_start..sig_start];
        let sig: &[u8; 64] = rest[sig_start..end].try_into().ok()?;
        return Some(OtaFrame::Meta { target, session, m, sig });
    }
    if let Some(rest) = data.strip_prefix(OTAD_PREFIX) {
        // target[3] session[2] seq[2] payload[..]
        if rest.len() < 3 + 2 + 2 {
            return None;
        }
        let target = parse_id3(&rest[0..3])?;
        let session = u16::from_le_bytes([rest[3], rest[4]]);
        let seq = u16::from_le_bytes([rest[5], rest[6]]);
        let payload = &rest[7..];
        // A chunk can never carry more than one payload's worth (defensive; the real
        // spatial gate is the signed-bounds check in the session, HOLE-3).
        if payload.len() > CHUNK_PAYLOAD {
            return None;
        }
        return Some(OtaFrame::Data { target, session, seq, payload });
    }
    if let Some(rest) = data.strip_prefix(OTAN_PREFIX) {
        // origin[3] session[2] window_base[2] bitmap[..OTAN_BITMAP_BYTES]
        if rest.len() < 3 + 2 + 2 {
            return None;
        }
        let origin = parse_id3(&rest[0..3])?;
        let session = u16::from_le_bytes([rest[3], rest[4]]);
        let window_base = u16::from_le_bytes([rest[5], rest[6]]);
        let bitmap = &rest[7..];
        if bitmap.len() > OTAN_BITMAP_BYTES {
            return None;
        }
        return Some(OtaFrame::Nak { origin, session, window_base, bitmap });
    }
    None
}

// ---------------------------------------------------------------------------
// Encoders (fixed-width, bounded; return the byte length written into `out`)
// ---------------------------------------------------------------------------

/// Encode an `OTAM`. `None` if `m` exceeds [`ota::SIGNED_MSG_MAX`] or `out` is too small.
pub fn encode_otam(
    target_id: u8,
    session: u16,
    m: &[u8],
    sig: &[u8; 64],
    out: &mut [u8],
) -> Option<usize> {
    if m.is_empty() || m.len() > ota::SIGNED_MSG_MAX {
        return None;
    }
    let total = OTAM_PREFIX.len() + 3 + 2 + 1 + m.len() + 64;
    if out.len() < total {
        return None;
    }
    let mut n = 0;
    out[..OTAM_PREFIX.len()].copy_from_slice(OTAM_PREFIX);
    n += OTAM_PREFIX.len();
    write_id3(target_id, &mut out[n..n + 3]);
    n += 3;
    out[n..n + 2].copy_from_slice(&session.to_le_bytes());
    n += 2;
    out[n] = m.len() as u8;
    n += 1;
    out[n..n + m.len()].copy_from_slice(m);
    n += m.len();
    out[n..n + 64].copy_from_slice(sig);
    n += 64;
    Some(n)
}

/// Encode an `OTAD`. `payload` is truncated to [`CHUNK_PAYLOAD`]; returns the length.
pub fn encode_otad(target_id: u8, session: u16, seq: u16, payload: &[u8], out: &mut [u8]) -> usize {
    let plen = payload.len().min(CHUNK_PAYLOAD);
    let mut n = 0;
    out[..OTAD_PREFIX.len()].copy_from_slice(OTAD_PREFIX);
    n += OTAD_PREFIX.len();
    write_id3(target_id, &mut out[n..n + 3]);
    n += 3;
    out[n..n + 2].copy_from_slice(&session.to_le_bytes());
    n += 2;
    out[n..n + 2].copy_from_slice(&seq.to_le_bytes());
    n += 2;
    out[n..n + plen].copy_from_slice(&payload[..plen]);
    n += plen;
    n
}

/// Encode an `OTAN`. `bitmap` is truncated to [`OTAN_BITMAP_BYTES`]; returns the length.
// #69 IRAM: the NAK-send hot path — built every time a window has gaps / needs a re-ack during the
// flash-write phase. IRAM-resident so it runs without a post-write XIP-refill stall. Small + pure.
#[esp_hal::ram]
pub fn encode_otan(origin_id: u8, session: u16, window_base: u16, bitmap: &[u8], out: &mut [u8]) -> usize {
    let blen = bitmap.len().min(OTAN_BITMAP_BYTES);
    let mut n = 0;
    out[..OTAN_PREFIX.len()].copy_from_slice(OTAN_PREFIX);
    n += OTAN_PREFIX.len();
    write_id3(origin_id, &mut out[n..n + 3]);
    n += 3;
    out[n..n + 2].copy_from_slice(&session.to_le_bytes());
    n += 2;
    out[n..n + 2].copy_from_slice(&window_base.to_le_bytes());
    n += 2;
    out[n..n + blen].copy_from_slice(&bitmap[..blen]);
    n += blen;
    n
}

// ---------------------------------------------------------------------------
// #237 arbitration codec (crown↔holder) — pure, host-testable, NOT IRAM (rare, off the hot path)
// ---------------------------------------------------------------------------

/// Parse one #237 arbitration frame (`ODEL`/`ODON`). `None` on ANY malformed input (never panics,
/// never over-reads) — the caller treats `None` as "not an arbitration frame". Deliberately NOT
/// `#[esp_hal::ram]`: arbitration is rare and off the receiver's per-chunk hot path, so it must not
/// spend `parse_ota_frame`'s IRAM budget.
#[allow(dead_code)] // #237 INC1: wired by the crown/holder dispatch in a later slice-1 increment
pub fn parse_arb_frame(data: &[u8]) -> Option<ArbFrame> {
    if let Some(rest) = data.strip_prefix(ODEL_PREFIX) {
        // target[3] build[u32 LE] session[2] term[u16 LE]
        if rest.len() < 3 + 4 + 2 + 2 {
            return None;
        }
        let target = parse_id3(&rest[0..3])?;
        let build = u32::from_le_bytes([rest[3], rest[4], rest[5], rest[6]]);
        let session = u16::from_le_bytes([rest[7], rest[8]]);
        let term = u16::from_le_bytes([rest[9], rest[10]]);
        return Some(ArbFrame::Odel { target, build, session, term });
    }
    if let Some(rest) = data.strip_prefix(ODON_PREFIX) {
        // target[3] build[u32 LE] session[2] result[1]
        if rest.len() < 3 + 4 + 2 + 1 {
            return None;
        }
        let target = parse_id3(&rest[0..3])?;
        let build = u32::from_le_bytes([rest[3], rest[4], rest[5], rest[6]]);
        let session = u16::from_le_bytes([rest[7], rest[8]]);
        let result = ServeResult::from_u8(rest[9])?;
        return Some(ArbFrame::Odon { target, build, session, result });
    }
    None
}

/// Encode an `ODEL` (crown→holder delegate-to-serve). Fixed-width (no manifest) → always fits one
/// ESP-NOW frame. Returns the byte length written, or `None` if `out` is too small.
#[allow(dead_code)] // #237 INC1: wired later
pub fn encode_odel(target_id: u8, build: u32, session: u16, term: u16, out: &mut [u8]) -> Option<usize> {
    let total = ODEL_PREFIX.len() + 3 + 4 + 2 + 2;
    if out.len() < total {
        return None;
    }
    let mut n = 0;
    out[..ODEL_PREFIX.len()].copy_from_slice(ODEL_PREFIX);
    n += ODEL_PREFIX.len();
    write_id3(target_id, &mut out[n..n + 3]);
    n += 3;
    out[n..n + 4].copy_from_slice(&build.to_le_bytes());
    n += 4;
    out[n..n + 2].copy_from_slice(&session.to_le_bytes());
    n += 2;
    out[n..n + 2].copy_from_slice(&term.to_le_bytes());
    n += 2;
    Some(n)
}

/// Encode an `ODON` (holder→crown serve outcome). Returns the byte length, `None` if `out` too small.
#[allow(dead_code)] // #237 INC1: wired later
pub fn encode_odon(
    target_id: u8,
    build: u32,
    session: u16,
    result: ServeResult,
    out: &mut [u8],
) -> Option<usize> {
    let total = ODON_PREFIX.len() + 3 + 4 + 2 + 1;
    if out.len() < total {
        return None;
    }
    let mut n = 0;
    out[..ODON_PREFIX.len()].copy_from_slice(ODON_PREFIX);
    n += ODON_PREFIX.len();
    write_id3(target_id, &mut out[n..n + 3]);
    n += 3;
    out[n..n + 4].copy_from_slice(&build.to_le_bytes());
    n += 4;
    out[n..n + 2].copy_from_slice(&session.to_le_bytes());
    n += 2;
    out[n] = result.as_u8();
    n += 1;
    Some(n)
}

// ---------------------------------------------------------------------------
// Small helpers (id ⇄ 3-ASCII, chunk-count math) — pure, panic-free
// ---------------------------------------------------------------------------

/// Parse a 3-digit ASCII id (`b"007"` → 7). `None` on short/non-digit/>255.
fn parse_id3(b: &[u8]) -> Option<u8> {
    if b.len() < 3 {
        return None;
    }
    let mut v: u16 = 0;
    for &c in &b[..3] {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as u16;
    }
    (v <= 255).then_some(v as u8)
}

/// Write a u8 id as 3 zero-padded ASCII digits into `out[..3]` (caller guarantees len ≥ 3).
fn write_id3(id: u8, out: &mut [u8]) {
    out[0] = b'0' + id / 100;
    out[1] = b'0' + (id / 10) % 10;
    out[2] = b'0' + id % 10;
}

/// `ceil(size / CHUNK_PAYLOAD)` — total chunks for an image of `size` bytes. Saturating.
pub fn total_chunks(size: u32) -> u32 {
    (size / CHUNK_PAYLOAD as u32).saturating_add((!size.is_multiple_of(CHUNK_PAYLOAD as u32)) as u32)
}

/// "All chunks present" bitmap mask for a window of `len` chunks (`len` ≤ 64). Shared by
/// the leaf (completeness check) and the gateway (retransmit set).
// #69 IRAM: tiny + on the per-chunk completeness check inside `on_data`. Negligible IRAM cost.
#[esp_hal::ram]
pub fn window_full_mask(len: u32) -> u64 {
    if len >= 64 {
        u64::MAX
    } else {
        (1u64 << len) - 1
    }
}

/// Decode a ≤8-byte LE `OTAN` missing-bitmap into a `u64` (bit `i` ⇒ chunk `base+i` missing).
pub fn bitmap_to_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    let n = b.len().min(8);
    a[..n].copy_from_slice(&b[..n]);
    u64::from_le_bytes(a)
}

/// #40 §C#3: parse the running build# from a `SMOLv1 STAT` value `"<screen>:<page>|<build>"`
/// (the last `|`-field). Used by the gateway's Tier-2 confirm — "reappeared AT THE NEW
/// BUILD", never bare presence (so a rolled-back leaf HELLOing at the OLD build doesn't
/// false-confirm). `None` on an old screen-only value / non-numeric build (backward-safe).
pub fn stat_build(value: &[u8]) -> Option<u32> {
    let s = core::str::from_utf8(value).ok()?;
    s.rsplit('|').next()?.parse().ok()
}

// ===========================================================================
// Leaf receive session (the brick-critical path).
//
// One transfer at a time (canary). Driven ENTIRELY by inbound frames + a timer, from
// `RadioManager::service()`. The order of gates is load-bearing and must not be
// reordered: verify-sig → parse M → freshness gate → begin writer → per-chunk signed
// bounds → partition-scoped write → readback verify → activate. ANY failure discards
// with otadata untouched (the good slot boots; a hard brick ⇒ USB recovery, §4).
// ===========================================================================

use esp_bootloader_esp_idf::ota::Slot;

/// Emit a gap-NAK if a window stays incomplete this long since the last NAK (ms).
const LEAF_IDLE_NAK_MS: u64 = 500;
/// Abort the session if NO new chunk arrives for this long (a jam / dead gateway, ms).
const LEAF_PROGRESS_STALL_MS: u64 = 30_000;
/// #3b: grace for the ARMED-but-no-chunk-yet phase. Pre-fetch-arm arms the leaf via the OTAM
/// BEFORE the gateway's WiFi fetch, so the FIRST OTAD arrives only after the fetch (up to
/// `OTA_FETCH_BUDGET`=300s later). Without a longer grace the 30 s stall would abort the armed
/// session mid-fetch (→ leaf drops the hold + scans → the bug we're fixing). Applies ONLY while
/// window 0 has received nothing; once chunks flow, the tight 30 s stall resumes. Still bounded
/// by `LEAF_SESSION_MAX_MS` (600s) → brick-safe (otadata untouched on abort).
const LEAF_FIRST_CHUNK_GRACE_MS: u64 = 330_000;
/// Hard total-session cap (ms) — a runaway transfer aborts → good slot boots (R1: USB).
const LEAF_SESSION_MAX_MS: u64 = 600_000;
/// #157: after the LAST window verifies, broadcast a finalize-ack (an all-zero OTAN at the
/// last window base = "image complete, activating") up to this many times before
/// self-activating. Gives the gateway a positive *delivered-confirmed* signal that survives
/// a lost terminal frame — the crown records confirmed-vs-unconfirmed from hearing it.
const LEAF_FINALIZE_ACK_MAX: u8 = 4;
/// #157: max time (ms) to spend emitting finalize-acks before self-activating ANYWAY. The
/// leaf never *waits* on the gateway — this is a short courtesy window so the ack can be
/// heard; on expiry the leaf activates regardless (the belt-and-braces that un-strands a
/// complete-but-unconfirmed image). ~2 OTAN cadences.
const LEAF_FINALIZE_ACK_WINDOW_MS: u64 = 1_200;

/// Outcome/phase of a GATEWAY → leaf relay attempt — published to `smol/<leaf>/ota/diag`
/// (headless observability: the mesh-only leaf gives no serial, so the gateway reports the
/// terminal phase over MQTT on its next burst). Also drives the install clear/retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafOtaOutcome {
    /// Leaf reappeared at the NEW build (Tier-2 build-matched) — the update stuck.
    Confirmed,
    /// Leaf reappeared at an OLDER build — its self-test failed → app-side rollback (HA re-offers).
    RolledBack,
    /// The gateway could not FETCH/stage the image (WiFi/HTTP/verify) — never relayed.
    FetchFailed,
    /// The ESP-NOW relay loop exhausted its retransmit rounds (leaf not NAKing) — never confirmed.
    RelayFailed,
    /// #157: the feed reached the leaf, but the leaf NEVER confirmed completion — no finalize-ack
    /// was heard AND it settled on the OLD build / never re-STATed. The last-window terminal frame
    /// was lost and the leaf stranded on the old image (the two live 07-15/07-18 occurrences).
    /// Distinct from `RolledBack` (a completed image that self-tested then rolled back) and
    /// `Timeout` (a completed image that went silent). Transient → the install is RE-OFFERED (retry).
    RelayUnconfirmed,
    /// No STAT reappearance within the confirm window — possible brick (USB recovery).
    Timeout,
    /// The armed install's target leaf MAC isn't in the roster yet (never heard its HELLO).
    MacUnknown,
    /// #40 IDENTITY: the physical leaf we targeted (by sticky MAC) reappeared STATing a
    /// DIFFERENT logical id than we relayed to — the image booted with a stolen/wrong id
    /// (baked-default collision / NVS not seeded). Explicit diag instead of a silent
    /// leaf-timeout; TERMINAL (a bad NVS won't heal by re-relaying the same image).
    IdMismatch,
    /// Operator aborted (long-press) mid-session.
    Aborted,
}

impl LeafOtaOutcome {
    /// Short retained-payload phase string for `smol/<leaf>/ota/diag`.
    pub fn as_str(&self) -> &'static str {
        match self {
            LeafOtaOutcome::Confirmed => "confirmed",
            LeafOtaOutcome::RolledBack => "rolled-back",
            LeafOtaOutcome::FetchFailed => "fetch-failed",
            LeafOtaOutcome::RelayFailed => "relay-failed",
            LeafOtaOutcome::RelayUnconfirmed => "delivered-unconfirmed",
            LeafOtaOutcome::Timeout => "leaf-timeout",
            LeafOtaOutcome::MacUnknown => "mac-unknown",
            LeafOtaOutcome::IdMismatch => "id-mismatch",
            LeafOtaOutcome::Aborted => "aborted",
        }
    }

    /// Terminal ⇒ the leaf DEFINITIVELY acted on the image (installed the new build, or
    /// self-tested + rolled back) → clear the install, don't auto-retry. The rest are
    /// transient (no fetch / no relay / MAC not yet learned) → leave the install retained to
    /// retry, bounded by a cap.
    pub fn is_terminal(&self) -> bool {
        // #40: IdMismatch is terminal — the image DID install (a board booted the new
        // build), it just reports a wrong id; re-relaying can't fix a bad NVS, so clear
        // the install + surface the diag rather than burn retries.
        matches!(
            self,
            LeafOtaOutcome::Confirmed | LeafOtaOutcome::RolledBack | LeafOtaOutcome::IdMismatch
        )
    }

    /// #134: did the image feed actually HAND OFF to the leaf (relay bytes started flowing)?
    /// `false` for the GATEWAY-LOCAL pre-relay failures — `FetchFailed` (couldn't stage the image
    /// from the OTA host) and `MacUnknown` (never learned the target MAC) — where the feed never
    /// reached the leaf. Such a failure says nothing about the leaf or the image, so the retained
    /// `smol/<leaf>/ota/install` must SURVIVE it (for the next attempt / the next crown — orders are
    /// crown-portable per #111) instead of counting toward the doomed-image retry cap. `true` for the
    /// terminals and the post-handoff transients (`RelayFailed`/`Timeout` — bytes were flowing — and
    /// `Aborted`), which keep the bounded-retry-then-clear backstop.
    pub fn reached_leaf(&self) -> bool {
        !matches!(self, LeafOtaOutcome::FetchFailed | LeafOtaOutcome::MacUnknown)
    }
}

/// What `service()` must do after handing a frame / a tick to the leaf session.
pub enum LeafAction {
    /// Nothing to send.
    None,
    /// Unicast `out[..len]` (an `OTAN`) back to the gateway's MAC.
    Nak(usize),
    /// The image is fully received AND verified (sig + size + readback sha). Activate
    /// this slot with the manifest build# — [`crate::ota::activate`] reboots into it
    /// (never returns on success); the build# tags the self-test exemption marker.
    Complete(Slot, u32),
    /// The session aborted (bad frame class handled internally) — discard; nothing to send.
    Abort,
}

/// One-window reassembly buffer (off the stack, in `.bss`). Alias-safe: exactly one
/// leaf OTA at a time (canary), single-threaded, single-caller.
static mut OTA_WINDOW_BUF: [u8; WINDOW_BYTES] = [0u8; WINDOW_BYTES];

/// The leaf's mesh-OTA receive state. Small + `!Copy` (owns the writer handle). Lives
/// in `RadioManager`; `active` is false except during a transfer.
pub struct OtaLeafSession {
    active: bool,
    session_id: u16,
    build: u32,
    size: u32,
    sha256: [u8; 32],
    total_chunks: u32,
    /// First seq of the current window (always a multiple of `WINDOW_CHUNKS`).
    window_base: u32,
    /// Received bitmap for the current window: bit `i` ⇒ chunk `window_base + i` is in buf.
    window_recv: u64,
    gateway_mac: [u8; 6],
    session_deadline_ms: u64,
    last_new_chunk_ms: u64,
    last_nak_ms: u64,
    writer: Option<crate::ota::LeafImageWriter>,
    /// #40 #3 LEAF RX-DIAG (instrumentation): lifetime counters surfaced via the leaf's `LDBG`
    /// beacon (captured by a relaying gateway → `smol/<leaf>/ota/relaydiag`) so a headless
    /// `relay-failed` (gateway `rx>0 otan=0`) names WHY the leaf never NAK'd:
    /// `dbg_otam_heard` = OTAMs addressed to us that reached `on_meta`; `dbg_verdict` = the last
    /// `on_meta` outcome (0 never-heard, 1 armed, 2 sig-fail, 3 build≤running, 4 ≤fresh_floor,
    /// 5 size, 6 writer-open-fail, 7 dedup/live); `dbg_otan_sent` = OTANs this leaf emitted.
    dbg_otam_heard: u16,
    dbg_verdict: u8,
    dbg_otan_sent: u16,
    /// #49 observability: lifetime OTA integrity-verify outcomes on the leaf mesh-OTA receive
    /// path, folded into the retained DIAG record (`vok`/`vfl`) — the on-device proof of the #32
    /// signed-refuse that was previously UART0-only. `verify_fail` bumps on an ed25519-sig fail
    /// (the #32 refuse line) OR a readback-SHA mismatch; `verify_ok` on a full readback-verified
    /// image. Policy rejections (build≤running / ≤floor / size) are NOT counted here — they are
    /// gating decisions over an already-valid signature, tracked by `dbg_verdict`/relaydiag.
    verify_ok: u16,
    verify_fail: u16,
    /// #157 finalize-ack phase. `> 0` while the VERIFIED last window is broadcasting
    /// finalize-acks before the leaf self-activates; `0` = not finalizing. Set when the last
    /// window passes readback verify (in `on_data`); cleared when the leaf activates or aborts.
    /// The verify happens BEFORE this (unchanged brick-safety); this only defers the *reboot*
    /// by a short courtesy window so the gateway can hear the completion.
    finalize_since_ms: u64,
    /// The last window's base seq — the OTAN window_base the finalize-ack carries.
    finalize_wb: u32,
    /// The verified target slot to activate (captured before `finalize()` consumed the writer).
    finalize_slot: Option<Slot>,
    /// Finalize-acks emitted so far this session (bounded by `LEAF_FINALIZE_ACK_MAX`).
    finalize_acks_sent: u8,
}

impl OtaLeafSession {
    pub const fn new() -> Self {
        Self {
            active: false,
            session_id: 0,
            build: 0,
            size: 0,
            sha256: [0u8; 32],
            total_chunks: 0,
            window_base: 0,
            window_recv: 0,
            gateway_mac: [0u8; 6],
            session_deadline_ms: 0,
            last_new_chunk_ms: 0,
            last_nak_ms: 0,
            writer: None,
            dbg_otam_heard: 0,
            dbg_verdict: 0,
            dbg_otan_sent: 0,
            verify_ok: 0,
            verify_fail: 0,
            finalize_since_ms: 0,
            finalize_wb: 0,
            finalize_slot: None,
            finalize_acks_sent: 0,
        }
    }

    /// #49: lifetime OTA integrity-verify counters `(ok, fail)` for the DIAG record (`vok`/`vfl`).
    pub fn verify_counts(&self) -> (u16, u16) {
        (self.verify_ok, self.verify_fail)
    }

    /// #40 #3: lifetime leaf RX-diag `(otam_heard, last_on_meta_verdict, otan_sent)` — the leaf
    /// beacons this via `LDBG` so a headless canary can name (a) never-heard-OTAM /
    /// (b) on_meta-rejected / (c) armed-but-never-NAK'd, splitting the gateway's `rx>0 otan=0`.
    pub fn dbg(&self) -> (u16, u8, u16) {
        (self.dbg_otam_heard, self.dbg_verdict, self.dbg_otan_sent)
    }

    /// #3b: is a mesh-OTA transfer live? `leaf_scan_tick` holds ch6 (no hop) while true so the
    /// windowed transfer isn't dropped by a channel hop. False in steady state → scan unaffected.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The gateway MAC this session locked onto (unicast target for NAKs). Zero if idle.
    pub fn gateway_mac(&self) -> [u8; 6] {
        self.gateway_mac
    }

    /// #161: a live snapshot for the on-board OTA screen — `(done, total, build, gateway_mac)`
    /// where `done`/`total` are BLOCKS: fully-committed windows (`window_base`) plus the current
    /// window's received chunks (`window_recv` popcount), clamped to `total_chunks`. `None` when
    /// idle. READ-ONLY — touches no flash/otadata and does not perturb the transfer; the caller
    /// resolves `gateway_mac` → a source id for the "from <noun>" line.
    pub fn rx_progress(&self) -> Option<(u32, u32, u32, [u8; 6])> {
        if !self.active {
            return None;
        }
        let done = self.window_base.saturating_add(self.window_recv.count_ones());
        Some((done.min(self.total_chunks), self.total_chunks, self.build, self.gateway_mac))
    }

    /// Discard the session (drop the writer WITHOUT activating → otadata untouched).
    fn discard(&mut self) {
        self.active = false;
        self.window_recv = 0;
        self.writer = None;
        self.finalize_since_ms = 0; // #157: abandon any pending finalize-ack phase
        self.finalize_slot = None;
        self.finalize_acks_sent = 0;
    }

    /// Handle an `OTAM` (signed announce). VERIFY-SIG-FIRST is the whole point: a frame
    /// that fails the ed25519 check over `m` changes NO state and costs one verify (never
    /// a flash op — the DoS/wear bound, attack row E). Then, and only then, parse M and
    /// apply the freshness gate (`build > running ∧ build > fresh_floor ∧ size ok`).
    /// `src` is the OTAM sender's MAC (the gateway); `my_id` is this leaf's id.
    // Load-bearing signature: each param is a distinct signed-wire / RX-context value
    // on the OTA receive path; bundling into a struct would just relocate the fields
    // without simplifying the call site. Lint-only allow — no restructuring the hot path.
    #[allow(clippy::too_many_arguments)]
    pub fn on_meta(
        &mut self,
        target: u8,
        session: u16,
        m: &[u8],
        sig: &[u8; 64],
        src: [u8; 6],
        my_id: u8,
        now: u64,
    ) -> LeafAction {
        if target != my_id {
            return LeafAction::None; // not for us
        }
        self.dbg_otam_heard = self.dbg_otam_heard.saturating_add(1); // #3: an OTAM reached us
        if self.active && self.session_id == session {
            self.dbg_verdict = 7; // #3: dedup — already armed & live for this session
            return LeafAction::None; // periodic OTAM re-send for the live session — dedupe
        }
        // (1) AUTHENTICITY — the SOLE root of trust on the unauth mesh. Fail-closed.
        if !crate::ota::verify_signature(m, sig) {
            log::warn!("smol #40: OTAM sig FAILED — ignored (no state, no flash)");
            self.dbg_verdict = 2; // #3: signature verify failed
            self.verify_fail = self.verify_fail.saturating_add(1); // #49: the #32 refuse-line proof
            return LeafAction::None;
        }
        // (2) Only now is M trustworthy → parse build/size/sha.
        let Some((build, size, sha256)) = crate::ota::parse_manifest(m) else {
            return LeafAction::None;
        };
        // (3) FRESHNESS + size gate (design §3C). Monotonicity ∧ floor ∧ slot bound.
        if build <= crate::ota::BUILD_NUMBER {
            log::info!("smol #40: OTAM build {} <= running {} — rejected", build, crate::ota::BUILD_NUMBER);
            self.dbg_verdict = 3; // #3: build not fresh (<= running)
            return LeafAction::None;
        }
        let floor = crate::ota::fresh_floor_get();
        if build <= floor {
            log::info!("smol #40: OTAM build {} <= fresh_floor {} — replay rejected", build, floor);
            self.dbg_verdict = 4; // #3: build <= fresh_floor (replay)
            return LeafAction::None;
        }
        if size == 0 || size > crate::ota::MAX_IMAGE_SIZE {
            log::info!("smol #40: OTAM size {} out of range — rejected", size);
            self.dbg_verdict = 5; // #3: size out of range
            return LeafAction::None;
        }
        // (4) Open the inactive-slot writer. If a prior session was live, its writer is
        // dropped here (otadata untouched — it was never activated).
        let Some(writer) = crate::ota::LeafImageWriter::begin() else {
            log::error!("smol #40: cannot open inactive slot — OTAM ignored");
            self.dbg_verdict = 6; // #3: inactive-slot writer open failed
            return LeafAction::None;
        };
        self.dbg_verdict = 1; // #3: ARMED — session accepted (confirm via hear-a-frame)
        self.active = true;
        self.session_id = session;
        self.build = build;
        self.size = size;
        self.sha256 = sha256;
        self.total_chunks = total_chunks(size);
        self.window_base = 0;
        self.window_recv = 0;
        self.gateway_mac = src;
        self.session_deadline_ms = now.saturating_add(LEAF_SESSION_MAX_MS);
        self.last_new_chunk_ms = now;
        self.last_nak_ms = 0;
        self.writer = Some(writer);
        log::info!(
            "smol #40: mesh-OTA session {} armed — build {} ({} B, {} chunks) from the gateway",
            session, build, size, self.total_chunks
        );
        LeafAction::None
    }

    /// Number of valid chunks in the window starting at `wb` (the last window is short).
    fn window_len(&self, wb: u32) -> u32 {
        core::cmp::min(wb + WINDOW_CHUNKS as u32, self.total_chunks) - wb
    }

    /// Handle an `OTAD` image chunk. Enforces the HOLE-3 signed bounds BEFORE any buffer
    /// write, MAC-filters to the locked gateway, routes the chunk into the current window,
    /// and — on a completed window — flushes it to the (partition-scoped) inactive slot
    /// and advances (acking with an all-zero NAK). On the final window it finalizes
    /// (readback verify) and returns `Complete`. `out` receives an OTAN when one is due.
    // Load-bearing signature: each param is a distinct signed-wire / RX-context value
    // on the OTA receive path; bundling into a struct would just relocate the fields
    // without simplifying the call site. Lint-only allow — no restructuring the hot path.
    #[allow(clippy::too_many_arguments)]
    // #69 IRAM: THE per-chunk hot path — bounds-check + window-buffer copy + bitmap track for every
    // OTAD, and the once-per-window flash flush. Placed in IRAM so the per-chunk RX work runs at
    // full speed immediately after each flash write cold-invalidates the XIP cache (the stall the
    // #40 canary saw as 1721 NAK repairs). The `feed_window` flash write it calls owns its own
    // cache-off window (unchanged). Largest of the #69 placements — first to back off if a profile
    // overflows the IRAM region.
    #[esp_hal::ram]
    pub fn on_data(
        &mut self,
        target: u8,
        session: u16,
        seq: u16,
        payload: &[u8],
        src: [u8; 6],
        my_id: u8,
        now: u64,
        out: &mut [u8],
    ) -> LeafAction {
        if target != my_id || !self.active || session != self.session_id {
            return LeafAction::None;
        }
        // R2/R3 defense-in-depth: only accept chunks from the MAC that sent the (signed) OTAM.
        if src != self.gateway_mac {
            return LeafAction::None;
        }
        let seq = seq as u32;
        // ---- HOLE-3: signed-bounds every chunk, BEFORE any write. -----------------
        // Bounds come from the SIGNED manifest (total_chunks/size) → un-tamperable.
        if seq >= self.total_chunks {
            return LeafAction::None;
        }
        let off = seq * CHUNK_PAYLOAD as u32;
        // Exact expected length for this seq (full chunk, or the short final chunk). A
        // wrong-length chunk is refused outright (never buffered) so reassembly can't be
        // corrupted by a truncated/padded in-range chunk.
        let expected = if seq == self.total_chunks - 1 {
            self.size - off
        } else {
            CHUNK_PAYLOAD as u32
        };
        if payload.len() as u32 != expected {
            return LeafAction::None;
        }
        // (redundant with `expected`, but keep the explicit §E spatial invariant visible)
        if off + payload.len() as u32 > self.size {
            return LeafAction::None;
        }

        let wb = self.window_base;
        if seq < wb {
            // A chunk for an ALREADY-COMPLETED window → the gateway didn't get our
            // advance-ack. Re-send an all-zero NAK for that window (idempotent advance).
            let acked_base = (seq / WINDOW_CHUNKS as u32) * WINDOW_CHUNKS as u32;
            let zero = [0u8; OTAN_BITMAP_BYTES];
            let n = encode_otan(my_id, self.session_id, acked_base as u16, &zero, out);
            self.dbg_otan_sent = self.dbg_otan_sent.saturating_add(1); // #3: OTAN emitted
            return LeafAction::Nak(n);
        }
        if seq >= wb + WINDOW_CHUNKS as u32 {
            return LeafAction::None; // future window — gateway advances in order; ignore
        }

        // In the current window: buffer the payload at its window-relative offset.
        let i = (seq - wb) as usize;
        let bit = 1u64 << i;
        let buf = unsafe { &mut *core::ptr::addr_of_mut!(OTA_WINDOW_BUF) };
        let base = i * CHUNK_PAYLOAD;
        buf[base..base + payload.len()].copy_from_slice(payload);
        if self.window_recv & bit == 0 {
            self.window_recv |= bit;
            self.last_new_chunk_ms = now; // genuine progress (the hard-cap deadline is fixed)
        }

        // Window complete?
        let wlen = self.window_len(wb);
        let mask = window_full_mask(wlen);
        if self.window_recv & mask != mask {
            return LeafAction::None; // still gaps — the idle timer will NAK them
        }

        // ---- Window complete → flush to the inactive slot (partition-scoped). -----
        let window_bytes = core::cmp::min(WINDOW_BYTES as u32, self.size - wb * CHUNK_PAYLOAD as u32) as usize;
        let ok = match self.writer.as_mut() {
            Some(w) => w.feed_window(&buf[..window_bytes]),
            None => false,
        };
        if !ok {
            log::error!("smol #40: flash write failed at window {} — session discarded", wb);
            self.discard();
            return LeafAction::Abort;
        }
        // Advance.
        self.window_base = wb + WINDOW_CHUNKS as u32;
        self.window_recv = 0;
        self.last_new_chunk_ms = now;
        self.last_nak_ms = now;

        if self.window_base < self.total_chunks {
            // More windows: ack this one (all-zero NAK) so the gateway sends the next.
            let zero = [0u8; OTAN_BITMAP_BYTES];
            let n = encode_otan(my_id, self.session_id, wb as u16, &zero, out);
            self.dbg_otan_sent = self.dbg_otan_sent.saturating_add(1); // #3: OTAN emitted
            return LeafAction::Nak(n);
        }

        // ---- LAST window done → readback-VERIFY, then enter the #157 finalize-ack phase. ----
        // Verify is UNCHANGED (brick-safety: a bad image never activates). On PASS we do NOT
        // activate immediately — we broadcast a finalize-ack (an all-zero OTAN at the last window
        // base) so the gateway learns we completed + is activating, and self-activate on a short
        // timer in `tick()` REGARDLESS of whether the gateway hears it (#157 belt-and-braces:
        // a lost terminal frame no longer strands a complete image on the old build).
        let (size, sha) = (self.size, self.sha256);
        match self.writer.take() {
            Some(w) => {
                let target_slot = w.target(); // capture BEFORE finalize() consumes the writer
                if w.finalize(size, &sha) {
                    log::info!("smol #40 #157: image VERIFIED — finalize-ack then activate build {}", self.build);
                    self.verify_ok = self.verify_ok.saturating_add(1); // #49: full integrity-verified
                    // Enter the finalize-ack phase; stay `active` so tick() drives ack + activate.
                    self.finalize_since_ms = now.max(1); // never 0 (0 == "not finalizing")
                    self.finalize_wb = wb;
                    self.finalize_slot = Some(target_slot);
                    self.finalize_acks_sent = 1;
                    self.last_nak_ms = now;
                    let zero = [0u8; OTAN_BITMAP_BYTES];
                    let n = encode_otan(my_id, self.session_id, wb as u16, &zero, out);
                    self.dbg_otan_sent = self.dbg_otan_sent.saturating_add(1);
                    LeafAction::Nak(n) // the finalize-ack (gateway reads it as delivered-confirmed)
                } else {
                    log::error!("smol #40: readback verify FAILED — discarded (good slot intact)");
                    self.verify_fail = self.verify_fail.saturating_add(1); // #49: readback SHA mismatch
                    self.discard();
                    LeafAction::Abort
                }
            }
            None => {
                self.discard();
                LeafAction::Abort
            }
        }
    }

    /// Timer nudge (call each `service()` pass). Emits a gap-NAK for the current window if
    /// it has stalled, and aborts the session on a progress stall / hard-cap timeout.
    pub fn tick(&mut self, my_id: u8, now: u64, out: &mut [u8]) -> LeafAction {
        if !self.active {
            return LeafAction::None;
        }
        // #157: finalize-ack phase — the last window is VERIFIED; re-broadcast the finalize-ack a
        // few times so the gateway records delivered-CONFIRMED, then self-activate REGARDLESS (the
        // leaf never waits on the gateway). Runs BEFORE the stall/NAK logic below — the transfer is
        // already complete, so the progress-stall abort must not fire on it.
        if self.finalize_since_ms != 0 {
            let window_open =
                now.saturating_sub(self.finalize_since_ms) < LEAF_FINALIZE_ACK_WINDOW_MS;
            if window_open && self.finalize_acks_sent < LEAF_FINALIZE_ACK_MAX {
                if now.saturating_sub(self.last_nak_ms) < LEAF_IDLE_NAK_MS {
                    return LeafAction::None; // throttle between acks
                }
                self.last_nak_ms = now;
                self.finalize_acks_sent = self.finalize_acks_sent.saturating_add(1);
                let zero = [0u8; OTAN_BITMAP_BYTES];
                let n = encode_otan(my_id, self.session_id, self.finalize_wb as u16, &zero, out);
                self.dbg_otan_sent = self.dbg_otan_sent.saturating_add(1);
                return LeafAction::Nak(n);
            }
            // Courtesy window elapsed or acks maxed → self-activate now (belt-and-braces).
            let slot = self.finalize_slot.take();
            self.finalize_since_ms = 0;
            self.active = false;
            return match slot {
                Some(s) => {
                    log::info!(
                        "smol #40 #157: finalize-acks done ({}) — self-activating build {}",
                        self.finalize_acks_sent, self.build
                    );
                    LeafAction::Complete(s, self.build)
                }
                None => LeafAction::Abort, // unreachable: slot is set whenever finalize_since_ms != 0
            };
        }
        // #3b: while ARMED but still awaiting the very first chunk (window 0, nothing received),
        // allow the fetch-spanning grace — the gateway is fetching off-ch6, not dead. Once any
        // chunk lands, the tight progress stall resumes. The hard session cap still bounds it.
        let stall_ms = if self.window_base == 0 && self.window_recv == 0 {
            LEAF_FIRST_CHUNK_GRACE_MS
        } else {
            LEAF_PROGRESS_STALL_MS
        };
        if now >= self.session_deadline_ms
            || now.saturating_sub(self.last_new_chunk_ms) >= stall_ms
        {
            log::warn!("smol #40: mesh-OTA stalled — session discarded (good slot intact; USB recovery)");
            self.discard();
            return LeafAction::Abort;
        }
        if now.saturating_sub(self.last_nak_ms) < LEAF_IDLE_NAK_MS {
            return LeafAction::None; // throttle
        }
        // Current window incomplete → NAK its missing chunks.
        let wb = self.window_base;
        let wlen = self.window_len(wb);
        let mask = window_full_mask(wlen);
        let missing = mask & !self.window_recv;
        if missing == 0 {
            return LeafAction::None; // complete (an advance-ack is pending elsewhere)
        }
        self.last_nak_ms = now;
        let n = encode_otan(my_id, self.session_id, wb as u16, &missing.to_le_bytes(), out);
        self.dbg_otan_sent = self.dbg_otan_sent.saturating_add(1); // #3: OTAN emitted
        LeafAction::Nak(n)
    }
}
