//! The smol realm-fantasy palette for observatory.
//!
//! Colours are authored in *linear* space with values that intentionally exceed 1.0
//! — the HDR camera + bloom turn those over-bright cores into glowing halos. A value
//! of ~1.0 reads as "lit"; 3–8 reads as "radiant". Muted structural elements (the
//! void, dim filaments) sit below 1.0.

use bevy::prelude::*;
use mesh_model::model::SyncFreshness;

/// NTP-sync freshness aura colour (parity with meshscope's dots): jade fresh → amber
/// aging → infernal red stale → cold grey unsynced. HDR-bright so it blooms.
pub fn sync_color(f: SyncFreshness) -> Color {
    match f {
        SyncFreshness::Fresh => Color::linear_rgb(0.1, 1.7, 0.5),
        SyncFreshness::Aging => Color::linear_rgb(2.6, 1.7, 0.2),
        SyncFreshness::Stale => Color::linear_rgb(3.2, 0.4, 0.3),
        SyncFreshness::Unsynced => Color::linear_rgb(0.35, 0.38, 0.5),
    }
}

/// The crown's ring colour as its dead-downstream health degrades (#204 `cdeaf`):
/// steady gold when healthy (`sick`=0) → infernal red as the deaf streak climbs
/// (`sick`→1), the whole thing scaled by `brightness` (the caller flickers/dims it).
pub fn crown_health(sick: f32, brightness: f32) -> Color {
    let s = sick.clamp(0.0, 1.0);
    let b = brightness.clamp(0.0, 1.0);
    Color::linear_rgb(lerp(6.0, 5.2, s) * b, lerp(3.4, 0.35, s) * b, lerp(0.5, 0.2, s) * b)
}

/// Deep arcane void — the field the constellation floats in. A near-black indigo so
/// bloom has darkness to bloom against. This is the base clear colour at channel 6;
/// [`channel_tint`] warps it as the mesh channel shifts. (A fn, not a const — Bevy
/// 0.14's colour constructors are not `const`.)
pub fn void() -> Color {
    Color::linear_rgb(0.006, 0.008, 0.020)
}

/// Crown gold — the reigning gateway's halo + the election comet.
pub fn crown_gold() -> Color {
    Color::linear_rgb(6.0, 3.4, 0.5)
}

/// A leaf node's core glow — luminous cool cyan-white.
pub fn leaf_core() -> Color {
    Color::linear_rgb(1.2, 2.4, 3.6)
}

/// A gateway (non-crown, if the fleet ever had two) core — warmer.
pub fn gateway_core() -> Color {
    Color::linear_rgb(2.2, 2.6, 3.2)
}

/// The crowned gateway's core — bright regal gold-white.
pub fn crown_core() -> Color {
    Color::linear_rgb(4.5, 3.6, 1.6)
}

/// A stale/aged node — dim, desaturated, ghostly. Below 1.0 so it does not bloom.
pub fn stale_core() -> Color {
    Color::linear_rgb(0.16, 0.18, 0.24)
}

/// OTA particle stream colour — an electric spellcaster cyan.
pub fn ota_stream() -> Color {
    Color::linear_rgb(0.6, 4.5, 5.5)
}

/// OTA completion burst — a bright mana-white flash.
pub fn ota_burst() -> Color {
    Color::linear_rgb(5.0, 6.0, 6.5)
}

/// Map an RSSI (dBm, ~ -30 strong .. -95 weak) to a filament colour. Strong links
/// glow a healthy jade; weak links fade to a dim infernal amber-red. Brightness (not
/// width) encodes strength, so gizmo lines read as "strong = radiant".
pub fn rssi_color(rssi: i32, alpha: f32) -> Color {
    let t = ((rssi as f32 + 90.0) / 55.0).clamp(0.0, 1.0); // 0 weak .. 1 strong
    // weak (amber-red, dim) -> strong (jade, bright)
    let r = lerp(2.2, 0.4, t);
    let g = lerp(0.7, 3.2, t) * alpha;
    let b = lerp(0.35, 1.6, t) * alpha;
    Color::linear_rgba(r * alpha.max(0.2), g, b, alpha)
}

/// A rough dBm→"bars" strength in [0,1] for sizing/particle cadence.
pub fn rssi_strength(rssi: i32) -> f32 {
    ((rssi as f32 + 90.0) / 55.0).clamp(0.0, 1.0)
}

/// Warp the void colour by the active mesh channel. smol hops channels (6, 11, 1…)
/// and observatory answers with a colour-temperature shift of the whole field: low
/// channels cool/indigo, high channels warm/violet. Purely cosmetic — a felt cue
/// that "the whole mesh just moved".
pub fn channel_tint(channel: u8) -> Color {
    // Map common 2.4GHz channels (1..13) onto a hue-ish temperature ramp.
    let t = ((channel.clamp(1, 13) as f32 - 1.0) / 12.0).clamp(0.0, 1.0);
    let r = lerp(0.006, 0.028, t);
    let g = lerp(0.010, 0.006, t);
    let b = lerp(0.022, 0.030, t);
    Color::linear_rgb(r, g, b)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
