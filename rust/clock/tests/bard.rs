//! bard (#300) host-side integration tests: the SBRD blob the firmware ships is the fixture.
//!
//! Gated on `hostsim` (the only feature that compiles the pure cores as a LIBRARY). Run with
//! `cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu
//!  --lib --test bard` — the bare `cargo test` form also builds the firmware BIN, which cannot
//! compile without the `hw` crates.
#![cfg(feature = "hostsim")]
use clock::bard_tokenizer::Tokenizer;
use clock::nano_llm::{Bufs, Model, ParseErr, Session};

pub const BLOB: &[u8] = include_bytes!("../model/stories260K-q8.bin");

#[test]
fn parses_real_blob() {
    let m = Model::parse(BLOB).expect("blob parses");
    assert_eq!(m.cfg.dim, 64);
    assert_eq!(m.cfg.hidden, 172);
    assert_eq!(m.cfg.n_layers, 5);
    assert_eq!(m.cfg.vocab, 512);
    assert_eq!(m.cfg.n_heads, 8);
    assert_eq!(m.cfg.n_kv_heads, 4);
    assert_eq!(m.cfg.gs, 64);
    assert!(m.cfg.shared_cls);
}

#[test]
fn rejects_corruption() {
    let mut bad = BLOB.to_vec();
    let mid = bad.len() / 2;
    bad[mid] ^= 0xFF;
    assert!(matches!(Model::parse(&bad), Err(ParseErr::Crc)));
    assert!(matches!(Model::parse(&BLOB[..40]), Err(ParseErr::Truncated)));
}

/// Re-stamp the trailing crc32 so a mutated blob fails on the field under test rather than
/// on integrity. Without this every mutant just returns `Crc` and proves nothing.
fn recrc(mut v: std::vec::Vec<u8>) -> std::vec::Vec<u8> {
    let n = v.len() - 4;
    let c = clock::nano_llm::crc32(&v[..n]);
    v[n..].copy_from_slice(&c.to_le_bytes());
    v
}

#[test]
fn rejects_bad_header_fields() {
    // Wrong magic.
    let mut v = BLOB.to_vec();
    v[0] ^= 0xFF;
    assert!(matches!(Model::parse(&recrc(v)), Err(ParseErr::Magic)));

    // Unknown format version.
    let mut v = BLOB.to_vec();
    v[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert!(matches!(Model::parse(&recrc(v)), Err(ParseErr::Version)));

    // dim past MAX_DIM (65 > 64).
    let mut v = BLOB.to_vec();
    v[8..12].copy_from_slice(&65u32.to_le_bytes());
    assert!(matches!(Model::parse(&recrc(v)), Err(ParseErr::DimsTooBig)));

    // Header intact, payload 4 bytes short: this is the guard on the EXACT q-section length,
    // which nothing else covers — a blob whose header disagrees with its payload must not run.
    let mut v = BLOB[..BLOB.len() - 4].to_vec();
    v.truncate(v.len() - 4);
    v.extend_from_slice(&[0; 4]);
    assert!(matches!(Model::parse(&recrc(v)), Err(ParseErr::Truncated)));
}

/// Decode `ids[1..n]` exactly as a caller would, WITHOUT any trimming: the BOS-space strip
/// inside `decode` is what has to produce a clean string, so tolerating a leading space here
/// would hide a bug in it.
fn decode_all(t: &Tokenizer, ids: &[u16]) -> std::string::String {
    let mut out = std::vec::Vec::new();
    let mut prev = 1u16; // BOS
    for &id in &ids[1..] {
        out.extend_from_slice(t.decode(prev, id));
        prev = id;
    }
    std::string::String::from_utf8(out).expect("decoded bytes are valid utf8")
}

#[test]
fn tokenizer_roundtrip() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut ids = [0u16; 64];
    let n = t.encode("Once upon a time, there was a little dragon", &mut ids);
    // Exact upstream (llama2.c) tokenization: ' Once' ' upon' ' a' ' time' ',' ' there'
    // ' was' ' a' ' little' ' d' 'r' 'a' 'g' 'on' — "dragon" has no whole-word token here.
    assert_eq!(
        &ids[..n],
        &[1, 403, 407, 261, 378, 432, 383, 286, 261, 376, 279, 420, 412, 428, 289]
    );
    assert_eq!(
        decode_all(&t, &ids[..n]),
        "Once upon a time, there was a little dragon"
    );
}

#[test]
fn tokenizer_seeds_whole_codepoints() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut ids = [0u16; 64];
    // U+2019 RIGHT SINGLE QUOTATION MARK has its OWN token (id 468). Seeding per byte would
    // shred it into three `<0xXX>` fallbacks (229, 131, 156) that can never merge back.
    let n = t.encode("Lily’s cat", &mut ids);
    assert_eq!(&ids[..n], &[1, 317, 468, 419, 280, 294]);
    assert!(ids[..n].contains(&468), "the ’ token must be used");
    for bad in [229u16, 131, 156] {
        assert!(
            !ids[..n].contains(&bad),
            "byte-fallback id {bad} leaked: {:?}",
            &ids[..n]
        );
    }
    assert_eq!(decode_all(&t, &ids[..n]), "Lily’s cat");
}

#[test]
fn tokenizer_new_rejects_malformed_table() {
    let m = Model::parse(BLOB).unwrap();
    let v = m.cfg.vocab;
    // Truncated mid-entry: the walk runs past the end. Unreachable through Model::parse (the
    // CRC would fail first), so it has to be exercised directly.
    let cut = m.tok_table.len() - 1;
    assert!(Tokenizer::new(&m.tok_table[..cut], v).is_none());
    // Shorter than the leading max_token_len word.
    assert!(Tokenizer::new(&[], v).is_none());
    assert!(Tokenizer::new(&m.tok_table[..3], v).is_none());
    // Degenerate / oversized vocab.
    assert!(Tokenizer::new(m.tok_table, 0).is_none());
    assert!(Tokenizer::new(m.tok_table, 100_000).is_none());
    // Trailing junk means the table and the vocab count disagree.
    let mut padded = m.tok_table.to_vec();
    padded.push(0);
    assert!(Tokenizer::new(&padded, v).is_none());
}

#[test]
fn forward_is_deterministic_and_finite() {
    let m = Model::parse(BLOB).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let logits1 = {
        let mut s = Session::new();
        s.forward(&m, &mut bufs, 1, 0).to_vec()
    };
    let logits2 = {
        let mut s = Session::new();
        s.forward(&m, &mut bufs, 1, 0).to_vec()
    };
    assert_eq!(logits1, logits2);
    assert!(logits1.iter().all(|v| v.is_finite()));
    let spread = logits1.iter().cloned().fold(f32::MIN, f32::max)
        - logits1.iter().cloned().fold(f32::MAX, f32::min);
    assert!(spread > 1.0, "logits look degenerate: spread={spread}");
}
