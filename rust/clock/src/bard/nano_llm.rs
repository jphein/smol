//! bard (#300) core: the SBRD v1 model container.
//!
//! One flash-resident blob (`model/stories260K-q8.bin`, produced by
//! `tools/bard_export_model.py`) holds everything the storyteller needs: the header, the
//! packed tok512 table, the f32 RMSNorm weights and the int8 weight matrices with their f32
//! group scales. `Model::parse` validates it once and hands back BORROWED views — nothing is
//! copied, so a 277 KB model costs 277 KB of flash and ~0 bytes of RAM to open.
//!
//! Layout (little-endian throughout; see the exporter for the authoritative description):
//! ```text
//!   0  u32 magic = b"SBRD"          4  u32 version = 1
//!   8  u32 dim, hidden, n_layers, n_heads, n_kv_heads, vocab, seq_len, gs, shared_cls  (9)
//!  44  u32 tok_bytes                48  tokenizer section, then 0-pad to 4-byte alignment
//!      f32 norms: rms_att[L][dim] · rms_ffn[L][dim] · rms_final[dim]
//!      q8 families, in this order — for EACH family, i8 data for ALL layers contiguously,
//!      THEN the f32 scales for ALL layers contiguously:
//!         tok_emb · wq · wk · wv · wo · w1 · w2 · w3 · (wcls only if !shared_cls)
//!            family i8   = n_total          bytes, n_total = n_layers * (rows * in)
//!            family f32s = n_total/gs * 4   bytes
//!      u32 crc32 of every preceding byte
//! ```
//!
//! ⚠️ FAMILY-GROUPED, NOT PER-MATRIX INTERLEAVED. llama2.c's `export.py` writes `q,s` per
//! MATRIX (q₀ s₀ q₁ s₁ …); SBRD writes one i8 block then one scale block per FAMILY. So a
//! layer's scales are NOT at `family_start + layer_i8_len` — reading floats just past a
//! layer's i8 data lands in the NEXT layer's i8 data. Address a weight as
//! `i8[family_i8 + layer*numel + row*in + k]` and its scale as
//! `f32[family_scales + (layer*numel + row*in + k)/gs]`, i.e. ONE flattened index per family
//! that runs across layer boundaries.
//!
//! Quantization groups run over that flattened data, so a group may straddle ROW boundaries
//! (it does here: w2's in-dim is 172, not a multiple of gs=64) — consumers must index scales
//! by flattened position, never per row. Groups never straddle a LAYER or MATRIX boundary,
//! because each matrix's numel is a multiple of `gs` (checked by the exporter and re-checked
//! here), which is also why the per-family flattened index equals
//! `layer * numel/gs + (row*in + k)/gs`. See the exporter's docstring for the full rationale.

/// Compile-time maxima: the statically-sized buffers the forward pass will use are cut to
/// these, and [`Model::parse`] refuses any header that would overflow them. They bound
/// `.bss`, not this blob — headroom above the shipped model (hidden 172 ≤ 192) is free.
pub const MAX_DIM: usize = 64;
/// Max FFN width. 192 > the shipped 172 on purpose: leaves room for a future checkpoint.
pub const MAX_HIDDEN: usize = 192;
/// Max transformer blocks.
pub const MAX_LAYERS: usize = 5;
/// Max attention heads.
pub const MAX_HEADS: usize = 8;
/// Max vocabulary entries.
pub const MAX_VOCAB: usize = 512;
/// KV-cache depth the firmware allocates, independent of the header's `seq_len` (512): a
/// story is capped at this many tokens so the cache fits RAM.
pub const SEQ_CAP: usize = 256;
/// Row stride of one cached K (or V) vector, in int8 slots. The shipped model's `kv_dim` is
/// exactly 32; [`Model::parse`] REFUSES anything wider, which is what lets the forward pass
/// index the cache without a bounds fallback.
pub const KV_STRIDE: usize = 32;

/// `b"SBRD"` read as a little-endian u32.
const MAGIC: u32 = 0x4452_4253;
/// The only format version this build understands.
const VERSION: u32 = 1;
/// Fixed prefix: the 11-u32 header plus the `tok_bytes` length word.
const HEADER_LEN: usize = 48;

/// Model geometry, straight from the header (all validated by [`Model::parse`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Residual-stream width.
    pub dim: usize,
    /// FFN inner width.
    pub hidden: usize,
    /// Transformer blocks.
    pub n_layers: usize,
    /// Query heads.
    pub n_heads: usize,
    /// Key/value heads (< `n_heads` ⇒ grouped-query attention).
    pub n_kv_heads: usize,
    /// Vocabulary entries.
    pub vocab: usize,
    /// Context the checkpoint was trained for (the runtime cap is [`SEQ_CAP`]).
    pub seq_len: usize,
    /// Weights per quantization group (one f32 scale each).
    pub gs: usize,
    /// `true` when the classifier reuses `tok_emb` (no separate `wcls` section).
    pub shared_cls: bool,
}

impl Config {
    /// Width of the packed K/V vectors: `dim * n_kv_heads / n_heads`.
    pub const fn kv_dim(&self) -> usize {
        self.dim * self.n_kv_heads / self.n_heads
    }

    /// Width of a single attention head.
    pub const fn head_dim(&self) -> usize {
        self.dim / self.n_heads
    }
}

/// Why a blob was rejected. Every variant is a refusal to run on data we cannot trust —
/// a wrong-model or bit-rotted flash region must never be interpreted as weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErr {
    /// Missing `SBRD` marker.
    Magic,
    /// Format version this build does not implement.
    Version,
    /// Trailing crc32 disagrees with the content.
    Crc,
    /// Too short, or a section length disagrees with the header's geometry.
    Truncated,
    /// Geometry exceeds the compile-time maxima, or is internally inconsistent.
    DimsTooBig,
}

/// A parsed, integrity-checked model: geometry plus borrowed views into the blob.
#[derive(Debug, Clone, Copy)]
pub struct Model<'a> {
    /// Validated geometry.
    pub cfg: Config,
    /// Packed tokenizer table: `u32 max_token_len`, then `vocab × { f32 score, u8 len, bytes }`.
    pub tok_table: &'a [u8],
    /// f32 RMSNorm weights: `rms_att[L][dim] · rms_ffn[L][dim] · rms_final[dim]`.
    pub norms: &'a [u8],
    /// The q8 tensor families, in blob order, each an i8 block for ALL layers followed by the
    /// f32 scale block for ALL layers (see the module doc — NOT a per-matrix `q,s` interleave).
    pub qdata: &'a [u8],
}

/// crc32 (reflected, poly `0xEDB8_8320`) — the zlib/PNG variant `python zlib.crc32` emits.
/// Table-free on purpose: a 1 KiB lookup table would cost more flash than the ~1 ms this
/// takes to sweep 277 KB once at boot.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c: u32 = !0;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// Little-endian u32 at byte offset `i`. Callers bounds-check first.
fn ru32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Little-endian f32 at byte offset `i`. The blob's f32s are only 4-byte aligned relative to
/// the section, never to the mapped flash address, so read them BYTEWISE — a pointer cast
/// would be UB (and traps on some targets), and `from_le_bytes` compiles to the same load
/// when the address happens to be aligned.
pub fn rf32(b: &[u8], i: usize) -> f32 {
    f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

impl<'a> Model<'a> {
    /// Validate `blob` and borrow its sections.
    ///
    /// Order is deliberate: length, then crc32, then identity, then geometry — nothing is
    /// trusted before the integrity check that covers it.
    pub fn parse(blob: &'a [u8]) -> Result<Self, ParseErr> {
        if blob.len() < HEADER_LEN + 4 {
            return Err(ParseErr::Truncated);
        }
        let (body, tail) = blob.split_at(blob.len() - 4);
        if crc32(body) != ru32(tail, 0) {
            return Err(ParseErr::Crc);
        }
        if ru32(body, 0) != MAGIC {
            return Err(ParseErr::Magic);
        }
        if ru32(body, 4) != VERSION {
            return Err(ParseErr::Version);
        }

        let cfg = Config {
            dim: ru32(body, 8) as usize,
            hidden: ru32(body, 12) as usize,
            n_layers: ru32(body, 16) as usize,
            n_heads: ru32(body, 20) as usize,
            n_kv_heads: ru32(body, 24) as usize,
            vocab: ru32(body, 28) as usize,
            seq_len: ru32(body, 32) as usize,
            gs: ru32(body, 36) as usize,
            shared_cls: ru32(body, 40) != 0,
        };

        // Geometry must fit the static buffers AND be self-consistent, both because the
        // forward pass indexes with it and because every size computed below derives from
        // it — bounding it here is what keeps the arithmetic that follows overflow-free.
        let heads_ok = cfg.n_heads != 0
            && cfg.n_kv_heads != 0
            && cfg.n_kv_heads <= cfg.n_heads
            && cfg.n_heads.is_multiple_of(cfg.n_kv_heads)
            && cfg.dim.is_multiple_of(cfg.n_heads);
        let dims_ok = (1..=MAX_DIM).contains(&cfg.dim)
            && (1..=MAX_HIDDEN).contains(&cfg.hidden)
            && (1..=MAX_LAYERS).contains(&cfg.n_layers)
            && cfg.n_heads <= MAX_HEADS
            && (1..=MAX_VOCAB).contains(&cfg.vocab)
            && cfg.seq_len != 0
            // The KV cache is cut at KV_STRIDE per vector; a wider model would index past a
            // row. Refusing it HERE is what keeps `Session::forward` free of bounds fallbacks.
            && cfg.dim * cfg.n_kv_heads / cfg.n_heads <= KV_STRIDE;
        // gs divides the row length, so a group never straddles a MATRIX boundary; it may
        // still straddle a ROW (hidden=172 % 64 != 0) — that is the consumer's problem.
        let gs_ok = cfg.gs >= 4 && cfg.gs.is_multiple_of(4) && cfg.dim.is_multiple_of(cfg.gs);
        if !(heads_ok && dims_ok && gs_ok) {
            return Err(ParseErr::DimsTooBig);
        }

        // Tokenizer section: length is attacker-controlled, so add it checked.
        let tok_bytes = ru32(body, 44) as usize;
        let tok_end = HEADER_LEN.checked_add(tok_bytes).ok_or(ParseErr::Truncated)?;
        if tok_end > body.len() {
            return Err(ParseErr::Truncated);
        }
        let tok_table = &body[HEADER_LEN..tok_end];

        // Norms start at the next 4-byte boundary after the tokenizer section.
        let norms_start = tok_end + (4 - tok_bytes % 4) % 4;
        let norms_len = (cfg.n_layers * cfg.dim * 2 + cfg.dim) * 4;
        let norms_end = norms_start
            .checked_add(norms_len)
            .ok_or(ParseErr::Truncated)?;
        if norms_end > body.len() {
            return Err(ParseErr::Truncated);
        }
        let norms = &body[norms_start..norms_end];
        let qdata = &body[norms_end..];

        // EXACT q-section length, recomputed from the header alone: i8 data plus one f32
        // scale per group, for each family in blob order. An off-by-one-tensor blob (or a
        // header that disagrees with its payload) fails here rather than silently reading
        // one family's bytes as another's.
        let (nl, kv) = (cfg.n_layers, cfg.kv_dim());
        let mut want = 0usize;
        for n in [
            cfg.vocab * cfg.dim,       // tok_emb
            nl * cfg.dim * cfg.dim,    // wq
            nl * kv * cfg.dim,         // wk
            nl * kv * cfg.dim,         // wv
            nl * cfg.dim * cfg.dim,    // wo
            nl * cfg.hidden * cfg.dim, // w1
            nl * cfg.dim * cfg.hidden, // w2
            nl * cfg.hidden * cfg.dim, // w3
        ] {
            want += n + (n / cfg.gs) * 4;
        }
        if !cfg.shared_cls {
            let n = cfg.vocab * cfg.dim; // wcls
            want += n + (n / cfg.gs) * 4;
        }
        if qdata.len() != want {
            return Err(ParseErr::Truncated);
        }

        Ok(Self {
            cfg,
            tok_table,
            norms,
            qdata,
        })
    }
}

// ===================================================================================
// Tensor views
// ===================================================================================

/// One quantized tensor FAMILY: `q` is the int8 data for ALL layers contiguously, `s` the f32
/// scales (little-endian bytes) for ALL layers contiguously.
///
/// A weight at `layer`, output row `r`, input `k` of an `[out, in]` matrix lives at
/// `q[layer*out*in + r*in + k]`, and its scale at element `(layer*out*in + r*in + k) / gs` of
/// `s` — ONE flattened index per family, running across layer boundaries. See the module doc.
pub(crate) struct QTensor<'a> {
    /// int8 weights as raw bytes (read through [`i8at`] — a `u8` slice keeps this `unsafe`-free).
    pub q: &'a [u8],
    /// f32 group scales, little-endian (read through [`f32at`]).
    pub s: &'a [u8],
}

/// A quantized ACTIVATION vector: int8 values plus one scale per `gs`-sized group. The final
/// group is short when the length isn't a multiple of `gs` (it is for `hidden = 172`, `gs = 64`).
struct QAct<'a> {
    q: &'a [i8],
    s: &'a [f32],
}

/// Sign-extending read of an int8 weight stored in a `u8` slice. `u8 as i8` is a defined
/// two's-complement reinterpretation, so this needs no `unsafe` — and routing EVERY weight
/// read through it makes a forgotten sign extension impossible.
#[inline]
pub(crate) fn i8at(b: &[u8], i: usize) -> i32 {
    b[i] as i8 as i32
}

/// f32 at ELEMENT index `i` of a little-endian f32 byte slice (i.e. byte offset `4*i`).
/// The `*4` lives here so callers can index scales/norms by element, never by byte.
#[inline]
pub(crate) fn f32at(b: &[u8], i: usize) -> f32 {
    rf32(b, i * 4)
}

impl<'a> Model<'a> {
    /// Element count of family `idx` across all layers.
    /// `0=emb 1=wq 2=wk 3=wv 4=wo 5=w1 6=w2 7=w3 8=wcls`.
    fn family_numel(&self, idx: usize) -> usize {
        let c = &self.cfg;
        let (nl, d, h, kv) = (c.n_layers, c.dim, c.hidden, c.kv_dim());
        match idx {
            0 | 8 => c.vocab * d,
            1 | 4 => nl * d * d,
            2 | 3 => nl * kv * d,
            5 | 7 => nl * h * d,
            6 => nl * d * h,
            _ => 0,
        }
    }

    /// Borrow family `idx` (see [`Self::family_numel`] for the index map).
    ///
    /// Walks the preceding families' `i8 + scales` sizes. `idx == 8` is only meaningful when
    /// `!shared_cls` — use [`Self::classifier`] instead of hand-picking it.
    pub(crate) fn tensor(&self, idx: usize) -> QTensor<'a> {
        let gs = self.cfg.gs;
        let mut off = 0usize;
        for k in 0..idx {
            let n = self.family_numel(k);
            off += n + (n / gs) * 4;
        }
        let n = self.family_numel(idx);
        debug_assert!(off + n + (n / gs) * 4 <= self.qdata.len(), "family {idx} out of range");
        QTensor {
            q: &self.qdata[off..off + n],
            s: &self.qdata[off + n..off + n + (n / gs) * 4],
        }
    }

    /// The output classifier: `tok_emb` when the checkpoint ties them, else the `wcls` family.
    pub(crate) fn classifier(&self) -> QTensor<'a> {
        self.tensor(if self.cfg.shared_cls { 0 } else { 8 })
    }

    /// `rms_att[l]` as f32 LE bytes.
    pub(crate) fn norm_att(&self, l: usize) -> &'a [u8] {
        let d = self.cfg.dim;
        &self.norms[l * d * 4..(l * d + d) * 4]
    }

    /// `rms_ffn[l]` as f32 LE bytes (the second block of `n_layers * dim` floats).
    pub(crate) fn norm_ffn(&self, l: usize) -> &'a [u8] {
        let (d, nl) = (self.cfg.dim, self.cfg.n_layers);
        let base = nl * d + l * d;
        &self.norms[base * 4..(base + d) * 4]
    }

    /// `rms_final` as f32 LE bytes.
    pub(crate) fn norm_final(&self) -> &'a [u8] {
        let (d, nl) = (self.cfg.dim, self.cfg.n_layers);
        let base = 2 * nl * d;
        &self.norms[base * 4..(base + d) * 4]
    }
}

// ===================================================================================
// Scratch buffers — one static allocation, no heap
// ===================================================================================

/// Every mutable buffer the forward pass needs, sized by the compile-time maxima.
///
/// [`Bufs::INIT`] is an all-zero `const`, so a firmware `static mut BUFS: Bufs = Bufs::INIT`
/// lands in `.bss` (no initializer bytes in flash); host tests `Box` it to keep it off the
/// stack. Nothing here is heap-allocated and nothing is per-token allocated.
pub struct Bufs {
    /// Residual stream.
    pub x: [f32; MAX_DIM],
    /// Normalized / attention-output scratch.
    xb: [f32; MAX_DIM],
    /// Second `dim`-wide scratch (matmul destination before the residual add).
    xb2: [f32; MAX_DIM],
    /// FFN scratch (`w1` output, then SwiGLU result).
    hb: [f32; MAX_HIDDEN],
    /// FFN gate scratch (`w3` output).
    hb2: [f32; MAX_HIDDEN],
    /// Query vector for the current token.
    q: [f32; MAX_DIM],
    /// Attention scores over positions `0..=pos`.
    att: [f32; SEQ_CAP],
    /// Output logits (`vocab` valid).
    pub logits: [f32; MAX_VOCAB],
    /// Quantized activation values (wide enough for `hidden`, the longest vector quantized).
    xq: [i8; MAX_HIDDEN],
    /// One scale per activation group; `+1` covers a ragged final group.
    xs: [f32; MAX_HIDDEN / 4 + 1],
    /// int8 K cache, `[layer][pos][KV_STRIDE]`.
    k_cache: [i8; MAX_LAYERS * SEQ_CAP * KV_STRIDE],
    /// int8 V cache, same shape.
    v_cache: [i8; MAX_LAYERS * SEQ_CAP * KV_STRIDE],
    /// One scale per cached K vector, `[layer][pos]`.
    k_scale: [f32; MAX_LAYERS * SEQ_CAP],
    /// One scale per cached V vector.
    v_scale: [f32; MAX_LAYERS * SEQ_CAP],
}

impl Bufs {
    /// All-zero buffers: `static mut BUFS: Bufs = Bufs::INIT` costs `.bss`, not flash.
    pub const INIT: Bufs = Bufs {
        x: [0.0; MAX_DIM],
        xb: [0.0; MAX_DIM],
        xb2: [0.0; MAX_DIM],
        hb: [0.0; MAX_HIDDEN],
        hb2: [0.0; MAX_HIDDEN],
        q: [0.0; MAX_DIM],
        att: [0.0; SEQ_CAP],
        logits: [0.0; MAX_VOCAB],
        xq: [0; MAX_HIDDEN],
        xs: [0.0; MAX_HIDDEN / 4 + 1],
        k_cache: [0; MAX_LAYERS * SEQ_CAP * KV_STRIDE],
        v_cache: [0; MAX_LAYERS * SEQ_CAP * KV_STRIDE],
        k_scale: [0.0; MAX_LAYERS * SEQ_CAP],
        v_scale: [0.0; MAX_LAYERS * SEQ_CAP],
    };
}

// ===================================================================================
// Kernels
//
// Float ORDER OF OPERATIONS in this section is part of the golden contract with the T6
// reference implementation — these are not algebraically-free rewrites. Do not "simplify"
// `a / b * c` into `a * (1/b) * c`, or reassociate the accumulations.
// ===================================================================================

/// RMSNorm: `out[i] = w[i] * x[i] / sqrt(mean(x²) + 1e-5)`.
fn rmsnorm(out: &mut [f32], x: &[f32], w: &[u8]) {
    let mut ss = 0f32;
    for v in x {
        ss += v * v;
    }
    let inv = 1.0 / libm::sqrtf(ss / x.len() as f32 + 1e-5);
    for (i, (o, xv)) in out.iter_mut().zip(x.iter()).enumerate() {
        *o = f32at(w, i) * (xv * inv);
    }
}

/// Symmetric int8 quantization in groups of `gs`, one scale per group.
///
/// `chunks()` yields the ragged final group naturally (activation lengths need not divide
/// `gs`: `hidden = 172` with `gs = 64` gives groups of 64, 64, 44). The rounding —
/// `(v/scale ± 0.5) as i8` — is deliberately the truncating C form, part of the golden contract.
fn quantize(x: &[f32], gs: usize, q: &mut [i8], s: &mut [f32]) {
    for (g, chunk) in x.chunks(gs).enumerate() {
        let mut m = 0f32;
        for v in chunk {
            let a = libm::fabsf(*v);
            if a > m {
                m = a;
            }
        }
        let sc = if m == 0.0 { 1.0 } else { m / 127.0 };
        s[g] = sc;
        for (j, v) in chunk.iter().enumerate() {
            // Iterate chunk.len(), never gs — the final group may be short.
            q[g * gs + j] = (v / sc + if *v >= 0.0 { 0.5 } else { -0.5 }) as i8;
        }
    }
}

/// `out[i] = row_i(W) · x`, both operands int8 with per-group scales.
///
/// Weight groups run over the FLATTENED family, so with `hidden = 172` a `w2` row STRADDLES
/// weight-group boundaries; activation groups are position-aligned with a ragged tail. The
/// walk therefore flushes the i32 accumulator at every weight- OR activation-group edge, and
/// scales that segment with the pair of scales that actually covers it. When the in-dim
/// divides `gs` this degenerates to one flush per `gs` chunk — i.e. plain runq behaviour.
fn matmul(out: &mut [f32], x: &QAct, w: &QTensor, w_off: usize, n_in: usize, gs: usize) {
    for (i, o) in out.iter_mut().enumerate() {
        let row = w_off + i * n_in;
        let mut acc = 0f32;
        let mut j = 0usize;
        while j < n_in {
            let wg = (row + j) / gs; // weight-group index (flattened, crosses rows)
            let ag = j / gs; // activation-group index (row-local)
            let seg_end = core::cmp::min(
                core::cmp::min((wg + 1) * gs - row, (ag + 1) * gs),
                n_in,
            );
            let mut ival: i32 = 0;
            for k in j..seg_end {
                ival += i8at(w.q, row + k) * x.q[k] as i32;
            }
            acc += ival as f32 * f32at(w.s, wg) * x.s[ag];
            j = seg_end;
        }
        *o = acc;
    }
}

/// In-place softmax, max-shifted for stability.
fn softmax(x: &mut [f32]) {
    let mut mx = f32::MIN;
    for v in x.iter() {
        if *v > mx {
            mx = *v;
        }
    }
    let mut sum = 0f32;
    for v in x.iter_mut() {
        *v = libm::expf(*v - mx);
        sum += *v;
    }
    for v in x.iter_mut() {
        *v /= sum;
    }
}

// ===================================================================================
// Session
// ===================================================================================

/// Generation state across a story: just the write cursor into the KV cache.
///
/// The cache itself lives in [`Bufs`] so one static allocation serves every session; a new
/// `Session` over the same `Bufs` simply overwrites cache slots from position 0.
pub struct Session {
    /// Last position written (informational; `forward` takes `pos` explicitly).
    pub pos: u16,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// A session positioned at the start of a story.
    pub fn new() -> Self {
        Session { pos: 0 }
    }

    /// Run one token through the transformer and return `&logits[..vocab]`.
    ///
    /// Mirrors llama2.c's `forward()` with two changes: the KV cache is int8 with one scale
    /// per cached vector (a quarter of the RAM), and every matmul uses the segment-flush walk
    /// above. `pos` is clamped to [`SEQ_CAP`] - 1 so a runaway caller cannot write past the
    /// cache; the story state machine is responsible for stopping earlier.
    pub fn forward<'b>(
        &mut self,
        m: &Model<'_>,
        b: &'b mut Bufs,
        token: u16,
        pos: usize,
    ) -> &'b [f32] {
        let c = m.cfg;
        let (d, h, gs) = (c.dim, c.hidden, c.gs);
        let (kvd, hd) = (c.kv_dim(), c.head_dim());
        let kv_mul = c.n_heads / c.n_kv_heads; // query heads per kv head (GQA)
        let pos = pos.min(SEQ_CAP - 1);
        self.pos = pos as u16;

        // 1. Embed: dequantize the token's row of tok_emb straight into the residual stream.
        let emb = m.tensor(0);
        let base = token as usize * d;
        for (i, o) in b.x[..d].iter_mut().enumerate() {
            *o = i8at(emb.q, base + i) as f32 * f32at(emb.s, (base + i) / gs);
        }

        let (wq, wk, wv, wo) = (m.tensor(1), m.tensor(2), m.tensor(3), m.tensor(4));
        let (w1, w2, w3) = (m.tensor(5), m.tensor(6), m.tensor(7));
        let inv_sqrt_hd = 1.0 / libm::sqrtf(hd as f32);

        for l in 0..c.n_layers {
            // 2. Attention norm, then Q/K/V. K and V go to locals: they are rotated and
            //    quantized into the cache below, never needed at full width afterwards.
            rmsnorm(&mut b.xb[..d], &b.x[..d], m.norm_att(l));
            quantize(&b.xb[..d], gs, &mut b.xq[..d], &mut b.xs);
            let act = QAct {
                q: &b.xq[..d],
                s: &b.xs,
            };
            matmul(&mut b.q[..d], &act, &wq, l * d * d, d, gs);
            let mut kt = [0f32; KV_STRIDE];
            let mut vt = [0f32; KV_STRIDE];
            matmul(&mut kt[..kvd], &act, &wk, l * kvd * d, d, gs);
            matmul(&mut vt[..kvd], &act, &wv, l * kvd * d, d, gs);

            // 3. RoPE — llama2.c's adjacent-pair rotation with a head_dim-relative exponent.
            //    Q gets every pair; K only the first kv_dim (the GQA-shared half).
            for i in (0..d).step_by(2) {
                let freq = 1.0 / libm::powf(10000.0, (i % hd) as f32 / hd as f32);
                let val = pos as f32 * freq;
                let (fcr, fci) = (libm::cosf(val), libm::sinf(val));
                let (q0, q1) = (b.q[i], b.q[i + 1]);
                b.q[i] = q0 * fcr - q1 * fci;
                b.q[i + 1] = q0 * fci + q1 * fcr;
                if i < kvd {
                    let (k0, k1) = (kt[i], kt[i + 1]);
                    kt[i] = k0 * fcr - k1 * fci;
                    kt[i + 1] = k0 * fci + k1 * fcr;
                }
            }

            // 4. Quantize K/V into this layer's cache slot — ONE scale per vector, which is
            //    what makes a 256-deep cache affordable (int8 + f32 scale ≈ 1/4 of f32).
            let slot = l * SEQ_CAP + pos;
            for (src, cache, scales) in [
                (&kt, &mut b.k_cache, &mut b.k_scale),
                (&vt, &mut b.v_cache, &mut b.v_scale),
            ] {
                let mut mx = 0f32;
                for v in &src[..kvd] {
                    let a = libm::fabsf(*v);
                    if a > mx {
                        mx = a;
                    }
                }
                let sc = if mx == 0.0 { 1.0 } else { mx / 127.0 };
                scales[slot] = sc;
                for i in 0..kvd {
                    cache[slot * KV_STRIDE + i] =
                        (src[i] / sc + if src[i] >= 0.0 { 0.5 } else { -0.5 }) as i8;
                }
            }

            // 5. Attention, one head at a time, dequantizing cached K/V on the fly.
            for hh in 0..c.n_heads {
                let (qo, kvo) = (hh * hd, (hh / kv_mul) * hd);
                for t in 0..=pos {
                    let ts = l * SEQ_CAP + t;
                    let mut dot = 0f32;
                    for i in 0..hd {
                        dot += b.q[qo + i] * b.k_cache[ts * KV_STRIDE + kvo + i] as f32;
                    }
                    b.att[t] = dot * b.k_scale[ts] * inv_sqrt_hd;
                }
                softmax(&mut b.att[..=pos]);
                for i in 0..hd {
                    b.xb[qo + i] = 0.0;
                }
                for t in 0..=pos {
                    let ts = l * SEQ_CAP + t;
                    let a = b.att[t] * b.v_scale[ts];
                    for i in 0..hd {
                        b.xb[qo + i] += a * b.v_cache[ts * KV_STRIDE + kvo + i] as f32;
                    }
                }
            }

            // 6. Output projection + residual.
            quantize(&b.xb[..d], gs, &mut b.xq[..d], &mut b.xs);
            matmul(
                &mut b.xb2[..d],
                &QAct {
                    q: &b.xq[..d],
                    s: &b.xs,
                },
                &wo,
                l * d * d,
                d,
                gs,
            );
            for (xv, av) in b.x[..d].iter_mut().zip(b.xb2[..d].iter()) {
                *xv += av;
            }

            // 7. FFN: SwiGLU(w1 x, w3 x) -> w2, + residual.
            rmsnorm(&mut b.xb[..d], &b.x[..d], m.norm_ffn(l));
            quantize(&b.xb[..d], gs, &mut b.xq[..d], &mut b.xs);
            let act = QAct {
                q: &b.xq[..d],
                s: &b.xs,
            };
            matmul(&mut b.hb[..h], &act, &w1, l * h * d, d, gs);
            matmul(&mut b.hb2[..h], &act, &w3, l * h * d, d, gs);
            for (v, g) in b.hb[..h].iter_mut().zip(b.hb2[..h].iter()) {
                *v = *v / (1.0 + libm::expf(-*v)) * g; // SiLU(v) * gate
            }
            // hidden is 172 and gs is 64 — this is the ragged-tail quantize.
            quantize(&b.hb[..h], gs, &mut b.xq[..h], &mut b.xs);
            matmul(
                &mut b.xb2[..d],
                &QAct {
                    q: &b.xq[..h],
                    s: &b.xs,
                },
                &w2,
                l * d * h,
                h,
                gs,
            );
            for (xv, av) in b.x[..d].iter_mut().zip(b.xb2[..d].iter()) {
                *xv += av;
            }
        }

        // 8. Final norm and classifier.
        rmsnorm(&mut b.xb[..d], &b.x[..d], m.norm_final());
        quantize(&b.xb[..d], gs, &mut b.xq[..d], &mut b.xs);
        matmul(
            &mut b.logits[..c.vocab],
            &QAct {
                q: &b.xq[..d],
                s: &b.xs,
            },
            &m.classifier(),
            0,
            d,
            gs,
        );
        &b.logits[..c.vocab]
    }
}
