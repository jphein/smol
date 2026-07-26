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
            && cfg.seq_len != 0;
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
