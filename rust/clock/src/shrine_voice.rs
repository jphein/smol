//! The shrine's voice clips (tapstone 0033): the pack's WAV header, and its IMA-ADPCM blocks
//! decoded to 16-bit samples. Pure, so the host tests run it (`tests/shrine_voice.rs`, bit-exact
//! against ffmpeg's `adpcm_ima_wav` decoder on a committed fixture); the station
//! (`tapstone_station/voice.rs`) does the card and the I²S.
//!
//! The pack's format is tapstone's `tools/voice_pack.py`: IMA-ADPCM, 4-bit mono, 22,050 Hz, in
//! 256-byte blocks of 505 samples. Each block starts from its own header (one sample and the step
//! index), so a clip streams one block at a time with no state carried between blocks.

/// The pack's one rate (`board_s3` L5: the codec's BCLK-derived floor).
pub const RATE: u32 = 22_050;
/// Bytes per ADPCM block.
pub const BLOCK: usize = 256;
/// Samples per block: the header's one, then two per byte.
pub const SAMPLES_PER_BLOCK: usize = (BLOCK - 4) * 2 + 1;
/// RIFF/WAVE, `fmt ` (20 B, IMA), `fact`, `data`: the pack writes exactly this header.
pub const HEADER_BYTES: usize = 12 + (8 + 20) + (8 + 4) + 8;

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73,
    80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494,
    544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499,
    2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487,
    12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// A clip's header, checked: how many real samples it holds (the last block is padded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wav {
    pub samples: u32,
    pub data_bytes: u32,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// The pack's WAV header, checked against the file's length (tapstone `wav_info`). `None` for
/// anything else: another format, rate, block size or channel count, other chunks, or a file
/// truncated or padded. A clip the shrine can't decode is skipped, never guessed at.
pub fn parse_header(h: &[u8], file_len: u32) -> Option<Wav> {
    if h.len() < HEADER_BYTES || &h[0..4] != b"RIFF" || &h[8..12] != b"WAVE" || &h[12..16] != b"fmt " {
        return None;
    }
    let fmt_ok = u32_at(h, 16) == 20
        && u16_at(h, 20) == 0x11
        && u16_at(h, 22) == 1
        && u32_at(h, 24) == RATE
        && u16_at(h, 32) as usize == BLOCK
        && u16_at(h, 34) == 4
        && u16_at(h, 36) == 2
        && u16_at(h, 38) as usize == SAMPLES_PER_BLOCK;
    if !fmt_ok || &h[40..44] != b"fact" || u32_at(h, 44) != 4 || &h[52..56] != b"data" {
        return None;
    }
    let samples = u32_at(h, 48);
    let data_bytes = u32_at(h, 56);
    let whole = u32_at(h, 4) == file_len.wrapping_sub(8)
        && data_bytes == file_len.wrapping_sub(HEADER_BYTES as u32)
        && (data_bytes as usize).is_multiple_of(BLOCK);
    let fits = samples as u64 <= (data_bytes as usize / BLOCK * SAMPLES_PER_BLOCK) as u64;
    (whole && fits).then_some(Wav { samples, data_bytes })
}

/// One nibble: the predictor and step index after it. The delta is `((2m + 1) * step) >> 3`, the
/// exact form ffmpeg's `adpcm_ima_wav` decoder and tapstone's encoder both use.
fn next(nib: u8, pred: i32, idx: i32) -> (i32, i32) {
    let step = STEPS[idx as usize];
    let d = ((2 * (nib & 7) as i32 + 1) * step) >> 3;
    let pred = if nib & 8 != 0 { pred - d } else { pred + d };
    (pred.clamp(-32768, 32767), (idx + INDEX[(nib & 7) as usize]).clamp(0, 88))
}

/// A clip being played, decoding as it goes: it holds one ADPCM block (256 B) and the decoder's
/// state, not the block's 505 decoded samples (1,010 B): the station's RAM is the stack's. Samples
/// come out as stereo 16-bit frames (the codec plays the left slot; both carry the sample),
/// trimmed to the header's sample count.
pub struct Clip {
    block: [u8; BLOCK],
    /// The next sample's index in the block (0 is the header's sample); [`SAMPLES_PER_BLOCK`]
    /// when the block is used up.
    pos: usize,
    pred: i32,
    idx: i32,
    /// Real samples not yet handed out (the last block's padding is never played).
    left: u32,
}

impl Clip {
    pub const fn new() -> Self {
        Self { block: [0; BLOCK], pos: SAMPLES_PER_BLOCK, pred: 0, idx: 0, left: 0 }
    }

    /// Start a clip of `samples` real samples (0 ends the current one).
    pub fn start(&mut self, samples: u32) {
        self.pos = SAMPLES_PER_BLOCK;
        self.left = samples;
    }

    /// Every real sample handed out.
    pub fn done(&self) -> bool {
        self.left == 0
    }

    /// Does the clip want its next block?
    pub fn wants_block(&self) -> bool {
        self.left > 0 && self.pos == SAMPLES_PER_BLOCK
    }

    /// Give it the next block. False if the block doesn't decode (the clip ends there).
    pub fn feed(&mut self, block: &[u8]) -> bool {
        if block.len() < BLOCK || block[2] > 88 {
            self.left = 0;
            return false;
        }
        self.block.copy_from_slice(&block[..BLOCK]);
        self.pos = 0;
        true
    }

    /// The next sample of the block (`pos` < [`SAMPLES_PER_BLOCK`]).
    fn sample(&mut self) -> i16 {
        if self.pos == 0 {
            self.pred = i16::from_le_bytes([self.block[0], self.block[1]]) as i32;
            self.idx = self.block[2] as i32;
        } else {
            let k = self.pos - 1;
            let b = self.block[4 + k / 2];
            let nib = if k % 2 == 0 { b & 0x0F } else { b >> 4 };
            (self.pred, self.idx) = next(nib, self.pred, self.idx);
        }
        self.pos += 1;
        self.pred as i16
    }

    /// Fill `out` (whole stereo frames, 4 bytes each) from the block; returns the bytes written,
    /// which stops short when the block runs out (feed the next one and call again).
    pub fn fill_stereo(&mut self, out: &mut [u8]) -> usize {
        let frames = (out.len() / 4)
            .min(SAMPLES_PER_BLOCK - self.pos)
            .min(self.left as usize);
        for f in 0..frames {
            let s = self.sample().to_le_bytes();
            out[4 * f..4 * f + 4].copy_from_slice(&[s[0], s[1], s[0], s[1]]);
        }
        self.left -= frames as u32;
        frames * 4
    }
}

impl Default for Clip {
    fn default() -> Self {
        Self::new()
    }
}
