//! bard (#302): continue ONE story across chapters, to read what the sliding window does to it.
//!
//! ```text
//! cargo run --example bard_continue --no-default-features --features hostsim \
//!     --target x86_64-unknown-linux-gnu --release -- [chapters] [seeds]
//! ```
//!
//! This is the eyeball check that chose `nano_llm::KEEP` (spec §11's sample-quality bar, applied
//! to continuation instead of to a single story). The interesting question is not "does it run" —
//! the host tests answer that — but whether prose written from a context that has slid out from
//! under it stays readable at 260K parameters. Chapter boundaries are marked, so a collapse into
//! repetition is visible at a glance rather than needing a metric. The number after each chapter
//! is the fraction of DISTINCT tokens in it, which is the cheapest collapse detector there is.
use clock::nano_llm::*;
use clock::tokenizer::Tokenizer;

const PROMPT: &str = "Once upon a time, there was a little dragon";

fn main() {
    let mut args = std::env::args().skip(1);
    let chapters: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(5);
    let seeds: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);

    let blob = include_bytes!("../model/stories260K-q8.bin");
    let m = Model::parse(blob).expect("blob parses");
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).expect("tokenizer table");
    let mut bufs = Box::new(Bufs::INIT);
    println!(
        "bard #302 — SEQ_CAP={SEQ_CAP} KEEP={KEEP} WINDOW={WINDOW} \
         chapter={} tok · {chapters} chapters × {seeds} seeds",
        Story::CHAPTER_TOKENS
    );

    for seed in 1..=seeds {
        let mut story = Story::new(&t, PROMPT, seed);
        println!("\n=== seed {seed} ===\n{PROMPT}");
        for ch in 0..chapters {
            let mut tokens = 0u32;
            let mut text = String::new();
            let ended = loop {
                match story.step(&m, &t, &mut bufs) {
                    StepOut::Text(b) => {
                        text.push_str(&String::from_utf8_lossy(b));
                        tokens += 1;
                    }
                    StepOut::Working => {}
                    StepOut::Done { truncated } => break truncated,
                }
            };
            // Distinct WORDS, not tokens: a 260K model collapses by repeating phrases, and the
            // word ratio catches that where a token-id ratio would not.
            let words: Vec<&str> = text.split_whitespace().collect();
            let uniq = words.iter().collect::<std::collections::BTreeSet<_>>().len();
            let ratio = if words.is_empty() {
                0.0
            } else {
                uniq as f64 / words.len() as f64
            };
            print!("{text}");
            println!(
                "\n  [chapter {} · {} tok · pos {} · {} words, {:.0}% distinct · ended by {}]",
                ch + 1,
                tokens,
                story.pos(),
                words.len(),
                ratio * 100.0,
                if ended { "chapter limit" } else { "end-of-text" }
            );
            if !story.resume() {
                println!("  [story is over — the model chose to stop; a press starts a new one]");
                break;
            }
        }
    }
}
