//! bard (#300) host-side integration tests: the SBRD blob the firmware ships is the fixture.
//!
//! Gated on `hostsim` (the only feature that compiles the pure cores as a LIBRARY). Run with
//! `cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu
//!  --lib --test bard` — the bare `cargo test` form also builds the firmware BIN, which cannot
//! compile without the `hw` crates.
#![cfg(feature = "hostsim")]
use clock::bard_tokenizer::Tokenizer;
use clock::nano_llm::{Bufs, Model, ParseErr, Session, StepOut, Story};

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

/// The prompt the golden baseline was generated from (`tools/bard_golden_baseline.sh`).
const GOLDEN_PROMPT: &str = "Once upon a time, there was a little dragon";

/// Greedy-decode `steps` tokens after feeding `prompt`, returning the continuation text and
/// the generated ids. Mirrors `tools/bard_reference.py`'s loop exactly: feed ids[0..n-1], then
/// generate starting from ids[n-1] at position n-1.
fn greedy(
    m: &Model,
    t: &Tokenizer,
    bufs: &mut Bufs,
    prompt: &str,
    steps: usize,
) -> (std::string::String, std::vec::Vec<u16>) {
    let mut ids = [0u16; 64];
    let n = t.encode(prompt, &mut ids);
    let mut s = Session::new();
    for (i, &id) in ids[..n - 1].iter().enumerate() {
        s.forward(m, bufs, id, i);
    }
    let mut token = ids[n - 1];
    let mut text = std::string::String::new();
    let mut generated = std::vec::Vec::new();
    for pos in n - 1..steps {
        let logits = s.forward(m, bufs, token, pos);
        let mut best = 0usize;
        for (i, &v) in logits.iter().enumerate() {
            if v > logits[best] {
                best = i;
            }
        }
        let next = best as u16;
        if next == 1 || next == 2 {
            break;
        }
        generated.push(next);
        text.push_str(&std::string::String::from_utf8_lossy(t.decode(token, next)));
        token = next;
    }
    (text, generated)
}

/// Take the first `n` CHARS (not bytes) — the stories are ASCII today, but a byte slice would
/// panic the day the model emits a multi-byte token.
fn head(s: &str, n: usize) -> std::string::String {
    s.chars().take(n).collect()
}

#[test]
fn golden_prefix_matches_reference_runq() {
    let golden = include_str!("../src/bard/testdata/golden_ref.txt");
    let golden_story = golden.lines().skip(1).collect::<std::vec::Vec<_>>().join("\n");
    // The reference prints prompt+continuation as one story; the Rust below produces only the
    // continuation. Strip the prompt so both sides start at the same character.
    let golden_cont = golden_story
        .trim_start()
        .strip_prefix(GOLDEN_PROMPT)
        .expect("golden story must start with the prompt it was generated from");

    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let (text, generated) = greedy(&m, &t, &mut bufs, GOLDEN_PROMPT, 200);

    // Debugging aid (plan Step 6.4): the first divergent token id localises a mismatch far
    // better than a diff of prose. Transcendentals (libm vs numpy expf/sinf/powf) may drift in
    // the tail, so this compares the first 32 ids only.
    let golden_ids: std::vec::Vec<u16> = include_str!("../src/bard/testdata/golden_tokens.txt")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().parse().expect("token id"))
        .collect();
    let k = 32.min(golden_ids.len()).min(generated.len());
    assert_eq!(
        generated[..k],
        golden_ids[..k],
        "first divergence at token {:?}",
        generated[..k]
            .iter()
            .zip(&golden_ids[..k])
            .position(|(a, b)| a != b)
    );

    // Spec §11 (amended): int8 KV rounding may diverge in the tail; the bar is a long shared
    // prefix. 120 chars is 30+ tokens of exact agreement.
    let bar = 120.min(golden_cont.chars().count()).min(text.chars().count());
    assert!(bar >= 120, "continuation too short to judge: {bar} chars");
    assert_eq!(head(&text, bar), head(golden_cont, bar));
}

#[test]
fn greedy_from_bos_is_the_known_opening() {
    // The cheapest regression guard this numeric stack gets: bare BOS is fully deterministic,
    // so any drift in the parser, tokenizer or kernels shows up in the very first words.
    // Regenerate alongside the golden files if the blob ever changes.
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let mut s = Session::new();
    let mut text = std::string::String::new();
    let (mut token, mut prev) = (1u16, 1u16);
    for pos in 0..24 {
        let logits = s.forward(&m, &mut bufs, token, pos);
        let mut best = 0usize;
        for (i, &v) in logits.iter().enumerate() {
            if v > logits[best] {
                best = i;
            }
        }
        token = best as u16;
        if token == 1 || token == 2 {
            break;
        }
        text.push_str(&std::string::String::from_utf8_lossy(t.decode(prev, token)));
        prev = token;
    }
    assert!(
        text.starts_with("Once upon a time, there was a little girl named Lily"),
        "greedy-from-BOS opening changed: {text:?}"
    );
}

/// Drive a `Story` to completion, returning its text and the number of `step()` calls.
fn run_story(m: &Model, t: &Tokenizer, bufs: &mut Bufs, seed: u32) -> (std::string::String, u32) {
    let mut story = Story::new(t, GOLDEN_PROMPT, seed);
    let mut text = std::string::String::new();
    let mut steps = 0u32;
    loop {
        match story.step(m, t, bufs) {
            StepOut::Text(bytes) => text.push_str(core::str::from_utf8(bytes).unwrap()),
            StepOut::Working => {}
            StepOut::Done { .. } => break,
        }
        steps += 1;
        assert!(steps < 300, "no termination");
    }
    (text, steps)
}

#[test]
fn story_generates_and_terminates() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let mut story = Story::new(&t, "Once upon a time, there was a little dragon", 0xC0FFEE);
    let mut text = std::string::String::new();
    let mut steps = 0;
    loop {
        match story.step(&m, &t, &mut bufs) {
            StepOut::Text(bytes) => text.push_str(core::str::from_utf8(bytes).unwrap()),
            StepOut::Working => {}
            StepOut::Done { .. } => break,
        }
        steps += 1;
        assert!(steps < 300, "no termination");
    }
    assert!(text.len() > 80, "story too short: {text}");
    assert!(text.is_ascii());
}

#[test]
fn different_seeds_different_stories() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let (a, _) = run_story(&m, &t, &mut bufs, 1);
    let (b, _) = run_story(&m, &t, &mut bufs, 2);
    assert_ne!(a, b, "two seeds produced the same story");
    // Reusing one Bufs must not couple the runs: the same seed replays identically even after
    // another story has scribbled all over the KV cache.
    let (a_again, _) = run_story(&m, &t, &mut bufs, 1);
    assert_eq!(a, a_again, "story is not reproducible from its seed");
}

#[test]
fn story_reports_how_it_ended() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let mut story = Story::new(&t, GOLDEN_PROMPT, 1);
    let ending = loop {
        if let StepOut::Done { truncated } = story.step(&m, &t, &mut bufs) {
            break truncated;
        }
    };
    // The step result and the getter must agree — a renderer may consult either.
    assert_eq!(ending, story.truncated());
    assert!(story.is_done());
    // At 260K params EOS is essentially never sampled, so this seed runs to the budget. If this
    // ever flips to a natural stop, the UI's `…` path stops being the common case — worth knowing.
    assert!(ending, "expected a budget cut; EOS-terminated instead");
    // Once done, further steps keep reporting the same ending rather than resuming.
    assert!(matches!(
        story.step(&m, &t, &mut bufs),
        StepOut::Done { truncated: true }
    ));
}
