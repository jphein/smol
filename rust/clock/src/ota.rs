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
    url: [u8; URL_MAX],
    url_len: usize,
}

impl Announce {
    /// The announce URL as `&str` (validated ASCII at parse; "" on the impossible
    /// non-UTF8 case — panic-free).
    pub fn url(&self) -> &str {
        core::str::from_utf8(&self.url[..self.url_len]).unwrap_or("")
    }
}

/// Parse `OTA|build|size|sha256hex|url` (ASCII, decimal build/size, 64-hex sha, url
/// last so it may contain no `|`). Panic-free — returns `None` on ANY malformed field.
pub fn parse_announce(payload: &[u8]) -> Option<Announce> {
    let s = core::str::from_utf8(payload).ok()?;
    let rest = s.strip_prefix("OTA|")?;
    let mut it = rest.splitn(4, '|');
    let build: u32 = it.next()?.parse().ok()?;
    let size: u32 = it.next()?.parse().ok()?;
    let sha256 = parse_sha256(it.next()?)?;
    let url = it.next()?;
    if url.is_empty() || url.len() > URL_MAX || !url.is_ascii() {
        return None;
    }
    let mut buf = [0u8; URL_MAX];
    buf[..url.len()].copy_from_slice(url.as_bytes());
    Some(Announce {
        build,
        size,
        sha256,
        url: buf,
        url_len: url.len(),
    })
}

/// 64 hex chars → 32 bytes. `None` on wrong length or a non-hex char.
fn parse_sha256(hex: &str) -> Option<[u8; 32]> {
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

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
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
    stage: [u8; CHUNK],
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
            stage: [0xFF; CHUNK],
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
pub fn activate(target: Slot) {
    if set_slot_new(target).is_some() {
        log::info!("smol OTA: image verified — activating new slot, rebooting");
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
    let bl_auto_revert = matches!(state, OtaImageState::PendingVerify);
    log::info!(
        "smol OTA: unconfirmed image on boot — running self-test (bootloader auto-revert {})",
        if bl_auto_revert { "ON" } else { "OFF/unknown" }
    );
    if self_test_passed {
        let _ = ota.set_current_ota_state(OtaImageState::Valid);
        log::info!("smol OTA: self-test PASS — image CONFIRMED (Valid)");
    } else {
        if let Ok(cur) = ota.current_slot() {
            let _ = ota.set_current_slot(cur.next());
        }
        let _ = ota.set_current_ota_state(OtaImageState::Valid);
        log::warn!("smol OTA: self-test FAIL — ROLLING BACK to the previous slot");
        esp_hal::system::software_reset();
    }
}
