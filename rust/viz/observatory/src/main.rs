//! observatory — the smol mesh rendered as a living fantasy constellation (issue #159).
//!
//! A pure MQTT *listener* (via the shared `mesh-model`): it never publishes to the
//! fleet. It folds `smol/#` into a world model and renders it as art — glowing
//! force-directed orbs, RSSI filaments, crown-travel comets on elections, OTA particle
//! streams crown→leaf, and channel-change colour shifts. Built for a wall display and
//! the Hackaday hero clip. meshscope (#158) is the instrument; this is the showpiece.
//!
//! Modes:
//!   observatory                 live: connect to the broker (SMOL_MQTT_* / .env)
//!   observatory --demo          synthetic scripted mesh, no broker needed
//!   observatory --selftest      headless: fold the demo timeline, assert, print, exit
//!   observatory --demo --screenshot out.png [--at 12]
//!                               render the demo and save one frame (needs a display;
//!                               on a headless host run under `xvfb-run`)

mod mesh;
mod palette;
mod viz;

use bevy::prelude::*;
use bevy::render::view::screenshot::ScreenshotManager;
use bevy::window::PrimaryWindow;

use mesh::{demo, demo_driver, live, DemoScript, MeshHandle};
use viz::VizPlugin;

const HELP: &str = "\
observatory — the smol mesh as a living fantasy constellation

USAGE:
    observatory [FLAGS]

FLAGS:
    --demo               Render a synthetic scripted mesh (no broker needed)
    --selftest           Headless: fold the demo timeline, print a summary, assert, exit
    --screenshot <PATH>  Save one rendered frame to PATH then exit (implies a render;
                         on a display-less host run via `xvfb-run -a observatory ...`)
    --at <SECONDS>       When to grab the screenshot (default 12; a nice mid-OTA beat)
    --help               Show this help
    --version            Show version

ENV (live mode; a local .env is loaded if present):
    SMOL_MQTT_HOST   broker host, or host:port  (required for live mode)
    SMOL_MQTT_PORT   broker port (default 1883)
    SMOL_MQTT_USER   broker username
    SMOL_MQTT_PASS   broker password
";

/// Deferred screenshot request (best-effort headless capture).
#[derive(Resource)]
struct ShotReq {
    path: String,
    at: f64,
    queued: bool,
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("observatory {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest") {
        return mesh::selftest();
    }

    let is_demo = args.iter().any(|a| a == "--demo");
    let handle = if is_demo {
        demo()
    } else {
        match live() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("observatory: {e}");
                eprintln!("Set SMOL_MQTT_HOST (+ USER/PASS), or run `observatory --demo`. See --help.");
                std::process::exit(2);
            }
        }
    };

    let shot = arg_value(&args, "--screenshot").map(|path| ShotReq {
        path,
        at: arg_value(&args, "--at").and_then(|s| s.parse().ok()).unwrap_or(12.0),
        queued: false,
    });

    run_app(handle, is_demo, shot);
    Ok(())
}

fn run_app(handle: MeshHandle, is_demo: bool, shot: Option<ShotReq>) {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "observatory — smol mesh".into(),
                    resolution: (1600.0_f32, 1000.0_f32).into(),
                    ..default()
                }),
                ..default()
            })
            .set(ImagePlugin::default_nearest()),
    )
    .insert_resource(handle)
    .add_plugins(VizPlugin);

    if is_demo {
        app.insert_resource(DemoScript::default()).add_systems(Update, demo_driver);
    }

    if let Some(req) = shot {
        app.insert_resource(req).add_systems(Update, (screenshot_sys, exit_after_shot));
    }

    app.run();
}

/// Queue a disk screenshot once the scene has had `at` seconds to develop.
fn screenshot_sys(
    mesh: Res<MeshHandle>,
    mut req: ResMut<ShotReq>,
    mut manager: ResMut<ScreenshotManager>,
    windows: Query<Entity, With<PrimaryWindow>>,
) {
    if req.queued || mesh.now() < req.at {
        return;
    }
    if let Ok(win) = windows.get_single() {
        match manager.save_screenshot_to_disk(win, req.path.clone()) {
            Ok(()) => {
                info!("observatory: screenshot queued → {}", req.path);
                req.queued = true;
            }
            Err(e) => error!("observatory: screenshot failed: {e}"),
        }
    }
}

/// Exit shortly after the screenshot frame has been written.
fn exit_after_shot(mesh: Res<MeshHandle>, req: Res<ShotReq>) {
    if req.queued && mesh.now() >= req.at + 1.8 {
        std::process::exit(0);
    }
}
