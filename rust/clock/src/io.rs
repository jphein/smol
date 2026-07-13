//! #72 IO/component registry — runtime pin-binding core (v1: digital `Flex`).
//!
//! ## The model: ESPHome inverted
//!
//! ESPHome generates C++ from YAML and **recompiles per config**. smol wants **no
//! recompile**, so we invert it: every driver is compiled into ONE image (a driver
//! menu), and a runtime pin-map — relayed over the #56 keyed-CFG `G` channel, NOT
//! rebuilt — selects which driver binds to which GPIO at boot. Same trade smol
//! already made for app plugins (#7), one level down: this `PinMap` is to GPIOs
//! what the `Plugin` trait is to screens.
//!
//! ## What this module is (the de-risked foundation)
//!
//! Issue #72 §2.4 flagged ONE real unknown: esp-hal moves *typed* GPIO singletons
//! at construction, so runtime binding needs the free pins held type-erased and
//! **re-typed between input and output at runtime**. This module proves that for
//! the digital case: the 5 free pins are held as [`Flex`] (which is just an erased
//! `AnyPin` inside), and [`PinMap::bind_input`] / [`PinMap::bind_output`] flip a
//! pin's direction live by toggling its input/output buffers — no recompile, no
//! peripheral re-take. [`boot_selftest`] exercises exactly that transition on real
//! silicon so the hardware canary can confirm it.
//!
//! **Digital only in v1.** `Flex` re-types digital in/out cleanly; ADC and RMT/WS2812
//! do NOT — attenuation/calibration and the RMT channel are fixed at bind — so they
//! are deferred to v1.5/v2 behind their own bind-time spike.
//!
//! ## Persistence: none (relay-only, by design)
//!
//! The pin-map is NOT persisted in NVS — the `G` key writes ZERO flash (no wear, no
//! sector risk; the nvs partition is already full, sectors 0-5 all owned). It rides
//! purely on the gateway's retained-config relay: `broadcast_cached_configs`
//! (main.rs) re-relays every cached `(id, key)` config on the ~10 s flush cadence,
//! so a rebooted leaf re-arms its pins within ~10 s of the gateway being up — the
//! exact mechanism the S/L/U/P/Y config keys already rely on. Trade-off: a leaf that
//! reboots *while the gateway is down* has no IO config until the gateway returns.

use esp_hal::gpio::{Flex, InputConfig, Level, OutputConfig, Pull};

/// The GPIOs free for runtime binding on the ESP32-C3 SuperMini. Everything else on
/// the exposed header is already claimed by an on-board peripheral or is a boot
/// strapping / USB-serial pin — see [`RESERVED_PINS`]. Slot `i` of a [`PinMap`] owns
/// the physical pin `FREE_PINS[i]`.
pub const FREE_PINS: [u8; 5] = [0, 1, 3, 7, 10];

/// GPIOs that MUST NEVER be bound: a `G`-key descriptor naming one is rejected and
/// surfaced in DIAG, never applied. Cited against their real claim sites (NOT
/// `board.rs`, which is the per-board *identity* template — `NODE_ID`/`DEFAULT_APP`
/// only — and holds no pin map):
///   * `4`     — battery ADC        (`main.rs` `sensors::Sensors::new(.. GPIO4)`; `sensors::BATT_ADC_GPIO`)
///   * `5`,`6` — OLED I²C SDA/SCL   (`main.rs` `.with_sda(GPIO5)` / `.with_scl(GPIO6)`; `main.rs` header)
///   * `8`     — blue status LED    (`main.rs` `led::Led::new(Output::new(GPIO8 ..))`; `led.rs`, strapping)
///   * `9`     — BOOT button        (`main.rs` `Button::new(GPIO9)`; `input.rs`, strapping)
///   * `2`     — boot strapping pin (C3 boot-mode select; #72 pin-budget note)
///   * `20`,`21` — USB-serial UART  (log console; #72 pin-budget note)
pub const RESERVED_PINS: [u8; 8] = [2, 4, 5, 6, 8, 9, 20, 21];

/// Number of runtime-bindable slots (= free pins). Fixed capacity → no alloc.
pub const NPIN: usize = FREE_PINS.len();

/// The role a slot is currently bound to. v1 is digital only — the provably-feasible
/// `Flex` case. (v2 adds analog/PWM/bus kinds behind the §2.4 bind-time spike.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinDir {
    /// Digital input with the internal pull-up (active-low convention, as the BOOT
    /// button uses): a button/contact reads `Low` when pressed/closed.
    Input,
    /// Digital push-pull output: drives a relay coil or a plain single-color LED
    /// (the #75 dollhouse room lamp).
    Output,
}

/// A configured component's kind — the semantic role HA sees, layered over the
/// electrical [`PinDir`]. v1 is digital only. The wire char is the single byte after
/// the pin number in a `G`-key binding (e.g. `7B` = GPIO7 is a Button).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComponentKind {
    /// Momentary button / contact (`B`) → HA `binary_sensor` / press events. Digital
    /// input, pull-up (reads `Low` when pressed, like the BOOT button).
    Button,
    /// Latching binary sensor (`S`) — reed / PIR / float → HA `binary_sensor`. Digital
    /// input, pull-up.
    BinarySensor,
    /// Relay / GPIO switch (`R`) → HA `switch`. Digital push-pull output.
    Relay,
    /// Plain single-color LED (`L`) → HA `light` — the #75 dollhouse room lamp. Digital
    /// push-pull output (drive High = on for a header-wired LED-to-GND).
    Led,
}

impl ComponentKind {
    /// Decode the wire char. Unknown → `None` (forward-compat drop, #56 / #46 clamp).
    pub fn from_wire(c: u8) -> Option<Self> {
        match c {
            b'B' => Some(Self::Button),
            b'S' => Some(Self::BinarySensor),
            b'R' => Some(Self::Relay),
            b'L' => Some(Self::Led),
            _ => None,
        }
    }

    /// The electrical direction this kind binds the pin to.
    pub fn dir(self) -> PinDir {
        match self {
            Self::Button | Self::BinarySensor => PinDir::Input,
            Self::Relay | Self::Led => PinDir::Output,
        }
    }
}

/// A raw input level must be stable this long (ms) before a committed edge counts —
/// same 25 ms debounce the BOOT button uses (`input.rs`), rejecting tact-switch bounce.
const INPUT_DEBOUNCE_MS: u64 = 25;

/// Per-slot debounced input state (meaningful only for a bound INPUT slot). Active-low
/// convention (pull-up): a press pulls the pin LOW, so a HIGH→LOW committed edge is a
/// "press" and bumps `count`. Reset when the slot is (re)bound as an input.
#[derive(Clone, Copy)]
struct InState {
    /// Last COMMITTED (debounced) level — `true` = high = released.
    level: bool,
    /// The raw candidate level currently being debounced.
    cand: bool,
    /// When the current candidate was first seen (monotonic ms).
    cand_since: u64,
    /// Falling-edge (press) count — monotonic, wraps. HA reads it as a press event source.
    count: u16,
}

impl InState {
    const fn new() -> Self {
        // Released (high) at rest — the pull-up idles high until something pulls it low.
        Self { level: true, cand: true, cand_since: 0, count: 0 }
    }
}

/// Fixed-capacity runtime pin-map: one optional `Flex` slot per [`FREE_PINS`] entry,
/// no alloc. Every free pin is held as a `Flex` for the whole run (claimed once at
/// boot); [`PinDir`] tracks whether a slot is currently bound as input, output, or
/// unbound (Hi-Z). Binding is a runtime buffer-direction flip — the #72 §2.4 spike.
pub struct PinMap {
    /// Slot `i` owns physical pin `FREE_PINS[i]`, type-erased as a `Flex`.
    flex: [Flex<'static>; NPIN],
    /// Slot `i`'s current electrical role, or `None` if unbound (both buffers off → Hi-Z).
    dir: [Option<PinDir>; NPIN],
    /// Slot `i`'s debounced input state (press counter + level). Only advanced for a
    /// bound INPUT slot by [`PinMap::poll_inputs`]; reset when (re)bound as an input.
    inputs: [InState; NPIN],
}

impl PinMap {
    /// Adopt the free GPIOs (already wrapped as `Flex` by the caller, which owns the
    /// peripheral singletons). Every slot starts UNBOUND / Hi-Z — `Flex::new` resets
    /// each pin to a known state with no buffer enabled, so nothing is driven until a
    /// binding selects it.
    pub fn new(flex: [Flex<'static>; NPIN]) -> Self {
        Self {
            flex,
            dir: [None; NPIN],
            inputs: [InState::new(); NPIN],
        }
    }

    /// Slot index owning physical pin `gpio`, or `None` if `gpio` is not a free pin.
    pub fn slot_of(gpio: u8) -> Option<usize> {
        FREE_PINS.iter().position(|&p| p == gpio)
    }

    /// Is `gpio` on the never-bind reject list ([`RESERVED_PINS`])?
    pub fn is_reserved(gpio: u8) -> bool {
        RESERVED_PINS.contains(&gpio)
    }

    /// Bind slot `slot` as a digital input with the internal pull-up. Runtime re-type:
    /// disable the output driver, apply the pull, enable the input buffer.
    pub fn bind_input(&mut self, slot: usize) {
        let f = &mut self.flex[slot];
        f.set_output_enable(false);
        f.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
        f.set_input_enable(true);
        self.dir[slot] = Some(PinDir::Input);
        self.inputs[slot] = InState::new(); // fresh debounce + zeroed press counter
    }

    /// Bind slot `slot` as a digital push-pull output at `level`. Runtime re-type:
    /// disable the input buffer, set the initial level BEFORE enabling the driver
    /// (so the pin never glitches through the opposite level), enable the output.
    pub fn bind_output(&mut self, slot: usize, level: Level) {
        let f = &mut self.flex[slot];
        f.set_input_enable(false);
        f.apply_output_config(&OutputConfig::default());
        f.set_level(level);
        f.set_output_enable(true);
        self.dir[slot] = Some(PinDir::Output);
    }

    /// Bind slot `slot` to a component `kind`: applies the kind's electrical direction
    /// (inputs → pull-up; outputs → OFF = `Level::Low`, so a freshly-configured relay/LED
    /// starts de-energised until commanded on). The semantic kind is re-derived from the
    /// `G` wire each apply; per-slot kind storage lands in a later stage (uplink/discovery).
    pub fn bind_component(&mut self, slot: usize, kind: ComponentKind) {
        match kind.dir() {
            PinDir::Input => self.bind_input(slot),
            PinDir::Output => self.bind_output(slot, Level::Low),
        }
    }

    /// Release slot `slot` back to unbound / Hi-Z (both buffers off). Used when a `G`
    /// descriptor clears a pin, or to leave pins safe after the self-test.
    pub fn clear(&mut self, slot: usize) {
        let f = &mut self.flex[slot];
        f.set_output_enable(false);
        f.set_input_enable(false);
        self.dir[slot] = None;
    }

    /// Read a bound-input slot's live level. `None` if the slot is not an input.
    pub fn read(&self, slot: usize) -> Option<Level> {
        match self.dir[slot] {
            Some(PinDir::Input) => Some(self.flex[slot].level()),
            _ => None,
        }
    }

    /// Drive a bound-output slot to `level`. No-op if the slot is not an output.
    pub fn write(&mut self, slot: usize, level: Level) {
        if self.dir[slot] == Some(PinDir::Output) {
            self.flex[slot].set_level(level);
        }
    }

    /// The role slot `slot` is currently bound to (`None` = unbound).
    pub fn dir(&self, slot: usize) -> Option<PinDir> {
        self.dir[slot]
    }

    /// Poll every bound INPUT slot at monotonic `now_ms`: debounce the raw level and
    /// count a "press" on each committed HIGH→LOW edge (active-low, pull-up). Cheap —
    /// call every subtick. Output / unbound slots are skipped.
    pub fn poll_inputs(&mut self, now_ms: u64) {
        for slot in 0..NPIN {
            if self.dir[slot] != Some(PinDir::Input) {
                continue;
            }
            let raw = self.flex[slot].is_high();
            let st = &mut self.inputs[slot];
            if raw != st.cand {
                // new raw candidate — restart its debounce window
                st.cand = raw;
                st.cand_since = now_ms;
            } else if raw != st.level && now_ms.saturating_sub(st.cand_since) >= INPUT_DEBOUNCE_MS {
                // candidate held stable past the debounce window → commit it
                st.level = raw;
                if !raw {
                    st.count = st.count.wrapping_add(1); // fell to LOW = a press
                }
            }
        }
    }

    /// The debounced press count for a bound INPUT slot, or `None` if the slot is not a
    /// bound input. Folded into the DIAG record (`io=<pin>:<count>,…`) so HA sees presses.
    pub fn input_count(&self, slot: usize) -> Option<u16> {
        match self.dir[slot] {
            Some(PinDir::Input) => Some(self.inputs[slot].count),
            _ => None,
        }
    }
}

/// #72 spike (canary-observable): prove runtime direction re-typing works on real
/// silicon. For each free pin, at RUNTIME with no recompile: bind it as OUTPUT and
/// latch Low then High (reading the driver latch back each time), then RE-TYPE the
/// SAME pin to INPUT-pull-up and read its live level — the exact `Flex` typed-
/// singleton transition issue #72 §2.4 called the one real unknown. Each step is
/// logged to serial so the hardware canary can confirm it. Every pin is left
/// CLEARED (Hi-Z) on exit, so the probe is safe even if something is wired. Called
/// ONCE at boot under the `io` feature.
pub fn boot_selftest(map: &mut PinMap) {
    for (slot, &gpio) in FREE_PINS.iter().enumerate() {
        map.bind_output(slot, Level::Low);
        let latched_low = map.flex[slot].output_level();
        map.write(slot, Level::High);
        let latched_high = map.flex[slot].output_level();
        let as_out = map.dir(slot);

        map.bind_input(slot); // <-- the runtime re-type: output -> input, same pin
        let read_level = map.read(slot);
        let as_in = map.dir(slot);

        log::info!(
            "smol #72 io-spike: GPIO{} {:?}->latch(lo={:?} hi={:?}) then {:?}->level={:?} [runtime rebind OK]",
            gpio,
            as_out,
            latched_low,
            latched_high,
            as_in,
            read_level,
        );

        map.clear(slot); // leave Hi-Z / unbound — safe default
    }
    // Exercise the bind-time validation helpers (stage 2's `G`-parser gates on these):
    // a free pin resolves to a slot; a reserved pin is reject-listed and slot-less.
    log::info!(
        "smol #72 io-spike: {} free pins {:?} rebindable (GPIO{} -> slot {:?}); reserved {:?} reject-listed (GPIO4 reserved? {})",
        NPIN,
        FREE_PINS,
        FREE_PINS[0],
        PinMap::slot_of(FREE_PINS[0]),
        RESERVED_PINS,
        PinMap::is_reserved(4),
    );
}

/// Apply a whole `G`-key pin-map wire to `map`, panic-free. Format: `;`-separated
/// tokens `<pin><kind>` where pin is decimal (0/1/3/7/10) and kind is one of
/// `B`(button) `S`(sensor) `R`(relay) `L`(led) — e.g. `0L;7B;10R`. Every slot is
/// CLEARED first (so a binding removed from the new map releases its pin), then each
/// valid token binds its slot. Invalid tokens — unparsable pin, reserved / non-free
/// pin, unknown or missing kind — are SKIPPED with a log and never applied (#46 clamp
/// / #56 forward-compat drop). An empty wire clears every pin. Returns the count bound.
///
/// The whole map rides ONE `G` value because `CfgCache` upserts on `(id, key)` — one
/// value per key per node (there is no per-pin sub-key). A 5-pin map is ~15 B, well
/// inside `CFG_VALUE_MAX` (64).
pub fn apply_wire(map: &mut PinMap, wire: &[u8]) -> usize {
    // Release every slot first — a binding dropped from the new map frees its pin.
    for slot in 0..NPIN {
        map.clear(slot);
    }
    let mut bound = 0;
    for tok in wire.split(|&b| b == b';') {
        let tok = tok.trim_ascii();
        if tok.is_empty() {
            continue;
        }
        // Leading decimal run = the pin; the first non-digit begins the kind + params.
        let split = tok.iter().position(|b| !b.is_ascii_digit()).unwrap_or(tok.len());
        let (pin_bytes, rest) = tok.split_at(split);
        let Some(pin) = parse_pin(pin_bytes) else {
            log::warn!("smol #72 io: token without a pin number — skipped");
            continue;
        };
        let Some(&kc) = rest.first() else {
            log::warn!("smol #72 io: GPIO{} has no type char — skipped", pin);
            continue;
        };
        let Some(kind) = ComponentKind::from_wire(kc) else {
            log::warn!("smol #72 io: GPIO{} unknown type '{}' — skipped", pin, kc as char);
            continue;
        };
        if PinMap::is_reserved(pin) {
            log::warn!("smol #72 io: GPIO{} is reserved — rejected", pin);
            continue;
        }
        let Some(slot) = PinMap::slot_of(pin) else {
            log::warn!("smol #72 io: GPIO{} is not a bindable pin — rejected", pin);
            continue;
        };
        map.bind_component(slot, kind);
        bound += 1;
        log::info!("smol #72 io: GPIO{} bound as {:?}", pin, kind);
    }
    bound
}

/// Apply a `g`-key output-states wire to `map`, panic-free. Format: `;`-separated
/// `<pin>=<0|1>` tokens (e.g. `0=1;10=0`) — `1`/`0` drives the pin's bound OUTPUT
/// High/Low (a room LED / relay on / off). No-op for a pin that is unbound or bound as
/// an INPUT (a control frame can arrive before its config `G`; both are retained +
/// relayed, so they converge, and `main` re-asserts the states after a `G` re-bind).
/// Unparsable tokens are SKIPPED with a log. Returns the count of outputs driven.
pub fn apply_set(map: &mut PinMap, wire: &[u8]) -> usize {
    let mut driven = 0;
    for tok in wire.split(|&b| b == b';') {
        let tok = tok.trim_ascii();
        if tok.is_empty() {
            continue;
        }
        let Some(eq) = tok.iter().position(|&b| b == b'=') else {
            log::warn!("smol #72 io: io-set token without '=' — skipped");
            continue;
        };
        let (pin_bytes, val_bytes) = tok.split_at(eq);
        let Some(pin) = parse_pin(pin_bytes.trim_ascii()) else {
            log::warn!("smol #72 io: io-set token without a pin number — skipped");
            continue;
        };
        // val_bytes starts with the '=' — skip it, then trim.
        let v = val_bytes[1..].trim_ascii();
        let level = if v == b"1" {
            Level::High
        } else if v == b"0" {
            Level::Low
        } else {
            log::warn!("smol #72 io: GPIO{} io-set value not 0/1 — skipped", pin);
            continue;
        };
        let Some(slot) = PinMap::slot_of(pin) else {
            log::warn!("smol #72 io: GPIO{} not a bindable pin — io-set skipped", pin);
            continue;
        };
        if map.dir(slot) == Some(PinDir::Output) {
            map.write(slot, level);
            driven += 1;
        } else {
            log::warn!("smol #72 io: GPIO{} not a bound output — io-set ignored", pin);
        }
    }
    driven
}

/// Parse 1-2 decimal digits as a pin number, panic-free. `None` if empty, > 2 digits,
/// or non-numeric (no exposed GPIO is > 21, so 2 digits is ample and keeps it bounded).
fn parse_pin(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() || bytes.len() > 2 {
        return None;
    }
    let mut n: u8 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(b - b'0')?;
    }
    Some(n)
}
