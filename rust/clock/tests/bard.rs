//! bard (#300) host-side integration tests: the SBRD blob the firmware ships is the fixture.
//!
//! Gated on `hostsim` (the only feature that compiles the pure cores as a LIBRARY). Run with
//! `cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu
//!  --lib --test bard` — the bare `cargo test` form also builds the firmware BIN, which cannot
//! compile without the `hw` crates.
#![cfg(feature = "hostsim")]
use clock::persona::{prompt, protagonist, PROTAGONISTS};
use clock::textflow::wrap_tail;
use clock::tokenizer::Tokenizer;
use clock::nano_llm::{Bufs, Model, ParseErr, Session, StepOut, Story, SEQ_CAP};

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
    // Stop at the KV cache depth as well as the step budget: this helper drives `forward`
    // directly (it is not a `Story`), so it owes the cache the same discipline `Story` keeps.
    for pos in n - 1..steps.min(SEQ_CAP) {
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

#[test]
fn golden_matches_python_reference() {
    let golden = include_str!("../src/bard/testdata/golden_ref.txt");
    // Bind the golden to the blob it was generated from. Without this, a re-exported model with
    // stale testdata fails as a mystifying prose diff instead of "you forgot to regenerate".
    let stamped = golden
        .lines()
        .next()
        .and_then(|l| l.split("crc32=").nth(1))
        .map(str::trim)
        .expect("golden header must carry crc32=<hex>; run tools/bard_golden_baseline.sh");
    let want_crc = clock::nano_llm::crc32(&BLOB[..BLOB.len() - 4]);
    assert_eq!(
        stamped,
        std::format!("{want_crc:08x}"),
        "golden was built for a DIFFERENT blob — rerun tools/bard_golden_baseline.sh"
    );

    let golden_story = golden.lines().skip(1).collect::<std::vec::Vec<_>>().join("\n");
    // The reference prints prompt+continuation as one story; the Rust below produces only the
    // continuation. Strip the prompt so both sides start at the same character.
    let golden_cont = golden_story
        .trim_start()
        .strip_prefix(GOLDEN_PROMPT)
        .expect("golden story must start with the prompt it was generated from");
    let golden_ids: std::vec::Vec<u16> = include_str!("../src/bard/testdata/golden_tokens.txt")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().parse().expect("token id"))
        .collect();
    // Guard the guard: an empty or truncated testdata file must not pass by comparing nothing.
    assert!(
        golden_ids.len() >= 32,
        "golden_tokens.txt looks empty or truncated: {} ids",
        golden_ids.len()
    );
    assert!(
        golden_cont.chars().count() >= 120,
        "golden continuation is too short to judge: {} chars",
        golden_cont.chars().count()
    );

    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let (text, generated) = greedy(&m, &t, &mut bufs, GOLDEN_PROMPT, 200);

    // FULL equality, no tolerance. T6 measured bit-for-bit agreement across the entire story
    // (186 ids / 446 chars) between two independent implementations in different languages, so
    // anything short of exact is a regression and should fail as loudly as possible. The
    // divergence index is reported first because it localises a numeric bug far better than a
    // prose diff — see the reference's docstring for the accumulation-order contract.
    if generated != golden_ids {
        let at = generated
            .iter()
            .zip(&golden_ids)
            .position(|(a, b)| a != b);
        panic!(
            "token stream diverges at index {at:?} (rust {} ids, golden {} ids)\n  rust:   {:?}\n  golden: {:?}",
            generated.len(),
            golden_ids.len(),
            &generated[at.unwrap_or(0)..generated.len().min(at.unwrap_or(0) + 8)],
            &golden_ids[at.unwrap_or(0)..golden_ids.len().min(at.unwrap_or(0) + 8)],
        );
    }
    assert_eq!(text, golden_cont, "text differs despite identical token ids");
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

#[test]
fn persona_prompts_fit_the_vocabulary() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    assert_eq!(PROTAGONISTS.len(), 16);
    // The three live boards get a creature matching their realm persona (spec §8).
    assert!(protagonist(7).contains("dragon"), "id7 Draconic Dominion");
    assert!(protagonist(8).contains("owl"), "id8 Eldritch Nexus");
    assert!(protagonist(9).contains("bird"), "id9 Jade Herald");

    let mut ids = [0u16; 64];
    // EVERY node id, not just the 16 table entries: the `% 16` fallback has to hold for an
    // unprovisioned board too, and a prompt is the one string the model never chose.
    for id in 0..=255u8 {
        let mut buf = [0u8; 64];
        let n = prompt(id, &mut buf);
        let s = core::str::from_utf8(&buf[..n]).expect("prompt is ASCII");
        let k = t.encode(s, &mut ids);
        assert!(k < 32, "prompt for node {id} is {k} tokens: {s:?}");
        // Spec §8's real requirement: every word must exist in the 512-token vocabulary. Ids
        // 3..=258 are the `<0xXX>` byte fallbacks — one of those means the prompt SHREDDED,
        // which is exactly what would happen if we fed the model a realm name.
        assert!(
            !ids[..k].iter().any(|i| (3..=258).contains(i)),
            "node {id} prompt {s:?} hit byte fallback: {:?}",
            &ids[..k]
        );
    }
}

/// Render `wrap_tail`'s spans back into visible lines, so the assertions read like the panel.
fn wrapped(text: &str, cols: usize, rows: usize) -> std::vec::Vec<&str> {
    let mut spans = [(0u16, 0u16); 8];
    let n = wrap_tail(text.as_bytes(), cols, rows, &mut spans[..rows]);
    // The fixtures are ASCII, so the byte spans are also char boundaries and the &str can be
    // sliced directly (the firmware renderer decodes defensively instead — see draw_story).
    spans[..n].iter().map(|&(a, b)| &text[a as usize..b as usize]).collect()
}

#[test]
fn wrap_tail_lays_out_the_panel() {
    // Empty text has nothing to show — not one blank line.
    assert_eq!(wrapped("", 14, 5), std::vec::Vec::<&str>::new());

    // Exactly the column count must NOT spill into a second line.
    assert_eq!(wrapped("abcdefghijklmn", 14, 5), ["abcdefghijklmn"]);
    assert_eq!(wrapped("abcdefghijklmno", 14, 5), ["abcdefghijklmn", "o"]);

    // Greedy break at the last space, and the space itself is consumed (not leading the line).
    assert_eq!(wrapped("the dragon flew away", 14, 5), ["the dragon", "flew away"]);

    // A word longer than the panel is hard-broken rather than lost.
    assert_eq!(
        wrapped("supercalifragilistic", 14, 5),
        ["supercalifragi", "listic"]
    );

    // Model-emitted newlines are hard breaks.
    assert_eq!(wrapped("one\ntwo", 14, 5), ["one", "two"]);

    // 6 lines into 5 rows keeps the LAST five — the panel scrolls.
    let six = "aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll";
    let all = wrapped(six, 7, 8);
    assert_eq!(all.len(), 6, "fixture should wrap to 6 lines: {all:?}");
    let tail = wrapped(six, 7, 5);
    assert_eq!(tail.len(), 5);
    assert_eq!(tail[..], all[1..], "should drop the OLDEST line");
}

#[test]
fn wrap_tail_survives_every_reveal_position_of_a_real_story() {
    // The typewriter re-wraps a growing prefix every frame, so the invariants have to hold at
    // EVERY prefix — not just for the tidy synthetic cases above. This walks a real generated
    // story one revealed character at a time, exactly as the panel does.
    let golden = include_str!("../src/bard/testdata/golden_ref.txt");
    let story = golden.lines().skip(1).collect::<std::vec::Vec<_>>().join("\n");
    let bytes = story.as_bytes();
    const COLS: usize = 14;
    const ROWS: usize = 5;
    let mut spans = [(0u16, 0u16); ROWS];
    for shown in 0..=bytes.len() {
        let n = wrap_tail(&bytes[..shown], COLS, ROWS, &mut spans);
        assert!(n <= ROWS, "shown={shown}: {n} lines exceeds the panel");
        for (i, &(a, b)) in spans[..n].iter().enumerate() {
            assert!(a <= b, "shown={shown} line{i}: inverted span ({a},{b})");
            assert!(
                (b as usize) <= shown,
                "shown={shown} line{i}: span end {b} past the revealed text"
            );
            let w = (b - a) as usize;
            assert!(w <= COLS, "shown={shown} line{i}: {w} chars is wider than the panel");
            // A line must never begin with the space we were supposed to swallow, or the text
            // would visibly drift right as the story scrolls.
            if w > 0 && a > 0 {
                assert_ne!(
                    bytes[a as usize], b' ',
                    "shown={shown} line{i}: line starts with a space"
                );
            }
        }
        // Lines must be in reading order and non-overlapping.
        for w in spans[..n].windows(2) {
            assert!(w[0].1 <= w[1].0, "shown={shown}: spans out of order/overlapping");
        }
    }
    // With the bottom row reserved for `~ fin ~`, the story gets 4 rows and still fits.
    let n = wrap_tail(bytes, COLS, ROWS - 1, &mut spans[..ROWS - 1]);
    assert_eq!(n, ROWS - 1);
}
