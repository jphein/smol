# Bard #300 — Tasks 3 & 4 (SBRD parser + tok512 BPE) — vesper-implementer

Worktree: `/home/jp/Projects/smol/.claude/worktrees/bard-300` · branch `feat/300-bard-tiny-llm`
- **d708512** `feat(bard): #300 SBRD parser with CRC + dims validation`
- **d534a7f** `feat(bard): #300 tok512 BPE encode/decode`

Status: **DONE_WITH_CONCERNS** — TDD red→green both tasks, 3/3 tests green; one required
Cargo.toml addition outside the task's file list, two small deliberate calls in the tokenizer.

## HOSTTEST needed one more fix: `required-features` on the bin
`--lib --test bard` still failed on `src/main.rs` (~14 unresolved imports). Cause: **cargo
builds a package's BINARIES alongside any INTEGRATION test** (they get `CARGO_BIN_EXE_*`).
Measured, not guessed: `--lib` alone → 0 main.rs errors; `--test bard` → 14.
Fix in `rust/clock/Cargo.toml`:
```toml
[[bin]]
name = "clock"
path = "src/main.rs"
required-features = ["hw"]
```
Cargo now SKIPS the bin under a hostsim-only build — which also makes "hostsim compiles NO
firmware code" an invariant cargo enforces rather than a convention. Every firmware tier
implies `hw`, so firmware builds are unchanged (same 267000-byte ELF).

## T3 — `src/bard/nano_llm.rs` (stub doc replaced with the real module doc)
`Model::parse` → borrowed views (`tok_table`/`norms`/`qdata`), nothing copied: a 277 KB model
costs 277 KB flash + ~0 RAM. Validation order length → crc32 → magic → version → geometry, so
nothing is trusted before the integrity check covering it. Extras beyond the letter of the spec:
- `parses_real_blob` **is** the zlib cross-check — parse only succeeds if the table-free
  reflected crc32 reproduced the blob's trailing `0x2693eb7c`.
- q-section length recomputed from the header alone and compared EXACTLY (a header/payload
  disagreement fails instead of reading one tensor family's bytes as another's).
- `checked_add` on the attacker-controlled `tok_bytes`; geometry bounded BEFORE the size
  arithmetic, which is what keeps that arithmetic overflow-free.
- `rf32`/`ru32` via `from_le_bytes` — the blob's f32s are 4-aligned within their section but
  never to the mapped flash address, so a pointer cast would be UB.

## T4 — `src/bard/tokenizer.rs` (exported as `clock::bard_tokenizer`)
### What the shipped table actually contains (measured)
512 entries · longest token **7** bytes · **88 single-char tokens**
``` !"$%&'()*+,-./0-9:;<>?A-Z[\]`a-z|~``  → uppercase covered, every char of the test
sentence has its own token. ids 0..2 = `<unk>`, `\n<s>\n`, `\n</s>\n`; ids 3..259 = byte
fallback whose text is the literal STRING `<0xXX>`, not a raw byte.

### Encode matches llama2.c bit-for-bit
Verified against an independent Python simulation of the upstream algorithm — identical
`n=15` and identical ids:
```
[1, 403, 407, 261, 378, 432, 383, 286, 261, 376, 279, 420, 412, 428, 289]
 BOS ' Once' ' upon' ' a' ' time' ',' ' there' ' was' ' a' ' little' ' d' 'r' 'a' 'g' 'on'
```
("dragon" has no whole-word token in a 512-vocab.) Dummy prefix is a lookup of the one-space
STRING (not `byte_id[' ']`), decode strips ONE leading space after BOS.

### Two deliberate calls beyond the task text
1. **`<0xXX>` decodes to its byte** (static 256-entry pieces table, still zero alloc) instead
   of returning empty. Encode never needs it for ASCII, but the MODEL can sample such an id at
   inference and returning empty would silently eat story characters. Matches llama2.c decode().
   ⚠️ Consequence for T10: `decode` can return a single byte of a multi-byte UTF-8 sequence, so
   a caller needing `&str` must reassemble; the display path writes bytes, so it doesn't.
2. **`max_token_len` is MEASURED** during the table walk, not read from the table's leading
   word (advisory) — a too-small declared value would silently suppress valid merges. The
   merge loop skips pairs longer than it, which bounds the concat buffer without a runtime check.

## Verification
| gate | result |
|---|---|
| `--lib --test bard` (hostsim) | **3/3 green**: parses_real_blob, rejects_corruption, tokenizer_roundtrip |
| clippy hostsim, my files | clean (fixed 4 `is_multiple_of` lints in my code) |
| clippy hostsim, pre-existing | 3 lints in `app.rs`/`clock.rs`/`sensors.rs` — untouched, out of scope (this is the plan's "hostsim-clippy caveat") |
| `--features hw` build + clippy -D warnings | clean, 267000-byte ELF |
| `--features bard` build | clean (still compiles no bard code until T9) |

## Notes for T5
- `rf32` is `pub` in `nano_llm` for the forward pass to reuse; `SEQ_CAP=256`, `MAX_HIDDEN=192`.
- `Config::kv_dim()` = 32, `head_dim()` = 8 for the shipped model.
- Parse guarantees `gs` divides `dim` (so groups never straddle a MATRIX), but NOT `hidden` —
  w2's rows still straddle, per T2's finding. The parser deliberately does not pretend otherwise.
