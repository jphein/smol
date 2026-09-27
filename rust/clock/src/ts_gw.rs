//! #548 — the Tapstone USB-serial gateway (`--features tapstone-gw`).
//!
//! One smol node on the arena laptop's USB, bridging the laptop to the mesh (tapstone arena spec
//! §4): every received `SMOLv1 MATCH ` frame goes up as `@TS1 RX …` (emitted from the RX drain,
//! `RadioManager::ts_forward`), and every `@TS1 TX …` line comes down and goes out through
//! `send_to`, answered `TXOK` / `TXERR`. `HELLO` at boot and on `PING`, `ROSTER` every 2 s. The
//! wire format is `net::ts_lines`; the design and the bench procedure are
//! `targets/c3-tapstone-gw/README.md`.
//!
//! The node joins the mesh as an ordinary leaf with WiFi off, and so can never be the crown: the
//! guards are in `net/mode.rs` (search `#548`).
//!
//! ## Sharing the USB-Serial-JTAG with `println!`
//!
//! The pattern is c6-watch's debug console (`targets/c6-watch/src/debug_console.rs`): esp-println
//! (`jtag-serial`) writes the peripheral's TX FIFO by raw MMIO under its own critical section and
//! never touches RX, so this module takes the esp-hal driver and uses ONLY its RX half, polled.
//! All output goes back through `println!`. `UsbSerialJtag::new` does not reset the peripheral on
//! the C3 (USB_DEVICE is in esp-hal's `KEEP_ENABLED` set, so the refcount is already 1 and the
//! reset branch is not taken), which is what keeps the host's `/dev/ttyACM*` alive.

use esp_hal::peripherals::USB_DEVICE;
use esp_hal::time::{Duration, Instant};
use esp_hal::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx};
use esp_hal::Blocking;
use esp_println::println;

use crate::net::mode::{RadioManager, ROSTER_VIEW_CAP};
use crate::net::ts_lines::{self, Inbound, LineReader, Reason, Show, TX_FRAME_MAX};

/// ROSTER cadence (spec D6).
const ROSTER_EVERY_MS: u64 = 2_000;

/// While a line is part-read, keep polling the FIFO this long for its next packet before handing
/// the loop back. The C3's RX FIFO is 64 bytes and the host can only send the next USB packet once
/// it is drained, so without this a maximal ~500-byte `TX` line would take one 64-byte packet per
/// loop pass — eight passes, ~160 ms — to arrive. Full-speed USB delivers the next packet within
/// the same 1 ms frame, so a short spin collects the whole line in one pass.
const LINE_SPIN: Duration = Duration::from_micros(2_000);

/// Upper bound on one `pump`'s USB reading, so a host flooding lines cannot starve the loop.
const PUMP_BUDGET: Duration = Duration::from_micros(4_000);

/// The gateway's main-loop sub-tick: [`crate::SUBTICK_MS`] split into this many slices, with the
/// radio drained and the USB pumped between them. `SUBTICK_MS` itself must stay 20 ms — the loop's
/// `now / N != (now - SUBTICK_MS) / N` edge detectors look back exactly one sub-tick (see
/// `subtick` in main.rs) — so the gateway keeps the 20 ms tick and services the radio inside it.
/// A frame then waits at most one slice (5 ms), not one tick (20 ms), before its `RX` line is
/// written — the spec's budget is 20 ms end to end.
const SLICES: u32 = 4;

pub struct Gateway {
    rx: UsbSerialJtagRx<'static, Blocking>,
    /// Never written — all output is `println!` (see the module doc). Held so the driver half is
    /// not dropped.
    _tx: UsbSerialJtagTx<'static, Blocking>,
    reader: LineReader,
    frame: [u8; TX_FRAME_MAX],
    next_roster_ms: u64,
    warned_crown: bool,
}

impl Gateway {
    pub fn new(usb: USB_DEVICE<'static>) -> Self {
        let (rx, tx) = UsbSerialJtag::new(usb).split();
        Self {
            rx,
            _tx: tx,
            reader: LineReader::new(),
            frame: [0; TX_FRAME_MAX],
            next_roster_ms: 0,
            warned_crown: false,
        }
    }

    /// `@TS1 HELLO …` — at boot; `PING` answers through the same function.
    pub fn hello(&self, radio: Option<&RadioManager>) {
        emit_hello(radio);
    }

    /// Read what the host sent, answer each complete line, and emit ROSTER when due.
    pub fn pump(&mut self, mut radio: Option<&mut RadioManager>, now: u64) {
        let start = Instant::now();
        let mut idle_since = start;
        loop {
            match self.rx.read_byte() {
                Ok(b) => {
                    idle_since = Instant::now();
                    if let Some((line, truncated)) = self.reader.push(b) {
                        let parsed = ts_lines::parse_inbound(line, truncated, &mut self.frame);
                        handle(parsed, line, &self.frame, radio.as_deref_mut());
                    }
                }
                Err(_) => {
                    let waiting = self.reader.in_progress() && idle_since.elapsed() < LINE_SPIN;
                    if !waiting {
                        break;
                    }
                }
            }
            if start.elapsed() >= PUMP_BUDGET {
                break;
            }
        }

        if now >= self.next_roster_ms {
            self.next_roster_ms = now + ROSTER_EVERY_MS;
            if let Some(r) = radio.as_deref() {
                if r.ts_is_crown() && !self.warned_crown {
                    // Unreachable by construction (WiFi never comes up); said loudly if it ever is.
                    log::error!("smol #548: tapstone gateway is the CROWN — its WiFi bursts will deafen it");
                    self.warned_crown = true;
                }
                let mut entries = [(0u8, [0u8; 6], 0i8); ROSTER_VIEW_CAP];
                let n = r.ts_roster(now, &mut entries);
                // An empty ROSTER is not a line the arena can parse (see `write_roster`), so a
                // beat with no known peer sends nothing.
                if n > 0 {
                    println!("{}", Show(|f| ts_lines::write_roster(f, &entries[..n]).map(|_| ())));
                }
            }
        }
    }
}

/// `@TS1 HELLO <mac> <node_id> <fw_hash8> <group_epoch>`. Without a radio there is no MAC to
/// report and nothing to bridge, so it logs instead; the arena's discovery then moves on.
fn emit_hello(radio: Option<&RadioManager>) {
    match radio {
        Some(r) => {
            let mac = r.ts_self_mac();
            let fw = ts_lines::fw_hash8(env!("BUILD_HASH"));
            let epoch = crate::secrets::GROUP_KEY_EPOCH;
            println!("{}", Show(|f| ts_lines::write_hello(f, &mac, r.ts_node_id(), fw, epoch)));
        }
        None => log::error!("smol #548: tapstone gateway has no radio — no HELLO, TX refused"),
    }
}

fn handle(parsed: Inbound, line: &[u8], frame: &[u8; TX_FRAME_MAX], radio: Option<&mut RadioManager>) {
    match parsed {
        Inbound::Ping => emit_hello(radio.as_deref()),
        Inbound::Tx { id, dst, len } => {
            let result = match radio {
                Some(r) => r.ts_send(dst, &frame[..len]),
                None => Err(Reason::NoRadio),
            };
            match result {
                Ok(()) => println!("{}", Show(|f| ts_lines::write_txok(f, id))),
                Err(reason) => println!("{}", Show(|f| ts_lines::write_txerr(f, id, reason))),
            }
        }
        Inbound::TxRefused { id, reason } => {
            println!("{}", Show(|f| ts_lines::write_txerr(f, id, reason)));
        }
        Inbound::Ignored => {
            // Only worth a word if it looked like it was meant for us; a stray newline is not.
            if line.starts_with(ts_lines::PREFIX.as_bytes()) {
                log::warn!("smol #548: ignored an @TS1 line the gateway cannot answer ({} B)", line.len());
            }
        }
    }
}

/// The gateway's version of main.rs's `subtick`: the same 20 ms, in [`SLICES`] slices, draining
/// the radio (which forwards MATCH frames as it goes) and pumping USB between them.
pub async fn subtick(radio: &mut Option<&'static mut RadioManager>, gw: &mut Gateway) {
    let slice = embassy_time::Duration::from_millis((crate::SUBTICK_MS / SLICES) as u64);
    for _ in 0..SLICES {
        embassy_time::Timer::after(slice).await;
        if let Some(r) = radio.as_deref_mut() {
            let _ = r.service();
        }
        gw.pump(radio.as_deref_mut(), crate::millis());
    }
}
