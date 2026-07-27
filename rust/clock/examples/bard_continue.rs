//! bard (#302): narrate ENDLESSLY on the host, to read what the sliding window does to the prose.
//!
//! ```text
//! cargo run --example bard_continue --no-default-features --features hostsim \
//!     --target x86_64-unknown-linux-gnu --release -- [blocks] [seeds]
//! ```
//!
//! This is the eyeball check that chose `nano_llm::KEEP` (spec §11's sample-quality bar, applied to
//! endless narration instead of to a single story). The interesting question is not "does it run" —
//! the host tests answer that — but whether prose written from a context that has slid out from
//! under it stays readable at 260K parameters. Output is grouped into 80-token blocks so a collapse
//! into repetition is visible at a glance; the number after each block is the fraction of DISTINCT
//! words in it, the cheapest collapse detector there is. Block 1 is the only one written from a
//! whole context; every block after it is the regime the firmware now lives in.
//!
//! When the model ends a tale, the narrator opens the next one — exactly what the screen does — so
//! `~` markers in the output are tale boundaries, not restarts of the program.
use clock::nano_llm::*;
use clock::tokenizer::Tokenizer;

const PROMPT: &str = "Once upon a time, there was a little dragon";
/// Tokens per reported block. One window, so block N+1 is written entirely from evicted context.
const BLOCK: u32 = SEQ_CAP as u32;

fn main() {
    let mut args = std::env::args().skip(1);
    let blocks: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(5);
    let seeds: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);

    let blob = include_bytes!("../model/stories260K-q8.bin");
    let m = Model::parse(blob).expect("blob parses");
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).expect("tokenizer table");
    let mut bufs = Box::new(Bufs::INIT);
    println!(
        "bard #302 — SEQ_CAP={SEQ_CAP} KEEP={KEEP} WINDOW={WINDOW} · \
         {blocks} blocks x {BLOCK} tok x {seeds} seeds"
    );

    for seed in 1..=seeds {
        println!("\n=== seed {seed} ===\n{PROMPT}");
        let mut story = Story::new(&t, PROMPT, seed);
        let mut tales = 0u32;
        for b in 0..blocks {
            let mut text = String::new();
            let mut tokens = 0u32;
            while tokens < BLOCK {
                match story.step(&m, &t, &mut bufs) {
                    StepOut::Text(bytes) => {
                        text.push_str(&String::from_utf8_lossy(bytes));
                        tokens += 1;
                    }
                    StepOut::Working => {}
                    StepOut::Done { truncated } => {
                        // The model finished a tale (or, never seen here, the cursor recycled).
                        // The narrator opens the next one over the same buffers — the screen's path.
                        tales += 1;
                        text.push_str(if truncated { "  ~ (cursor) ~  " } else { "  ~  " });
                        story = Story::new(&t, PROMPT, seed * 1000 + tales);
                        text.push_str(PROMPT);
                    }
                }
            }
            let words: Vec<&str> = text.split_whitespace().collect();
            let uniq = words.iter().collect::<std::collections::BTreeSet<_>>().len();
            let ratio = if words.is_empty() {
                0.0
            } else {
                uniq as f64 / words.len() as f64
            };
            print!("{text}");
            println!(
                "\n  [block {} · pos {} · {} words, {:.0}% distinct{}]",
                b + 1,
                story.pos(),
                words.len(),
                ratio * 100.0,
                if tales > 0 {
                    format!(" · {tales} tale(s) closed")
                } else {
                    String::new()
                }
            );
        }
    }
}
