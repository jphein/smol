//! OTA self-update engine (issue #6) — compiled ONLY in `wifi`/`espnow` builds
//! (`mod ota;` is `#[cfg(feature = "wifi")]` in `main.rs`).
//!
//! # Safety model (see `scratch/smol-ha-batt/ota-firmware-spec.md` §4)
//!
//! We are bare-metal, NOT esp-idf: there is no `esp_ota_*` C runtime. This crate
//! only lets us READ/WRITE the `otadata` partition (slot pointer + state) in the
//! ESP-IDF-compatible format; the actual slot BOOT is done by the on-chip 2nd-stage
//! bootloader. Consequence: **a broken app cannot roll itself back.** So:
//!   * Integrity — a full SHA-256 gate (`ImageWriter::finalize`) runs BEFORE otadata
//!     is ever touched; a corrupt/truncated image is discarded with the good slot
//!     still active. **Proven safe** (no reboot risked).
//!   * Boots-but-unhealthy — [`boot_confirm`] runs a self-test on first boot and, on
//!     failure, the still-running app flips otadata back to the old slot + resets.
//!     Works regardless of the bootloader's rollback config (**app-side = PRIMARY net**).
//!   * Panic/hard-fault — only the bootloader can revert, and only if built with
//!     rollback enabled (UNPROVEN, likely OFF). Mitigation: MF-2 (`custom_halt`→
//!     `software_reset` in `main.rs`) makes a panic RESET, and the CANARY (one board
//!     at a time via the per-id announce topic) is the mass-brick defense.
//!
//! # Authenticity (spec §4b-3, Option B — a CONSCIOUS choice for the trusted home LAN)
//! The announced SHA-256 proves "not corrupted in transit"; it does **NOT** prove
//! "from a trusted source." Whoever can publish to the broker controls both the URL
//! and the hash. v1 posture: **OTA authority == MQTT broker write access**, acceptable
//! only because the broker sits on a trusted VLAN. The URL-host allowlist ([`IMAGE_HOST_ALLOWLIST`])
//! is defence-in-depth, not authentication. Do NOT let any code imply sha256 == trust.

use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
use esp_bootloader_esp_idf::partitions::{read_partition_table, DataPartitionSubType, PartitionType};
use esp_storage::FlashStorage;
// Named only by the download/activation path (the wifi-only build reads announces but
// never fetches, so these would be unused imports there).
#[cfg(feature = "espnow")]
use esp_bootloader_esp_idf::ota::Slot;
#[cfg(feature = "espnow")]
use esp_bootloader_esp_idf::partitions::AppPartitionSubType;

/// This firmware's monotonic build number (git `rev-list --count`), embedded by
/// `build.rs` as `BUILD_NUMBER` and parsed at compile time. The MONOTONICITY gate
/// (spec §4b-1) acts on an announce iff `announced_build > BUILD_NUMBER` — one
/// comparison that blocks BOTH downgrades and retained-announce replay loops.
pub const BUILD_NUMBER: u32 = parse_u32(env!("BUILD_NUMBER"));

/// #33 SINGLE TOGGLE (auto-install vs command). `false` (default, recommended) =
/// install-on-command: a gated announce only advertises `latest_version` to the HA
/// Update entity; the fetch arms ONLY on the native Install button's `install` command.
/// `true` = legacy auto-install: a gated announce fetches on the next burst. Flip this
/// one line to change the posture (closes the reload-misfire class when false, D2).
// Consumed by the espnow main-loop OTA trigger; a wifi-only build compiles the OTA
// engine but has no such trigger, so it's intentionally unread there.
#[allow(dead_code)]
pub const OTA_AUTO_INSTALL: bool = false;

/// Image-host allowlist (spec §4b-5): an announce whose URL host is not one of these
/// is refused BEFORE any socket opens. The real LAN host(s) live in the GIT-IGNORED
/// `crate::secrets` (this repo is PUBLIC — never commit a LAN IP), mirroring how
/// `MQTT_BROKER_IP` is sourced. Rebuild to change the host. Gate logic is unchanged.
pub const IMAGE_HOST_ALLOWLIST: &[&str] = crate::secrets::OTA_IMAGE_HOSTS;

/// Max image = one app slot (`ota_0`/`ota_1` size from `partitions-ota.csv`). An
/// announce larger than this is refused before any flash op; also cross-checked vs
/// the HTTP `Content-Length`.
pub const MAX_IMAGE_SIZE: u32 = 0x1F_0000;

/// Streaming chunk cap. The ESP32-C3 has ~400 KB SRAM vs a ~600 KB image, so the
/// image CANNOT be buffered — it is streamed HTTP-body → ≤4 KB → flash (spec §4b-2).
/// 4096 == the flash sector, so a full stage flush spans exactly one erase unit.
/// (Download-only → `espnow`; the wifi-only build reads announces but never fetches.)
#[cfg(feature = "espnow")]
pub const CHUNK: usize = 4096;

/// Max announce URL length kept in an owned [`Announce`] (bounded, no alloc).
pub const URL_MAX: usize = 160;

/// #32: max bytes of the signed manifest M = `"build|size|sha256hex"` (≤10+1+10+1+64 = 86).
pub const SIGNED_MSG_MAX: usize = 96;

/// Partition-table read scratch (an ESP-IDF table is ≤ 0xC00 bytes). Stack-only,
/// used transiently by the flash helpers.
const PT_SCRATCH: usize = 0xC00;

/// Compile-time decimal `&str` → `u32` (digits only; ignores any stray bytes).
const fn parse_u32(s: &str) -> u32 {
    let b = s.as_bytes();
    let mut n: u32 = 0;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c >= b'0' && c <= b'9' {
            n = n * 10 + (c - b'0') as u32;
        }
        i += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// Announce parsing + gating
// ---------------------------------------------------------------------------

/// A parsed retained OTA announce: `OTA|build|size|sha256hex|url`. OWNED (the URL is
/// copied into a fixed buffer) so it can be stashed between the burst that READS it
/// and the burst that FETCHES it, without borrowing the MQTT receive buffer.
#[derive(Clone, Copy)]
pub struct Announce {
    pub build: u32,
    pub size: u32,
    // Read by the fetch (espnow) integrity gate; parsed but unused in a wifi-only build.
    #[allow(dead_code)]
    pub sha256: [u8; 32],
    // #32: the 64-byte Ed25519 signature over M, and the exact signed manifest bytes.
    // Parsed (written) in every build; READ only by the espnow verify-before-activate →
    // unused in a wifi-only build.
    #[allow(dead_code)]
    sig: [u8; 64],
    #[allow(dead_code)]
    signed_msg: [u8; SIGNED_MSG_MAX],
    #[allow(dead_code)]
    signed_len: usize,
    url: [u8; URL_MAX],
    url_len: usize,
}

impl Announce {
    /// The announce URL as `&str` (validated ASCII at parse; "" on the impossible
    /// non-UTF8 case — panic-free).
    pub fn url(&self) -> &str {
        core::str::from_utf8(&self.url[..self.url_len]).unwrap_or("")
    }
    /// #32: the exact signed manifest M = `"build|size|sha256hex"` wire bytes (espnow verify).
    #[cfg(feature = "espnow")]
    pub fn signed_msg(&self) -> &[u8] {
        &self.signed_msg[..self.signed_len]
    }
    /// #32: the 64-byte Ed25519 signature (espnow verify).
    #[cfg(feature = "espnow")]
    pub fn sig(&self) -> &[u8; 64] {
        &self.sig
    }
}

/// Parse `OTA|build|size|sha256hex|sighex|url` (#32: `sighex` = 128-hex Ed25519 sig; url
/// last so it may contain no `|`). ASCII, decimal build/size. Panic-free — `None` on ANY
/// malformed field. An OLD unsigned 5-field-less announce (`OTA|build|size|sha|url`) fails
/// closed: its `url` lands in the `sig` slot and fails the 128-hex parse.
pub fn parse_announce(payload: &[u8]) -> Option<Announce> {
    let s = core::str::from_utf8(payload).ok()?;
    let rest = s.strip_prefix("OTA|")?;
    // Keep the field &strs so M can be rebuilt from their EXACT wire bytes (no re-serialize).
    let mut it = rest.splitn(5, '|');
    let build_s = it.next()?;
    let size_s = it.next()?;
    let sha_s = it.next()?;
    let sig_s = it.next()?;
    let url = it.next()?;
    let build: u32 = build_s.parse().ok()?;
    let size: u32 = size_s.parse().ok()?;
    let sha256 = parse_hex_n::<32>(sha_s)?;
    let sig = parse_hex_n::<64>(sig_s)?;
    if url.is_empty() || url.len() > URL_MAX || !url.is_ascii() {
        return None;
    }
    // #32: M = the EXACT wire bytes "build|size|sha256hex" (fields 1-3 + their two '|'),
    // reconstructed from the field lengths so it is byte-identical to what the host signed
    // (no decimal/hex re-serialization). M is a prefix of `rest`, so the slice is in-bounds.
    let m_len = build_s.len() + 1 + size_s.len() + 1 + sha_s.len();
    if m_len > SIGNED_MSG_MAX {
        return None;
    }
    let mut signed_msg = [0u8; SIGNED_MSG_MAX];
    signed_msg[..m_len].copy_from_slice(&rest.as_bytes()[..m_len]);
    let mut buf = [0u8; URL_MAX];
    buf[..url.len()].copy_from_slice(url.as_bytes());
    Some(Announce {
        build,
        size,
        sha256,
        sig,
        signed_msg,
        signed_len: m_len,
        url: buf,
        url_len: url.len(),
    })
}

/// `N*2` hex chars → `N` bytes. `None` on wrong length or a non-hex char. Panic-free.
/// (#32 generalized the old `parse_sha256`; used for BOTH the 32-byte sha and the 64-byte sig.)
fn parse_hex_n<const N: usize>(hex: &str) -> Option<[u8; N]> {
    let b = hex.as_bytes();
    if b.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = (hexval(b[i * 2])? << 4) | hexval(b[i * 2 + 1])?;
        i += 1;
    }
    Some(out)
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// #32 — Ed25519 verify-before-activate (AUTHENTICITY). The announced SHA-256 proves
// INTEGRITY (the bytes are what the announce claims); the signature proves the announce
// came from the offline key-holder (AUTHENTICITY) — closing the "sha256 == integrity,
// NOT authentication" gap. We sign the MANIFEST M = "build|size|sha256hex", not the bare
// digest: binding `build` into the signature blocks a rollback/mislabel replay (an old
// genuinely-signed image re-announced under a false higher build#). espnow-only: only
// run_ota_fetch verifies, so a wifi-only build parses the sig field but links NEITHER this
// code NOR the ed25519-compact crate. See issue-32-ed25519-design.md.
// ---------------------------------------------------------------------------

/// #32 fleet root-of-trust — the PUBLIC Ed25519 verify key (32 bytes). PUBLIC by design →
/// committed here (NOT `crate::secrets`); the PRIVATE key lives ONLY in Vaultwarden
/// (securenote `smol-ota-signing-ed25519`, never on disk) and signs in `tools/ota_publish.sh`.
/// Rotating the key = a firmware rebuild. Real key hex:
/// `774f8ad71d3752ffe8f90a7bde1c1e7d334b55cd9ace40e4df2b5f5bd5f76709` (team-lead keygen,
/// JP-authorized; round-trip sign→verify confirmed).
#[cfg(feature = "espnow")]
pub const OTA_SIGNING_PUBKEY: [u8; 32] = [
    0x77, 0x4f, 0x8a, 0xd7, 0x1d, 0x37, 0x52, 0xff, 0xe8, 0xf9, 0x0a, 0x7b, 0xde, 0x1c, 0x1e, 0x7d,
    0x33, 0x4b, 0x55, 0xcd, 0x9a, 0xce, 0x40, 0xe4, 0xdf, 0x2b, 0x5f, 0x5b, 0xd5, 0xf7, 0x67, 0x09,
];

/// #32: Ed25519-verify `signed_msg` (the wire bytes `"build|size|sha256hex"`) against
/// [`OTA_SIGNING_PUBKEY`]. Returns FALSE on ANY error (bad key/sig encoding, or a failed
/// check) — fail-closed. No alloc, no RNG. Called at the integrity gate BEFORE `activate`.
#[cfg(feature = "espnow")]
pub fn verify_signature(signed_msg: &[u8], sig: &[u8; 64]) -> bool {
    let Ok(pk) = ed25519_compact::PublicKey::from_slice(&OTA_SIGNING_PUBKEY) else {
        return false;
    };
    let Ok(s) = ed25519_compact::Signature::from_slice(sig) else {
        return false;
    };
    pk.verify(signed_msg, &s).is_ok()
}

/// Split `http://host[:port]/path` → `(host, port, path)`. Only plaintext `http://`
/// (there is no on-device TLS). Panic-free; `None` on a malformed URL.
pub fn split_url(url: &str) -> Option<(&str, u16, &str)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rfind(':') {
        Some(i) => (&authority[..i], authority[i + 1..].parse().ok()?),
        None => (authority, 80u16),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path))
}

/// Why an announce was refused (logged; the sad path never panics or touches flash).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// `build <= BUILD_NUMBER` — downgrade or stale-retained replay (§4b-1).
    NotNewer,
    /// URL host not on [`IMAGE_HOST_ALLOWLIST`] (§4b-5).
    HostNotAllowed,
    /// `size` is 0 or larger than a slot (§4b-2 bound).
    BadSize,
    /// URL did not parse.
    BadUrl,
}

/// Gate an announce in spec order (§3 Stage C-2): monotonicity → host allowlist →
/// size bound. Authenticity is Option B (documented broker-trust; see the module doc)
/// so there is no signature step. `Ok` means "safe to fetch".
pub fn gate(a: &Announce) -> Result<(), Reject> {
    if a.build <= BUILD_NUMBER {
        return Err(Reject::NotNewer);
    }
    let (host, _port, _path) = split_url(a.url()).ok_or(Reject::BadUrl)?;
    if !IMAGE_HOST_ALLOWLIST.contains(&host) {
        return Err(Reject::HostNotAllowed);
    }
    if a.size == 0 || a.size > MAX_IMAGE_SIZE {
        return Err(Reject::BadSize);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP response helpers (used by the fetch burst in net/wifi.rs)
// ---------------------------------------------------------------------------

/// Index one-past the header terminator (`\r\n\r\n`) — the body start. `None` if the
/// headers are not yet complete in `buf`.
#[cfg(feature = "espnow")]
pub fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Status code from an HTTP/1.x status line (`HTTP/1.0 200 OK` → 200).
#[cfg(feature = "espnow")]
pub fn status_code(headers: &[u8]) -> Option<u16> {
    let line = core::str::from_utf8(headers).ok()?.lines().next()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// `Content-Length` value (case-insensitive header name). `None` if absent/unparseable.
#[cfg(feature = "espnow")]
pub fn content_length(headers: &[u8]) -> Option<u32> {
    let s = core::str::from_utf8(headers).ok()?;
    for line in s.lines() {
        if let Some((name, val)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                return val.trim().parse().ok();
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Streaming image writer (HTTP body → inactive slot + running SHA-256)
// ---------------------------------------------------------------------------

/// Streams a downloaded image into the INACTIVE OTA slot while hashing it on the fly.
/// Never buffers the whole image (spec §4b-2). Word-alignment for `NorFlash::write` is
/// handled by a one-sector stage buffer; the final partial write is 0xFF-padded to a
/// word boundary (the esp-image header carries the true length, so trailing 0xFF in
/// the slot is inert). otadata is NOT touched here — activation is a separate, gated
/// step, so a partial/failed write leaves the good slot booting. (Download-only → `espnow`.)
/// F2 (oracle): the OTA stage buffer, moved OFF the `run_ota_fetch` stack into `.bss`.
/// One sector (`CHUNK`), borrowed once per OTA by [`ImageWriter::begin`]. Alias-safe:
/// OTA is single-caller + one-shot (mesh-deaf, reboots on success), so the `&'static mut`
/// borrow always ends before any next `begin`. `addr_of_mut!` avoids the ref-to-static-mut
/// lint. Init `0xFF` (erased-flash inert), though `flush_stage` re-pads regardless.
#[cfg(feature = "espnow")]
static mut OTA_STAGE: [u8; CHUNK] = [0xFF; CHUNK];

#[cfg(feature = "espnow")]
pub struct ImageWriter {
    flash: FlashStorage,
    /// Absolute flash offset of the target slot.
    base: u32,
    /// Target slot capacity (write bound).
    size: u32,
    /// Real image bytes accepted (== SHA-fed == announced size at finalize).
    written: u32,
    /// Real image bytes committed to flash (stage-flush boundary).
    flushed: u32,
    /// Absolute address erased through (sector granularity).
    erased_upto: u32,
    /// F2 (oracle): the one-sector stage buffer lives in a `static` (see `OTA_STAGE`),
    /// NOT inline — a 4 KB inline array here put it on `run_ota_fetch`'s stack alongside
    /// the 4 KB rx window, overflowing the 8 KB task stack on the download. All
    /// `self.stage[..]` accesses are unchanged (a `&mut [u8; CHUNK]` indexes/slices like
    /// the array did). Alias-safe: one OTA at a time (`begin` is single-caller, one-shot).
    stage: &'static mut [u8; CHUNK],
    stage_len: usize,
    hasher: sha2::Sha256,
    target: Slot,
}

#[cfg(feature = "espnow")]
impl ImageWriter {
    /// Open a writer for the inactive slot (the one `otadata`'s current slot is NOT).
    /// `None` on any partition/flash error (e.g. a board without the OTA table).
    pub fn begin() -> Option<ImageWriter> {
        use sha2::Digest;
        let target = inactive_slot()?;
        let sub = if target == Slot::Slot1 {
            AppPartitionSubType::Ota1
        } else {
            AppPartitionSubType::Ota0
        };
        // Fresh flash + scratch: only `find_partition` is used here (it borrows the
        // parsed table, NOT the flash), so `flash` stays free to move into the writer.
        let mut flash = FlashStorage::new();
        let mut buf = [0u8; PT_SCRATCH];
        let (base, size) = {
            let pt = read_partition_table(&mut flash, &mut buf).ok()?;
            let app = pt.find_partition(PartitionType::App(sub)).ok()??;
            (app.offset(), app.len())
        };
        Some(ImageWriter {
            flash,
            base,
            size,
            written: 0,
            flushed: 0,
            erased_upto: base,
            // F2: borrow the static stage buffer (off the stack). Alias-safe — one OTA
            // at a time. No stale-data risk: `flush_stage` re-pads `[len..padded]` to
            // 0xFF and writes only `stage[..padded]`, so a reused buffer never leaks
            // prior bytes to flash. `stage_len: 0` starts the accumulation fresh.
            stage: unsafe { &mut *core::ptr::addr_of_mut!(OTA_STAGE) },
            stage_len: 0,
            hasher: sha2::Sha256::new(),
            target,
        })
    }

    /// The slot this writer targets (needed by [`activate`] after a good finalize).
    pub fn target(&self) -> Slot {
        self.target
    }

    /// Bytes written so far (for the OLED progress bar).
    pub fn written(&self) -> u32 {
        self.written
    }

    /// Accept one downloaded body slice: feed the hash + stage → flush full sectors.
    /// Returns `false` on overflow past the slot or any flash error (caller aborts;
    /// otadata untouched → good slot stays active).
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        use sha2::Digest;
        if self.written.saturating_add(bytes.len() as u32) > self.size {
            return false; // image claims more than a slot — refuse
        }
        self.hasher.update(bytes);
        self.written += bytes.len() as u32;
        let mut off = 0;
        while off < bytes.len() {
            let take = core::cmp::min(CHUNK - self.stage_len, bytes.len() - off);
            self.stage[self.stage_len..self.stage_len + take]
                .copy_from_slice(&bytes[off..off + take]);
            self.stage_len += take;
            off += take;
            if self.stage_len == CHUNK && !self.flush_stage() {
                return false;
            }
        }
        true
    }

    /// Erase (on first touch) + word-aligned write of the staged bytes. Non-final
    /// flushes are exactly `CHUNK` (word-multiple); the final partial flush 0xFF-pads
    /// to a word boundary.
    fn flush_stage(&mut self) -> bool {
        use embedded_storage::nor_flash::NorFlash;
        let len = self.stage_len;
        if len == 0 {
            return true;
        }
        let erase_to = self.base + self.flushed + len as u32;
        let sector = <FlashStorage as NorFlash>::ERASE_SIZE as u32;
        while self.erased_upto < erase_to {
            let s = self.erased_upto;
            if self.flash.erase(s, s + sector).is_err() {
                return false;
            }
            self.erased_upto = self.erased_upto.saturating_add(sector);
        }
        let ws = <FlashStorage as NorFlash>::WRITE_SIZE;
        let padded = len.div_ceil(ws) * ws;
        for b in self.stage[len..padded].iter_mut() {
            *b = 0xFF;
        }
        if self.flash.write(self.base + self.flushed, &self.stage[..padded]).is_err() {
            return false;
        }
        self.flushed += len as u32;
        self.stage_len = 0;
        true
    }

    /// Flush the tail, then the INTEGRITY GATE: written byte-count == announced size
    /// AND running SHA-256 == announced hash. `true` ⇒ the whole image landed intact
    /// and [`activate`] is safe. otadata is still untouched here.
    pub fn finalize(mut self, expected_size: u32, expected_sha: &[u8; 32]) -> bool {
        use sha2::Digest;
        if !self.flush_stage() {
            return false;
        }
        if self.written != expected_size {
            return false;
        }
        self.hasher.finalize().as_slice() == &expected_sha[..]
    }
}

// ---------------------------------------------------------------------------
// otadata operations: inactive-slot lookup, activation, first-boot confirm
// ---------------------------------------------------------------------------

/// Read `otadata` → the INACTIVE slot (the write target). Self-contained flash borrow
/// (its own `FlashStorage`, dropped on return), so it never pins flash for callers.
#[cfg(feature = "espnow")]
fn inactive_slot() -> Option<Slot> {
    let mut flash = FlashStorage::new();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut buf).ok()?;
    let od = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Ota))
        .ok()??;
    let mut region = od.as_embedded_storage(&mut flash);
    let mut ota = Ota::new(&mut region).ok()?;
    Some(ota.current_slot().ok()?.next())
}

/// Point `otadata` at the freshly-written `target` slot + arm the state machine
/// (`New`), then REBOOT into it. Call ONLY after [`ImageWriter::finalize`] returned
/// true. If the otadata write fails, we do NOT reboot — the good slot stays active.
#[cfg(feature = "espnow")]
pub fn activate(target: Slot, new_build: u32, is_leaf_ota: bool) {
    if set_slot_new(target).is_some() {
        // #40: tag the marker with the NEW image's build# + OTA type. `boot_confirm` self-tests
        // IFF the running build matches (bug #5 — a USB flash of a different build won't match a
        // stale marker), and the deferred LEAF self-test runs only for `is_leaf_ota` (so a
        // self-OTA'd gateway confirms via DHCP, never the hear-a-frame path → no 113↔114 loop).
        mark_ota_activated(new_build, is_leaf_ota);
        log::info!(
            "smol OTA: image verified — activating new slot (build {}, {}), rebooting",
            new_build,
            if is_leaf_ota { "leaf-mesh-OTA" } else { "self-OTA" }
        );
        esp_hal::system::software_reset();
    } else {
        log::error!("smol OTA: activation (otadata write) failed — staying on current image");
    }
}

#[cfg(feature = "espnow")]
fn set_slot_new(target: Slot) -> Option<()> {
    let mut flash = FlashStorage::new();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut buf).ok()?;
    let od = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Ota))
        .ok()??;
    let mut region = od.as_embedded_storage(&mut flash);
    let mut ota = Ota::new(&mut region).ok()?;
    ota.set_current_slot(target).ok()?;
    ota.set_current_ota_state(OtaImageState::New).ok()?;
    Some(())
}

/// #40 BRICK-SAFETY: does `slot` hold a bootable app image? Reads the slot's first word
/// through a partition-scoped region and checks the ESP-IDF app-image magic byte `0xE9`.
/// An erased/empty slot reads `0xFF` → `false`. Used to REFUSE a rollback that would flip
/// otadata to a slot with no image (e.g. a USB-flashed board whose other slot was never
/// written → both-slots-unbootable BRICK). Conservative: any read/partition error → `false`
/// (don't roll back into the unknown). `wifi`-scoped so `boot_confirm` (also `wifi`) can call
/// it; the types resolve from the `esp-bootloader-esp-idf` dep present in every radio build.
#[cfg(feature = "wifi")]
fn slot_has_valid_image(slot: esp_bootloader_esp_idf::ota::Slot) -> bool {
    use embedded_storage::nor_flash::ReadNorFlash;
    use esp_bootloader_esp_idf::ota::Slot;
    use esp_bootloader_esp_idf::partitions::AppPartitionSubType;
    let sub = match slot {
        Slot::Slot0 => AppPartitionSubType::Ota0,
        Slot::Slot1 => AppPartitionSubType::Ota1,
        _ => return false, // >2-slot table we don't use — refuse (brick-safe)
    };
    let mut flash = FlashStorage::new();
    let mut buf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut buf) else {
        return false;
    };
    let Ok(Some(app)) = pt.find_partition(PartitionType::App(sub)) else {
        return false;
    };
    let mut region = app.as_embedded_storage(&mut flash);
    let mut hdr = [0u8; 4]; // word-aligned read of the image header start
    if region.read(0, &mut hdr).is_err() {
        return false;
    }
    hdr[0] == 0xE9 // ESP-IDF app-image magic; erased flash is 0xFF
}

/// MF-1 (spec §4, LOAD-BEARING): first-boot self-test + APP-SIDE rollback. Call VERY
/// EARLY, once per boot, after the WiFi/DHCP result is known (`self_test_passed` =
/// "reached DHCP" — a broken-WiFi image can't, and the just-finished download proves
/// the network is up, so a healthy image won't false-rollback).
///
/// The trigger is `state ∈ {New, PendingVerify}` — NOT `PendingVerify` alone. Because
/// the bootloader never promotes `New → PendingVerify` when its rollback config is OFF
/// (the likely case, spec V1), a `PendingVerify`-only trigger would NEVER run on these
/// boards → no net → brick. Reading `PendingVerify` here also serves as the runtime
/// probe: it means the bootloader DID promote → auto-revert is ON (hard-crash covered).
///
/// Pass ⇒ commit `Valid`. Fail ⇒ flip otadata back to the previous slot, mark it
/// `Valid` (so we don't loop), and reset — the app-side net that works even with the
/// bootloader's auto-revert OFF.
pub fn boot_confirm(self_test_passed: bool) {
    let mut flash = FlashStorage::new();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = match read_partition_table(&mut flash, &mut buf) {
        Ok(pt) => pt,
        Err(_) => return,
    };
    let od = match pt.find_partition(PartitionType::Data(DataPartitionSubType::Ota)) {
        Ok(Some(p)) => p,
        _ => return, // no otadata (non-OTA board) → nothing to confirm
    };
    let mut region = od.as_embedded_storage(&mut flash);
    let mut ota = match Ota::new(&mut region) {
        Ok(o) => o,
        Err(_) => return,
    };
    let state = match ota.current_ota_state() {
        Ok(s) => s,
        Err(_) => return,
    };
    if !matches!(state, OtaImageState::New | OtaImageState::PendingVerify) {
        return; // Valid/Undefined/etc — a normal, already-confirmed boot
    }
    // #40 USB-flash EXEMPTION (build#-tagged, bug #5): self-test ONLY if the RUNNING build# is
    // the one our `activate()` tagged — i.e. this exact OTA image booted. A `New` image whose
    // build# doesn't match the marker (a USB flash, incl. a STALE marker that survived the
    // USB-JTAG reflash, or a power-cycled OTA) must NOT self-test — else a freshly USB-flashed
    // image gets reverted for hearing no mesh frame. It's operator-intended (USB) or already
    // ed25519+sha verified (OTA), so ACCEPT it as-is.
    if !ota_was_activated_for(BUILD_NUMBER) {
        let _ = ota.set_current_ota_state(OtaImageState::Valid);
        log::info!("smol OTA: unconfirmed image but build# doesn't match a fresh OTA activate (USB flash / stale marker) — accepting, no self-test");
        return;
    }
    let bl_auto_revert = matches!(state, OtaImageState::PendingVerify);
    log::info!(
        "smol OTA: unconfirmed image on boot — running self-test (bootloader auto-revert {})",
        if bl_auto_revert { "ON" } else { "OFF/unknown" }
    );
    if self_test_passed {
        let _ = ota.set_current_ota_state(OtaImageState::Valid);
        clear_ota_activated(); // fate decided — don't re-self-test on a later crash-reset
        log::info!("smol OTA: self-test PASS — image CONFIRMED (Valid)");
    } else {
        // #40 BRICK-SAFETY (was a hard brick): roll back ONLY if the target slot holds a
        // valid bootable image. Flipping otadata to an EMPTY/invalid slot (a USB-flashed
        // board's never-written other slot) marks BOTH slots unbootable → "No bootable app
        // partitions" crash-loop. If the other slot has no image, DO NOT flip — ACCEPT the
        // current image (mark Valid, keep running). A USB-flashed image is operator-intended
        // and a mesh-OTA image was ed25519+sha verified before activate, so accepting it is
        // safe; bricking is not.
        let target = ota.current_slot().ok().map(|c| c.next());
        let can_rollback = target.map(slot_has_valid_image).unwrap_or(false);
        clear_ota_activated(); // fate decided either way
        if can_rollback {
            if let Some(t) = target {
                let _ = ota.set_current_slot(t);
            }
            let _ = ota.set_current_ota_state(OtaImageState::Valid);
            log::warn!("smol OTA: self-test FAIL — ROLLING BACK to the previous (valid) slot");
            esp_hal::system::software_reset();
        } else {
            let _ = ota.set_current_ota_state(OtaImageState::Valid);
            log::warn!(
                "smol OTA: self-test FAIL, but the rollback target has NO valid image — ACCEPTING the current image (brick-safe: no flip, no reset)"
            );
        }
    }
}

// ===========================================================================
// #40 leaf-mesh-OTA additions (espnow-only). See `crate::ota_mesh` for the
// transport; this module owns the flash/otadata/NVS engine the leaf path reuses.
// ===========================================================================

/// #40: parse a bare signed manifest `M = "build|size|sha256hex"` (the exact bytes the
/// gateway relays inside an `OTAM`, byte-identical to fields 1–3 of `smol/ota/staged`).
/// Returns `(build, size, sha256)`. Panic-free — `None` on ANY malformed field. The
/// caller MUST have already verified the ed25519 signature over these bytes (§3, the
/// verify-BEFORE-trust order) before acting on the result.
#[cfg(feature = "espnow")]
pub fn parse_manifest(m: &[u8]) -> Option<(u32, u32, [u8; 32])> {
    let s = core::str::from_utf8(m).ok()?;
    let mut it = s.splitn(3, '|');
    let build: u32 = it.next()?.parse().ok()?;
    let size: u32 = it.next()?.parse().ok()?;
    let sha256 = parse_hex_n::<32>(it.next()?)?;
    // The final field must be EXACTLY the 64-hex sha with nothing trailing (splitn(3)
    // would otherwise fold a `build|size|sha|junk` tail into field 3 — parse_hex_n's
    // strict length check rejects that, so a trailing field fails closed).
    Some((build, size, sha256))
}

// ---------------------------------------------------------------------------
// #40 HOLE-3b — the partition-scoped leaf image writer.
//
// The shipped `ImageWriter` writes to ABSOLUTE flash offsets on the whole-flash
// `FlashStorage` (fine for its SEQUENTIAL append). The leaf reassembles from a lossy
// mesh, so a hostile/buggy chunk seq must NEVER be able to write past the inactive
// slot into the running slot / otadata (HOLE-3 = a mid-transfer BRICK, before the
// end-of-transfer verify ever runs). `LeafImageWriter` writes ONLY through a
// `FlashRegion` (`as_embedded_storage`) on the inactive app partition, which
// bounds-checks every erase/write against `[offset, offset+len)` and returns
// `OutOfBounds` otherwise — so a write physically cannot escape the slot regardless
// of any missed logical bound. (a)=the session's signed-bounds check, (b)=this. Both.
//
// It is fed COMPLETED WINDOWS in strict image order (the session releases a window
// only when all its chunks are present), so the flash write stays sequential +
// sector-erase-ahead (identical discipline to `ImageWriter`), and every write offset
// is word-aligned (window offset = k·(64·231) = k·14784, a word multiple). Integrity
// is proven by a READBACK hash of `slot[..size]` at `finalize` (row H / TOCTOU: hash
// the exact bytes that will boot, not an in-RAM copy).
// ---------------------------------------------------------------------------

/// One-window readback scratch for [`LeafImageWriter::finalize`] (off the stack, in
/// `.bss`). Alias-safe: one leaf OTA at a time (canary), single-caller, one-shot.
#[cfg(feature = "espnow")]
static mut OTA_READBACK: [u8; 4096] = [0u8; 4096];

#[cfg(feature = "espnow")]
pub struct LeafImageWriter {
    /// The inactive slot's app-partition subtype (re-found each flush so the
    /// `FlashRegion` borrow — which needs the parsed table + flash together — is
    /// re-derived from locals; identical pattern to `inactive_slot`/`set_slot_new`).
    sub: AppPartitionSubType,
    /// Inactive slot capacity (write bound; a defense-in-depth mirror of the region check).
    part_len: u32,
    /// Real image bytes accepted so far == the next (word-aligned) write offset.
    written: u32,
    /// Partition-relative address erased through (sector granularity, monotonic).
    erased_upto: u32,
    /// The slot [`activate`] targets after a good finalize.
    target: Slot,
}

#[cfg(feature = "espnow")]
impl LeafImageWriter {
    /// Open a writer for the inactive slot. `None` on any partition/flash error.
    pub fn begin() -> Option<LeafImageWriter> {
        let target = inactive_slot()?;
        let sub = if target == Slot::Slot1 {
            AppPartitionSubType::Ota1
        } else {
            AppPartitionSubType::Ota0
        };
        let mut flash = FlashStorage::new();
        let mut buf = [0u8; PT_SCRATCH];
        let part_len = {
            let pt = read_partition_table(&mut flash, &mut buf).ok()?;
            let app = pt.find_partition(PartitionType::App(sub)).ok()??;
            app.len()
        };
        Some(LeafImageWriter { sub, part_len, written: 0, erased_upto: 0, target })
    }

    /// The slot this writer targets (passed to [`activate`] after a good finalize).
    pub fn target(&self) -> Slot {
        self.target
    }

    /// Append one COMPLETED WINDOW's bytes (in strict image order) to the inactive
    /// slot. The write offset is `self.written` (word-aligned by construction — every
    /// non-final window is exactly 64·231 bytes). Erases sector-ahead, then writes the
    /// word-aligned prefix + a 0xFF-padded ≤3-byte tail (the tail only ever occurs on
    /// the FINAL window, so it never misaligns a following write). Returns `false` on
    /// any bound/flash error → the caller ABORTS (otadata untouched, good slot boots).
    pub fn feed_window(&mut self, data: &[u8]) -> bool {
        use embedded_storage::nor_flash::NorFlash;
        let real = data.len() as u32;
        if real == 0 {
            return true;
        }
        // Defense-in-depth spatial bound (the region also enforces this physically).
        if self.written.saturating_add(real) > self.part_len {
            return false;
        }
        // Re-derive the partition-scoped region from locals (table + flash borrowed
        // together for the region's lifetime; dropped at end of this call).
        let mut flash = FlashStorage::new();
        let mut ptbuf = [0u8; PT_SCRATCH];
        let pt = match read_partition_table(&mut flash, &mut ptbuf) {
            Ok(pt) => pt,
            Err(_) => return false,
        };
        let app = match pt.find_partition(PartitionType::App(self.sub)) {
            Ok(Some(p)) => p,
            _ => return false,
        };
        let mut region = app.as_embedded_storage(&mut flash);

        let word = <FlashStorage as NorFlash>::WRITE_SIZE as u32; // 4
        let sector = <FlashStorage as NorFlash>::ERASE_SIZE as u32; // 4096
        let padded = real.div_ceil(word) * word;
        let write_end = self.written + padded;
        // Erase-ahead (sector granularity). Monotonic — never re-erases written bytes.
        while self.erased_upto < write_end {
            let s = self.erased_upto;
            if region.erase(s, s + sector).is_err() {
                return false;
            }
            self.erased_upto = self.erased_upto.saturating_add(sector);
        }
        // Word-aligned prefix, straight from `data` (no copy).
        let prefix = (real & !(word - 1)) as usize;
        if prefix > 0 && region.write(self.written, &data[..prefix]).is_err() {
            return false;
        }
        // Sub-word tail (final window only) — 0xFF-pad to a word (inert; the image
        // header carries the true length). Written at a word-aligned offset.
        let tail = real as usize - prefix;
        if tail > 0 {
            let mut w = [0xFFu8; 4];
            w[..tail].copy_from_slice(&data[prefix..real as usize]);
            if region.write(self.written + prefix as u32, &w).is_err() {
                return false;
            }
        }
        self.written += real;
        true
    }

    /// The INTEGRITY GATE: exact size match AND a READBACK SHA-256 of `slot[..size]`
    /// (the exact bytes that will boot) == the signed sha. `true` ⇒ [`activate`] is
    /// safe. otadata is still untouched here — a failure discards with the good slot
    /// active. The caller has ALREADY verified the ed25519 sig over the manifest.
    pub fn finalize(self, expected_size: u32, expected_sha: &[u8; 32]) -> bool {
        use embedded_storage::nor_flash::ReadNorFlash;
        use sha2::Digest;
        if self.written != expected_size {
            return false;
        }
        let mut flash = FlashStorage::new();
        let mut ptbuf = [0u8; PT_SCRATCH];
        let pt = match read_partition_table(&mut flash, &mut ptbuf) {
            Ok(pt) => pt,
            Err(_) => return false,
        };
        let app = match pt.find_partition(PartitionType::App(self.sub)) {
            Ok(Some(p)) => p,
            _ => return false,
        };
        let mut region = app.as_embedded_storage(&mut flash);
        let buf = unsafe { &mut *core::ptr::addr_of_mut!(OTA_READBACK) };
        let mut hasher = sha2::Sha256::new();
        let mut off = 0u32;
        while off < expected_size {
            let want = core::cmp::min(buf.len() as u32, expected_size - off);
            // Read word-aligned (offset is a multiple of 4096; round the length up so a
            // non-word-aligned tail still reads legally — extra bytes are not hashed).
            let read_len = (want.div_ceil(4) * 4) as usize;
            if region.read(off, &mut buf[..read_len]).is_err() {
                return false;
            }
            hasher.update(&buf[..want as usize]);
            off += want;
        }
        hasher.finalize().as_slice() == &expected_sha[..]
    }
}

// ---------------------------------------------------------------------------
// #40 gateway relay — read the staged image back from a slot to relay over ESP-NOW.
//
// After `run_ota_fetch(relay_mode=true)` stages+verifies a leaf's image into the gateway's
// INACTIVE slot, the relay reads it back window-by-window (partition-scoped, word-aligned)
// and sends the chunks. Read-only; never writes → cannot brick anything.
// ---------------------------------------------------------------------------

#[cfg(feature = "espnow")]
pub struct SlotReader {
    sub: AppPartitionSubType,
    part_len: u32,
}

#[cfg(feature = "espnow")]
impl SlotReader {
    /// Open a reader for `slot` (the gateway's just-staged inactive slot). `None` on error.
    pub fn open(slot: Slot) -> Option<SlotReader> {
        let sub = if slot == Slot::Slot1 {
            AppPartitionSubType::Ota1
        } else {
            AppPartitionSubType::Ota0
        };
        let mut flash = FlashStorage::new();
        let mut buf = [0u8; PT_SCRATCH];
        let part_len = {
            let pt = read_partition_table(&mut flash, &mut buf).ok()?;
            let app = pt.find_partition(PartitionType::App(sub)).ok()??;
            app.len()
        };
        Some(SlotReader { sub, part_len })
    }

    /// Read `out.len()` bytes at partition-relative `off` (both word-aligned by the caller).
    /// `false` on any bound/flash error. Partition-scoped → an out-of-slot `off` errors, it
    /// never reads foreign flash.
    pub fn read(&self, off: u32, out: &mut [u8]) -> bool {
        use embedded_storage::nor_flash::ReadNorFlash;
        if off.saturating_add(out.len() as u32) > self.part_len {
            return false;
        }
        let mut flash = FlashStorage::new();
        let mut ptbuf = [0u8; PT_SCRATCH];
        let pt = match read_partition_table(&mut flash, &mut ptbuf) {
            Ok(pt) => pt,
            Err(_) => return false,
        };
        let app = match pt.find_partition(PartitionType::App(self.sub)) {
            Ok(Some(p)) => p,
            _ => return false,
        };
        let mut region = app.as_embedded_storage(&mut flash);
        region.read(off, out).is_ok()
    }
}

// ---------------------------------------------------------------------------
// #40 §3C — the persistent signed-freshness FLOOR (NVS-backed).
//
// `fresh_floor` = the highest build ever booted-as-`New`. The leaf accepts a mesh OTA
// iff `sig ok ∧ build > running ∧ build > fresh_floor ∧ size/sha ok`, closing the
// signed-INTERMEDIATE / rolled-back-build replay (a genuinely-signed old build re-pushed
// over the unauth mesh). FLOOR-ONLY, #40-local: NO epoch, zero change to #32's signed M.
//
// ⚠️ #40 CLAIMS THE `nvs` PARTITION as a raw persistent store (smol uses no ESP-IDF NVS
// today — verified). This is NOT the ESP-IDF NVS key/value format; if a future feature
// ever needs real NVS, give it a different partition or coordinate this offset. All
// access is through a partition-scoped `FlashRegion` (OOB-safe, like the slot writer),
// so a bug here can only corrupt the floor cell — never the app slots or otadata, and
// never a brick (the sig + monotonicity gates still hold; the floor is defense-in-depth).
//
// Layout: two ping-pong cells at nvs relative offsets 0 and 4096, each a 12-byte record
// `[MAGIC(4 LE) | build(4 LE) | crc32(4 LE)]`. Read = max valid build across both cells.
// Write = erase the cell holding the SMALLER/invalid build, write the new record there;
// the other cell always retains ≥ the erased one, so a torn write never regresses the
// floor (power-loss-safe). One erase+write per OTA → negligible wear.
// ---------------------------------------------------------------------------

#[cfg(feature = "espnow")]
const FLOOR_MAGIC: u32 = 0x736D_6C46; // "smlF"
#[cfg(feature = "espnow")]
const FLOOR_CELL_STRIDE: u32 = 4096; // one sector apart (erase granularity)
#[cfg(feature = "espnow")]
const FLOOR_RECORD_LEN: usize = 12;

/// CRC-32 (IEEE, reflected poly 0xEDB88320) over `data` — torn-write detector for the
/// floor cell. Pure, panic-free, no table (small input, speed irrelevant).
#[cfg(feature = "espnow")]
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        let mut i = 0;
        while i < 8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            i += 1;
        }
    }
    !crc
}

/// Read one floor cell at `rel` (nvs-relative). `None` on flash error / bad magic / bad crc.
#[cfg(feature = "espnow")]
fn floor_cell_read(rel: u32) -> Option<u32> {
    use embedded_storage::nor_flash::ReadNorFlash;
    let mut flash = FlashStorage::new();
    let mut ptbuf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut ptbuf).ok()?;
    let nvs = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()??;
    let mut region = nvs.as_embedded_storage(&mut flash);
    let mut rec = [0u8; FLOOR_RECORD_LEN];
    region.read(rel, &mut rec).ok()?;
    let magic = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
    if magic != FLOOR_MAGIC {
        return None; // erased (0xFFFFFFFF) or never written
    }
    let build = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
    let crc = u32::from_le_bytes([rec[8], rec[9], rec[10], rec[11]]);
    (crc == crc32(&rec[0..8])).then_some(build)
}

/// The current freshness floor = max valid build across both cells (0 if none written).
#[cfg(feature = "espnow")]
pub fn fresh_floor_get() -> u32 {
    let a = floor_cell_read(0).unwrap_or(0);
    let b = floor_cell_read(FLOOR_CELL_STRIDE).unwrap_or(0);
    a.max(b)
}

/// Raise the floor to `max(floor, build)`, persisting it. Idempotent (a no-op if the
/// floor already covers `build`). Writes to the cell holding the SMALLER/invalid build
/// so the other cell always retains the prior floor → a torn write cannot regress it.
/// Called once at first-boot-pre-self-test on `otadata == New` (before radio init).
#[cfg(feature = "espnow")]
pub fn fresh_floor_bump(build: u32) {
    use embedded_storage::nor_flash::NorFlash;
    let a = floor_cell_read(0);
    let b = floor_cell_read(FLOOR_CELL_STRIDE);
    let cur = a.unwrap_or(0).max(b.unwrap_or(0));
    if build <= cur {
        return; // already covered — no write, no wear
    }
    // Target the cell with the smaller (or invalid) build; the other keeps the prior max.
    let target_rel = if a.unwrap_or(0) <= b.unwrap_or(0) { 0 } else { FLOOR_CELL_STRIDE };
    let mut rec = [0u8; FLOOR_RECORD_LEN];
    rec[0..4].copy_from_slice(&FLOOR_MAGIC.to_le_bytes());
    rec[4..8].copy_from_slice(&build.to_le_bytes());
    let crc = crc32(&rec[0..8]);
    rec[8..12].copy_from_slice(&crc.to_le_bytes());

    let mut flash = FlashStorage::new();
    let mut ptbuf = [0u8; PT_SCRATCH];
    let pt = match read_partition_table(&mut flash, &mut ptbuf) {
        Ok(pt) => pt,
        Err(_) => return,
    };
    let nvs = match pt.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)) {
        Ok(Some(p)) => p,
        _ => return,
    };
    let mut region = nvs.as_embedded_storage(&mut flash);
    if region.erase(target_rel, target_rel + FLOOR_CELL_STRIDE).is_err() {
        return;
    }
    let _ = region.write(target_rel, &rec);
}

// ---------------------------------------------------------------------------
// #40 §3/self-test §3 — the unconfirmed-boot (K) counter.
//
// A `New` image that PANICS before finishing the leaf self-test resets (MF-2
// custom_halt→software_reset) STILL in `New` → it would re-run the self-test and could
// boot-loop the bad slot forever (bootloader auto-revert is OFF). This bounds it: the
// counter is bumped at the ABSOLUTE EARLIEST boot point (before any subsystem that can
// panic); at K it forces the app-side flip to the good slot. Stored in RTC-fast RAM —
// survives `software_reset`, cleared on power-loss (acceptable: a power-cycled leaf
// re-runs the self-test cleanly, §5). Persistent RTC RAM is UNINITIALIZED at power-on,
// so a magic word gates it: garbage magic ⇒ treat as a fresh (count 0) power-on.
// ---------------------------------------------------------------------------

/// `[magic, count]` in persistent RTC-fast RAM. `#[ram(rtc_fast, persistent)]` keeps it
/// across a `software_reset`; it is uninitialized at power-on (magic detects that).
#[cfg(feature = "espnow")]
#[esp_hal::ram(rtc_fast, persistent)]
static mut OTA_BOOT_GUARD: [u32; 2] = [0u32; 2];

#[cfg(feature = "espnow")]
const BOOT_GUARD_MAGIC: u32 = 0x736D_6C4B; // "smlK"

// ---------------------------------------------------------------------------
// #40 USB-flash EXEMPTION — only a genuine OTA-`activate()` of THIS EXACT image self-tests.
//
// A USB-flashed image boots as `New` (unconfirmed) just like an OTA image, so `boot_confirm`
// couldn't tell them apart → EVERY USB flash ran the self-test → false rollback (or, before
// the brick-safe fix, a brick). We tag an RTC-fast marker with the ACTIVATED image's build#
// inside `activate()`; `boot_confirm` self-tests ONLY when the RUNNING `BUILD_NUMBER` matches
// the marker's build#.
//
// ⚠️ CRITICAL (bug #5): a bare "an OTA happened" flag is NOT enough — RTC-fast RAM SURVIVES a
// USB-JTAG reflash (`rst:0x15` is a peripheral reset, NOT power-off; only a true power cycle
// clears it). A board that self-OTA'd earlier and stayed powered would carry a STALE marker
// into a USB flash → false self-test → rollback (exactly what bricked/reverted id7). Tagging
// the SPECIFIC build# fixes it: a USB flash of a DIFFERENT build won't match the stale marker
// → accepted as operator-confirmed. (Re-flashing the byte-identical build# is a harmless
// edge — it'd self-test, but brick-safely.) `wifi`-scoped so `boot_confirm` can read it.
// ---------------------------------------------------------------------------

/// `[magic, activated_build, is_leaf_ota]` in persistent RTC-fast RAM (survives
/// `software_reset` AND a USB-JTAG reflash; only a power cycle clears it). `activated_build`
/// = the build# passed to the `activate()` that produced this boot chain (matched against the
/// running `BUILD_NUMBER`); `is_leaf_ota` = 1 for a mesh-OTA (confirm via hear-a-frame), 0 for
/// a self-OTA (confirm via reached-DHCP). The OTA TYPE, not the runtime role, decides the
/// confirm path — a self-OTA'd gateway that transiently misses DHCP at one boot must NOT be
/// rolled back by the leaf hear-a-frame path (the 113↔114 oscillation), and its role is
/// ambiguous at that boot anyway.
#[cfg(feature = "wifi")]
#[esp_hal::ram(rtc_fast, persistent)]
static mut OTA_ACTIVATE_GUARD: [u32; 3] = [0u32; 3];

#[cfg(feature = "wifi")]
const ACTIVATE_GUARD_MAGIC: u32 = 0x736D_6C41; // "smlA"

/// Tag the marker with the build# + OTA type of the image being activated (call inside
/// `activate()`, passing the NEW image's build# and whether this is a LEAF mesh-OTA). Survives
/// the reset AND a USB reflash; only a power cycle clears it. `espnow`-scoped (only `activate()`
/// — itself espnow — sets it; wifi-only never OTAs).
#[cfg(feature = "espnow")]
pub fn mark_ota_activated(build: u32, is_leaf_ota: bool) {
    unsafe {
        let g = &mut *core::ptr::addr_of_mut!(OTA_ACTIVATE_GUARD);
        g[0] = ACTIVATE_GUARD_MAGIC;
        g[1] = build;
        g[2] = is_leaf_ota as u32;
    }
}

/// True iff the RUNNING image (`running_build`) is the exact image our `activate()` produced —
/// i.e. a genuine OTA of THIS build, not a USB flash (whose build# won't match a stale marker)
/// nor uninitialized RTC RAM (bad magic ⇒ false).
#[cfg(feature = "wifi")]
pub fn ota_was_activated_for(running_build: u32) -> bool {
    unsafe {
        let g = &*core::ptr::addr_of!(OTA_ACTIVATE_GUARD);
        g[0] == ACTIVATE_GUARD_MAGIC && g[1] == running_build && running_build != 0
    }
}

/// True iff the current OTA-activate boot chain is a LEAF mesh-OTA (confirm via hear-a-frame).
/// A self-OTA (is_leaf_ota=0) confirms via reached-DHCP only and NEVER runs the deferred leaf
/// self-test — so a transient DHCP miss can't roll a self-OTA'd gateway back on a quiet mesh.
/// `cfg(espnow)`, not `wifi`: the ONLY caller is the `#[cfg(espnow)]` deferred-leaf-selftest gate
/// in `main`; a wifi-only (gateway) build confirms via DHCP and never reads this (would warn-unused).
#[cfg(feature = "espnow")]
pub fn ota_activated_is_leaf() -> bool {
    unsafe {
        let g = &*core::ptr::addr_of!(OTA_ACTIVATE_GUARD);
        g[0] == ACTIVATE_GUARD_MAGIC && g[2] == 1
    }
}

/// Clear the activate marker (call once the image's fate is decided — confirmed / rolled
/// back / accepted — so a later crash-reset doesn't re-run the self-test on the same chain).
#[cfg(feature = "wifi")]
fn clear_ota_activated() {
    unsafe {
        let g = &mut *core::ptr::addr_of_mut!(OTA_ACTIVATE_GUARD);
        g[0] = ACTIVATE_GUARD_MAGIC;
        g[1] = 0;
        g[2] = 0;
    }
}

/// Bump the unconfirmed-boot counter and return the new value. Call at the absolute
/// earliest boot point when `otadata` is `New`/`PendingVerify`, BEFORE anything that
/// can panic in init (so an early-init crash still trips the crash-loop bound).
#[cfg(feature = "espnow")]
pub fn unconfirmed_boot_bump() -> u32 {
    unsafe {
        let g = &mut *core::ptr::addr_of_mut!(OTA_BOOT_GUARD);
        if g[0] != BOOT_GUARD_MAGIC {
            g[0] = BOOT_GUARD_MAGIC;
            g[1] = 0;
        }
        g[1] = g[1].saturating_add(1);
        g[1]
    }
}

/// Reset the counter (call on a successful self-test confirm).
#[cfg(feature = "espnow")]
pub fn unconfirmed_boot_reset() {
    unsafe {
        let g = &mut *core::ptr::addr_of_mut!(OTA_BOOT_GUARD);
        g[0] = BOOT_GUARD_MAGIC;
        g[1] = 0;
    }
}

/// True iff `otadata`'s state is `New`/`PendingVerify` — i.e. a freshly-activated image
/// on its FIRST boot, whose self-test has not yet confirmed. Drives the earliest-boot
/// freshness-floor bump + K-counter (and the deferred leaf self-test in the main loop).
/// Self-contained flash borrow; `false` on any flash/partition error (fail-safe: a board
/// we can't read otadata on simply skips the OTA bookkeeping).
#[cfg(feature = "espnow")]
pub fn otadata_unconfirmed() -> bool {
    let mut flash = FlashStorage::new();
    let mut buf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut buf) else {
        return false;
    };
    let Ok(Some(od)) = pt.find_partition(PartitionType::Data(DataPartitionSubType::Ota)) else {
        return false;
    };
    let mut region = od.as_embedded_storage(&mut flash);
    let Ok(mut ota) = Ota::new(&mut region) else {
        return false;
    };
    matches!(
        ota.current_ota_state(),
        Ok(OtaImageState::New) | Ok(OtaImageState::PendingVerify)
    )
}
