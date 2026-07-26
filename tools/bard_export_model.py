#!/usr/bin/env python3
"""bard (#300): stories260K.pt + tok512.bin -> rust/clock/model/stories260K-q8.bin (SBRD v1).

Inputs come from tools/bard_fetch_model.sh (sha256-pinned, MIT, karpathy/tinyllamas).
Run inside the CPU-torch venv:  scratch/bard/venv/bin/python tools/bard_export_model.py

GROUP SIZE — read before changing (this bit is subtle):
  Quantization groups run over each matrix's FLATTENED (row-major) data, exactly like
  llama2.c's `quantize_q80` + runq.c's on-disk q8 layout. The only hard requirement is
  therefore `numel % GS == 0` per matrix, which is what upstream export.py asserts
  (export.py:210); its backoff loop tests `dim % GS` only (export.py:193 — the printed
  message says "hidden_dim" but the code reads `dim`).

  For stories260K that yields GS=64, and hidden_dim=172 is NOT a multiple of 64. So for
  w2 (shape [dim, hidden] = [64, 172]) a 64-weight group STRADDLES row boundaries:
  172 = 2*64 + 44. That is fine on disk and fine for an exact dequant, but the CONSUMER
  must index scales by FLATTENED position — `s[(row*in + k) / GS]` — and flush its i32
  accumulator whenever the weight-group OR activation-group index changes. It must NOT
  assume one scale per row-chunk.

  ⚠️ Upstream runq.c does NOT do this: `for (j = 0; j <= n - GS; j += GS)` (runq.c:332)
  walks the in-dim in whole GS strides, so with n=172 it silently drops elements 128..171,
  and `num_groups = n / GS` (runq.c:146) under-quantizes the activation the same way.
  runq.c's q8 path is thus NOT a valid golden reference for this checkpoint — use the fp32
  export (run.c) or a Python reference instead.

  Requiring in-dim alignment instead (GS must divide `dim` AND `hidden`) forces GS=4 here
  (gcd(64,172)=4): one f32 scale per 4 int8 weights = 16 bits/weight, and a 528 KB blob
  instead of 283 KB. Not worth it for a 260K-param model on a 4 MB flash budget.
"""
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

# weights_only=True: the file is sha256-pinned, but never unpickle arbitrary objects.
ckpt = torch.load(SRC / "stories260K.pt", map_location="cpu", weights_only=True)
a, sd = ckpt["model_args"], ckpt["model"]
sd = {k.removeprefix("_orig_mod."): v for k, v in sd.items()}
dim, nl, nh = a["dim"], a["n_layers"], a["n_heads"]
nkv, vocab, seq = a["n_kv_heads"], a["vocab_size"], a["max_seq_len"]
hidden = sd["layers.0.feed_forward.w1.weight"].shape[0]
# A checkpoint with no separate output head is tied by construction.
shared = 1 if ("output.weight" not in sd
               or torch.equal(sd["output.weight"], sd["tok_embeddings.weight"])) else 0

# Every matrix we export, in the order the SBRD body stores them (see qtensor calls below).
_mats = [sd["tok_embeddings.weight"]]
for key in ("attention.wq", "attention.wk", "attention.wv", "attention.wo",
            "feed_forward.w1", "feed_forward.w2", "feed_forward.w3"):
    _mats += [sd[f"layers.{i}.{key}.weight"] for i in range(nl)]
if not shared:
    _mats.append(sd["output.weight"])

# Upstream's rule (export.py:210): groups run over flattened data, so per-matrix numel is
# what must divide. See the module docstring for why in-dim alignment is NOT required.
gs = 64
while any(m.numel() % gs for m in _mats):
    gs //= 2
    assert gs >= 4, "no workable group size"

tok = (SRC / "tok512.bin").read_bytes()  # llama2.c format: u32 max_len, then {f32 score, i32 len, bytes}*
p, out_tok = 4, [tok[0:4]]
for _ in range(vocab):
    score = tok[p:p+4]; ln = struct.unpack_from("<i", tok, p+4)[0]
    b = tok[p+8:p+8+ln]; p += 8 + ln
    assert ln < 256
    out_tok += [score, struct.pack("<B", ln), b]
assert p == len(tok), f"tokenizer trailing bytes: parsed {p} of {len(tok)}"
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
