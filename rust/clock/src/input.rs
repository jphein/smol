//! BOOT button on GPIO9 — debounced, non-blocking short-tap / long-press input.
//!
//! ## Hardware
//!
//! The ESP32-C3 SuperMini's onboard **BOOT** button is wired to **GPIO9** and is
//! **ACTIVE-LOW**: pressed pulls the pin to GND (reads LOW), released floats HIGH
//! via the chip's internal pull-up. GPIO9 is also the C3's *boot strapping* pin
//! (held low at reset -> download mode), but once the firmware is running it is
//! free to use as a normal input, which is exactly what we do here.
//!
//! We configure it as an [`Input`] with the internal [`Pull::Up`] so no external
//! resistor is needed, then read logical "pressed" as `is_low()`.
//!
//! ## Debounce + gesture detection (non-blocking, time-driven)
//!
//! The render loop calls [`Button::poll`] every sub-tick with the current
//! monotonic-millisecond time. `poll` runs a tiny state machine that:
//!
//!   * **debounces** the raw level (a candidate edge must be stable for
//!     [`DEBOUNCE_MS`] before it counts), and
//!   * classifies a completed press as either a **short tap** or a **long
//!     press** using the [`LONG_PRESS_MS`] threshold.
//!
//! A **long press** is reported *as soon as* the hold crosses the threshold
//! (while the button is still held) so "enter / back" feels immediate; the
//! subsequent release is then swallowed so it does not also fire a short tap. A
//! **short tap** is reported on *release* (only if the press never reached the
//! long threshold). This mirrors how phone/console UIs treat tap-vs-hold and,
//! importantly, needs no blocking delay — everything is derived from the
//! timestamps handed in by the caller, so the OLED/LED keep updating.

use esp_hal::gpio::{Input, InputConfig, Pull};

/// A raw level must be stable this long (ms) before we accept it as a real
/// edge. 25 ms comfortably rejects the few-ms mechanical bounce of a tact switch
/// without adding perceptible latency.
const DEBOUNCE_MS: u64 = 25;

/// Press duration (ms) at/above which a press is a **long** press rather than a
/// short tap. ~700 ms per the spec: long enough that a normal "click" never
/// trips it, short enough that "hold to enter/back" doesn't feel sticky.
const LONG_PRESS_MS: u64 = 700;

/// The gesture a completed (or crossing-threshold) button interaction produced.
/// Returned by [`Button::poll`]; `None` most ticks (nothing happened).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// A quick press-and-release (held < [`LONG_PRESS_MS`]). Reported on release.
    Short,
    /// The button has been held for [`LONG_PRESS_MS`]. Reported *once*, the
    /// instant the threshold is crossed, while still held.
    Long,
}

/// Internal debounce/gesture phase.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Button released and stable.
    Idle,
    /// Saw a press edge; waiting for it to be stable for `DEBOUNCE_MS`.
    DebouncingPress { since_ms: u64 },
    /// Debounced press confirmed; timing the hold. `fired_long` guards the
    /// one-shot long-press report so we don't re-fire every tick past 700 ms.
    Held { since_ms: u64, fired_long: bool },
}

/// Debounced BOOT button with short-tap / long-press classification.
pub struct Button {
    pin: Input<'static>,
    phase: Phase,
}

impl Button {
    /// Wrap GPIO9 as a pulled-up active-low input. `main` owns `esp_hal::init()`
    /// and the pin singleton and passes it in, so the HAL is initialised once.
    pub fn new(pin: esp_hal::peripherals::GPIO9<'static>) -> Self {
        let input = Input::new(pin, InputConfig::default().with_pull(Pull::Up));
        Self {
            pin: input,
            phase: Phase::Idle,
        }
    }

    /// Raw logical "is the button pressed right now" (active-low -> `is_low`).
    #[inline]
    fn is_pressed(&self) -> bool {
        self.pin.is_low()
    }

    /// Advance the debounce/gesture state machine at monotonic time `now_ms`.
    ///
    /// Call every sub-tick. Returns `Some(Press::Long)` the instant a hold
    /// crosses [`LONG_PRESS_MS`] (button still down), `Some(Press::Short)` when a
    /// sub-threshold press is released, and `None` otherwise. Pure function of
    /// the pin level + the timestamps, so it never blocks.
    pub fn poll(&mut self, now_ms: u64) -> Option<Press> {
        let pressed = self.is_pressed();
        match self.phase {
            Phase::Idle => {
                if pressed {
                    // Candidate press edge; start debouncing it.
                    self.phase = Phase::DebouncingPress { since_ms: now_ms };
                }
                None
            }
            Phase::DebouncingPress { since_ms } => {
                if !pressed {
                    // Bounced back up before settling — spurious, ignore.
                    self.phase = Phase::Idle;
                    None
                } else if now_ms.saturating_sub(since_ms) >= DEBOUNCE_MS {
                    // Stable press -> start timing the hold from the ORIGINAL
                    // edge so the long-press threshold measures true hold time.
                    self.phase = Phase::Held {
                        since_ms,
                        fired_long: false,
                    };
                    None
                } else {
                    None
                }
            }
            Phase::Held { since_ms, fired_long } => {
                if !pressed {
                    // Released. If we never crossed the long threshold, this was
                    // a short tap; report it. (If we did, the Long was already
                    // reported and we swallow this release.)
                    self.phase = Phase::Idle;
                    if fired_long {
                        None
                    } else {
                        Some(Press::Short)
                    }
                } else if !fired_long && now_ms.saturating_sub(since_ms) >= LONG_PRESS_MS {
                    // Crossed the long-press threshold while still held: fire Long
                    // once, then latch so we don't repeat or also fire Short.
                    self.phase = Phase::Held {
                        since_ms,
                        fired_long: true,
                    };
                    Some(Press::Long)
                } else {
                    None
                }
            }
        }
    }
}
