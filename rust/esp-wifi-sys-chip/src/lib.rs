//! The per-chip `esp-wifi-sys` PACKAGE alias, isolated so that nothing else in smol has to know
//! there is more than one of them.
//!
//! # What this crate is for
//!
//! `net.rs` needs three raw esp-idf items for the #141 TX-power clamp and the AP-info readback:
//! `esp_wifi_set_max_tx_power`, `esp_wifi_sta_get_ap_info` and `wifi_ap_record_t`. `esp-radio`'s
//! own `sys` module is `pub(crate)`, so they have to come from the bindings crate directly.
//!
//! And the bindings crate is **not one crate with a chip feature**. It is four separate packages —
//! `esp-wifi-sys-esp32c3`, `-esp32c5`, `-esp32c6`, `-esp32s3` — which is a different problem from
//! every other dependency in the de-pin. `esp-hal`, `esp-radio`, `esp-storage` and friends are ONE
//! package with a chip FEATURE, so smol's chip feature can just forward to them
//! (`esp-hal?/esp32c3`). A `package = ` alias cannot be made conditional: cargo resolves the
//! manifest before it knows anything about features.
//!
//! # Why a shim and not four keys in `clock`
//!
//! Four keys in `clock/Cargo.toml` would work only if the chip feature could enable one of them,
//! and that is precisely what it must NOT do — because of a second, sharper constraint:
//!
//! > **The chip is chosen independently of whether the radio is on at all.**
//!
//! smol's `default` tier is `hw` only: no `esp-radio`, no `esp-rtos`, no OTA crates, and therefore
//! no bindings. But a `default` build still has to name a chip (it is compiled for a C3). If the
//! chip feature said `dep:esp-wifi-sys-esp32c3`, the default tier would start linking the
//! Espressif blob archives that crate's build script emits — a build that today links none of
//! them. That is not a subtle regression: it is the #348 byte-stability constraint failing on the
//! one tier the whole de-pin promised not to touch.
//!
//! So what is needed is the **conjunction** `chip AND radio`, and cargo's feature language has no
//! `AND`. What it does have is the weak dependency-feature form `dep?/feature`, meaning *"if that
//! optional dependency ends up enabled, turn this feature on inside it"*. Routing the choice
//! through one optional shim turns the impossible conjunction into that form:
//!
//! ```text
//!   clock's `wifi` feature   -> enables the shim          (the RADIO half)
//!   clock's `esp32c3`        -> "esp-wifi-sys?/esp32c3"   (the CHIP half, weak: only if enabled)
//!   ------------------------------------------------------------------------------------------
//!   default tier (no wifi)   -> shim absent, no bindings, no blobs, byte-stable
//!   fleet tier on a C3       -> shim present with its esp32c3 arm -> the C3 bindings
//!   fleet tier on an S3      -> shim present with its esp32s3 arm -> the S3 bindings
//! ```
//!
//! The dependency key in `clock/Cargo.toml` is still spelled `esp-wifi-sys`, so
//! `esp_wifi_sys::include::…` in `net.rs` is **unchanged, character for character**. The de-pin
//! adds a crate to the graph and zero edits to the call sites.
//!
//! # The direction of the boundary, which is the easy thing to get backwards
//!
//! `sigil-names` sits outside `rust/clock/` because it is chip-AGNOSTIC and shared. This crate
//! sits outside for the opposite reason: it is the most chip-DEPENDENT thing in the tree. Both
//! belong outside the workspace root, and the #347 rule covers both, but it is worth saying which
//! is which — the `*-core` framing suggests "extract the pure parts", and a reader applying only
//! that half would try to fold this crate back in, where its four mutually-exclusive aliases would
//! sit inside the very feature-unification graph they exist to stay out of.

#![no_std]

// ── The selection ───────────────────────────────────────────────────────────────────────────────
//
// A glob re-export, deliberately, rather than a hand-listed surface. `net.rs` needs three items
// today, but the crate's job is "be esp-wifi-sys for this chip", not "be the three items smol
// currently calls" — a curated list would make the next raw-binding call site a change to this
// file, and this file is the one place a chip name appears, so it is the worst possible place to
// invite unrelated edits.
//
// The four arms are `cfg`-exclusive rather than `cfg_if`-chained so that enabling two chips is a
// DUPLICATE DEFINITION error naming both, instead of the higher arm silently winning. Two chip
// features on at once is not a hypothetical: it is what `cargo build --features esp32c5` does when
// `default` (which carries `esp32c3`) has not been turned off, and a silent win there would build
// C3 bindings into an image stamped C5.

#[cfg(feature = "esp32c3")]
pub use esp_wifi_sys_esp32c3::*;

#[cfg(feature = "esp32c5")]
pub use esp_wifi_sys_esp32c5::*;

#[cfg(feature = "esp32c6")]
pub use esp_wifi_sys_esp32c6::*;

#[cfg(feature = "esp32s3")]
pub use esp_wifi_sys_esp32s3::*;

// ── Fail closed ─────────────────────────────────────────────────────────────────────────────────
//
// With no chip feature this crate would compile to an EMPTY module, and the error would surface at
// `net.rs` as "could not find `include` in `esp_wifi_sys`" — which reads as a version problem in a
// vendor crate and sends the reader upstream. Naming the real cause here costs one const and saves
// that trip. Same fail-closed rule as `budget.rs`: an absent selection is a build failure, never a
// default.
#[cfg(not(any(
    feature = "esp32c3",
    feature = "esp32c5",
    feature = "esp32c6",
    feature = "esp32s3"
)))]
compile_error!(
    "esp-wifi-sys-chip: no chip feature selected, so there are no bindings to re-export. This \
     crate is pulled by smol's `wifi` feature and its chip arm is supplied by smol's chip feature \
     with the WEAK form `esp-wifi-sys?/esp32cX` (rust/clock/Cargo.toml). Reaching this message \
     means the radio half was enabled without the chip half — i.e. `wifi` is on and no chip \
     feature is, which for a firmware build should be impossible because `default` carries \
     `esp32c3`. The likely cause is `--no-default-features` with a tier but no chip: add one of \
     `esp32c3` / `esp32c5` / `esp32c6` / `esp32s3`."
);
