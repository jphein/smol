//! #267 — the PURE resume-key logic for cross-burst OTA-fetch resume.
//!
//! ## What this is
//! The OTA self-fetch / relay-fetch downloads the image as sequential HTTP `Range` chunks; a
//! broken chunk resumes from `ImageWriter::written()` — but that cursor only lived for one
//! `run_ota_fetch` call. The #195 retry is **per burst within one boot** (the #100 in-RAM idiom),
//! so each burst re-invoked `ImageWriter::begin()`, which reset the cursor (and the streaming SHA)
//! to 0 → every retry re-`Range`d from byte 0. Under the coexist bulk-RX disease (≈1 chunk/burst)
//! a multi-chunk image never accumulated (#267).
//!
//! The fix keeps a same-boot, cross-burst resume cursor (in `.bss` — **no NVS**, since the retry
//! never crosses a reboot; a reboot legitimately restarts from 0). This module is the **pure,
//! host-testable brain**: given a saved cursor and the current `(build, sha, slot)`, what offset
//! is it safe to resume from? Keying it to the exact staged image + target slot means a *re-stage*
//! or a slot flip forces a fresh fetch from byte 0 (never resume onto a stale/wrong prefix). The
//! HW flash writer + the `.bss` static live in `crate::ota`; this module holds only the decision.
//! Host-tested in `experiments/267_resume_verify` (the `flood`/`etx`/`ledger` pattern).

/// Identifies the staged image + target slot a resume cursor belongs to. A resume is honored only
/// when the current fetch's key equals the saved one — so a newer staged build, a different image
/// with the same build number, or a flipped inactive slot all invalidate the cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResumeKey {
    /// Monotonic build number from the signed manifest.
    pub build: u32,
    /// First 16 bytes of the announced sha256 — ample to disambiguate two images (2⁻¹²⁸).
    pub sha_key: [u8; 16],
    /// Target (inactive) slot: `0` = Slot0, `1` = Slot1.
    pub slot: u8,
}

/// The first 16 bytes of an announced sha256 — the resume key's image discriminator.
pub fn sha_key16(sha: &[u8; 32]) -> [u8; 16] {
    let mut k = [0u8; 16];
    k.copy_from_slice(&sha[..16]);
    k
}

/// The offset a fresh `ImageWriter::begin` may resume from.
///
/// Returns `committed` iff a cursor is saved, its key **exactly** matches `want`, and
/// `0 < committed <= size` (the committed prefix fits the slot). Otherwise `0` — a fresh fetch from
/// byte 0. `committed` is expected to be a flush boundary (sector-aligned) by construction; this
/// pure decision does not depend on that, but the caller must only *save* on-flash (flushed) bytes.
pub fn resume_offset(saved: Option<(ResumeKey, u32)>, want: &ResumeKey, size: u32) -> u32 {
    match saved {
        Some((key, committed)) if key == *want && committed > 0 && committed <= size => committed,
        _ => 0,
    }
}
