//! #43 display units — fleet-global temperature (°F/°C) + clock (12h/24h) config.
//!
//! Unlike the per-node screen (#21) / LED (#48) / plugin-mask (#55) channels, display
//! units are ONE fleet-wide setting (`smol/config/units`, no id in the topic). The gateway
//! reads its own copy directly from MQTT and RELAYS it to every leaf over the keyed CFG
//! channel under the broadcast target [`CFG_TARGET_ALL`](crate::net::CFG_TARGET_ALL) (255) —
//! one cached `(255, 'U')` entry fans out to all leaves, and a leaf applies a CFG frame
//! whose target is its own id OR the broadcast sentinel.
//!
//! The struct + render usage compile in EVERY profile (the CLOCK screen is universal);
//! only espnow nodes actually receive a config (the relay/apply path is radio-only, exactly
//! like the screen/LED channels). A non-espnow build shows the [`Units::default`] units.

/// #43 (key `U`, GLOBAL): the fleet-wide display units. Wire form `<F|C>|<24|12>`
/// (e.g. `F|24`, `C|12`) — mirrors the HA `smol/config/units` retained payload.
///
/// Defaults (`°F`, 24h) match the HA input_select `initial:` values, so a node with a live
/// config and a node without one agree — no jarring 12h→24h flip when the first config lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Units {
    /// `true` = temperature in °F (default), `false` = °C.
    pub temp_f: bool,
    /// `true` = 24-hour clock (default, `HH:MM`), `false` = 12-hour (`H:MM` + AM/PM).
    pub clk_24h: bool,
}

impl Default for Units {
    fn default() -> Self {
        // °F + 24h. Matches the HA input_select `initial:` values (smol_mesh.yaml #43),
        // so the FW no-config default equals the fleet standard the gateway relays.
        Self { temp_f: true, clk_24h: true }
    }
}

impl Units {
    /// Parse a `smol/config/units` payload `<F|C>|<24|12>` (e.g. `F|24`, `C|12`). ANY
    /// unrecognised / malformed token → `None`, so the caller KEEPS its current units
    /// rather than half-applying a garbage frame (untrusted retained/relayed value — the
    /// #46 clamp discipline; never panics). Whitespace-tolerant; empty → `None`.
    pub fn from_wire(s: &str) -> Option<Units> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut it = s.split('|');
        let temp_f = match it.next().unwrap_or("").trim() {
            "F" => true,
            "C" => false,
            _ => return None,
        };
        let clk_24h = match it.next().unwrap_or("").trim() {
            "24" => true,
            "12" => false,
            _ => return None,
        };
        Some(Units { temp_f, clk_24h })
    }
}
