# Bard #300 — Tasks 1 & 2 (fetch script + SBRD export) — vesper-implementer

Worktree: `/home/jp/Projects/smol/.claude/worktrees/bard-300` · branch `feat/300-bard-tiny-llm`
- **984c713** `feat(bard): #300 pinned fetch script for stories260K artifacts`
- **d3a3c28** `feat(bard): #300 SBRD v1 export pipeline + stories260K-q8 blob (MIT, karpathy/tinyllamas)`

Status: **DONE_WITH_CONCERNS** — both committed and verified; one deliberate spec deviation
(group size) plus a blocker-grade finding for Task 6.

## Task 1 — `tools/bard_fetch_model.sh` (+ `.gitignore`)
Upstream URLs/filenames worked exactly as specified (no adaptation). Pins baked:
```
stories260K.pt sha256=eec953f9d0f139e894ef8996302680e64b24813c7a98425424f5c85f7cf4abb1
tok512.bin     sha256=037cb335abb25d1fa9e8ecae30ed2a3a8ace9302862ebcdc05d51a6bbb10c312
```
- plain re-run → `bard model artifacts OK in scratch/bard/`
- **guard proven, not assumed**: a corrupted pin → `PIN MISMATCH stories260K.pt: got eec9… want deadbeef`, exit 1
- plan correction applied: `/scratch/bard/` added to `.gitignore` (`git check-ignore -v` → `.gitignore:60`)

## Task 2 — `tools/bard_export_model.py` + `rust/clock/model/stories260K-q8.bin`
Stats line (verbatim):
```
dim=64 hidden=172 layers=5 heads=8/4 vocab=512 seq=512 gs=64 shared=1 size=283096 max_qerr=0.0070
```
`shared=1` (output.weight is tied to tok_embeddings → no wcls section). Blob sha256
`f0e50c5e7df3aaad3a96a1e305cf14280ab2b43e1b0e1dc85b4306bf274e2d16`; a second run is
byte-identical (deterministic). `weights_only=True` worked — no unpickling fallback needed.

### Independent verification (separate reader, not the writer)
| check | result |
|---|---|
| crc32 stored vs computed | `0x2693eb7c` == `0x2693eb7c` |
| magic/version | `SBRD` / 1; all 11 header fields sane |
| tokenizer section | 512 tokens, consumed 4691/4691 B, `max_token_len=7` == measured longest; ids 260-266 = `he`, ` a`, ` s`, ` w`, `nd`, ` the`, `ed` |
| size recomputed from header alone | 283096 == file size → **nothing stored twice** |
| dequant vs checkpoint | rms_final max\|err\| = 0.0; tok_emb = 0.0049 |
| efficiency | 259328 weights, 4052 scales, **8.50 bits/weight** |

### ⚠️ Group-size deviation (deliberate) — GS=64, not the plan's rule
The plan said halve GS until it divides the in-dims (`dim` AND `hidden`). `hidden_dim=172`
→ gcd(64,172)=**4** → one f32 scale per 4 int8 weights = **16 bits/weight, 528 KB blob**,
which trips the plan's own >400 KB STOP and contradicts its expected `gs=64 size≈300000`.
Groups actually run over each matrix's **flattened** data, so the real constraint is
`numel % GS == 0` per matrix — upstream's actual rule (`export.py:210`; its backoff at
`:193` tests `dim`, not hidden, despite the printed "hidden_dim" message). All matrices
pass at GS=64 (w1/w2/w3 numel = 64*172 = 11008, divisible by 64). Kept GS=64 → 283 KB.
Reversible: re-running the script with the other rule is ~1 s.

### Consequence for T5 (must-read)
With GS=64 and hidden=172, a weight group **straddles row boundaries** in w2 ([64,172];
172 = 2*64 + 44). The forward pass must index scales by flattened position
`s[(row*in + k)/GS]` and flush the i32 accumulator whenever the weight- **or**
activation-group index changes. Activation quantization for a 172-long vector needs
ceil-division with a ragged final group of 44. Groups never straddle *matrices*
(11008 % 64 == 0), only rows.

### 🚩 Consequence for T6 (blocker) — upstream runq.c is NOT a valid golden reference
For this checkpoint runq.c's q8 path is simply wrong:
- `runq.c:332` `for (j = 0; j <= n - GS; j += GS)` — with n=172, GS=64 it covers j=0,64 only,
  **silently dropping in-dim elements 128..171** of every w2 row.
- `runq.c:146` `num_groups = n / GS` → 2 groups for a 172-long activation, leaving
  `q[128..171]` unquantized/uninitialized.
Our (correct) implementation will therefore DISAGREE with runq.c by construction. T6 needs a
different baseline: the fp32 export path (`run.c`, v0) or a Python reference forward pass.
Recommend the Python reference — same venv, no C build, and it can assert against the exact
dequant semantics the Rust must implement.

## Environment notes
- venv: `scratch/bard/venv` (git-ignored), torch **2.13.0+cpu**; **numpy had to be added**
  (`pip install numpy`) — the CPU wheel ships without it and the script's `.numpy().tobytes()`
  needs it. Worth adding to the plan's venv step.
- No regression: `--features hw` release build OK, `--features hostsim --lib` test green.
- Working tree clean; no venv/upstream bytes leaked into git.
- `.gitignore:3` `*.bin` would have swallowed the deliverable → re-included via
  `!rust/clock/model/*.bin` (repo's existing exclude-then-negate convention). Proof: it
  staged without `-f`.
