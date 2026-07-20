//! Host verification of the PURE #267 cross-burst OTA-resume key logic (`net/ota_resume.rs`),
//! included verbatim (#[path], no drift). Run: `cargo run` — panics on failure. `cargo test` runs
//! the same suite (no false-green).
//!
//! Two things must hold for #267 to be correct:
//!  1. **Keying** — a resume cursor is honored ONLY for the exact staged image + slot it belongs
//!     to (build + sha16 + slot); any mismatch (re-stage, slot flip, different image) → fetch from
//!     byte 0. This is the guard against resuming onto a stale/wrong flash prefix.
//!  2. **Segmentation-invariance** — an image written in one shot vs resumed across N bursts (each
//!     resuming from the last committed offset via the real `resume_offset`) leaves BYTE-IDENTICAL
//!     flash, hence an identical finalize readback-SHA. This is the property the whole fix rests on:
//!     a fetch that keeps dying mid-body ACCUMULATES instead of restarting.

#[path = "../../../rust/clock/src/net/ota_resume.rs"]
mod ota_resume;

use ota_resume::{resume_offset, sha_key16, ResumeKey};
use sha2::{Digest, Sha256};

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

fn key(build: u32, sha: &[u8; 32], slot: u8) -> ResumeKey {
    ResumeKey { build, sha_key: sha_key16(sha), slot }
}

/// A deterministic pseudo-image of `n` bytes (not all-equal, so a mis-offset write would corrupt it).
fn image(n: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 2654435761usize) >> 13) as u8 ^ (i as u8)).collect()
}

/// Model the flash writer's resume behaviour against a mock slot: fetch `img` in segments whose
/// boundaries are the "death points" `deaths`; each new segment resumes from the last COMMITTED
/// (flushed, sector-aligned) offset as the real `resume_offset` would return it. Returns the mock
/// slot bytes — which must equal a one-shot write for any death pattern.
fn fetch_with_deaths(img: &[u8], deaths: &[usize], build: u32, sha: &[u8; 32], slot: u8) -> Vec<u8> {
    const SECTOR: usize = 4096;
    let size = img.len() as u32;
    let want = key(build, sha, slot);
    let mut flash = vec![0xFFu8; img.len()];
    let mut saved: Option<(ResumeKey, u32)> = None; // the .bss cursor across bursts
    let mut cut = 0usize; // absolute death index into `deaths`
    loop {
        // A fresh burst: begin() consults the saved cursor for the resume offset.
        let start = resume_offset(saved, &want, size) as usize;
        if start >= img.len() {
            break;
        }
        // This burst writes until it "dies" at the next death point past `start`, else to the end.
        let die_at = deaths.get(cut).copied().unwrap_or(img.len());
        cut += 1;
        let end = die_at.min(img.len()).max(start);
        // Commit only whole sectors (mid-sector RAM stage is lost on burst death — mirrors flush).
        let committed_end = start + ((end - start) / SECTOR) * SECTOR;
        // ...but the FINAL burst (reaching the end) flushes the padded tail too.
        let (write_end, flush_end) = if end >= img.len() {
            (img.len(), img.len())
        } else {
            (committed_end, committed_end)
        };
        flash[start..write_end].copy_from_slice(&img[start..write_end]);
        if flush_end > 0 {
            saved = Some((want, flush_end as u32)); // persist the on-flash (flushed) cursor
        }
        if flush_end >= img.len() {
            break;
        }
        if flush_end == start && die_at <= start {
            // no forward progress this burst (death before a full sector) — avoid an infinite loop
            // in the model; a real fetch would stall-cap. Nudge past this death point.
            if cut > deaths.len() + 2 {
                break;
            }
        }
    }
    flash
}

fn run() {
    // ---- 1. KEYING --------------------------------------------------------
    let sha = sha256(b"image-A");
    let other = sha256(b"image-B");
    let k = key(42, &sha, 1);
    assert_eq!(resume_offset(Some((k, 49152)), &k, 590_304), 49152, "exact-match key resumes at committed");
    assert_eq!(resume_offset(None, &k, 590_304), 0, "no saved cursor → fresh fetch");
    assert_eq!(resume_offset(Some((key(43, &sha, 1), 49152)), &k, 590_304), 0, "build mismatch → 0 (re-stage)");
    assert_eq!(resume_offset(Some((key(42, &other, 1), 49152)), &k, 590_304), 0, "sha mismatch → 0 (different image, same build)");
    assert_eq!(resume_offset(Some((key(42, &sha, 0), 49152)), &k, 590_304), 0, "slot mismatch → 0 (slot flipped)");
    assert_eq!(resume_offset(Some((k, 0)), &k, 590_304), 0, "committed 0 → 0");
    assert_eq!(resume_offset(Some((k, 700_000)), &k, 590_304), 0, "committed > size → 0 (corrupt cursor)");
    assert_eq!(resume_offset(Some((k, 590_304)), &k, 590_304), 590_304, "committed == size resumes (finalize will size-check)");

    // ---- 2. SEGMENTATION-INVARIANCE (the load-bearing property) -----------
    let img = image(590_304); // ~ a real image size, not sector-multiple (tail exercised)
    let sha_img: [u8; 32] = sha256(&img);
    let build = 344;
    let slot = 1u8;

    // one-shot fetch (no deaths)
    let one_shot = fetch_with_deaths(&img, &[], build, &sha_img, slot);
    assert_eq!(one_shot, img, "one-shot fetch reconstructs the image");
    assert_eq!(sha256(&one_shot), sha_img, "one-shot readback-SHA matches");

    // 3-segment resume: die at 49152 (1 chunk) and 262144, mirroring the coexist ≈1-chunk/burst reality
    let three_seg = fetch_with_deaths(&img, &[49152, 262144], build, &sha_img, slot);
    assert_eq!(three_seg, one_shot, "3-segment resumed fetch is BYTE-IDENTICAL to one-shot");
    assert_eq!(sha256(&three_seg), sha_img, "3-segment readback-SHA == one-shot (segmentation-invariant)");

    // the pathological coexist case: die every single 48 KB chunk (≈13 bursts) — must STILL complete
    let per_chunk_deaths: Vec<usize> = (1..13).map(|i| i * 49152).collect();
    let sawtooth = fetch_with_deaths(&img, &per_chunk_deaths, build, &sha_img, slot);
    assert_eq!(sawtooth, img, "die-every-chunk still accumulates the full image (the #267 fix)");
    assert_eq!(sha256(&sawtooth), sha_img, "die-every-chunk readback-SHA matches");

    // a re-stage mid-fetch (different image, same build) must NOT resume onto the stale prefix
    let stale = resume_offset(Some((key(build, &sha_img, slot), 49152)), &key(build, &other, slot), img.len() as u32);
    assert_eq!(stale, 0, "a re-staged (different-sha) image ignores the stale cursor → fresh fetch");

    println!("resume_verify: all assertions passed");
}

fn main() {
    run();
}

#[cfg(test)]
mod tests {
    /// `cargo test` entry — runs the full suite (identical to `cargo run`); no false-green.
    #[test]
    fn ota_resume_laws() {
        super::run();
    }
}
