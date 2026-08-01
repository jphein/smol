//! #152 host-emulator library boundary.
//!
//! `smol` ships as a single `no_std` esp-hal BINARY (`main.rs`). This library target
//! exposes ONLY the pure game/render cores — the same `snake.rs` / `clock.rs` / `app.rs`
//! source the firmware compiles, no fork — so a host/wasm crate (`web-emu`) can run them
//! in a browser. It is compiled ONLY under `--features hostsim` (which excludes the `hw`
//! bare-metal crates); under any firmware build this file is an empty `no_std` lib and the
//! BINARY is what builds.
//!
//! The boundary audit (issue #152, "half the value") loosened exactly three couplings,
//! all cfg-gated + behavior-preserving for the firmware:
//!   * `app::Oled` — a third concrete display type (`hostsim::CanvasOled`) beside the
//!     existing plain/`cast` panels; plugins draw through the same `DrawTarget` + `flush()`.
//!   * `sensors::Sensors` — a host stub returning a canned `Reading` (no ADC/tsens).
//!   * `input::Button` / `app::App` dispatch union — gated OUT of the host lib (the
//!     emulator drives the `Plugin`s directly and synthesizes `Press` from the keyboard).

#![no_std]

// #348 per-chip memory budgets. The ONE module here that is NOT host-only: it is pure data
// plus const-eval assertions (no code emitted), and compiling it in both targets means the
// budget predicates are checked by a plain `cargo check` of either. It is also what lets
// `tests/budget.rs` assert the arithmetic host-side without a cross-compile.
// ⚠️ Moves to `smol-core` (#347 Phase 2) — see the module docs.
pub mod budget;

// Everything below is host-only. Under a firmware build (no `hostsim`) this lib is empty
// and `main.rs` (the bin) carries the real modules.
#[cfg(feature = "hostsim")]
mod host {
    //! Crate-root shims the shared modules reach via `crate::…`. In the BINARY these live
    //! in `main.rs`; the host lib re-provides the two the pure cores actually read.

    /// Fixed timezone offset (mirrors `main.rs`'s `TZ_OFFSET_SECONDS`). The emulator's
    /// clock renders `(unix_now + TZ) mod 86_400`; the host can override `unix_now` live.
    pub const TZ_OFFSET_SECONDS: i64 = -7 * 3600;

    /// This node's id for name derivation (`clock.rs` → `net::names::name_for_id`). A
    /// fixed demo id in the emulator (no NVS/board identity host-side).
    pub(crate) fn node_id() -> u8 {
        1
    }
}

// Re-export the shims at the crate root so `crate::TZ_OFFSET_SECONDS` / `crate::node_id()`
// resolve exactly as they do in the binary (the shared source files are path-agnostic).
#[cfg(feature = "hostsim")]
pub use host::TZ_OFFSET_SECONDS;
#[cfg(feature = "hostsim")]
pub(crate) use host::node_id;

// --- the REAL, unforked pure cores -------------------------------------------------
#[cfg(feature = "hostsim")]
pub mod app;
#[cfg(feature = "hostsim")]
pub mod clock;
#[cfg(feature = "hostsim")]
pub mod input;
// #300 The Bard's tiny-LLM core. Lives in `src/bard/` (its own dir, room for the
// tokenizer/model siblings) but is a plain pure core like the rest — no `hw`, no radio.
#[cfg(feature = "hostsim")]
#[path = "bard/nano_llm.rs"]
pub mod nano_llm;
// The bard's siblings, exported under their PLAIN names. That is load-bearing: inside
// `src/bard/` the three files reach each other as `super::nano_llm` / `super::tokenizer`, which
// resolves to `crate::bard::*` in the firmware and to these crate-root modules here — one
// spelling that compiles under both roots, with no cfg juggling in the shared source.
#[cfg(feature = "hostsim")]
#[path = "bard/tokenizer.rs"]
pub mod tokenizer;
#[cfg(feature = "hostsim")]
#[path = "bard/persona.rs"]
pub mod persona;
#[cfg(feature = "hostsim")]
#[path = "bard/textflow.rs"]
pub mod textflow;
// #302 the delivery setting (reveal pace + inf/page mode) and its strict CFG-`V` parser. Pure, so
// the parser's refusals are host-tested rather than discovered over the air.
#[cfg(feature = "hostsim")]
#[path = "bard/delivery.rs"]
pub mod delivery;
// Only the pure sentinel scanner compiles here; the paint itself is device-only (linker symbols).
#[cfg(feature = "hostsim")]
#[path = "bard/stack_paint.rs"]
pub mod stack_paint;
#[cfg(feature = "hostsim")]
pub mod sensors;
#[cfg(feature = "hostsim")]
pub mod snake;
#[cfg(feature = "hostsim")]
pub mod units;

// `clock.rs` derives the bottom-line label from the REAL name table. Pull ONLY `names`
// (via its real path) — not `net.rs`'s radio submodules — so the host closure stays pure.
#[cfg(feature = "hostsim")]
pub mod net {
    // Path is resolved relative to `src/net/` (the inline module's implied dir).
    #[path = "names.rs"]
    pub mod names;
}

// --- host display back-end ---------------------------------------------------------
#[cfg(feature = "hostsim")]
pub mod hostsim {
    //! A 72×40, 1-bit canvas display: the concrete `app::Oled` under `feature = "hostsim"`.
    //! It impls the SAME `DrawTarget<Color = BinaryColor>` + inherent `flush()`/`init()`
    //! surface the OLED offers, so `snake.rs` / `clock.rs` draw into it UNCHANGED. The
    //! backing framebuffer is a flat `[u8; 72*40]` (1 = lit) that the wasm host blits to a
    //! `<canvas>`; `flush()` is a no-op (the host reads the buffer every frame).

    use embedded_graphics::pixelcolor::BinaryColor;
    use embedded_graphics::prelude::*;

    /// Panel width in pixels (the 0.42" SSD1306 the firmware drives).
    pub const WIDTH: usize = 72;
    /// Panel height in pixels.
    pub const HEIGHT: usize = 40;

    /// Canvas-backed 1-bit display. `fb[y*WIDTH + x]` = 1 when the pixel is lit.
    pub struct CanvasOled {
        fb: [u8; WIDTH * HEIGHT],
    }

    impl Default for CanvasOled {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CanvasOled {
        pub fn new() -> Self {
            Self {
                fb: [0u8; WIDTH * HEIGHT],
            }
        }

        /// No-op on the host (mirrors the panel's `init()` so any boot call is harmless).
        pub fn init(&mut self) -> Result<(), core::convert::Infallible> {
            Ok(())
        }

        /// No-op: the host reads [`framebuffer`](Self::framebuffer) every frame, so there is
        /// no separate GDDRAM to push. Present so plugins can call `display.flush()` as-is.
        pub fn flush(&mut self) -> Result<(), core::convert::Infallible> {
            Ok(())
        }

        /// The lit/unlit byte grid (row-major, `WIDTH*HEIGHT`), for the wasm host to blit.
        pub fn framebuffer(&self) -> &[u8] {
            &self.fb
        }
    }

    impl OriginDimensions for CanvasOled {
        fn size(&self) -> Size {
            Size::new(WIDTH as u32, HEIGHT as u32)
        }
    }

    impl DrawTarget for CanvasOled {
        type Color = BinaryColor;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(coord, color) in pixels {
                if coord.x >= 0
                    && coord.y >= 0
                    && (coord.x as usize) < WIDTH
                    && (coord.y as usize) < HEIGHT
                {
                    let idx = coord.y as usize * WIDTH + coord.x as usize;
                    self.fb[idx] = matches!(color, BinaryColor::On) as u8;
                }
            }
            Ok(())
        }
    }
}
