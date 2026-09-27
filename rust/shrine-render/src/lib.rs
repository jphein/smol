//! The Tapstone shrine renderer.
//!
//! Draws the shrine's screens at exactly 320×240 RGB565 through `embedded-graphics`, into any
//! `DrawTarget`. **`no_std` and allocation-free**, so it can be vendored into smol the way
//! `tapstone-rules` is: byte-identical `src/`, a `VENDOR.sha256`, fixes upstream first.
//!
//! # Why this crate is separate from `shrine-preview`
//!
//! The preview crate claimed to be "a prototype of the shrine renderer" while using `format!`,
//! `String` and `Vec` throughout — 37 alloc-dependent lines across the drawing core. On a
//! thumbv7em target with no allocator none of it compiles, so the claim was false and nobody
//! would have found out until someone tried to vendor it and failed.
//!
//! The split makes the claim checkable rather than asserted. Everything here builds for
//! `thumbv7em-none-eabi` in CI, exactly as the rules crate does. Everything that needs a host —
//! the CLI, PNG export, the simulator, driving `tapstone-sim` for real game states — lives in
//! `shrine-preview` and never crosses back.
//!
//! # Drawing model
//!
//! Nothing here allocates or owns a framebuffer. Every entry point takes `&mut D` where
//! `D: DrawTarget<Color = Rgb565>` and draws into it, which is the shape firmware needs: the
//! device owns its buffer and the renderer only writes to it.
//!
//! # The panel this targets
//!
//! ILI9341V 320×240 RGB565, driven landscape, write-only SPI at 40 MHz. A full repaint measures
//! ≈29 ms and per-cell SPI windows measure 2× that, which shapes what can animate — see `cost`.
//! The shrine **rasterises locally**; decision 0010's amendment makes that a requirement rather
//! than a preference, because the design has no server at the table.

#![no_std]
#![forbid(unsafe_code)]

pub mod band;
pub mod battlefield;
pub mod commander;
pub mod cost;
pub mod draws;
pub mod flourish;
pub mod fmt;
pub mod geom;
pub mod ink;
pub mod motion;
pub mod palette;
pub mod screens;
pub mod sprite;
pub mod station;
pub mod voice;
