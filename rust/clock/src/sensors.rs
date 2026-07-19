//! On-board sensor readouts: ESP32-C3 internal die temperature + battery
//! voltage via ADC1.
//!
//! Two independent, always-compiled (Phase 1+) readouts:
//!
//!   * **Chip temperature** — the C3's *internal* temperature sensor
//!     (`esp_hal::tsens`). This is the DIE temperature, NOT ambient: the silicon
//!     runs warmer than the room (CPU clock + I/O load self-heat it), so treat it
//!     as a rough "how hot is the chip" gauge, not a thermometer. Range −40..125°C.
//!
//!   * **Battery voltage** — one ADC1 oneshot read on [`BATT_ADC_GPIO`] (GPIO4).
//!     THIS ASSUMES an external resistor divider (ratio [`BATT_DIVIDER`]) is wired
//!     from the battery + to that pin. **With nothing wired, the pin floats and
//!     the reading is meaningless.** See the README "Sensors" section for the
//!     divider wiring. The raw 12-bit code is converted to the pin voltage, scaled
//!     back up by the divider, then mapped to a rough 1S-LiPo percentage.
//!
//! Both the temperature sensor and ADC1 live behind the SAR-ADC peripheral; each
//! takes its own reference-counted peripheral guard from esp-hal, so they
//! coexist without fighting over the peripheral.

// #152: the physical die-temp + battery-ADC reads are HAL (`hw`). The host emulator
// (`hostsim`) provides a stub `Sensors` further down that returns a canned `Reading`, so
// `draw_clock` (which takes `&mut Sensors` + calls `.read()`) compiles + renders unchanged.
#[cfg(feature = "hw")]
use esp_hal::{
    analog::adc::{Adc, AdcConfig, AdcPin, Attenuation},
    peripherals::{ADC1, GPIO4, TSENS},
    tsens::{Config as TsensConfig, TemperatureSensor},
};

// --- Battery ADC configuration (documented constants) -----------------------

/// GPIO used for the battery-voltage ADC read. **GPIO4 = ADC1 channel 4.**
///
/// Chosen because it is free on the smol board and has no boot/strapping or
/// 32 kHz-crystal role on the C3: GPIO5/6 are the OLED I²C, GPIO8 is the blue
/// LED, GPIO9 is BOOT, and GPIO2 is a strapping pin — so GPIO0/1/3/4 are the
/// safe ADC1 candidates and GPIO4 is the cleanest.
///
/// This is documentation only; the pin is bound as `peripherals.GPIO4` in
/// [`Sensors::new`]. Keep this const and that binding in sync.
pub const BATT_ADC_GPIO: u8 = 4;

/// External resistor-divider ratio on [`BATT_ADC_GPIO`]: `Vbatt / Vpin`.
///
/// `2.0` is the classic two-equal-resistor divider (e.g. 100 kΩ / 100 kΩ),
/// which halves the battery voltage so a full 4.2 V cell lands at ~2.1 V — well
/// inside the ADC's range at 11 dB attenuation. If you wire a different divider,
/// change this to `(R_top + R_bottom) / R_bottom`.
pub const BATT_DIVIDER: f32 = 2.0;

/// Assumed ADC full-scale voltage at 11 dB attenuation, in volts.
///
/// This is the *uncalibrated* nominal: a 12-bit code of 4095 is treated as
/// ~3.3 V at the pin. The real per-chip full-scale at 11 dB is closer to
/// ~2.5–3.1 V and drifts with the (unimplemented) eFuse calibration, so the
/// absolute voltage here is a ballpark, not a precision measurement.
pub const ADC_FULL_SCALE_V: f32 = 3.3;

/// 1S-LiPo empty voltage (→ 0 %).
pub const BATT_EMPTY_V: f32 = 3.3;
/// 1S-LiPo full voltage (→ 100 %).
pub const BATT_FULL_V: f32 = 4.2;

/// A single sensor sample.
#[derive(Clone, Copy)]
pub struct Reading {
    /// Internal chip (die) temperature in °C. NOT ambient.
    pub chip_c: f32,
    /// Estimated battery voltage in volts (after applying [`BATT_DIVIDER`]).
    /// Meaningless unless a divider is actually wired to [`BATT_ADC_GPIO`].
    pub batt_v: f32,
    /// Rough 1S-LiPo state-of-charge, 0..100 %, clamped. Meaningless without a
    /// divider (see [`batt_v`](Self::batt_v)).
    pub batt_pct: u8,
}

/// Owns the temperature sensor + a configured ADC1 channel and produces a
/// [`Reading`] on demand.
#[cfg(feature = "hw")]
pub struct Sensors<'d> {
    tsens: TemperatureSensor<'d>,
    adc: Adc<'d, ADC1<'d>, esp_hal::Blocking>,
    batt_pin: AdcPin<GPIO4<'d>, ADC1<'d>>,
}

#[cfg(feature = "hw")]
impl<'d> Sensors<'d> {
    /// Bring up the chip temperature sensor and ADC1 on [`BATT_ADC_GPIO`].
    ///
    /// `TSENS`, `ADC1` and `GPIO4` are peripheral singletons handed out by
    /// `esp_hal::init()`; `main` owns that call and passes them in here.
    pub fn new(tsens: TSENS<'d>, adc1: ADC1<'d>, gpio4: GPIO4<'d>) -> Self {
        // Temperature sensor: default config (XTAL clock). `new` powers it up;
        // caller should allow a few hundred µs before the first read to settle.
        let tsens = TemperatureSensor::new(tsens, TsensConfig::default())
            .expect("temperature sensor init");

        // ADC1 oneshot on the battery pin. 11 dB attenuation gives the widest
        // input range (~0..3.3 V at the pin) so a 2:1-divided 1S LiPo (≤2.1 V)
        // fits with headroom.
        let mut adc_config = AdcConfig::new();
        let batt_pin = adc_config.enable_pin(gpio4, Attenuation::_11dB);
        let adc = Adc::new(adc1, adc_config);

        Self {
            tsens,
            adc,
            batt_pin,
        }
    }

    /// Take one temperature + one battery-voltage sample.
    pub fn read(&mut self) -> Reading {
        let chip_c = self.tsens.get_temperature().to_celsius();

        // Blocking oneshot ADC read. `read_oneshot` is non-blocking (returns
        // WouldBlock while converting); `nb::block!` spins until the conversion
        // completes. The conversion takes only a few ADC clocks, so this is a
        // very short busy-wait, not a stall.
        let raw: u16 = nb::block!(self.adc.read_oneshot(&mut self.batt_pin)).unwrap_or(0);

        let batt_v = raw_to_batt_volts(raw);
        let batt_pct = batt_percent(batt_v);

        Reading {
            chip_c,
            batt_v,
            batt_pct,
        }
    }
}

/// #152 host emulator stub: a `Sensors` with the SAME `read()` surface but no HAL — it
/// returns a fixed, plausible `Reading` (mild die temp, ~full 1S cell) so the Clock's
/// sensor line renders in the browser. Keeps the `'d` lifetime so `Ctx`'s
/// `&'a mut Sensors<'static>` field type is unchanged. `Default` = `new()`.
#[cfg(not(feature = "hw"))]
#[derive(Default)]
pub struct Sensors<'d> {
    _marker: core::marker::PhantomData<&'d ()>,
}

#[cfg(not(feature = "hw"))]
impl<'d> Sensors<'d> {
    pub fn new() -> Self {
        Self {
            _marker: core::marker::PhantomData,
        }
    }

    /// A canned sample (24.0 °C die, 4.05 V ≈ 92 % 1S) — the emulator has no hardware.
    pub fn read(&mut self) -> Reading {
        Reading {
            chip_c: 24.0,
            batt_v: 4.05,
            batt_pct: batt_percent(4.05),
        }
    }
}

/// Convert a 12-bit ADC code (0..4095) to the estimated battery voltage,
/// undoing the external divider. Uncalibrated (see [`ADC_FULL_SCALE_V`]).
pub fn raw_to_batt_volts(raw: u16) -> f32 {
    let v_pin = (raw as f32 / 4095.0) * ADC_FULL_SCALE_V;
    v_pin * BATT_DIVIDER
}

/// Map a 1S-LiPo terminal voltage to a rough 0..100 % charge, linear between
/// [`BATT_EMPTY_V`] and [`BATT_FULL_V`], clamped at both ends. This is a crude
/// linear approximation — a real LiPo discharge curve is not linear — but it is
/// good enough for a "roughly how full" bar.
pub fn batt_percent(v: f32) -> u8 {
    if v <= BATT_EMPTY_V {
        return 0;
    }
    if v >= BATT_FULL_V {
        return 100;
    }
    (((v - BATT_EMPTY_V) / (BATT_FULL_V - BATT_EMPTY_V)) * 100.0) as u8
}

/// A tiny fixed-capacity, `no_std`/heap-free string builder for the OLED sensor
/// line, so the default (allocator-less) build can still format `"23C 3.9V"`.
/// Writes past the capacity are silently dropped (the content is bounded and
/// short by construction).
pub struct LineBuf {
    buf: [u8; Self::CAP],
    len: usize,
}

impl LineBuf {
    const CAP: usize = 20;

    pub fn new() -> Self {
        Self {
            buf: [0; Self::CAP],
            len: 0,
        }
    }

    /// Borrow the written bytes as a `&str` (always valid UTF-8: we only ever
    /// write ASCII via the formatter).
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < Self::CAP {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Format a compact sensor line for the 72 px OLED bottom row, e.g. `"23C 3.9V"`.
///
/// Temperature is rounded to a whole °C and voltage to one decimal, keeping the
/// string to ~8–10 chars so it never overflows 72 px in FONT_5X8 (~12 chars).
pub fn format_sensor_line(r: &Reading, temp_f: bool) -> LineBuf {
    use core::fmt::Write;
    let mut line = LineBuf::new();
    // #43: chip temp in °F (default) or °C per the fleet-global units, rounded to a whole
    // degree; volts to one decimal. e.g. "73F 3.9V" / "23C 3.9V".
    if temp_f {
        let f = r.chip_c * 9.0 / 5.0 + 32.0;
        let _ = write!(line, "{}F {:.1}V", f as i32, r.batt_v);
    } else {
        let _ = write!(line, "{}C {:.1}V", r.chip_c as i32, r.batt_v);
    }
    line
}
