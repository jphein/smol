//! #237 host verification of the peer-sourced-relay ARBITRATION contract (spec
//! `docs/superpowers/design/peer-sourced-mesh-ota.md` §8/§10):
//!   (1) the ODEL/ODON codec round-trips every field + REJECTS (fail-closed) oversize/short/zero-M;
//!   (2) GOLDEN-BYTE assertions pin the exact §10 wire layout (LE fields, 3-ASCII id, M_len,
//!       M+sig) so a real `ota_mesh.rs` encoder drift diverges from these bytes (and from a leaf's
//!       OTAM sig-over-M) at the next canary/review;
//!   (3) the split-brain `term` guard (§5.3): a holder rejects a `term` older than the highest seen;
//!   (4) the baton source-pick (§8.2): delegate iff the last-confirmed holder runs the exact build
//!       and isn't the target;
//!   (5) the outcome→ODON mapping is FAIL-CLOSED — only a build-matched Confirmed is Ok, so any
//!       other outcome (corrupt-holder sha-reject included) makes the crown fall back to #40.
//!
//! `ota_mesh.rs` cannot be `#[path]`-included on host (its `#[esp_hal::ram]` attrs + HAL/`crate::ota`
//! deps don't exist off-target), so the codec + decision logic below are VENDORED verbatim from the
//! firmware — the relay_compat pattern. Keep them in sync with `ota_mesh.rs` / `net/mode.rs`; the
//! GOLDEN-BYTE + logic assertions pin the CONTRACT that any drift must still satisfy.
//! Run: `cargo run` — panics on failure.

// ======================================================================================
// VENDORED verbatim from rust/clock/src/ota_mesh.rs (arb codec) — keep in sync.
// ======================================================================================
const SIGNED_MSG_MAX: usize = 96; // ota::SIGNED_MSG_MAX (OTAM-shared cap; real M ≤ 86)
const ODEL_PREFIX: &[u8] = b"SMOLv1 ODEL ";
const ODON_PREFIX: &[u8] = b"SMOLv1 ODON ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeResult {
    Ok,
    TargetUnreachable,
    Aborted,
    SelfSlotVerifyFailed,
}
impl ServeResult {
    fn as_u8(self) -> u8 {
        match self {
            ServeResult::Ok => 0,
            ServeResult::TargetUnreachable => 1,
            ServeResult::Aborted => 2,
            ServeResult::SelfSlotVerifyFailed => 3,
        }
    }
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ServeResult::Ok),
            1 => Some(ServeResult::TargetUnreachable),
            2 => Some(ServeResult::Aborted),
            3 => Some(ServeResult::SelfSlotVerifyFailed),
            _ => None,
        }
    }
    // Mirrors ServeResult::from_leaf_outcome (ota_mesh.rs) — FAIL-CLOSED: only Confirmed → Ok.
    fn from_leaf_outcome(o: LeafOtaOutcome) -> Self {
        match o {
            LeafOtaOutcome::Confirmed => ServeResult::Ok,
            LeafOtaOutcome::MacUnknown | LeafOtaOutcome::RelayFailed => {
                ServeResult::TargetUnreachable
            }
            LeafOtaOutcome::FetchFailed => ServeResult::SelfSlotVerifyFailed,
            _ => ServeResult::Aborted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArbFrame<'a> {
    Odel {
        target: u8,
        build: u32,
        session: u16,
        term: u16,
        m: &'a [u8],
        sig: &'a [u8; 64],
    },
    Odon {
        target: u8,
        build: u32,
        session: u16,
        result: ServeResult,
    },
}

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

fn write_id3(id: u8, out: &mut [u8]) {
    out[0] = b'0' + id / 100;
    out[1] = b'0' + (id / 10) % 10;
    out[2] = b'0' + id % 10;
}

fn parse_arb_frame(data: &[u8]) -> Option<ArbFrame<'_>> {
    if let Some(rest) = data.strip_prefix(ODEL_PREFIX) {
        if rest.len() < 3 + 4 + 2 + 2 + 1 {
            return None;
        }
        let target = parse_id3(&rest[0..3])?;
        let build = u32::from_le_bytes([rest[3], rest[4], rest[5], rest[6]]);
        let session = u16::from_le_bytes([rest[7], rest[8]]);
        let term = u16::from_le_bytes([rest[9], rest[10]]);
        let m_len = rest[11] as usize;
        if m_len == 0 || m_len > SIGNED_MSG_MAX {
            return None;
        }
        let m_start = 12;
        let sig_start = m_start + m_len;
        let end = sig_start + 64;
        if rest.len() < end {
            return None;
        }
        let m = &rest[m_start..sig_start];
        let sig: &[u8; 64] = rest[sig_start..end].try_into().ok()?;
        return Some(ArbFrame::Odel { target, build, session, term, m, sig });
    }
    if let Some(rest) = data.strip_prefix(ODON_PREFIX) {
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

fn encode_odel(
    target_id: u8,
    build: u32,
    session: u16,
    term: u16,
    m: &[u8],
    sig: &[u8; 64],
    out: &mut [u8],
) -> Option<usize> {
    if m.is_empty() || m.len() > SIGNED_MSG_MAX {
        return None;
    }
    let total = ODEL_PREFIX.len() + 3 + 4 + 2 + 2 + 1 + m.len() + 64;
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
    out[n] = m.len() as u8;
    n += 1;
    out[n..n + m.len()].copy_from_slice(m);
    n += m.len();
    out[n..n + 64].copy_from_slice(sig);
    n += 64;
    Some(n)
}

fn encode_odon(
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

// ======================================================================================
// VENDORED decision logic from net/mode.rs (baton + split-brain term guard) — keep in sync.
// ======================================================================================

// A minimal mirror of ota_mesh::LeafOtaOutcome (only the variants from_leaf_outcome distinguishes).
#[derive(Debug, Clone, Copy)]
enum LeafOtaOutcome {
    Confirmed,
    MacUnknown,
    RelayFailed,
    FetchFailed,
    RolledBack,
    RelayUnconfirmed,
    Timeout,
    IdMismatch,
    Aborted,
}

/// Mirrors `handle_arb_frame`'s §5.3 replay guard: a holder ACCEPTS an ODEL iff its `term` is not
/// older than the highest term it has seen (a dethroned crown's lower term is refused).
fn arb_term_accepts(incoming_term: u16, highest_seen: u16) -> bool {
    incoming_term >= highest_seen
}

/// Mirrors `baton_holder_for`'s core decision: the crown DELEGATES iff the last-confirmed holder is
/// running the exact `want_build` and is not the target itself. (MAC resolvability is a runtime
/// concern tested on hardware.)
fn baton_delegates(last_confirmed: Option<(u8, u32)>, want_build: u32, target: u8) -> Option<u8> {
    let (hid, hbuild) = last_confirmed?;
    if hbuild != want_build || hid == target {
        return None;
    }
    Some(hid)
}

// ======================================================================================
// Assertions
// ======================================================================================

fn main() {
    // A representative signed manifest (real M = "build|size|sha256hex", ≤ 86 B) + a sig.
    let m: &[u8] = b"343|360448|9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    assert!(m.len() <= SIGNED_MSG_MAX, "test manifest fits the shared cap");
    let sig = [0xABu8; 64];

    // --- (1) ODEL round-trip: every field survives encode → parse ----------------------
    let mut buf = [0u8; 256];
    let n = encode_odel(7, 343, 0x1234, 5, m, &sig, &mut buf).expect("encode ODEL");
    assert_eq!(n, ODEL_PREFIX.len() + 3 + 4 + 2 + 2 + 1 + m.len() + 64, "ODEL length");
    match parse_arb_frame(&buf[..n]) {
        Some(ArbFrame::Odel { target, build, session, term, m: pm, sig: psig }) => {
            assert_eq!(target, 7, "ODEL target");
            assert_eq!(build, 343, "ODEL build");
            assert_eq!(session, 0x1234, "ODEL session");
            assert_eq!(term, 5, "ODEL term");
            assert_eq!(pm, m, "ODEL M round-trips byte-identically (leaf sig-over-M depends on it)");
            assert_eq!(psig, &sig, "ODEL sig round-trips");
        }
        other => panic!("ODEL did not round-trip: {other:?}"),
    }

    // --- (2a) GOLDEN ODEL bytes: pin the exact §10 wire layout -------------------------
    // target=7 build=343(0x0157) session=0x1234 term=5 with a fixed 3-byte M="ABC", sig=0xCD*64.
    let gm: &[u8] = b"ABC";
    let gsig = [0xCDu8; 64];
    let gn = encode_odel(7, 343, 0x1234, 5, gm, &gsig, &mut buf).expect("golden ODEL");
    assert_eq!(&buf[0..12], b"SMOLv1 ODEL ", "golden: tag");
    assert_eq!(&buf[12..15], b"007", "golden: 3-ASCII target id");
    assert_eq!(&buf[15..19], &[0x57, 0x01, 0x00, 0x00], "golden: build u32 LE");
    assert_eq!(&buf[19..21], &[0x34, 0x12], "golden: session u16 LE");
    assert_eq!(&buf[21..23], &[0x05, 0x00], "golden: term u16 LE");
    assert_eq!(buf[23], 3, "golden: M_len");
    assert_eq!(&buf[24..27], b"ABC", "golden: M");
    assert_eq!(&buf[27..27 + 64], &gsig, "golden: 64-byte sig");
    assert_eq!(gn, 27 + 64, "golden: total ODEL = 91 B (< 250 MTU)");

    // --- (2b) GOLDEN ODON bytes --------------------------------------------------------
    let on = encode_odon(7, 343, 0x1234, ServeResult::Ok, &mut buf).expect("golden ODON");
    assert_eq!(&buf[0..12], b"SMOLv1 ODON ", "golden: ODON tag");
    assert_eq!(&buf[12..15], b"007", "golden: ODON target");
    assert_eq!(&buf[15..19], &[0x57, 0x01, 0x00, 0x00], "golden: ODON build LE");
    assert_eq!(&buf[19..21], &[0x34, 0x12], "golden: ODON session LE");
    assert_eq!(buf[21], 0, "golden: ODON result=Ok");
    assert_eq!(on, 22, "golden: total ODON = 22 B");

    // --- (3) ODON round-trip across ALL result codes -----------------------------------
    for (code, res) in [
        (0u8, ServeResult::Ok),
        (1, ServeResult::TargetUnreachable),
        (2, ServeResult::Aborted),
        (3, ServeResult::SelfSlotVerifyFailed),
    ] {
        let n = encode_odon(9, 400, 0xBEEF, res, &mut buf).expect("encode ODON");
        assert_eq!(buf[21], code, "ODON result byte matches {res:?}");
        match parse_arb_frame(&buf[..n]) {
            Some(ArbFrame::Odon { target, build, session, result }) => {
                assert_eq!((target, build, session, result), (9, 400, 0xBEEF, res), "ODON round-trip {res:?}");
            }
            other => panic!("ODON {res:?} did not round-trip: {other:?}"),
        }
    }
    assert_eq!(ServeResult::from_u8(4), None, "unknown ODON result byte → None (fail-closed)");

    // --- (4) FAIL-CLOSED codec: oversize / zero / short → None (never panic/over-read) --
    let big = [b'x'; SIGNED_MSG_MAX + 1];
    assert_eq!(encode_odel(1, 1, 1, 1, &big, &sig, &mut buf), None, "encode REJECTS M > cap (not clamp)");
    assert_eq!(encode_odel(1, 1, 1, 1, b"", &sig, &mut buf), None, "encode REJECTS empty M");
    // A hand-built ODEL claiming M_len=255 (→ 343 B) must be rejected by the parser, not over-read.
    let mut hostile = Vec::new();
    hostile.extend_from_slice(ODEL_PREFIX);
    hostile.extend_from_slice(b"007");
    hostile.extend_from_slice(&343u32.to_le_bytes());
    hostile.extend_from_slice(&1u16.to_le_bytes());
    hostile.extend_from_slice(&1u16.to_le_bytes());
    hostile.push(255); // M_len way over the cap
    hostile.extend_from_slice(&[0u8; 8]);
    assert_eq!(parse_arb_frame(&hostile), None, "parse REJECTS M_len > cap (fail-closed, no over-read)");
    let mut zero_m = Vec::new();
    zero_m.extend_from_slice(ODEL_PREFIX);
    zero_m.extend_from_slice(b"007");
    zero_m.extend_from_slice(&343u32.to_le_bytes());
    zero_m.extend_from_slice(&[0u8; 2 + 2]);
    zero_m.push(0); // M_len = 0
    zero_m.extend_from_slice(&[0u8; 64]);
    assert_eq!(parse_arb_frame(&zero_m), None, "parse REJECTS M_len == 0");
    // Truncated frames at EVERY prefix length must never panic (fail-closed) — a hostile mesh
    // can deliver a frame cut anywhere. Build one fresh full ODEL + one full ODON and walk every cut.
    let mut odel_full = [0u8; 256];
    let odel_len = encode_odel(7, 343, 0x1234, 5, m, &sig, &mut odel_full).expect("full ODEL");
    for cut in 0..=odel_len {
        let _ = parse_arb_frame(&odel_full[..cut]); // must not panic at ANY cut
    }
    let mut odon_full = [0u8; 64];
    let odon_len = encode_odon(7, 343, 0x1234, ServeResult::Ok, &mut odon_full).expect("full ODON");
    for cut in 0..=odon_len {
        let _ = parse_arb_frame(&odon_full[..cut]); // must not panic at ANY cut
    }
    assert_eq!(parse_arb_frame(b"SMOLv1 XXXX not-arb"), None, "non-arb prefix → None");

    // --- (5) split-brain term guard (§5.3) --------------------------------------------
    assert!(arb_term_accepts(5, 5), "equal term accepted (same crown re-issue)");
    assert!(arb_term_accepts(6, 5), "newer term accepted (fresh crown out-terms)");
    assert!(!arb_term_accepts(4, 5), "OLDER term REJECTED — dethroned crown cannot delegate");

    // --- (6) baton source-pick (§8.2) -------------------------------------------------
    assert_eq!(baton_delegates(None, 343, 7), None, "no baton → seed via gateway fetch");
    assert_eq!(baton_delegates(Some((8, 343)), 343, 7), Some(8), "holder id8 on build 343 → delegate");
    assert_eq!(baton_delegates(Some((8, 342)), 343, 7), None, "holder on OLD build → seed (no stale source)");
    assert_eq!(baton_delegates(Some((7, 343)), 343, 7), None, "never delegate a leaf to ITSELF");

    // --- (7) outcome → ODON mapping is FAIL-CLOSED (only Confirmed → Ok) ---------------
    assert_eq!(ServeResult::from_leaf_outcome(LeafOtaOutcome::Confirmed), ServeResult::Ok);
    assert_eq!(ServeResult::from_leaf_outcome(LeafOtaOutcome::FetchFailed), ServeResult::SelfSlotVerifyFailed);
    assert_eq!(ServeResult::from_leaf_outcome(LeafOtaOutcome::RelayFailed), ServeResult::TargetUnreachable);
    assert_eq!(ServeResult::from_leaf_outcome(LeafOtaOutcome::MacUnknown), ServeResult::TargetUnreachable);
    // The corrupt-holder / rolled-back / stranded / brick / id-clash cases ALL map to non-Ok →
    // the crown falls back to the trusted #40 gateway fetch (the safety floor), never advancing
    // the baton onto an unconfirmed node.
    for bad in [
        LeafOtaOutcome::RolledBack,
        LeafOtaOutcome::RelayUnconfirmed,
        LeafOtaOutcome::Timeout,
        LeafOtaOutcome::IdMismatch,
        LeafOtaOutcome::Aborted,
    ] {
        assert_ne!(ServeResult::from_leaf_outcome(bad), ServeResult::Ok, "{bad:?} is non-Ok → fallback");
    }

    println!("arb_verify: ALL CHECKS PASSED (ODEL/ODON round-trip + golden §10 wire + fail-closed reject + \
              split-brain term guard + baton source-pick + fail-closed outcome→ODON mapping)");
}
