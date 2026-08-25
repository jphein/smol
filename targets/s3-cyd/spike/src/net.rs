//! M2 — WiFi STA association + DHCP. Feature `wifi`.
//!
//! Minimal by intent: bring the radio up, associate, take a lease, print it, and
//! keep the heartbeat running. No sockets, no application traffic — that is M4.
//!
//! ---------------------------------------------------------------------------
//! CREDENTIALS — build-time only, never on disk, never in the tree
//! ---------------------------------------------------------------------------
//! `build-remote.sh` pulls the PSK from Vaultwarden on katana and passes it as an
//! environment variable to the remote cargo over ssh. `option_env!` bakes it in
//! at compile time. It is never written to a file on either host and never
//! echoed into a build log.
//!
//! **`option_env!`, not `env!`** — this is a deliberate divergence from
//! `cyd-c5/spike`, which uses `env!` and therefore fails the build outright when
//! the vault is locked. Here a credential-less build must still COMPILE, FLASH
//! and RUN, saying so on serial, so that the M1 screens stay reachable for
//! someone without vault access. (burrito-fw's pattern, and it earns its keep:
//! the person most likely to hit a locked vault is the person debugging
//! something else entirely.)

use core::fmt::Write as _;

use embassy_futures::block_on;
use esp_hal::{
    delay::Delay,
    interrupt::software::SoftwareInterruptControl,
    peripherals,
    time::{Duration as HalDuration, Instant as HalInstant},
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::wifi::{
    sta::StationConfig, Config as WifiConfig, ControllerConfig, PowerSaveMode, WifiController,
};
use smoltcp::{
    iface::{Config as IfaceConfig, Interface as SmolIface, SocketSet, SocketStorage},
    socket::dhcpv4,
    time::Instant as SmolInstant,
    wire::{EthernetAddress, HardwareAddress, IpCidr},
};
use static_cell::StaticCell;

use crate::{mqtt, radio_dev::SmolWifiDevice};

// ------------------------------------------------------------ credentials ----

/// Injected by `build-remote.sh` from Vaultwarden. `None` means "built without
/// credentials" — the board says so rather than pretending to connect.
///
/// Names match `cyd-c5/spike/build-remote.sh` so one operator habit covers both
/// spikes.
pub const WIFI_SSID: Option<&str> = option_env!("SPIKE_WIFI_SSID");
pub const WIFI_PSK: Option<&str> = option_env!("SPIKE_WIFI_PSK");

// ------------------------------------------------------------ espnow-only ----

/// **ESP-NOW-only mode: bring the radio up, pin a channel, do NOT associate.**
///
/// ## Why this exists — a physics constraint, not a convenience
///
/// This board has ONE radio. A STA association owns the radio's channel, and the
/// AP is glass-verified on **channel 1** (M2's first flash: `ssid jplovescl,
/// bssid 9e:5c:8e:cb:db:90, channel 1`). The smol mesh lives on **channel 6**.
/// **An associated board cannot hear the mesh at all** — not weakly, not
/// intermittently; it is listening to a different channel.
///
/// So M3-with-association is not "degraded", it is **impossible today**, and the
/// probe would report a dead mesh while every part of it worked correctly. That
/// is exactly the failure shape this fleet has already paid for once: a channel
/// mismatch misread as a coexistence/physics problem.
///
/// An earlier version of this spike omitted this mode, reasoning that M3 should
/// "run co-channel or not at all, rather than hide the single-radio channel
/// constraint phase 2 must face". **That was right about phase 2 and wrong about
/// the probe.** Phase 2 does have to solve co-channel operation; M3's job is to
/// prove ESP-NOW reaches the mesh THAT EXISTS. Refusing to look until the network
/// is rearranged is not rigour.
///
/// Set at build time by `build-remote.sh`: `SPIKE_ESPNOW_ONLY=1`. Meaningless
/// without the probe, so it is `cfg!`-gated on `radio` — otherwise a `wifi`-only
/// build would silently never associate, which would read as a broken radio.
pub const ESPNOW_ONLY: bool = cfg!(feature = "radio") && const_is_one(option_env!("SPIKE_ESPNOW_ONLY"));

/// `Some("1")` at COMPILE TIME.
///
/// cyd-c5 writes this as `matches!(option_env!(..), Some("1"))`, which is correct
/// there because it runs inside a function. In a `const` it does not compile:
/// `str` cannot be compared at compile time (`PartialEq` is not yet a const
/// trait), so the match arm is rejected. Comparing the bytes is the const-safe
/// equivalent, and the reason is recorded here so the "simpler" form is not
/// helpfully restored.
const fn const_is_one(s: Option<&str>) -> bool {
    match s {
        Some(v) => {
            let b = v.as_bytes();
            b.len() == 1 && b[0] == b'1'
        }
        None => false,
    }
}

/// esp-radio heap size in KiB. `SPIKE_HEAP_KB`, default **96** (smol's
/// known-good pairing with the #140 RX tuning). Exists so the
/// cadence-vs-heap experiment described in `init` is one build away rather than
/// an argument. Internal RAM either way — see landmine L3.
pub const HEAP_KB: u32 = const_u32(option_env!("SPIKE_HEAP_KB"), 96);
const HEAP_BYTES: usize = HEAP_KB as usize * 1024;

const fn const_u32(s: Option<&str>, default: u32) -> u32 {
    match s {
        None => default,
        Some(v) => {
            let b = v.as_bytes();
            if b.is_empty() || b.len() > 4 {
                return default;
            }
            let mut acc: u32 = 0;
            let mut i = 0;
            while i < b.len() {
                if b[i] < b'0' || b[i] > b'9' {
                    return default;
                }
                acc = acc * 10 + (b[i] - b'0') as u32;
                i += 1;
            }
            acc
        }
    }
}

// A heap too small to hold the RX ceiling is the bug we just shipped; a heap
// larger than the S3's internal DRAM will not link. Bound both ends.
const _: () = assert!(
    HEAP_KB >= 32 && HEAP_KB <= 192,
    "SPIKE_HEAP_KB must be 32..=192 KiB (internal DRAM; see the M2 OOM note in net::init)"
);

/// Channel to pin in espnow-only mode. `SPIKE_ESPNOW_CHANNEL`, default **6** —
/// `ESP_NOW_FIXED_CHANNEL`, the smol mesh channel.
pub const ESPNOW_CHANNEL: u8 = const_u8(option_env!("SPIKE_ESPNOW_CHANNEL"), 6);

/// Parse a decimal `u8` at compile time, falling back on anything unexpected.
const fn const_u8(s: Option<&str>, default: u8) -> u8 {
    match s {
        None => default,
        Some(v) => {
            let b = v.as_bytes();
            if b.is_empty() || b.len() > 2 {
                return default;
            }
            let mut acc: u8 = 0;
            let mut i = 0;
            while i < b.len() {
                if b[i] < b'0' || b[i] > b'9' {
                    return default;
                }
                acc = acc * 10 + (b[i] - b'0');
                i += 1;
            }
            acc
        }
    }
}

// 2.4 GHz only on this silicon, so 1..=14 is the whole legal space. Catching a
// bad channel HERE beats catching it as a silent no-op on the air, where the
// symptom is "the mesh never answers" — indistinguishable from the very problem
// espnow-only mode exists to rule out.
const _: () = assert!(
    ESPNOW_CHANNEL >= 1 && ESPNOW_CHANNEL <= 14,
    "SPIKE_ESPNOW_CHANNEL must be 1..=14 (2.4 GHz); the S3 has no other band"
);

// ------------------------------------------------------------------ tuning ---

/// Association backoff floor, doubling to [`BACKOFF_MAX_MS`], reset on success.
/// burrito-fw's proven shape on this exact hardware.
const BACKOFF_MIN_MS: u64 = 500;
const BACKOFF_MAX_MS: u64 = 15_000;

/// ===========================================================================
/// RX DRAIN DUTY CYCLE — the second half of the M2 OOM, and the subtler half
/// ===========================================================================
///
/// **The original design serviced the network stack 50 ms out of every 1000 ms —
/// a 5% duty cycle — and that was my error, made for a defensible reason.**
///
/// The old `DHCP_SLICE_MS = 50` existed so "a silent DHCP server costs 50 ms per
/// heartbeat instead of the whole loop". That protected the heartbeat and
/// created a 950 ms window in which *nothing drained the RX path*.
///
/// The math, which is the point:
///
/// ```text
///   heartbeat period          1000 ms
///   stack serviced                50 ms   -> 5% duty cycle
///   unattended window            950 ms
///
///   esp-radio's STA queue caps at rx_queue_size = 8 frames, so frames beyond
///   that are DROPPED, not queued — the queue itself cannot run away.
///   BUT every queued frame pins its driver buffer (`PacketBuffer` owns an `eb`
///   and only releases it on Drop, i.e. when WE poll it out). So a 5% duty
///   cycle holds up to 8 driver buffers hostage for ~950 ms at a time, while a
///   broadcast-heavy VLAN keeps asking the pool for more.
/// ```
///
/// So the slice did not *cause* the exhaustion — the undersized heap did — but it
/// made the pool's job much harder, and on a busier VLAN it would be sufficient
/// on its own. **Servicing the stack is now the loop's default activity**: the
/// heartbeat window is spent polling rather than sleeping (see [`Net::tick`]).
///
/// Kept as a named constant rather than inlined because it is the knob to reach
/// for if the heartbeat ever needs protecting again — but raise the heap first.
const STACK_POLL_STEP_MS: u64 = 1;

/// smoltcp's socket set: DHCP (M2) + one TCP socket (M4's MQTT session).
static SOCKETS: StaticCell<[SocketStorage<'static>; 2]> = StaticCell::new();

/// MQTT's TCP buffers. Static rather than heap because the heap belongs to
/// esp-radio — see the OOM note in `init`; adding a 4 KiB heap consumer to the
/// pool that just ran out would be an unforced error.
static TCP_RX: StaticCell<[u8; 1536]> = StaticCell::new();
static TCP_TX: StaticCell<[u8; 1536]> = StaticCell::new();

// ------------------------------------------------------------------- state ---

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Built without credentials. Terminal — the radio is up, nothing will join.
    NoCredentials,
    /// espnow-only mode: radio up, channel pinned, association deliberately
    /// skipped. Terminal — this build is not trying to join anything.
    EspNowOnly,
    /// Waiting out the association backoff.
    Backoff,
    /// Associated; DHCP has not yet produced a lease.
    Dhcp,
    /// Lease in hand.
    Up,
}

impl Link {
    /// Deliberately says *which* leg is broken. "no credentials", "cannot
    /// associate" and "associated but no lease" send you to three completely
    /// different places, and a single "offline" would hide that.
    pub fn label(self) -> &'static str {
        match self {
            Link::NoCredentials => "no wifi credentials in this build",
            // ⚠️ This used to read "espnow-only - channel pinned, not
            // associating" — asserting a pin that had FAILED, on a line that
            // never checked it. It actively misled the M3 bench read. The pin's
            // real outcome is reported by the probe (`espnow_probe::label`),
            // which measures it; this label now claims only what it knows.
            Link::EspNowOnly => "espnow-only - not associating",
            Link::Backoff => "not associated - retrying",
            Link::Dhcp => "associated - waiting for DHCP",
            Link::Up => "up",
        }
    }
}

pub struct Net {
    live: Option<Live>,
    state: Link,
    /// Handed to `espnow_probe::attach` by main. Held here because ESP-NOW and
    /// the STA come out of the SAME `wifi::new` call — there is one radio, and
    /// `interfaces` is consumed to get the station device.
    #[cfg(feature = "radio")]
    esp_now: Option<esp_radio::esp_now::EspNow<'static>>,
    /// ⛔ **THE FIELD THAT EXISTS SO THE RADIO STAYS UP.**
    ///
    /// On any path that returns without a [`Live`] — espnow-only, or no
    /// credentials — the `WifiController` has nowhere to live, and a local that
    /// goes out of scope is DROPPED. `WifiController::drop` calls
    /// `wifi_deinit()` (esp-radio `wifi/mod.rs`, `impl Drop for WifiController`),
    /// which tears the radio down again.
    ///
    /// **That is exactly what broke M3's first window** (2026-08-25): `init`
    /// called `set_config` — which really does start the controller — then
    /// returned `Net { live: None, .. }`, dropping the controller on the way out.
    /// By the time `espnow_probe::attach` ran, the radio was deinitialised:
    /// `set_channel(6)` returned `Error(Other(12289))` = `0x3001`, the
    /// WIFI_NOT_INIT class, and every send failed `InterfaceMismatch`.
    ///
    /// The hazard was already written down — in `espnow_probe.rs`, which says
    /// "net::init owns the WifiController and must keep it alive". It was
    /// documented in the file that consumes the radio and violated in the file
    /// that creates it.
    ///
    /// ## Why this carries `#[allow(dead_code)]`
    ///
    /// It is never READ, and clippy is right to say so. It is load-bearing by
    /// **existing**: the value's lifetime is the point, and its `Drop` is the
    /// hazard. This is the dead-code lint as a QUESTION, not a verdict — and the
    /// answer is that the field's apparent uselessness IS the bug it prevents.
    ///
    /// ⛔ **Deleting this field to satisfy the lint reintroduces the M3 failure
    /// verbatim**: zero frames transmitted, `set_channel` → `0x3001`, every send
    /// `InterfaceMismatch`. The lint would go green and the radio would go down.
    #[allow(dead_code)]
    parked: Option<WifiController<'static>>,
}

struct Live {
    controller: WifiController<'static>,
    device: SmolWifiDevice,
    iface: SmolIface,
    sockets: SocketSet<'static>,
    dhcp: smoltcp::iface::SocketHandle,
    backoff_ms: u64,
    next_attempt: HalInstant,
    /// M4. Idle until a lease exists; `tick` only drives it in `Link::Up`.
    mqtt: mqtt::Client,
}

/// smoltcp's clock, from the HAL's.
fn now() -> SmolInstant {
    SmolInstant::from_micros(HalInstant::now().duration_since_epoch().as_micros() as i64)
}

// -------------------------------------------------------------------- init ---

/// Bring up the heap, the scheduler and the radio, then (if credentials exist)
/// configure the station and begin associating.
///
/// Order is load-bearing and each step is asserted by the layer above it:
/// **heap -> scheduler -> radio**. esp-radio panics with *"The scheduler must be
/// initialized before initializing the radio"* if `esp_rtos::start` has not run,
/// so getting this wrong is loud rather than subtle.
pub fn init(
    timg0: peripherals::TIMG0<'static>,
    sw_interrupt: peripherals::SW_INTERRUPT<'static>,
    wifi: peripherals::WIFI<'static>,
) -> Net {
    // ---- heap --------------------------------------------------------------
    // ⛔ INTERNAL RAM ON PURPOSE. DO NOT MOVE THIS HEAP TO PSRAM.
    //
    // The obvious objection is in the boot log: this board has 8 MiB of octal
    // PSRAM mapped and this spends 64 KiB of scarce internal DRAM — DRAM that
    // comes out of the same pool as `.bss`. `esp_alloc::psram_allocator!` exists
    // and appears to fix that for free.
    //
    // **It would be a silent-corruption bug on this chip.** esp-alloc's own docs:
    // on ESP32, ESP32-S2 and **ESP32-S3** the atomic instructions DO NOT WORK
    // CORRECTLY when the memory they access is in PSRAM, so the allocator must
    // not be used for `Atomic*` types — *directly or indirectly*. We are an S3
    // and the consumer here is the WiFi driver, whose internals we neither
    // control nor audit and which certainly contains synchronisation primitives.
    //
    // "Indirectly" is the word that matters: nothing fails at the allocation
    // site. Atomics simply stop being atomic, inside the radio, intermittently.
    //
    // ===================================================================
    // 96 KiB, NOT 64 — and this line is why M2's first flash panicked.
    // ===================================================================
    //
    // **THE BUG (2026-08-24, first M2 flash):** the board associated fine, then
    // died during the DHCP wait with
    //
    //     memory allocation of 96 bytes failed
    //     VecDeque<esp_radio::wifi::private::PacketBuffer>::push_back_mut -> grow
    //
    // **THE ROOT CAUSE WAS NOT THE RX QUEUE**, which is what that backtrace looks
    // like. Two pieces of evidence rule it out:
    //
    //   1. That queue IS bounded. `recv_cb_sta` in esp-radio pushes only
    //      `if queue.len() < RX_QUEUE_SIZE` (wifi/mod.rs ~:1018), and we set
    //      `rx_queue_size = 8`. It cannot run away.
    //   2. The failed allocation was **96 bytes**. A heap that cannot serve 96 B
    //      was already exhausted; the VecDeque was merely the next caller through
    //      the door. **The backtrace names the victim, not the culprit.**
    //
    // The culprit is the WiFi driver's demand-driven RX buffer pool. We ask for
    // `static_rx_buf_num = 16` / `dynamic_rx_buf_num = 40` (below, in the
    // ControllerConfig) — smol's #140 tuning, copied verbatim and correctly cited.
    // **What was NOT copied is the heap those numbers were sized against.**
    // In smol they live NINE LINES APART in one file: `net.rs:322`
    // (`heap_allocator!(size: 96 * 1024)`) and `net.rs:331`
    // (`radio_controller_config()`). I took the second and not the first.
    //
    // Association succeeded because it needs few packets. VLAN8 is
    // broadcast-heavy, so the pool filled during the DHCP wait — which is exactly
    // when the failure appeared.
    //
    // No byte-per-buffer figure is quoted here on purpose: it is not derivable
    // from the crates in this tree, and smol's own `budget.rs` says the same in
    // as many words — esp-radio's demand-driven RX pool "could **not** be bounded
    // from source", which is why its scan floor carries a 0.5x margin instead of
    // an enumeration. Matching smol's known-good PAIRING is therefore the honest
    // fix; inventing arithmetic to justify a smaller number would not be.
    //
    // ---------------------------------------------------------------------
    // ⚠️ VERDICT REVISED 2026-08-25 — I CANNOT ISOLATE THIS FROM EVIDENCE ALONE
    // ---------------------------------------------------------------------
    // My first verdict called the heap the root cause and the drain cadence a
    // contributing factor. The C5 session's data does not support that ordering,
    // and the honest answer is that **the two are not separable from what we
    // observed.**
    //
    // Their M2/M4 ran DHCP+MQTT on the SAME VLAN8 broadcast environment with the
    // SAME #140 tuning and never OOM'd. They differ from the failing build in
    // BOTH variables at once: 96 KiB heap AND a continuous ~10 ms drain. One
    // observation, two differences — that isolates nothing.
    //
    // Both mechanisms are real and they compound:
    //   * Total RX demand is CEILINGED by construction (16 static + 40 dynamic).
    //     If that ceiling plus everything else exceeds the heap, a broadcast-heavy
    //     VLAN reaches it whatever the cadence. -> heap matters.
    //   * But dynamic buffers are allocated ON DEMAND. Draining every 10 ms may
    //     steady-state at a handful; leaving 950 ms gaps walks demand toward the
    //     cap. -> cadence decides whether the ceiling is ever approached.
    //
    // **THE EXPERIMENT THAT WOULD SETTLE IT** (one flash, and the reason this is
    // a knob instead of a literal): build with the cadence fix and the ORIGINAL
    // heap —
    //
    //     SPIKE_HEAP_KB=64 ./build-remote.sh --features wifi
    //
    // If DHCP completes at 64 KiB, the cadence was the fix and 96 KiB is margin.
    // If it still OOMs, the heap was load-bearing and cadence was amplification.
    // Either way the answer is one flash, not an argument.
    //
    // The DEFAULT is 96 KiB regardless — matching smol's known-good pairing is
    // right for a build meant to work, and the knob exists so headroom cannot
    // silently mask an unproven fix. Do not remove it before the experiment runs.
    //
    // ⛔ STILL INTERNAL RAM. DO NOT MOVE THIS HEAP TO PSRAM (landmine L3).
    // esp-alloc's docs: on ESP32/S2/**S3** atomics DO NOT WORK CORRECTLY in
    // PSRAM, and the allocator must not serve `Atomic*` — *directly or
    // indirectly*. The consumer here is the WiFi driver, whose internals we do
    // not audit and which certainly contains synchronisation primitives. Nothing
    // would fail at the allocation site; atomics would just quietly stop being
    // atomic inside the radio. The S3 has 512 KB internal, so 96 KiB is
    // affordable — and it is the one number here with a known-good precedent.
    esp_alloc::heap_allocator!(size: HEAP_BYTES);

    // ---- scheduler ---------------------------------------------------------
    // MUST precede the radio. Pre-0.18 this lived inside esp-wifi as
    // `builtin-scheduler`; #233 moved it out to esp-rtos. `esp_rtos::start()`
    // turns main() into the scheduler's pinned main task, so the blocking
    // superloop keeps running as straight-line code — no executor, no async.
    let timg0 = TimerGroup::new(timg0);
    let sw = SoftwareInterruptControl::new(sw_interrupt);
    esp_rtos::start(timg0.timer0, sw.software_interrupt0);
    println!("[net] esp-rtos scheduler started");

    // ---- radio -------------------------------------------------------------
    // `wifi::new` is what actually initialises the radio in 0.18 — there is no
    // public `esp_radio::init()` any more.
    //
    // The RX tuning mirrors smol's #140 values (`net.rs::radio_controller_config`)
    // so this board's buffer behaviour matches the rest of the fleet rather than
    // whatever the upstream default happens to be this release.
    let cfg = ControllerConfig::default()
        .with_static_rx_buf_num(16)
        .with_dynamic_rx_buf_num(40)
        .with_rx_queue_size(8)
        .with_rx_ba_win(12);
    let (mut controller, interfaces) = esp_radio::wifi::new(wifi, cfg).expect("wifi::new");
    println!("[net] radio up");

    // Taken FIRST: `interfaces.station` is moved into the smoltcp device below,
    // and both halves come out of this one struct.
    #[cfg(feature = "radio")]
    let esp_now = Some(interfaces.esp_now);

    // ⚠️ NO BAND PINNING HERE, AND THAT IS CORRECT FOR THIS SILICON.
    //
    // A reader arriving from `cyd-c5/spike` will look for a
    // `set_band_mode(BandMode::_2_4G)` call at exactly this point — it is that
    // spike's single most important line, because the C5 is dual-band, defaults
    // to `BandMode::Auto`, and actively prefers 5 GHz with a +10 dB bias, which
    // silently drags ESP-NOW off the 2.4 GHz C3 mesh.
    //
    // **The ESP32-S3 has no 5 GHz radio at all.** There is no band to pin, no
    // `Auto` to correct, and no way for this board to leave 2.4 GHz. Stated here
    // rather than left as an absence, because an absence is indistinguishable
    // from an oversight — and the oversight it resembles cost the C5 lane real
    // time.

    let (ssid, psk) = match (WIFI_SSID, WIFI_PSK) {
        (Some(s), Some(p)) if !s.is_empty() && !p.is_empty() => (s, p),
        _ => {
            // The radio is up — M2's bring-up half is proven either way — but
            // nothing will be joined.
            //
            // NOTE the real limitation this leaves: `set_config` is what STARTS
            // the controller in 0.18 (there is no separate `start()`), so on this
            // path the controller is never started and ESP-NOW would not
            // transmit either. A credential-less `--features radio` build
            // therefore proves compilation and boot, not the air. Said plainly
            // rather than papered over; the fix, if M3 ever needs it, is
            // cyd-c5's espnow-only mode.
            println!("[net] ⚠️ no wifi credentials in this build — not associating");
            println!("[net]    (build with ./build-remote.sh, which pulls the PSK from the vault)");
            // NOTE: `set_config` was never reached on this path, so the
            // controller is initialised but NOT STARTED — ESP-NOW cannot
            // transmit from here. Parked anyway: dropping it would additionally
            // deinit the radio, and a half-up radio is easier to diagnose than a
            // torn-down one.
            return Net {
                live: None,
                state: Link::NoCredentials,
                #[cfg(feature = "radio")]
                esp_now,
                parked: Some(controller),
            };
        }
    };

    // ---- station config ----------------------------------------------------
    // The SSID is printed; the PSK never is, and its length is the most that is
    // ever said about it.
    println!("[net] joining \"{}\" (psk {} chars)", ssid, psk.len());

    let sta = StationConfig::default()
        .with_ssid(ssid)
        .with_password(psk.into());

    // #233: `set_config` STARTS the controller in 0.18 — there is no separate
    // `start()`. If you are looking for one, this is it.
    controller
        .set_config(&WifiConfig::Station(sta))
        .expect("set_config");

    // ---- espnow-only: start the controller, skip association ---------------
    //
    // `set_config` IS still called above — in esp-radio 0.18 that is what STARTS
    // the controller (there is no separate `start()`), and ESP-NOW cannot
    // transmit on a stopped controller. Only `connect_async` is skipped. Same
    // shape as cyd-c5's spike, which is glass-verified on the C5.
    //
    // The channel itself is pinned later, on the `EspNow` handle in
    // `espnow_probe::attach` — see there for why the ORDER matters.
    if ESPNOW_ONLY {
        println!(
            "[net] ESPNOW-ONLY mode: controller STARTED in STA mode, association skipped"
        );
        println!(
            "[net]    channel {} will be pinned by the probe — watch for the result line",
            ESPNOW_CHANNEL
        );
        println!("[net]    (the AP is on ch1 and the mesh on ch6 — one radio cannot do both)");
        // `controller` is PARKED, not dropped. See `Net::parked` — dropping it
        // here is the bug that made M3's first window transmit zero frames.
        return Net {
            live: None,
            state: Link::EspNowOnly,
            #[cfg(feature = "radio")]
            esp_now,
            parked: Some(controller),
        };
    }

    // ---- power saving ------------------------------------------------------
    // `PowerSaveMode::None` BEFORE the first join. Modem sleep costs 100-300 ms
    // per exchange and is the single highest-leverage latency line on this whole
    // class of board (burrito-fw measured it). A spike that leaves it on will
    // produce latency numbers nobody should trust.
    match controller.set_power_saving(PowerSaveMode::None) {
        Ok(()) => println!("[net] power save disabled"),
        Err(e) => println!("[net] ⚠️ could not disable power save: {:?}", e),
    }

    // ---- smoltcp -----------------------------------------------------------
    let mut device = SmolWifiDevice::new(interfaces.station);
    let mac = device.mac_address();
    println!(
        "[net] sta mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    let iface = SmolIface::new(
        IfaceConfig::new(HardwareAddress::Ethernet(EthernetAddress::from_bytes(&mac))),
        &mut device,
        now(),
    );

    let mut sockets = SocketSet::new(&mut SOCKETS.init([SocketStorage::EMPTY; 2])[..]);
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let tcp = sockets.add(smoltcp::socket::tcp::Socket::new(
        smoltcp::socket::tcp::SocketBuffer::new(&mut TCP_RX.init([0; 1536])[..]),
        smoltcp::socket::tcp::SocketBuffer::new(&mut TCP_TX.init([0; 1536])[..]),
    ));

    Net {
        live: Some(Live {
            controller,
            device,
            iface,
            sockets,
            dhcp,
            backoff_ms: BACKOFF_MIN_MS,
            // First attempt immediately.
            next_attempt: HalInstant::now(),
            mqtt: mqtt::Client::new(tcp),
        }),
        state: Link::Backoff,
        #[cfg(feature = "radio")]
        esp_now,
        // The controller lives in `Live` on this path.
        parked: None,
    }
}

// -------------------------------------------------------------------- tick ---

impl Net {
    pub fn state(&self) -> Link {
        self.state
    }

    /// M4's state, for the heartbeat line. `None` before a lease exists.
    pub fn mqtt_state(&self) -> Option<mqtt::Mqtt> {
        self.live.as_ref().map(|l| l.mqtt.state())
    }

    /// Hand the ESP-NOW half to the M3 probe. Callable once; subsequent calls
    /// return `None` rather than handing out a second owner of one radio.
    #[cfg(feature = "radio")]
    pub fn take_esp_now(&mut self) -> Option<esp_radio::esp_now::EspNow<'static>> {
        self.esp_now.take()
    }

    /// Called once per heartbeat from `main`'s superloop, and **given the whole
    /// heartbeat window to spend**.
    ///
    /// This REPLACES the main loop's blind `delay_millis(HEARTBEAT_MS)`. The loop
    /// used to sleep through the window; now it services the network stack
    /// through it, because sleeping is what starved the RX path and turned an
    /// undersized heap into a panic (see `STACK_POLL_STEP_MS` for the duty-cycle
    /// math, and the heap comment in `init` for the root cause).
    ///
    /// **Do not "optimise" this back into a quick tick plus a sleep.** The sleep
    /// is not free time; it is time during which queued frames pin driver
    /// buffers.
    pub fn tick(&mut self, delay: &Delay, window_ms: u64) {
        let deadline = HalInstant::now() + HalDuration::from_millis(window_ms);

        let Some(live) = self.live.as_mut() else {
            // NoCredentials or EspNowOnly — both terminal and both already
            // reported at init. There is no smoltcp stack to service (espnow-only
            // never built one), so honour the window as a plain sleep and let the
            // ESP-NOW probe have the loop.
            delay.delay_millis(window_ms as u32);
            return;
        };

        // ---- one state-machine step (may overrun the window; association does)
        match self.state {
            Link::NoCredentials | Link::EspNowOnly => {}

            Link::Backoff => {
                if HalInstant::now() >= live.next_attempt && live.associate() {
                    self.state = Link::Dhcp;
                }
            }

            // Disconnect is normal, not exceptional — an AP reboot or a roam drops
            // the association with no warning. Notice it here rather than letting
            // DHCP time out into something more confusing.
            Link::Dhcp | Link::Up => {
                if !live.controller.is_connected() {
                    println!("[net] ⚠️ association lost ({}) — reassociating", self.state.label());
                    live.fall_back();
                    self.state = Link::Backoff;
                }
            }
        }

        // ---- spend the REST of the window servicing the stack ----------------
        // Unconditional: smoltcp must be polled in every state that owns a device,
        // both to complete DHCP and — the part that matters here — to drain
        // received frames so their driver buffers are released.
        while HalInstant::now() < deadline {
            live.iface.poll(now(), &mut live.device, &mut live.sockets);

            if self.state == Link::Dhcp && live.take_lease() {
                self.state = Link::Up;
            }

            // M4 rides the lease. Driven INSIDE the service loop rather than
            // once per heartbeat so its own bounded waits get a polled stack
            // underneath them — a CONNACK cannot arrive through a stack nobody
            // is turning.
            if self.state == Link::Up {
                let Live {
                    iface,
                    device,
                    sockets,
                    mqtt,
                    ..
                } = live;
                mqtt.tick(iface, device, sockets, delay);
            }

            // A short step rather than a hot spin: the radio task needs the core
            // to actually receive the frames we are here to drain.
            delay.delay_millis(STACK_POLL_STEP_MS as u32);
        }
    }
}

impl Live {
    /// One association attempt. Returns true if associated.
    ///
    /// ===========================================================================
    /// WHY THIS ONE IS `block_on` AND THE ESP-NOW SEND IS NOT
    /// ===========================================================================
    ///
    /// `espnow_probe::send_bounded` goes to real trouble to put a 30 ms deadline
    /// on a send, so it would be reasonable to ask why association is allowed to
    /// block. **They are different hazards, and the difference is the reason.**
    ///
    /// * `SendWaiter`'s wait/drop is `while !FLAG.load() {}` — a bare, NON-YIELDING
    ///   spin on a private atomic. It burns the core, nothing else can run, and a
    ///   lost completion pins the CPU forever. That must be bounded.
    /// * `connect_async` awaits an esp-rtos event channel. The scheduler PREEMPTS
    ///   the `block_on` busy-loop to run the radio task that posts the completion,
    ///   so a slow AP costs latency, not a wedged core, and the association
    ///   genuinely cannot complete without waiting for it.
    ///
    /// It is also not obviously safe to cancel: `connect_async` calls
    /// `connect_impl()` FIRST and then awaits the event, so abandoning the future
    /// leaves a connect in flight while a later attempt issues another one. That
    /// re-entrancy is unverified, so it is not being invented in a spike.
    ///
    /// The bound that does exist is between attempts, not inside one:
    /// [`BACKOFF_MIN_MS`] -> [`BACKOFF_MAX_MS`], reset on success.
    fn associate(&mut self) -> bool {
        match block_on(self.controller.connect_async()) {
            Ok(info) => {
                println!("[net] associated: {:?}", info);
                self.backoff_ms = BACKOFF_MIN_MS;
                true
            }
            Err(e) => {
                println!(
                    "[net] associate failed: {:?} — retry in {} ms",
                    e, self.backoff_ms
                );
                self.arm_backoff();
                false
            }
        }
    }

    /// Drop back to the backoff state after losing the link.
    fn fall_back(&mut self) {
        // A fresh lease is required after reassociation; drop the old address so
        // a stale IP cannot outlive the association that earned it.
        self.iface.update_ip_addrs(|addrs| addrs.clear());
        self.arm_backoff();
    }

    fn arm_backoff(&mut self) {
        self.next_attempt = HalInstant::now() + HalDuration::from_millis(self.backoff_ms);
        self.backoff_ms = (self.backoff_ms * 2).min(BACKOFF_MAX_MS);
    }

    /// Check the DHCP socket ONCE and adopt a lease if one has landed.
    ///
    /// Single-shot by design: [`Net::tick`] owns the polling loop now, so this
    /// must not contain one of its own. (It used to slice its own 50 ms, which is
    /// the duty-cycle bug described at `STACK_POLL_STEP_MS`.)
    ///
    /// ===========================================================================
    /// 📌 M4 IMPLEMENTER: THE BROKER LEG IS DECIDED BY THE LEASE THIS PRINTS
    /// ===========================================================================
    ///
    /// HA's Mosquitto runs on a **quad-homed** VM and binds `0.0.0.0`, so every
    /// leg is THE SAME BROKER — retention and topics are shared. But the legs are
    /// not interchangeable from a client's point of view:
    ///
    /// > **Target the leg on the client's OWN subnet.** A cross-VLAN leg lets the
    /// > TCP connect succeed and then **silently drops the CONNACK** (asymmetric
    /// > return path, reproduced). The failure looks like a hung broker, not a
    /// > routing problem, which is why it costs an afternoon.
    ///
    /// | if this lease is on | use |
    /// |---|---|
    /// | VLAN8  `10.0.8.x`  | **`10.0.8.111:1883`** ← this board, joining `jplovescl` |
    /// | VLAN11 `10.0.11.x` | `10.0.11.110:1883` |
    /// | VLAN6  `10.0.6.x`  | `10.0.6.108:1883` (also the katana-side test leg) |
    ///
    /// ⚠️ **`smol/ha/README.md`'s broker table marks `10.0.8.111` "❌ never" — do
    /// not read that verdict and pick a different leg.** The full row qualifies it
    /// as never *cross-VLAN*, and the README's own follow-up blockquote says the
    /// `10.0.8.111` guidance "holds **only for VLAN8 devices**". This board IS a
    /// VLAN8 device. The verdict column is written from katana's VLAN6 vantage
    /// point and inverts for a board that lives on VLAN8.
    ///
    /// ⚠️ Second staleness in the same table: it says "the smol boards are on
    /// VLAN11 → they use `10.0.11.110`". That was verified 2026-07-08, BEFORE the
    /// fleet moved to the FT-off `jplovescl` SSID. Which VLAN a board lands on
    /// follows from the SSID it joins, so do not inherit VLAN11 from that line.
    ///
    /// **Ground truth beats both tables: read the lease this function prints.**
    /// `10.0.8.x` → `10.0.8.111`. (`10.0.8.111` was glass-verified from the C5's
    /// M4, from a board in exactly this position.)
    ///
    fn take_lease(&mut self) -> bool {
        match self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            Some(dhcpv4::Event::Configured(cfg)) => {
                self.iface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    let _ = addrs.push(IpCidr::Ipv4(cfg.address));
                });

                let mut line = heapless_line();
                let _ = write!(line, "[net] DHCP lease {}", cfg.address);
                if let Some(gw) = cfg.router {
                    let _ = self.iface.routes_mut().add_default_ipv4_route(gw);
                    let _ = write!(line, " gw {}", gw);
                } else {
                    let _ = write!(line, " (no router option)");
                }
                println!("{}", line.as_str());
                true
            }
            Some(dhcpv4::Event::Deconfigured) => {
                self.iface.update_ip_addrs(|addrs| addrs.clear());
                false
            }
            None => false,
        }
    }
}

// ------------------------------------------------------------------ fmt ------

/// A tiny fixed formatter, so the lease line is assembled once and printed once
/// rather than dribbled out in fragments that interleave with the heartbeat.
/// Deliberately not `heapless` the crate — this spike has no such dependency and
/// one 96-byte buffer does not justify adding one.
struct Line {
    buf: [u8; 96],
    len: usize,
}

fn heapless_line() -> Line {
    Line {
        buf: [0; 96],
        len: 0,
    }
}

impl Line {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("[net] <unprintable>")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                break; // truncate rather than fail; this is a log line
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}
