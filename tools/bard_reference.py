#!/usr/bin/env python3
"""bard (#300): independent Python reference forward pass over the committed SBRD blob.

The port-correctness gate for the Rust implementation in rust/clock/src/bard/nano_llm.rs.
Run from the T2 venv (numpy only — torch is NOT needed):

    scratch/bard/venv/bin/python tools/bard_reference.py rust/clock/model/stories260K-q8.bin \\
        --temp 0 --steps 200 -i "Once upon a time, there was a little dragon"

WHY NOT upstream runq.c: for this checkpoint it is wrong. `runq.c:332` walks `j <= n - GS`, so
with n=172 / GS=64 it silently drops in-dim elements 128..171 of every w2 row, and
`runq.c:146` (`num_groups = n / GS`) leaves the activation tail unquantized. A CORRECT
implementation must disagree with it. (llama2.c @ 350e04fe35433e6d2941dce5a1f53308f87058eb.)
So the reference is a second implementation, in a different language, over the same bytes.

═══ THE CONTRACT ═══
This file is worthless unless it mirrors the Rust *as built*, including float ROUNDING. Rules:

  * Everything is float32. numpy 2.x (NEP 50) keeps `float32 op python_scalar` in float32, but
    the dtype asserts below are what actually holds the line.
  * FLOAT ACCUMULATION ORDER IS PART OF THE CONTRACT. Float addition is not associative, so a
    "faster" reduction changes results. Every float accumulator here is stepped in the same
    order as the Rust loop. INTEGER dots are exact and order-independent, so those — and only
    those — are vectorized. Where a float accumulation is vectorized, it is across INDEPENDENT
    accumulators (e.g. all positions t at once, stepping i sequentially), never across a
    single accumulator's addends.
  * Exact forms, not algebraic equivalents:
      rmsnorm   inv = 1/sqrt(ss/n + 1e-5); out = w * (x * inv); ss summed sequentially
      quantize  q = trunc(v/s + (+0.5 if v>=0 else -0.5)) clamped ±127   (Rust's `as i8`
                truncating+saturating cast — NOT np.round, which is banker's rounding)
      score     att[t] = (dot * k_scale[t]) * inv_sqrt_hd, inv_sqrt_hd hoisted
      v-accum   a = att[t] * v_scale[t]; xb[i] += a * v[t][i]
      SwiGLU    (v / (1 + exp(-v))) * gate      -- a DIVIDE, not v * (1/(1+exp(-v)))
      RoPE      llama2.c adjacent-pair, freq = 1/pow(10000, (i % head_dim)/head_dim)
      matmul    per segment: acc += (int32_dot as f32 * w_scale[wg]) * x_scale[ag]
  * Transcendental drift (numpy's expf/sinf/cosf/powf vs libm's) is the one thing that could
    legitimately differ. In practice it does not: the two implementations agree BIT-FOR-BIT
    over the whole 186-token story, so the Rust test now pins the ENTIRE id sequence and the
    ENTIRE text. If a libm/numpy update ever perturbs an argmax, that test is where it shows.
"""
import argparse
import hashlib
import pathlib
import struct
import zlib

import numpy as np

F32 = np.float32
# Families in blob order; `tensor()` in the Rust uses these same indices.
FAMILIES = ("emb", "wq", "wk", "wv", "wo", "w1", "w2", "w3", "wcls")
# Mirrors nano_llm::SEQ_CAP — the cache DEPTH, not the header's seq_len (512). 192 because the
# canonical fleet image cannot afford a deeper cache (see the Rust const); generation stops
# here regardless of --steps, exactly as the firmware's Story does.
SEQ_CAP = 80


def _f32(x):
    """A float32 scalar (and a tripwire against a float64 sneaking in)."""
    v = F32(x)
    assert v.dtype == np.float32
    return v


class Blob:
    """SBRD v1 reader — the same layout the Rust `Model::parse` validates.

    Per FAMILY: int8 data for ALL layers contiguously, then f32 scales for ALL layers
    contiguously (NOT llama2.c's per-matrix q,s interleave).
    """

    def __init__(self, path):
        raw = pathlib.Path(path).read_bytes()
        self.sha256 = hashlib.sha256(raw).hexdigest()
        stored = struct.unpack_from("<I", raw, len(raw) - 4)[0]
        # The same value the Rust parser checks. Stamped into the golden header so a
        # re-exported blob with stale goldens fails as "wrong blob", not as a prose diff.
        self.crc32 = zlib.crc32(raw[:-4])
        if self.crc32 != stored:
            raise SystemExit("crc32 mismatch — refusing to run on a corrupt blob")
        (magic, ver, self.dim, self.hidden, self.n_layers, self.n_heads, self.n_kv_heads,
         self.vocab, self.seq_len, self.gs, shared) = struct.unpack_from("<11I", raw, 0)
        if magic != 0x44524253 or ver != 1:
            raise SystemExit(f"not an SBRD v1 blob (magic={magic:#x} version={ver})")
        self.shared_cls = bool(shared)
        self.kv_dim = self.dim * self.n_kv_heads // self.n_heads
        self.head_dim = self.dim // self.n_heads

        tok_bytes = struct.unpack_from("<I", raw, 44)[0]
        self._parse_tokenizer(raw[48:48 + tok_bytes])

        off = 48 + tok_bytes + (-tok_bytes % 4)
        nl, d = self.n_layers, self.dim
        n_norm = nl * d * 2 + d
        norms = np.frombuffer(raw, dtype="<f4", count=n_norm, offset=off).astype(np.float32)
        self.rms_att = norms[: nl * d].reshape(nl, d)
        self.rms_ffn = norms[nl * d: 2 * nl * d].reshape(nl, d)
        self.rms_final = norms[2 * nl * d:]

        self.fam = {}
        qoff = off + n_norm * 4
        for name in FAMILIES:
            n = self._numel(name)
            if n == 0:
                continue
            q = np.frombuffer(raw, dtype=np.int8, count=n, offset=qoff)
            s = np.frombuffer(raw, dtype="<f4", count=n // self.gs,
                              offset=qoff + n).astype(np.float32)
            self.fam[name] = (q, s)
            qoff += n + (n // self.gs) * 4
        if qoff != len(raw) - 4:
            raise SystemExit(f"q-section length mismatch: ended at {qoff}, blob body {len(raw)-4}")

    def _numel(self, name):
        nl, d, h, kv, v = self.n_layers, self.dim, self.hidden, self.kv_dim, self.vocab
        return {
            "emb": v * d,
            "wq": nl * d * d, "wk": nl * kv * d, "wv": nl * kv * d, "wo": nl * d * d,
            "w1": nl * h * d, "w2": nl * d * h, "w3": nl * h * d,
            "wcls": 0 if self.shared_cls else v * d,
        }[name]

    def _parse_tokenizer(self, tok):
        """Unpack `u32 max_token_len` + vocab × {f32 score, u8 len, bytes}."""
        self.max_token_len = struct.unpack_from("<I", tok, 0)[0]
        p, self.texts, self.scores = 4, [], []
        for _ in range(self.vocab):
            self.scores.append(_f32(struct.unpack_from("<f", tok, p)[0]))
            ln = tok[p + 4]
            self.texts.append(tok[p + 5: p + 5 + ln])
            p += 5 + ln
        if p != len(tok):
            raise SystemExit(f"tokenizer table: consumed {p} of {len(tok)}")
        self.lut = {t: i for i, t in enumerate(self.texts)}

    # ---- tokenizer ------------------------------------------------------------------
    def encode(self, text):
        """BOS, dummy-space token, per-CODEPOINT seeds, then greedy best-score merges.

        Per-codepoint (not per-byte) matters: the table holds 14 multi-byte tokens (curly
        quotes, dashes, accents). Seeding byte-wise shreds them into `<0xXX>` fallbacks that
        can never merge back — the Rust made exactly that mistake and was fixed.
        """
        ids = [1]
        if text:
            ids.append(self.lut[b" "])
        for ch in text:
            piece = ch.encode("utf-8")
            whole = self.lut.get(piece, -1)
            if whole != -1:
                ids.append(whole)
            else:
                ids.extend(3 + b for b in piece)  # ids 3..259 are the `<0xXX>` tokens
        while True:
            # -inf, mirroring the Rust's f32::NEG_INFINITY. Unreachable either way (scores are
            # small negatives), but exactness is this file's entire job.
            best_score, best_id, best_at = -np.inf, -1, -1
            for i in range(len(ids) - 1):
                cand = self.texts[ids[i]] + self.texts[ids[i + 1]]
                if len(cand) > self.max_token_len:
                    continue
                j = self.lut.get(cand, -1)
                if j != -1 and self.scores[j] > best_score:
                    best_score, best_id, best_at = self.scores[j], j, i
            if best_at == -1:
                return ids
            ids[best_at] = best_id
            del ids[best_at + 1]

    def decode(self, prev, tid):
        """Printable bytes: strip ONE leading space after BOS, expand `<0xXX>` to its byte."""
        p = self.texts[tid]
        if prev == 1 and p[:1] == b" ":
            p = p[1:]
        if len(p) == 6 and p[:3] == b"<0x" and p[5:] == b">":
            return bytes([int(p[3:5], 16)])
        return p


# ═══ kernels ═══════════════════════════════════════════════════════════════════════════

def rmsnorm(x, w):
    """`out = w * (x * 1/sqrt(mean(x²) + 1e-5))`, with `ss` summed SEQUENTIALLY."""
    ss = _f32(0.0)
    for v in x:
        ss = F32(ss + F32(v * v))  # same order as the Rust `for v in x { ss += v*v }`
    inv = F32(_f32(1.0) / np.sqrt(F32(ss / F32(x.shape[0])) + _f32(1e-5)))
    out = (w * (x * inv)).astype(np.float32)
    assert out.dtype == np.float32
    return out


def quantize(x, gs):
    """int8 groups of `gs` with a ragged tail; one scale each.

    `max|v|` is order-independent so it vectorizes, and the rounding is elementwise — but it
    must be TRUNCATION of `v/s ± 0.5`, mirroring Rust's `as i8` (saturating) cast.
    """
    n = x.shape[0]
    ng = (n + gs - 1) // gs
    q = np.zeros(n, dtype=np.int8)
    s = np.zeros(ng, dtype=np.float32)
    for g in range(ng):
        ch = x[g * gs: (g + 1) * gs]
        m = np.abs(ch).max()
        sc = _f32(1.0) if m == _f32(0.0) else F32(m / _f32(127.0))
        s[g] = sc
        t = (ch / sc) + np.where(ch >= _f32(0.0), _f32(0.5), _f32(-0.5))
        q[g * gs: g * gs + ch.shape[0]] = np.clip(np.trunc(t), -127, 127).astype(np.int8)
    return q, s


def matmul(wq, ws, xq, xs, w_off, n_in, n_out, gs):
    """`out[i] = row_i(W) · x`, both int8 with per-group scales.

    Weight groups run over the FLATTENED family, so a row may straddle them (w2: in-dim 172,
    gs 64). Each segment is bounded by the next weight-group edge OR activation-group edge, and
    is scaled by the pair that actually covers it.

    Fast path: when `n_in == gs` and the family offset is group-aligned, every row is exactly
    one segment, so there is no float ACCUMULATION at all — `acc = 0 + term` is exact — and the
    whole matmul reduces to one integer matrix-vector product. Identical results, ~100× faster.
    """
    xq32 = xq.astype(np.int32)
    if n_in == gs and w_off % gs == 0:
        w = wq[w_off: w_off + n_out * n_in].reshape(n_out, n_in).astype(np.int32)
        ivals = w @ xq32
        wg = (np.arange(n_out, dtype=np.int64) * n_in + w_off) // gs
        out = ((ivals.astype(np.float32) * ws[wg]) * xs[0]).astype(np.float32)
        assert out.dtype == np.float32
        return out
    out = np.zeros(n_out, dtype=np.float32)
    for i in range(n_out):
        row = w_off + i * n_in
        acc = _f32(0.0)
        j = 0
        while j < n_in:
            wg = (row + j) // gs
            ag = j // gs
            seg_end = min((wg + 1) * gs - row, (ag + 1) * gs, n_in)
            ival = int(wq[row + j: row + seg_end].astype(np.int32) @ xq32[j:seg_end])
            acc = F32(acc + F32(F32(F32(ival) * ws[wg]) * xs[ag]))
            j = seg_end
        out[i] = acc
    return out


def softmax(x):
    """In-place-equivalent softmax: max-shift, exp, then a SEQUENTIAL sum."""
    mx = x.max()
    e = np.exp(x - mx).astype(np.float32)
    tot = _f32(0.0)
    for v in e:
        tot = F32(tot + v)
    return (e / tot).astype(np.float32)


class Reference:
    """One story's worth of state: the int8 KV cache, mirroring `Bufs`."""

    def __init__(self, b: Blob):
        self.b = b
        nl = b.n_layers
        self.k_cache = np.zeros((nl, SEQ_CAP, b.kv_dim), dtype=np.int8)
        self.v_cache = np.zeros((nl, SEQ_CAP, b.kv_dim), dtype=np.int8)
        self.k_scale = np.zeros((nl, SEQ_CAP), dtype=np.float32)
        self.v_scale = np.zeros((nl, SEQ_CAP), dtype=np.float32)

    def _quant_vec(self, v):
        """One scale for the whole vector (that is what makes a 256-deep cache affordable)."""
        m = np.abs(v).max()
        sc = _f32(1.0) if m == _f32(0.0) else F32(m / _f32(127.0))
        t = (v / sc) + np.where(v >= _f32(0.0), _f32(0.5), _f32(-0.5))
        return np.clip(np.trunc(t), -127, 127).astype(np.int8), sc

    def forward(self, token, pos):
        b = self.b
        d, h, gs, nl = b.dim, b.hidden, b.gs, b.n_layers
        kvd, hd = b.kv_dim, b.head_dim
        kv_mul = b.n_heads // b.n_kv_heads
        inv_sqrt_hd = F32(_f32(1.0) / np.sqrt(F32(hd)))
        assert pos < SEQ_CAP

        # 1. embed: dequantize the token's row of tok_emb into the residual stream
        eq, es = b.fam["emb"]
        base = token * d
        x = np.empty(d, dtype=np.float32)
        for i in range(d):
            x[i] = F32(F32(eq[base + i]) * es[(base + i) // gs])

        for l in range(nl):
            # 2. attention norm + Q/K/V
            xb = rmsnorm(x, b.rms_att[l])
            xq, xs = quantize(xb, gs)
            q = matmul(*b.fam["wq"], xq, xs, l * d * d, d, d, gs)
            kt = matmul(*b.fam["wk"], xq, xs, l * kvd * d, d, kvd, gs)
            vt = matmul(*b.fam["wv"], xq, xs, l * kvd * d, d, kvd, gs)

            # 3. RoPE — adjacent-pair rotation, head_dim-relative exponent
            for i in range(0, d, 2):
                freq = F32(_f32(1.0) / np.power(_f32(10000.0), F32(F32(i % hd) / F32(hd))))
                val = F32(F32(pos) * freq)
                fcr, fci = F32(np.cos(val)), F32(np.sin(val))
                q0, q1 = q[i], q[i + 1]
                q[i] = F32(F32(q0 * fcr) - F32(q1 * fci))
                q[i + 1] = F32(F32(q0 * fci) + F32(q1 * fcr))
                if i < kvd:
                    k0, k1 = kt[i], kt[i + 1]
                    kt[i] = F32(F32(k0 * fcr) - F32(k1 * fci))
                    kt[i + 1] = F32(F32(k0 * fci) + F32(k1 * fcr))

            # 4. quantize K/V into this layer's cache slot
            self.k_cache[l, pos], self.k_scale[l, pos] = self._quant_vec(kt)
            self.v_cache[l, pos], self.v_scale[l, pos] = self._quant_vec(vt)

            # 5. attention, head at a time. Vectorized over POSITIONS (independent
            #    accumulators) while stepping i sequentially — same order as the Rust per t.
            t_n = pos + 1
            xb = np.zeros(d, dtype=np.float32)
            kmat = self.k_cache[l, :t_n].astype(np.float32)
            vmat = self.v_cache[l, :t_n].astype(np.float32)
            for hh in range(b.n_heads):
                qo, kvo = hh * hd, (hh // kv_mul) * hd
                dot = np.zeros(t_n, dtype=np.float32)
                for i in range(hd):
                    dot = (dot + F32(q[qo + i]) * kmat[:, kvo + i]).astype(np.float32)
                att = softmax(((dot * self.k_scale[l, :t_n]) * inv_sqrt_hd).astype(np.float32))
                a = (att * self.v_scale[l, :t_n]).astype(np.float32)
                acc = np.zeros(hd, dtype=np.float32)
                for t in range(t_n):
                    acc = (acc + a[t] * vmat[t, kvo: kvo + hd]).astype(np.float32)
                xb[qo: qo + hd] = acc

            # 6. output projection + residual
            xq, xs = quantize(xb, gs)
            xb2 = matmul(*b.fam["wo"], xq, xs, l * d * d, d, d, gs)
            x = (x + xb2).astype(np.float32)

            # 7. FFN: SwiGLU(w1 x, w3 x) -> w2, + residual
            xb = rmsnorm(x, b.rms_ffn[l])
            xq, xs = quantize(xb, gs)
            hb = matmul(*b.fam["w1"], xq, xs, l * h * d, d, h, gs)
            hb2 = matmul(*b.fam["w3"], xq, xs, l * h * d, d, h, gs)
            hb = ((hb / (_f32(1.0) + np.exp(-hb))) * hb2).astype(np.float32)  # DIVIDE
            hq, hs = quantize(hb, gs)  # hidden=172 with gs=64 — the ragged-tail case
            xb2 = matmul(*b.fam["w2"], hq, hs, l * d * h, h, d, gs)
            x = (x + xb2).astype(np.float32)

        # 8. final norm + classifier
        xb = rmsnorm(x, b.rms_final)
        xq, xs = quantize(xb, gs)
        cls = b.fam["emb"] if b.shared_cls else b.fam["wcls"]
        logits = matmul(*cls, xq, xs, 0, d, b.vocab, gs)
        assert logits.dtype == np.float32
        return logits


def main():
    ap = argparse.ArgumentParser(description="bard #300 golden reference forward pass")
    ap.add_argument("blob")
    ap.add_argument("--temp", type=float, default=0.0,
                    help="only 0 (greedy) is supported; sampling belongs to Task 7's RNG")
    ap.add_argument("--steps", type=int, default=200)
    ap.add_argument("-i", "--prompt", default="Once upon a time, there was a little dragon")
    ap.add_argument("--tokens-out", help="write generated token ids, one per line")
    args = ap.parse_args()
    if args.temp != 0.0:
        raise SystemExit("only --temp 0 (greedy) is implemented — Task 7 owns the sampler "
                         "and its RNG contract; a reference must not invent one")

    b = Blob(args.blob)
    ref = Reference(b)
    ids = b.encode(args.prompt)
    print(f"# prompt ids: {ids}", file=__import__("sys").stderr)

    # Feed the prompt, then greedy-continue from its last token.
    for i in range(len(ids) - 1):
        ref.forward(ids[i], i)
    token = ids[-1]
    generated, out = [], bytearray()
    for pos in range(len(ids) - 1, min(args.steps, SEQ_CAP)):
        logits = ref.forward(token, pos)
        nxt = int(np.argmax(logits))  # first max wins, matching the Rust's `>` comparison
        if nxt == 1 or nxt == 2:  # BOS/EOS terminate
            break
        generated.append(nxt)
        out += b.decode(token, nxt)
        token = nxt

    print(f"# reference {b.sha256[:12]} temp0 crc32={b.crc32:08x}")
    print(args.prompt + out.decode("utf-8", "replace"))
    if args.tokens_out:
        pathlib.Path(args.tokens_out).write_text("".join(f"{t}\n" for t in generated))


if __name__ == "__main__":
    main()
