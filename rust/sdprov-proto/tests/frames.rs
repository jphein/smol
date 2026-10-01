//! The provisioner's framing. The two hex vectors are also asserted by tapstone's
//! `tools/test_sd_provision.py`, computed there with Python's zlib.crc32: the two sides agree on
//! bytes, not just on a description.
use sdprov_proto::*;

fn enc(kind: u8, seq: u16, p: &[u8]) -> Vec<u8> {
    let mut out = [0u8; FRAME_MAX];
    let n = encode(kind, seq, p, &mut out).unwrap();
    out[..n].to_vec()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn crc32_is_the_ieee_one() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn frames_match_the_shared_vectors() {
    assert_eq!(hex(&enc(INFO, 1, &[])), VEC_INFO);
    assert_eq!(hex(&enc(ARM, 7, &30_953_963_520u64.to_le_bytes())), VEC_ARM);
}

const VEC_INFO: &str = include_str!("vec_info.txt");
const VEC_ARM: &str = include_str!("vec_arm.txt");

#[test]
fn a_frame_round_trips_even_split_into_single_bytes() {
    let mut payload = vec![0u8; 4 + 512 * 8];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = (i * 7) as u8;
    }
    let f = enc(WRITE, 0xBEEF, &payload);
    let mut d = Decoder::new();
    let mut got = None;
    for &b in &f {
        assert_eq!(d.push(&[b]), 1);
        if let Some(Event::Frame { kind, seq, payload: p }) = d.next_event() {
            got = Some((kind, seq, p.to_vec()));
        }
    }
    assert_eq!(got, Some((WRITE, 0xBEEF, payload)));
}

#[test]
fn a_corrupted_byte_is_reported_with_its_seq_and_the_next_frame_still_decodes() {
    let mut bad = enc(WRITE, 42, &[0, 0, 0, 0, 1, 2, 3]);
    bad[9] ^= 0x10; // a payload byte
    let good = enc(INFO, 43, &[]);
    let mut d = Decoder::new();
    d.push(&bad);
    d.push(&good);
    assert_eq!(d.next_event(), Some(Event::Corrupt { seq: 42 }));
    assert_eq!(d.next_event(), Some(Event::Frame { kind: INFO, seq: 43, payload: &[] }));
    assert_eq!(d.next_event(), None);
}

#[test]
fn junk_before_a_frame_and_an_impossible_length_are_skipped() {
    let mut s = b"boot log line\r\nS\x00SPx\xff\xff".to_vec(); // 'SP' with len 0xFFFF: not a frame
    s.extend(enc(FILE, 9, b"TAPSTONE/VOICE/SET1/MANIFEST.TSV"));
    let mut d = Decoder::new();
    d.push(&s);
    assert_eq!(
        d.next_event(),
        Some(Event::Frame { kind: FILE, seq: 9, payload: b"TAPSTONE/VOICE/SET1/MANIFEST.TSV" })
    );
}

#[test]
fn an_oversized_payload_is_refused_at_encode() {
    let mut out = [0u8; FRAME_MAX + 1];
    assert!(encode(WRITE, 0, &[0u8; MAX_PAYLOAD + 1], &mut out).is_none());
}

#[test]
fn a_full_read_answer_fits_one_frame() {
    // OK + 8 blocks is the largest answer the board sends.
    let mut out = [0u8; FRAME_MAX];
    assert!(encode(OK, 1, &[0u8; MAX_BLOCKS * 512], &mut out).is_some());
    assert_eq!(READ, b'R');
}
