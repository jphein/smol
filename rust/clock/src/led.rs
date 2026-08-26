//! Blue status LED on GPIO8 — non-blocking, time-driven state machine.
//!
//! ## Hardware
//!
//! The ESP32-C3 SuperMini has a blue LED wired to **GPIO8**. On this board the
//! LED is **ACTIVE-LOW**: driving the pin LOW sinks current and lights it,
//! driving it HIGH turns it off. That polarity is captured once in
//! [`LED_ACTIVE_LOW`] so the rest of the code can think in logical on/off.
//!
//! GPIO8 is also a **boot strapping pin** on the C3 (it must read a valid level
//! at reset to select the boot mode). We only ever drive it *after* boot for the
//! LED, and we hand it out as a fresh `Output` initialised to the logical-OFF
//! level, so we never hold it low through a reset. Do NOT wire anything that
//! forces GPIO8 low during power-up.
//!
//! ## The four states (deliberately distinct blink rates so they're tellable
//! apart by eye):
//!
//! | State          | Meaning                                       | LED behaviour     |
//! |----------------|-----------------------------------------------|-------------------|
//! | `WifiSync`     | WiFi associating / NTP sync in progress       | FAST blink ~10 Hz |
//! | `Idle`         | No ESP-NOW peer heard (beacon stale > ~3 s)   | OFF               |
//! | `PeerDetected` | Heard a peer HELLO, two-way link NOT confirmed| SLOW blink ~2 Hz  |
//! | `Connected`    | Two-way handshake confirmed (we heard peer +  | SOLID on          |
//! |                | have an ACK proving the peer heard us)        |                   |
//!
//! Blinking is computed from a monotonic millisecond clock rather than a
//! blocking sleep, so the main render loop stays responsive: on every fast
//! sub-tick we ask [`LedState::level_at`] whether the LED should be lit *right
//! now* for the current time, and push that to the pin. No timers, no ISRs.

use esp_hal::gpio::Level;
#[cfg(not(feature = "esp32s3"))]
use esp_hal::gpio::Output;

/// LED polarity for GPIO8 on the ESP32-C3 SuperMini.
///
/// `true`  => ACTIVE-LOW: physical LOW = lit (this board).
/// `false` => ACTIVE-HIGH: physical HIGH = lit.
///
/// Everything else in this module works in *logical* on/off; this const is the
/// single place that maps logical->physical, so re-targeting a board with the
/// opposite wiring is a one-line change.
#[cfg(not(feature = "esp32s3"))]
pub const LED_ACTIVE_LOW: bool = true;

// --- Blink half-periods (milliseconds) -----------------------------------
// A "half-period" is how long the LED stays on, then off, per blink. The blink
// frequency is therefore 1000 / (2 * half_period_ms). We pick rates that are
// obviously different from each other by eye:
//
//   ~10 Hz  -> half period  50 ms   (WiFi/NTP sync — a rapid flutter)
//   ~2  Hz  -> half period 250 ms   (peer detected — a lazy blink)
//
// 10 Hz vs 2 Hz is a 5x separation, so nobody has to count flashes to tell
// "still connecting" from "found a peer".
const FAST_BLINK_HALF_MS: u64 = 50; // ~10 Hz  (WifiSync)
const SLOW_BLINK_HALF_MS: u64 = 250; // ~2 Hz  (PeerDetected)

/// What the status LED should currently indicate. Ordered loosely by "progress"
/// but the value is chosen each tick by the caller from live radio state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedState {
    /// WiFi associating / NTP sync in progress. FAST blink (~10 Hz).
    WifiSync,
    /// No ESP-NOW peer heard recently. OFF.
    Idle,
    /// Heard a peer HELLO beacon but two-way link not yet confirmed.
    /// SLOW blink (~2 Hz).
    PeerDetected,
    /// Bidirectional handshake confirmed. SOLID on.
    Connected,
}

impl LedState {
    /// Whether the LED should be *logically* lit at monotonic time `now_ms`.
    ///
    /// For the blinking states this derives the on/off phase purely from the
    /// timestamp (a square wave), so calling it at any sampling rate produces a
    /// steady blink without keeping any per-blink state.
    pub fn is_lit(self, now_ms: u64) -> bool {
        match self {
            LedState::Idle => false,
            LedState::Connected => true,
            LedState::WifiSync => blink_phase(now_ms, FAST_BLINK_HALF_MS),
            LedState::PeerDetected => blink_phase(now_ms, SLOW_BLINK_HALF_MS),
        }
    }
}

/// #48 dashboard LED control — the per-node MODE that gates the auto-driven [`LedState`]
/// indicator. `Status` (default) keeps the ESP-NOW peer-state machine (backward-compatible);
/// `On`/`Off` force the LED solid/dark regardless of link state. Relayed to leaves over the
/// keyed CFG channel (key `L`); the gateway reads its own `smol/<id>/config/led`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LedMode {
    /// Auto-drive from the peer-state machine — the pre-#48 behavior. Default.
    #[default]
    Status,
    /// Force the LED solid ON, ignoring link state.
    On,
    /// Force the LED dark, ignoring link state.
    Off,
}

impl LedMode {
    /// Parse a `smol/<id>/config/led` payload (`status`/`on`/`off`, case-sensitive to match
    /// the HA input_select options). Unknown/garbage → `None` (caller keeps the current mode;
    /// this is an untrusted retained/relayed value, so never panic — #46 clamp discipline).
    pub fn from_wire(s: &str) -> Option<LedMode> {
        match s.trim() {
            "status" => Some(LedMode::Status),
            "on" => Some(LedMode::On),
            "off" => Some(LedMode::Off),
            _ => None,
        }
    }

    /// #74: the wire token for the LED-mode readback (`led=<mode>:<state>` in the DIAG record).
    /// Inverse of [`from_wire`]; matches luna's HA input_select options exactly.
    ///
    /// [`from_wire`]: LedMode::from_wire
    pub fn to_wire(self) -> &'static str {
        match self {
            LedMode::Status => "status",
            LedMode::On => "on",
            LedMode::Off => "off",
        }
    }
}

/// Square-wave phase: true for the first half-period, false for the second.
#[inline]
fn blink_phase(now_ms: u64, half_ms: u64) -> bool {
    // (now / half) parity: even half = on, odd half = off.
    (now_ms / half_ms).is_multiple_of(2)
}

/// Thin wrapper over the status-light hardware so the caller only ever
/// expresses *logical* on/off. On the C3 boards that is the GPIO8 `Output`
/// with [`LED_ACTIVE_LOW`] applied; on the S3/ES3C28P it is the WS2812 pixel
/// on GPIO42, driven over RMT (a plain level is invisible to a WS2812 — the
/// pre-RMT arm proved that on glass). Same public contract either way:
/// construct once, then call [`Led::apply`]/[`Led::apply_mode`] every sub-tick.
pub struct Led {
    #[cfg(not(feature = "esp32s3"))]
    pin: Output<'static>,
    /// The RMT TX channel, absent while a frame's `transmit()` owns it and
    /// `None` for good if RMT setup failed at boot (the LED then stays dark
    /// but the peer-state machine is unaffected — a status light must never
    /// be able to brick the node).
    #[cfg(feature = "esp32s3")]
    channel: Option<esp_hal::rmt::Channel<'static, esp_hal::Blocking, esp_hal::rmt::Tx>>,
    /// Last pixel state actually transmitted. WS2812s LATCH: the pixel holds
    /// its color until told otherwise, so a frame goes on the wire only when
    /// the logical state CHANGES — not 50×/second from the sub-tick loop.
    #[cfg(feature = "esp32s3")]
    last_lit: Option<bool>,
}

// --- WS2812 frame encoding (S3/ES3C28P, GPIO42) ---------------------------
// RMT at 80 MHz, divider 1 => 12.5 ns/tick. WS2812B datasheet timings:
//   0-bit: 0.40 µs high + 0.85 µs low  ->  32 + 68 ticks
//   1-bit: 0.80 µs high + 0.45 µs low  ->  64 + 36 ticks
//   reset: >= 50 µs low                ->  4000 ticks (split: a PulseCode
//          with a ZERO length field is an end marker, so the reset rides two
//          non-zero low halves, then the explicit end marker terminates).
// One pixel = 24 bits, G-R-B, MSB first.
#[cfg(feature = "esp32s3")]
const WS2812_FRAME_LEN: usize = 26; // 24 bit pulses + reset + end marker

/// The lit color, deliberately dim: this is a status light on a desk, not a
/// flashlight. GRB order. Green ~8%: distinct from the RGB apps' colors and
/// easy on dark-room eyes.
#[cfg(feature = "esp32s3")]
const WS2812_LIT_GRB: [u8; 3] = [0x14, 0x00, 0x00];

#[cfg(feature = "esp32s3")]
fn ws2812_frame(grb: [u8; 3]) -> [esp_hal::rmt::PulseCode; WS2812_FRAME_LEN] {
    use esp_hal::rmt::PulseCode;
    let bit0 = PulseCode::new(Level::High, 32, Level::Low, 68);
    let bit1 = PulseCode::new(Level::High, 64, Level::Low, 36);
    let mut frame = [PulseCode::end_marker(); WS2812_FRAME_LEN];
    let mut i = 0;
    for byte in grb {
        for bit in (0..8).rev() {
            frame[i] = if (byte >> bit) & 1 == 1 { bit1 } else { bit0 };
            i += 1;
        }
    }
    // >= 50 µs latch, both halves non-zero so it is not read as an end marker.
    frame[24] = PulseCode::new(Level::Low, 2000, Level::Low, 2000);
    // frame[25] stays the end marker.
    frame
}

impl Led {
    /// Wrap an already-created GPIO8 `Output`. `main` owns `esp_hal::init()` and
    /// the pin, and constructs the `Output` at the logical-OFF level (see
    /// [`Led::off_level`]) so GPIO8 is not held low through any reset here.
    #[cfg(not(feature = "esp32s3"))]
    pub fn new(pin: Output<'static>) -> Self {
        Self { pin }
    }

    /// #398 S3: wrap a configured RMT TX channel already bound to GPIO42.
    /// `None` (RMT setup failed) is a legal dark LED, not an error.
    #[cfg(feature = "esp32s3")]
    pub fn new_ws2812(
        channel: Option<esp_hal::rmt::Channel<'static, esp_hal::Blocking, esp_hal::rmt::Tx>>,
    ) -> Self {
        Self {
            channel,
            last_lit: None,
        }
    }

    /// The *physical* level that means logical-OFF, given the board polarity.
    /// Used by `main` when creating the `Output` so it powers up dark.
    #[cfg(not(feature = "esp32s3"))]
    pub const fn off_level() -> Level {
        if LED_ACTIVE_LOW {
            Level::High
        } else {
            Level::Low
        }
    }

    /// The physical level for a given logical "lit" flag.
    #[cfg(not(feature = "esp32s3"))]
    #[inline]
    fn level_for(lit: bool) -> Level {
        // XOR the request with the active-low flag: active-low inverts.
        match lit ^ LED_ACTIVE_LOW {
            true => Level::High,
            false => Level::Low,
        }
    }

    /// Push the logical state to the hardware. C3: a pin level, every call.
    #[cfg(not(feature = "esp32s3"))]
    fn set_lit(&mut self, lit: bool) {
        self.pin.set_level(Self::level_for(lit));
    }

    /// Push the logical state to the hardware. S3: one WS2812 frame over RMT,
    /// only on CHANGE (the pixel latches). The blocking `wait()` is ~80 µs of
    /// wire time on the rare change tick — invisible to the 20 ms sub-tick.
    /// Any transmit error hands the channel back and drops the frame: the
    /// next change retries, and a persistently broken RMT degrades to a dark
    /// LED rather than a panic.
    #[cfg(feature = "esp32s3")]
    fn set_lit(&mut self, lit: bool) {
        if self.last_lit == Some(lit) {
            return;
        }
        let Some(channel) = self.channel.take() else {
            return;
        };
        let grb = if lit { WS2812_LIT_GRB } else { [0, 0, 0] };
        let frame = ws2812_frame(grb);
        match channel.transmit(&frame) {
            Ok(txn) => match txn.wait() {
                Ok(ch) => {
                    self.channel = Some(ch);
                    self.last_lit = Some(lit);
                }
                Err((_, ch)) => self.channel = Some(ch),
            },
            Err((_, ch)) => self.channel = Some(ch),
        }
    }

    /// Drive the LED to reflect `state` at monotonic time `now_ms`. Cheap enough
    /// to call on every fast sub-tick; just recomputes the square-wave phase and
    /// pushes it (the S3 arm no-ops unless the phase actually changed).
    pub fn apply(&mut self, state: LedState, now_ms: u64) {
        let lit = state.is_lit(now_ms);
        self.set_lit(lit);
    }

    /// #48: drive the LED honoring the dashboard [`LedMode`]. `Status` defers to the auto
    /// `state` (identical to [`Led::apply`]); `On`/`Off` force lit/dark. Same cheap
    /// per-sub-tick contract as `apply`.
    pub fn apply_mode(&mut self, mode: LedMode, state: LedState, now_ms: u64) {
        let lit = match mode {
            LedMode::Status => state.is_lit(now_ms),
            LedMode::On => true,
            LedMode::Off => false,
        };
        self.set_lit(lit);
    }
}
