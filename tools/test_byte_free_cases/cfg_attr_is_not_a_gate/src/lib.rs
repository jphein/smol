// Regression: the checker's first draft read `cfg_attr` as a gate. `widget` is gated on
// `alpha` by its `mod` declaration; the `cfg_attr` inside widget.rs conditions an ATTRIBUTE
// and must not be mistaken for the gate, or the two are reported as disagreeing.
#[cfg(feature = "alpha")]
pub mod widget;
