//! The shared world model — the **liftable `mesh-model` core**. Together with
//! `parse.rs` + `names.rs` this is a closed, UI-free / MQTT-free module set (it
//! references only `crate::parse` / `crate::names`, and no eframe/rumqttc type
//! crosses this boundary), so it lifts VERBATIM into a standalone `mesh-model` lib
//! crate that #159 (observatory) can also depend on — per
//! docs/superpowers/specs/2026-07-18-mesh-visualizers-design.md ("one model, two
//! faces"). Inlined here per team-lead direction; structured to lift.
//!
//! The MQTT thread folds every retained/live `smol/#` payload in via
//! [`Model::ingest`]; a frontend reads the state each frame. `ingest` is pure w.r.t.
//! the clock (takes `now_s`), so it is unit-testable and demo-seedable.

use std::collections::{BTreeMap, VecDeque};

use crate::names;
use crate::parse::{self, Diag, OtaState, PeerLink};

// --- Semantic thresholds — the SINGLE SOURCE OF DERIVATION TRUTH (HA parity) ---
// Every derived signal is defined ONCE here; the HA dashboard mirrors these (and
// vice-versa) — the bidirectional-parity rule in the spec. Change a threshold here
// → update HA to match.
/// A node fades to "stale" after this long without any message (~1.5 flush cycles).
pub const STALE_S: f64 = 45.0;
/// A link at or weaker than this dBm is a "weak link" (dashed in the graph).
pub const WEAK_LINK_DBM: i32 = -80;
/// Free heap at or below this many bytes is "low heap" (flagged).
pub const LOW_HEAP_B: u64 = 20_000;
/// Time-sync (DIAG `tage`) is FRESH under this many seconds since last sync (green).
pub const SYNC_FRESH_S: u64 = 300;
/// ... AGING up to here (amber); STALE beyond (red) — an NTP-stale board wants to show.
pub const SYNC_STALE_S: u64 = 3600;

const HIST_CAP: usize = 240; // ~ retained cadence * this = tens of minutes of trail
const EVENT_CAP: usize = 300;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnState {
    Connecting,
    Connected,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
    Crown,   // election / crown change
    Version, // installed build flip
    Ota,     // ota progress / fetch retries / diag
    Join,    // node first seen
}

/// A node's clock-sync freshness, derived from DIAG `tage`/`tsrc`. Both frontends
/// colour this identically (green/amber/red/grey) — one derivation, two faces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncFreshness {
    Fresh,    // synced within SYNC_FRESH_S
    Aging,    // synced, but SYNC_FRESH_S..SYNC_STALE_S ago
    Stale,    // synced longer ago than SYNC_STALE_S
    Unsynced, // tsrc=none, or no tage reported
}

#[derive(Clone, Debug)]
pub struct Event {
    pub t_s: f64,
    pub kind: EventKind,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: u8,
    pub noun: &'static str,
    pub discovery_noun: Option<String>, // from HA discovery, if present
    pub gateway: bool,
    pub channel: Option<u8>,
    pub telemetry: Option<String>,
    pub status: Option<String>, // "<screen>:<page>"
    pub uplink_rssi: Option<i32>,
    pub diag: Option<Diag>,
    pub ota: Option<OtaState>,
    pub ota_armed: bool,
    pub ota_phase: Option<String>, // latest smol/<id>/ota/diag phase (e.g. "fetch-failed retry=3")
    pub links: Vec<PeerLink>, // peers this node hears (edges out)
    pub heap_hist: VecDeque<[f64; 2]>,
    pub rssi_hist: VecDeque<[f64; 2]>,
    pub last_seen_s: f64,
}

impl Node {
    fn new(id: u8, now_s: f64) -> Self {
        Node {
            id,
            noun: names::noun_for_id(id),
            discovery_noun: None,
            gateway: false,
            channel: None,
            telemetry: None,
            status: None,
            uplink_rssi: None,
            diag: None,
            ota: None,
            ota_armed: false,
            ota_phase: None,
            links: Vec::new(),
            heap_hist: VecDeque::new(),
            rssi_hist: VecDeque::new(),
            last_seen_s: now_s,
        }
    }

    /// The label meshscope shows: the discovery noun if HA gave us one, else the
    /// vendored id->noun (always available).
    pub fn label(&self) -> &str {
        self.discovery_noun.as_deref().unwrap_or(self.noun)
    }

    /// Installed build number, if the node has published an ota/state.
    pub fn build(&self) -> Option<&str> {
        self.ota.as_ref().map(|o| o.installed.as_str()).filter(|s| !s.is_empty())
    }

    pub fn is_stale(&self, now_s: f64) -> bool {
        now_s - self.last_seen_s > STALE_S
    }

    /// The node's LIVE screen — the "familiar"/app it is showing. The AppKind from the
    /// STAT topic (`<AppKind>:<page>`), falling back to the DIAG `cfg=` default-screen
    /// echo (`cfg=<AppKind>:<page>,…`). `None` until either is seen.
    pub fn screen(&self) -> Option<String> {
        if let Some(s) = &self.status {
            let name = s.split(':').next().unwrap_or(s).trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
        self.diag.as_ref().and_then(|d| d.get("cfg")).and_then(|cfg| {
            let name = cfg.split(',').next().unwrap_or(cfg).split(':').next().unwrap_or(cfg).trim();
            (!name.is_empty()).then(|| name.to_string())
        })
    }

    /// Seconds since this node last synced its clock (DIAG `tage`).
    pub fn time_age(&self) -> Option<u64> {
        self.diag.as_ref().and_then(|d| d.u64("tage"))
    }

    /// This node's time source (DIAG `tsrc`: `ntp` / `mesh` / `none`).
    pub fn time_src(&self) -> Option<&str> {
        self.diag.as_ref().and_then(|d| d.get("tsrc"))
    }

    /// Classify clock-sync freshness from `tage`/`tsrc` (shared by both frontends).
    pub fn sync_freshness(&self) -> SyncFreshness {
        if self.time_src() == Some("none") {
            return SyncFreshness::Unsynced;
        }
        match self.time_age() {
            None => SyncFreshness::Unsynced,
            Some(a) if a < SYNC_FRESH_S => SyncFreshness::Fresh,
            Some(a) if a < SYNC_STALE_S => SyncFreshness::Aging,
            Some(_) => SyncFreshness::Stale,
        }
    }
}

fn push_hist(h: &mut VecDeque<[f64; 2]>, t: f64, v: f64) {
    h.push_back([t, v]);
    while h.len() > HIST_CAP {
        h.pop_front();
    }
}

#[derive(Debug)]
pub struct Model {
    pub nodes: BTreeMap<u8, Node>,
    pub crown: Option<parse::MeshChannel>,
    pub events: VecDeque<Event>,
    pub conn: ConnState,
    pub broker: String,
    pub batt: Option<String>, // smol/display/batt payload
    pub grid: Option<String>, // smol/display/grid payload
    pub msg_count: u64,
    pub last_msg_s: f64,
}

impl Model {
    pub fn new(broker: String) -> Self {
        Model {
            nodes: BTreeMap::new(),
            crown: None,
            events: VecDeque::new(),
            conn: ConnState::Connecting,
            broker,
            batt: None,
            grid: None,
            msg_count: 0,
            last_msg_s: 0.0,
        }
    }

    fn event(&mut self, t_s: f64, kind: EventKind, text: impl Into<String>) {
        self.events.push_back(Event { t_s, kind, text: text.into() });
        while self.events.len() > EVENT_CAP {
            self.events.pop_front();
        }
    }

    fn node_mut(&mut self, id: u8, now_s: f64) -> &mut Node {
        let is_new = !self.nodes.contains_key(&id);
        if is_new {
            let n = Node::new(id, now_s);
            let label = n.label().to_string();
            self.nodes.insert(id, n);
            self.event(now_s, EventKind::Join, format!("＋ id{id} {label} appeared"));
        }
        let n = self.nodes.get_mut(&id).unwrap();
        n.last_seen_s = now_s;
        n
    }

    /// Fold one MQTT message into the model. `payload` is raw bytes; non-UTF8 (the
    /// base64 `screen` image is UTF-8, but be defensive) is ignored for text topics.
    pub fn ingest(&mut self, now_s: f64, topic: &str, payload: &[u8]) {
        self.msg_count += 1;
        self.last_msg_s = now_s;
        let text = std::str::from_utf8(payload).ok();

        // --- exact-topic (mesh-global) shapes ---
        match topic {
            "smol/mesh/channel" => {
                if let Some(mc) = text.and_then(parse::parse_mesh_channel) {
                    let changed = self.crown.map(|c| c.owner) != Some(mc.owner);
                    if changed {
                        let noun = names::noun_for_id(mc.owner);
                        self.event(
                            now_s,
                            EventKind::Crown,
                            format!("👑 id{} {} crowned (ch {}, seq {})", mc.owner, noun, mc.channel, mc.seq),
                        );
                    }
                    self.crown = Some(mc);
                    // Mark the owner as gateway (belt-and-suspenders vs its peers role).
                    let owner = mc.owner;
                    self.node_mut(owner, now_s).gateway = true;
                }
                return;
            }
            "smol/display/batt" => {
                self.batt = text.filter(|s| !s.is_empty()).map(|s| s.to_string());
                return;
            }
            "smol/display/grid" => {
                self.grid = text.filter(|s| !s.is_empty()).map(|s| s.to_string());
                return;
            }
            _ => {}
        }

        // --- HA discovery: harvest the realm noun, ignore the rest ---
        if topic.starts_with("homeassistant/") {
            if topic.ends_with("/config") {
                if let Some(text) = text {
                    if let Some(noun) = parse::parse_discovery_noun(text) {
                        if let Some(id) = discovery_node_id(topic) {
                            self.nodes.entry(id).or_insert_with(|| Node::new(id, now_s)).discovery_noun =
                                Some(noun);
                        }
                    }
                }
            }
            return;
        }

        // --- per-node smol/<id>/<suffix> shapes ---
        let Some((id, suffix)) = parse::split_node_topic(topic) else {
            return;
        };
        let text = match text {
            Some(t) => t,
            None => return,
        };

        match suffix {
            "peers" => {
                if let Some(rec) = parse::parse_peers(text) {
                    let n = self.node_mut(id, now_s);
                    n.gateway = rec.gateway;
                    n.channel = Some(rec.channel);
                    n.links = rec.links;
                } else if text.is_empty() {
                    // Retained clear on demotion — drop this node's edges + role.
                    if let Some(n) = self.nodes.get_mut(&id) {
                        n.links.clear();
                        n.gateway = false;
                    }
                }
            }
            "status" => {
                if let Some(s) = parse::parse_status(text) {
                    self.node_mut(id, now_s).status = Some(s);
                }
            }
            "telemetry" => {
                if !text.is_empty() {
                    self.node_mut(id, now_s).telemetry = Some(text.to_string());
                }
            }
            "uplink" => {
                if let Ok(rssi) = text.trim().parse::<i32>() {
                    let n = self.node_mut(id, now_s);
                    n.uplink_rssi = Some(rssi);
                    push_hist(&mut n.rssi_hist, now_s, rssi as f64);
                }
            }
            "diag" => {
                if let Some(d) = parse::parse_diag(text) {
                    let heap = d.u64("heap").map(|h| h as f64);
                    let n = self.node_mut(id, now_s);
                    if let Some(h) = heap {
                        push_hist(&mut n.heap_hist, now_s, h);
                    }
                    n.diag = Some(d);
                }
            }
            "ota/state" => {
                if let Some(o) = parse::parse_ota_state(text) {
                    let (old_installed, old_prog) = self
                        .nodes
                        .get(&id)
                        .and_then(|n| n.ota.as_ref())
                        .map(|p| (p.installed.clone(), p.in_progress))
                        .unwrap_or_default();
                    if !old_installed.is_empty() && old_installed != o.installed {
                        self.event(now_s, EventKind::Version, format!("⬆ id{id} v{old_installed}→v{}", o.installed));
                    }
                    if o.in_progress && !old_prog {
                        self.event(now_s, EventKind::Ota, format!("⏳ id{id} installing v{}", o.latest));
                    }
                    self.node_mut(id, now_s).ota = Some(o);
                }
            }
            "ota/install" => {
                let armed = !text.is_empty();
                self.node_mut(id, now_s).ota_armed = armed;
                if armed {
                    self.event(now_s, EventKind::Ota, format!("🎯 id{id} install armed"));
                }
            }
            // OTA diagnostics — surface verbatim as ticker events (fetch retries live here).
            "ota/diag" | "ota/relaydiag" | "ota/armdiag" => {
                if !text.is_empty() {
                    let short: String = text.chars().take(80).collect();
                    let n = self.node_mut(id, now_s);
                    // The transfer PHASE ("fetch-failed retry=3", terminal outcomes) lands on
                    // ota/diag — keep the latest for the inspector; the others stay ticker-only.
                    if suffix == "ota/diag" {
                        n.ota_phase = Some(short.clone());
                    }
                    self.event(now_s, EventKind::Ota, format!("id{id} {suffix}: {short}"));
                }
            }
            _ => {
                // scan / screen / config echoes — count as liveness, no dedicated view yet.
                self.node_mut(id, now_s);
            }
        }
    }

    /// Deduplicated undirected edges for drawing: (a,b) -> strongest rssi + freshest age.
    pub fn edges(&self) -> Vec<Edge> {
        use std::collections::HashMap;
        let mut map: HashMap<(u8, u8), Edge> = HashMap::new();
        for (&src, node) in &self.nodes {
            for l in &node.links {
                if l.id == src {
                    continue;
                }
                let key = if src < l.id { (src, l.id) } else { (l.id, src) };
                let e = map.entry(key).or_insert(Edge { a: key.0, b: key.1, rssi: l.rssi, age_s: l.age_s });
                // Keep the strongest (max) rssi seen from either direction.
                if l.rssi > e.rssi {
                    e.rssi = l.rssi;
                }
                if l.age_s < e.age_s {
                    e.age_s = l.age_s;
                }
            }
        }
        map.into_values().collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub a: u8,
    pub b: u8,
    pub rssi: i32,
    pub age_s: u32,
}

/// `homeassistant/<comp>/smol<id>/<obj>/config` -> id.
fn discovery_node_id(topic: &str) -> Option<u8> {
    let mut segs = topic.split('/');
    segs.next()?; // homeassistant
    segs.next()?; // component
    let node = segs.next()?; // smol<id>
    node.strip_prefix("smol")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Model {
        Model::new("test".into())
    }

    #[test]
    fn screen_from_status_then_cfg_fallback() {
        let mut model = m();
        // Live STAT wins: "<AppKind>:<page>" -> AppKind.
        model.ingest(1.0, "smol/7/status", b"STAT|Familiar:0");
        assert_eq!(model.nodes[&7].screen().as_deref(), Some("Familiar"));
        // With no STAT, fall back to the DIAG cfg= default-screen echo.
        model.ingest(1.0, "smol/8/diag", b"DIAG|boot=1|heap=40000|cfg=Clock:1,status,00FF");
        assert_eq!(model.nodes[&8].screen().as_deref(), Some("Clock"));
    }

    #[test]
    fn sync_freshness_from_tage_tsrc() {
        let mut model = m();
        model.ingest(1.0, "smol/1/diag", b"DIAG|boot=1|heap=40000|tage=30|tsrc=ntp");
        model.ingest(1.0, "smol/2/diag", b"DIAG|boot=1|heap=40000|tage=820|tsrc=mesh");
        model.ingest(1.0, "smol/3/diag", b"DIAG|boot=1|heap=40000|tage=4200|tsrc=mesh");
        model.ingest(1.0, "smol/4/diag", b"DIAG|boot=1|heap=40000|tsrc=none");
        assert_eq!(model.nodes[&1].sync_freshness(), SyncFreshness::Fresh);
        assert_eq!(model.nodes[&2].sync_freshness(), SyncFreshness::Aging);
        assert_eq!(model.nodes[&3].sync_freshness(), SyncFreshness::Stale);
        assert_eq!(model.nodes[&4].sync_freshness(), SyncFreshness::Unsynced);
        assert_eq!(model.nodes[&1].time_age(), Some(30));
        assert_eq!(model.nodes[&1].time_src(), Some("ntp"));
    }

    #[test]
    fn ota_diag_phase_stored_on_node() {
        let mut model = m();
        model.ingest(1.0, "smol/8/ota/diag", b"fetch-failed retry=3");
        assert_eq!(model.nodes[&8].ota_phase.as_deref(), Some("fetch-failed retry=3"));
        // relaydiag stays ticker-only (not the transfer phase).
        model.ingest(2.0, "smol/8/ota/relaydiag", b"leaf=H2V1N0");
        assert_eq!(model.nodes[&8].ota_phase.as_deref(), Some("fetch-failed retry=3"));
    }

    #[test]
    fn peers_build_edges_and_dedup() {
        let mut model = m();
        model.ingest(1.0, "smol/7/peers", b"PEERS|G|6|8,-55,2,6,3;9,-72,5,6,1");
        model.ingest(1.0, "smol/8/peers", b"PEERS|L|6|7,-58,1,6,3");
        let edges = model.edges();
        assert_eq!(edges.len(), 2); // (7,8) merged, (7,9)
        let e78 = edges.iter().find(|e| (e.a, e.b) == (7, 8)).unwrap();
        assert_eq!(e78.rssi, -55); // strongest of -55 / -58
        assert!(model.nodes[&7].gateway);
        assert!(!model.nodes[&8].gateway);
    }

    #[test]
    fn crown_change_emits_event() {
        let mut model = m();
        model.ingest(1.0, "smol/mesh/channel", b"MC|7|6|10");
        model.ingest(2.0, "smol/mesh/channel", b"MC|8|6|11");
        let crowns = model.events.iter().filter(|e| e.kind == EventKind::Crown).count();
        assert_eq!(crowns, 2);
        assert_eq!(model.crown.unwrap().owner, 8);
        assert!(model.nodes[&8].gateway);
    }

    #[test]
    fn version_flip_emits_event() {
        let mut model = m();
        model.ingest(1.0, "smol/7/ota/state", br#"{"installed_version":"45","latest_version":"45","in_progress":false,"title":"v45"}"#);
        model.ingest(2.0, "smol/7/ota/state", br#"{"installed_version":"48","latest_version":"48","in_progress":false,"title":"v48"}"#);
        let flips = model.events.iter().filter(|e| e.kind == EventKind::Version).count();
        assert_eq!(flips, 1);
    }

    #[test]
    fn diag_feeds_heap_history() {
        let mut model = m();
        model.ingest(1.0, "smol/7/diag", b"DIAG|boot=1|heap=41000|up=10");
        model.ingest(11.0, "smol/7/diag", b"DIAG|boot=1|heap=40500|up=20");
        assert_eq!(model.nodes[&7].heap_hist.len(), 2);
        assert_eq!(model.nodes[&7].diag.as_ref().unwrap().u64("up"), Some(20));
    }

    #[test]
    fn peers_empty_clears_edges() {
        let mut model = m();
        model.ingest(1.0, "smol/7/peers", b"PEERS|G|6|8,-55,2,6,3");
        assert_eq!(model.edges().len(), 1);
        model.ingest(2.0, "smol/7/peers", b"");
        assert_eq!(model.edges().len(), 0);
        assert!(!model.nodes[&7].gateway);
    }
}
