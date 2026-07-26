#!/usr/bin/env python3
"""bard (#300): stories260K.pt + tok512.bin -> rust/clock/model/stories260K-q8.bin (SBRD v1).

Inputs come from tools/bard_fetch_model.sh (sha256-pinned, MIT, karpathy/tinyllamas).
Run inside the CPU-torch venv:  scratch/bard/venv/bin/python tools/bard_export_model.py
Requires: torch==2.13.0+cpu, numpy==2.5.1 (the versions this export was produced with; the
CPU torch wheel does NOT pull numpy, so install it explicitly).

LAYOUT — the one thing to get right when writing a consumer:
  Per FAMILY, the i8 data for ALL layers is contiguous, and THEN the f32 scales for ALL
  layers are contiguous. This is NOT llama2.c's per-matrix `q,s` interleave (q0 s0 q1 s1 ...):
  reading floats just past one layer's i8 data lands inside the NEXT layer's i8 data. Address
  a weight as i8[family_i8 + layer*numel + row*in + k] and its scale as
  f32[family_scales + (layer*numel + row*in + k)/gs] — one flattened index per family, running
  across layer boundaries.

GROUP SIZE — read before changing (this bit is subtle):
  Quantization groups run over each matrix's FLATTENED (row-major) data, exactly like
  llama2.c's `quantize_q80`. The only hard requirement is therefore `numel % GS == 0` per
  matrix, which is what upstream export.py asserts (export.py:210); its backoff loop tests
  `dim % GS` only (export.py:193 — the printed message says "hidden_dim" but the code reads
  `dim`). Upstream refs are llama2.c @ 350e04fe35433e6d2941dce5a1f53308f87058eb.

  For stories260K that yields GS=64, and hidden_dim=172 is NOT a multiple of 64. So for
  w2 (shape [dim, hidden] = [64, 172]) a 64-weight group STRADDLES row boundaries:
  172 = 2*64 + 44. That is fine on disk and fine for an exact dequant, but the CONSUMER
  must index scales by FLATTENED position and flush its i32 accumulator whenever the
  weight-group OR activation-group index changes. It must NOT assume one scale per row-chunk.
  Groups never straddle a layer/matrix boundary, since every matrix's numel divides GS.

  ⚠️ Upstream runq.c does NOT do this: `for (j = 0; j <= n - GS; j += GS)` (runq.c:332)
  walks the in-dim in whole GS strides, so with n=172 it silently drops elements 128..171,
  and `num_groups = n / GS` (runq.c:146) under-quantizes the activation the same way.
  runq.c's q8 path is thus NOT a valid golden reference for this checkpoint — use an
  independent reference implementation instead.

  Requiring in-dim alignment instead (GS must divide `dim` AND `hidden`) forces GS=4 here
  (gcd(64,172)=4): one f32 scale per 4 int8 weights = 16 bits/weight, and a 528 KB blob
  instead of 283 KB. Not worth it for a 260K-param model on a 4 MB flash budget.
"""
import hashlib
import os
import pathlib
import struct
import zlib

try:
    import torch
except ModuleNotFoundError as e:  # the venv is the only supported way to run this
    raise SystemExit(
        f"{e.name} is not importable — create the CPU-torch venv first:\n"
        "  python3 -m venv scratch/bard/venv\n"
        "  scratch/bard/venv/bin/pip install torch numpy "
        "--index-url https://download.pytorch.org/whl/cpu\n"
        "  scratch/bard/venv/bin/python tools/bard_export_model.py"
    ) from e

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

# ONE ordered list of families drives everything below: the group-size backoff, the emit
# sequence, and the per-family error report. Order here IS the on-disk order.
families = [("emb", [sd["tok_embeddings.weight"]])]
families += [(name, [sd[f"layers.{i}.{key}.weight"] for i in range(nl)])
             for name, key in (("wq", "attention.wq"), ("wk", "attention.wk"),
                               ("wv", "attention.wv"), ("wo", "attention.wo"),
                               ("w1", "feed_forward.w1"), ("w2", "feed_forward.w2"),
                               ("w3", "feed_forward.w3"))]
if not shared:
    families.append(("wcls", [sd["output.weight"]]))

# Upstream's rule (export.py:210): groups run over flattened data, so per-matrix numel is
# what must divide. See the module docstring for why in-dim alignment is NOT required.
gs = 64
while any(m.numel() % gs for _, mats in families for m in mats):
    gs //= 2
    assert gs >= 4, "no workable group size"

tok = (SRC / "tok512.bin").read_bytes()  # llama2.c format: u32 max_len, then {f32 score, i32 len, bytes}*
# Repack each entry's length from llama2.c's i32 down to a u8: token texts are < 256 bytes by
# construction (asserted per entry below), so 3 of those 4 bytes are pure padding — ~1.5 KB of
# flash across 512 entries — and a u8 keeps the on-device table walk a single byte read.
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

# Emit each family as one i8 block (all layers) followed by one f32 scale block (all layers).
errs = {}
for name, mats in families:
    qs, ss, werr = [], [], 0.0
    for m in mats:
        q, s, e = q8(m, gs)
        qs.append(q); ss.append(s); werr = max(werr, e)
    body.append(b"".join(qs))
    body.append(b"".join(ss))
    errs[name] = werr

blob = b"".join(body)
blob += struct.pack("<I", zlib.crc32(blob))
OUT.parent.mkdir(parents=True, exist_ok=True)
# Atomic publish: a killed run must never leave a half-written blob where the firmware build
# (or a committed deliverable) would pick it up.
tmp = OUT.with_name(OUT.name + ".tmp")
tmp.write_bytes(blob)
os.replace(tmp, OUT)
print(f"dim={dim} hidden={hidden} layers={nl} heads={nh}/{nkv} vocab={vocab} "
      f"seq={seq} gs={gs} shared={shared} size={len(blob)} max_qerr={max(errs.values()):.4f} "
      f"sha256={hashlib.sha256(blob).hexdigest()}")
