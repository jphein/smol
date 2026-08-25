//! Board module — **LCDWIKI/QDtech ES3C28P** ("Hosyond 2.8in ESP32-S3 Touchscreen"),
//! smol fleet node **162**.
//!
//! STAGING DRAFT for the smol#398 phase-2 intake PR. Pure constants + provenance: no
//! `esp-hal` types, no imports, so it compiles anywhere and can be lifted into whatever
//! module layout the intake PR chooses. Where a real driver wants a typed value
//! (`PsramMode`, `ColorInversion`, `Orientation`), the raw value is given here with the
//! construction named in the doc comment — the type stays on the consuming side.
//!
//! # Discipline this file follows
//!
//! Transposed from `cyd-c5/watch-port/src/board.rs` (the C5 CYD's L3 layer):
//!
//! 1. **Every `pub const` carries a source citation** in its doc comment.
//! 2. **Hazards live AT the constant**, not in a README nobody re-reads at the call site.
//! 3. **Unmeasured values are labelled `PLACEHOLDER`** with the procedure that settles
//!    them named — never a plausible-looking number with no provenance.
//!
//! # Provenance chain
//!
//! Facts here are triple-sourced unless marked otherwise, in this order of authority:
//!
//! 1. **Vendor schematic** — `ES3C28P_Schematic.pdf`, fetched 2026-08-01 from lcdwiki,
//!    archived at `ember.realm.watch/docs/vendor/`. Settles electrical questions
//!    (`docs/vendor/README.md`).
//! 2. **emberboy** — `retro-go/components/retro-go/targets/ember-s3/config.h`. A working
//!    C firmware; authoritative on pins, *not* on MADCTL (see [`MADCTL_LANDSCAPE`]).
//! 3. **ESPHome** — `ember.realm.watch/esphome/ember-satellite.yaml`. Live on the same
//!    board class for months.
//! 4. **Rust on glass** — `emberburrito/burrito-fw`. Proves the values work through
//!    `esp-hal 1.1.x` + `mipidsi 0.10`, which is the stack smol will use.
//! 5. **Spike M1 on THIS unit** — flashed 2026-08-24 23:2x, serial heartbeat verified live
//!    (`targets/s3-cyd/spike/README.md:14`).
//!
//! Distilled in `targets/s3-cyd/BOARD.md`; recon in
//! `~/.claude/projects/-home-jp/scratch/s3-cyd-target/explore-ember.md`.
//!
//! # ⚠️ Verification tiers — glass-verified vs board-class-verified
//!
//! These are **not** the same claim and the file marks which is which:
//!
//! * **UNIT-VERIFIED** on `14:C1:9F:D1:C8:10` (spike M1): boot, console, octal PSRAM maps
//!   (`8388608` bytes), panel paints, button reads, chip rev v0.2 / efuse block v1.4 /
//!   crystal 40 MHz. **Panel ORIENTATION is still awaiting a human eyeball on the glass**
//!   — see [`MADCTL_LANDSCAPE`].
//! * **BOARD-CLASS-VERIFIED**: everything else — proven on sibling ES3C28P units (Ember,
//!   ember-mobile, ember-dad, emberburrito, emberboy), not on this one. Same model, same
//!   schematic. That is strong, and it is still one inference step away from this PCB.
//! * **SCHEMATIC-ONLY / INFERRED**: called out individually. [`FREE_GPIOS`] is the big one.
//!
//! # ⛔ Serial safety — byte-exact matching only
//!
//! Five other ES3C28P boards share this bench and **four are live family services**.
//! reliquary's sealed board is `14:C1:9F:D1:C3:C8` — it differs from this unit only in the
//! last two octets, and it also contains `C8`. Never prefix-match a serial here; the flash
//! guard (`targets/s3-cyd/spike/flash.sh:88-95`) says the same thing at more length.

#![allow(dead_code)]

// ===========================================================================
// Identity
// ===========================================================================

/// Human board name. `BOARD.md` "Identity".
pub const BOARD_NAME: &str = "ES3C28P";

/// Module part number: ESP32-S3 **N16R8** — 16 MB flash, 8 MB octal PSRAM, Xtensa LX7
/// dual-core, 2.4 GHz WiFi + BLE 5.0, **no 802.15.4**. `BOARD.md` "Identity".
pub const MODULE: &str = "ESP32-S3-N16R8";

/// Rust target triple. **Tier 3** — mainline rustup has no Xtensa; needs the espup fork
/// (`channel = "esp"`) plus `build-std = ["core", "alloc"]`, and every build shell must
/// `source ~/export-esp.sh` first or the link fails with ``linker `xtensa-esp32s3-elf-gcc`
/// not found`` (`reliquary/reliquary-fw/flash.sh:40-47`), which reads like a broken
/// toolchain rather than an unsourced shell.
pub const TARGET_TRIPLE: &str = "xtensa-esp32s3-none-elf";

/// Flash size in bytes — 16 MB. `BOARD.md` "Identity"; vendor spec.
pub const FLASH_BYTES: u32 = 16 * 1024 * 1024;

/// PSRAM size in bytes — 8 MB octal (OPI). **UNIT-VERIFIED**: this board reported exactly
/// `8388608` at spike M1.
///
/// ⚠️ **Assert the SIZE, never the mapping ADDRESS.** The base is image-dependent, not a
/// board constant: burrito-fw saw `0x3c020000`, this spike sees `0x3c060000`, because
/// flash-mapped segments shift it. A test pinned to the address passes on one firmware and
/// fails on the next for no hardware reason. (`BOARD.md` "Identity".)
pub const PSRAM_BYTES: u32 = 8 * 1024 * 1024;

/// This unit's base MAC = its USB `ID_SERIAL_SHORT` (native USB-Serial/JTAG, `303a:1001`,
/// CDC-ACM). **The flash guard's allow-list value.**
///
/// ⛔ See the module header: `14:C1:9F:D1:C3:C8` (reliquary, SEALED) differs only in the
/// last two octets. Byte-exact comparison, never a prefix.
pub const UNIT_SERIAL: &str = "14:C1:9F:D1:C8:10";

/// Silicon revision **v0.2**, efuse block rev **v1.4**, crystal **40 MHz** — captured from
/// this unit's first flash log 2026-08-25 (`BOARD.md` "Identity"), closing a gap no repo
/// had recorded. **UNIT-VERIFIED.**
pub const CHIP_REV: &str = "v0.2";

// ===========================================================================
// Display — ILI9341V on SPI2
// ===========================================================================
//
// `BOARD.md` "Pin map"; emberboy `config.h`; ESPHome `ember-satellite.yaml`; running in
// `burrito-fw/src/main.rs:7` ("SPI2 @ 40 MHz, CLK=12 MOSI=11 CS=10 DC=46").

/// SPI2 clock. `BOARD.md` GPIO 12 (`LCD_CLK`).
pub const PIN_LCD_SCK: u8 = 12;

/// SPI2 MOSI. `BOARD.md` GPIO 11 (`LCD_MOSI`).
pub const PIN_LCD_MOSI: u8 = 11;

/// SPI2 MISO — **unused**: the panel is write-only and nothing else sits on this bus.
///
/// Recorded rather than omitted so a future port does not reuse the pin believing it free.
/// Unlike the C5 CYD, this board has **no shared SPI bus**: no XPT2046, no SD slot (see
/// [`HAS_SD_CARD`]). The whole `SharedSpiBus` / chip-select-interleave hazard class from
/// `cyd-c5/watch-port/src/drivers/spi_bus.rs` **does not exist here**.
pub const PIN_LCD_MISO: u8 = 13;

/// Display chip select. `BOARD.md` GPIO 10 (`LCD_CS`).
pub const PIN_LCD_CS: u8 = 10;

/// Display data/command select. `BOARD.md` GPIO 46 (`LCD_DC`).
///
/// A strapping pin, but harmless as a runtime output — straps latch at reset, long before
/// firmware configures anything.
pub const PIN_LCD_DC: u8 = 46;

/// Backlight, **active-HIGH** through a BSS138 gate. `BOARD.md` GPIO 45 (`LCD_BL`).
///
/// ⚠️ GPIO45 is also the **VDD_SPI strapping pin**, which normally makes it a pin to fear:
/// strapped high it selects a 1.8 V flash rail and browns the module out. **Safe here, for
/// a schematic reason** — `R32` (10 K to GND) hard-wires the strap LOW, and the strap
/// latches at reset while GPIO drivers are still inputs, so R32 always wins. No firmware
/// setting can cause that brownout on this board. (Vendor schematic; `explore-ember.md` §2.)
///
/// burrito-fw drives it high *after* the first full paint, so boot shows a painted panel
/// rather than a white flash. ESPHome uses LEDC PWM @ 20 kHz for dimming.
pub const PIN_LCD_BACKLIGHT: u8 = 45;

/// `true` — backlight is asserted by driving the pin HIGH. See [`PIN_LCD_BACKLIGHT`].
pub const LCD_BACKLIGHT_ACTIVE_HIGH: bool = true;

// ---------------------------------------------------------------------------
// THERE IS NO DISPLAY RESET GPIO.
// ---------------------------------------------------------------------------
// LCD_RST is bonded to CHIP_PU / EN — the module's own reset line (`BOARD.md` pin map,
// final rows). So the panel is already out of reset whenever firmware runs, and there is
// no pin to toggle.
//
// ⚠️ This must be an EXPLICIT ABSENCE in the driver, not a stubbed dummy pin. mipidsi's
// builder takes `NoResetPin` (`burrito-fw/src/main.rs:56`) and relies on the software
// reset inside `init()`. Handing it some unrelated GPIO "because the type wants one"
// drives an unconnected pad and yields a panel that is never reset — which on glass looks
// exactly like a wiring fault. The C5 CYD hit precisely this: the vendor's MicroPython
// demo passes `reset=Pin(0)` purely to satisfy a required argument, and GPIO0 is
// documented FREE in the same repo (`cyd-c5/watch-port/src/board.rs`, the no-reset block).
// Same shape, same trap, different board.

/// Named absence: this board exposes **no** LCD reset GPIO. Software `SWRESET` only.
pub const HAS_LCD_RESET_PIN: bool = false;

/// Display SPI clock in Hz — **40 MHz**, proven on glass by burrito-fw
/// (`burrito-fw/src/main.rs:7`), Mode 0.
///
/// Note this is double the C5 CYD's vendor-proven 20 MHz. Nothing is shared with that
/// board: dedicated bus, different panel, native IOMUX pins.
pub const SPI_DISPLAY_HZ: u32 = 40_000_000;

/// SPI mode — 0. `esp-hal`'s `Config::default()` is already `Mode::_0`.
pub const SPI_DISPLAY_MODE: u8 = 0;

// ===========================================================================
// Panel geometry & MADCTL
// ===========================================================================

/// Panel controller. mipidsi model `ILI9341Rgb565` — **not** ST7789.
///
/// ⚠️ Worth stating loudly because "2.8 inch CYD-shaped ESP32 board" reads as ST7789 to
/// everyone: this board is dimensionally drop-in with the classic ESP32-2432S028 CYD
/// (`ember.realm.watch/docs/enclosure.md` §4) while sharing neither its panel controller
/// nor its touch controller. Dimensional compatibility is not electrical compatibility.
pub const PANEL_CONTROLLER: &str = "ILI9341V";

/// Native die geometry: 240×320 portrait.
pub const PANEL_NATIVE_W: u16 = 240;
/// See [`PANEL_NATIVE_W`].
pub const PANEL_NATIVE_H: u16 = 320;

/// Logical width in the shipped orientation (landscape).
pub const LCD_WIDTH: u16 = 320;
/// Logical height in the shipped orientation (landscape).
pub const LCD_HEIGHT: u16 = 240;

/// MADCTL (`0x36`) for landscape: **`0x28`** = `MV | BGR`.
///
/// In mipidsi 0.10 this is `Orientation::new().rotate(Deg90).flip_vertical()`.
///
/// ⛔ **Do NOT copy retro-go's `ILI9341_CMD(0x36, 0x68)`.** Its comment reads
/// "(MX|MV|BGR) = landscape", but `0x68` is `0x28` **with MX set — a horizontal mirror**,
/// which retro-go compensates for in its own framebuffer scan order. A normal renderer
/// must not. Taking that value at face value **shipped mirror-writing in burrito-fw v0.1**
/// (2026-08-15, fixed same day). This is landmine **L4** in `BOARD.md`.
///
/// Orientation ground truth is ESPHome's `ember-satellite.yaml`, which drives this exact
/// panel with **no** `transform:` / `mirror_x` / `mirror_y` in native portrait — so the
/// panel obeys the standard rotation table.
///
/// **If the image is upside-down but READABLE**, the fix is [`MADCTL_LANDSCAPE_FLIPPED`].
/// ⚠️ **Never re-add a mirror bit to correct a rotation** — mirrored and rotated look
/// similar on a symmetric boot screen and diverge the moment text appears.
///
/// **Verification status: BOARD-CLASS-VERIFIED, not unit-verified.** Spike M1 painted this
/// unit's panel, but orientation is still *"awaiting a human eyeball on the glass"*
/// (`spike/README.md:14`). Treat as correct-but-unwitnessed here.
pub const MADCTL_LANDSCAPE: u8 = 0x28;

/// The 180°-opposite landscape: **`0xE8`** = mipidsi `.flip_horizontal()`.
///
/// The **only** sanctioned escape hatch if the panel reads upside-down. `0x68` and `0xA8`
/// are the mirrored values and are never the answer — see [`MADCTL_LANDSCAPE`].
pub const MADCTL_LANDSCAPE_FLIPPED: u8 = 0xE8;

/// Colour order is **BGR** (the `0x08` bit, already folded into [`MADCTL_LANDSCAPE`]).
pub const MADCTL_COLOR_ORDER_BGR: bool = true;

/// Display inversion **ON** — mipidsi `ColorInversion::Inverted`, raw command `0x21`.
///
/// Stated loudly because it is the *opposite* of the C5 CYD's ST7789 panel, which needs
/// `INVOFF` (`cyd-c5/watch-port/src/board.rs`, `INVERT_COLORS = false`). Two 2.8" panels
/// on two boards in the same fleet, opposite answers: never inherit this one across boards.
pub const INVERT_COLORS: bool = true;

/// GRAM column offset — **zero in every rotation**: a clean full-frame 240×320 die, unlike
/// the 240×240 / 135×240 variants that carry offsets, and unlike the C6 watch's CO5300
/// (`col_offset = 22`).
pub const LCD_COL_OFFSET: u16 = 0;
/// See [`LCD_COL_OFFSET`].
pub const LCD_ROW_OFFSET: u16 = 0;

/// The ILI9341 has **no** even-alignment rule on `CASET`/`RASET`; 1×1 windows are legal.
///
/// Consequence for a Slint port: the C6 watch vendors a fork of Slint's software renderer
/// solely to even-align dirty regions for the CO5300's 2×2 restriction. **That restriction
/// does not exist here.** (The watch keeps the fork on every board anyway — it also carries
/// the `[POOL]` heap-attribution instrumentation, and even windows are a legal subset:
/// `esp32c6-watch/src/drivers/panel.rs:25-33`.)
pub const PANEL_REQUIRES_EVEN_WINDOWS: bool = false;

// ===========================================================================
// Touch — FT6336U on I²C0
// ===========================================================================

/// Capacitive touch controller: **FT6336U/G**, reports chip id `100` (`0x64`).
/// Capacitive, **not** the C5 CYD's resistive XPT2046 — different contract entirely
/// (contact vs pressure, no calibration span to measure).
pub const TOUCH_CONTROLLER: &str = "FT6336U";

/// Touch I²C address. `BOARD.md` "Touch".
pub const I2C_ADDR_TOUCH: u8 = 0x38;

/// Shared I²C0 **SDA**. `BOARD.md` GPIO 16 (`I2C_SDA`).
pub const PIN_I2C_SDA: u8 = 16;

/// Shared I²C0 **SCL**. `BOARD.md` GPIO 15 (`I2C_SCL`).
pub const PIN_I2C_SCL: u8 = 15;

/// I²C bus speed — 100 kHz. Shared by touch (`0x38`) and the ES8311 codec (`0x18`).
pub const I2C_HZ: u32 = 100_000;

/// Touch interrupt (`CTP_INT`). `BOARD.md` GPIO 17.
///
/// **Genuinely wired and usable** — ESPHome uses it. burrito-fw instead polls at UI frame
/// rate, which is a choice, not a limitation. (Contrast the C5 CYD, where the touch IRQ is
/// wired but *every* vendor config says `IRQ = -1`, so its usability was an inference; here
/// a shipping config actually uses it.)
pub const PIN_TOUCH_INT: u8 = 17;

// ---------------------------------------------------------------------------
// ⛔ GPIO 18 — CTP_RST — NEVER CONFIGURE. Landmine L1.
// ---------------------------------------------------------------------------
// Touch reset is physically wired to GPIO18, and **driving it breaks the FT6336**. Both a
// `reset_pin:` binding and a plain output-high were tested; both kill touch. Left entirely
// alone, the FT6336 pulls RSTN high internally and works.
//
// ⚠️ **The schematic says this pin floats and should be driven.** It is wrong in practice.
// TESTED BEATS DERIVED — this is the one constant in this file where the primary source
// loses to an experiment, and it is recorded that way deliberately.
//
// The absence of GPIO18 from touch initialisation is THE TRICK, not an oversight. A future
// reader "completing" the driver by adding the reset line will break working touch and will
// have every reason to think they fixed something. That is why this is a named constant
// with a refusal in its name rather than a silent omission.

/// ⛔ **NEVER configure this pin.** See the block above and `BOARD.md` landmine L1.
/// Named so the prohibition is greppable and survives a refactor.
pub const PIN_TOUCH_RST_DO_NOT_CONFIGURE: u8 = 18;

/// Landscape touch transform, from retro-go: `swap_xy = true`.
///
/// ⚠️ **PLACEHOLDER-GRADE — awaiting real-finger confirmation under Rust.** burrito-fw
/// flagged the whole triple as unconfirmed on its stack. Being capacitive, there is no
/// calibration span to measure (unlike the C5's XPT2046) — the transform is either right
/// or visibly wrong.
///
/// **Procedure that settles it:** paint a known corner marker, tap each of the four
/// corners, log raw + transformed coordinates. Ten seconds, once. Until then expect taps
/// to land in the right *region* under an unverified axis flip.
pub const TOUCH_SWAP_XY: bool = true;
/// See [`TOUCH_SWAP_XY`] — same PLACEHOLDER status and same procedure.
pub const TOUCH_INVERT_X: bool = false;
/// See [`TOUCH_SWAP_XY`] — same PLACEHOLDER status and same procedure.
pub const TOUCH_INVERT_Y: bool = true;

// ===========================================================================
// Audio — ES8311 codec + SC8002B amp
// ===========================================================================

/// ES8311 codec I²C address. Shares I²C0 with touch.
pub const I2C_ADDR_CODEC: u8 = 0x18;

// ---------------------------------------------------------------------------
// ⚠️ I²S PIN NAMING TRAP — the silkscreen names data pins from the CODEC's side.
// ---------------------------------------------------------------------------
// `I2S_DO` is the codec's data OUT, i.e. the ESP's data IN (microphone). `I2S_DI` is the
// codec's data IN, i.e. the ESP's data OUT (playback). Reading them as ESP-side names
// swaps record and playback, which presents as silence in both directions.
//
// This trap is LIVE, not hypothetical: `BOARD.md` records that GPIO6 "gets dropped from
// third-party pinouts", and the brief commissioning this very file listed the I²S pins as
// "4/5/7/8" — omitting GPIO6, the microphone. Corrected here.

/// I²S MCLK. `BOARD.md` GPIO 4.
///
/// ⚠️ Physically wired **but the codec is BCLK-derived** — which is exactly what makes
/// landmine [`ES8311_REG01_BCLK_DERIVED`] look right when it is wrong.
pub const PIN_I2S_MCLK: u8 = 4;

/// I²S bit clock (codec SCLK/BCLK). `BOARD.md` GPIO 5.
pub const PIN_I2S_BCLK: u8 = 5;

/// Codec `ASDOUT` → **ESP data IN**. This is the **MICROPHONE** path. `BOARD.md` GPIO 6.
/// The pin third-party pinouts drop — see the naming-trap block above.
pub const PIN_I2S_DIN_MIC: u8 = 6;

/// I²S word select (codec LRCK/WS). `BOARD.md` GPIO 7.
pub const PIN_I2S_WS: u8 = 7;

/// **ESP data OUT** → codec `DSDIN`. This is the **PLAYBACK** path. `BOARD.md` GPIO 8.
pub const PIN_I2S_DOUT_SPK: u8 = 8;

/// Speaker amplifier (SC8002B, 3 W class-AB) shutdown control. `BOARD.md` GPIO 1.
///
/// ⚠️ **ACTIVE LOW: drive LOW to turn the amp ON.** The inverted sense means the safe
/// power-on default (pin low / undriven) is *amp enabled*, not muted — the opposite of the
/// intuition. Drive it HIGH to silence.
pub const PIN_AMP_SHUTDOWN: u8 = 1;

/// `true` — the amp is enabled by driving [`PIN_AMP_SHUTDOWN`] LOW.
pub const AMP_SHUTDOWN_ACTIVE_LOW: bool = true;

/// ES8311 register `0x01` must be **`0xBF`** (BCLK-derived), **not `0x3F`**. Landmine L5.
///
/// ⚠️ MCLK *is* physically wired to GPIO4, which is precisely what makes `0x3F` look
/// correct — the wrong value is the one the schematic argues for. It **fails as silence,
/// never as an error**: no bus NACK, no status bit, just no sound.
///
/// Two consequences that ride along:
/// * BCLK-derived mode **refuses sample rates below 22050 Hz**.
/// * **Init order is load-bearing**: reset (`0x00 = 0x00`) FIRST, power-on
///   (`0x00 = 0x80`) LAST.
pub const ES8311_REG01_BCLK_DERIVED: u8 = 0xBF;

/// I²C bring-up order: **codec first, touch second.** Landmine L6.
///
/// The codec needs the bus once at boot and never again; touch needs it forever. In that
/// order the two never contend and **zero bus-sharing machinery is required** — a whole
/// category of code that exists only if you initialise them the other way round.
pub const I2C_INIT_CODEC_BEFORE_TOUCH: bool = true;

/// There is **no AEC** and the speaker sits inches from the microphone. Anything doing
/// simultaneous playback and capture must expect to hear itself.
pub const HAS_ACOUSTIC_ECHO_CANCELLATION: bool = false;

// ===========================================================================
// LED, button, battery
// ===========================================================================

/// WS2812 RGB LED (×1, **GRB** order) driven over RMT. `BOARD.md` GPIO 42.
///
/// ⚠️ **`esp-hal-smartled` 0.17 wants esp-hal ~1.0 and is incompatible with 1.1.x**, which
/// is smol's pin. Drive the RMT peripheral directly (~50 lines) rather than pulling the
/// crate and being forced to downgrade the HAL for one LED.
pub const PIN_WS2812: u8 = 42;

/// BOOT button (K2), **active-low**. `BOARD.md` GPIO 0.
///
/// ⚠️ **This is the entire hardware input budget** — one button, no rotary, no second key.
/// smol's `input.rs` `Press` model maps onto it directly, which is convenient; any UI
/// assuming two buttons does not fit this board. Also a strapping pin, and RTC-capable so
/// it can serve as a wake source.
pub const PIN_BOOT_BUTTON: u8 = 0;

/// `true` — [`PIN_BOOT_BUTTON`] reads LOW when pressed.
pub const BOOT_BUTTON_ACTIVE_LOW: bool = true;

/// Battery sense ADC. `BOARD.md` GPIO 9 (`BAT_ADC`).
///
/// Onboard **2:1 divider** (200 K / 200 K, schematic `R14`/`R15`) → multiply the reading by
/// **2.0**; 12 dB attenuation.
///
/// ⚠️ **Floats and reads noise with no cell fitted.** A "battery voltage" from an
/// unpopulated `JP1` is meaningless, not zero — do not publish it as telemetry without
/// knowing a cell is present.
///
/// ⚠️ **There is NO protection IC on this board.** The 2-pin BAT connector goes straight to
/// the cell; the only over-discharge floor is the ME6217 LDO's dropout (browns out ~3.4 V),
/// after which the divider (~9 µA) keeps draining. **Bare cells require an external 1S
/// protection strip** — JP fits one. (Vendor schematic; `ember.realm.watch` #44.)
pub const PIN_BAT_ADC: u8 = 9;

/// Battery divider ratio — multiply the ADC reading by this. See [`PIN_BAT_ADC`].
pub const BAT_ADC_DIVIDER: f32 = 2.0;

// ===========================================================================
// Reserved / absent
// ===========================================================================

/// GPIOs **consumed by the octal PSRAM** — 33..=37 inclusive. Do not use, do not probe.
pub const PSRAM_RESERVED_GPIOS: [u8; 5] = [33, 34, 35, 36, 37];

/// PSRAM octal mode is a **RUNTIME field**, not a build feature. Landmine L2.
///
/// In esp-hal 1.1.x: `PsramConfig { mode: PsramMode::OctalSpi, size: PsramSize::AutoDetect,
/// .. }`. There is **no `psram` cargo feature** and **no `ESP_HAL_CONFIG_PSRAM_MODE`
/// build-time knob** — both claims were re-verified false on 2026-08-14, so an older doc
/// asserting either is stale.
///
/// ⚠️ **Never leave an N16R8 on `Auto`.** Release builds only.
pub const PSRAM_MODE_IS_RUNTIME_CONFIG: bool = true;

/// ⛔ **Never place the esp-radio heap in PSRAM.** Landmine L3.
///
/// On the S3, atomics in PSRAM are **silently non-atomic** (esp-alloc's own documentation),
/// and the WiFi driver's internals are full of synchronisation primitives. Spend the 64 KiB
/// of internal RAM knowingly rather than discovering this as intermittent corruption.
pub const RADIO_HEAP_MAY_USE_PSRAM: bool = false;

/// **No SD card slot exists on this board.** Use a FAT partition on internal flash if
/// storage is ever needed. (The classic CYD has one; this board does not — another place
/// dimensional similarity misleads.)
pub const HAS_SD_CARD: bool = false;

/// **No LDR** (ambient light sensor). Another classic-CYD feature this board lacks.
pub const HAS_LDR: bool = false;

/// **No 802.15.4 radio** — WiFi + BLE only. Zigbee/Thread is not reachable from this chip.
pub const HAS_IEEE802154: bool = false;

/// GPIOs believed free.
///
/// ⚠️ **INFERRED, NOT SCHEMATIC-VERIFIED** — derived by subtracting claimed pins from the
/// package, which cannot see a net that exists but went unrecorded. Treat as a hypothesis:
/// meter a pin before committing hardware to it. Note **19/20 are the native USB D-/D+**
/// and are only "free" if USB is not used.
pub const FREE_GPIOS: [u8; 13] = [2, 3, 14, 19, 20, 21, 38, 39, 40, 41, 43, 44, 47];

// ===========================================================================
// Fleet identity
// ===========================================================================

/// smol fleet node id for **this unit**. `docs/protocol.md:87-89` — `160–175` is the S3
/// board class (ES3C28P family); `160` = Ember, `161` = emberburrito, **`162` = this dev
/// board**.
///
/// ⚠️ **Ids are per-NODE, never per-product.** Three ids in this block are the same model.
///
/// Identity lives in **NVS** and **OTA never touches NVS**, so only re-provisioning changes
/// it. The baked factory default is **7** — always pass `SMOL_NODE_ID=162` when
/// provisioning, or a fresh board silently lands on 7 alongside whatever else is there.
pub const NODE_ID: u8 = 162;

/// Fleet sigil name for this board: **`eldritch-insignia`**.
///
/// ⚠️ **MAC-derived via the `sigil-id` crate — never derive it by hand.** Two hand
/// derivations of the C5's sigil were both wrong. Row landed in `esp32c6-watch`
/// `feat/cyd-c5-target` @ `ba46f74` with the dual-contract test.
///
/// ⚠️ **Speech collision (not a protocol problem):** `eldritch-insignia` shares its
/// adjective with `eldritch-lantern`, JP's primary watch. Full sigils and MQTT topics are
/// unambiguous, but "the eldritch one" now means two devices — **never identify a board by
/// adjective when debugging by ear.**
pub const SIGIL_NAME: &str = "eldritch-insignia";

// ---------------------------------------------------------------------------
// 🔗 smol#396 — the variant-byte seam. LEAVE IT UNREAD.
// ---------------------------------------------------------------------------
// `BoardProfile::ha_device_extras()` matches on `(chip, has_display)`, and its
// `(CHIP_ESP32S3, _)` arm hard-returns `"smol ESP32-S3 Ember"`. Every S3 in the fleet —
// Ember, emberburrito, reliquary, this board — is `CHIP_ESP32S3` **with a display**, so
// `has_display` cannot separate them and all four would announce as Ember.
//
// The fix is smol#396's variant axis. Its shape was FROZEN 2026-08-25: an **NVS
// variant/product byte provisioned beside `SMOL_NODE_ID`**, with `protocol.md`'s table as
// the human record. It must be NVS rather than a runtime probe because the hardware is
// genuinely identical — there is nothing to detect. And it is explicitly **not** a cargo
// feature: #352's standing rule keeps the variant axis at runtime.
//
// ⚠️ **RULE FOR THIS FILE, from PORT-SCOPING.md:110-114: leave the seam, do not read the
// byte yet.** #396 lands with smol-d8's profile.rs lane after depin's PR. A constant here
// that reads or guesses the variant would have to be unpicked, and would meanwhile ship a
// second source of truth for board identity — the exact defect #352 was written to remove.

/// Placeholder for the smol#396 NVS variant byte. **Deliberately not read yet** — see the
/// block above. Present only so the seam is greppable from this file.
pub const VARIANT_BYTE_NVS_KEY: &str = "smol_variant";

// ===========================================================================
// Compile-time sanity — these catch a bad edit, not a bad board
// ===========================================================================
//
// Cheap, and they fail at build time rather than as a mystery on glass. They assert
// INTERNAL CONSISTENCY of this file; they cannot validate a wrong-but-consistent pin.

/// No pin is claimed by two different functions.
const _: () = {
    let pins = [
        PIN_LCD_SCK,
        PIN_LCD_MOSI,
        PIN_LCD_MISO,
        PIN_LCD_CS,
        PIN_LCD_DC,
        PIN_LCD_BACKLIGHT,
        PIN_I2C_SDA,
        PIN_I2C_SCL,
        PIN_TOUCH_INT,
        PIN_TOUCH_RST_DO_NOT_CONFIGURE,
        PIN_I2S_MCLK,
        PIN_I2S_BCLK,
        PIN_I2S_DIN_MIC,
        PIN_I2S_WS,
        PIN_I2S_DOUT_SPK,
        PIN_AMP_SHUTDOWN,
        PIN_WS2812,
        PIN_BOOT_BUTTON,
        PIN_BAT_ADC,
    ];
    let mut i = 0;
    while i < pins.len() {
        let mut j = i + 1;
        while j < pins.len() {
            assert!(pins[i] != pins[j], "two functions claim the same GPIO");
            j += 1;
        }
        i += 1;
    }
};

/// No claimed pin collides with the octal-PSRAM reservation (33..=37).
const _: () = {
    let pins = [
        PIN_LCD_SCK,
        PIN_LCD_MOSI,
        PIN_LCD_MISO,
        PIN_LCD_CS,
        PIN_LCD_DC,
        PIN_LCD_BACKLIGHT,
        PIN_I2C_SDA,
        PIN_I2C_SCL,
        PIN_TOUCH_INT,
        PIN_I2S_MCLK,
        PIN_I2S_BCLK,
        PIN_I2S_DIN_MIC,
        PIN_I2S_WS,
        PIN_I2S_DOUT_SPK,
        PIN_AMP_SHUTDOWN,
        PIN_WS2812,
        PIN_BOOT_BUTTON,
        PIN_BAT_ADC,
    ];
    let mut i = 0;
    while i < pins.len() {
        assert!(
            pins[i] < 33 || pins[i] > 37,
            "a claimed GPIO lands inside the octal-PSRAM reservation 33..=37"
        );
        i += 1;
    }
};

/// The landscape MADCTL is not one of the two MIRRORED values.
///
/// Same guard shape as `burrito-fw`'s `assert_madctl_matches_upstream()`: `0x68` and `0xA8`
/// are the mirror-writing values that shipped a bug once already, and `0xE8` (the
/// legitimate 180° escape hatch) must stay representable.
const _: () = {
    assert!(
        MADCTL_LANDSCAPE != 0x68 && MADCTL_LANDSCAPE != 0xA8,
        "MADCTL is a MIRRORED value — see MADCTL_LANDSCAPE's retro-go note"
    );
    assert!(
        MADCTL_LANDSCAPE == 0x28 || MADCTL_LANDSCAPE == MADCTL_LANDSCAPE_FLIPPED,
        "MADCTL is neither of the two canonical ILI9341 landscape values"
    );
};

/// Landscape geometry is the native die transposed — catches an edited dimension.
const _: () = {
    assert!(LCD_WIDTH == PANEL_NATIVE_H && LCD_HEIGHT == PANEL_NATIVE_W);
};
