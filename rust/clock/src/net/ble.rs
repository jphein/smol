//! #22 passive BLE observer — the PURE half: HCI command codec, LE Advertising
//! Report parser, and the heapless sighting table.
//!
//! ## What this is
//! A gateway/leaf running the `ble` feature drives the ESP32-C3's Bluetooth
//! controller as a **passive scanner only** — never connecting, never actively
//! probing. It listens for BLE advertisements sharing the single 2.4 GHz radio
//! with WiFi + ESP-NOW under Espressif's software coexistence arbiter, collects
//! room-presence sightings (MAC + RSSI + a compact ad-type summary), and relays
//! a summary gateway-ward for HA. `feature = "ble"` (⊃ `espnow`); the default /
//! wifi / espnow / wled / cast builds are byte-free of it.
//!
//! ## This file is PURE (no esp-hal / esp-wifi deps)
//! Everything here is plain byte/integer logic so it is host-unit-testable off
//! the target (see `scratch/22-ble/ble_verify`). The part that owns the esp-wifi
//! `BleConnector` and drives the blocking HCI transport lives in
//! [`crate::net::ble_scan`] (it needs esp-wifi) — mirroring the `cast` (pure) /
//! `cast_oled` (HAL) split.
//!
//! ## Why raw HCI, no host stack
//! A passive scan is two HCI commands (`LE Set Scan Parameters` +
//! `LE Set Scan Enable`) and a parse of the `LE Advertising Report` meta-event.
//! esp-wifi 0.15 exposes a *blocking* `BleConnector` (`embedded_io::Write` for
//! commands, a non-blocking `next()` drain for events) — so we drive raw HCI
//! bytes directly and need NO `trouble-host` / `bleps` host stack. That keeps
//! zero Cargo-pin risk against smol's deliberately-frozen esp-hal/esp-wifi
//! quartet (the whole point of the #22 de-risking note).
//!
//! ## Airtime safety (the coexist gate — issue #22/#23)
//! The controller's own scan interval/window ([`SCAN_INTERVAL_UNITS`] /
//! [`SCAN_WINDOW_UNITS`]) bound the *average* BLE airtime request to ~4 %; the
//! coex arbiter interleaves WiFi/ESP-NOW at millisecond scale within each scan
//! window, so the mesh never goes deaf for the multi-second stretch a WiFi flush
//! costs. Both are `pub const` so the soak's fallback ladder (shrink the duty)
//! is a recompile, not a rewrite.

extern crate alloc;

use core::fmt::Write as _;

// =========================================================================
// HCI transport constants (H4 UART framing over esp-wifi's VHCI).
// =========================================================================

/// H4 packet-type indicator for an HCI **command** (host → controller). The
/// esp-wifi blocking `Write` path forwards bytes verbatim, so we prefix it.
pub const HCI_CMD: u8 = 0x01;
/// H4 packet-type indicator for an HCI **event** (controller → host). esp-wifi's
/// receive queue delivers packets WITH this leading byte (`data[0]`).
pub const HCI_EVT: u8 = 0x04;

/// `LE Set Scan Parameters` opcode 0x200B (OGF=0x08 LE, OCF=0x000B), little-endian.
const OP_SET_SCAN_PARAMS: [u8; 2] = [0x0B, 0x20];
/// `LE Set Scan Enable` opcode 0x200C.
const OP_SET_SCAN_ENABLE: [u8; 2] = [0x0C, 0x20];

/// HCI event code for an LE Meta event, and its LE-Advertising-Report subevent.
const EVT_LE_META: u8 = 0x3E;
const SUBEVT_ADV_REPORT: u8 = 0x02;

/// Encoded length of the [`scan_params_cmd`] packet (H4 byte + 2 opcode + len + 7 params).
pub const SCAN_PARAMS_CMD_LEN: usize = 11;
/// Encoded length of the [`scan_enable_cmd`] packet (H4 byte + 2 opcode + len + 2 params).
pub const SCAN_ENABLE_CMD_LEN: usize = 6;

// =========================================================================
// Scan duty (0.625 ms units) — the coexist airtime lever (#22/#23).
// =========================================================================

/// LE scan interval, in 0.625 ms units. 8000 = 5.00 s. The controller opens ONE
/// scan window per interval; a longer interval = a smaller average BLE airtime slice.
pub const SCAN_INTERVAL_UNITS: u16 = 8000;
/// LE scan window, in 0.625 ms units. 320 = 200 ms. window / interval ≈ 4 % duty —
/// far below the multi-second WiFi-flush deaf window the mesh already tolerates.
pub const SCAN_WINDOW_UNITS: u16 = 320;

/// Build the `LE Set Scan Parameters` command. `passive` selects a listen-only scan
/// (no SCAN_REQ probes → no airtime spent talking). `own_address_type` = public,
/// `scanning_filter_policy` = accept-all (we filter/dedup host-side).
pub fn scan_params_cmd(passive: bool, interval: u16, window: u16) -> [u8; SCAN_PARAMS_CMD_LEN] {
    let scan_type = if passive { 0x00 } else { 0x01 };
    [
        HCI_CMD,
        OP_SET_SCAN_PARAMS[0],
        OP_SET_SCAN_PARAMS[1],
        0x07, // parameter length
        scan_type,
        (interval & 0xFF) as u8,
        (interval >> 8) as u8,
        (window & 0xFF) as u8,
        (window >> 8) as u8,
        0x00, // own_address_type = public
        0x00, // scanning_filter_policy = accept all
    ]
}

/// Build the `LE Set Scan Enable` command. `filter_duplicates` is left OFF by the
/// caller: the controller's dup-filter would suppress the repeat adverts we want
/// for fresh RSSI, and we dedup + age host-side in [`SightingTable`].
pub fn scan_enable_cmd(enable: bool, filter_duplicates: bool) -> [u8; SCAN_ENABLE_CMD_LEN] {
    [
        HCI_CMD,
        OP_SET_SCAN_ENABLE[0],
        OP_SET_SCAN_ENABLE[1],
        0x02, // parameter length
        enable as u8,
        filter_duplicates as u8,
    ]
}

// =========================================================================
// LE Advertising Report parsing.
// =========================================================================

/// `ad_summary` bit flags — a compact, privacy-cheap fingerprint of what kinds of
/// AD structures the advert carried, without storing the raw payload.
pub mod ad {
    /// The advert is connectable (event_type ADV_IND / ADV_DIRECT_IND).
    pub const CONNECTABLE: u8 = 0x01;
    /// Carries a device name (AD type 0x08 shortened / 0x09 complete).
    pub const NAME: u8 = 0x02;
    /// Carries Manufacturer Specific Data (AD type 0xFF) — iBeacon/tags live here.
    pub const MFG: u8 = 0x04;
    /// Carries Service Data (AD type 0x16 / 0x20 / 0x21) — Eddystone/tags live here.
    pub const SVC_DATA: u8 = 0x08;
    /// Carries a Flags field (AD type 0x01).
    pub const FLAGS: u8 = 0x10;
    /// Carries a Service UUID list (AD type 0x02/0x03/0x06/0x07).
    pub const SVC_UUID: u8 = 0x20;
}

/// One decoded advertisement observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sighting {
    /// Advertiser address, big-endian (display order: `addr[0]` is the high octet).
    /// HCI delivers it little-endian on the wire; [`parse_adv_reports`] reverses it.
    pub addr: [u8; 6],
    /// 0 = public, 1 = random (incl. resolvable/non-resolvable private).
    pub addr_type: u8,
    /// Received signal strength, dBm (signed).
    pub rssi: i8,
    /// [`ad`] bit-flag summary of the advert's AD structures.
    pub ad_summary: u8,
}

/// Walk an AD-structure blob (`[len][type][len-1 payload]…`) into an [`ad`] bitmask.
/// Total-safe: a malformed length just ends the walk (never panics / over-reads).
fn summarize_ad(data: &[u8]) -> u8 {
    let mut flags = 0u8;
    let mut i = 0usize;
    while i < data.len() {
        let len = data[i] as usize;
        // A structure spans data[i ..= i+len] (the len byte covers type + payload).
        // A zero length or one that would over-read ends the walk.
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        let ad_type = data[i + 1];
        flags |= match ad_type {
            0x01 => ad::FLAGS,
            0x02 | 0x03 | 0x06 | 0x07 => ad::SVC_UUID,
            0x08 | 0x09 => ad::NAME,
            0x16 | 0x20 | 0x21 => ad::SVC_DATA,
            0xFF => ad::MFG,
            _ => 0,
        };
        i += len + 1; // advance past this structure (len byte covers type + payload)
    }
    flags
}

/// Parse an HCI event packet (with its leading H4 [`HCI_EVT`] byte) and invoke `f`
/// once per advertising report it carries. Non-advertising events are ignored.
///
/// Every field access is bounds-checked; a truncated / malformed packet simply
/// yields fewer (or no) callbacks — a stray on-air frame can never panic the node.
/// We parse reports in the controller's interleaved per-report layout (the layout
/// the esp32c3 btdm controller emits, always with `num_reports == 1` in practice).
pub fn parse_adv_reports<F: FnMut(Sighting)>(pkt: &[u8], mut f: F) {
    // [0]=0x04 evt · [1]=0x3E LE-meta · [2]=param_len · [3]=0x02 adv-report subevt · [4]=num
    if pkt.len() < 5 || pkt[0] != HCI_EVT || pkt[1] != EVT_LE_META || pkt[3] != SUBEVT_ADV_REPORT {
        return;
    }
    let num_reports = pkt[4] as usize;
    let mut off = 5usize;
    for _ in 0..num_reports {
        // event_type(1) addr_type(1) addr(6) data_len(1) data(L) rssi(1)
        if off + 9 > pkt.len() {
            return;
        }
        let event_type = pkt[off];
        let addr_type = pkt[off + 1];
        let mut addr = [0u8; 6];
        for (j, a) in addr.iter_mut().enumerate() {
            *a = pkt[off + 2 + (5 - j)]; // wire is little-endian → store big-endian
        }
        let data_len = pkt[off + 8] as usize;
        let data_start = off + 9;
        let rssi_idx = data_start + data_len;
        if rssi_idx >= pkt.len() {
            return;
        }
        let rssi = pkt[rssi_idx] as i8;
        let mut ad_summary = summarize_ad(&pkt[data_start..data_start + data_len]);
        if event_type == 0x00 || event_type == 0x01 {
            ad_summary |= ad::CONNECTABLE;
        }
        f(Sighting { addr, addr_type, rssi, ad_summary });
        off = rssi_idx + 1;
    }
}

// =========================================================================
// Heapless sighting table (no_std, no alloc for the standing structure).
// =========================================================================

/// Max distinct advertisers tracked in one window. Bounds the table to a fixed
/// `.bss` footprint (~19 B/entry); a busy RF environment evicts the weakest.
pub const TABLE_CAP: usize = 24;

/// A device unseen for longer than this is aged out of the live window, so
/// [`SightingTable::live_count`] reflects *currently-present* devices — the
/// room-presence signal — not an ever-growing cumulative set.
pub const SIGHTING_TTL_MS: u64 = 15_000;

/// Number of strongest sightings emitted in the relayed record (bounds the frame
/// well under `RELAY_VALUE_MAX`).
pub const TOP_REPORT: usize = 6;

#[derive(Clone, Copy)]
struct Entry {
    addr: [u8; 6],
    addr_type: u8,
    rssi: i8,
    ad_summary: u8,
    last_ms: u64,
    used: bool,
}

impl Entry {
    const EMPTY: Self = Self {
        addr: [0; 6],
        addr_type: 0,
        rssi: 0,
        ad_summary: 0,
        last_ms: 0,
        used: false,
    };
}

/// A fixed-capacity, deduped table of the BLE devices heard this window. No heap:
/// the entries live inline (`.bss`); only [`SightingTable::format_record`] touches
/// `alloc` (to build the relay string, mirroring `own_scan`).
pub struct SightingTable {
    entries: [Entry; TABLE_CAP],
    /// Cumulative advertising reports observed since boot (a liveness counter for
    /// the soak's `BLESOAK advrpt=` line — proves the scanner is actually hearing RF).
    total_reports: u32,
}

impl SightingTable {
    pub const fn new() -> Self {
        Self { entries: [Entry::EMPTY; TABLE_CAP], total_reports: 0 }
    }

    /// Fold one sighting in: refresh an existing device, or insert a new one
    /// (evicting the oldest entry when full). Dedupe is by (addr, addr_type).
    pub fn record(&mut self, s: Sighting, now: u64) {
        self.total_reports = self.total_reports.saturating_add(1);
        // Existing device → refresh RSSI / ad-flags / last-seen.
        for e in self.entries.iter_mut() {
            if e.used && e.addr == s.addr && e.addr_type == s.addr_type {
                e.rssi = s.rssi;
                e.ad_summary |= s.ad_summary;
                e.last_ms = now;
                return;
            }
        }
        // New device → take a free slot, else evict the least-recently-seen entry.
        let mut victim = 0usize;
        let mut oldest = u64::MAX;
        for (i, e) in self.entries.iter().enumerate() {
            if !e.used {
                victim = i;
                break;
            }
            if e.last_ms < oldest {
                oldest = e.last_ms;
                victim = i;
            }
        }
        self.entries[victim] = Entry {
            addr: s.addr,
            addr_type: s.addr_type,
            rssi: s.rssi,
            ad_summary: s.ad_summary,
            last_ms: now,
            used: true,
        };
    }

    /// Drop entries unseen for longer than [`SIGHTING_TTL_MS`] so the live view
    /// tracks presence, not history. Call each tick.
    pub fn age_out(&mut self, now: u64) {
        for e in self.entries.iter_mut() {
            if e.used && now.saturating_sub(e.last_ms) > SIGHTING_TTL_MS {
                e.used = false;
            }
        }
    }

    /// Distinct devices currently present (used, within the TTL). This is the
    /// `ble=` DIAG scalar and the record's `n=` header.
    pub fn live_count(&self, now: u64) -> usize {
        self.entries
            .iter()
            .filter(|e| e.used && now.saturating_sub(e.last_ms) <= SIGHTING_TTL_MS)
            .count()
    }

    /// Cumulative advertising reports observed since boot (soak liveness counter).
    pub fn total_reports(&self) -> u32 {
        self.total_reports
    }

    /// Compact relay record for `smol/<id>/ble`: `BLE|n=<live>|<hex12>,<rssi>,<ad>;…`
    /// for the [`TOP_REPORT`] strongest live devices (nearest-first — the dollhouse
    /// nearest-node signal). `<ad>` is the [`ad`] bitmask as 2 hex nibbles. Diverges
    /// from `DIAG|`/`GRID|`/`BATT|` at byte 0, so a receiver keys on the marker.
    pub fn format_record(&self, now: u64) -> alloc::string::String {
        let mut out = alloc::string::String::new();
        let _ = write!(out, "BLE|n={}", self.live_count(now));
        // Collect live entries, strongest RSSI first, capped at TOP_REPORT. A small
        // fixed selection sort over the bounded table — no alloc, no full sort.
        let mut picked = [false; TABLE_CAP];
        for _ in 0..TOP_REPORT {
            let mut best: Option<usize> = None;
            for (i, e) in self.entries.iter().enumerate() {
                if !e.used || picked[i] || now.saturating_sub(e.last_ms) > SIGHTING_TTL_MS {
                    continue;
                }
                if best.is_none_or(|b| e.rssi > self.entries[b].rssi) {
                    best = Some(i);
                }
            }
            match best {
                Some(i) => {
                    picked[i] = true;
                    let e = &self.entries[i];
                    let a = &e.addr;
                    let _ = write!(
                        out,
                        "|{:02x}{:02x}{:02x}{:02x}{:02x}{:02x},{},{:02x}",
                        a[0], a[1], a[2], a[3], a[4], a[5], e.rssi, e.ad_summary
                    );
                }
                None => break,
            }
        }
        out
    }
}

impl Default for SightingTable {
    fn default() -> Self {
        Self::new()
    }
}
