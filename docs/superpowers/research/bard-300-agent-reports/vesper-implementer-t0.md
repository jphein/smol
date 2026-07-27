# Bard #300 — Task 0 (scaffolding) — vesper-implementer

Worktree: `/home/jp/Projects/smol/.claude/worktrees/bard-300` · branch `feat/300-bard-tiny-llm`
Commit: **4859c78** `feat(bard): #300 scaffolding — bard feature + libm + hostsim core export`
Status: **DONE_WITH_CONCERNS** (all deliverables green; three notes for later tasks + one recovered git incident)

## Changed (4 files, +21/-1)
- `rust/clock/Cargo.toml` — `libm = { version = "0.2", optional = true, default-features = false }` after `nb`;
  `bard = ["hw", "dep:libm"]` after the `cast` block; `hostsim = []` → `["dep:libm"]` (comments preserved).
- `rust/clock/Cargo.lock` — libm 0.2.16 added.
- `rust/clock/src/bard/nano_llm.rs` (new) — `//! bard #300 core (populated by later tasks)`.
- `rust/clock/src/lib.rs` — `#[cfg(feature="hostsim")] #[path="bard/nano_llm.rs"] pub mod nano_llm;`
  placed alphabetically among the pure cores (input → nano_llm → sensors) with a 2-line comment.

## Verification (all with `CARGO_TARGET_DIR=$HOME/.cache/smol-bard-target`)
| command | result |
|---|---|
| `cargo build --release --no-default-features --features hw` | OK (267 KB ELF) |
| `cargo clippy --release --no-default-features --features hw -- -D warnings` | clean |
| `cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu --lib` | ok, 0 passed |
| *(extra)* `cargo build`/`clippy --features bard` | OK / clean |
| *(extra)* `cargo tree` libm count: `hw` = **0**, `bard` = 1, `hostsim` = 1 | default tree stays libm-free |

## Notes for later tasks
1. **The plan's hostsim test command is wrong** — without `--lib`, `cargo test --features hostsim`
   also builds the BIN target (`src/main.rs`), which needs the `hw` crates → 16 errors
   (`unresolved import esp_hal`, `Sensors::new` arity, …). **Verified identical at HEAD** (pre-existing,
   not caused by Task 0). T3/T4/T5/T7 must scope the target: `--lib`, or `--test <name>` for `tests/`.
2. **Fresh worktrees need the gitignored per-board files**: `cp` `src/board.rs` + `src/secrets.rs`
   from `/home/jp/Projects/smol/rust/clock/src/` (tracked template is `board.rs.example`), else the bin
   fails with `file not found for module 'board'`. Both are gitignored → never committed.
3. **lib target name = `clock`** (no `[lib]` section → derives from `[package] name`). Tests will
   `use clock::nano_llm::…` — the `#[path]` export makes it top-level `clock::nano_llm`, NOT
   `clock::bard::nano_llm`.

## Incident (recovered, no data lost) — don't repeat
Baseline check ran `git stash -q -- rust/clock/…` from cwd `rust/clock` → repo-relative pathspecs
failed; the follow-up `git stash pop` (chained after `;`, not `&&`) then popped an **unrelated
pre-existing stash** — `8f5115f` "On dream/agent-af54169dac83bf0e9: crash-2026-07-22" — restoring its
untracked files (`experiments/*/Cargo.lock`, `scratch/…`) and dropping the entry.
Recovery: located the commit via `git fsck --unreachable`, `git stash store 8f5115f… -m "On
dream/agent-af54169dac83bf0e9: crash-2026-07-22"` (back at stash@{0}, list = 6 entries again), then
`git clean -fd experiments scratch`. Tracked files were never touched (pop said "Already up to date").
**Lessons:** (a) never chain a bare `git stash pop` after a stash that may fail — pop is not scoped and
takes whatever is on top; (b) shared repos carry other agents' crash stashes — treat the stash stack as
someone else's data; (c) to A/B a baseline, copy files to `/tmp` + `git checkout -- <paths>` instead.
