//! M3 radio probe — **feature-gated, off by default** (`--features radio`).
//!
//! ===========================================================================
//! WHAT THIS IS FOR: ANSWERING ONE QUESTION, NOT SHIPPING A CAPABILITY
//! ===========================================================================
//!
//! **THE UNKNOWN:** does esp-radio 0.18's `wifi` + `esp-now` pair COMPILE AND
//! LINK on `esp32s3`?
//!
//! It is genuinely open. smol main proves that pair on **esp32c3** (its `espnow`
//! feature is `["wifi", "esp-radio/esp-now"]`); emberburrito's burrito-fw proves
//! `wifi` alone on **esp32s3**. Nothing in either tree proves the INTERSECTION,
//! and "both halves work separately" is not evidence about the pair — that is the
//! reasoning shape that keeps costing this fleet afternoons.
//!
//! So the FIRST deliverable of this module is a `cargo check --release --features
//! radio` verdict, before a single byte goes on the air. Until that is green,
//! treat S3 ESP-NOW as **unproven**, not as inherited from the C3.
//!
//! **NOT A HAZARD HERE (stated so nobody re-derives it):** the `ieee802154`
//! feature-panic hazard that governs the C5/C6 work **does not apply to this
//! target**. The ESP32-S3 has **no 802.15.4 radio at all** — that is C5/C6
//! silicon — so esp-radio's `ieee802154` feature is not selectable for `esp32s3`
//! and cannot arrive transitively. The S3's radio hazard is a different one:
//! WiFi and BLE contend for one antenna, which is what `coex` arbitrates.
//!
//! **We deliberately do NOT enable `coex`.** esp-radio's build script HARD-ERRORS
//! if `coex` is set without `ble`, and smol's WiFi<->ESP-NOW coexistence is
//! SAME-radio channel management (both live in the WiFi domain), not cross-radio
//! arbitration. ESP-NOW *is* WiFi. So: esp-now only.

use core::{future::Future, task::Poll};

use esp_hal::{
    interrupt::software::SoftwareInterruptControl, peripherals, time::Instant,
    timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::{
    esp_now::{EspNowManager, EspNowReceiver, EspNowSender, BROADCAST_ADDRESS},
    wifi::{ControllerConfig, WifiController},
};

use crate::NODE_ID;

// ---------------------------------------------------------------- wire ------

/// The M3 hello frame. **Exactly 16 bytes on the wire**, node id baked in.
///
/// Written as a literal rather than formatted at runtime so the length is a
/// compile-time fact and the frame cannot drift when someone "tidies" the id
/// into a `write!`. (A `write!` would also link `core::fmt`, which this spike
/// does not otherwise need.)
const HELLO: &[u8; 16] = b"SMOLv1 HELLO 162";

/// The ACK we are listening for — **the 14-byte PREFIX only**.
///
/// ⚠️ The ACK **on the air is 23 bytes**: this 14-byte prefix followed by the
/// 9-byte #190 trailer. Matching on the whole received frame therefore never
/// fires, and it fails SILENTLY — you see sends leaving and no acks arriving, and
/// conclude the radio is deaf when it is working perfectly. **Match the prefix.**
const ACK_PREFIX: &[u8; 14] = b"SMOLv1 ACK 162";

/// The full on-air ACK length, recorded so the prefix rule above has a number
/// next to it. 14 (prefix) + 9 (#190 trailer) = 23.
const ACK_ON_AIR_LEN: usize = ACK_PREFIX.len() + 9;

// The frame sizes are contractual. If someone edits a literal above, fail HERE at
// compile time rather than on the air, where the symptom is silence.
const _: () = assert!(HELLO.len() == 16, "the hello frame must be exactly 16 bytes");
const _: () = assert!(
    ACK_PREFIX.len() == 14,
    "the ack PREFIX must be exactly 14 bytes; the on-air frame is prefix + 9-byte #190 trailer"
);
const _: () = assert!(ACK_ON_AIR_LEN == 23, "on-air ack length drifted");

/// Broadcast every N heartbeat ticks. `main`'s loop runs at ~1 Hz, so 2 ≈ 2 s.
const SEND_EVERY_TICKS: u32 = 2;

// ---------------------------------------------------------------- probe -----

/// Owns the radio for the life of the program. The `WifiController` is held (not
/// dropped) on purpose: dropping it deinitialises the WiFi driver and stops the
/// radio, which would take ESP-NOW down with it.
pub struct RadioProbe<'d> {
    _controller: WifiController<'d>,
    _manager: EspNowManager<'d>,
    sender: EspNowSender<'d>,
    receiver: EspNowReceiver<'d>,
}

/// Bring up the scheduler, the radio, and ESP-NOW.
///
/// Order is load-bearing and each step is asserted by the layer above it:
/// heap -> scheduler -> radio. esp-radio panics with
/// *"The scheduler must be initialized before initializing the radio"* if
/// `esp_rtos::start` has not run, so getting this wrong is loud rather than
/// subtle — which is the one mercy in this path.
pub fn init(
    timg0: peripherals::TIMG0<'static>,
    sw_interrupt: peripherals::SW_INTERRUPT<'static>,
    wifi: peripherals::WIFI<'static>,
) -> RadioProbe<'static> {
    // ---- heap --------------------------------------------------------------
    // The heap exists for esp-radio, which allocates its driver state.
    //
    // ⛔ THIS HEAP IS INTERNAL RAM ON PURPOSE. DO NOT MOVE IT TO PSRAM.
    //
    // The obvious objection is right there in the boot log: this board has 8 MiB
    // of octal PSRAM mapped, and this spends 64 KiB of scarce internal DRAM —
    // DRAM that comes out of the same pool as `.bss`, and therefore out of the
    // stack gap. `esp_alloc::psram_allocator!` exists and appears to fix that for
    // free.
    //
    // **It would be a silent-corruption bug on this chip.** From esp-alloc's own
    // docs: on ESP32, ESP32-S2 and **ESP32-S3** the atomic instructions DO NOT
    // WORK CORRECTLY when the memory they access is in PSRAM, so the allocator
    // must not be used to allocate `Atomic*` types — *either directly or
    // indirectly*. We are an S3, and the sole consumer of this heap is the WiFi
    // driver, whose internal state we neither control nor audit and which
    // certainly contains synchronisation primitives.
    //
    // "Indirectly" is the word that matters. Nothing would fail at the allocation
    // site. Atomics would simply stop being atomic, somewhere inside the radio,
    // intermittently. The 64 KiB is a real cost, accepted knowingly. If the stack
    // gap ever needs reclaiming, shrink this number or a static — never relocate
    // this heap.
    esp_alloc::heap_allocator!(size: 64 * 1024);

    // ---- scheduler ---------------------------------------------------------
    // MUST precede the radio init. In the pre-0.18 stack this scheduler lived
    // inside esp-wifi as `builtin-scheduler`; #233 moved it out to esp-rtos,
    // which is why esp-radio cannot be used without it.
    //
    // `esp_rtos::start()` turns main() into the scheduler's pinned main task, so
    // the blocking superloop in main.rs keeps running as straight-line code — no
    // executor, no async. (This is where the spike follows smol rather than
    // burrito-fw, which is an embassy application.)
    let timg0 = TimerGroup::new(timg0);
    let sw = SoftwareInterruptControl::new(sw_interrupt);
    esp_rtos::start(timg0.timer0, sw.software_interrupt0);
    println!("[radio] esp-rtos scheduler started");

    // ---- radio + esp-now ---------------------------------------------------
    // `wifi::new` is what actually initialises the radio in 0.18 — there is no
    // public `esp_radio::init()` any more. ESP-NOW arrives as a field on the
    // returned `Interfaces`, gated on the `esp-now` + `unstable` features. That
    // field's very existence is half of what this probe is testing.
    let (controller, interfaces) =
        esp_radio::wifi::new(wifi, ControllerConfig::default()).expect("wifi/esp-now init");
    println!("[radio] wifi controller up");

    let (manager, sender, receiver) = interfaces.esp_now.split();
    println!(
        "[radio] ESP-NOW ready — broadcasting {} bytes every ~{} s, node {}",
        HELLO.len(),
        SEND_EVERY_TICKS,
        NODE_ID
    );
    println!(
        "[radio] listening for the {}-byte ACK PREFIX (on-air frame is {} bytes)",
        ACK_PREFIX.len(),
        ACK_ON_AIR_LEN
    );

    RadioProbe {
        _controller: controller,
        _manager: manager,
        sender,
        receiver,
    }
}

impl RadioProbe<'_> {
    /// Called once per heartbeat tick from `main`'s superloop.
    ///
    /// Non-blocking by construction: `receive()` returns `Option`, and TX goes
    /// through `send_bounded` (a deadline-polled `SendFuture` — NOT a `SendWaiter`,
    /// whose wait AND drop both spin unboundedly; see the block comment on
    /// `send_bounded`). A probe that blocks the heartbeat would make a wedged
    /// radio look like a wedged board.
    pub fn tick(&mut self, tick: u32) {
        // --- RX: drain whatever arrived since the last tick -----------------
        while let Some(frame) = self.receiver.receive() {
            let data = frame.data();

            // ⚠️ `starts_with`, NEVER `==` — see ACK_PREFIX. The on-air ACK
            // carries a 9-byte #190 trailer after these 14 bytes, so an equality
            // test is ALWAYS false and would report a healthy link as silent.
            // `starts_with` is used rather than a hand-rolled slice compare
            // because it says "prefix" in the code, and because it folds in the
            // length check that a hand-rolled version has to remember.
            if data.starts_with(&ACK_PREFIX[..]) {
                println!(
                    "[radio] ✅ ACK matched — {} bytes on air ({} prefix + {} trailer)",
                    data.len(),
                    ACK_PREFIX.len(),
                    data.len() - ACK_PREFIX.len()
                );
            } else {
                println!("[radio] rx {} bytes (not our ack)", data.len());
            }
        }

        // --- TX: broadcast the hello ----------------------------------------
        if !tick.is_multiple_of(SEND_EVERY_TICKS) {
            return;
        }
        match send_bounded(&mut self.sender, &BROADCAST_ADDRESS, HELLO) {
            TxOutcome::Done => println!("[radio] tx hello ({} B) -> broadcast", HELLO.len()),
            TxOutcome::Failed(e) => println!("[radio] tx FAILED: {:?}", e),
            // Log and carry on. An abandoned frame is a dropped packet on a
            // best-effort broadcast; a hung superloop is a dead board.
            TxOutcome::TimedOut => println!(
                "[radio] ⚠️ tx deadline ({} ms) — frame abandoned, radio may be wedged",
                TX_WAIT_MS
            ),
        }
    }
}

// ------------------------------------------------------------ bounded tx ----

/// How long one ESP-NOW frame gets to complete before we abandon it.
///
/// A broadcast normally completes in single-digit milliseconds. This only has to
/// be generous enough not to abandon healthy frames and short enough that a stuck
/// radio cannot stall the heartbeat. 30 ms is the esp32c6-watch's proven value.
const TX_WAIT_MS: u64 = 30;

pub enum TxOutcome {
    Done,
    Failed(esp_radio::esp_now::EspNowError),
    /// The deadline expired. The frame is abandoned; the board keeps running.
    TimedOut,
}

/// Send one ESP-NOW frame **with a hard deadline**.
///
/// ===========================================================================
/// ⛔⛔ NEVER REPLACE THIS WITH `sender.send(..)` AND A `wait()` OR A DROP.
/// ===========================================================================
///
/// **THE WAR STORY (esp32c6-watch, a full day lost).** esp-radio 0.18's
/// `EspNowSender::send()` returns a `SendWaiter`, and BOTH of its exits are an
/// **unbounded, non-yielding spin on an atomic**. Verified in
/// `esp-radio-0.18.0/src/esp_now/mod.rs`:
///
/// ```text
///     impl SendWaiter<'_> {
///         pub fn wait(self) -> Result<(), EspNowError> {
///             core::mem::forget(self);
///             while !ESP_NOW_SEND_CB_INVOKED.load(Ordering::Acquire) {}   // :590
///             ...
///     impl Drop for SendWaiter<'_> {
///         fn drop(&mut self) {
///             while !ESP_NOW_SEND_CB_INVOKED.load(Ordering::Acquire) {}   // :604
/// ```
///
/// **ONE LOST TX COMPLETION PINS THE CPU FOREVER.** There is no timeout, no
/// yield, and no way to observe the flag from outside the crate — it is a private
/// static, so a bounded poll on `SendWaiter` is not merely awkward, it is
/// impossible.
///
/// The trap is that `wait()` is the obvious call AND SO IS NOT CALLING IT.
/// Binding the waiter to `_waiter` and letting it fall out of scope *looks* like
/// fire-and-forget and is the SAME SPIN, just spelled invisibly — an earlier
/// revision of this file did exactly that, with a confident comment explaining
/// why dropping it was cheap. It is not cheap. It is the identical `while` loop.
///
/// **THE WAY OUT** is `send_async`, whose `SendFuture` has **no `Drop` impl at
/// all** (checked, same file, `:951`) — so abandoning it is genuinely free. We
/// poll that future against a deadline and drop it if the deadline passes.
///
/// The esp32c6-watch does this with `select(send_async(..), Timer::after(..))`.
/// This spike has **no async executor** (esp-rtos without the `embassy` feature —
/// it is a blocking superloop like smol main), so the equivalent is written out
/// by hand: a bounded poll loop with a `noop` waker and an `Instant` deadline.
/// Same guarantee, no executor.
///
/// If you are here to "simplify" this back to `send()`: the thing you are
/// deleting is the only reason a wedged radio does not become a wedged board.
fn send_bounded(
    sender: &mut EspNowSender<'_>,
    addr: &[u8; 6],
    data: &[u8],
) -> TxOutcome {
    // `SendFuture` is inert until polled and carries no Drop — abandoning it on
    // the timeout path costs nothing and runs no spin.
    let fut = sender.send_async(addr, data);
    let mut fut = core::pin::pin!(fut);

    // No executor, so nothing will ever wake us — we are polling, not sleeping,
    // and a noop waker is exactly right. `Waker::noop()` is stable on the esp
    // toolchain (Rust 1.95).
    let waker = core::task::Waker::noop();
    let mut cx = core::task::Context::from_waker(waker);

    let started = Instant::now();
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(Ok(())) => return TxOutcome::Done,
            Poll::Ready(Err(e)) => return TxOutcome::Failed(e),
            Poll::Pending => {
                if started.elapsed().as_millis() >= TX_WAIT_MS {
                    // Drop `fut` by returning. No Drop impl == no spin == the
                    // whole point of this function.
                    return TxOutcome::TimedOut;
                }
            }
        }
    }
}
