//! #22 passive BLE observer — the HAL half: owns the esp-wifi [`BleConnector`]
//! and drives the blocking HCI transport that feeds [`crate::net::ble`]'s pure
//! codec + table.
//!
//! Mirrors the `cast` (pure) / `cast_oled` (HAL) split: all the byte logic is in
//! `net/ble.rs` (host-testable); this file is the thin esp-wifi-dependent driver.
//!
//! ## Lifecycle
//! Constructed once from the leaked `'static` [`EspWifiController`] (the same one
//! `RadioManager` uses for WiFi/ESP-NOW) plus the `BT` peripheral, AFTER
//! `esp_wifi::init` + `wifi::new` have run — the coex arbiter must be up before
//! the BT controller starts. [`BleConnector::new`] calls the controller's
//! `ble_init`; dropping it calls `ble_deinit`, so the scanner is held for the
//! program's life (never dropped → BT stays initialised).
//!
//! ## Passive-only, non-blocking drain
//! On the first [`tick`], we send `LE Set Scan Parameters` (passive) + `LE Set
//! Scan Enable`. Thereafter each tick drains the controller's event queue via the
//! **non-blocking** `BleConnector::next` (it returns 0 when empty — it never
//! parks the single-threaded main loop) and folds any advertising reports into
//! the [`SightingTable`]. We never issue a connection or an active-scan probe.
//!
//! [`tick`]: BleScanner::tick
//! [`EspWifiController`]: esp_wifi::EspWifiController

extern crate alloc;

use esp_hal::peripherals::BT;
use esp_wifi::ble::controller::BleConnector;
use esp_wifi::EspWifiController;

use crate::net::ble::{self, SightingTable};

/// Max reports drained per tick — bounds the worst-case main-loop time we spend
/// in a dense RF environment (each drained packet is parsed + folded). Leftover
/// packets stay queued for the next tick; the controller's RX queue absorbs the
/// slack between ticks.
const MAX_DRAIN_PER_TICK: usize = 16;

/// Scratch for one drained HCI packet. Sized to esp-wifi's own HCI buffer (259 B =
/// the max event: 1 H4 + 1 code + 1 len + 255 params + slack) so `read_next`'s
/// unchecked copy of a full-size packet can never overrun it.
const RX_BUF: usize = 259;

/// Owns the BLE controller connection + the sighting table. Lives inside
/// `RadioManager` under `cfg(feature = "ble")`.
pub struct BleScanner {
    conn: BleConnector<'static>,
    table: SightingTable,
    /// False until the first tick has enabled the passive scan.
    started: bool,
    /// Count of `enable`/`params` HCI commands that failed to write (a soak health
    /// signal — a wedged controller shows up as a non-zero, growing value).
    cmd_errs: u32,
}

impl BleScanner {
    /// Bring up the BT controller and take ownership of the HCI connection. The
    /// `ctrl` must be the SAME leaked controller that initialised WiFi/ESP-NOW
    /// (one `esp_wifi::init` per program); `bt` is the `BT` peripheral singleton.
    pub fn new(ctrl: &'static EspWifiController<'static>, bt: BT<'static>) -> Self {
        Self {
            conn: BleConnector::new(ctrl, bt),
            table: SightingTable::new(),
            started: false,
            cmd_errs: 0,
        }
    }

    /// Send the two passive-scan setup commands. Idempotent-safe to re-issue.
    fn enable_scan(&mut self) {
        use embedded_io::Write;
        let params = ble::scan_params_cmd(true, ble::SCAN_INTERVAL_UNITS, ble::SCAN_WINDOW_UNITS);
        if self.conn.write(&params).is_err() {
            self.cmd_errs = self.cmd_errs.saturating_add(1);
        }
        // filter_duplicates OFF: we want repeat adverts for fresh RSSI + host-side dedup.
        let enable = ble::scan_enable_cmd(true, false);
        if self.conn.write(&enable).is_err() {
            self.cmd_errs = self.cmd_errs.saturating_add(1);
        }
    }

    /// Drive one service pass: enable the scan on the first call, then drain any
    /// queued advertising reports into the table and age out stale devices. Cheap
    /// and non-blocking — safe to call every main-loop iteration.
    pub fn tick(&mut self, now: u64) {
        if !self.started {
            self.enable_scan();
            self.started = true;
        }
        let mut buf = [0u8; RX_BUF];
        for _ in 0..MAX_DRAIN_PER_TICK {
            match self.conn.next(&mut buf) {
                Ok(0) | Err(_) => break, // queue empty (or a transient read error) → done this tick
                Ok(n) => ble::parse_adv_reports(&buf[..n], |s| self.table.record(s, now)),
            }
        }
        self.table.age_out(now);
    }

    /// Distinct BLE devices currently present (the `ble=` DIAG scalar).
    pub fn live_count(&self, now: u64) -> usize {
        self.table.live_count(now)
    }

    /// Cumulative advertising reports since boot (soak liveness).
    pub fn total_reports(&self) -> u32 {
        self.table.total_reports()
    }

    /// HCI command-write failures since boot (0 = controller healthy).
    pub fn cmd_errs(&self) -> u32 {
        self.cmd_errs
    }

    /// Compact relay record for `smol/<id>/ble` (nearest-first top-N). Empty-ish
    /// (`BLE|n=0`) when nothing is in range — the caller may skip publishing it.
    pub fn record(&self, now: u64) -> alloc::string::String {
        self.table.format_record(now)
    }
}
