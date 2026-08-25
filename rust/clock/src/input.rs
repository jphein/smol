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
//! The render loop calls [`Button::poll`] every sub-tick (`main::SUBTICK_MS` = 20 ms) with the
//! current monotonic-millisecond time; [`Gesture`] is the state machine it drives, kept free of the
//! HAL so it is host-testable (`tests/input.rs`) rather than only verifiable by pressing a button.
//!
//! A **long press** is reported *as soon as* the hold crosses [`LONG_PRESS_MS`] (while the button is
//! still held) so "enter / back" feels immediate; the subsequent release is then swallowed so it does
//! not also fire a short tap. A **short tap** is reported on *release*. Everything is derived from
//! the timestamps handed in by the caller, so nothing blocks and the OLED/LED keep updating.
//!
//! ### The 20 ms sampler decides what "debounce" can mean (fixed 2026-07-27, JP at the bench)
//!
//! A tap under ~40 ms used to be **silently discarded**: the machine advanced one state per poll, so
//! a press needed a *second* poll (~40 ms later) to be confirmed, and a release seen before that was
//! written off as "bounced back up — spurious". Worse on the crown, whose sub-tick stretches during a
//! WiFi burst, so the floor moved around and the button felt unreliable rather than merely fast.
//!
//! The reasoning that fixes it: **a 25 ms software debounce cannot filter switch bounce that we
//! sample every 20 ms.** Tact-switch bounce lasts ~1-5 ms — it is over long before the next sample —
//! so a level that is still pressed 20 ms later was never in doubt, and a press seen on one poll and
//! gone by the next is far more likely a genuine fast tap than bounce. What the old 25 ms actually
//! filtered was fast taps. So:
//!
//!   * a press released *within* the settle window now reports [`Press::Short`] instead of being
//!     dropped — any tap the sampler catches at all is a tap;
//!   * [`DEBOUNCE_MS`] drops to 5 ms, i.e. the next poll always settles it, so a normal press reaches
//!     the hold-timing state one poll after the edge, so long-press timing is unaffected;
//!   * the real bounce protection becomes a **post-gesture lockout** ([`SETTLE_MS`]), which is the
//!     hazard that matches this sample rate: contact bounce on RELEASE, sampled as a fresh press,
//!     would otherwise report a phantom second tap. Nothing is accepted for 40 ms after a gesture
//!     completes — two sample periods of protection, while still allowing ~12 deliberate taps/s
//!     (human maximum is ~8).
//!
//! Honest residual: a tap so short that NO poll observes it is still invisible. Fixing that needs a
//! GPIO interrupt, not a smaller constant. And a single spurious low sample (EMI) now reports a tap
//! where two consecutive samples were once required — accepted deliberately, because the pin has a
//! ~45 kΩ internal pull-up and a phantom tap is a screen pausing, whereas the previous behaviour was
//! JP's presses going nowhere.

// #152: the physical BOOT-button HAL (GPIO9 debounce) rides `hw`; the host emulator
// (`hostsim`) synthesizes `Press` from the keyboard directly, so it needs only the
// `Press` contract below — not the GPIO plumbing.
#[cfg(feature = "hw")]
use esp_hal::gpio::{Input, InputConfig, Pull};

/// A press must be stable this long (ms) before the hold timer starts. 5 ms, not 25: at a 20 ms
/// sample rate anything larger only filters fast TAPS (see the module doc), and a release inside this
/// window is now reported as a tap rather than discarded. Kept as a real constant rather than folded
/// away so a faster poll loop (or an interrupt-driven one) still has a settle step to honour.
pub const DEBOUNCE_MS: u64 = 5;

/// Nothing is accepted for this long (ms) after a gesture completes — the bounce protection that
/// matches a 20 ms sampler: contact bounce on RELEASE, caught by a sample, would otherwise look like
/// a fresh press and report a phantom tap. 40 ms = two sample periods, and still ~12 taps/s.
pub const SETTLE_MS: u64 = 40;

/// Press duration (ms) at/above which a press is a **long** press rather than a
/// short tap. ~700 ms per the spec: long enough that a normal "click" never
/// trips it, short enough that "hold to enter/back" doesn't feel sticky.
pub const LONG_PRESS_MS: u64 = 700;

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

/// Debounce/gesture phase. Deliberately PRIVATE: the host tests drive [`Gesture::poll`] and assert
/// the gestures it reports, never the state it is in — a test that inspects the phase would pass a
/// refactor that broke the button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Button released and stable.
    Idle,
    /// Saw a press edge; waiting for it to settle for [`DEBOUNCE_MS`]. A release from HERE is a fast
    /// tap (it used to be discarded as bounce — see the module doc).
    DebouncingPress {
        /// When the press edge was first observed; the hold timer starts from this, not from the
        /// settle, so [`LONG_PRESS_MS`] measures true hold time.
        since_ms: u64,
    },
    /// Settled press; timing the hold. `fired_long` guards the one-shot long report so it neither
    /// repeats every poll past 700 ms nor also fires a tap on release.
    Held {
        /// The original press edge.
        since_ms: u64,
        /// Whether [`Press::Long`] has already been reported for this hold.
        fired_long: bool,
    },
    /// A gesture just completed: swallow the release bounce for [`SETTLE_MS`].
    Settling {
        /// When new input starts being accepted again.
        until_ms: u64,
    },
}

/// The pure gesture state machine: pin level and time in, gesture out.
///
/// Split from [`Button`] deliberately — this is the input every screen shares (menu, Snake,
/// page-turn, the Bard's pause), its rules are all timing, and timing is exactly what a bench press
/// cannot systematically verify. With no HAL in here, `tests/input.rs` drives synthetic timelines:
/// fast taps, stretched sub-ticks, bounce bursts, double taps.
#[derive(Clone, Copy)]
pub struct Gesture {
    phase: Phase,
}

impl Default for Gesture {
    fn default() -> Self {
        Self::new()
    }
}

impl Gesture {
    /// Idle, nothing pressed.
    pub const fn new() -> Self {
        Self {
            phase: Phase::Idle,
        }
    }

    /// Advance the machine with the debounced-by-sampling level at `now_ms`.
    ///
    /// Returns `Some(Press::Long)` the instant a hold crosses [`LONG_PRESS_MS`] (button still down),
    /// `Some(Press::Short)` when a sub-threshold press is released, and `None` otherwise. Exactly one
    /// gesture per press, always.
    pub fn poll(&mut self, pressed: bool, now_ms: u64) -> Option<Press> {
        match self.phase {
            Phase::Idle => {
                if pressed {
                    self.phase = Phase::DebouncingPress { since_ms: now_ms };
                }
                None
            }
            Phase::DebouncingPress { since_ms } => {
                if !pressed {
                    // THE FIX (JP, 2026-07-27): a press we saw and that is now gone is a TAP, not
                    // bounce. At a 20 ms sample rate this is the only way a fast tap can look, and
                    // discarding it is what made short presses fail to register.
                    self.settle(now_ms);
                    Some(Press::Short)
                } else if now_ms.saturating_sub(since_ms) >= DEBOUNCE_MS {
                    // Settled -> time the hold from the ORIGINAL edge.
                    self.phase = Phase::Held {
                        since_ms,
                        fired_long: false,
                    };
                    None
                } else {
                    None
                }
            }
            Phase::Held {
                since_ms,
                fired_long,
            } => {
                if !pressed {
                    let held = now_ms.saturating_sub(since_ms);
                    self.settle(now_ms);
                    if fired_long {
                        // Long already reported; swallow the release.
                        None
                    } else if held >= LONG_PRESS_MS {
                        // Crossed the threshold with no poll in between — the crown's sub-tick
                        // stretches during a WiFi burst (see main.rs's HARDWARE-WATCH note), so a
                        // 700 ms+ hold can be observed only on release. Classify by ELAPSED time,
                        // not by whether we happened to get a poll inside the window: reporting a
                        // deliberate hold as a tap sends the Bard to pause instead of to the menu.
                        Some(Press::Long)
                    } else {
                        Some(Press::Short)
                    }
                } else if !fired_long && now_ms.saturating_sub(since_ms) >= LONG_PRESS_MS {
                    // Crossed the long threshold while still held: fire once, then latch.
                    self.phase = Phase::Held {
                        since_ms,
                        fired_long: true,
                    };
                    Some(Press::Long)
                } else {
                    None
                }
            }
            Phase::Settling { until_ms } => {
                if now_ms >= until_ms {
                    // Leaving the lockout: a level that is STILL pressed is a new edge, not a
                    // continuation — without this a press arriving during the window would strand
                    // the machine here until the user let go, losing the press entirely.
                    self.phase = if pressed {
                        Phase::DebouncingPress { since_ms: now_ms }
                    } else {
                        Phase::Idle
                    };
                }
                None
            }
        }
    }

    /// Enter the post-gesture lockout.
    fn settle(&mut self, now_ms: u64) {
        self.phase = Phase::Settling {
            until_ms: now_ms.saturating_add(SETTLE_MS),
        };
    }
}

/// Debounced BOOT button with short-tap / long-press classification./// Debounced BOOT button with short-tap / long-press classification.
/// The chip's BOOT-button pin type — see [`Button::new`].
#[cfg(all(feature = "hw", not(feature = "esp32s3")))]
type BootPin<'d> = esp_hal::peripherals::GPIO9<'d>;
#[cfg(all(feature = "hw", feature = "esp32s3"))]
type BootPin<'d> = esp_hal::peripherals::GPIO0<'d>;

#[cfg(feature = "hw")]
pub struct Button {
    pin: Input<'static>,
    gesture: Gesture,
}

#[cfg(feature = "hw")]
impl Button {
    /// Wrap the BOOT pin as a pulled-up active-low input. `main` owns `esp_hal::init()`
    /// and the pin singleton and passes it in, so the HAL is initialised once.
    ///
    /// The PIN is a chip fact: GPIO9 on the C3 boards, GPIO0 on the S3/ES3C28P (#398 —
    /// where GPIO9 is the battery ADC). Both are that chip's boot-strapping pin with the
    /// same pulled-up active-low behaviour, so only the type changes.
    pub fn new(pin: BootPin<'static>) -> Self {
        let input = Input::new(pin, InputConfig::default().with_pull(Pull::Up));
        Self {
            pin: input,
            gesture: Gesture::new(),
        }
    }

    /// Raw logical "is the button pressed right now" (active-low -> `is_low`).
    #[inline]
    fn is_pressed(&self) -> bool {
        self.pin.is_low()
    }

    /// Advance the gesture machine at monotonic time `now_ms` with the pin's current level.
    ///
    /// Call every sub-tick. All the rules (and the reasoning behind their constants) live in
    /// [`Gesture::poll`]; this only reads the pin, so the logic stays host-testable.
    pub fn poll(&mut self, now_ms: u64) -> Option<Press> {
        self.gesture.poll(self.is_pressed(), now_ms)
    }
}
