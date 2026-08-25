//! #349 — **image target identity**: what an image is FOR, so a board can refuse one that
//! isn't for it. PURE (no HAL, no alloc, no `env!` outside the cfg-gated self-identity block)
//! so `experiments/target_guard_verify` `#[path]`-includes this exact file and proves the
//! checker on the host — including proving it REFUSES.
//!
//! # The gap this closes
//!
//! smol's OTA chain proves an image is **authentic** (ed25519 over `build|size|sha256`) and
//! **intact** (readback SHA-256 over the written slot). It proves nothing about
//! **suitability**. Cross-*chip* is caught for free — `esp_image_header_t.chip_id` at image
//! byte 12 is validated by the second-stage bootloader — but that costs a boot-loop to
//! discover, and a cross-*tier* image passes every check smol has **and boots**. Since
//! `smol/ota/staged` is retained fleet-wide, every board sees every announcement.
//!
//! # Design, and why it is shaped like this
//!
//! Prior art (`scratch/parity/multitarget-prior-art.md`): OpenWrt's `fwtool_check_image()` is
//! **data + one checker** — `supported_devices` is a list, `DEVICE_ALT*` are aliases, and the
//! checker itself never changes when a board is renamed. WLED put the same idea in the right
//! PLACE for an ESP — a magic-tagged, hash-validated descriptor embedded in the image, found
//! by a linear scan of the incoming bytes — but gave it the wrong TYPE: one opaque
//! `release_name` string. Because the identity was one label, permitting a single legitimate
//! cross-upgrade required hardcoding a suffix strip *inside the checker*
//! (`normalizeReleaseName()`). So:
//!
//! * **`TargetId` is a structured tuple, never a display string.** Chip, feature bitset, and
//!   persistent-state compat are independent fields, so the checker reasons about each axis on
//!   its own and a new legitimate cross-flash is a change to DATA, not to [`decide`].
//! * **Board VARIANT is deliberately absent.** OLED vs SuperMini is detected at runtime
//!   (`crate::headless()`), and a variant that is not a build artifact needs no OTA guard, no
//!   CI job and no row in a table. Keep it that way; do not add a variant field here.
//!   Since #352 the variant has a home of its own — [`super::profile::BoardProfile`] — and the
//!   two are ORTHOGONAL BY CONSTRUCTION, not by convention: a `TargetId` must be decidable from
//!   an IMAGE ALONE, because that is the entire suitability guard, and a board variant can only
//!   be learned by probing hardware. Anyone tempted to "unify" them should note that the merge
//!   does not merely blur a boundary, it deletes [`decide`]'s ability to run. The one field they
//!   share is `chip`, and `profile` **borrows** [`SELF_CHIP`] from here rather than re-deriving
//!   it — re-deriving it (from `cfg(target_feature = "a")`) was the bug #352 removed.
//! * **It is a WIRE type with a C++-emittable subset** (#331 Phase 1 keeps the stationary Ember
//!   on ESPHome, so a non-Rust fleet member has to be able to emit this). See [`Desc`] below.
//!
//! # The anti-lesson this file is built against
//!
//! WLED's minimum-version half of the same guard is **dead code in shipped firmware**: the
//! descriptor is instantiated with a literal `1` where the constant `WLED_CUSTOM_DESC_VERSION`
//! (`= 2`) belongs, and the check reads `> 1`, so it cannot fire on any image they ship. The
//! disabling `FIXME` survived a release boundary.
//!
//! Two structural defences here, not a comment:
//!
//! 1. [`SELF_DESC`] is `SELF.encode()` — the emitted bytes are *computed from* the same const
//!    the checker compares against. There is no second literal to drift.
//! 2. `const _: () = assert!(...)` below round-trips `decode(encode(SELF)) == SELF` at compile
//!    time, field by field. A literal smuggled into either side fails the BUILD.
//!
//! And every refusal branch in [`decide`] is exercised by `experiments/target_guard_verify`,
//! which asserts the REFUSALS, not just the accept.
//!
//! # Wire format — 16 bytes, little-endian, C++-emittable
//!
//! ```text
//! off  size  field            meaning
//!  0    4    magic            "SMLT" (0x53 0x4D 0x4C 0x54)
//!  4    1    desc_version     descriptor FORMAT version (this file: 1)
//!  5    1    chip             CHIP_ESP32C3=1 / C6=2 / S3=3 / C5=4; 0 = unknown
//!  6    2    features         u16 capability bitset (FEAT_*)
//!  8    1    compat           persistent-state (NVS) layout version this image writes
//!  9    1    min_from_compat  the OLDEST running `compat` this image will install OVER
//! 10    2    reserved         0
//! 12    4    checksum         FNV-1a/32 over bytes 0..12
//! ```
//!
//! The identical struct in C++ (ESPHome external component, #331 Phase 1):
//!
//! ```c
//! struct __attribute__((packed)) smol_target_desc_t {
//!   uint32_t magic;            // 0x544C4D53  ("SMLT" little-endian)
//!   uint8_t  desc_version;     // 1
//!   uint8_t  chip;             // 1=C3 2=C6 3=S3
//!   uint16_t features;         // FEAT_* bitset
//!   uint8_t  compat;
//!   uint8_t  min_from_compat;
//!   uint16_t reserved;         // 0
//!   uint32_t checksum;         // FNV-1a/32 over the first 12 bytes
//! };
//! ```
//!
//! # Where it is checked
//!
//! The descriptor is found by scanning the image bytes as they are **read back out of the
//! written slot** (`ImageWriter::finalize` / `LeafImageWriter::finalize`), not as they stream
//! in. That placement is deliberate and load-bearing: a #267-resumed fetch skips a prefix that
//! is already on flash, so a scan of the *incoming* stream would miss a descriptor in the
//! skipped bytes and report "absent" for a perfectly good image. Reading the slot back is what
//! the SHA gate already does, it is burst-count invariant, and it covers the mesh-relay leaf
//! path with the same code. otadata is still untouched at that point, so a refusal costs a
//! download and nothing else.

#![allow(dead_code)] // the pure core is also compiled by the host verifier, which uses a subset

/// Descriptor magic. Chosen with **no proper border** (no prefix of "SMLT" is also a suffix),
/// which is what makes the naive restart in [`DescScan::feed_byte`] an exact match rather than
/// an approximate one — see the assertion next to it.
pub const MAGIC: [u8; 4] = *b"SMLT";

/// Total descriptor length in bytes.
pub const DESC_LEN: usize = 16;

/// Descriptor FORMAT version understood by this firmware. An image declaring a HIGHER one is
/// refused: we cannot read its fields, therefore we cannot judge suitability, therefore we do
/// not install it. (Unlike WLED's, this comparison is against a named constant that is also
/// what gets encoded — see the compile-time round-trip below.)
pub const DESC_VERSION: u8 = 1;

// --- chip axis -------------------------------------------------------------------------
// A raw u8, NOT a Rust enum: an unknown chip id must stay unknown and compare unequal to
// every known one, rather than being lost in a `_ =>` arm at decode time.
pub const CHIP_UNKNOWN: u8 = 0;
pub const CHIP_ESP32C3: u8 = 1;
pub const CHIP_ESP32C6: u8 = 2;
pub const CHIP_ESP32S3: u8 = 3;
pub const CHIP_ESP32C5: u8 = 4;

/// The chip's operator-facing name — the same spelling `budget.rs` and `build.rs` use, and the
/// segment a per-chip MQTT topic is built from (`smol/ota/staged/esp32c3`).
///
/// One table, because these names are now load-bearing in three places (topic routing, the
/// `SMOL_CHIP` build override, and #348's `ChipBudget.chip`). #348 says outright that **#349
/// owns chip identity**; this is that owner.
pub fn chip_name(chip: u8) -> &'static str {
    match chip {
        CHIP_ESP32C3 => "esp32c3",
        CHIP_ESP32C6 => "esp32c6",
        CHIP_ESP32S3 => "esp32s3",
        CHIP_ESP32C5 => "esp32c5",
        _ => "unknown",
    }
}

// --- feature axis ----------------------------------------------------------------------
// One bit per WIRE-VISIBLE capability. A bitset rather than a "tier" label is the direct
// repair of WLED's opaque `release_name`: the checker can ask "does this image keep the
// radio path I need?" and "does it carry a bench-only tier?" independently, and neither
// question needs a table of tier names or a special case per legitimate cross-flash.
pub const FEAT_WIFI: u16 = 1 << 0;
pub const FEAT_ESPNOW: u16 = 1 << 1;
pub const FEAT_IO: u16 = 1 << 2;
pub const FEAT_CAST: u16 = 1 << 3;
pub const FEAT_WLED: u16 = 1 << 4;
pub const FEAT_BARD: u16 = 1 << 5;
// Bench-only tiers. These perturb a live mesh (mesh-test injects traffic; coexist-soak drives
// the radio; stack-paint repaints the stack region) and must never land on a fleet board.
pub const FEAT_MESH_TEST: u16 = 1 << 6;
pub const FEAT_COEXIST_SOAK: u16 = 1 << 7;
pub const FEAT_STACK_PAINT: u16 = 1 << 8;

/// The bench-only bits, as one mask. Membership is DATA — adding a bench tier is one line
/// here, not a branch in [`decide`].
pub const BENCH_FEATS: u16 = FEAT_MESH_TEST | FEAT_COEXIST_SOAK | FEAT_STACK_PAINT;

/// Features whose LOSS strands the board — after installing an image without these, there is
/// no radio path left to install another one, and recovery is a USB cable.
///
/// `FEAT_ESPNOW` is in here for a non-obvious, verified reason: `run_ota_fetch` is
/// `#[cfg(feature = "espnow")]` (see `ota.rs` — "the wifi-only build reads announces but never
/// fetches"). A wifi-only image therefore **cannot self-update at all**. Dropping espnow is a
/// one-way trip, so it is treated exactly like dropping wifi.
pub const RECOVERY_CRITICAL: u16 = FEAT_WIFI | FEAT_ESPNOW;

/// A structured image identity. Every field is an independent axis; nothing here is a
/// display string, and nothing here is a board VARIANT (that stays runtime-detected).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetId {
    pub desc_version: u8,
    pub chip: u8,
    pub features: u16,
    /// The persistent-state (NVS record) layout this image writes.
    pub compat: u8,
    /// The OLDEST running `compat` this image is willing to install over. This is WLED's
    /// `safe_update_version` — here it is live, encoded from a named constant, and proven to
    /// fire by `experiments/target_guard_verify`.
    pub min_from_compat: u8,
}

/// Why an image was judged unsuitable for THIS board. Terse, stable labels because they ride
/// the retained `smol/<id>/ota/diag` payload, which is packet-capped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TargetReject {
    /// No valid descriptor anywhere in the image. An image that will not say what it is for
    /// cannot be judged suitable, so it is not installed (OpenWrt refuses metadata-less
    /// images for the same reason). Every build from #349 onward carries one, and the
    /// monotonicity gate already blocks older builds, so this fires on foreign or
    /// hand-rolled images.
    Absent,
    /// Descriptor format newer than [`DESC_VERSION`] — unreadable, therefore unjudgeable.
    DescVersion,
    /// Built for a different SoC. The bootloader would also catch this, but only by
    /// boot-looping into rollback; catching it here yields a diagnosis instead.
    Chip,
    /// The image will not install over persistent state this old (`running.compat <
    /// image.min_from_compat`). Escape hatch: USB flash.
    CompatTooOld,
    /// The image drops a [`RECOVERY_CRITICAL`] feature this board currently has — installing
    /// it would leave no way to install anything else.
    FeatureLoss,
    /// The image carries a bench-only tier ([`BENCH_FEATS`]) that this board does not itself
    /// run. Bench images reach bench boards over USB, never over the fleet-wide staged topic.
    FeatureForbidden,
}

impl TargetReject {
    /// Short stable token for the retained diag payload. Kept terse on purpose — the MQTT
    /// packet is capped and DIAG sheds fields under budget pressure.
    pub fn label(self) -> &'static str {
        match self {
            TargetReject::Absent => "tgt-absent",
            TargetReject::DescVersion => "tgt-descver",
            TargetReject::Chip => "tgt-chip",
            TargetReject::CompatTooOld => "tgt-compat",
            TargetReject::FeatureLoss => "tgt-featloss",
            TargetReject::FeatureForbidden => "tgt-bench",
        }
    }

    /// Dense 0-based ordinal, so a caller with a NUMERIC diagnostic channel (the leaf's
    /// `dbg_verdict` byte in the `LDBG` beacon, the `ota_fail` fail-point codes) can carry the
    /// reason at its own base without a second mapping table to keep in sync.
    pub fn code(self) -> u8 {
        match self {
            TargetReject::Absent => 0,
            TargetReject::DescVersion => 1,
            TargetReject::Chip => 2,
            TargetReject::CompatTooOld => 3,
            TargetReject::FeatureLoss => 4,
            TargetReject::FeatureForbidden => 5,
        }
    }

    /// Inverse of [`code`](Self::code). `None` for an unknown ordinal (a record written by a
    /// firmware that knows more rejection reasons than this one does).
    pub fn from_code(c: u8) -> Option<TargetReject> {
        Some(match c {
            0 => TargetReject::Absent,
            1 => TargetReject::DescVersion,
            2 => TargetReject::Chip,
            3 => TargetReject::CompatTooOld,
            4 => TargetReject::FeatureLoss,
            5 => TargetReject::FeatureForbidden,
            _ => return None,
        })
    }

    /// Number of distinct reasons — the width a numeric channel must reserve.
    pub const COUNT: u8 = 6;
}

/// **The whole checker.** Data in, verdict out — no per-board cases, no string normalisation,
/// no chip named anywhere in the body. Order is chosen so the verdict names the most
/// fundamental mismatch first (an S3 image on a C3 is reported as a chip mismatch, not as
/// whatever feature bits happen to differ).
///
/// `running` is this board's own [`TargetId`]; `image` is the one decoded out of the image.
pub fn decide(running: TargetId, image: TargetId) -> Result<(), TargetReject> {
    // 1. Can we even read it?
    if image.desc_version > DESC_VERSION {
        return Err(TargetReject::DescVersion);
    }
    // 2. Same silicon. Exact equality, and CHIP_UNKNOWN (0) matches nothing real.
    if image.chip != running.chip {
        return Err(TargetReject::Chip);
    }
    // 3. Persistent state old enough to be refused by this image.
    if running.compat < image.min_from_compat {
        return Err(TargetReject::CompatTooOld);
    }
    // 4. Do not install something that takes away the way back. DERIVED from what this board
    //    actually has, so a board that never had espnow is not held to it — a fixed required
    //    mask would strand exactly the boards it was meant to protect.
    let required = running.features & RECOVERY_CRITICAL;
    if image.features & required != required {
        return Err(TargetReject::FeatureLoss);
    }
    // 5. Bench tiers. Also DERIVED: a bench board tolerates its own bench bits, a fleet board
    //    tolerates none. No allowlist to maintain, no branch per tier.
    let forbidden = BENCH_FEATS & !running.features;
    if image.features & forbidden != 0 {
        return Err(TargetReject::FeatureForbidden);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Codec
// ---------------------------------------------------------------------------------------

/// FNV-1a/32 — const-evaluable so the checksum is computed at COMPILE time for [`SELF_DESC`]
/// (WLED's constexpr djb2 plays the same role). Its only job is to make a coincidental
/// 4-byte magic match in unrelated image bytes fail to decode.
pub const fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < data.len() {
        h ^= data[i] as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    h
}

impl TargetId {
    /// Serialise to the 16-byte wire descriptor. `const fn` so the embedded static is built at
    /// compile time FROM THE SAME VALUE the checker uses — the structural reason this cannot
    /// drift the way WLED's literal did.
    pub const fn encode(&self) -> [u8; DESC_LEN] {
        let mut d = [0u8; DESC_LEN];
        d[0] = MAGIC[0];
        d[1] = MAGIC[1];
        d[2] = MAGIC[2];
        d[3] = MAGIC[3];
        d[4] = self.desc_version;
        d[5] = self.chip;
        d[6] = (self.features & 0xff) as u8;
        d[7] = (self.features >> 8) as u8;
        d[8] = self.compat;
        d[9] = self.min_from_compat;
        // d[10], d[11] reserved = 0 (inside the checksum, so a future use fails closed here).
        let body = [d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10], d[11]];
        let ck = fnv1a32(&body);
        d[12] = (ck & 0xff) as u8;
        d[13] = ((ck >> 8) & 0xff) as u8;
        d[14] = ((ck >> 16) & 0xff) as u8;
        d[15] = ((ck >> 24) & 0xff) as u8;
        d
    }
}

/// Parse a 16-byte candidate. `None` unless the magic AND the checksum hold — that pair is
/// what lets a linear scan over ~600 KB of arbitrary image bytes trust what it finds.
/// Panic-free; never indexes past `DESC_LEN`.
pub fn decode(rec: &[u8]) -> Option<TargetId> {
    if rec.len() < DESC_LEN {
        return None;
    }
    if rec[0] != MAGIC[0] || rec[1] != MAGIC[1] || rec[2] != MAGIC[2] || rec[3] != MAGIC[3] {
        return None;
    }
    let want = u32::from_le_bytes([rec[12], rec[13], rec[14], rec[15]]);
    if want != fnv1a32(&rec[..12]) {
        return None;
    }
    Some(TargetId {
        desc_version: rec[4],
        chip: rec[5],
        features: u16::from_le_bytes([rec[6], rec[7]]),
        compat: rec[8],
        min_from_compat: rec[9],
    })
}

// ---------------------------------------------------------------------------------------
// Hex form — the SAME 16 bytes, for the signed OTA manifest
// ---------------------------------------------------------------------------------------

/// Hex length of a descriptor carried as a manifest field.
pub const DESC_HEX_LEN: usize = DESC_LEN * 2;

/// Serialise a [`TargetId`] as the lowercase-hex form used in the `OTA2|` manifest.
///
/// **This is deliberately the identical 16 bytes as the embedded descriptor, magic and all** —
/// not a second, tidier encoding of the same facts. One encoding means the publisher can lift
/// the record verbatim out of the image it just built and paste it into the manifest, which
/// makes "the manifest says one thing and the image says another" structurally impossible
/// rather than merely unlikely. (WLED derives its artifact FILENAME from the same define that
/// makes the embedded string, for exactly this reason; the 4 magic bytes are the small price.)
pub fn encode_hex(t: &TargetId) -> [u8; DESC_HEX_LEN] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let raw = t.encode();
    let mut out = [0u8; DESC_HEX_LEN];
    let mut i = 0;
    while i < DESC_LEN {
        out[i * 2] = HEX[(raw[i] >> 4) as usize];
        out[i * 2 + 1] = HEX[(raw[i] & 0x0f) as usize];
        i += 1;
    }
    out
}

/// Parse the hex manifest field back into a [`TargetId`]. `None` on wrong length, a non-hex
/// character, a bad magic, or a bad checksum — the same fail-closed rules as [`decode`], so a
/// mangled manifest field can never be read as a permissive target.
pub fn decode_hex(s: &str) -> Option<TargetId> {
    let b = s.as_bytes();
    if b.len() != DESC_HEX_LEN {
        return None;
    }
    let mut raw = [0u8; DESC_LEN];
    let mut i = 0;
    while i < DESC_LEN {
        raw[i] = (hexval(b[i * 2])? << 4) | hexval(b[i * 2 + 1])?;
        i += 1;
    }
    decode(&raw)
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// The signed manifest M — the string the ed25519 signature covers
// ---------------------------------------------------------------------------------------

/// The fields of a signed OTA manifest M, in either generation:
///
/// * `build|size|sha256hex`             — #32 legacy, `target: None`
/// * `build|size|sha256hex|targethex`   — #349
///
/// This lives HERE, in the pure module, rather than beside the flash writer, for one reason:
/// **M is the exact string the signature covers, and it is also the compatibility boundary that
/// decides whether an un-upgraded fleet gets stranded.** That is not something to leave provable
/// only on hardware. `experiments/target_guard_verify` exercises both generations, including the
/// legacy form a pre-#349 publisher emits, so "old lines still parse" is a test rather than a
/// belief. `ota.rs` wraps this; there is one definition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Manifest {
    pub build: u32,
    pub size: u32,
    pub sha256: [u8; 32],
    pub target: Option<TargetId>,
}

/// Parse M. Fail-closed on anything malformed.
///
/// Strictness that does the work: `splitn(4)` folds any 5th field into the target slot, where
/// [`decode_hex`]'s exact-length + magic + checksum checks reject it; on the 3-field form the
/// exact-64 sha check rejects a trailing tail the same way. A present-but-unparseable target
/// fails the WHOLE manifest rather than degrading to `None` — a corrupted target must never be
/// read as a permissive one.
pub fn parse_manifest_str(m: &str) -> Option<Manifest> {
    let mut it = m.splitn(4, '|');
    let build: u32 = it.next()?.parse().ok()?;
    let size: u32 = it.next()?.parse().ok()?;
    let sha256 = parse_sha256_hex(it.next()?)?;
    let target = match it.next() {
        Some(t) => Some(decode_hex(t)?),
        None => None,
    };
    Some(Manifest { build, size, sha256, target })
}

/// Exactly 64 lowercase/uppercase hex chars → 32 bytes. `None` on any other length.
fn parse_sha256_hex(hex: &str) -> Option<[u8; 32]> {
    let b = hex.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (hexval(b[i * 2])? << 4) | hexval(b[i * 2 + 1])?;
        i += 1;
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Streaming scanner
// ---------------------------------------------------------------------------------------

/// Finds the descriptor in an image fed as arbitrarily-split slices, with 20 bytes of state
/// and one comparison per byte — no fixed offset, because where `.rodata` lands in the image
/// is a linker detail and depending on it is how a guard quietly stops firing. (WLED scans
/// linearly for exactly this reason.)
#[derive(Clone, Copy)]
pub struct DescScan {
    buf: [u8; DESC_LEN],
    /// Bytes of a candidate accumulated so far: `< MAGIC.len()` = still matching the magic.
    n: usize,
    found: Option<TargetId>,
}

impl Default for DescScan {
    fn default() -> Self {
        Self::new()
    }
}

impl DescScan {
    pub const fn new() -> DescScan {
        DescScan { buf: [0u8; DESC_LEN], n: 0, found: None }
    }

    /// Feed one image slice. Chunk boundaries are irrelevant — state carries across calls.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.found.is_some() {
            return;
        }
        for &b in bytes {
            self.feed_byte(b);
            if self.found.is_some() {
                return;
            }
        }
    }

    #[inline]
    fn feed_byte(&mut self, b: u8) {
        if self.n < MAGIC.len() {
            // Naive restart is EXACT here only because MAGIC has no proper border (no prefix
            // of "SMLT" is also a suffix of it); with a bordered magic this would skip real
            // matches. The assertion below is what keeps that true if MAGIC ever changes.
            if b == MAGIC[self.n] {
                self.buf[self.n] = b;
                self.n += 1;
            } else if b == MAGIC[0] {
                self.buf[0] = b;
                self.n = 1;
            } else {
                self.n = 0;
            }
            return;
        }
        self.buf[self.n] = b;
        self.n += 1;
        if self.n == DESC_LEN {
            // First CHECKSUM-VALID candidate wins; a magic that only matched by accident (the
            // scanner's own immediate, say) fails here and scanning continues.
            self.found = decode(&self.buf);
            self.n = 0;
        }
    }

    /// The descriptor found so far, if any.
    pub fn found(&self) -> Option<TargetId> {
        self.found
    }

    /// The verdict for `running`, including the "never said what it was for" case. This is the
    /// single entry point a caller needs.
    pub fn verdict(&self, running: TargetId) -> Result<(), TargetReject> {
        match self.found {
            Some(image) => decide(running, image),
            None => Err(TargetReject::Absent),
        }
    }
}

/// MAGIC must stay border-free for the scanner's restart rule to be exact.
const _: () = {
    let m = MAGIC;
    assert!(m[0] != m[3], "MAGIC has a 1-byte border — DescScan restart would skip matches");
    assert!(!(m[0] == m[2] && m[1] == m[3]), "MAGIC has a 2-byte border");
    assert!(!(m[0] == m[1] && m[1] == m[2]), "MAGIC has a 3-byte border");
};

// ---------------------------------------------------------------------------------------
// This firmware's own identity  (cfg-gated: absent from the host verifier and from tiers
// that have no OTA engine at all)
// ---------------------------------------------------------------------------------------

/// The persistent-state (NVS record) layout this firmware writes. Bump when the `nvs`
/// sector-0 identity record or the CFG record layout changes incompatibly.
#[cfg(feature = "wifi")]
pub const NVS_COMPAT: u8 = 1;

/// The oldest running [`NVS_COMPAT`] this image will install over. `0` = "over anything",
/// which is correct today: no NVS break has shipped. Raising it is a deliberate act that says
/// "boards older than this must be USB-flashed" — and, unlike WLED's, it is the value that is
/// actually ENCODED (see the round-trip assertion) and it is proven to fire in
/// `experiments/target_guard_verify`.
#[cfg(feature = "wifi")]
pub const MIN_FROM_COMPAT: u8 = 0;

/// Chip id stamped by `build.rs` from the target triple (riscv32imc → C3, xtensa-esp32s3 →
/// S3). Derived rather than declared so a chip de-pin (#331/#348) cannot produce an image
/// that misreports what it was built for. `riscv32imac` is deliberately NOT mapped: the C5
/// and the C6 share it, and a guessed chip id is a valid-looking value the suitability check
/// would then trust — an ambiguous triple must fail the build (below) until `SMOL_CHIP`
/// names the silicon.
#[cfg(feature = "wifi")]
pub const SELF_CHIP: u8 = parse_chip(env!("SMOL_CHIP_ID"));

#[cfg(feature = "wifi")]
const fn parse_chip(s: &str) -> u8 {
    let b = s.as_bytes();
    let mut n: u8 = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] >= b'0' && b[i] <= b'9' {
            n = n * 10 + (b[i] - b'0');
        }
        i += 1;
    }
    n
}

/// This firmware's capability bitset, read off the cargo features that are actually enabled.
#[cfg(feature = "wifi")]
const fn self_features() -> u16 {
    let mut f: u16 = 0;
    if cfg!(feature = "wifi") {
        f |= FEAT_WIFI;
    }
    if cfg!(feature = "espnow") {
        f |= FEAT_ESPNOW;
    }
    if cfg!(feature = "io") {
        f |= FEAT_IO;
    }
    if cfg!(feature = "cast") {
        f |= FEAT_CAST;
    }
    if cfg!(feature = "wled") {
        f |= FEAT_WLED;
    }
    if cfg!(feature = "bard") {
        f |= FEAT_BARD;
    }
    if cfg!(feature = "mesh-test") {
        f |= FEAT_MESH_TEST;
    }
    if cfg!(feature = "coexist-soak") {
        f |= FEAT_COEXIST_SOAK;
    }
    if cfg!(feature = "stack-paint") {
        f |= FEAT_STACK_PAINT;
    }
    f
}

/// What THIS image is. Both the embedded descriptor and the checker's "running" side are this
/// one value.
#[cfg(feature = "wifi")]
pub const SELF: TargetId = TargetId {
    desc_version: DESC_VERSION,
    chip: SELF_CHIP,
    features: self_features(),
    compat: NVS_COMPAT,
    min_from_compat: MIN_FROM_COMPAT,
};

/// The descriptor embedded in the image, for OTHER boards to read.
///
/// `#[used]` + `#[no_mangle]` keep it through compiler DCE; it is additionally READ at runtime
/// by [`self_desc_present`], which is what keeps `--gc-sections` from dropping the section.
/// The bytes are `SELF.encode()` — computed, never transcribed.
#[cfg(feature = "wifi")]
#[used]
// edition 2024: `no_mangle` is now an unsafe attribute (symbol-name control).
#[unsafe(no_mangle)]
pub static SMOL_TARGET_DESC: [u8; DESC_LEN] = SELF.encode();

/// Reads the embedded descriptor back and confirms it decodes to [`SELF`]. Called once on the
/// OTA path; its real job is to be a genuine runtime REFERENCE to [`SMOL_TARGET_DESC`] so the
/// linker cannot garbage-collect the very bytes the guard depends on. Returns `false` only if
/// the section went missing — in which case peers will report `tgt-absent` for our images and
/// this board has a build problem, not a fleet problem.
#[cfg(feature = "wifi")]
pub fn self_desc_present() -> bool {
    // Volatile so the read is not const-folded away along with the reference it exists for.
    let mut raw = [0u8; DESC_LEN];
    for (i, b) in raw.iter_mut().enumerate() {
        *b = unsafe { core::ptr::read_volatile(SMOL_TARGET_DESC.as_ptr().add(i)) };
    }
    decode(&raw) == Some(SELF)
}

/// **The WLED anti-lesson, structurally.** Their descriptor is instantiated with a literal `1`
/// where the constant belongs, and the check reads `> 1`, so the gate cannot fire on any
/// shipped image. Here the emitted bytes are decoded back at COMPILE time and every field is
/// compared to the value the checker uses. Smuggling a literal into either side fails the
/// build rather than silently disabling the guard.
#[cfg(feature = "wifi")]
const _: () = {
    let d = SELF.encode();
    assert!(d[0] == MAGIC[0] && d[1] == MAGIC[1] && d[2] == MAGIC[2] && d[3] == MAGIC[3]);
    assert!(d[4] == DESC_VERSION, "encoded desc_version is not DESC_VERSION");
    assert!(d[5] == SELF_CHIP, "encoded chip is not SELF_CHIP");
    assert!(d[6] == (self_features() & 0xff) as u8, "encoded features low byte drifted");
    assert!(d[7] == (self_features() >> 8) as u8, "encoded features high byte drifted");
    assert!(d[8] == NVS_COMPAT, "encoded compat is not NVS_COMPAT");
    assert!(d[9] == MIN_FROM_COMPAT, "encoded min_from_compat is not MIN_FROM_COMPAT");
    // A build must never ship claiming an unknown chip — that would make every peer's chip
    // check meaningless.
    assert!(
        SELF_CHIP != CHIP_UNKNOWN,
        "build.rs could not determine the chip from the target. If the triple is ambiguous \
         (riscv32imac = C5 or C6), name the silicon: SMOL_CHIP=esp32c5 (or esp32c6) cargo build ..."
    );
    // And the OTA-capable tiers must always claim the radio path they actually have.
    assert!(self_features() & FEAT_WIFI != 0);
};
