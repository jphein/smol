//! The shrine's clip decoder (src/shrine_voice.rs) against an independent one: `voice_clip.wav`
//! was written by tapstone's `tools/voice_pack.py` encoder, and `voice_clip.s16le` is what
//! ffmpeg 6.1's `adpcm_ima_wav` decoder makes of it, trimmed to the header's 3,000 samples (six
//! blocks, the last one padded). The fixture holds a sweep, noise, a quiet tail and a full-scale
//! square burst.
use clock::shrine_voice::{parse_header, Clip, HEADER_BYTES, SAMPLES_PER_BLOCK, BLOCK};

const WAV: &[u8] = include_bytes!("fixtures/voice_clip.wav");
const GOLD: &[u8] = include_bytes!("fixtures/voice_clip.s16le");

fn gold() -> Vec<i16> {
    GOLD.chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

/// Play the whole clip the way the station does: a block when it wants one, stereo out in
/// awkward slice sizes so a fill that straddles a block boundary is exercised.
fn play(wav: &[u8]) -> Vec<(i16, i16)> {
    let h = parse_header(&wav[..HEADER_BYTES], wav.len() as u32).expect("the pack's header");
    let mut clip = Clip::new();
    clip.start(h.samples);
    let mut blocks = wav[HEADER_BYTES..].chunks(BLOCK);
    let mut out = Vec::new();
    let mut buf = [0u8; 1000]; // 250 frames: not a divisor of 505
    while !clip.done() {
        if clip.wants_block() {
            assert!(clip.feed(blocks.next().expect("a block per 505 samples")));
        }
        let n = clip.fill_stereo(&mut buf);
        for f in buf[..n].chunks(4) {
            out.push((i16::from_le_bytes([f[0], f[1]]), i16::from_le_bytes([f[2], f[3]])));
        }
    }
    out
}

#[test]
fn the_decoder_is_bit_exact_against_ffmpeg() {
    let out = play(WAV);
    let g = gold();
    assert_eq!(g.len(), 3000);
    assert_eq!(out.len(), g.len(), "exactly the header's samples: the padding is never played");
    for (i, (&(l, r), &want)) in out.iter().zip(&g).enumerate() {
        assert_eq!(l, want, "sample {i}");
        assert_eq!(r, want, "both slots carry it");
    }
}

#[test]
fn the_header_is_the_packs_or_nothing() {
    let len = WAV.len() as u32;
    assert_eq!(parse_header(WAV, len).map(|h| (h.samples, h.data_bytes)), Some((3000, 6 * 256)));
    assert_eq!(parse_header(WAV, len + 1), None, "padded");
    assert_eq!(parse_header(WAV, len - 256), None, "truncated");
    let mut w = WAV.to_vec();
    w[24..28].copy_from_slice(&16_000u32.to_le_bytes());
    assert_eq!(parse_header(&w, len), None, "another rate");
    let mut w = WAV.to_vec();
    w[20] = 0x01;
    assert_eq!(parse_header(&w, len), None, "PCM, not IMA-ADPCM");
    let mut w = WAV.to_vec();
    w[48..52].copy_from_slice(&(6 * SAMPLES_PER_BLOCK as u32 + 1).to_le_bytes());
    assert_eq!(parse_header(&w, len), None, "more samples than blocks");
}

#[test]
fn a_block_that_does_not_decode_ends_the_clip() {
    let mut clip = Clip::new();
    clip.start(3000);
    let mut bad = [0u8; BLOCK];
    bad[2] = 89; // a step index past the table
    assert!(!clip.feed(&bad));
    assert!(clip.done());
    assert!(!clip.feed(&bad[..100]), "a short block too");
}
