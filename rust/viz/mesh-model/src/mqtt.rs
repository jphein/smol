//! The MQTT listener thread. Owns a blocking `rumqttc` connection, subscribes
//! `smol/#` (+ HA discovery for realm nouns), and folds every publish into the
//! shared [`Model`]. rumqttc's event loop auto-reconnects; we re-subscribe on each
//! CONNACK (subscriptions don't survive a reconnect).
//!
//! Lifted from meshscope @ 81ebb14. The ONE change vs that source: `BrokerCfg` now
//! carries a `client_id`, used verbatim as the MQTT client identifier. Two clients
//! that connect with the SAME id fight over the connection — the broker kicks the
//! older session on each new CONNECT, so meshscope and observatory running at once
//! would flap in a reconnect war. Each frontend now picks a distinct id
//! (`meshscope` / `observatory`).

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rumqttc::{Client, Event, MqttOptions, Packet, QoS};

use crate::model::{ConnState, Model};

#[derive(Clone, Debug)]
pub struct BrokerCfg {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    /// MQTT client identifier. MUST be unique per live connection to the broker —
    /// a duplicate id causes the broker to disconnect the older session.
    pub client_id: String,
}

impl BrokerCfg {
    /// Build from SMOL_MQTT_* env (after an optional .env load). Host accepts
    /// `host` or `host:port`; SMOL_MQTT_PORT is the fallback port. `client_id`
    /// defaults to `"mesh-model"`; callers SHOULD override it per-frontend via
    /// [`with_client_id`](Self::with_client_id).
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var("SMOL_MQTT_HOST")
            .map_err(|_| "SMOL_MQTT_HOST is not set (see .env.example)".to_string())?;
        let env_port = std::env::var("SMOL_MQTT_PORT").ok().and_then(|s| s.parse().ok());
        let (host, port) = match raw.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap()),
            _ => (raw, env_port.unwrap_or(1883)),
        };
        Ok(BrokerCfg {
            host,
            port,
            user: std::env::var("SMOL_MQTT_USER").unwrap_or_default(),
            pass: std::env::var("SMOL_MQTT_PASS").unwrap_or_default(),
            client_id: "mesh-model".to_string(),
        })
    }

    /// Set the MQTT client id (chainable). Use a name unique to the frontend so two
    /// listeners can share one broker (`"meshscope"`, `"observatory"`).
    pub fn with_client_id(mut self, id: impl Into<String>) -> Self {
        // An explicit SMOL_MQTT_CLIENT_ID always wins (e.g. to run two of the same tool
        // at once); otherwise the frontend's own name is used.
        self.client_id = std::env::var("SMOL_MQTT_CLIENT_ID").unwrap_or_else(|_| id.into());
        self
    }

    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Spawn the listener. `start` is the shared monotonic origin (also used by the UI
/// for ages) so every timestamp shares one clock.
pub fn spawn(model: Arc<Mutex<Model>>, cfg: BrokerCfg, start: Instant) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut opts = MqttOptions::new(&cfg.client_id, &cfg.host, cfg.port);
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_max_packet_size(256 * 1024, 256 * 1024); // screen BMPs + big diag
        if !cfg.user.is_empty() {
            opts.set_credentials(&cfg.user, &cfg.pass);
        }
        let (client, mut connection) = Client::new(opts, 64);

        for notification in connection.iter() {
            let now = start.elapsed().as_secs_f64();
            match notification {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    // (Re)subscribe on every connect — rumqttc doesn't replay them.
                    let _ = client.subscribe("smol/#", QoS::AtMostOnce);
                    let _ = client.subscribe("homeassistant/+/+/config", QoS::AtMostOnce);
                    if let Ok(mut m) = model.lock() {
                        m.conn = ConnState::Connected;
                    }
                }
                Ok(Event::Incoming(Packet::Publish(p))) => {
                    if let Ok(mut m) = model.lock() {
                        m.ingest(now, &p.topic, &p.payload);
                    }
                }
                Ok(_) => {}
                Err(_e) => {
                    if let Ok(mut m) = model.lock() {
                        m.conn = ConnState::Error;
                    }
                    // rumqttc reconnects on its own cadence; avoid a hot spin.
                    thread::sleep(Duration::from_millis(750));
                }
            }
        }
    })
}
