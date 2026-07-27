//! bard (#300): print 20 stories, one per seed, for the spec §11 eyeball check.
//!
//! ```text
//! cargo run --example bard_stories --no-default-features --features hostsim \
//!     --target x86_64-unknown-linux-gnu --release
//! ```
//!
//! This is the same code path the firmware runs — the pure cores compiled for the host — so it
//! is also where the host-side tokens/sec number comes from. The device will be far slower
//! (160 MHz, no FPU, flash-resident weights); Task 11 measures that on hardware.
use clock::bard_tokenizer::Tokenizer;
use clock::nano_llm::*;
use std::time::Instant;

const PROMPT: &str = "Once upon a time, there was a little dragon";

fn main() {
    let blob = include_bytes!("../model/stories260K-q8.bin");
    let m = Model::parse(blob).expect("blob parses");
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).expect("tokenizer table");
    let mut bufs = Box::new(Bufs::INIT);
    println!(
        "bard #300 — dim={} hidden={} layers={} heads={}/{} vocab={} gs={} \
         temp={} top_p={} max_tokens={}",
        m.cfg.dim,
        m.cfg.hidden,
        m.cfg.n_layers,
        m.cfg.n_heads,
        m.cfg.n_kv_heads,
        m.cfg.vocab,
        m.cfg.gs,
        Story::TEMP,
        Story::TOP_P,
        Story::MAX_TOKENS
    );

    let (mut total_tokens, mut total_secs) = (0u32, 0f64);
    for seed in 1..=20u32 {
        let mut story = Story::new(&t, PROMPT, seed);
        print!("\n=== seed {seed} ===\n{PROMPT}");
        let start = Instant::now();
        let mut tokens = 0u32;
        let truncated = loop {
            match story.step(&m, &t, &mut bufs) {
                StepOut::Text(b) => {
                    print!("{}", core::str::from_utf8(b).unwrap());
                    tokens += 1;
                }
                StepOut::Working => {}
                StepOut::Done { truncated } => break truncated,
            }
        };
        let secs = start.elapsed().as_secs_f64();
        total_tokens += tokens;
        total_secs += secs;
        // A cut is the normal ending at 260K params (EOS is essentially never sampled), so say
        // which one happened rather than letting a mid-sentence stop look like a bug.
        println!(
            "{}\n[seed {seed}: {tokens} tokens in {:.0} ms = {:.0} tok/s, ended by {}]",
            if truncated { " …" } else { "" },
            secs * 1000.0,
            tokens as f64 / secs,
            if truncated { "token budget" } else { "end-of-text" }
        );
    }
    println!(
        "\n=== {total_tokens} tokens in {:.2} s = {:.0} tok/s (host, release) ===",
        total_secs,
        total_tokens as f64 / total_secs
    );
}
