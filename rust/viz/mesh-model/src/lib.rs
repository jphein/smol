//! mesh-model — the shared core behind both smol mesh visualizers.
//!
//! A pure MQTT *listener* (never a publisher): it subscribes `smol/#`, parses the
//! wire formats, and folds every retained/live payload into a [`Model`] — the same
//! world state that meshscope renders as an instrument and observatory renders as a
//! constellation. A bug fixed here fixes both faces; a new telemetry field lights up
//! both.
//!
//! ```no_run
//! use std::sync::{Arc, Mutex};
//! use std::time::Instant;
//! use mesh_model::{mqtt, model::Model};
//!
//! let start = Instant::now();
//! let cfg = mqtt::BrokerCfg::from_env().unwrap().with_client_id("observatory");
//! let model = Arc::new(Mutex::new(Model::new(cfg.endpoint())));
//! mqtt::spawn(model.clone(), cfg, start);
//! // ... render loop reads `model.lock()` each frame ...
//! ```

pub mod model;
pub mod mqtt;
pub mod names;
pub mod parse;

// Ergonomic re-exports so frontends can `use mesh_model::{Model, Node, Edge, ...}`.
pub use model::{ConnState, Edge, Event, EventKind, Model, Node};
pub use mqtt::BrokerCfg;
