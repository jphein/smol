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
use clock::nano_llm::{
    cache_slot, live_slots, Bufs, Model, ParseErr, Session, StepOut, Story, KEEP, POS_MAX, SEQ_CAP,
    WINDOW,
};

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

// ── #302 KV ring buffer: the arithmetic that lets a story continue ──────────────────────
//
// This is where the bugs would be, and it is pure — so it is tested exhaustively rather than
// sampled. Everything below holds for ANY (SEQ_CAP, KEEP) pair, so it keeps holding if the
// Embassy re-platform (#198) grows the window.

#[test]
fn cache_slot_is_the_identity_until_the_ring_fills() {
    // The golden token stream depends on this: a first chapter must write slots 0, 1, 2 … in
    // order, exactly as the pre-#302 flat cache did.
    for pos in 0..SEQ_CAP {
        assert_eq!(cache_slot(pos), pos, "pos {pos} should still be its own slot");
        assert_eq!(live_slots(pos), pos + 1, "attention should widen by one per token");
    }
    // And then the live set stops growing — every slot holds a token of the sliding window.
    for pos in SEQ_CAP..SEQ_CAP * 4 {
        assert_eq!(live_slots(pos), SEQ_CAP);
    }
}

#[test]
fn cache_slot_keeps_the_last_window_of_positions_addressable() {
    // THE load-bearing invariant: at any position, the SEQ_CAP most recent positions must live in
    // SEQ_CAP DISTINCT slots. If two of them ever collided, attention would read one key twice
    // and silently lose the other — a corruption that looks like bad prose, not like a crash.
    for pos in 0..SEQ_CAP * 5 {
        let live = live_slots(pos);
        let mut seen = [false; SEQ_CAP];
        for back in 0..live {
            let s = cache_slot(pos - back);
            assert!(s < SEQ_CAP, "slot {s} out of the cache at pos {pos}");
            assert!(!seen[s], "pos {} and {} collide in slot {s}", pos - back, pos);
            seen[s] = true;
        }
        // Every slot accounted for once the ring is full: no dead slot, no wasted RAM.
        assert_eq!(seen.iter().filter(|&&v| v).count(), live);
    }
}

#[test]
fn cache_slot_evicts_the_oldest_and_only_the_oldest() {
    // Writing `pos` must land on the slot holding `pos - WINDOW` (the oldest evictable token) and
    // on nothing else — that is what makes the window slide by exactly one.
    for pos in SEQ_CAP..SEQ_CAP * 4 {
        assert_eq!(
            cache_slot(pos),
            cache_slot(pos - WINDOW),
            "pos {pos} should reuse the slot of pos {}",
            pos - WINDOW
        );
        // The pinned prefix (KEEP) is never a write target once the ring is turning.
        assert!(cache_slot(pos) >= KEEP, "pos {pos} overwrote a pinned sink slot");
    }
    // Nothing addressable is outside the cache, all the way to the cursor's limit.
    for pos in [POS_MAX - 1, POS_MAX] {
        assert!(cache_slot(pos) < SEQ_CAP);
    }
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

/// Compare a generated id stream against the golden one, panicking with a DIAGNOSIS rather than
/// a bare inequality. Extracted so `golden_failure_names_the_divergent_token` exercises the real
/// code path instead of a copy of it.
fn assert_ids_match(generated: &[u16], golden: &[u16]) {
    if generated == golden {
        return;
    }
    match generated.iter().zip(golden).position(|(a, b)| a != b) {
        // Real numeric divergence: show a window of both streams around it.
        Some(at) => panic!(
            "token stream diverges at index {at} (rust {} ids, golden {} ids)\n  rust:   {:?}\n  golden: {:?}",
            generated.len(),
            golden.len(),
            &generated[at..generated.len().min(at + 8)],
            &golden[at..golden.len().min(at + 8)],
        ),
        // Every SHARED id matches and only the lengths differ — that is not a numerics bug, it is
        // a stale golden: SEQ_CAP or the step budget moved on one side only. Say that, because
        // "diverges at index None" sends the reader hunting for a rounding error.
        None => panic!(
            "token streams AGREE on all {} shared ids but lengths differ (rust {}, golden {}) \
             — SEQ_CAP or the step budget changed on one side only; rerun \
             tools/bard_golden_baseline.sh to regenerate the goldens",
            generated.len().min(golden.len()),
            generated.len(),
            golden.len(),
        ),
    }
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
    assert_ids_match(&generated, &golden_ids);
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

// ── #302 continuation: one press keeps the SAME story going ─────────────────────────────

/// Run the current chapter to its end, returning its text and how it ended.
fn run_chapter(
    m: &Model,
    t: &Tokenizer,
    bufs: &mut Bufs,
    story: &mut Story,
) -> (std::string::String, bool) {
    let mut text = std::string::String::new();
    let mut steps = 0u32;
    loop {
        match story.step(m, t, bufs) {
            StepOut::Text(b) => text.push_str(&std::string::String::from_utf8_lossy(b)),
            StepOut::Working => {}
            StepOut::Done { truncated } => return (text, truncated),
        }
        steps += 1;
        assert!(steps < 400, "chapter did not terminate");
    }
}

#[test]
fn story_continues_past_the_cache_depth() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let mut story = Story::new(&t, GOLDEN_PROMPT, 2);

    let (first, truncated) = run_chapter(&m, &t, &mut bufs, &mut story);
    assert!(truncated, "chapter 1 should be cut, not EOS-ended");
    assert!(story.is_done() && story.can_continue());
    // The pre-#302 stopping point, unchanged: the cache depth is still where chapter 1 pauses.
    assert_eq!(story.pos() as usize, SEQ_CAP - 1);

    // Four more chapters — well past the point where the whole prompt has been evicted.
    let mut all = first;
    for ch in 2..=5 {
        assert!(story.resume(), "chapter {ch} should be resumable");
        assert!(!story.is_done(), "resume must clear the done flag");
        let (text, _) = run_chapter(&m, &t, &mut bufs, &mut story);
        assert!(!text.is_empty(), "chapter {ch} produced no text");
        assert!(text.is_ascii(), "chapter {ch} is not ASCII: {text:?}");
        all.push_str(&text);
        if !story.can_continue() {
            break; // the model chose to end it — a real ending, tested separately
        }
    }

    // Positions ran past the cache, which is the whole point: the ring, not a second cache.
    assert!(
        story.pos() as usize > SEQ_CAP,
        "story never left the first window: pos {}",
        story.pos()
    );
    // A continued story is LONGER than a single chapter could ever be, and still prose: the
    // failure mode to catch is a collapse into one repeated token, which would tank both.
    assert!(all.len() > 300, "continued story is only {} chars", all.len());
    let words: std::vec::Vec<&str> = all.split_whitespace().collect();
    let uniq: std::collections::BTreeSet<&&str> = words.iter().collect();
    assert!(
        uniq.len() * 3 > words.len(),
        "continuation collapsed into repetition: {} distinct of {} words\n{all}",
        uniq.len(),
        words.len()
    );
}

#[test]
fn continuation_is_seamless_and_reproducible() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);

    // Nothing is re-fed across a chapter boundary, so the continued text must NOT repeat the
    // tail of the previous chapter — the seam lands mid-word as often as not.
    let tell = |bufs: &mut Bufs| {
        let mut story = Story::new(&t, GOLDEN_PROMPT, 7);
        let (a, _) = run_chapter(&m, &t, bufs, &mut story);
        assert!(story.resume());
        let (b, _) = run_chapter(&m, &t, bufs, &mut story);
        (a, b)
    };
    let (a, b) = tell(&mut bufs);
    let tail: std::string::String = a.chars().rev().take(12).collect::<std::string::String>()
        .chars().rev().collect();
    assert!(
        !b.starts_with(&tail),
        "chapter 2 replayed the tail of chapter 1 — a token was fed twice"
    );

    // And the whole continued story is still a pure function of its seed, even after another
    // story has scribbled over the shared KV cache (the `Bufs` ownership contract).
    let mut other = Story::new(&t, GOLDEN_PROMPT, 99);
    run_chapter(&m, &t, &mut bufs, &mut other);
    let (a2, b2) = tell(&mut bufs);
    assert_eq!((a, b), (a2, b2), "a continued story is not reproducible from its seed");
}

#[test]
fn a_story_the_model_ended_is_not_continuable() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);

    // EOS is ~5% of chapter endings (T8), so hunt for one by continuing seeds — with chapters
    // available, a natural ending shows up quickly.
    let mut found = false;
    'seeds: for seed in 1..=8u32 {
        let mut story = Story::new(&t, GOLDEN_PROMPT, seed);
        for _ in 0..8 {
            let (_, truncated) = run_chapter(&m, &t, &mut bufs, &mut story);
            if !truncated {
                // The model said the story is over: no continuing, and `resume` must refuse
                // rather than quietly restart mid-breath.
                assert!(!story.can_continue(), "seed {seed} ended on EOS but claims continuable");
                assert!(!story.resume(), "resume must refuse an EOS-ended story");
                assert!(story.is_done(), "a refused resume must leave the story done");
                assert!(matches!(
                    story.step(&m, &t, &mut bufs),
                    StepOut::Done { truncated: false }
                ));
                found = true;
                break 'seeds;
            }
            assert!(story.resume());
        }
    }
    assert!(found, "no seed ended on end-of-text in 8 seeds x 8 chapters — EOS path untested");
}

#[test]
fn chapters_are_bounded_and_the_cursor_is_never_exhausted() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let mut story = Story::new(&t, GOLDEN_PROMPT, 3);

    // Every chapter must be bounded by CHAPTER_TOKENS — an unbounded chapter would hold the UI
    // (and, on the board, the mesh's tick) for as long as the model felt like talking.
    let mut prev = story.pos();
    for _ in 0..12 {
        run_chapter(&m, &t, &mut bufs, &mut story);
        let grown = story.pos() - prev;
        assert!(
            grown <= Story::CHAPTER_TOKENS,
            "a chapter generated {grown} tokens, over the {} budget",
            Story::CHAPTER_TOKENS
        );
        prev = story.pos();
        if !story.resume() {
            break;
        }
    }
    // The far end of the cursor: a story parked near POS_MAX must refuse to continue instead of
    // wrapping `pos` (which would feed every later token into one slot forever).
    assert!(POS_MAX < u16::MAX as usize, "POS_MAX must leave the u16 cursor room");
    assert!(
        POS_MAX + Story::CHAPTER_TOKENS as usize <= u16::MAX as usize,
        "a final chapter must not be able to overflow the cursor"
    );
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

// ── #302 rolling scrollback: what a continuation does to the text buffer ────────────────

/// `roll` with the buffer/len bookkeeping the firmware does around it, so the fixtures read like
/// the panel's state rather than like a slice call.
fn rolled(text: &str, keep: usize, revealed: usize) -> (std::string::String, usize) {
    let mut buf = text.as_bytes().to_vec();
    let dropped = clock::textflow::roll(&mut buf, text.len(), keep, revealed);
    let kept = std::string::String::from_utf8(buf[..text.len() - dropped].to_vec())
        .expect("roll must never split a character");
    (kept, dropped)
}

#[test]
fn roll_drops_the_oldest_text_and_says_how_much() {
    // Under the keep bound: nothing moves (the common case — one chapter does not fill 1 KB).
    assert_eq!(rolled("a short story", 256, 999), ("a short story".into(), 0));

    // Over it: the OLDEST bytes go and the tail slides to offset 0.
    let (kept, dropped) = rolled("0123456789", 4, 999);
    assert_eq!((kept.as_str(), dropped), ("6789", 6));

    // Degenerate bounds must not panic or wrap: keep 0, and an empty buffer.
    assert_eq!(rolled("abc", 0, 999), ("".into(), 3));
    assert_eq!(rolled("", 256, 999), ("".into(), 0));
}

#[test]
fn roll_never_eats_an_unread_word() {
    // The typewriter is 4 bytes in, so at most 4 bytes may be dropped even though the keep bound
    // asks for 6 — the reader has not seen bytes 4.. yet, and losing them would silently skip
    // text mid-story.
    let (kept, dropped) = rolled("0123456789", 4, 4);
    assert_eq!((kept.as_str(), dropped), ("456789", 4));
    // Nothing revealed at all ⇒ nothing may be dropped, however full the buffer is.
    assert_eq!(rolled("0123456789", 2, 0), ("0123456789".into(), 0));
}

#[test]
fn roll_never_splits_a_character() {
    // U+2019 (’) is one 3-byte token in this vocabulary. A cut that lands inside it must walk
    // FORWARD to the next character boundary — a dangling continuation byte would make
    // `draw_story` skip the entire line it lands on, losing a line of story to a blank.
    let text = "ab’cd"; // bytes: a b E2 80 99 c d
    assert_eq!(text.len(), 7);
    for keep in 0..=text.len() {
        let (kept, dropped) = rolled(text, keep, 999);
        assert!(
            !kept.is_empty() || dropped == text.len(),
            "keep={keep} produced an empty buffer without dropping everything"
        );
        // The invariant: whatever is kept is still decodable (checked inside `rolled`) and the
        // cut landed on a boundary, never at byte 3 or 4.
        assert_ne!(dropped, 3, "cut inside the ’ token");
        assert_ne!(dropped, 4, "cut inside the ’ token");
    }
}

#[test]
fn golden_failure_names_the_divergent_token() {
    // The length-only arm has been exercised for real (planting stale goldens against a changed
    // SEQ_CAP). The numeric-divergence arm never has, because the two implementations have never
    // disagreed — so provoke it deliberately, on an in-memory COPY. The committed testdata is
    // never touched: a test that corrupts a fixture it shares with another test is a trap.
    let golden: std::vec::Vec<u16> = include_str!("../src/bard/testdata/golden_tokens.txt")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().parse().expect("token id"))
        .collect();
    let at = 40usize;
    assert!(golden.len() > at + 8, "fixture too short to window around {at}");
    let mut mutated = golden.clone();
    mutated[at] = 999; // not a real id in this vocabulary — unmistakable in the output

    // Silence the expected panic so a passing run does not print a scary backtrace.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(std::boxed::Box::new(|_| {}));
    let payload = std::panic::catch_unwind(|| assert_ids_match(&mutated, &golden))
        .expect_err("a flipped id must fail the comparison");
    std::panic::set_hook(prev_hook);

    let msg = payload
        .downcast_ref::<std::string::String>()
        .map(|s| s.as_str())
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic payload>");
    // It must name the position, not just complain.
    assert!(
        msg.contains(&std::format!("diverges at index {at}")),
        "expected the divergence index in: {msg}"
    );
    // And BOTH windows must straddle the divergence — the rust side showing the flipped id, the
    // golden side the original, each starting at `at`.
    let want_rust = std::format!("{:?}", &mutated[at..at + 8]);
    let want_golden = std::format!("{:?}", &golden[at..at + 8]);
    assert!(msg.contains(&want_rust), "expected rust window {want_rust} in: {msg}");
    assert!(
        msg.contains(&want_golden),
        "expected golden window {want_golden} in: {msg}"
    );
}

#[test]
fn stack_paint_scanner_finds_the_high_water_mark() {
    use clock::stack_paint::{untouched_bytes, SENTINEL};
    // The paint itself is device-only (linker symbols, raw writes); the SCAN is pure arithmetic
    // and is where an off-by-one would silently flatter the bench number, so test it here.
    let mut region = [SENTINEL; 16];

    // Nothing overwritten: the whole painted span is still untouched.
    assert_eq!(untouched_bytes(&region), 64);

    // The stack grows DOWN, so index 0 is the deepest address. A frame that reached word 4
    // leaves words 0..4 untouched = 16 bytes.
    region[4] = 0xDEAD_BEEF;
    assert_eq!(untouched_bytes(&region), 16);

    // A deeper excursion wins even if shallower marks exist above it.
    region[1] = 0x1234_5678;
    assert_eq!(untouched_bytes(&region), 4);

    // Fully consumed, and the degenerate empty region — neither may panic or wrap.
    region[0] = 1;
    assert_eq!(untouched_bytes(&region), 0);
    assert_eq!(untouched_bytes(&[]), 0);
}

// ── #303 runtime prompt validation ──────────────────────────────────────────────────────

/// The built-in prompts must all pass the validator they gate operator input with — otherwise
/// the firmware would reject its own defaults.
#[test]
fn builtin_prompts_pass_validation() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    for id in 0u8..=255 {
        let mut buf = [0u8; 64];
        let n = clock::persona::prompt(id, &mut buf);
        let got = clock::persona::validate_prompt(&t, &buf[..n]);
        assert!(got.is_ok(), "node {id} default prompt rejected: {got:?}");
        assert!(got.unwrap() <= clock::persona::PROMPT_TOKEN_BUDGET);
    }
}

#[test]
fn validation_accepts_plain_tinystories_prose() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    for good in [
        "Once upon a time, there was a little cat",
        "One day, a small dog went to the park",
        "The little bird was very happy",
    ] {
        let r = clock::persona::validate_prompt(&t, good.as_bytes());
        assert!(r.is_ok(), "rejected good prompt {good:?}: {r:?}");
    }
}

#[test]
fn validation_rejects_the_four_failure_modes() {
    use clock::persona::PromptErr;
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();

    // 1. over the 64-byte buffer/CFG-value bound
    let long = "a".repeat(65);
    assert!(matches!(
        clock::persona::validate_prompt(&t, long.as_bytes()),
        Err(PromptErr::TooLong { got: 65 })
    ));

    // 2. not UTF-8 (a CFG payload is arbitrary wire bytes)
    assert_eq!(
        clock::persona::validate_prompt(&t, &[0xff, 0xfe]),
        Err(PromptErr::NotUtf8)
    );

    // 3. an emoji has no token at all -> byte fallbacks -> refused, and it says WHERE
    match clock::persona::validate_prompt(&t, "the cat 🐱 sat".as_bytes()) {
        Err(PromptErr::UnrepresentableByte { at_byte }) => {
            assert!(at_byte > 0, "position should locate the emoji, got {at_byte}")
        }
        other => panic!("emoji should be refused, got {other:?}"),
    }

    // 4. fits 64 bytes but spends too much of the shared window. Note WHICH text does this:
    // ordinary prose is dense (~2.5 B/token, so 58 bytes is only ~23 tokens and passes), so the
    // budget is reached by FRAGMENTATION — nonsense words encode ~1 token/char. That makes the
    // budget an automatic backstop on hazard (2): mild fragmentation is accepted and reported,
    // severe fragmentation refuses itself here.
    let fragmenting = "Xyzzy Plugh Frobnitz Quux Zork Grue Blorb Vogon";
    assert!(fragmenting.len() <= 64);
    match clock::persona::validate_prompt(&t, fragmenting.as_bytes()) {
        Err(PromptErr::TooManyTokens { got }) => {
            assert!(got > clock::persona::PROMPT_TOKEN_BUDGET)
        }
        other => panic!("a window-hogging prompt should be refused, got {other:?}"),
    }
}

/// The honest distinction that the doc comment claims: an ASCII realm name is REPRESENTABLE
/// (single-char tokens exist) so it is ACCEPTED — but it fragments, and the token count is the
/// signal. If a future vocabulary changes this, this test says so instead of the comment lying.
#[test]
fn ascii_realm_names_are_accepted_but_fragment() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let odd = "Once upon a time, there was Eldritch";
    let n = clock::persona::validate_prompt(&t, odd.as_bytes())
        .expect("ASCII is representable, so this must be accepted, not refused");
    let plain = "Once upon a time, there was a little owl";
    let n_plain = clock::persona::validate_prompt(&t, plain.as_bytes()).unwrap();
    // Same byte-length ballpark, materially more tokens = the fragmentation the operator is warned about.
    assert!(
        n > n_plain,
        "expected 'Eldritch' to fragment into more tokens ({n}) than plain prose ({n_plain})"
    );
}
