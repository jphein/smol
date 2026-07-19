//! meshscope — a realtime egui instrument for the smol ESP-NOW mesh (issue #158).
//!
//! Pure MQTT listener: subscribes `smol/#`, folds DIAG/STAT/status/peers/mesh-channel
//! /ota-state/uplink/telemetry into a live world model, and renders a force-directed
//! node graph + per-node detail + event ticker. No firmware changes.
//!
//! Modes:
//!   meshscope            connect to the broker (SMOL_MQTT_HOST/USER/PASS, or .env)
//!   meshscope --demo     launch the UI with synthetic mesh data (no broker)
//!   meshscope --selftest headless: feed sample payloads through the model, print, exit
//!   meshscope --help / --version

mod operator;
mod ui;

use std::sync::{Arc, Mutex};
use std::time::Instant;

// The ingest + world model live in the shared `mesh-model` crate (converged #159);
// observatory folds identical state through the same core.
use mesh_model::model::{ConnState, Model};
use mesh_model::mqtt;

const HELP: &str = "\
meshscope — realtime egui instrument for the smol mesh

USAGE:
    meshscope [FLAGS]

FLAGS:
    --operator    Enable OPERATOR mode: an Operator dock appears and meshscope may PUBLISH
                  to smol command topics (OTA install, config/*, reboot/scan, channel_hint).
                  WITHOUT this flag meshscope is a pure listener (default). Live mode only.
    --demo        Launch the UI seeded with synthetic mesh data (no broker needed)
    --selftest    Headless: feed sample payloads through the model, print a summary, exit
    --help        Show this help
    --version     Show version

ENV (live mode; a local .env is loaded if present):
    SMOL_MQTT_HOST   broker host, or host:port  (required for live mode)
    SMOL_MQTT_PORT   broker port (default 1883)
    SMOL_MQTT_USER   broker username
    SMOL_MQTT_PASS   broker password
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv(); // load .env if present; ignore if absent
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("meshscope {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest") {
        return selftest();
    }

    let demo = args.iter().any(|a| a == "--demo");
    let operator_on = args.iter().any(|a| a == "--operator");
    let start = Instant::now();

    let (model, operator): (Arc<Mutex<Model>>, Option<operator::Publisher>) = if demo {
        let mut m = Model::new("(demo — no broker)".into());
        seed_demo(&mut m);
        m.conn = ConnState::Connected;
        if operator_on {
            eprintln!("meshscope: --operator has no effect with --demo (no live broker to publish to)");
        }
        (Arc::new(Mutex::new(m)), None)
    } else {
        let cfg = match mqtt::BrokerCfg::from_env().map(|c| c.with_client_id("meshscope")) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("meshscope: {e}");
                eprintln!("Set SMOL_MQTT_HOST (+ USER/PASS), or run `meshscope --demo`. See --help.");
                std::process::exit(2);
            }
        };
        let m = Arc::new(Mutex::new(Model::new(cfg.endpoint())));
        mqtt::spawn(m.clone(), cfg.clone(), start);
        // #23: the operator publisher is a SEPARATE client (distinct `<id>-op` id) so it
        // can never collide with the listener; constructed ONLY with --operator.
        let operator = operator_on.then(|| operator::Publisher::new(&cfg));
        if operator_on {
            eprintln!("meshscope: ⚡ OPERATOR MODE — publishing to smol/* is ARMED");
        }
        (m, operator)
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1480.0, 940.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("meshscope — smol mesh"),
        ..Default::default()
    };

    let model_for_app = model.clone();
    let initial_selected = if demo { Some(7) } else { None };
    eframe::run_native(
        "meshscope",
        native_options,
        Box::new(move |cc| Ok(Box::new(ui::MeshscopeApp::new(cc, model_for_app, start, initial_selected, operator)))),
    )?;
    Ok(())
}

/// Headless model check — runs on the (display-less) build host to prove the parse +
/// fold path works end-to-end without a GUI. Feeds representative payloads and prints
/// the resulting world state.
fn selftest() -> Result<(), Box<dyn std::error::Error>> {
    let mut m = Model::new("(selftest)".into());
    seed_demo(&mut m);
    println!("== meshscope selftest ==");
    println!("nodes: {}", m.nodes.len());
    for (id, n) in &m.nodes {
        println!(
            "  id{id:<3} {:<10} role={:<7} ch={:<3} build={:<4} heap_hist={} links={}",
            n.label(),
            if n.gateway { "gateway" } else { "leaf" },
            n.channel.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
            n.build().unwrap_or("?"),
            n.heap_hist.len(),
            n.links.len(),
        );
    }
    println!("crown: {:?}", m.crown);
    println!("edges: {}", m.edges().len());
    println!("events: {}", m.events.len());
    assert!(m.nodes.len() >= 3, "expected >= 3 demo nodes");
    assert!(m.crown.is_some(), "expected a crown");
    assert!(!m.edges().is_empty(), "expected edges");
    println!("OK");
    Ok(())
}

/// Seed a realistic 3-node topology across a synthetic timeline (feeds through the
/// real `ingest`, so demo == a broker replay). id7 = gateway/crown, id8 = strong
/// leaf a build behind, id9 = weak/stranded leaf.
fn seed_demo(m: &mut Model) {
    // A trail of DIAG (heap) + uplink RSSI over ~5 minutes for nice sparklines.
    for k in 0..40u32 {
        let t = k as f64 * 8.0;
        let heap7 = 41_000 - (k as i64 * 30) + ((k % 5) as i64 * 40);
        let heap8 = 39_500 - (k as i64 * 12);
        let heap9 = 38_200 + ((k % 7) as i64 * 25);
        m.ingest(t, "smol/7/diag", format!("DIAG|slot=0|rst=POWERON|boot=4|ota=ok|up={}|heap={heap7}|hmin=37200|loss=1|rtt=12|rx={}|tx={}|led=status:on|tage=30|tsrc=ntp|net=0:ok|brk=baked|otah=slot|fwd=0|dedup=0|ttl=0|hop=1|dlseq=0|dfwd=0|ap=6:-58:a1b2c3d4e5f6|cdeaf=3:1:0", 100 + t as u64, 200 + k, 190 + k).as_bytes());
        m.ingest(t, "smol/8/diag", format!("DIAG|slot=1|rst=SW|boot=7|ota=ok|up={}|heap={heap8}|hmin=36900|loss=3|rtt=18|rx={}|tx={}|led=status:on|tage=820|tsrc=mesh|net=0:ok|brk=baked|otah=slot|fwd=0|dedup=2|ttl=0|hop=1|dlseq=1783|dfwd=1|cfg=Batt:2", 90 + t as u64, 150 + k, 140 + k).as_bytes());
        // id9: NTP-stale (~70 min since sync) — the case JP wants visible.
        m.ingest(t, "smol/9/diag", format!("DIAG|slot=0|rst=POWERON|boot=2|ota=ok|up={}|heap={heap9}|hmin=37600|loss=22|rtt=40|rx={}|tx={}|led=status:on|tage=4200|tsrc=mesh|net=0:ok|brk=baked|otah=slot|fwd=0|dedup=0|ttl=1|hop=2|dlseq=1783|dfwd=0", 60 + t as u64, 40 + k, 30 + k).as_bytes());
        m.ingest(t, "smol/7/uplink", format!("{}", -54 - (k % 6) as i32).as_bytes());
    }
    let t = 320.0;
    // Roster / edges.
    m.ingest(t, "smol/7/peers", b"PEERS|G|6|8,-52,2,6,3;9,-78,20,6,1");
    m.ingest(t, "smol/8/peers", b"PEERS|L|6|7,-55,1,6,3");
    m.ingest(t, "smol/9/peers", b"PEERS|L|6|7,-80,25,6,1");
    // Election / crown.
    m.ingest(t, "smol/mesh/channel", b"MC|7|6|41");
    // OTA state: id8 is mid-update (45 → 48) so meshscope shows the live badge; id7/id9 settled.
    m.ingest(t, "smol/7/ota/state", br#"{"installed_version":"48","latest_version":"48","in_progress":false,"title":"v48 Molten Crucible"}"#);
    m.ingest(t, "smol/8/ota/state", br#"{"installed_version":"45","latest_version":"48","in_progress":true,"title":"v48 Molten Crucible"}"#);
    m.ingest(t, "smol/8/ota/diag", b"relaying block 22/40 retry=0");
    m.ingest(t, "smol/9/ota/state", br#"{"installed_version":"48","latest_version":"48","in_progress":false,"title":"v48 Molten Crucible"}"#);
    // Status (live screen:page) — id7 shows the Familiar; id8's OTA screen has taken over.
    m.ingest(t, "smol/7/status", b"STAT|Familiar:0");
    m.ingest(t, "smol/8/status", b"STAT|OTA:0");
    m.ingest(t, "smol/9/status", b"STAT|About:0");
    // Telemetry lines.
    m.ingest(t, "smol/7/telemetry", b"23C 3.98V Nexus");
    m.ingest(t, "smol/8/telemetry", b"24C 3.91V Crown");
    m.ingest(t, "smol/9/telemetry", b"22C 3.72V Relic");
    // HA display readout.
    m.ingest(t, "smol/display/batt", b"BATT|48V 52.8V|HV 391.9V|d 43mV");
    // A few live events for the ticker.
    m.ingest(t + 5.0, "smol/9/ota/install", b"INSTALL");
    m.ingest(t + 8.0, "smol/9/ota/diag", b"fetch retry 1/3 (window 12)");
    // (id8 stays mid-update — leave its in_progress ota/state as the final demo state so
    // the live OTA badge is visible.)
    m.ingest(t + 20.0, "smol/mesh/channel", b"MC|7|6|42");
}
