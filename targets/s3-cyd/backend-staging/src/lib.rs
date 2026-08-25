//! **S3 display backend — staging crate.** The 4th `app::Oled` arm for the ES3C28P
//! (smol node 162), proven to COMPILE for `xtensa-esp32s3-none-elf` before any
//! `rust/clock` intake.
//!
//! # Why this crate exists at all
//!
//! An uncompiled draft is smol's dominant defect shape — a correct-looking comment
//! describing behaviour the binary does not have. This crate's whole job is to be the
//! opposite: it holds no logic worth reading on its own, and its value is entirely that
//! `cargo check --release` for xtensa is **green**. That green proves, today, that four
//! independently-developed pieces actually compose:
//!
//! | piece | what it contributes |
//! |---|---|
//! | [`oled_scale::ScaledOled`] | logical 72×40 `DrawTarget<BinaryColor>` → RGB565 at 4× |
//! | `mipidsi = "=0.10.0"` | `ILI9341Rgb565` + `NoResetPin` |
//! | `esp-hal 1.1.1` | SPI2, `Output`, `Delay` |
//! | [`board`] | the pin/geometry constants, dependency-free by design |
//!
//! Any type mismatch between them is a phase-2 blocker, and it is much cheaper to find
//! here than inside an intake PR against `rust/clock`.
//!
//! # What this is NOT
//!
//! **Not wired into `app.rs`.** That is the intake PR's job and it is routed through
//! smol-d8's lane. See [`INTEGRATION_SKETCH`] for the exact arm, which is the one part of
//! this work that cannot compile here. Design rationale: `targets/s3-cyd/DISPLAY-PACKAGE.md`
//! §4, Path A.
//!
//! Nor is it flashable: there is no `main`, no `[[bin]]`, and **no cargo `runner`** — the
//! watch-port convention (`cyd-c5/watch-port/.cargo/config.toml`). A library staging crate
//! must not be able to write to a bench that carries four live family services.

#![no_std]

/// The board constants, included **by path from the committed file** rather than copied.
///
/// `targets/s3-cyd/board-staging/board_es3c28p.rs` (committed `5c3a9a0`) is dependency-free
/// on purpose — pure `pub const`, no imports, no `esp-hal` types — and *this* is the payoff:
/// it can be `#[path]`-included by a crate that does have those dependencies, without
/// duplicating it.
///
/// ⚠️ **Deliberately not a copy.** A second copy of a pin table is a second source of truth,
/// and the one that gets edited is never the one that gets read.
#[path = "../../board-staging/board_es3c28p.rs"]
pub mod board;

pub mod s3_oled;

pub use s3_oled::{S3Oled, LETTERBOX_X, LETTERBOX_Y};

/// The `app.rs` arm the intake PR adds. **A SKETCH — it cannot compile here**, because it
/// names `crate::` paths that only exist inside `rust/clock`.
///
/// Placed in a doc comment rather than a `.rs` file precisely so it cannot be mistaken for
/// working code. It mirrors the three arms that already exist
/// (`rust/clock/src/app.rs:34-53`).
///
/// ```text
/// // ── in rust/clock/src/app.rs, beside the existing three arms ──────────────────
///
/// /// #398 S3: under `feature = "s3-display"` the one concrete OLED becomes the
/// /// ES3C28P's scaling backend — smol's logical 72×40 drawn at 4× into RGB565 and
/// /// blitted to a 320×240 ILI9341V. Same `DrawTarget<Color = BinaryColor>` + inherent
/// /// `init()`/`flush()` the plugins already call, so every screen draws UNCHANGED.
/// #[cfg(feature = "s3-display")]
/// pub type Oled = crate::s3_oled::S3Oled<'static>;
///
/// // …and the existing arms gain `not(feature = "s3-display")` the same way `cast`
/// // and `hostsim` already exclude each other.
///
/// // ── in main(), replacing the ssd1306 construction ─────────────────────────────
/// let panel = s3_oled::build_panel(
///     peripherals.SPI2, peripherals.GPIO12, peripherals.GPIO11,
///     peripherals.GPIO10, peripherals.GPIO46, delay,
/// );
/// let mut display = S3Oled::new(panel, s3_oled::scaled_buffer());
/// display.init().ok();
///
/// // Backlight LAST — see `S3Oled::flush`'s note on first-paint ordering.
/// let mut backlight = Output::new(
///     peripherals.GPIO45, Level::Low, OutputConfig::default(),
/// );
/// // …draw the first frame, THEN:
/// backlight.set_high();
/// ```
///
/// # Three things the intake PR must decide, which this crate deliberately does not
///
/// 1. **Feature name and exclusivity.** `hostsim`/`cast` already exclude each other via
///    `#[cfg(all(feature = "hw", not(feature = "cast")))]`; a fourth arm needs the same
///    treatment, and the combinatorics are smol-d8's call.
/// 2. **Where `scaled_buffer()`'s static lives.** 92,160 B must not land on a task stack —
///    see [`s3_oled::scaled_buffer`]. smol's `.bss`/stack-gap situation is the constraint
///    (`stack is not headroom`), and only the intake PR can see the whole budget.
/// 3. **Whether `Ctx` still holds the display concretely.** `app.rs:28-33` explains it does
///    so because `flush()` lives on the panel type rather than on `DrawTarget`. That
///    reasoning holds here unchanged — [`S3Oled`] has an inherent `flush()` too — so the
///    expected answer is "yes, no change", but it should be checked, not assumed.
pub const INTEGRATION_SKETCH: () = ();
