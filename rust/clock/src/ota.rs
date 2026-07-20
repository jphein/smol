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
//! only because the broker sits on a trusted VLAN. The URL-host allowlist (`host_allowed`)
//! is defence-in-depth, not authentication. Do NOT let any code imply sha256 == trust.

use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
// #233: esp-bootloader 0.5 made the `Slot` enum PRIVATE. The public currency is now
// `AppPartitionSubType` (the APP partition you name), so smol's OTA engine uses that
// throughout: Slot0↔Ota0, Slot1↔Ota1, blank-otadata↔Factory (what current_app_partition()
// returns on a blank/uninitialised record — smol has no factory partition, so the ROM boots
// ota_0 and the true inactive target is Ota1).
use esp_bootloader_esp_idf::partitions::{
    read_partition_table, AppPartitionSubType, DataPartitionSubType, PartitionType,
};
use esp_storage::FlashStorage;

/// #233: esp-storage 0.9's `FlashStorage::new` CONSUMES the FLASH peripheral. Its
/// "Panics if called more than once" doc is the *compile-time move contract* on the
/// esp-hal peripheral singleton, NOT a runtime check — two independent source-reads
/// (scratch/233-upgrade/flashstorage-steal-verdict.md) confirmed there is NO once-guard
/// anywhere in esp-storage 0.9 (the lone `.unwrap()` is capacity-detect), and esp-hal
/// 1.1.1's `FLASH::steal()` fabricates the zero-state token unconditionally. So a
/// freshly-stolen per-op handle reproduces the exact semantics of the old 0.7 throwaway
/// `FlashStorage::new()` — each call just re-reads the constant flash-size header. This
/// avoids threading the FLASH peripheral through the whole radio stack
/// (main → mode → RadioManager → run_ota_fetch) and dodges a `&mut FlashStorage` borrow
/// held across the streaming write.
///
/// ⚠️ HUMAN-AUDITED INVARIANT (steal() moves the exclusivity guarantee from the compiler
/// to us — the risk class is a torn write from a concurrent flash user overlapping an
/// in-flight OTA op):
/// - Callers must NEVER hold two `FlashStorage` instances live-and-in-use at once.
/// - ALL flash access stays on the main loop: OTA is strictly serial (one
///   `ImageWriter`/`LeafImageWriter` at a time; the otadata/identity helpers run before
///   begin / after finalize, never concurrently), and no ISR touches flash.
///
/// This holds STRUCTURALLY in the non-embassy superloop. #198 (embassy-net migration):
/// re-evaluate this deliberately — once flash users can interleave at await points, move
/// to a single owned instance (StaticCell/once-init here in `ota::`) instead of per-op
/// steal. Do not carry this helper into an async world unexamined.
#[cfg(feature = "wifi")]
fn flash() -> FlashStorage<'static> {
    FlashStorage::new(unsafe { esp_hal::peripherals::FLASH::steal() })
}

/// The OTHER app partition — the inactive slot to write / roll back to. Replaces the old
/// `Slot::next()` (now private in 0.5). Blank otadata reports `Factory`; smol has no factory
/// partition (ROM boots ota_0), so `Factory`/`Ota0` ⇒ the inactive target is `Ota1`.
#[cfg(feature = "wifi")]
fn other_app(cur: AppPartitionSubType) -> AppPartitionSubType {
    match cur {
        AppPartitionSubType::Ota1 => AppPartitionSubType::Ota0,
        _ => AppPartitionSubType::Ota1, // Ota0 | Factory(blank) | anything else
    }
}

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

/// Image-host allowlist test (spec §4b-5): an announce whose URL host is not allowed is refused
/// BEFORE any socket opens. The real LAN host(s) live in the GIT-IGNORED `crate::secrets` (this repo
/// is PUBLIC — never commit a LAN IP). #142: the base allowlist is the single baked network's
/// `ota_hosts` (`crate::secrets::WIFI_NETWORK`), PLUS the NVS CFG-`O` override (one extra RFC1918
/// host, dashboard-set). Read at gate time — no reboot. A bad override can only ever REFUSE a fetch
/// (it is merely an additional allowed host); the image itself stays monotonicity-gated, so this is
/// defense-in-depth, never a brick surface.
#[cfg(feature = "wifi")]
fn host_allowed(host: &str) -> bool {
    let cfg = read_net_cfg();
    if crate::secrets::WIFI_NETWORK.ota_hosts.contains(&host) {
        return true;
    }
    // Stage 3: the CFG-`O` override — one extra RFC1918 host, matched by parsing the URL host.
    matches!(cfg.and_then(|c| c.ota_host), Some(ovr) if parse_ipv4(host) == Some(ovr))
}

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
/// #153: OTA transfer progress surfaced to the UI so `main`'s tick can paint the 1-px
/// bottom progress edge that replaced the retired full-screen syncing overlay. `done`/
/// `total` are bytes (self-fetch) or chunks (leaf relay) — the renderer only needs the
/// ratio. Threaded as a `&Cell<OtaProgress>` into the fetch/relay so the UI-agnostic
/// `net/` layer writes the counts without ever touching the display.
// #172: constructed only on the espnow OTA fetch/relay paths (run_ota_fetch / mode /
// main's OTA closures — all cfg(espnow)); keep the type in every build but silence
// dead-code in a wifi-without-espnow build, matching the `Announce` field idiom below.
// Lets `clippy --features wifi -D warnings` pass on all tiers.
#[cfg_attr(not(feature = "espnow"), allow(dead_code))]
#[derive(Clone, Copy, Default)]
pub struct OtaProgress {
    pub done: u32,
    pub total: u32,
}

/// #161: a read-only snapshot of the mesh-OTA a RECEIVING leaf is currently taking, for the
/// dedicated on-board OTA screen (`crate::ota_screen`). `main` reads it each tick via
/// [`crate::net::mode::RadioManager::ota_rx_view`] and, while it is `Some`, paints the OTA
/// screen OVER the frozen app frame — auto-activated by an inbound transfer, never a menu
/// item. Pure PRESENTATION of state the leaf receive session (`OtaLeafSession`) already
/// tracks: no new telemetry, no wire/format change. Copy + tiny so it crosses the borrow
/// (read `ctx.radio`, release, then draw with `ctx.display`) as a value. espnow-only — the
/// leaf receive path only exists in the mesh build.
#[cfg(feature = "espnow")]
#[derive(Clone, Copy)]
pub struct OtaRxView {
    /// The feeding gateway's logical id when the roster can place its MAC (`None` = a MAC we
    /// can't name yet → the screen says "from mesh" rather than inventing an id).
    pub source_id: Option<u8>,
    /// ESP-NOW hops from the source. Leaf-mesh-OTA is single-hop today (gateway→leaf), so this
    /// is 1; kept a field so a future multi-hop relay can surface the true distance unchanged.
    pub hop: u8,
    /// The incoming image's monotonic build number (from the SIGNED manifest) — drives the
    /// "→ v<n>" line + its deterministic FORGE codename ("what you're becoming").
    pub build: u32,
    /// Image blocks committed so far / total blocks (the `k/n` readout + the bar fill ratio).
    pub done: u32,
    pub total: u32,
}

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
    /// URL host not allowed by `host_allowed` (§4b-5).
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
    if !host_allowed(host) {
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
        if let Some((name, val)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length") {
                return val.trim().parse().ok();
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
    // #233: esp-storage 0.9 FlashStorage is lifetime'd; the writer owns a 'static handle
    // (from the stolen FLASH peripheral via `flash()`), held for the whole streaming write.
    flash: FlashStorage<'static>,
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
    target: AppPartitionSubType,
}

#[cfg(feature = "espnow")]
impl ImageWriter {
    /// Open a writer for the inactive slot (the one `otadata`'s current slot is NOT).
    /// `None` on any partition/flash error (e.g. a board without the OTA table).
    pub fn begin() -> Option<ImageWriter> {
        use sha2::Digest;
        let target = inactive_slot()?;
        // #233: `target` is already the AppPartitionSubType to write (inactive_slot → other_app,
        // always Ota0/Ota1), so the write-target partition IS the target.
        let sub = target;
        // Fresh flash + scratch: only `find_partition` is used here (it borrows the
        // parsed table, NOT the flash), so `flash` stays free to move into the writer.
        let mut flash = flash();
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
    pub fn target(&self) -> AppPartitionSubType {
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
        let sector = <FlashStorage<'_> as NorFlash>::ERASE_SIZE as u32;
        while self.erased_upto < erase_to {
            let s = self.erased_upto;
            if self.flash.erase(s, s + sector).is_err() {
                return false;
            }
            self.erased_upto = self.erased_upto.saturating_add(sector);
        }
        let ws = <FlashStorage<'_> as NorFlash>::WRITE_SIZE;
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
// #40 IDENTITY: runtime node-id persisted in the `nvs` data partition
// ---------------------------------------------------------------------------
// The compile-time `crate::NODE_ID` is only a FACTORY SEED. A single OTA image is
// shared by the whole fleet — `ota_publish.sh` builds ONE image with no per-node
// `SMOL_NODE_ID`, so a leaf that installs it would otherwise boot with the baked
// default id (7) → a stolen identity → a duplicate-id roster → the gateway's confirm
// waits for a STAT from an id that no longer exists → a structural leaf-timeout.
//
// Fix: the TRUE id lives in NVS, in `nvs` SECTOR 0 (offset 0, 16 B). The OTA image
// transfer writes ONLY the inactive app slot + `otadata` — never `nvs` — so identity
// survives ANY image. The freshness-floor bump (below) DOES write `nvs`, but only sectors
// 1-2 (offsets ≥ 4096); it never touches sector 0 (oracle F1 — a floor bump erases a whole
// sector, so identity and the floor cells MUST live in different sectors, or the bump wipes
// the id → fleet reverts to the baked default 7). A USB flash (`espflash erase-flash` +
// flash) wipes NVS to 0xFF; the first boot then SEEDS sector 0 from the baked const — which
// the USB flow sets correctly via `SMOL_NODE_ID=<n>`. Steady state: NVS is the id's truth.
//
// BRICK-SAFE: any flash/partition/parse error → fall back to the baked const and do
// NOT write. The seed is best-effort (a failed seed just retries next boot). The
// record is self-validating (magic + version + id + ~id + checksum) so an erased or
// corrupt sector is REJECTED (→ reseed), never misread as an id.

/// Identity record magic ("smol identity, format v1").
#[cfg(feature = "wifi")]
const IDENT_MAGIC: [u8; 4] = *b"SMi1";
#[cfg(feature = "wifi")]
const IDENT_VERSION: u8 = 1;
/// Record length — WRITE_SIZE(4)-aligned; the payload is 8 B, the rest reserved (0).
#[cfg(feature = "wifi")]
const IDENT_REC_LEN: usize = 16;

/// Decode a stored identity record. `Some(id)` iff magic + version + both redundancy
/// guards check out. Erased flash (all 0xFF) and any corruption fail every check.
#[cfg(feature = "wifi")]
fn parse_identity(rec: &[u8]) -> Option<u8> {
    if rec.len() < 8 || rec[0..4] != IDENT_MAGIC || rec[4] != IDENT_VERSION {
        return None;
    }
    let id = rec[5];
    if rec[6] != id ^ 0xFF || rec[7] != id.wrapping_add(0x5A) {
        return None; // complement + checksum guards
    }
    Some(id)
}

/// Encode the 16-byte identity record for `id`.
#[cfg(feature = "wifi")]
fn encode_identity(id: u8) -> [u8; IDENT_REC_LEN] {
    let mut r = [0u8; IDENT_REC_LEN];
    r[0..4].copy_from_slice(&IDENT_MAGIC);
    r[4] = IDENT_VERSION;
    r[5] = id;
    r[6] = id ^ 0xFF;
    r[7] = id.wrapping_add(0x5A);
    r
}

/// Read the node id from the `nvs` partition. `None` on any flash/partition error OR
/// when no valid record is present (erased/corrupt) — the caller then falls back to
/// (and seeds from) the baked const.
#[cfg(feature = "wifi")]
fn read_identity_nvs() -> Option<u8> {
    use embedded_storage::nor_flash::ReadNorFlash;
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut buf).ok()?;
    let nvs = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()??;
    let mut region = nvs.as_embedded_storage(&mut flash);
    let mut rec = [0u8; IDENT_REC_LEN];
    region.read(0, &mut rec).ok()?;
    parse_identity(&rec)
}

/// Seed the `nvs` identity record from `id` — ONLY when the first sector is fully
/// erased (0xFF), so we NEVER clobber foreign data. Best-effort: any error is swallowed
/// (identity still works this boot from the baked const; the next boot retries the seed).
#[cfg(feature = "wifi")]
fn seed_identity_nvs(id: u8) {
    use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut buf) else {
        return;
    };
    let Ok(Some(nvs)) = pt.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)) else {
        return;
    };
    let mut region = nvs.as_embedded_storage(&mut flash);
    // Refuse to write unless the WHOLE first sector is erased (all 0xFF) — guards any
    // (unexpected) real NVS content from being erased. An erase-flashed board's nvs is
    // uniformly 0xFF, so a genuinely-fresh board always seeds.
    let sector = <FlashStorage<'_> as NorFlash>::ERASE_SIZE as u32;
    let mut chunk = [0u8; 64];
    let mut off = 0u32;
    while off < sector {
        if region.read(off, &mut chunk).is_err() {
            return;
        }
        if chunk.iter().any(|&b| b != 0xFF) {
            return; // not erased → someone else owns this sector; don't touch it
        }
        off += chunk.len() as u32;
    }
    if region.erase(0, sector).is_err() {
        return;
    }
    let _ = region.write(0, &encode_identity(id));
}

/// The node's RUNTIME identity: the NVS record if valid, else the baked `crate::NODE_ID`
/// (which is ALSO seeded into NVS so it persists across every future OTA). Called once at
/// boot and cached by `crate::node_id()`. BRICK-SAFE — never panics.
#[cfg(feature = "wifi")]
pub fn resolve_node_id() -> u8 {
    if let Some(id) = read_identity_nvs() {
        return id;
    }
    let baked = crate::NODE_ID;
    seed_identity_nvs(baked);
    baked
}

// ---------------------------------------------------------------------------
// otadata operations: inactive-slot lookup, activation, first-boot confirm
// ---------------------------------------------------------------------------

/// Read `otadata` → the INACTIVE slot (the write target). Self-contained flash borrow
/// (its own `FlashStorage`, dropped on return), so it never pins flash for callers.
#[cfg(feature = "espnow")]
fn inactive_slot() -> Option<AppPartitionSubType> {
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut buf).ok()?;
    let od = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Ota))
        .ok()??;
    let region = od.as_embedded_storage(&mut flash);
    let mut ota = Ota::new(region, 2).ok()?;
    // #226/#233 SELF-OVERWRITE FIX: on a BLANK/uninitialised otadata, current_app_partition()
    // returns `Factory` (esp-bootloader 0.5). smol has NO factory partition (partitions-ota.csv),
    // so the ROM boots ota_0 — the running slot is provably ota_0 and the true inactive
    // write-target is ota_1. `other_app` encodes exactly that (Factory|Ota0 => Ota1, Ota1 => Ota0),
    // replacing the old blind `Slot::next()` — now PRIVATE in 0.5 — which folded a blank record to
    // ota_0 (the LIVE image → a self-overwrite/brick class). The 0.5 rename made the footgun
    // structurally unwritable from app code; `other_app` keeps the explicit, audited mapping.
    // Validated against JP's ESP32-C6 watch (same Factory=>Ota1 mapping).
    Some(other_app(ota.current_app_partition().ok()?))
}

/// Point `otadata` at the freshly-written `target` slot + arm the state machine
/// (`New`), then REBOOT into it. Call ONLY after [`ImageWriter::finalize`] returned
/// true. If the otadata write fails, we do NOT reboot — the good slot stays active.
#[cfg(feature = "espnow")]
pub fn activate(target: AppPartitionSubType, new_build: u32, is_leaf_ota: bool) {
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
fn set_slot_new(target: AppPartitionSubType) -> Option<()> {
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut buf).ok()?;
    let od = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Ota))
        .ok()??;
    let region = od.as_embedded_storage(&mut flash);
    let mut ota = Ota::new(region, 2).ok()?;
    ota.set_current_app_partition(target).ok()?;
    ota.set_current_ota_state(OtaImageState::New).ok()?;
    Some(())
}

/// #226 FIRST-BOOT OTADATA INIT: a freshly USB-flashed board has BLANK `otadata` (both
/// select-entries erased → `current_slot()` == `AppPartitionSubType::Factory`). The ESP-IDF ROM then boots
/// `ota_0` (there is no factory partition in `partitions-ota.csv`), but the ota-select record
/// stays absent, so `boot_slot()` reports 255 ("unprovisioned") and luna's rollback automation
/// would see a spurious 255→N jump on the first real OTA. This writes a VALID select record for
/// the running slot exactly once, so otadata is always well-formed.
///
/// Provably correct + safe:
/// - A blank otadata ALWAYS ROM-falls-back to `ota_0` (no factory partition), so the running
///   slot is `Slot0` — we merely formalize a state that is already physically true (no slot
///   change, no reboot).
/// - `Valid` (not `New`) so `otadata_unconfirmed()` stays false — a USB flash is not an OTA under
///   self-test, so it must not arm the #40 K-counter / leaf self-test.
/// - ONE-TIME: only fires while `current_slot()` is blank; the write makes it non-blank, so every
///   later boot is a no-op (no repeated flash wear).
/// - VERIFY-AFTER-WRITE + FAIL-SAFE: any partition/flash error → no-op, and the `inactive_slot()`
///   `AppPartitionSubType::Factory => Slot1` net still targets a subsequent OTA correctly even if this never ran.
///
/// Call ONCE at earliest boot, BEFORE `capture_boot_diag` (so `boot_slot` reads 0, not 255) and
/// BEFORE the #40 unconfirmed-boot block. Boot-critical partition write → HW-canary gated.
#[cfg(feature = "espnow")]
pub fn init_otadata_if_blank() {
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut buf) else {
        return;
    };
    let Ok(Some(od)) = pt.find_partition(PartitionType::Data(DataPartitionSubType::Ota)) else {
        return; // no otadata partition (non-OTA board) → nothing to init
    };
    let region = od.as_embedded_storage(&mut flash);
    let Ok(mut ota) = Ota::new(region, 2) else {
        return;
    };
    // Only touch a genuinely BLANK otadata — never perturb a provisioned record.
    if !matches!(ota.current_app_partition(), Ok(AppPartitionSubType::Factory)) {
        return;
    }
    // Blank ⟹ ROM booted ota_0 ⟹ formalize Slot0 as the valid boot selection.
    if ota.set_current_app_partition(AppPartitionSubType::Ota0).is_err()
        || ota.set_current_ota_state(OtaImageState::Valid).is_err()
    {
        log::error!(
            "smol #226: otadata first-boot init write failed — staying blank (inactive_slot None=>Slot1 net still protects OTA)"
        );
        return;
    }
    // Verify-after-write (boot-critical): confirm the record now resolves to Slot0.
    match ota.current_app_partition() {
        Ok(AppPartitionSubType::Ota0) => log::info!("smol #226: otadata first-boot init — blank → Slot0/Valid"),
        other => log::error!(
            "smol #226: otadata init verify FAILED (got {other:?}) — inactive_slot net still protects OTA"
        ),
    }
}

/// #40 BRICK-SAFETY: does `slot` hold a bootable app image? Reads the slot's first word
/// through a partition-scoped region and checks the ESP-IDF app-image magic byte `0xE9`.
/// An erased/empty slot reads `0xFF` → `false`. Used to REFUSE a rollback that would flip
/// otadata to a slot with no image (e.g. a USB-flashed board whose other slot was never
/// written → both-slots-unbootable BRICK). Conservative: any read/partition error → `false`
/// (don't roll back into the unknown). `wifi`-scoped so `boot_confirm` (also `wifi`) can call
/// it; the types resolve from the `esp-bootloader-esp-idf` dep present in every radio build.
#[cfg(feature = "wifi")]
fn slot_has_valid_image(slot: AppPartitionSubType) -> bool {
    use embedded_storage::nor_flash::ReadNorFlash;
    // #233: brick-safe guard — only ota_0/ota_1 are valid rollback targets on smol's table.
    let sub = match slot {
        AppPartitionSubType::Ota0 | AppPartitionSubType::Ota1 => slot,
        _ => return false, // factory/test/>2-slot — refuse (brick-safe)
    };
    let mut flash = flash();
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
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = match read_partition_table(&mut flash, &mut buf) {
        Ok(pt) => pt,
        Err(_) => return,
    };
    let od = match pt.find_partition(PartitionType::Data(DataPartitionSubType::Ota)) {
        Ok(Some(p)) => p,
        _ => return, // no otadata (non-OTA board) → nothing to confirm
    };
    let region = od.as_embedded_storage(&mut flash);
    let mut ota = match Ota::new(region, 2) {
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
        mark_ota_outcome(false); // #70: DIAG ota=confirmed
        log::info!("smol OTA: self-test PASS — image CONFIRMED (Valid)");
    } else {
        // #40 BRICK-SAFETY (was a hard brick): roll back ONLY if the target slot holds a
        // valid bootable image. Flipping otadata to an EMPTY/invalid slot (a USB-flashed
        // board's never-written other slot) marks BOTH slots unbootable → "No bootable app
        // partitions" crash-loop. If the other slot has no image, DO NOT flip — ACCEPT the
        // current image (mark Valid, keep running). A USB-flashed image is operator-intended
        // and a mesh-OTA image was ed25519+sha verified before activate, so accepting it is
        // safe; bricking is not.
        let target = ota.current_app_partition().ok().map(other_app);
        let can_rollback = target.map(slot_has_valid_image).unwrap_or(false);
        clear_ota_activated(); // fate decided either way
        if can_rollback {
            if let Some(t) = target {
                let _ = ota.set_current_app_partition(t);
            }
            let _ = ota.set_current_ota_state(OtaImageState::Valid);
            mark_ota_outcome(true); // #70: DIAG ota=rolled-back — set BEFORE reset; the good-slot
                                    // boot reports it (drives luna's rollback alert automation).
            log::warn!("smol OTA: self-test FAIL — ROLLING BACK to the previous (valid) slot");
            esp_hal::system::software_reset();
        } else {
            let _ = ota.set_current_ota_state(OtaImageState::Valid);
            // #70: brick-safe accept (no valid rollback target) — the image IS running, so from
            // HA's outcome view it's `confirmed` (no rollback occurred), not a silent failure.
            mark_ota_outcome(false);
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
    target: AppPartitionSubType,
}

#[cfg(feature = "espnow")]
impl LeafImageWriter {
    /// Open a writer for the inactive slot. `None` on any partition/flash error.
    pub fn begin() -> Option<LeafImageWriter> {
        let target = inactive_slot()?;
        // #233: `target` is already the AppPartitionSubType to write (inactive_slot → other_app,
        // always Ota0/Ota1), so the write-target partition IS the target.
        let sub = target;
        let mut flash = flash();
        let mut buf = [0u8; PT_SCRATCH];
        let part_len = {
            let pt = read_partition_table(&mut flash, &mut buf).ok()?;
            let app = pt.find_partition(PartitionType::App(sub)).ok()??;
            app.len()
        };
        Some(LeafImageWriter { sub, part_len, written: 0, erased_upto: 0, target })
    }

    /// The slot this writer targets (passed to [`activate`] after a good finalize).
    pub fn target(&self) -> AppPartitionSubType {
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
        let mut flash = flash();
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

        let word = <FlashStorage<'_> as NorFlash>::WRITE_SIZE as u32; // 4
        let sector = <FlashStorage<'_> as NorFlash>::ERASE_SIZE as u32; // 4096
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
        let mut flash = flash();
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
    pub fn open(slot: AppPartitionSubType) -> Option<SlotReader> {
        // #233: `slot` is already the AppPartitionSubType to read.
        let sub = slot;
        let mut flash = flash();
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
        let mut flash = flash();
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
// today — verified). It is shared by TWO #40 users, SEGREGATED BY SECTOR: the IDENTITY
// record owns sector 0 (offset 0); the freshness-floor cells own sectors 1-2 (offsets
// 4096/8192). This is NOT the ESP-IDF NVS key/value format; a future feature needing real
// NVS must get a different partition or coordinate these offsets. All access is through a
// partition-scoped `FlashRegion` (OOB-safe, like the slot writer), so a bug here can only
// corrupt a floor cell — never the app slots, otadata, OR the identity sector — and never a
// brick (the sig + monotonicity gates still hold; the floor is defense-in-depth).
//
// Layout: two ping-pong cells at nvs relative offsets 4096 and 8192 (sectors 1 and 2),
// each a 12-byte record `[MAGIC(4 LE) | build(4 LE) | crc32(4 LE)]`. Read = max valid
// build across both cells. Write = erase the cell holding the SMALLER/invalid build, write
// the new record there; the other cell always retains ≥ the erased one, so a torn write
// never regresses the floor (power-loss-safe). One erase+write per OTA → negligible wear.
//
// ⚠️ SECTOR SEGREGATION (brick-class, oracle F1): the floor cells MUST NOT share a 4 KB
// sector with the #40 IDENTITY record (nvs offset 0, sector 0). The floor bump ERASES a
// full sector (`erase(target_rel, target_rel + 4096)`); if a cell sat at offset 0 that
// erase would DESTROY the identity record → the leaf falls back to the baked default id →
// with the fleet-shared image, the whole fleet becomes id 7 on the next reboot. So the
// cells live at sectors 1 & 2 (base 4096), leaving sector 0 exclusively for identity.
// ---------------------------------------------------------------------------

#[cfg(feature = "espnow")]
const FLOOR_MAGIC: u32 = 0x736D_6C46; // "smlF"
/// First floor cell's nvs offset — SECTOR 1, NOT sector 0 (sector 0 is the identity record;
/// a floor bump erases its whole target sector and would wipe identity if it sat at 0).
#[cfg(feature = "espnow")]
const FLOOR_CELL_BASE: u32 = 4096;
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
    let mut flash = flash();
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
    let a = floor_cell_read(FLOOR_CELL_BASE).unwrap_or(0);
    let b = floor_cell_read(FLOOR_CELL_BASE + FLOOR_CELL_STRIDE).unwrap_or(0);
    a.max(b)
}

/// Raise the floor to `max(floor, build)`, persisting it. Idempotent (a no-op if the
/// floor already covers `build`). Writes to the cell holding the SMALLER/invalid build
/// so the other cell always retains the prior floor → a torn write cannot regress it.
/// Called once at first-boot-pre-self-test on `otadata == New` (before radio init).
#[cfg(feature = "espnow")]
pub fn fresh_floor_bump(build: u32) {
    use embedded_storage::nor_flash::NorFlash;
    let a = floor_cell_read(FLOOR_CELL_BASE);
    let b = floor_cell_read(FLOOR_CELL_BASE + FLOOR_CELL_STRIDE);
    let cur = a.unwrap_or(0).max(b.unwrap_or(0));
    if build <= cur {
        return; // already covered — no write, no wear
    }
    // Target the cell with the smaller (or invalid) build; the other keeps the prior max.
    // Both offsets are in sectors 1/2 — NEVER sector 0, so this erase can't wipe identity.
    let target_rel = if a.unwrap_or(0) <= b.unwrap_or(0) {
        FLOOR_CELL_BASE
    } else {
        FLOOR_CELL_BASE + FLOOR_CELL_STRIDE
    };
    let mut rec = [0u8; FLOOR_RECORD_LEN];
    rec[0..4].copy_from_slice(&FLOOR_MAGIC.to_le_bytes());
    rec[4..8].copy_from_slice(&build.to_le_bytes());
    let crc = crc32(&rec[0..8]);
    rec[8..12].copy_from_slice(&crc.to_le_bytes());

    let mut flash = flash();
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
#[esp_hal::ram(unstable(rtc_fast, persistent))]
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
#[esp_hal::ram(unstable(rtc_fast, persistent))]
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
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut buf) else {
        return false;
    };
    let Ok(Some(od)) = pt.find_partition(PartitionType::Data(DataPartitionSubType::Ota)) else {
        return false;
    };
    let region = od.as_embedded_storage(&mut flash);
    let Ok(mut ota) = Ota::new(region, 2) else {
        return false;
    };
    matches!(
        ota.current_ota_state(),
        Ok(OtaImageState::New) | Ok(OtaImageState::PendingVerify)
    )
}

// ===========================================================================
// #70 observability — per-boot diagnostics for the retained DIAG record.
//
// Captured ONCE at boot (`capture_boot_diag`) into a `static mut` cache (rv32imc has
// no atomics; single boot-path caller, no ISR — the same discipline as `NODE_ID_CACHE`
// / the OTA scratch statics), then read every diag cadence by `boot_diag()`:
//   • boot_count  — NVS-persisted, bumped once per boot (survives power-loss; the OTA
//                   image write never touches nvs, so it survives every image too).
//   • reset_reason— por / sw / panic / bo / wdt / usb / dslp / other / unk.
//   • boot_slot   — the running app slot (0=ota_0 / 1=ota_1). A silent rollback FLIPS it.
//   • ota_state   — otadata image state token (valid/new/pend/…).
// All `espnow`-scoped: only the mesh DIAG record consumes them (a `wifi`-only build has
// no `RadioManager` to build the frame). BRICK-SAFE: every read fails to a benign default
// and never panics; the boot-count store is SECTOR-SEGREGATED from identity + the floor.
// ---------------------------------------------------------------------------

// #70 boot-count store: `[MAGIC(4 LE) | count(4 LE) | crc32(4 LE)]`, two ping-pong cells in
// nvs SECTORS 3 & 4 (offsets 12288/16384). nvs is 0x6000 (6 sectors): sector 0 = identity,
// sectors 1-2 = freshness floor, so 3/4 are free (5 left spare). Ping-pong = torn-write-safe
// (a power-loss mid-bump keeps the other cell's prior count) AND halves per-sector flash wear
// (each cell is erased only every OTHER boot). Same sector-segregation invariant as the floor:
// a bump erases a whole 4 KB sector, so these MUST NOT share sector 0 (identity) or 1-2 (floor).
#[cfg(feature = "espnow")]
const BOOTCOUNT_MAGIC: u32 = 0x736D_6C42; // "smlB"
#[cfg(feature = "espnow")]
const BOOTCOUNT_CELL_BASE: u32 = 12288; // nvs sector 3 — NOT 0/1/2 (identity + floor)
#[cfg(feature = "espnow")]
const BOOTCOUNT_CELL_STRIDE: u32 = 4096; // one sector apart (erase granularity)
#[cfg(feature = "espnow")]
const BOOTCOUNT_RECORD_LEN: usize = 12;

/// Read one boot-count cell at `rel` (nvs-relative). `None` on flash error / bad magic / crc.
#[cfg(feature = "espnow")]
fn bootcount_cell_read(rel: u32) -> Option<u32> {
    use embedded_storage::nor_flash::ReadNorFlash;
    let mut flash = flash();
    let mut ptbuf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut ptbuf).ok()?;
    let nvs = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()??;
    let mut region = nvs.as_embedded_storage(&mut flash);
    let mut rec = [0u8; BOOTCOUNT_RECORD_LEN];
    region.read(rel, &mut rec).ok()?;
    let magic = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
    if magic != BOOTCOUNT_MAGIC {
        return None; // erased (0xFFFFFFFF) or never written
    }
    let count = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
    let crc = u32::from_le_bytes([rec[8], rec[9], rec[10], rec[11]]);
    (crc == crc32(&rec[0..8])).then_some(count)
}

/// Increment the NVS-persisted boot count and return the NEW value. Call ONCE per boot.
/// Writes the incremented count to the cell holding the SMALLER/invalid count, so the other
/// cell always retains the prior count → a torn write cannot lose it (power-loss safe).
/// Best-effort: any flash error still returns the in-memory increment (the diag just isn't
/// persisted this boot; the next boot re-reads the last good cell). Never panics, never
/// touches identity (sector 0) or the floor (sectors 1-2).
#[cfg(feature = "espnow")]
pub fn boot_count_bump() -> u32 {
    use embedded_storage::nor_flash::NorFlash;
    let a = bootcount_cell_read(BOOTCOUNT_CELL_BASE);
    let b = bootcount_cell_read(BOOTCOUNT_CELL_BASE + BOOTCOUNT_CELL_STRIDE);
    let cur = a.unwrap_or(0).max(b.unwrap_or(0));
    let next = cur.saturating_add(1);
    // Target the cell with the smaller (or invalid) count; the other keeps the prior max.
    // Both offsets are in sectors 3/4 — never sector 0/1/2, so this erase can't wipe identity
    // or the freshness floor.
    let target_rel = if a.unwrap_or(0) <= b.unwrap_or(0) {
        BOOTCOUNT_CELL_BASE
    } else {
        BOOTCOUNT_CELL_BASE + BOOTCOUNT_CELL_STRIDE
    };
    let mut rec = [0u8; BOOTCOUNT_RECORD_LEN];
    rec[0..4].copy_from_slice(&BOOTCOUNT_MAGIC.to_le_bytes());
    rec[4..8].copy_from_slice(&next.to_le_bytes());
    let crc = crc32(&rec[0..8]);
    rec[8..12].copy_from_slice(&crc.to_le_bytes());
    let mut flash = flash();
    let mut ptbuf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut ptbuf) else {
        return next;
    };
    let Ok(Some(nvs)) = pt.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)) else {
        return next;
    };
    let mut region = nvs.as_embedded_storage(&mut flash);
    if region.erase(target_rel, target_rel + BOOTCOUNT_CELL_STRIDE).is_err() {
        return next;
    }
    let _ = region.write(target_rel, &rec);
    next
}

/// #70 panic marker: a panic on this fw halts via `custom_halt` → `software_reset`, which the
/// SoC records as a plain software reset (`CoreSw`) — indistinguishable from an intentional
/// reboot / OTA activate. `custom_halt` sets this RTC-fast persistent marker FIRST, so the
/// next boot can tell a panic-reset (`rr=panic`) from a clean `rr=sw`. Survives the reset AND
/// a USB reflash; a true power cycle clears it (a power-cycled board reports `por`, correct).
#[cfg(feature = "wifi")]
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut PANIC_MARK: [u32; 1] = [0u32; 1];

#[cfg(feature = "wifi")]
const PANIC_MARK_MAGIC: u32 = 0x736D_6C50; // "smlP"

/// Set the panic marker (call from `custom_halt`, BEFORE `software_reset`).
#[cfg(feature = "wifi")]
pub fn mark_panic() {
    unsafe {
        let m = &mut *core::ptr::addr_of_mut!(PANIC_MARK);
        m[0] = PANIC_MARK_MAGIC;
    }
}

/// Read-and-clear the panic marker. `true` iff this boot chain began with a panic. Cleared on
/// read so only the FIRST boot after the panic reports it. `espnow`-scoped: only the mesh diag
/// path (`reset_reason_token`) reads it; a wifi-only build sets the marker (via `mark_panic`) but
/// has no DIAG consumer, so it never takes it.
#[cfg(feature = "espnow")]
fn take_panic_mark() -> bool {
    unsafe {
        let m = &mut *core::ptr::addr_of_mut!(PANIC_MARK);
        let hit = m[0] == PANIC_MARK_MAGIC;
        m[0] = 0;
        hit
    }
}

/// The last reset reason as a short, stable token for the DIAG record. Reads (and clears) the
/// panic marker first — a panic shows as `CoreSw` at the SoC level, so the marker recovers the
/// `panic` distinction. Everything else maps the C3 `SocResetReason` set to a compact token.
#[cfg(feature = "espnow")]
pub fn reset_reason_token() -> &'static str {
    if take_panic_mark() {
        return "panic";
    }
    // Value strings match luna's DEPLOYED #70 HA parser (PR #81) exactly — the parser passes
    // rst= through for display, so an out-of-enum value like "panic" still shows correctly.
    use esp_hal::rtc_cntl::SocResetReason as R;
    match esp_hal::system::reset_reason() {
        Some(R::ChipPowerOn) => "power-on",
        Some(R::CoreSw) | Some(R::Cpu0Sw) => "sw",
        Some(R::CoreDeepSleep) => "deep-sleep",
        Some(R::SysBrownOut) => "brownout",
        Some(
            R::CoreMwdt0
            | R::CoreMwdt1
            | R::Cpu0Mwdt0
            | R::Cpu0Mwdt1
            | R::CoreRtcWdt
            | R::Cpu0RtcWdt
            | R::SysRtcWdt
            | R::SysSuperWdt,
        ) => "wdt",
        Some(R::CoreUsbUart | R::CoreUsbJtag) => "usb-jtag",
        Some(R::SysClkGlitch | R::CorePwrGlitch) => "glitch",
        Some(_) => "other",
        None => "unk",
    }
}

// =========================================================================
// #100 network-switcher: persisted network selection (nvs SECTOR 5, offset 0x5000).
// =========================================================================
//
// The runtime WiFi-slot selection lives in its OWN nvs sector, SEGREGATED from identity
// (sector 0), the freshness-floor (sectors 1-2 @ 4096/8192) and the boot-count (sectors 3-4 @
// 12288/16384) — verified the ONLY free sector in the 0x6000 (6-sector) nvs partition. A write
// erases the whole sector, so own-sector segregation is the #40/#70 brick-class discipline: this
// record can NEVER wipe identity/floor/boot-count, and they can never wipe it. Corrupt / erased /
// out-of-range → `None` → the caller defaults to slot 0 (the boot-default network — SAFE: a
// garbage record never strands the node on a phantom slot). Brick-safe: never panics, best-effort.
#[cfg(feature = "wifi")]
const NET_MAGIC: [u8; 4] = *b"SMn1";
/// Record version WRITTEN by this firmware. v1 (Stage 1) = the 10-byte core (slot selection only);
/// v2 (#100 Stage 2/3) added the broker + OTA-host overrides alongside the slot bytes; v3 (#142)
/// RETIRES the slot machinery — same on-flash layout, but the former slot bytes [5..8] are
/// reserved-zero and carry no meaning. `parse_net_cfg` MIGRATES on read: v2 → keep the override
/// fields, discard the slot bytes; v1 → no overrides; v3 → native. An unknown version / bad checksum
/// / erased flash → `None` → safe defaults (single baked network, no overrides). Deployed boards that
/// were never flash-erased carry a v2 record, so this migration is REQUIRED — it preserves their
/// broker/OTA-host overrides across the #142 OTA. No forced boot-time rewrite (flash wear): the first
/// genuine CFG-`B`/`O` change upgrades the record to v3 in place, verify-after-write gated. A pre-#142
/// (v2-reader) fw reading a v3 record rejects it → slot 0 (a SAFE rollback default), same as v1↔v2.
// #142: written only by the CFG-`B`/`O` apply (espnow tier) — the wifi-tier writer (the retired
// slot-fallback revert) is gone, so gate to `espnow` to stay dead-code-clean in a wifi-only build.
#[cfg(feature = "espnow")]
const NET_VERSION: u8 = 3;
/// nvs sector 5 = 0x5000 = 20480 (the one free sector; see the segregation note above).
#[cfg(feature = "wifi")]
const NET_REC_OFF: u32 = 5 * 4096;
/// v2 record length. The v1 core is 10 B (magic..guard2); the v2 ext ([10..25]) adds the broker
/// override (present + fallback flags, 4-B IP, 2-B port), the OTA-host override (present + 4-B IP),
/// and a sum-checksum + complement guard at [23]/[24]. Read fixed-width: a stored v1 record leaves
/// [16..] erased (0xFF), which the v1 parse path never inspects.
///
/// 28, NOT 25: esp-storage's `NorFlash::WRITE_SIZE == WORD_SIZE == 4` — a write whose length isn't
/// word-aligned returns `NotAligned`, which `write_net_cfg`'s swallowed error turned into "record
/// never persists" (HW-canary find, 2026-07-12: capture + edge-trigger fired, then verify-after-write
/// aborted EVERY apply). [25..28] is zero pad outside the checksum; the guards stay at [23]/[24].
#[cfg(feature = "wifi")]
const NET_REC_LEN: usize = 28;

/// The persisted network overrides. #142: the #100 dual-slot fields (`active`/`commanded`/`fallback`)
/// are RETIRED — boards run the single baked network (`crate::secrets::WIFI_NETWORK`), so there is no
/// slot to select or revert. The on-flash record FORMAT is unchanged (see `encode_net_cfg`): the
/// former slot bytes `[5..10]` are reserved-zero, so a pre-#142 record still parses and its overrides
/// (below) SURVIVE the OTA — the migration is brick-safe with no version bump. #100 Stage 2/3 overrides
/// (all runtime, NVS-persisted, RFC1918-gated at the CFG apply; independent of the slot machinery and
/// load-bearing per #116): `broker` = the COMMANDED broker leg override (the baked broker is used when
/// `None`); `broker_fallback` = the override was auto-disabled after repeated CONNACK failures —
/// `broker` is KEPT (for DIAG + the edge-trigger) but ignored at runtime; `ota_host` = one extra
/// RFC1918 host appended to the OTA allowlist.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct NetCfg {
    /// Stage 2: broker leg override (IPv4 octets + port). `None` = use the baked network's broker.
    pub broker: Option<([u8; 4], u16)>,
    /// Stage 2: the `broker` override was auto-disabled (N failed CONNACKs). Runtime uses the baked
    /// broker; `broker` is retained so DIAG shows `brk=fb` and CFG-`B` edge-triggers correctly.
    pub broker_fallback: bool,
    /// Stage 3: one extra OTA image-host (RFC1918) appended to the fetch allowlist. `None` = none.
    pub ota_host: Option<[u8; 4]>,
}

/// The v2 extension checksum over `rec[10..23]` (the override bytes). A wrapping sum + `0x5A` bias,
/// stored at `[23]` with its complement at `[24]` — the same two-guard scheme as the v1 core.
#[cfg(feature = "wifi")]
fn net_ext_checksum(rec: &[u8]) -> u8 {
    rec[10..23]
        .iter()
        .fold(0u8, |a, &b| a.wrapping_add(b))
        .wrapping_add(0x5A)
}

/// Decode a stored net record. `Some` iff magic + a known version + the core redundancy guards pass
/// AND the slot indices are in range (< 2). A v1 record yields no overrides; a v2 record must ALSO
/// pass the ext checksum + complement (else the overrides are corrupt → the whole record is rejected
/// → caller uses slot 0, no override). Erased flash (0xFF) / corruption all fail → `None`.
#[cfg(feature = "wifi")]
fn parse_net_cfg(rec: &[u8]) -> Option<NetCfg> {
    if rec.len() < 10 || rec[0..4] != NET_MAGIC {
        return None;
    }
    let ver = rec[4];
    // #142: accept v1 (Stage-1 core), v2 (#100 slot+overrides — MIGRATED), and v3 (#142 native).
    // Anything else (or a rollback-written future version) → None → safe defaults.
    if ver != 1 && ver != 2 && ver != 3 {
        return None;
    }
    // rec[5..8]: the slot bytes (active/commanded/fallback) in v1/v2, reserved-zero in v3. Read only
    // to validate the core redundancy guards — this is what lets a PRE-#142 v2 record (nonzero slot
    // bytes, valid checksum) still parse so its overrides survive the OTA. The values are DISCARDED
    // (there is no slot concept any more): v2 is thereby migrated to the v3 in-RAM shape on read.
    let (b5, b6, b7) = (rec[5], rec[6], rec[7]);
    // Core complement + checksum guards (mirror the identity record) — present in v1, v2 AND v3.
    if rec[8] != b5 ^ 0xFF || rec[9] != (b5 ^ b6 ^ b7).wrapping_add(0x5A) {
        return None;
    }
    if ver == 1 {
        return Some(NetCfg::default()); // Stage-1 record: no overrides.
    }
    // v2/v3 ext: require the full record + a matching checksum/complement before trusting the
    // overrides. (Identical ext layout in v2 and v3 — only the version byte + zeroed slots differ,
    // so the same extraction migrates a v2 record and reads a v3 one.)
    if rec.len() < NET_REC_LEN || rec[23] != net_ext_checksum(rec) || rec[24] != rec[23] ^ 0xFF {
        return None;
    }
    let broker = (rec[10] == 1).then(|| {
        (
            [rec[12], rec[13], rec[14], rec[15]],
            u16::from_le_bytes([rec[16], rec[17]]),
        )
    });
    let ota_host = (rec[18] == 1).then_some([rec[19], rec[20], rec[21], rec[22]]);
    Some(NetCfg {
        broker,
        broker_fallback: rec[11] == 1,
        ota_host,
    })
}

/// Encode the v3 net record (#142 — always written at [`NET_VERSION`] = 3; slot bytes reserved-zero).
/// Absent overrides zero their present-flag + bytes so the record is deterministic; the ext checksum
/// + complement close it. Length stays 28 B (word-aligned; [25..28] pad outside the checksum).
///
/// #142: espnow-gated — only the CFG-`B`/`O` apply (mesh tier) writes the record now.
#[cfg(feature = "espnow")]
fn encode_net_cfg(c: NetCfg) -> [u8; NET_REC_LEN] {
    let mut r = [0u8; NET_REC_LEN];
    r[0..4].copy_from_slice(&NET_MAGIC);
    r[4] = NET_VERSION;
    // #142: the retired slot bytes [5..8] are reserved-zero; the core guards still checksum over
    // them (0^0xFF at [8], (0^0^0)+0x5A at [9]) so the on-flash format is byte-identical to a
    // pre-#142 record and `parse_net_cfg` accepts both without a version bump.
    r[5] = 0;
    r[6] = 0;
    r[7] = 0;
    r[8] = 0xFF;
    r[9] = 0x5A;
    if let Some((ip, port)) = c.broker {
        r[10] = 1;
        r[12..16].copy_from_slice(&ip);
        r[16..18].copy_from_slice(&port.to_le_bytes());
    }
    r[11] = c.broker_fallback as u8;
    if let Some(ip) = c.ota_host {
        r[18] = 1;
        r[19..23].copy_from_slice(&ip);
    }
    r[23] = net_ext_checksum(&r);
    r[24] = r[23] ^ 0xFF;
    r
}

/// Read the persisted net selection. `None` on any flash/partition error, erased, or corrupt →
/// the caller defaults to slot 0 (boot-default network — SAFE). Brick-safe (never panics).
#[cfg(feature = "wifi")]
pub fn read_net_cfg() -> Option<NetCfg> {
    use embedded_storage::nor_flash::ReadNorFlash;
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let pt = read_partition_table(&mut flash, &mut buf).ok()?;
    let nvs = pt
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()??;
    let mut region = nvs.as_embedded_storage(&mut flash);
    let mut rec = [0u8; NET_REC_LEN];
    region.read(NET_REC_OFF, &mut rec).ok()?;
    parse_net_cfg(&rec)
}

/// Persist the net overrides (erase + write nvs sector 5). Best-effort — any error is swallowed
/// (the in-RAM value still governs THIS boot; the next boot retries). Sector 5 is the net record's
/// OWN sector (segregated), so the erase+write can never touch identity/floor/boot-count. #142:
/// called ONLY on a genuine CFG-`B`/`O` override change (the slot-switch + un-brick-fallback callers
/// are retired) — never in a retry loop, so no flash wear / NVS ping-pong. This is also what upgrades
/// a legacy v2 record to v3 in place, on the first real override change.
/// #142: espnow-gated — only the CFG-`B`/`O` apply (mesh tier) writes; the wifi-tier writer (the
/// retired #100 slot-fallback revert) is gone, so a wifi-only build carries no writer (dead-code-clean).
#[cfg(feature = "espnow")]
pub fn write_net_cfg(c: NetCfg) {
    use embedded_storage::nor_flash::NorFlash;
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut buf) else {
        return;
    };
    let Ok(Some(nvs)) = pt.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)) else {
        return;
    };
    // Sector 5 is the LAST nvs sector: the FlashRegion erase API bounds-checks its EXCLUSIVE
    // end with `contains(addr_to)` (strict `<`), so `erase(0x5000, 0x6000)` — ending exactly at
    // the partition boundary — is structurally impossible through the region (OutOfBounds).
    // Found on hardware (#100 canary reset-loop): the swallowed erase error meant the record
    // never persisted. Erase+write RAW at absolute addresses instead — same 4 KB, same
    // segregation (identity/floor/boot-count live in sectors 0-4, untouched).
    let base = nvs.offset(); // absolute flash address of the nvs partition
    let sector = <FlashStorage<'_> as NorFlash>::ERASE_SIZE as u32; // 4096
    if flash.erase(base + NET_REC_OFF, base + NET_REC_OFF + sector).is_err() {
        return;
    }
    let _ = flash.write(base + NET_REC_OFF, &encode_net_cfg(c));
}

/// Parse a dotted-quad IPv4 (`"10.0.8.111"`) into four octets. Panic-free, `no_std`: rejects any
/// part that isn't a 0..=255 decimal and anything other than exactly four parts. `None` on garbage.
#[cfg(feature = "wifi")]
pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut it = s.split('.');
    for o in octets.iter_mut() {
        *o = it.next()?.parse::<u8>().ok()?;
    }
    if it.next().is_some() {
        return None; // more than four parts
    }
    Some(octets)
}

/// RFC1918 private-range test (`10/8`, `172.16/12`, `192.168/16`). CFG-`B`/`O` overrides MUST pass
/// this so a dashboard typo can never point a board off-LAN (the on-LAN guard JP named in #100).
/// `espnow`-gated: reached only through the two override parsers below, which the espnow-only CFG
/// apply path calls (a wifi-only build has no `RadioManager`/apply, so it would be dead code there).
#[cfg(feature = "espnow")]
pub fn is_rfc1918(ip: [u8; 4]) -> bool {
    matches!(ip,
        [10, ..]
        | [172, 16..=31, ..]
        | [192, 168, ..])
}

/// Parse a CFG-`B` broker override value `"a.b.c.d"` or `"a.b.c.d:port"` (port defaults to 1883,
/// the plain-Mosquitto port). `None` unless the IP is RFC1918 and the port is non-zero — the apply
/// path treats `None` as "invalid, keep current" and an empty string as an explicit CLEAR.
/// `espnow`-gated: called only from the espnow-only CFG-`B` apply path (see `is_rfc1918`).
#[cfg(feature = "espnow")]
pub fn parse_broker_override(s: &str) -> Option<([u8; 4], u16)> {
    let s = s.trim();
    let (ip_str, port) = match s.rsplit_once(':') {
        Some((ip, p)) => (ip, p.parse::<u16>().ok()?),
        None => (s, 1883u16),
    };
    let ip = parse_ipv4(ip_str)?;
    if !is_rfc1918(ip) || port == 0 {
        return None;
    }
    Some((ip, port))
}

/// Parse a CFG-`O` OTA-host override value `"a.b.c.d"`. `None` unless the IP is RFC1918 (keeps OTA
/// fetches on-LAN; the image is still ed25519/monotonicity-gated regardless — belt-and-suspenders).
/// `espnow`-gated: called only from the espnow-only CFG-`O` apply path (see `is_rfc1918`).
#[cfg(feature = "espnow")]
pub fn parse_ota_host_override(s: &str) -> Option<[u8; 4]> {
    let ip = parse_ipv4(s.trim())?;
    if !is_rfc1918(ip) {
        return None;
    }
    Some(ip)
}

/// The running app slot as 0 (`ota_0`) / 1 (`ota_1`); 255 on a non-OTA board or any read
/// error. A silent rollback flips this vs the pushed slot — the headline #70 signal.
#[cfg(feature = "espnow")]
pub fn boot_slot() -> u8 {
    let mut flash = flash();
    let mut buf = [0u8; PT_SCRATCH];
    let Ok(pt) = read_partition_table(&mut flash, &mut buf) else {
        return 255;
    };
    let Ok(Some(od)) = pt.find_partition(PartitionType::Data(DataPartitionSubType::Ota)) else {
        return 255;
    };
    let region = od.as_embedded_storage(&mut flash);
    let Ok(mut ota) = Ota::new(region, 2) else {
        return 255;
    };
    match ota.current_app_partition() {
        Ok(AppPartitionSubType::Ota0) => 0,
        Ok(AppPartitionSubType::Ota1) => 1,
        _ => 255,
    }
}

/// #70 LAST OTA OUTCOME — the DIAG record's `ota=` field, matching luna's deployed parser
/// (`confirmed` | `rolled-back` | `none`), which drives her rollback ALERT automation. This is
/// an OUTCOME, not the raw otadata state: `boot_confirm` decides it (PASS → confirmed, self-test
/// FAIL → rolled-back) AFTER `capture_boot_diag` runs, so it is read LIVE each diag cadence (not
/// cached in `DiagBoot`). RTC-fast persistent (survives the rollback's `software_reset` so the
/// good-slot boot reports `rolled-back`; a power cycle clears it → `none`, correct). Magic-gated
/// so uninitialised RTC RAM reads as `none`, never a false outcome.
#[cfg(feature = "wifi")]
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut OTA_OUTCOME: [u32; 2] = [0u32; 2]; // [magic, 1=confirmed / 2=rolled-back]

#[cfg(feature = "wifi")]
const OTA_OUTCOME_MAGIC: u32 = 0x736D_6C4F; // "smlO"

/// Record the last OTA outcome (call from `boot_confirm`): `rolled_back` true ⇒ `rolled-back`,
/// else `confirmed`. Set before the rollback's `software_reset` so the next boot reports it.
#[cfg(feature = "wifi")]
pub fn mark_ota_outcome(rolled_back: bool) {
    unsafe {
        let m = &mut *core::ptr::addr_of_mut!(OTA_OUTCOME);
        m[0] = OTA_OUTCOME_MAGIC;
        m[1] = if rolled_back { 2 } else { 1 };
    }
}

/// The last OTA outcome token for the DIAG `ota=` field. `none` until an OTA confirm/rollback
/// happened this power-cycle (magic-gated: uninitialised RTC RAM ⇒ `none`).
#[cfg(feature = "espnow")]
pub fn ota_outcome_token() -> &'static str {
    unsafe {
        let m = core::ptr::addr_of!(OTA_OUTCOME).read();
        if m[0] != OTA_OUTCOME_MAGIC {
            return "none";
        }
        match m[1] {
            1 => "confirmed",
            2 => "rolled-back",
            _ => "none",
        }
    }
}

/// The once-per-boot diagnostics, captured into a cache by `capture_boot_diag` and read every
/// diag cadence by `boot_diag()`. `Copy` so it lives inline in the cache. (The OTA outcome is
/// NOT here — it's decided by `boot_confirm` after capture, so it's read live via
/// `ota_outcome_token`.)
#[cfg(feature = "espnow")]
#[derive(Clone, Copy)]
pub struct DiagBoot {
    pub boot_count: u32,
    pub reset_reason: &'static str,
    pub boot_slot: u8,
}

#[cfg(feature = "espnow")]
static mut BOOT_DIAG: Option<DiagBoot> = None;

/// Capture the per-boot diagnostics ONCE, very early in `main` (after the OTA bookkeeping, so a
/// forced rollback reboots BEFORE the count bumps — the rolled-back boot counts, the aborted one
/// does not). Bumps the persisted boot count, latches the reset reason (incl. the panic marker
/// take), and reads the boot slot + otadata state. Single-threaded boot-path caller (no ISR).
/// Call exactly once — a second call would double-bump the boot count.
#[cfg(feature = "espnow")]
pub fn capture_boot_diag() {
    let d = DiagBoot {
        boot_count: boot_count_bump(),
        reset_reason: reset_reason_token(),
        boot_slot: boot_slot(),
    };
    unsafe {
        *core::ptr::addr_of_mut!(BOOT_DIAG) = Some(d);
    }
}

/// The cached per-boot diagnostics (a benign all-unknown default if `capture_boot_diag` never
/// ran). Read by the DIAG frame builder each cadence.
#[cfg(feature = "espnow")]
pub fn boot_diag() -> DiagBoot {
    unsafe {
        core::ptr::addr_of!(BOOT_DIAG).read().unwrap_or(DiagBoot {
            boot_count: 0,
            reset_reason: "unk",
            boot_slot: 255,
        })
    }
}
