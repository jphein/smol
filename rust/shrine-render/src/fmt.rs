//! Fixed-capacity text, replacing `format!` on a target with no allocator.
//!
//! Every string this renderer builds is a short readout — an attack/health pair, a prompt, a
//! castle name and life. `Text` is sized for the longest of them (the prompt band, which
//! `battlefield::prompt_budget_chars` caps well below this) and writing past it truncates rather
//! than panicking, which is the right failure on a device: a clipped readout is recoverable and
//! a panic in the draw loop is not.

use core::fmt::Write;

/// A readout buffer. 64 bytes covers every string the renderer builds, with headroom.
pub type Text = heapless::String<64>;

/// Build a `Text` from format arguments, truncating rather than panicking if it overflows.
pub fn text(args: core::fmt::Arguments<'_>) -> Text {
    let mut s = Text::new();
    // `write_fmt` on a full `heapless::String` returns Err; the partial content is kept, which is
    // the behaviour we want - show what fits.
    let _ = s.write_fmt(args);
    s
}

/// `format!` for this crate: same syntax, fixed capacity, no allocator.
#[macro_export]
macro_rules! txt {
    ($($t:tt)*) => { $crate::fmt::text(format_args!($($t)*)) };
}
