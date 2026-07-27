# The Bard — an on-device tiny-LLM storyteller for the smol fleet

**Inspiration:** [slvDev/esp32-ai](https://github.com/slvDev/esp32-ai) (28.9M-param TinyStories model on an ESP32-S3 — 8MB PSRAM, 16MB flash) · **Lineage:** [karpathy/llama2.c](https://github.com/karpathy/llama2.c) (MIT) + the [TinyStories](https://arxiv.org/abs/2305.07759) dataset
**Status:** design approved in-session by JP 2026-07-26 — next step: implementation plan
**Author:** claude (fable-5, orchestrator) · **Date:** 2026-07-26

> ### ✏️ AMENDMENT — 2026-07-26 (T9, measured on the canonical image)
> **§5 RAM budget — the `SEQ_CAP` knob fired.** At `SEQ_CAP=256` the canonical fleet image (`espnow,cast,io,bard`) **fails to link** (`.bss` overflow by 20,704 B in DRAM). Shipped at **`SEQ_CAP=192`**. Measured loadable-section deltas (bard vs no-bard): `.bss` +78,528 B · `.data` +1,184 B · `.rodata` +285,360 B (283,096 = the blob). ~~Remaining DRAM headroom ≈ 2.6 KB~~ **CORRECTED (oracle, same day): that 2,592 B is the entire remaining RUNTIME STACK** — bard's `.bss` consumed the `.stack` region (82,304 → 2,592 B; the linker shrinks `.stack` silently to zero before erroring, so "it links" is NOT a stack guarantee, and `__stack_chk_guard` landed outside the stack region entirely). Overflow corrupts the adjacent `Bufs`/KV cache → garbled stories, not a clean crash. Remedy SHIPPED (f287c26), measured: **`SEQ_CAP=160`** → `.bss` 257,112 B, **`.stack` = 14,240 B, `__stack_chk_guard` back inside the region**; `repro_build.sh` now derives `_stack_start − _stack_end` via readelf and **hard-fails below 12,288 B** (negative-tested against the real 2,592 B ELF); a **stack-paint high-water measurement at T13** still gates the fleet roll. Stories cache-bound ~145 tokens / ~300 chars. The esp-wifi 128 KiB heap (#140) is the follow-up reclaim lever if stories should grow back. Goldens regenerate bit-for-bit at each cap change (146@160 and 178@192 both verified prefix-identical to their predecessors).
>
> ### ✏️ AMENDMENT — 2026-07-26 (implementation planning, same session)
> Four refinements settled while writing the plan (`docs/superpowers/plans/2026-07-26-bard-tiny-llm.md`):
> **§6 RNG** — hardware-TRNG seeding → **xorshift32 seeded from `now_ms` at the button press** (Snake's proven pattern, `src/snake.rs:106`); the TRNG peripheral is owned by the radio layer and unreachable in the `default` tier. **§11 golden test** — "must equal reference output exactly" → **≥120-char temp-0 prefix match vs an independent Python reference** (`tools/bard_reference.py`, numpy float32, mirroring the SBRD integer semantics exactly) plus a full-story self-golden for regressions. Upstream `runq.c` was disqualified as the reference at T2: it mishandles in-dims not divisible by GS — a correct port MUST disagree with it on this checkpoint. Pinned provenance (llama2.c @ `350e04fe35433e6d2941dce5a1f53308f87058eb`, blob shas verified via `git hash-object`): `runq.c:332` `for (j = 0; j <= n - GS; j += GS)` skips elements 128..171 of every 172-wide w2 row; `runq.c:146` `int num_groups = n / GS;` leaves the activation tail unquantized. Upstream's on-disk v2 format is also per-matrix q/s interleaved (`export.py:196-210`) — SBRD's family-grouped layout is genuinely different, by design. **§3 host tests** — ride the existing `hostsim` lib target (#152), no new crate; bard's pure-core modules are exported there. **§4 hidden_dim** — confirmed **172** at export; stats line of record: `dim=64 hidden=172 layers=5 heads=8/4 vocab=512 seq=512 gs=64 shared=1 size=283096 max_qerr=0.0070`. GS=64 groups run over each matrix's **flattened** data (upstream's real rule), so w2's 172-wide rows straddle group boundaries and the matmul flushes its accumulator at every weight- or activation-group edge. Blob layout is **family-grouped** (oracle byte-probe): per tensor family, `i8[all layers]` then `f32 scales[all layers]` — NOT llama2.c's per-layer q/s interleave; a port of upstream's `init_quantized_tensors` loop would mis-parse layers 1..4.

---

## 0. Thesis

A real transformer LLM, generating a children's story **entirely on the $3 ESP32-C3**, typewriter-style onto the 72×40 OLED. No WiFi, no cloud, no gateway — a leaf alone in a drawer can compose a tale. The inspiration project needs 100× smol's RAM; the trick that makes smol's version work is that **both projects share the llama2.c/TinyStories lineage**, which publishes a checkpoint small enough to execute-in-place from the C3's memory-mapped flash: **stories260K**.

The pitch in one line: *press the button, and the board writes you a story it has never written before.*

## 1. Constraints — smol vs. the inspiration's S3

The S3 is a **dual-core** Xtensa with PSRAM support; smol boards are **single-core RISC-V with no PSRAM** — a totally different class of part, and the reason a straight port is impossible. The repo's firmware today targets the C3 (`riscv32imc`, per `rust-toolchain.toml`); the Embassy re-platform (#198/#233) tracks the esp32c6-watch stack, and this design is deliberately **chip-portable across C3/C6** — same single-core scheduling story, same integer-only math; a C6 only *adds* SRAM headroom.

| | esp32-ai (S3) | smol (C3 / C6 SuperMini class) |
|---|---|---|
| Cores | **2× Xtensa LX7** (inference can hog a core) | **1× RV32** — inference must time-share with mesh/display (§7) |
| SRAM | 512KB **+ 8MB PSRAM** | ~400KB (C3) / ~512KB (C6), **no PSRAM** |
| Flash | 16MB | 4MB (two 1.94MB OTA slots; image ~590KB → **~1.3MB headroom**) |
| FPU | yes (S3) | **none** (RV32IMC / RV32IMAC) — integer + softfloat only |
| Model | 28.9M params, 14.9MB, 4-bit | **260K params, ~280KB int8** |
| Tok/s | 9.5 | est. 10–50 raw (unbenched); throttled for display |

Non-negotiable smol invariants this design honors: `no_std`, **no heap on the base build** (all static buffers), single-crate `rust/clock` firmware, byte-reproducible release image (#44), mesh/Familiar/time-sync must never starve, OTA image ≤ slot (gated by `ota_publish.sh`).

## 2. Decisions (locked in brainstorm, 2026-07-26)

1. **Model strategy:** stock **stories260K** checkpoint now (zero training); custom-trained ~1–2M-param model later as a weights-only swap (§12).
2. **Trigger (v1):** button press inside the app. No idle-dream mode, no HA/CFG trigger in v1.
3. **Story seed:** per-node protagonist mapped from node identity — id7 *Draconic Dominion* → "a little dragon". Prompts stay inside the model's 512-token vocab.
4. **Engine:** pure `no_std` Rust port of the llama2.c inference core. No C FFI (keeps the reproducible build toolchain-clean), no Markov fallback (the point is a real transformer).

## 3. Architecture

```
rust/clock/src/bard/
├── mod.rs        — the app: menu registration, button handling, screen state
├── nano_llm.rs   — display-agnostic inference core (the llama2.c port)
├── tokenizer.rs  — 512-token BPE encode/decode (llama2.c format)
└── weights.rs    — include_bytes! blobs + header parse + checksum
tools/bard_export_model.sh — offline: fetch checkpoint → quantize int8 → emit .bin
rust/clock/model/stories260K-q8.bin — committed artifact (~280KB, MIT provenance)
```

- **App layer** (`mod.rs`): plugs into the existing static-plugin dispatch exactly like Snake/Familiar. Owns the story text buffer, word-wrap, scroll, and the composing-quill animation.
- **Inference core** (`nano_llm.rs`): pure function of (weights, prompt tokens, RNG seed) → token stream. `#![no_std]`, zero alloc, **compiles and unit-tests on host x86** — this is what makes the golden-token test (§11) possible.
- **Weights** (`weights.rs`): `include_bytes!` puts the blob in `.rodata`, which the C3 **executes-in-place from flash through the 16KB cache — the weights never occupy RAM**. This is the same "stream from slow storage" idea esp32-ai used PSRAM+PLE for, done by the MMU for free.

## 4. Model artifact pipeline (offline, one-time per model)

`tools/bard_export_model.sh`:
1. Fetch `stories260K.pt` + `tok512.model` from HF `karpathy/tinyllamas` (pinned revision, sha256-verified).
2. Quantize to int8, group-wise (Q8_0-style: 64 weights share one f32 scale — +6.25% size).
3. Emit a single `.bin`: header (magic `SBRD`, version, dims: dim=64, n_layers=5, n_heads=8, n_kv_heads=4, vocab=512, seq=512, hidden as read from the checkpoint config — ~172) + tokenizer table + weight groups + trailing CRC32.
4. Commit the `.bin` to the repo. It is a **deterministic byte artifact** — reproducible builds (#44) are unaffected; the image hash stays a verifiable identity.

The runtime parses the header (no hardcoded dims beyond compile-time maxima), so the §12 model swap is a re-run of this script + a const bump — no engine change.

## 5. Memory budget

| Item | Where | Size |
|---|---|---|
| Weights int8 + group scales | flash `.rodata` (XIP, zero RAM) | ~280KB |
| Tokenizer table (512 BPE entries + scores) | flash `.rodata` | ~4KB |
| KV cache — **int8 + per-vector scales**, `SEQ_CAP=256` | static `.bss` | ~92KB |
| Activations (dim 64; head-at-a-time attention keeps scores ~1KB) | static `.bss` | ~6KB |
| Logits (512 × f32) | static `.bss` | 2KB |
| Story text buffer (≤250 tokens rendered) | static `.bss` | 1KB |
| **Total RAM** | | **~101KB** |

- Flash: image grows ~590KB → **~880KB** — 45% of a slot, mesh-OTA-able unchanged.
- **`SEQ_CAP` is the pressure-relief knob**: KV cost is linear (256→160 tokens ≈ 92→58KB). Free `.bss` headroom beside the ESP-NOW stack is the one number we only learn at first link; if it doesn't fit at 256 we ship at a lower cap — stories target ~200 tokens anyway.
- KV is quantized (weights would fit even f32; the cache wouldn't: f32 KV at seq 256 is 327KB — more than exists).

## 6. Inference core

- **Matmuls** (all of them): int8 × int8 → i32 accumulate, rescale by (weight-scale × activation-scale) — activations re-quantized per vector, group-wise, mirroring llama2.c's `runq.c`.
- **RMSNorm, RoPE, softmax, sampling:** softfloat f32. These touch vectors of 64–512 elements — microseconds per token even emulated; not worth fixed-point complexity.
- **Attention:** computed head-at-a-time (8 heads, head_dim 8, GQA 2:1 onto 4 KV heads) to keep score scratch at one row.
- **Sampling:** temperature ≈ 0.9 + top-p 0.9 (matching upstream's quality notes; temperature-0 output is repetitive). RNG: xoshiro-class PRNG seeded from the hardware TRNG per story. **Temperature 0 path retained** for the golden test.
- **Termination:** BOS/EOS token, 250-token cap, or `SEQ_CAP` — whichever first.

## 7. Generation state machine & scheduling

```rust
enum BardState { Idle, Composing { pos: u16, .. }, Told }
fn poll_generate(&mut self, budget: TickBudget) -> Step  // Step::{Working, Chars(&str), Done}
```

- The app calls `poll_generate()` once per main-loop tick; the core does **at most one token's forward pass per call** (est. 20–100ms per the §1 speed estimate — benched in §11 before we rely on it), then returns. Mesh RX, Familiar, time-sync, and display flush all run between tokens, exactly as they do between frames of Snake.
- If one token per tick proves too long a stall for the mesh (bench data decides), the fallback is a mid-forward-pass yield point (per-layer), which the state machine's `pos` field already accommodates. **Not built unless the bench says so.**
- Deliberately **executor-agnostic**: a polled state machine works identically in today's sync loop and in the post-#198/#233 Embassy world (where it becomes a task that awaits between tokens). No Embassy dependency, nothing to unpick at cutover.
- Output is throttled to ~4–8 chars/s for the typewriter feel; raw generation speed above that is banked headroom, not shown.

## 8. Story seeding — every node tells its own kind of story

- Const table of ~16 protagonists using **only TinyStories-frequent words**: "a little dragon", "a small owl", "a tiny robot", "a brave cat", "a little star"…
- Index = node-id hash → stable per board; known bench boards get curated picks (id7 *Draconic Dominion* → dragon, id8 *Eldritch Nexus* → owl, id9 *Jade Herald* → bird).
- Prompt template: `"Once upon a time, there was <protagonist>"` — BPE-encoded with the same tok512 vocab/merge-scores the model was trained with.
- Realm names themselves stay **out** of the prompt: a 512-token vocab would shred "Eldritch" into alien subwords and derail a 260K model. The persona mapping carries the flavor instead. (A future custom model may bake realm names into its vocab — §12.)

## 9. Display UX (72×40 OLED)

- ~14 chars × 5 lines in the existing small font; word-wrapped, auto-scrolling as text arrives; a blinking quill glyph marks "composing".
- **Short press:** new story — or, while composing, finish-fast (drop the throttle, generate to the end).
- **Long press:** exit to menu (existing app convention).
- A finished story stays on screen for re-reading; final screen shows a small `~ fin ~` marker.
- Boot/menu name: **Bard**. Fantasy theming per house style.

## 10. Error handling

- **Init:** CRC32 + magic + dims-fit-compile-time-maxima check on the blob. Failure → app renders "the bard is mute" and stays a harmless menu entry; nothing else in the firmware is affected.
- **Runtime:** generation is pure compute — no radio, no I/O, no alloc. The failure surface is timing (owned by the tick budget, §7) and the static buffers (sized at compile time, §5). A `SEQ_CAP` overrun ends the story gracefully at the cap.
- The app holds **no lock the mesh can contend on**; a wedged generation (shouldn't exist — no loops without bounds) could at worst stop advancing text, never the fleet.

## 11. Testing

1. **Golden-token test (host, `cargo test`):** the core compiled for x86, temperature 0, fixed prompt → token IDs must equal reference llama2.c `runq.c` output exactly. The port is proven correct on the desk before anything is flashed. Also a decode round-trip test on the tokenizer.
2. **On-target bench:** tok/s and worst-case per-tick stall measured on a bench board (serial DIAG line); numbers recorded in this doc's amendment section. This bench decides whether the per-layer yield fallback (§7) is needed.
3. **Sample-quality eyeball:** a batch of ~20 host-generated stories at temp 0.9 checked for "charming toddler prose, mostly English" — the honest bar for 260K params.
4. **Fleet:** canary-one-board OTA per standing rules (app-side rollback; espflash bootloader has no auto-revert), then the normal staged roll.
5. No new test framework — host tests ride `cargo test`, target validation rides the existing DIAG/serial path.

## 12. Out of scope for v1 (the named upgrade path)

- **Custom model** (~1–2M params, trained on katana with llama2.c training code, optionally realm-flavored dataset/vocab) → same `.bin` format, weights-only swap, golden test re-baselined. This is the quality lever.
- **HA / CFG-key trigger** ("write a story now" from the dashboard) and publishing finished stories back over MQTT (mind the ~490B publish cap — chunk or truncate).
- **Idle dream mode** (story-as-screensaver).
- **Persona override via CFG** (per-node protagonist editable from HA).
- 4-bit quantization (halves flash cost; only matters for the bigger model).

## 13. Risks

| Risk | Exposure | Mitigation |
|---|---|---|
| `.bss` headroom < ~101KB beside ESP-NOW stack | fit | `SEQ_CAP` knob (linear, §5); learned at first link, not on hardware |
| Token latency worse than estimate | mesh starvation | tick-budget design + benched fallback yield point (§7) |
| Int8 quality degradation | story charm | golden test pins math; upstream ships runq int8 as a supported path |
| 260K prose too incoherent even for whimsy | product | expectation set in §2 ("toddler babble is the charm"); §12 custom model is the real fix |
| Flash-cache thrash slows other apps while composing | UX | weights only touched inside `poll_generate`; app is foreground-only by design |
