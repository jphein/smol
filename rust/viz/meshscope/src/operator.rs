//! Operator mode (#23) — the sanctioned single exception to meshscope's listener-only
//! invariant (spec §3). Constructed ONLY when `--operator` is passed; without it, none
//! of this exists and meshscope is a pure listener.
//!
//! Safety is structural, not disciplinary:
//! - Every command is a [`PublishReq`] whose [`Retain`] is fixed by its *builder*. The
//!   transient commands (`cmd/reset`, `cmd/scan`) are built with [`Retain::Transient`]
//!   and there is NO builder that makes them retained — a retained `cmd/reset` is a
//!   permanent ~10 s reboot-loop soft-brick, so this is enforced by construction and a
//!   unit test ([`tests::transient_commands_never_retained`]).
//! - [`PublishReq::destructive`] routes an action through a confirmation modal that shows
//!   the exact topic + payload + retain before anything leaves the process.
//!
//! The publish path is meshscope-LOCAL: its own `rumqttc` client with a distinct
//! client-id (`<listener-id>-op`), so it coexists with the listener without the
//! same-id reconnect war. The shared `mesh-model` crate is never touched.

use std::thread;
use std::time::Duration;

use rumqttc::{Client, MqttOptions, QoS};

use mesh_model::mqtt::BrokerCfg;

/// Retain flag for a publish — a 2-variant type so a command's retention is a
/// deliberate, greppable choice, never a bare bool that can be flipped by accident.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retain {
    /// `config/*`, `ota/install` — the leaf must re-read it after a reboot.
    Retained,
    /// `cmd/reset`, `cmd/scan` — a one-shot command. **MUST NOT be retained**
    /// (a retained reset re-fires every ~10 s → soft-brick).
    Transient,
}

impl Retain {
    pub fn as_bool(self) -> bool {
        matches!(self, Retain::Retained)
    }
}

/// A fully-formed publish request. Built by the typed builders below so topic/payload/
/// retain are decided together and can't drift apart.
#[derive(Clone, Debug)]
pub struct PublishReq {
    pub topic: String,
    pub payload: Vec<u8>,
    pub retain: Retain,
    /// Human summary shown in the confirmation modal + the "last published" line.
    pub label: String,
    /// Requires a confirmation modal before it goes out (reboot / broker / ota_host /
    /// channel_hint / any fleet-wide action).
    pub destructive: bool,
}

impl PublishReq {
    fn retained(topic: String, payload: impl Into<Vec<u8>>, label: String, destructive: bool) -> Self {
        PublishReq { topic, payload: payload.into(), retain: Retain::Retained, label, destructive }
    }
    /// The ONLY constructor for a transient command — retain is Transient, full stop.
    fn transient(topic: String, label: String, destructive: bool) -> Self {
        // Payload is presence-triggered (the fw fires on receipt, ignores the value); "1"
        // is a clear, non-empty marker.
        PublishReq { topic, payload: b"1".to_vec(), retain: Retain::Transient, label, destructive }
    }

    pub fn summary(&self) -> String {
        let val = String::from_utf8_lossy(&self.payload);
        let r = if self.retain.as_bool() { "retained" } else { "transient" };
        format!("{}  →  {} = \"{}\"  [{}]", self.label, self.topic, val, r)
    }
}

// --- Typed control builders (the whole command surface, verified vs net/wifi.rs) -------
// Per-node -----------------------------------------------------------------------------

/// Arm an OTA install (idempotent; fw refuses `staged.build <= running`). "Install" verb
/// matches HA's Update entity (parity).
pub fn install(id: u8) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/ota/install"), b"INSTALL".to_vec(), format!("Install update on id{id}"), false)
}
pub fn default_screen(id: u8, appkind: &str, page: u8) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/default_screen"), format!("{appkind}:{page}"), format!("Default screen id{id} → {appkind}:{page}"), false)
}
pub fn led(id: u8, mode: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/led"), mode.as_bytes().to_vec(), format!("LED id{id} → {mode}"), false)
}
/// Plugin-visibility mask. The fw parses `config/plugins` as **up-to-4 ASCII-hex chars
/// → u16** (`menu.rs`), so emit hex, NOT decimal.
pub fn plugins(id: u8, mask: u16) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/plugins"), format!("{mask:x}"), format!("Plugins id{id} → 0x{mask:x}"), false)
}
pub fn custom(id: u8, compose: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/custom"), compose.as_bytes().to_vec(), format!("Custom screen id{id}"), false)
}
pub fn io_map(id: u8, descriptor: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/io"), descriptor.as_bytes().to_vec(), format!("IO pin-map id{id} → {descriptor}"), false)
}
pub fn io_set(id: u8, states: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/io/set"), states.as_bytes().to_vec(), format!("IO output id{id} → {states}"), false)
}
/// Broker override — reboots the leaf + can strand it → destructive (confirm).
pub fn broker(id: u8, ipv4_port: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/broker"), ipv4_port.as_bytes().to_vec(), format!("Broker override id{id} → {ipv4_port}"), true)
}
/// OTA-host override — strand risk → destructive (confirm).
pub fn ota_host(id: u8, host: &str) -> PublishReq {
    PublishReq::retained(format!("smol/{id}/config/ota_host"), host.as_bytes().to_vec(), format!("OTA host id{id} → {host}"), true)
}
/// Reboot — TRANSIENT (retain=false, enforced) + destructive (confirm).
pub fn reboot(id: u8) -> PublishReq {
    PublishReq::transient(format!("smol/{id}/cmd/reset"), format!("Reboot id{id}"), true)
}
/// On-demand WiFi scan — TRANSIENT (retain=false, enforced); not destructive.
pub fn scan(id: u8) -> PublishReq {
    PublishReq::transient(format!("smol/{id}/cmd/scan"), format!("WiFi scan id{id}"), false)
}

// Fleet-wide (v1: units + channel_hint ONLY — team-lead ruling) ------------------------

/// Fleet display units. `token` e.g. "°F 12h" — align to the HA dashboard's exact vocab.
pub fn units(token: &str) -> PublishReq {
    PublishReq::retained("smol/config/units".to_string(), token.as_bytes().to_vec(), format!("FLEET units → {token}"), true)
}
/// Crown channel-steer (#155). A decimal channel; empty payload clears the hint.
pub fn channel_hint(ch: Option<u8>) -> PublishReq {
    let (payload, label) = match ch {
        Some(c) => (c.to_string().into_bytes(), format!("FLEET channel hint → ch {c}")),
        None => (Vec::new(), "FLEET channel hint → CLEAR".to_string()),
    };
    PublishReq::retained("smol/mesh/channel_hint".to_string(), payload, label, true)
}

// --- The publisher (own rumqttc client, distinct id) ----------------------------------

/// Owns the operator MQTT client + its drain thread. `send` is fire-and-forget QoS 0.
pub struct Publisher {
    client: Client,
}

impl Publisher {
    /// Build from the listener's broker cfg, deriving a distinct `<id>-op` client id so
    /// the operator connection never collides with the listener's.
    pub fn new(base: &BrokerCfg) -> Self {
        let client_id = format!("{}-op", base.client_id);
        let mut opts = MqttOptions::new(client_id, &base.host, base.port);
        opts.set_keep_alive(Duration::from_secs(30));
        if !base.user.is_empty() {
            opts.set_credentials(&base.user, &base.pass);
        }
        let (client, mut connection) = Client::new(opts, 16);
        // Drain the eventloop so queued publishes actually flush + it auto-reconnects.
        thread::spawn(move || {
            for _ in connection.iter() {}
        });
        Publisher { client }
    }

    /// Publish a request. Retain comes from the typed request — transient commands are
    /// structurally retain=false. Errors are swallowed (rumqttc requeues/reconnects).
    pub fn send(&self, req: &PublishReq) {
        let _ = self
            .client
            .publish(&req.topic, QoS::AtMostOnce, req.retain.as_bool(), req.payload.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_commands_never_retained() {
        // The soft-brick guard: reset + scan MUST be retain=false, always.
        assert!(!reboot(7).retain.as_bool(), "cmd/reset must be transient (retained = reboot-loop brick)");
        assert!(!scan(7).retain.as_bool(), "cmd/scan must be transient");
        // And every config/* + install is retained (survives a leaf reboot).
        for r in [
            install(7),
            led(7, "on"),
            default_screen(7, "Clock", 0),
            plugins(7, 0b101),
            custom(7, "x"),
            io_map(7, "0L"),
            io_set(7, "0=1"),
            broker(7, "192.0.2.10:1883"),
            ota_host(7, "192.0.2.11"),
            units("C|24"),
            channel_hint(Some(6)),
            channel_hint(None),
        ] {
            assert!(r.retain.as_bool(), "{} must be retained", r.topic);
        }
    }

    #[test]
    fn destructive_flags() {
        // Confirm-gated: reboot, broker, ota_host, fleet units, channel_hint.
        assert!(reboot(7).destructive);
        assert!(broker(7, "192.0.2.10:1883").destructive);
        assert!(ota_host(7, "192.0.2.11").destructive);
        assert!(units("C|24").destructive);
        assert!(channel_hint(Some(6)).destructive);
        // Not confirm-gated: install (idempotent), led, screen, scan.
        assert!(!install(7).destructive);
        assert!(!led(7, "on").destructive);
        assert!(!scan(7).destructive);
    }

    #[test]
    fn channel_hint_clear_is_empty() {
        assert!(channel_hint(None).payload.is_empty());
        assert_eq!(channel_hint(Some(11)).payload, b"11");
    }

    #[test]
    fn topics_are_wellformed() {
        assert_eq!(reboot(9).topic, "smol/9/cmd/reset");
        assert_eq!(scan(9).topic, "smol/9/cmd/scan");
        assert_eq!(install(9).topic, "smol/9/ota/install");
        assert_eq!(led(9, "off").topic, "smol/9/config/led");
        assert_eq!(units("C|24").topic, "smol/config/units");
        // #25: the units wire token is PIPE-separated (fw `from_wire` split('|')) — a joined
        // "C24" would silently no-op on the board. Lock the pipe form against regression.
        assert_eq!(units("F|24").payload, b"F|24");
        assert_eq!(channel_hint(Some(6)).topic, "smol/mesh/channel_hint");
    }
}
