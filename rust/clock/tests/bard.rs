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

#[test]
fn tokenizer_roundtrip() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut ids = [0u16; 64];
    let n = t.encode("Once upon a time, there was a little dragon", &mut ids);
    assert!(n > 2 && n < 32, "n={n}");
    let mut out = std::string::String::new();
    let mut prev = 1u16; // BOS
    for &id in &ids[1..n] {
        // ids[0] is BOS itself
        out.push_str(core::str::from_utf8(t.decode(prev, id)).unwrap());
        prev = id;
    }
    assert_eq!(out.trim_start(), "Once upon a time, there was a little dragon");
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
