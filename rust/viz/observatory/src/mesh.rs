//! Bridge between the shared `mesh-model` and the Bevy world.
//!
//! [`MeshHandle`] wraps the `Arc<Mutex<Model>>` the render systems read each frame.
//! In LIVE mode the mesh-model MQTT thread fills it from the broker; in DEMO mode the
//! [`demo_driver`] system feeds a scripted synthetic timeline through the exact same
//! `Model::ingest` path — so a demo IS a broker replay, and every animation observatory
//! shows is driven by real model-state transitions, not faked in the renderer.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use bevy::prelude::*;
use mesh_model::model::{ConnState, Model};
use mesh_model::mqtt;

/// The demo fleet — five nodes; ids map to deterministic realm nouns via mesh-model.
pub const DEMO_FLEET: [u8; 5] = [7, 8, 9, 10, 11];

/// Shared handle to the live world model + the monotonic clock origin. Every model
/// timestamp and every UI age is measured from `start`, so they share one clock.
#[derive(Resource, Clone)]
pub struct MeshHandle {
    pub model: Arc<Mutex<Model>>,
    pub start: Instant,
}

impl MeshHandle {
    /// Seconds since the shared origin — the model's notion of "now".
    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

/// LIVE mode: spawn the mesh-model MQTT listener from `SMOL_MQTT_*` env and hand back
/// the shared model it fills. Client id `observatory` (distinct from meshscope's) so
/// both can listen at once without the broker kicking a duplicate id.
pub fn live() -> Result<MeshHandle, String> {
    let cfg = mqtt::BrokerCfg::from_env()?.with_client_id("observatory");
    let start = Instant::now();
    let model = Arc::new(Mutex::new(Model::new(cfg.endpoint())));
    mqtt::spawn(model.clone(), cfg, start);
    Ok(MeshHandle { model, start })
}

/// DEMO mode: an empty, "connected" model the [`demo_driver`] fills over wall-clock.
pub fn demo() -> MeshHandle {
    let start = Instant::now();
    let mut m = Model::new("(demo - no broker)".into());
    m.conn = ConnState::Connected;
    MeshHandle { model: Arc::new(Mutex::new(m)), start }
}

// ---------------------------------------------------------------------------------
// The scripted demo timeline.
// ---------------------------------------------------------------------------------

struct ScriptEvent {
    t: f64,
    topic: String,
    payload: Vec<u8>,
}

/// Drives the demo model: fires a one-shot showcase (nodes → OTA → election → channel
/// shift → OTA fan-out) once, keeps every node fresh with periodic DIAG/uplink, and
/// then rotates crown + OTA forever so a wall display never goes still.
#[derive(Resource)]
pub struct DemoScript {
    queue: Vec<ScriptEvent>,
    qi: usize,
    last_live_s: f64,
    last_cycle_s: f64,
    cycle_i: usize,
    tick: u64,
    showcase_end_s: f64,
}

fn ev(t: f64, topic: &str, payload: impl AsRef<[u8]>) -> ScriptEvent {
    ScriptEvent { t, topic: topic.to_string(), payload: payload.as_ref().to_vec() }
}

/// Encode a dBm reading the way the firmware puts it on the wire: ESP-NOW RSSI is a
/// **raw `u8`**, so a negative dBm ships as its unsigned byte (e.g. -46 dBm -> 210).
/// mesh-model's `normalize_rssi` decodes it back. Emitting raw here makes the demo a
/// faithful `smol/<id>/peers` replay and exercises the decode end-to-end.
fn raw_rssi(dbm: i32) -> i32 {
    if dbm < 0 {
        dbm + 256
    } else {
        dbm
    }
}

/// A star roster centred on `owner` at channel `ch`: owner is gateway 'G' linking to
/// every leaf; each leaf is 'L' linking back. RSSI falls off by list position so the
/// filaments vary. Returns the peers events to publish (raw-u8 RSSI, like the fleet).
fn peers_star(owner: u8, fleet: &[u8], ch: u8, t: f64) -> Vec<ScriptEvent> {
    let leaves: Vec<u8> = fleet.iter().copied().filter(|&i| i != owner).collect();
    let mut out = Vec::new();
    // Owner sees all leaves.
    let mut body = String::new();
    for (k, &l) in leaves.iter().enumerate() {
        let rssi = raw_rssi(-46 - (k as i32) * 9); // -46, -55, -64, -73 dBm on the wire
        let age = 1 + k as u32;
        body.push_str(&format!("{l},{rssi},{age},{ch},3;"));
    }
    out.push(ev(t, &format!("smol/{owner}/peers"), format!("PEERS|G|{ch}|{body}")));
    // Each leaf sees the owner (+ a cross-link to its neighbour for a richer graph).
    for (k, &l) in leaves.iter().enumerate() {
        let rssi = raw_rssi(-48 - (k as i32) * 9);
        let mut lb = format!("{owner},{rssi},{},{ch},3;", 1 + k as u32);
        if let Some(&nb) = leaves.get((k + 1) % leaves.len()) {
            if nb != l {
                lb.push_str(&format!("{nb},{},{},{ch},1;", raw_rssi(-70 - (k as i32) * 3), 8 + k as u32));
            }
        }
        out.push(ev(t, &format!("smol/{l}/peers"), format!("PEERS|L|{ch}|{lb}")));
    }
    out
}

fn ota_state(installed: u32, latest: u32, in_progress: bool, title: &str) -> String {
    format!(
        r#"{{"installed_version":"{installed}","latest_version":"{latest}","in_progress":{in_progress},"title":"{title}"}}"#
    )
}

impl DemoScript {
    pub fn new() -> Self {
        let f = &DEMO_FLEET;
        let ch = 6u8;
        let mut q: Vec<ScriptEvent> = Vec::new();

        // t=0 — the fleet ignites: crown 7 on ch6, star roster, builds (9 is behind).
        q.push(ev(0.0, "smol/mesh/channel", "MC|7|6|100"));
        q.extend(peers_star(7, f, ch, 0.0));
        for &id in f {
            let (inst, latest) = if id == 9 { (123, 124) } else { (124, 124) };
            q.push(ev(0.0, &format!("smol/{id}/ota/state"), ota_state(inst, latest, false, "v124 Mandrel")));
            q.push(ev(0.0, &format!("smol/{id}/status"), format!("STAT|Clock:{}", id % 3)));
        }
        q.push(ev(0.2, "smol/display/batt", "BATT|48V 52.8V|HV 391.9V|d 43mV|48V 42%|HV 100%|Chg 1.9A"));
        q.push(ev(0.2, "smol/display/grid", "GRID|1056W|L1 158W|L2 898W"));

        // t=8..15 — OTA to id8: crown(7)→8 particle stream, then completion burst.
        q.push(ev(8.0, "smol/8/ota/state", ota_state(124, 125, true, "smol v125")));
        q.push(ev(9.0, "smol/8/ota/diag", "chunk 4/40 last_wb=8192"));
        q.push(ev(11.5, "smol/8/ota/diag", "chunk 22/40 last_wb=45056"));
        q.push(ev(14.0, "smol/8/ota/diag", "chunk 40/40 last_wb=81920"));
        q.push(ev(15.0, "smol/8/ota/state", ota_state(125, 125, false, "smol v125")));

        // t=22 — ELECTION: crown travels 7 → 10 (comet), roster re-centres on 10.
        q.push(ev(22.0, "smol/mesh/channel", "MC|10|6|101"));
        q.extend(peers_star(10, f, ch, 22.0));

        // t=30 — CHANNEL SHIFT 6 → 11: the whole field's colour temperature warps.
        q.push(ev(30.0, "smol/mesh/channel", "MC|10|11|102"));
        q.extend(peers_star(10, f, 11, 30.0));

        // t=38..53 — OTA fan-out from the new crown: 10→9 (with a stumble) and 10→11.
        q.push(ev(38.0, "smol/9/ota/state", ota_state(123, 125, true, "smol v125")));
        q.push(ev(38.0, "smol/11/ota/state", ota_state(124, 125, true, "smol v125")));
        q.push(ev(41.0, "smol/11/ota/diag", "chunk 30/40 last_wb=61440"));
        q.push(ev(44.0, "smol/9/ota/diag", "fetch retry 1/3 (window 12)"));
        q.push(ev(48.0, "smol/11/ota/state", ota_state(125, 125, false, "smol v125")));
        q.push(ev(50.0, "smol/9/ota/diag", "chunk 40/40 last_wb=81920"));
        q.push(ev(53.0, "smol/9/ota/state", ota_state(125, 125, false, "smol v125")));

        q.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap());
        DemoScript {
            queue: q,
            qi: 0,
            last_live_s: -10.0,
            last_cycle_s: 60.0,
            cycle_i: 0,
            tick: 0,
            showcase_end_s: 56.0,
        }
    }
}

impl Default for DemoScript {
    fn default() -> Self {
        Self::new()
    }
}

/// Tiny deterministic jitter in [-1,1] from two integers — no rng dependency, fully
/// reproducible (so demo captures are stable).
fn jitter(a: u64, b: u64) -> f32 {
    let h = (a.wrapping_mul(0x9E3779B97F4A7C15) ^ b.wrapping_mul(0xC2B2AE3D27D4EB4F)) >> 40;
    ((h & 0xFFFF) as f32 / 32768.0) - 1.0
}

/// The demo system: pump scripted + procedural events into the model over wall-clock.
pub fn demo_driver(mesh: Res<MeshHandle>, mut script: ResMut<DemoScript>) {
    let now = mesh.now();
    let Ok(mut m) = mesh.model.lock() else { return };

    // 1) Fire due one-shot showcase events.
    while script.qi < script.queue.len() && script.queue[script.qi].t <= now {
        let e = &script.queue[script.qi];
        m.ingest(now, &e.topic, &e.payload);
        script.qi += 1;
    }

    // 2) Liveness: every ~2s refresh DIAG + uplink so nothing false-ages, with a
    //    little jitter for organic motion.
    if now - script.last_live_s >= 2.0 {
        script.last_live_s = now;
        script.tick += 1;
        let tick = script.tick;
        let crown = m.crown.map(|c| c.owner);
        for (slot, &id) in DEMO_FLEET.iter().enumerate() {
            let heap = 40_000 + (jitter(id as u64, tick) * 900.0) as i64;
            let up = 100 + now as u64;
            let boot = 4 + (id as u64 % 3);
            // Vary NTP freshness across the fleet so the aura shows every state.
            let (tage, tsrc): (u64, &str) = match id {
                9 => (4200, "mesh"), // stale (red)
                8 => (820, "mesh"),  // aging (amber)
                11 => (0, "none"),   // unsynced (grey)
                _ => (30, "ntp"),    // fresh (green)
            };
            let mut diag = format!(
                "DIAG|slot={}|rst=POWERON|boot={boot}|ota=ok|up={up}|heap={heap}|hmin=37200|led=status:on|tage={tage}|tsrc={tsrc}|net=0:ok|brk=baked|otah=slot|fwd=0|dedup=0|hop=1",
                slot % 2
            );
            // The reigning crown reports its AP association + a climbing dead-downstream
            // streak (#204) — the crown visibly sickens/reddens/flickers, then sheds and
            // recovers, so the coexist disease is drama on the wall display.
            if Some(id) == crown {
                let streak = ((tick / 2) % 10) as u8; // 0..9, climbs then resets
                diag.push_str(&format!("|ap=6:-58:a1b2c3d4e5f6|cdeaf={streak}:{}:{}", streak / 3, u8::from(streak >= 8)));
            }
            m.ingest(now, &format!("smol/{id}/diag"), diag.as_bytes());
            let rssi = -52 + (jitter(tick, id as u64) * 6.0) as i32;
            m.ingest(now, &format!("smol/{id}/uplink"), rssi.to_string().as_bytes());
        }
    }

    // 3) After the showcase, rotate crown + OTA forever (a wall display must never
    //    go static). Deterministic — no rng, so it's reproducible.
    if now > script.showcase_end_s && now - script.last_cycle_s >= 14.0 {
        script.last_cycle_s = now;
        script.cycle_i += 1;
        let order = [7u8, 8, 10, 9, 11];
        let owner = order[script.cycle_i % order.len()];
        let ch = match script.cycle_i % 3 {
            0 => 1,
            1 => 6,
            _ => 11,
        };
        let seq = 102 + script.cycle_i as u32;
        m.ingest(now, "smol/mesh/channel", format!("MC|{owner}|{ch}|{seq}").as_bytes());
        for e in peers_star(owner, &DEMO_FLEET, ch, now) {
            m.ingest(now, &e.topic, &e.payload);
        }
        // Kick an OTA on the next leaf, and finish the previous one.
        let leaf = order[(script.cycle_i + 2) % order.len()];
        m.ingest(now, &format!("smol/{leaf}/ota/state"), ota_state(124, 125, true, "smol v125").as_bytes());
    } else if now > script.showcase_end_s {
        // Mid-cycle: complete any in-flight demo OTA ~7s after it started.
        if now - script.last_cycle_s >= 7.0 && script.cycle_i > 0 {
            let order = [7u8, 8, 10, 9, 11];
            let leaf = order[(script.cycle_i + 2) % order.len()];
            // Idempotent: publishing a settled state just clears in_progress.
            m.ingest(now, &format!("smol/{leaf}/ota/state"), ota_state(125, 125, false, "smol v125").as_bytes());
        }
    }
}

// ---------------------------------------------------------------------------------
// Headless self-test — the display-less build-host (familiar) green check.
// ---------------------------------------------------------------------------------

/// Run the whole showcase script through a fresh model instantly, print the resulting
/// world, and assert the fold produced a sane mesh. No Bevy, no window — proves the
/// ingest→model path end-to-end on a headless host.
pub fn selftest() -> Result<(), Box<dyn std::error::Error>> {
    let script = DemoScript::new();
    let mut m = Model::new("(selftest)".into());
    // Replay the queue at its scripted timestamps, then a couple of liveness ticks.
    for e in &script.queue {
        m.ingest(e.t, &e.topic, &e.payload);
    }
    // A late diag round so every node is fresh.
    for &id in &DEMO_FLEET {
        m.ingest(60.0, &format!("smol/{id}/diag"), b"DIAG|boot=4|heap=40000|up=999|led=status:on|hop=1");
    }

    println!("== observatory selftest ==");
    println!("nodes: {}", m.nodes.len());
    for (id, n) in &m.nodes {
        println!(
            "  id{id:<3} {:<10} role={:<7} ch={:<3} build={:<4} links={} last_seen={:.0}",
            n.label(),
            if n.gateway { "gateway" } else { "leaf" },
            n.channel.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
            n.build().unwrap_or("?"),
            n.links.len(),
            n.last_seen_s,
        );
    }
    println!("crown: {:?}", m.crown);
    println!("edges: {}", m.edges().len());
    println!("events: {}", m.events.len());

    assert_eq!(m.nodes.len(), DEMO_FLEET.len(), "expected the full demo fleet");
    assert!(m.crown.is_some(), "expected a crown");
    assert!(m.edges().len() >= DEMO_FLEET.len() - 1, "expected a connected star");
    assert!(m.events.iter().any(|e| matches!(e.kind, mesh_model::model::EventKind::Crown)), "expected crown events");
    assert!(m.events.iter().any(|e| matches!(e.kind, mesh_model::model::EventKind::Version)), "expected a version flip");
    println!("OK");
    Ok(())
}
