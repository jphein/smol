//! bard (#300) host-side integration tests: the SBRD blob the firmware ships is the fixture.
//!
//! Gated on `hostsim` (the only feature that compiles the pure cores as a LIBRARY). Run with
//! `cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu
//!  --lib --test bard` — the bare `cargo test` form also builds the firmware BIN, which cannot
//! compile without the `hw` crates.
#![cfg(feature = "hostsim")]
use clock::nano_llm::{Model, ParseErr};

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
