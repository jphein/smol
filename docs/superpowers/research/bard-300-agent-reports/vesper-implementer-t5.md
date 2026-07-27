# Bard #300 — Task 5 (int8 forward pass + quantized KV cache) — vesper-implementer

Commit **b3ca0cf** `feat(bard): #300 int8 forward pass with quantized KV cache`
(rider was already committed earlier as **03b9834** — it was done before the "stays queued" note arrived)

Status: **DONE** — 4/4 tests green, and the port demonstrably generates coherent English.

## 🎯 It works — greedy decode proves it
```
from BOS: "Once upon a time, there was a little girl named Lily. She loved to play outside
           in the park. One day, she saw a big, red ball. She wanted to play with it, but
           it was too high"
continuing "Once upon a time, there was a little" -> "girl named Lily. She loved to play
           outside in the park. One day,"
```
Coherent English requires family-grouped addressing + straddling-group matmul + RoPE + GQA
head mapping + int8 KV round-trip + SwiGLU to ALL be right. Any single error → gibberish.
The probe was temporary (removed before commit); committed tests remain the 4 the plan
specifies since generation is T6/T8 scope. **Recommendation: fold a greedy-coherence assertion
into T6/T8** — cheapest possible regression guard for the whole numeric stack.

## Measured facts
- `size_of::<Bufs>()` = **98180 bytes** (~96 KB) — T9 must place this in `.bss`; k/v caches
  dominate (2 × 5 × 256 × 32 = 81920 B) plus 2 × 5 × 256 f32 scales (10240 B).
- Firmware ELF 267032 B (bard code still not linked in until T9).

## Implementation notes
- `QTensor`/`QAct`, `tensor(idx)`, `classifier()`. `classifier()` exists so `tensor(8)` can't
  be hand-picked on a shared-classifier blob (that would read past `qdata`).
- **No `unsafe`**: blob weights stay `&[u8]`, read via `i8at()` (`u8 as i8 as i32`). Deviation
  from the spec sketch's `q: &'a [i8]`, which would have needed a raw-pointer slice
  reinterpretation. Bonus: routing every weight read through the helper makes a forgotten sign
  extension impossible.
- `f32at(b, i)` takes an **element** index (`rf32` is the byte-level primitive). The spec's
  pseudocode wrote `rf32(w.s, wg)` / `rf32(w_f32le, i)` which would have read byte offset `wg`
  — a ×4 bug at ~10 call sites. Centralised instead.
- `matmul` segment flush: `seg_end = min((wg+1)*gs - row, (ag+1)*gs, n_in)`; provably advances
  (both bounds are > j). Degenerates to one flush per chunk when the in-dim divides gs.
- Parser addition: `kv_dim <= KV_STRIDE` → `DimsTooBig`. Cache row stride is a literal 32, so
  rejecting wider models at parse time keeps `forward()` free of bounds fallbacks.

## Golden contract for T6 (float order matters — do not reassociate)
| step | exact form used |
|---|---|
| activation quantize | `(v / sc + (if v >= 0.0 {0.5} else {-0.5})) as i8` — truncating cast, saturating at ±127 |
| zero rounding | `v == 0.0` takes `+0.5` (matches `signum()` returning 1.0 for 0.0) |
| KV quantize | one scale per vector, `max|v|/127` (1.0 if all-zero), same rounding |
| attention score | `dot * k_scale[ts] * inv_sqrt_hd` in **that** order (`inv_sqrt_hd = 1/sqrtf(hd)` hoisted) |
| V accumulate | `a = att[t] * v_scale[ts]` then `xb[i] += a * v_cache[..] as f32` |
| SwiGLU | `(v / (1.0 + expf(-v))) * gate` — a **divide**, not `v * (1/(1+exp(-v)))` |
| rmsnorm | `inv = 1/sqrtf(ss/n + 1e-5)`, then `w[i] * (x[i] * inv)` |
| RoPE | llama2.c adjacent-pair: `freq = 1/powf(10000, (i % hd)/hd)`, rotate q always, k while `i < kv_dim` |
| matmul scaling | per segment: `acc += ival as f32 * w_scale[wg] * x_scale[ag]` |

## Verification
4/4 tests green (`parses_real_blob`, `rejects_corruption`, `tokenizer_roundtrip`,
`forward_is_deterministic_and_finite`) · `--features hw` build + clippy `-D warnings` clean ·
`--features bard` builds · `src/bard/` + `tests/` clean under hostsim clippy AND rustdoc.
