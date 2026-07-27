# The Bard — on-device tiny-LLM storyteller — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `bard` app on the C3 fleet that generates a TinyStories tale with a real transformer (stories260K, int8, XIP from flash), typewriter-style onto the 72×40 OLED. Issue [#300](https://github.com/jphein/smol/issues/300); approved spec `docs/superpowers/specs/2026-07-26-tiny-llm-story-design.md`.

**Architecture:** A pure-`no_std` inference core (`src/bard/nano_llm.rs` + `tokenizer.rs`) exported through the existing `hostsim` lib target for host `cargo test`; an offline Python export pipeline producing one committed `SBRD` blob (weights+tokenizer+CRC); a `Plugin` app (`src/bard/mod.rs`) registered at the six `app.rs` sites, doing at most one token forward-pass per `update()` call.

**Tech stack:** Rust `no_std` (only new dep: `libm`, optional), embedded-graphics `FONT_5X8`, Python 3 + torch (offline only), reference C `llama2.c` (offline golden baseline only).

**Branch:** `feat/300-bard-tiny-llm` off `main`. Conventional commits. All host tests run as
`cd rust/clock && cargo test --no-default-features --features hostsim --target x86_64-unknown-linux-gnu --lib --test bard`
(referred to below as **HOSTTEST**; `.cargo/config.toml` pins the host linker already).
⚠️ Target scoping works because T3 added `[[bin]] required-features = ["hw"]` to Cargo.toml — cargo skips the firmware bin under hostsim-only builds (integration tests otherwise force-build package binaries). "hostsim compiles NO firmware code" is now cargo-enforced.
⚠️ `clippy --features hostsim` has 3 pre-existing lints (app.rs:43, clock.rs:28, sensors.rs:190) — no gate may expect hostsim-clippy green; the KATANA tiers + `--features bard` are the clippy gates.

**Worker constraints (read first):**
- The `default` tier has **no allocator** — the bard core must be alloc-free (fixed buffers only). Zero `alloc::` anywhere in `src/bard/`.
- Multi-KB buffers go in module-level `static mut … : [u8; N]` reached via `core::ptr::addr_of_mut!` (house idiom, see `src/ota_mesh.rs:676-678`) — **never** fields on the `App` union, **never** big stack locals.
- Every new registration arm carries `#[cfg(feature = "bard")]`. A `--no-default-features --features hw` build must be byte-identical to before (feature-absence = symbol absence).
- Do not touch `partitions-ota.csv`, `board.rs`, or anything under `src/net/` except where a task says so.
- Hardware steps: the première/bench board is **Eldritch Nexus (id8), /dev/ttyACM3, MAC `ac:a7:04:ba:1f:24`** (chip-verified esp32c3/4MB + MAC-matched to the known id8 fleet serial, 2026-07-26; JP plugged it for the first-story demo — show the first on-glass story THERE). **No fallback board without JP's positive confirmation** (ACM0 currently carries id7-as-measurement-DUT — not ours). **Verify identity by MAC before every flash, via `udevadm info --query=property --name=<port> | grep ID_SERIAL_SHORT` (passive — espflash board-info RESETS the target)**; ports float. Allowlist = `ACA704BA1F24` only. Deny always: laundry proxy `E8069065 9FE4`, JP's C6 watch `98A316A72FE4`, Dygma.
- Fresh worktrees: `cp` the gitignored `src/board.rs` + `src/secrets.rs` from the main checkout (`/home/jp/Projects/smol/rust/clock/src/`) or the bin target won't build (done once in this worktree at Task 0).
- Never use bare `git stash`/`git stash pop` — the stash stack is shared repo-wide and holds other sessions' crash WIP. A/B a baseline via file-copy to /tmp + `git checkout -- <paths>`.
- **HOSTTEST is the ONLY compile gate for `src/bard/*` until Task 9** wires `mod bard` into the bin — a green `--features bard` firmware build before T9 does NOT compile that code.

---

## File structure

| Path | Responsibility |
|---|---|
| `tools/bard_fetch_model.sh` | Create | download stories260K.pt + tok512.bin from HF (pinned sha256) into `scratch/bard/` |
| `tools/bard_export_model.py` | Create | .pt + tok512.bin → `rust/clock/model/stories260K-q8.bin` (SBRD v1, int8 Q8-grouped, CRC32) |
| `tools/bard_golden_baseline.sh` | Create | build pinned llama2.c `runq`, emit `rust/clock/src/bard/testdata/golden_ref.txt` |
| `rust/clock/model/stories260K-q8.bin` | Create (committed artifact, ~300KB) | the model blob |
| `rust/clock/src/bard/nano_llm.rs` | Create | SBRD parse, forward pass, sampler, generation state machine — pure core |
| `rust/clock/src/bard/tokenizer.rs` | Create | BPE encode/decode over the blob's table — pure core |
| `rust/clock/src/bard/persona.rs` | Create | per-node protagonist table + prompt builder — pure core |
| `rust/clock/src/bard/mod.rs` | Create | the `Plugin` app: states, typewriter render, buttons |
| `rust/clock/src/bard/testdata/*` | Create | golden files |
| `rust/clock/tests/bard.rs` | Create | host integration tests (hostsim target) |
| `rust/clock/src/lib.rs` | Modify | export bard core modules under `hostsim` |
| `rust/clock/src/main.rs:93-197` | Modify | `mod bard;` (cfg-gated) |
| `rust/clock/src/app.rs` | Modify | 6 registration sites + `plugin_bit`/wire arms |
| `rust/clock/Cargo.toml` | Modify | `libm` dep + `bard` feature |
| `README.md`, spec doc | Modify | app table row; bench-numbers amendment |

Everything in `src/bard/` except `mod.rs` must compile with no `esp-hal`/display imports — that is what makes HOSTTEST possible.

---

### Task 0: Branch + feature scaffolding

**Files:** Modify `rust/clock/Cargo.toml`, `rust/clock/src/lib.rs`, Create `rust/clock/src/bard/nano_llm.rs` (stub)

- [ ] **Step 0.1:** `git checkout -b feat/300-bard-tiny-llm` (from `main`).
- [ ] **Step 0.2:** In `Cargo.toml` `[dependencies]` (near `heapless`/small deps):

```toml
# bard (#300): float intrinsics (expf/cosf/sinf) for the no-FPU RV32IMC — pure Rust, no_std
libm = { version = "0.2", optional = true, default-features = false }
```

and in `[features]` after the `cast` block, following the `wled`/`cast` comment style:

```toml
# ── bard (#300): on-device tiny-LLM storyteller ─────────────────────────────
# Radio-free: rides `hw` only, so it can ship in every tier incl. `default`.
# Pulls no alloc — the core is static-buffer only (spec §5).
bard = ["hw", "dep:libm"]
```

and make `hostsim` pull the float lib too: `hostsim = ["dep:libm"]`.
- [ ] **Step 0.3:** Create `rust/clock/src/bard/nano_llm.rs` containing only `//! bard #300 core (populated by later tasks)` and register the **core** modules in `src/lib.rs` beside the other pure cores (`pub mod snake;` block):

```rust
#[cfg(feature = "hostsim")]
#[path = "bard/nano_llm.rs"]
pub mod nano_llm;
```

- [ ] **Step 0.4:** Verify both worlds still build:
  - `cargo build --release --no-default-features --features hw` → OK, and `cargo clippy --release --no-default-features --features hw -- -D warnings` → clean.
  - HOSTTEST → `0 passed` (no tests yet), compiles.
- [ ] **Step 0.5:** Commit: `feat(bard): #300 scaffolding — bard feature + libm + hostsim core export`

---

### Task 1: Model fetch script (pinned artifacts)

**Files:** Create `tools/bard_fetch_model.sh`

- [ ] **Step 1.1:** Write `tools/bard_fetch_model.sh`:

```bash
#!/usr/bin/env bash
# bard (#300): fetch the stories260K checkpoint + 512-token tokenizer (MIT, karpathy/tinyllamas).
# Artifacts land in scratch/bard/ (git-ignored). Pinned by sha256 — a drifted upstream FAILS.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p scratch/bard
BASE=https://huggingface.co/karpathy/tinyllamas/resolve/main/stories260K
# Baked-in pins. First-run bootstrap: run with SMOL_BARD_PIN=print to get values, then bake them.
SHA_PT="__FILL_ON_FIRST_RUN__"
SHA_TOK="__FILL_ON_FIRST_RUN__"
fetch() { # $1 file  $2 sha
  local f="scratch/bard/$1"
  [ -f "$f" ] || curl -fL --retry 3 -o "$f" "$BASE/$1"
  local got; got=$(sha256sum "$f" | cut -d' ' -f1)
  if [ "${SMOL_BARD_PIN:-}" = "print" ]; then echo "$1 sha256=$got"; return; fi
  [ "$got" = "$2" ] || { echo "PIN MISMATCH $1: got $got want $2" >&2; exit 1; }
}
fetch stories260K.pt "$SHA_PT"
fetch tok512.bin     "$SHA_TOK"
echo "bard model artifacts OK in scratch/bard/"
```

- [ ] **Step 1.2:** `chmod +x tools/bard_fetch_model.sh && SMOL_BARD_PIN=print tools/bard_fetch_model.sh` — expect two `sha256=` lines. Bake the printed values into `SHA_PT`/`SHA_TOK`, re-run without the env var, expect `bard model artifacts OK`.
- [ ] **Step 1.3:** Confirm `scratch/` is git-ignored (`git check-ignore scratch/bard/stories260K.pt` → path echoed).
- [ ] **Step 1.4:** Commit: `feat(bard): #300 pinned fetch script for stories260K artifacts`

---

### Task 2: SBRD export pipeline + the committed blob

**Files:** Create `tools/bard_export_model.py`, `rust/clock/model/stories260K-q8.bin`

**SBRD v1 format (byte-exact, little-endian throughout):**

```
u32 magic = b"SBRD" (0x44524253)     u32 version = 1
u32 dim, hidden_dim, n_layers, n_heads, n_kv_heads, vocab_size, seq_len
u32 group_size (GS)                  u32 shared_classifier (0|1)
u32 tok_bytes
tokenizer section (tok_bytes long, then zero-padded to 4-byte alignment):
    u32 max_token_len
    vocab_size × { f32 score; u8 len; u8 bytes[len] }   (packed)
f32 norm section: rms_att[n_layers][dim] · rms_ffn[n_layers][dim] · rms_final[dim]
q8 tensors, each = i8 data[n] then f32 scales[n/GS], in this exact order:
    tok_emb[vocab×dim]
    wq[n_layers×dim×dim]     wk[n_layers×kv_dim×dim]   wv[n_layers×kv_dim×dim]
    wo[n_layers×dim×dim]
    w1[n_layers×hidden×dim]  w2[n_layers×dim×hidden]   w3[n_layers×hidden×dim]
    (wcls[vocab×dim] only if shared_classifier == 0)
u32 crc32 of every preceding byte
```
`kv_dim = dim * n_kv_heads / n_heads`. Row-major `out×in` per layer, matching llama2.c `runq.c`. Quantization = symmetric int8 per GS-group: `scale = max|w|/127`, `q = round(w/scale)`. GS starts at 64 and halves until every tensor's in-dim divides it (mirrors llama2.c `export.py`; with hidden_dim 192 it stays 64).

- [ ] **Step 2.1:** Write `tools/bard_export_model.py` (stdlib + torch only):

```python
#!/usr/bin/env python3
"""bard (#300): stories260K.pt + tok512.bin -> rust/clock/model/stories260K-q8.bin (SBRD v1)."""
import struct, sys, zlib, pathlib
import torch

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "scratch/bard"
OUT = ROOT / "rust/clock/model/stories260K-q8.bin"

def q8(t, gs):
    w = t.detach().float().reshape(-1)
    assert w.numel() % gs == 0, (t.shape, gs)
    g = w.reshape(-1, gs)
    scale = g.abs().max(dim=1).values / 127.0
    scale = torch.where(scale == 0, torch.ones_like(scale), scale)
    q = torch.round(g / scale[:, None]).clamp(-127, 127).to(torch.int8)
    err = (q.float() * scale[:, None] - g).abs().max().item()
    return q.reshape(-1).numpy().tobytes(), scale.float().numpy().tobytes(), err

# weights_only: the .pt is sha256-pinned, but never unpickle arbitrary objects anyway
ckpt = torch.load(SRC / "stories260K.pt", map_location="cpu", weights_only=True)
a, sd = ckpt["model_args"], ckpt["model"]
sd = {k.removeprefix("_orig_mod."): v for k, v in sd.items()}
dim, nl, nh = a["dim"], a["n_layers"], a["n_heads"]
nkv, vocab, seq = a["n_kv_heads"], a["vocab_size"], a["max_seq_len"]
hidden = sd["layers.0.feed_forward.w1.weight"].shape[0]
shared = 1 if torch.equal(sd["output.weight"], sd["tok_embeddings.weight"]) else 0

gs = 64
dims_in = [dim, hidden]          # every matmul's in-dim is one of these
while any(d % gs for d in dims_in) or (vocab * dim) % gs:
    gs //= 2
    assert gs >= 4, "no workable group size"

tok = (SRC / "tok512.bin").read_bytes()  # llama2.c format: u32 max_len, then {f32,i32 len,bytes}*
# repack: drop the i32 len to u8 (max_token_len < 256), keep score
p, out_tok = 4, [tok[0:4]]
for _ in range(vocab):
    score = tok[p:p+4]; ln = struct.unpack_from("<i", tok, p+4)[0]
    b = tok[p+8:p+8+ln]; p += 8 + ln
    assert ln < 256
    out_tok += [score, struct.pack("<B", ln), b]
tok_sec = b"".join(out_tok)

hdr = struct.pack("<11I", 0x44524253, 1, dim, hidden, nl, nh, nkv, vocab, seq, gs, shared)
body = [hdr, struct.pack("<I", len(tok_sec)), tok_sec, b"\0" * (-len(tok_sec) % 4)]

def norms(names):
    for n in names:
        body.append(sd[n].detach().float().numpy().tobytes())
norms([f"layers.{i}.attention_norm.weight" for i in range(nl)])
norms([f"layers.{i}.ffn_norm.weight" for i in range(nl)])
norms(["norm.weight"])

def qtensor(mats):
    qs, ss, werr = [], [], 0.0
    for m in mats:
        q, s, e = q8(m, gs); qs.append(q); ss.append(s); werr = max(werr, e)
    body.append(b"".join(qs)); body.append(b"".join(ss)); return werr
errs = {}
errs["emb"] = qtensor([sd["tok_embeddings.weight"]])
for name, key in [("wq", "attention.wq"), ("wk", "attention.wk"), ("wv", "attention.wv"),
                  ("wo", "attention.wo"), ("w1", "feed_forward.w1"),
                  ("w2", "feed_forward.w2"), ("w3", "feed_forward.w3")]:
    errs[name] = qtensor([sd[f"layers.{i}.{key}.weight"] for i in range(nl)])
if not shared:
    errs["wcls"] = qtensor([sd["output.weight"]])

blob = b"".join(body)
blob += struct.pack("<I", zlib.crc32(blob))
OUT.parent.mkdir(parents=True, exist_ok=True)
OUT.write_bytes(blob)
print(f"dim={dim} hidden={hidden} layers={nl} heads={nh}/{nkv} vocab={vocab} "
      f"seq={seq} gs={gs} shared={shared} size={len(blob)} max_qerr={max(errs.values()):.4f}")
```

- [ ] **Step 2.2:** Run it (venv with torch — on katana `python3 -m venv scratch/bard/venv && scratch/bard/venv/bin/pip install torch --index-url https://download.pytorch.org/whl/cpu` then `scratch/bard/venv/bin/python tools/bard_export_model.py`). Expected output line: `dim=64 hidden=… layers=5 heads=8/4 vocab=512 seq=512 gs=64 shared=… size=~300000 max_qerr=<0.05`. **Record the printed line in the commit message.** If `size` > 400_000, stop and re-check (something is stored double).
- [ ] **Step 2.3:** Commit blob + script: `feat(bard): #300 SBRD v1 export pipeline + stories260K-q8 blob (MIT, karpathy/tinyllamas)` — include upstream shas + the export stats line in the body.

---

### Task 3: Blob parser with CRC + header validation (TDD)

**Files:** Modify `rust/clock/src/bard/nano_llm.rs`, Create `rust/clock/tests/bard.rs`

- [ ] **Step 3.1:** Write the failing test in `rust/clock/tests/bard.rs`:

```rust
#![cfg(feature = "hostsim")]
use clock::nano_llm::{Model, ParseErr};

pub const BLOB: &[u8] = include_bytes!("../model/stories260K-q8.bin");

#[test]
fn parses_real_blob() {
    let m = Model::parse(BLOB).expect("blob parses");
    assert_eq!(m.cfg.dim, 64);
    assert_eq!(m.cfg.n_layers, 5);
    assert_eq!(m.cfg.vocab, 512);
    assert_eq!(m.cfg.n_heads, 8);
    assert_eq!(m.cfg.n_kv_heads, 4);
}

#[test]
fn rejects_corruption() {
    let mut bad = BLOB.to_vec();
    let mid = bad.len() / 2;
    bad[mid] ^= 0xFF;
    assert!(matches!(Model::parse(&bad), Err(ParseErr::Crc)));
    assert!(matches!(Model::parse(&BLOB[..40]), Err(ParseErr::Truncated)));
}
```

(Check `Cargo.toml` for the lib name — `name = "clock"` under `[lib]` or the package name with `-`→`_`; adjust the `use` accordingly.)
- [ ] **Step 3.2:** HOSTTEST → expect FAIL: `Model` not found.
- [ ] **Step 3.3:** Implement in `nano_llm.rs` — **replacing** the T0 stub `//!` line with the real module doc below (never leave "(populated by later tasks)" behind) — config, CRC (the 8-line table-free reflected CRC-32, poly `0xEDB8_8320`), and section offsets:

```rust
//! bard (#300) — nano_llm: stories260K-class inference core.
//! Pure no_std + libm. No alloc. Weights stay in the memory-mapped blob (XIP).
#![allow(clippy::needless_range_loop)]

/// Compile-time maxima — header dims must fit or parse fails (spec §4).
pub const MAX_DIM: usize = 64;
pub const MAX_HIDDEN: usize = 192;
pub const MAX_LAYERS: usize = 5;
pub const MAX_HEADS: usize = 8;
pub const MAX_VOCAB: usize = 512;
/// Runtime context cap — the KV budget (spec §5). Independent of header seq_len.
pub const SEQ_CAP: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub dim: usize, pub hidden: usize, pub n_layers: usize,
    pub n_heads: usize, pub n_kv_heads: usize, pub vocab: usize,
    pub seq_len: usize, pub gs: usize, pub shared_cls: bool,
}
impl Config {
    pub fn kv_dim(&self) -> usize { self.dim * self.n_kv_heads / self.n_heads }
    pub fn head_dim(&self) -> usize { self.dim / self.n_heads }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseErr { Magic, Version, Crc, Truncated, DimsTooBig }

/// Borrowed views into the blob — nothing copied.
pub struct Model<'a> {
    pub cfg: Config,
    pub tok_table: &'a [u8],           // tokenizer section (parsed by tokenizer.rs)
    norms: &'a [u8],                   // f32 norm section
    qdata: &'a [u8],                   // all q8 tensors (data+scales runs)
}

fn ru32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut c: u32 = !0;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 { c = (c >> 1) ^ (0xEDB8_8320 & (!(c & 1)).wrapping_add(1) & 0xEDB8_8320); }
    }
    !c
}
```

⚠️ the crc bit-trick above is easy to fumble — the plain form is fine and preferred:
```rust
for _ in 0..8 { c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 }; }
```
then `Model::parse`:

```rust
impl<'a> Model<'a> {
    pub fn parse(blob: &'a [u8]) -> Result<Self, ParseErr> {
        if blob.len() < 52 { return Err(ParseErr::Truncated); }
        let (body, tail) = blob.split_at(blob.len() - 4);
        if ru32(tail, 0).ok_or(ParseErr::Truncated)? != crc32(body) { return Err(ParseErr::Crc); }
        if ru32(blob, 0) != Some(0x4452_4253) { return Err(ParseErr::Magic); }
        if ru32(blob, 4) != Some(1) { return Err(ParseErr::Version); }
        let g = |i: usize| ru32(blob, 8 + 4 * i).ok_or(ParseErr::Truncated).map(|v| v as usize);
        let cfg = Config { dim: g(0)?, hidden: g(1)?, n_layers: g(2)?, n_heads: g(3)?,
            n_kv_heads: g(4)?, vocab: g(5)?, seq_len: g(6)?, gs: g(7)?, shared_cls: g(8)? == 1 };
        if cfg.dim > MAX_DIM || cfg.hidden > MAX_HIDDEN || cfg.n_layers > MAX_LAYERS
            || cfg.n_heads > MAX_HEADS || cfg.vocab > MAX_VOCAB
            || cfg.gs < 4 || cfg.gs % 4 != 0 || cfg.dim % cfg.gs != 0 {
            return Err(ParseErr::DimsTooBig);
        }
        let tok_bytes = g(9)?;
        let tok_start = 48;
        let tok_end = tok_start + tok_bytes;
        let norms_start = tok_end + (4 - tok_end % 4) % 4;
        let norms_len = (cfg.n_layers * cfg.dim * 2 + cfg.dim) * 4;
        let q_start = norms_start + norms_len;
        if body.len() < q_start { return Err(ParseErr::Truncated); }
        Ok(Model { cfg,
            tok_table: &blob[tok_start..tok_end],
            norms: &blob[norms_start..norms_start + norms_len],
            qdata: &blob[q_start..body.len()] })
    }
}
```
**Header offset check:** magic(0) version(4) then 9 config u32s at 8..44, `tok_bytes` at 44, tokenizer at 48. The `g` closure indexes from byte 8 — index 9 = offset 44. Verify against the Python writer (11 u32s in `hdr` + 1 for tok_bytes = 48 bytes). Also add a `qdata` **exact-length** check: compute the expected total q8 run length from cfg (per the tensor table in Task 5) and return `Truncated` on mismatch — this is what catches a stale blob after a format change.
- [ ] **Step 3.4:** HOSTTEST → both tests PASS.
- [ ] **Step 3.5:** Commit: `feat(bard): #300 SBRD parser with CRC + dims validation`

---

### Task 4: Tokenizer — decode + BPE encode (TDD)

**Files:** Create `rust/clock/src/bard/tokenizer.rs`, Modify `src/lib.rs` (add `#[path = "bard/tokenizer.rs"] pub mod bard_tokenizer;` beside `nano_llm`), Modify `rust/clock/tests/bard.rs`

- [ ] **Step 4.1:** Failing tests (append to `tests/bard.rs`):

```rust
use clock::bard_tokenizer::Tokenizer;

#[test]
fn tokenizer_roundtrip() {
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut ids = [0u16; 64];
    let n = t.encode("Once upon a time, there was a little dragon", &mut ids);
    assert!(n > 2 && n < 32, "n={n}");
    let mut out = std::string::String::new();
    let mut prev = 1u16; // BOS
    for &id in &ids[..n] {
        out.push_str(core::str::from_utf8(t.decode(prev, id)).unwrap());
        prev = id;
    }
    assert_eq!(out.trim_start(), "Once upon a time, there was a little dragon");
}
```

- [ ] **Step 4.2:** HOSTTEST → FAIL (`bard_tokenizer` missing).
- [ ] **Step 4.3:** Implement `tokenizer.rs` — mirrors llama2.c exactly:

```rust
//! bard (#300) — the 512-entry BPE tokenizer over the SBRD in-blob table. No alloc.
pub struct Tokenizer<'a> { table: &'a [u8], vocab: usize, offsets: [u32; crate::nano_llm::MAX_VOCAB] }
```
(`offsets[i]` = byte offset of entry i, filled once in `new` by walking the packed `{f32 score, u8 len, bytes}` entries; in the hostsim lib the `crate::` paths above are `clock::` — inside the crate use `crate::nano_llm`, matching how `clock.rs` reaches `crate::net::names`.)

Methods (all straight ports of llama2.c `tokenizer.c`):
- `fn entry(&self, id: u16) -> (f32 /*score*/, &'a [u8] /*text*/)`
- `pub fn decode(&self, prev: u16, id: u16) -> &'a [u8]` — entry text; **if `prev == BOS(1)` strip one leading space**; raw-byte tokens `<0xXX>` (ids 3..259) decode to their literal byte via a static 256-entry pieces table (AS BUILT at T4 — matches upstream `decode()`; the model can sample these ids, and dropping them would eat story characters). Consequence for T7/T10: decode may return one byte of a multi-byte UTF-8 sequence — consumers write **bytes**, never assume `&str` per token.
- `pub fn encode(&self, text: &str, out: &mut [u16]) -> usize` —
  1. `out[0] = 1` (BOS); if text non-empty, push the id of the single-space token (`str_lookup(" ")`).
  2. Per byte of text: find the vocab id whose text equals that single byte (linear scan; build a `[u16; 256]` byte→id map once in `new`).
  3. Merge loop: repeatedly find the adjacent pair `(a,b)` whose concatenated text exists in vocab with the **best score**; replace; stop when no merge exists. Concatenation buffer: `[u8; 64]` (max_token_len bounded by table header).
  4. Return token count.

Linear scans over 512 entries are fine (encode runs once per story).
- [ ] **Step 4.4:** HOSTTEST → PASS. If the space-handling assert fails, print the token ids and compare against `runq` (Task 6 script) before "fixing" — the BOS-space convention must match llama2.c, not intuition.
- [ ] **Step 4.5:** Commit: `feat(bard): #300 tok512 BPE encode/decode`

---

### Task 5: Forward pass + int8 KV cache (TDD)

**Files:** Modify `rust/clock/src/bard/nano_llm.rs`, `rust/clock/tests/bard.rs`

- [ ] **Step 5.1:** Failing test:

```rust
use clock::nano_llm::{Bufs, Session};

#[test]
fn forward_is_deterministic_and_finite() {
    let m = Model::parse(BLOB).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let mut s = Session::new();
    let logits1 = s.forward(&m, &mut bufs, 1 /*BOS*/, 0).to_vec();
    let mut s2 = Session::new();
    let logits2 = s2.forward(&m, &mut bufs, 1, 0).to_vec();
    assert_eq!(logits1, logits2);
    assert!(logits1.iter().all(|v| v.is_finite()));
    let spread = logits1.iter().cloned().fold(f32::MIN, f32::max)
               - logits1.iter().cloned().fold(f32::MAX, f32::min);
    assert!(spread > 1.0, "logits look degenerate: spread={spread}");
}
```

- [ ] **Step 5.2:** HOSTTEST → FAIL.
- [ ] **Step 5.3:** Implement. Tensor accessors over `Model::qdata` first — each tensor is located by a running offset computed from cfg **in the Task-2 order**; write one table-driven fn:

```rust
/// One tensor FAMILY: i8 data for ALL layers contiguous, then f32 scales for all layers
/// (SBRD family-grouped layout — NOT llama2.c's per-layer q/s interleave; oracle-verified).
/// Flattened family index / GS therefore indexes `s` directly across layer boundaries.
pub(crate) struct QTensor<'a> { pub q: &'a [i8], pub s: &'a [u8] /* f32 LE scales */ }
impl<'a> Model<'a> {
    /// idx: 0=emb 1=wq 2=wk 3=wv 4=wo 5=w1 6=w2 7=w3 8=wcls
    pub(crate) fn tensor(&self, idx: usize) -> QTensor<'a> { /* offset walk, sizes from cfg */ }
    pub(crate) fn norm_att(&self, l: usize) -> &'a [u8] { /* f32 slice views into self.norms */ }
    // norm_ffn(l), norm_final() likewise
}
```
f32 reads from unaligned LE bytes: `f32::from_le_bytes([...])` helper `rf32(b, i)` — **never** cast pointers (alignment).

Buffers (all f32 unless noted; sizes are compile-time maxima):

```rust
pub struct Bufs {
    pub x: [f32; MAX_DIM], xb: [f32; MAX_DIM], xb2: [f32; MAX_DIM],
    hb: [f32; MAX_HIDDEN], hb2: [f32; MAX_HIDDEN],
    q: [f32; MAX_DIM],
    att: [f32; SEQ_CAP],
    pub logits: [f32; MAX_VOCAB],
    xq: [i8; MAX_HIDDEN], xs: [f32; MAX_HIDDEN / 4],       // quantized activation + scales (gs>=4)
    k_cache: [i8; MAX_LAYERS * SEQ_CAP * 32], v_cache: [i8; MAX_LAYERS * SEQ_CAP * 32],
    k_scale: [f32; MAX_LAYERS * SEQ_CAP], v_scale: [f32; MAX_LAYERS * SEQ_CAP],
}
impl Bufs { pub const INIT: Bufs = /* zeroed */; }
```
(32 = kv_dim maximum; one scale per cached vector.) Total ≈ 92KB — must be all-zero const so it lands in `.bss`, not `.data`.

Kernels:

```rust
fn rmsnorm(out: &mut [f32], x: &[f32], w_f32le: &[u8]) {
    let n = x.len();
    let mut ss = 0f32;
    for v in x { ss += v * v; }
    let inv = 1.0 / libm::sqrtf(ss / n as f32 + 1e-5);
    for i in 0..n { out[i] = rf32(w_f32le, i) * (x[i] * inv); }
}
fn quantize(x: &[f32], gs: usize, q: &mut [i8], s: &mut [f32]) {
    for (g, chunk) in x.chunks(gs).enumerate() {
        let mut m = 0f32;
        for v in chunk { m = if v.abs() > m { v.abs() } else { m }; }
        let sc = if m == 0.0 { 1.0 } else { m / 127.0 };
        s[g] = sc;
        for (j, v) in chunk.iter().enumerate() {
            q[g * gs + j] = (v / sc + if *v >= 0.0 { 0.5 } else { -0.5 }) as i8;
        }
    }
}
/// ⚠️ GS groups run over the FLATTENED weight tensor (T2 export semantics). hidden=172 means
/// w2's rows straddle weight-group boundaries; activation groups are position-aligned with a
/// ragged tail. The i32 accumulator therefore flushes at EVERY weight- OR activation-group edge.
/// For matrices whose in-dim divides GS this degenerates to exactly one flush per GS chunk.
fn matmul(out: &mut [f32], xq: &[i8], xs: &[f32], w: &QTensor, w_off: usize, n_in: usize, n_out: usize, gs: usize) {
    for i in 0..n_out {
        let row = w_off + i * n_in;
        let mut acc = 0f32;
        let mut j = 0usize;
        while j < n_in {
            let wg = (row + j) / gs;              // weight group (flattened index)
            let ag = j / gs;                      // activation group (position index)
            let seg_end = core::cmp::min(core::cmp::min((wg + 1) * gs - row, (ag + 1) * gs), n_in);
            let mut ival: i32 = 0;
            for k in j..seg_end { ival += w.q[row + k] as i32 * xq[k] as i32; }
            acc += ival as f32 * rf32(w.s, wg) * xs[ag];
            j = seg_end;
        }
        out[i] = acc;
    }
}
fn softmax(x: &mut [f32]) {
    let mut mx = f32::MIN; for v in x.iter() { if *v > mx { mx = *v; } }
    let mut sum = 0f32;
    for v in x.iter_mut() { *v = libm::expf(*v - mx); sum += *v; }
    for v in x.iter_mut() { *v /= sum; }
}
```

`Session::forward` — a faithful `runq.c` port with the KV-quantization twist (this is the heart; implement exactly):

```rust
pub struct Session { pub pos: u16 }
impl Session {
    pub fn new() -> Self { Session { pos: 0 } }
    /// One transformer step at `pos` for `token`; fills bufs.logits. Not yet budget-sliced.
    pub fn forward<'m>(&mut self, m: &Model<'m>, b: &mut Bufs, token: u16, pos: usize) -> &[f32] {
        let c = m.cfg; let (dim, hd, kvd, gs) = (c.dim, c.head_dim(), c.kv_dim(), c.gs);
        let kv_mul = c.n_heads / c.n_kv_heads;
        // 1. embed: dequant emb row -> x
        let emb = m.tensor(0);
        for i in 0..dim {
            let idx = token as usize * dim + i;
            b.x[i] = emb.q[idx] as f32 * rf32(emb.s, idx / gs);
        }
        for l in 0..c.n_layers {
            // 2. attention rmsnorm + qkv matmuls
            rmsnorm(&mut b.xb[..dim], &b.x[..dim], m.norm_att(l));
            quantize(&b.xb[..dim], gs, &mut b.xq[..dim], &mut b.xs[..dim / gs]);
            let (wq, wk, wv) = (m.tensor(1), m.tensor(2), m.tensor(3));
            matmul(&mut b.q[..dim], &b.xq, &b.xs, &wq, l * dim * dim, dim, dim, gs);
            let (mut kt, mut vt) = ([0f32; 32], [0f32; 32]);
            matmul(&mut kt[..kvd], &b.xq, &b.xs, &wk, l * kvd * dim, dim, kvd, gs);
            matmul(&mut vt[..kvd], &b.xq, &b.xs, &wv, l * kvd * dim, dim, kvd, gs);
            // 3. RoPE on q (per head) and k (per kv head): rotate adjacent pairs
            for i in (0..dim).step_by(2) {
                let hd_i = i % hd;
                let freq = 1.0 / libm::powf(10000.0, hd_i as f32 / hd as f32);
                let (s, cs) = (libm::sinf(pos as f32 * freq), libm::cosf(pos as f32 * freq));
                let (v0, v1) = (b.q[i], b.q[i + 1]);
                b.q[i] = v0 * cs - v1 * s; b.q[i + 1] = v0 * s + v1 * cs;
                if i < kvd {
                    let (k0, k1) = (kt[i], kt[i + 1]);
                    kt[i] = k0 * cs - k1 * s; kt[i + 1] = k0 * s + k1 * cs;
                }
            }
            // 4. quantize k,v into the cache (one scale per vector)
            let slot = l * SEQ_CAP + pos;
            let (mut km, mut vm) = (0f32, 0f32);
            for i in 0..kvd { km = km.max(kt[i].abs()); vm = vm.max(vt[i].abs()); }
            let (ks, vs) = (if km == 0.0 {1.0} else {km / 127.0}, if vm == 0.0 {1.0} else {vm / 127.0});
            b.k_scale[slot] = ks; b.v_scale[slot] = vs;
            for i in 0..kvd {
                b.k_cache[slot * 32 + i] = (kt[i] / ks + kt[i].signum() * 0.5) as i8;
                b.v_cache[slot * 32 + i] = (vt[i] / vs + vt[i].signum() * 0.5) as i8;
            }
            // 5. attention, head at a time
            for h in 0..c.n_heads {
                let qh = &b.q[h * hd..(h + 1) * hd];
                let kvh = h / kv_mul;
                for t in 0..=pos {
                    let ts = l * SEQ_CAP + t;
                    let mut dot = 0f32;
                    for i in 0..hd {
                        dot += qh[i] * b.k_cache[ts * 32 + kvh * hd + i] as f32 * b.k_scale[ts];
                    }
                    b.att[t] = dot / libm::sqrtf(hd as f32);
                }
                softmax(&mut b.att[..=pos]);
                for i in 0..hd {
                    let mut acc = 0f32;
                    for t in 0..=pos {
                        let ts = l * SEQ_CAP + t;
                        acc += b.att[t] * b.v_cache[ts * 32 + kvh * hd + i] as f32 * b.v_scale[ts];
                    }
                    b.xb[h * hd + i] = acc;
                }
            }
            // 6. wo, residual
            quantize(&b.xb[..dim], gs, &mut b.xq[..dim], &mut b.xs[..dim / gs]);
            matmul(&mut b.xb2[..dim], &b.xq, &b.xs, &m.tensor(4), l * dim * dim, dim, dim, gs);
            for i in 0..dim { b.x[i] += b.xb2[i]; }
            // 7. FFN: rmsnorm -> w1,w3 -> silu(w1)*w3 -> w2 -> residual
            rmsnorm(&mut b.xb[..dim], &b.x[..dim], m.norm_ffn(l));
            quantize(&b.xb[..dim], gs, &mut b.xq[..dim], &mut b.xs[..dim / gs]);
            let hn = c.hidden;
            matmul(&mut b.hb[..hn], &b.xq, &b.xs, &m.tensor(5), l * hn * dim, dim, hn, gs);
            matmul(&mut b.hb2[..hn], &b.xq, &b.xs, &m.tensor(7), l * hn * dim, dim, hn, gs);
            for i in 0..hn {
                let v = b.hb[i];
                b.hb[i] = v / (1.0 + libm::expf(-v)) * b.hb2[i];   // SwiGLU
            }
            quantize(&b.hb[..hn], gs, &mut b.xq[..hn], &mut b.xs[..hn / gs]);
            matmul(&mut b.xb2[..dim], &b.xq, &b.xs, &m.tensor(6), l * dim * hn, hn, dim, gs);
            for i in 0..dim { b.x[i] += b.xb2[i]; }
        }
        // 8. final norm + classifier
        rmsnorm(&mut b.xb[..dim], &b.x[..dim], m.norm_final());
        quantize(&b.xb[..dim], gs, &mut b.xq[..dim], &mut b.xs[..dim / gs]);
        let cls = if c.shared_cls { m.tensor(0) } else { m.tensor(8) };
        matmul(&mut b.logits[..c.vocab], &b.xq, &b.xs, &cls, 0, dim, c.vocab, gs);
        &b.logits[..c.vocab]
    }
}
```
⚠️ Two RoPE conventions exist; llama2.c rotates **adjacent pairs with per-head-position frequency** exactly as above (`head_dim`-relative exponent). If the Task-6 golden disagrees, this is the first place to look. ⚠️ `signum()*0.5` rounding: `0f32.signum()` is `1.0` — harmless (value is 0). ⚠️ `quantize` uses `chunks(gs)` which already yields the ragged 44-element tail for a 172-long vector — iterate `chunk.len()`, never `gs`, inside it; `xs` needs `ceil(n/gs)` valid scales. The activation rounding formula (`trunc(v/sc ± 0.5)` via the `as i8` cast) is part of the golden contract with `bard_reference.py` — change neither side alone.
- [ ] **Step 5.4:** HOSTTEST → PASS.
- [ ] **Step 5.5:** Commit: `feat(bard): #300 int8 forward pass with quantized KV cache`

---

### Task 6: Golden baseline vs an independent Python reference (TDD — the port-correctness gate)

> **Why not upstream runq.c (T2 finding, verified in source):** `runq.c:332` walks `j <= n - GS` so with n=172/GS=64 it silently drops in-dim elements 128..171 of every w2 row, and `runq.c:146` leaves the activation tail unquantized. A correct implementation MUST disagree with runq on this checkpoint. The reference is therefore an independent Python forward pass over the SAME committed blob, mirroring the T5 integer semantics exactly — a genuine cross-implementation check in a different language.

**Files:** Create `tools/bard_reference.py`, `tools/bard_golden_baseline.sh`, `rust/clock/src/bard/testdata/golden_ref.txt`, `rust/clock/src/bard/testdata/golden_tokens.txt`, Modify `tests/bard.rs`

- [ ] **Step 6.1:** Write `tools/bard_reference.py` (numpy only, from the T2 venv). Contract — every numeric detail must mirror T5's Rust **as built** (the exact-form table in the T5 report / `scratch/bard-300/vesper-implementer-t5.md` is normative: SwiGLU as a divide `v/(1+exp(-v))*gate`; score = `dot * k_scale * inv_sqrt_hd` with hoisted inv_sqrt_hd; V-accumulate as `a = att*v_scale` then `+= a*v`; zero rounds +0.5; do not reassociate):
  - Parse SBRD (same layout as T2's writer); all math in **np.float32** (assert dtypes; no float64 accumulators anywhere — `np.float32` scalars, `dtype=np.float32` arrays).
  - Weights: dequantize NOTHING up front — keep q8 + scales; matmul does the segment walk: for each output row, segments bounded by every weight-group edge (flattened index) and activation-group edge (position index); per segment `int32` dot of i8×i8, then `acc += np.float32(ival) * ws[wg] * xs[ag]` in row-major segment order (same order as Rust).
  - Activation quantization: per position-group of GS with ragged tail; `q = trunc(v/s + (0.5 if v>=0 else -0.5))` clamped to ±127 (mirrors Rust's `as i8` truncation — do NOT use np.round, it's banker's rounding).
  - RMSNorm eps 1e-5; RoPE = llama2.c adjacent-pair rotation with head_dim-relative exponent; softmax in float32; KV cache int8 per-vector scales, same trunc rounding.
  - Tokenizer: decode from the blob's table (strip one leading space after BOS); greedy BPE encode identical to Task 4's AS-FIXED semantics — **per-CODEPOINT seeding** (accumulate UTF-8 continuation bytes, look up the whole piece, byte-fallback per byte only on miss), BOS=1, leading-space token, best-score merges. The table has 14 multi-byte tokens (curly quotes, em-dash) — per-byte seeding shreds them irrecoverably (oracle-confirmed divergence).
  - CLI: `bard_reference.py <blob> --temp 0 --steps 200 -i "<prompt>"` → prints `# reference <blob-sha256-short> temp0` on line 1, then prompt+continuation as one story text; `--tokens-out <path>` writes generated token ids one per line. Termination on token 1 or 2 or step cap.
- [ ] **Step 6.2:** Write `tools/bard_golden_baseline.sh`:

```bash
#!/usr/bin/env bash
# bard (#300): golden reference from the independent Python forward pass (see plan Task 6 for
# why upstream runq.c is disqualified for this checkpoint).
set -euo pipefail
cd "$(dirname "$0")/.."
PY=scratch/bard/venv/bin/python
OUT=rust/clock/src/bard/testdata
mkdir -p "$OUT"
"$PY" tools/bard_reference.py rust/clock/model/stories260K-q8.bin \
  --temp 0 --steps 200 -i "Once upon a time, there was a little dragon" \
  --tokens-out "$OUT/golden_tokens.txt" > "$OUT/golden_ref.txt"
echo "golden written: $OUT/golden_ref.txt"
```

Run it; eyeball `golden_ref.txt` — expect a coherent-ish toddler story continuing the prompt (the fp32 model is known-good upstream; if the reference emits garbage, the reference or the blob is wrong — STOP and report rather than committing a garbage golden). Commit script + reference + both testdata files.
- [ ] **Step 6.3:** Failing test (append to `tests/bard.rs`) — note the golden file's line 1 is a `#` comment and the story text includes the prompt; also compare the generated token ids against `golden_tokens.txt` (exact, id-by-id, up to the first 32 — transcendental differences libm-vs-numpy may cause later drift; the 120-char text bar below is the gate, the id comparison is the debugging aid):

```rust
#[test]
fn golden_prefix_matches_reference_runq() {
    let golden = include_str!("../src/bard/testdata/golden_ref.txt");
    let golden_story = golden.lines().skip(1).collect::<Vec<_>>().join("\n");
    let m = Model::parse(BLOB).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = std::boxed::Box::new(Bufs::INIT);
    let mut ids = [0u16; 64];
    let n = t.encode("Once upon a time, there was a little dragon", &mut ids);
    let mut s = Session::new();
    let mut text = std::string::String::new();
    // feed prompt
    for i in 0..n - 1 { s.forward(&m, &mut bufs, ids[i], i); }
    let mut token = ids[n - 1];
    for pos in n - 1..200 {
        let logits = s.forward(&m, &mut bufs, token, pos);
        let mut best = 0usize;
        for (i, &v) in logits.iter().enumerate() { if v > logits[best] { best = i; } }
        let next = best as u16;
        if next == 1 || next == 2 { break; }
        text.push_str(core::str::from_utf8(t.decode(token, next)).unwrap());
        token = next;
    }
    // Spec §11 (amended): int8 KV rounding may diverge from runq's f32 KV in the tail;
    // the bar is a long shared prefix. 120 chars ≈ 30+ tokens of exact agreement.
    let bar = 120.min(golden_story.trim().len());
    assert_eq!(text.trim()[..bar], golden_story.trim()[..bar]);
}
```
(The reference output includes the prompt text — align both sides before comparing so the prefix starts at the same character; note what you did in the commit.)
- [ ] **Step 6.3b:** Also add the cheapest regression guard this numeric stack will get (T5 recommendation): a test that greedy-decodes from bare BOS and asserts the text starts with the known deterministic opening (`"Once upon a time, there was a little girl named Lily"` for this blob) — regenerate alongside the golden files on any blob change.
- [ ] **Step 6.4:** HOSTTEST → this test may FAIL first — that is the point. Debug order: prompt token ids (both sides print them), first divergent token id vs `golden_tokens.txt`, RoPE convention, activation-rounding formula, segment-flush order, BOS-space decode. **Do not weaken the bar below 120 chars without a written analysis in the commit message.** When it passes: PASS.
- [ ] **Step 6.5:** Commit: `test(bard): #300 golden prefix vs independent Python reference — port proven`

---

### Task 7: Sampler + story generator state machine (TDD)

**Files:** Modify `nano_llm.rs`, `tests/bard.rs`

- [ ] **Step 7.1:** Failing tests:

```rust
use clock::nano_llm::{Story, StepOut};

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
            StepOut::Done => break,
        }
        steps += 1;
        assert!(steps < 300, "no termination");
    }
    assert!(text.len() > 80, "story too short: {text}");
    assert!(text.is_ascii());
}

#[test]
fn different_seeds_different_stories() {
    /* same setup; two Story runs with seeds 1 and 2; collect both, assert_ne! */
}
```

- [ ] **Step 7.2:** HOSTTEST → FAIL.
- [ ] **Step 7.3:** Implement in `nano_llm.rs`:

```rust
/// xorshift32 (Snake's proven pattern, src/snake.rs:106-133) — seed from now_ms at press.
pub struct Rng32 { s: u32 }
impl Rng32 {
    pub fn new(seed: u32) -> Self { Self { s: if seed == 0 { 0xA5A5_A5A5 } else { seed } } }
    fn next(&mut self) -> u32 { let mut x = self.s; x ^= x << 13; x ^= x >> 17; x ^= x << 5; self.s = x; x }
    fn unit(&mut self) -> f32 { (self.next() >> 8) as f32 / (1u32 << 24) as f32 }
}

pub enum StepOut<'a> { Working, Text(&'a [u8]), Done }

pub struct Story {
    prompt: [u16; 48], prompt_len: u8, fed: u8,
    pos: u16, cur: u16, prev: u16, rng: Rng32, done: bool,
}
impl Story {
    pub const TEMP: f32 = 0.9;
    pub const TOP_P: f32 = 0.9;
    pub const MAX_TOKENS: u16 = 220;
    pub fn new(t: &Tokenizer, prompt: &str, seed: u32) -> Story { /* encode prompt */ }
    /// ONE forward pass per call (prompt-feeding or generation). ~20-100ms — the tick budget unit.
    pub fn step<'b>(&mut self, m: &Model, t: &Tokenizer<'_>, b: &'b mut Bufs) -> StepOut<'b> { … }
}
fn sample_top_p(logits: &mut [f32], temp: f32, top_p: f32, rng: &mut Rng32,
                idx: &mut [u16; MAX_VOCAB]) -> u16 { … }
```
`step`: while `fed < prompt_len - 1` → forward prompt token, return `Working`. Then forward + `sample_top_p` → decode via `t.decode(prev_actual, next)` and return `Text` (decode returns a blob-borrowed `&[u8]`; return that slice — lifetime from the tokenizer/blob is `'static`-like via the model borrow; if the borrow tangle fights you, copy into a `pub scratch_text: [u8; 16]` on `Bufs` and return `&b.scratch_text[..n]` — hence `StepOut<'b>`). Termination: token 1 or 2, `pos == MAX_TOKENS + prompt_len`, or `pos == SEQ_CAP - 1` → `Done`.
`sample_top_p`: `logits /= temp`, softmax, sort `idx` by prob desc (insertion sort, 512 entries), walk until cumulative ≥ `top_p`, draw `rng.unit() * cum` within that prefix. Temp-0/argmax stays a separate path (used by the golden test only).

**KV-cache ownership (oracle T5 review, Important #2):** the cache in `Bufs` has no owner — two interleaved sessions corrupt each other, and a first `forward` at `pos > 0` reads the previous story's K/V as plausible garbage. `Story` is therefore the ONLY caller of `forward` and guarantees monotonic-from-zero: `Story::new` resets `pos = 0`, and `Session::forward` gains `debug_assert_eq!(pos, self.pos as usize)` + `self.pos += 1` bookkeeping. Never use `pos.min(SEQ_CAP-1)`'s silent plateau as a stop condition — `Story` terminates explicitly at the cap.

**Rider items to fold into the T7 commit** (from oracle's T5/rider review, all one-liners): `const _: () = assert!(core::mem::size_of::<Bufs>() <= 100_000);` (makes the §5 RAM budget compiler-enforced) · softmax max seeded with `f32::NEG_INFINITY` not `f32::MIN` · `debug_assert!(s.len() >= x.len().div_ceil(gs))` inside `quantize` · `.gitignore` line `rust/clock/model/*.tmp` (a killed export currently strands an untracked 277KB tmp one `git add` from being committed).
- [ ] **Step 7.4:** HOSTTEST → PASS.
- [ ] **Step 7.5:** Commit: `feat(bard): #300 top-p sampler + Story state machine`

---

### Task 8: 20-story eyeball batch + host perf number

**Files:** Create `rust/clock/examples/bard_stories.rs`

- [ ] **Step 8.1:** Example (examples build with std against the hostsim lib):

```rust
//! cargo run --example bard_stories --no-default-features --features hostsim --target x86_64-unknown-linux-gnu --release
use clock::nano_llm::*;
use clock::bard_tokenizer::Tokenizer;
fn main() {
    let blob = include_bytes!("../model/stories260K-q8.bin");
    let m = Model::parse(blob).unwrap();
    let t = Tokenizer::new(m.tok_table, m.cfg.vocab).unwrap();
    let mut bufs = Box::new(Bufs::INIT);
    for seed in 1..=20u32 {
        let mut story = Story::new(&t, "Once upon a time, there was a little dragon", seed);
        print!("\n=== seed {seed} ===\nOnce upon a time, there was a little dragon");
        loop {
            match story.step(&m, &t, &mut bufs) {
                StepOut::Text(b) => print!("{}", core::str::from_utf8(b).unwrap()),
                StepOut::Working => {}
                StepOut::Done => break,
            }
        }
        println!();
    }
}
```

- [ ] **Step 8.2:** Run it. Read all 20. Bar (spec §11): charming toddler prose, mostly grammatical English, no garbage bytes, varied across seeds. Paste 2 favourites into the PR description later.
- [ ] **Step 8.3:** Commit: `feat(bard): #300 host story-batch example`

---

### Task 9: Firmware wiring — feature, statics, blob, registration (no UI yet)

**Files:** Modify `rust/clock/src/main.rs`, `rust/clock/src/app.rs`, Create `rust/clock/src/bard/mod.rs`, `rust/clock/src/bard/persona.rs`

- [ ] **Step 9.1:** `src/bard/persona.rs` (pure, also exported to hostsim beside the others):

```rust
//! bard (#300): per-node protagonist — every board tells its own kind of stories (spec §8).
//! Words must be TinyStories-frequent (512-token vocab; realm names would shred — spec §8).
pub const PROTAGONISTS: [&str; 16] = [
    "a little dragon", "a little owl", "a little bird", "a brave cat",
    "a tiny robot", "a little fish", "a small dog", "a little bunny",
    "a happy bear", "a little star", "a small mouse", "a little duck",
    "a small frog", "a kind girl", "a brave boy", "a little pony",
];
/// id7 Draconic Dominion → dragon, id8 Eldritch Nexus → owl, id9 Jade Herald → bird.
pub fn protagonist(node_id: u8) -> &'static str {
    PROTAGONISTS[match node_id { 7 => 0, 8 => 1, 9 => 2, n => (n as usize) % 16 }]
}
/// Fills `buf` with the full prompt; returns the used length.
pub fn prompt(node_id: u8, buf: &mut [u8; 64]) -> usize {
    let mut n = 0;
    for part in ["Once upon a time, there was ", protagonist(node_id)] {
        buf[n..n + part.len()].copy_from_slice(part.as_bytes());
        n += part.len();
    }
    n
}
```
Host test (append to `tests/bard.rs`): `protagonist(7)` contains `"dragon"`; `prompt` output encodes to < 32 tokens for **all 16** personas.
- [ ] **Step 9.2:** `src/bard/mod.rs` — statics + init + a stub Plugin that draws "the bard is mute" / "bard ready" (UI in Task 10):

```rust
//! bard (#300): The Bard — on-device tiny-LLM storyteller. Spec: docs/superpowers/specs/2026-07-26….
pub mod nano_llm;
pub mod persona;
pub mod tokenizer;

use crate::app::{Ctx, Plugin, Press, Transition};
use nano_llm::{Bufs, Model, Story};

/// ~300KB, .rodata → XIP from flash; never copied to RAM (spec §3).
static MODEL_BLOB: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/model/stories260K-q8.bin"));
/// ~92KB .bss — the ONLY big RAM cost (spec §5). House idiom: static mut + addr_of_mut.
static mut BUFS: Bufs = Bufs::INIT;
static mut STORY_TEXT: [u8; 1024] = [0; 1024];
```
Parse once per entry (`Model::parse` is cheap — a CRC walk over flash); on `Err`, set a `mute: true` flag. The `Tokenizer` value is **~2.6KB** (offsets + byte_id tables) — it lives in a module-level `static mut` beside BUFS, never on the stack and never in the App union. NOTE: `lib.rs` reaches the same files via `#[path]` (`bard/nano_llm.rs` etc.) while the bin reaches them as `bard::nano_llm` — keep the internal cross-references as `super::` within `src/bard/`, not `crate::bard::`, so both target roots compile the same source unchanged (`snake_core` precedent).
- [ ] **Step 9.3:** Registration — the six sites plus wire arms, each `#[cfg(feature = "bard")]`, exactly per the Sigil template:
  - `src/main.rs` mod block: `#[cfg(feature = "bard")] mod bard;`
  - `src/app.rs` `AppKind::Bard` (~:184), `App::Bard(crate::bard::BardApp)` (~:371), `enter` arm (~:404), `on_button`/`update` UFCS arms (~:446/:481), REGISTRY row `AppDesc { title: "Bard", kind: AppKind::Bard }` (~:601).
  - `plugin_bit` → `None` arm; `from_wire`/`as_wire`/`live_screen`: next unused wire id — read the existing `as_wire` arms and take highest+1; the cfg combination must keep `as_wire` **total** (`#[cfg(all(feature = "espnow", feature = "bard"))]` arm plus the model's existing fallback pattern — copy how another hw-tier-optional app handles it; if none exists, gate Bard's wire arms `espnow+bard` and give `from_wire` a graceful `None`).
- [ ] **Step 9.3b: Bard joins the canonical fleet image.** `tools/repro_build.sh:109` hardcodes the shipped feature set (`cargo build --release --features espnow,cast,io`) and `ota_publish.sh` builds through it — without this step, T14 would canary a **Bard-less** image. Change the list to `espnow,cast,io,bard` (both the build line and any echo/doc of the list in that script). ⚠️ This forks the #44 reproducible-image sha lineage — say so explicitly in the commit message (`feat(bard): #300 add bard to the canonical fleet image (repro lineage change, #44)`) and flag it in the PR body for JP's visibility.
- [ ] **Step 9.3c:** Prove libm actually resolves inside the riscv32imc **bin** (T0 only proved it links unused): after `mod bard` is wired, `CARGO_TARGET_DIR=… cargo build --release --features espnow,cast,io,bard` must succeed, and a libm-backed symbol must be reachable — e.g. `nm` the ELF (`riscv32-esp-elf-nm` or llvm-nm) and confirm an `expf`/`sinf` reference resolved (statically inlined is fine — the gate is the successful link of code that calls `libm::expf`).
- [ ] **Step 9.4:** Build gates:
  - `cargo build --release --features bard` → OK. Record `.bss` delta via **loadable-section sizes** (`riscv32-esp-elf-size`/llvm `size`, or readelf section headers) — never `stat` on the ELF file (non-loadable `.strtab` noise; oracle-verified at T5) — **with and without** `--features bard`; expect ≈ +96KB bss (Bufs measured 98180B + Tokenizer ~2.6KB + story buf), ≈ +310KB flash. **If total `.data`+`.bss` approaches the 313KB DRAM window minus stack (see `src/net.rs:210-216`), drop `SEQ_CAP` to 192 and note it in the spec amendment.**
  - `cargo build --release --no-default-features --features hw` then `cargo clippy` all four KATANA tiers + `--features espnow,bard` → clean.
  - HOSTTEST → still green.
- [ ] **Step 9.5:** Commit: `feat(bard): #300 firmware wiring — feature, statics, registration (stub UI)`

---

### Task 10: The Bard UI — typewriter, quill, buttons

**Files:** Modify `rust/clock/src/bard/mod.rs`

- [ ] **Step 10.1:** Implement `BardApp` per spec §9. State:

```rust
pub struct BardApp {
    phase: Phase,            // Idle | Composing | Told | Mute
    text_len: u16,           // bytes valid in STORY_TEXT
    shown: u16,              // typewriter reveal cursor
    next_reveal_ms: u64, next_token_ms: u64, last_paint_ms: u64,
    story: Option<Story>,    // (sized fine for the App union: prompt array ≈ 110B)
}
```
Behavior in `update(ctx)`:
- `Composing`: if `ctx.now_ms >= next_token_ms` → **one** `Story::step` (via `addr_of_mut!` statics), append `Text` bytes into `STORY_TEXT` (cap 1024 → force Done), set `next_token_ms = now` (token pacing is free-running; the *reveal* is what's throttled).
- Reveal: while `shown < text_len && now >= next_reveal_ms` → `shown += 1; next_reveal_ms = now + 160;` (~6 chars/s, spec §7).
- Repaint when `ctx.redraw || shown advanced || quill blink phase changed (400ms)`, owning clear→draw→flush like Sigil.
- Render: `FONT_5X8`, 14 cols × 5 rows. Word-wrap `STORY_TEXT[..shown]` (greedy break at spaces, hard-break 14), keep the **last 5** lines (single back-to-front scan — no line ring buffer). While `Composing`, draw a `'|'`/`' '` alternating quill at the cursor. On `Told`, last line gets `~ fin ~` centered. If the story ended by token-cap rather than natural EOS (a real path — the temp-0 golden runs to the cap mid-sentence; `Story` reports it via `Done { truncated }`), append `…` at the cut first so the hard stop reads as intentional trailing-off.
- `on_button`: `Press::Long` → `Transition::Switch(AppKind::Menu)`. `Press::Short`: `Idle|Told` → start a new story (`Story::new(tok, prompt(crate::node_id()), seed = ctx.now_ms as u32)`); `Composing` → drop the reveal throttle (`shown = text_len` each frame) so it finishes fast (spec §9). `Mute` → ignore short.
- All text drawing through the house heap-free pattern (`Line`/`Buf` builders, `src/bench.rs:58-85`) — zero `alloc`.
- [ ] **Step 10.2:** Host-verify the wrap logic: put `fn wrap_tail<'a>(text: &'a [u8], cols: usize, rows: usize, out: &mut [(u16, u16)]) -> usize` (returns line spans) in `nano_llm.rs`-adjacent pure code (`mod.rs` is bin-only — put it in `persona.rs` or a small `textflow.rs` exported to hostsim) + unit tests: empty, exact-14, long-word, 6-line scroll.
- [ ] **Step 10.3:** Build `--features bard` + clippy tiers + HOSTTEST → green.
- [ ] **Step 10.4:** Commit: `feat(bard): #300 typewriter UI — compose, reveal, quill, fin`

---

### Task 11: Serial perf instrumentation

**Files:** Modify `rust/clock/src/bard/mod.rs`

- [ ] **Step 11.1:** Around the `Story::step` call, measure `ctx.now_ms` before/after (ms resolution is enough at 20-100ms/token); keep `tok_count: u16, tok_ms_sum: u32, tok_ms_max: u16` on `BardApp`. On `Told`, one line (serial-only convention, `docs` §8 of the explorer notes — log is compile-time gated, so this is DIAG-build only by nature):

```rust
log::info!("smol #300: bard story done — {} tok, avg {} ms/tok, max {} ms",
    self.tok_count, self.tok_ms_sum / self.tok_count.max(1) as u32, self.tok_ms_max);
```
- [ ] **Step 11.2:** Build + clippy gates → green. Commit: `feat(bard): #300 tok/s serial instrumentation`

---

### Task 12: PR + review

- [ ] **Step 12.1:** Push branch; `gh pr create` titled `feat: The Bard — on-device tiny-LLM storyteller (#300)`; body: spec+plan links, the Task-9 size numbers, 2 sample stories from Task 8, HOSTTEST output. (Note: `gh pr edit` is broken on this repo — set everything at create time.)
- [ ] **Step 12.2:** Request review per superpowers:requesting-code-review. Fix findings; keep golden test green.

---

### Task 13: Bench-board validation (hardware — Eldritch Nexus id8, see Worker constraints for port/MAC)

- [ ] **Step 13.0:** `espflash board-info` on the target port — proceed ONLY if the MAC is `ac:a7:04:ba:1f:24` (id8) or `ACM0`'s known Nexus identity. Any other MAC = STOP.
- [ ] **Step 13.1:** Flash the bench board over USB: `cargo build --release --features espnow,bard` → `espflash flash` per `docs/BUILDING.md`. ⚠️ After ANY prior OTA on this board: `espflash erase-region 0xf000 0x2000` first (otadata → ota_0 trap), then verify the `Loaded app from offset 0x20000` boot line.
- [ ] **Step 13.2:** With `ESP_LOG=info` build (release images are serial-silent), on-glass run: menu → Bard → short press. Verify: story composes typewriter-style, mesh LED stays in its normal state (peer solid), Familiar/roster unaffected on a second board, long-press exits cleanly mid-compose.
- [ ] **Step 13.3:** Capture the Task-11 perf line for 3 stories. **Record avg/max ms/token + observed mesh health as an AMENDMENT block in the spec** (house style), including the go/no-go on the §7 per-layer-yield fallback (needed only if max-stall visibly degrades mesh RX — check a leaf's DIAG link-quality while composing).
- [ ] **Step 13.4:** Commit amendment: `docs(bard): #300 on-glass bench numbers`

---

### Task 14: Merge + canary + fleet roll

- [ ] **Step 14.1:** Merge the PR (squash per repo habit; no-eager-merge rules apply if other PRs are in flight).
- [ ] **Step 14.2:** OTA canary per `docs/ota.md`: `tools/ota_publish.sh stage` → install on **one** canary board → on-glass Bard story + 24h-soak-free sanity (boot loop, DIAG clean, mesh healthy) → then the staged fleet roll. (`PATH=~/.cargo/bin`, `BW_SESSION` for signing; roll from a stable crown.)
- [ ] **Step 14.3:** README app-table row: `| **The Bard** | a tiny LLM (260K-param TinyStories transformer, int8, XIP) that writes a fresh story on demand — fully on-device | 🟢 on glass (#300) |` + tick the #300 checkboxes; close #300 with the bench numbers.

---

## Self-review checklist (run after writing, fixed inline)

- Spec coverage: §3 modules→Tasks 3-10 · §4 pipeline→Tasks 1-2 · §5 budget→Task 9.4 gate · §6 core→Task 5 · §7 SM→Tasks 7/10 · §8 personas→Task 9.1 · §9 UX→Task 10 · §10 errors→Tasks 3/9.2 (mute path) · §11 tests→Tasks 6/8/11/13 · §12 out-of-scope untouched · §13 risks→9.4 (RAM), 13.3 (latency), 6 (quality) ✓
- No unresolved placeholders; the two "resolve at implementation" notes (lib name, wire-id) are verifiable instructions with acceptance criteria, not TBDs ✓
- Type consistency: `Model/Bufs/Session/Story/StepOut/Tokenizer/Rng32` names and signatures match across Tasks 3-10 ✓ (`Story::step` takes `&Tokenizer` — Task 7 & 10 agree)
