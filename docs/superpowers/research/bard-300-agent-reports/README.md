# Bard #300 — implementation agent reports

Working notes written by the implementer agents during the #300 campaign (The Bard: an on-device
tiny-LLM story app for the smol clock). They were originally scratch files outside the repo;
archived here because they record *measured* facts, deliberate spec deviations, and
gotchas-for-the-next-task that the commit messages alone don't carry.

These are **historical build logs, not living documentation.** The authoritative descriptions of
the shipped design live in `docs/superpowers/specs/` and `docs/superpowers/plans/`; where a report
disagrees with the code, the code wins (several notes below were superseded by later tasks — most
notably `SEQ_CAP`, which the reports quote as 256 and which T9.5 settled at 160).

| Report | Tasks | Commits | Status | One-line summary |
|---|---|---|---|---|
| [`vesper-implementer-t0.md`](vesper-implementer-t0.md) | T0 — scaffolding | `4859c78` | DONE_WITH_CONCERNS | `bard`/`hostsim` cargo features + optional `libm`; proves the default `hw` tree stays libm-free. |
| [`vesper-implementer-t1-t2.md`](vesper-implementer-t1-t2.md) | T1 — model fetch · T2 — SBRD export | `984c713`, `d3a3c28` | DONE_WITH_CONCERNS | sha256-pinned fetch of karpathy/tinyllamas `stories260K`, and the deterministic q8 SBRD v1 export (`dim=64 … gs=64 shared=1`, 283096 B). |
| [`vesper-implementer-t3-t4.md`](vesper-implementer-t3-t4.md) | T3 — SBRD parser · T4 — tok512 BPE | `d708512`, `d534a7f` | DONE_WITH_CONCERNS | Zero-copy borrowed-view parser validated length → crc32 → magic → version → geometry, plus BPE encode/decode; added `required-features = ["hw"]` to the bin so hostsim tests stop building firmware. |
| [`vesper-implementer-t5.md`](vesper-implementer-t5.md) | T5 — int8 forward pass | `b3ca0cf` (+ `03b9834`) | DONE | The no-`unsafe` int8 forward pass with a quantized KV cache — greedy decode produced coherent English, and the report pins the exact float-op order later used as T6's golden contract. |

No scratch reports exist for T6–T13; those tasks are documented by their commit messages and by
the spec/plan under `docs/superpowers/`.

## Cross-cutting findings worth remembering

- **hostsim test scope** (T0, resolved in T3/T4): `cargo test --features hostsim` used to drag in
  `src/main.rs` and fail on `esp_hal` imports. Fixed structurally with `[[bin]] required-features
  = ["hw"]`, which makes "hostsim compiles no firmware code" an invariant cargo enforces.
- **Fresh worktrees need the gitignored per-board files** (T0): copy `src/board.rs` and
  `src/secrets.rs` from the primary checkout (`board.rs.example` is the tracked template),
  otherwise the firmware target won't build.
- **Group size straddling** (T2 → T3 → T5): `gs` divides `dim` but not `hidden`, so w2's rows
  straddle quantization groups. The parser doesn't paper over it; the matmul handles the
  straddle explicitly.
- **Float-op order is load-bearing** (T5): the golden-baseline table at the end of the T5 report
  is the contract — SwiGLU is a divide, RoPE uses llama2.c adjacent-pair, attention scores
  multiply in a fixed order. Reassociating these breaks the goldens.
