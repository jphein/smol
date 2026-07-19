//! The observatory render layer — a Bevy plugin that turns the shared `mesh-model`
//! snapshot into a living fantasy constellation.
//!
//! Every visual derives from model state read fresh each frame:
//!   * nodes → glowing force-directed orbs (this file: layout; [`nodes`]: orbs/labels)
//!   * per-peer RSSI → luminous filaments ([`edges`])
//!   * elections / OTA / channel changes → animated effects ([`effects`])
//!   * broker + crown + fleet stats → the HUD ([`hud`])

use std::collections::HashMap;

use bevy::core_pipeline::bloom::BloomSettings;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;
use bevy::sprite::Mesh2dHandle;

use crate::mesh::MeshHandle;
use crate::palette;

pub mod effects;
pub mod edges;
pub mod hud;
pub mod nodes;

// --- layout tuning ---------------------------------------------------------------
const REPULSION: f32 = 460_000.0; // node-node push (∝ 1/d²)
const SPRING_K: f32 = 2.1; // edge pull stiffness
const CENTER_K: f32 = 0.42; // gentle pull to the origin
const CROWN_CENTER_K: f32 = 2.1; // the crown sits near the middle
const DAMPING: f32 = 0.86; // velocity retention per step
const MAX_SPEED: f32 = 420.0;
const IDEAL_MIN: f32 = 215.0; // ideal edge length for a strong link
const IDEAL_MAX: f32 = 430.0; // ... for a weak link

/// A visualized node — carries its mesh id and its layout velocity.
#[derive(Component)]
pub struct NodeId(pub u8);

#[derive(Component, Default)]
pub struct Velocity(pub Vec2);

/// Desired core radius (px), driven by role; the visual scale eases toward it.
#[derive(Component)]
pub struct Radius(pub f32);

/// A world-space label that follows the orb with this id.
#[derive(Component)]
pub struct LabelFor(pub u8);

/// id → orb entity, rebuilt as nodes appear/vanish.
#[derive(Resource, Default)]
pub struct NodeIndex(pub HashMap<u8, Entity>);

/// The shared unit-circle mesh every orb scales.
#[derive(Resource)]
pub struct OrbMesh(pub Handle<Mesh>);

/// A crown-travel comet in flight (endpoints resolved from live orb positions).
pub struct Comet {
    pub from: u8,
    pub to: u8,
    pub started: f64,
}

/// An OTA transfer stream crown→leaf; `ended` set when the transfer completes (then
/// it plays a burst for a beat before it's dropped).
pub struct Stream {
    pub from: u8,
    pub to: u8,
    pub started: f64,
    pub ended: Option<f64>,
}

/// Edge-detection + effect state carried between frames.
#[derive(Resource, Default)]
pub struct VizState {
    pub crown_owner: Option<u8>,
    pub channel: Option<u8>,
    pub ota_active: HashMap<u8, bool>,
    pub comet: Option<Comet>,
    pub streams: Vec<Stream>,
}

pub struct VizPlugin;

impl Plugin for VizPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(palette::void()))
            .init_resource::<NodeIndex>()
            .init_resource::<VizState>()
            .add_systems(Startup, (setup, hud::setup_hud))
            .add_systems(
                Update,
                (
                    nodes::sync_nodes,
                    layout,
                    nodes::pulse_and_size,
                    nodes::sync_labels,
                    edges::filaments,
                    effects::detect_transitions,
                    effects::draw_crown,
                    effects::draw_streams,
                    effects::channel_shift,
                    hud::update_hud,
                )
                    .chain(),
            );
    }
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut gizmo_config: ResMut<GizmoConfigStore>) {
    // HDR camera + bloom = the glow. Dark tonemap keeps the void black so bright cores
    // blossom into halos.
    commands.spawn((
        Camera2dBundle {
            camera: Camera { hdr: true, ..default() },
            tonemapping: Tonemapping::TonyMcMapface,
            ..default()
        },
        BloomSettings::NATURAL,
    ));

    // A little heavier gizmo lines so filaments read on a wall display.
    let (config, _) = gizmo_config.config_mut::<DefaultGizmoConfigGroup>();
    config.line_width = 2.5;

    commands.insert_resource(OrbMesh(meshes.add(Circle::new(1.0))));
}

/// Deterministic starting position on a ring — keeps the initial layout non-degenerate
/// (no divide-by-zero when every orb would otherwise stack on the origin).
pub fn ring_start_pos(id: u8) -> Vec2 {
    let a = id as f32 * 2.399_963_2; // golden angle (rad)
    Vec2::new(a.cos(), a.sin()) * 250.0
}

/// Force-directed integration: pairwise repulsion + per-edge springs (strong links pull
/// tighter) + a centring pull (stronger on the crown). Reads edges/crown from the model
/// snapshot; writes orb positions. The gentle never-settling drift is the "living" feel.
#[allow(clippy::type_complexity)]
fn layout(
    time: Res<Time>,
    mesh: Res<MeshHandle>,
    mut q: Query<(Entity, &NodeId, &mut Transform, &mut Velocity)>,
) {
    let dt = time.delta_seconds().min(0.05); // clamp on hitches so nothing explodes
    if dt <= 0.0 {
        return;
    }

    // Pull what we need from the model, then release the lock before touching Bevy.
    let (edges, crown_owner) = {
        let Ok(m) = mesh.model.lock() else { return };
        (m.edges(), m.crown.map(|c| c.owner))
    };

    // Snapshot positions.
    let snap: Vec<(Entity, u8, Vec2)> =
        q.iter().map(|(e, id, t, _)| (e, id.0, t.translation.truncate())).collect();
    if snap.len() < 2 {
        return;
    }
    let index: HashMap<u8, usize> = snap.iter().enumerate().map(|(k, (_, id, _))| (*id, k)).collect();
    let mut force: HashMap<Entity, Vec2> = HashMap::new();

    // Pairwise repulsion.
    for i in 0..snap.len() {
        for j in (i + 1)..snap.len() {
            let d = snap[i].2 - snap[j].2;
            let dist2 = d.length_squared().max(400.0);
            let f = d.normalize_or_zero() * (REPULSION / dist2);
            *force.entry(snap[i].0).or_default() += f;
            *force.entry(snap[j].0).or_default() -= f;
        }
    }

    // Edge springs — stronger RSSI ⇒ shorter ideal length ⇒ tighter cluster.
    for e in &edges {
        let (Some(&ia), Some(&ib)) = (index.get(&e.a), index.get(&e.b)) else { continue };
        let strength = palette::rssi_strength(e.rssi); // 0 weak..1 strong
        let ideal = IDEAL_MAX - (IDEAL_MAX - IDEAL_MIN) * strength;
        let d = snap[ib].2 - snap[ia].2;
        let dist = d.length().max(1.0);
        let f = d / dist * (SPRING_K * (dist - ideal));
        *force.entry(snap[ia].0).or_default() += f;
        *force.entry(snap[ib].0).or_default() -= f;
    }

    // Centring (crown pulled harder toward the middle).
    for (e, id, p) in &snap {
        let k = if Some(*id) == crown_owner { CROWN_CENTER_K } else { CENTER_K };
        *force.entry(*e).or_default() -= *p * k;
    }

    // Integrate.
    for (e, _id, mut t, mut v) in q.iter_mut() {
        let f = force.get(&e).copied().unwrap_or(Vec2::ZERO);
        let nv = ((v.0 + f * dt) * DAMPING).clamp_length_max(MAX_SPEED);
        v.0 = nv;
        t.translation += (nv * dt).extend(0.0);
    }
}

/// A shared unit-circle handle accessor guard for spawners.
pub fn mesh2d(handle: &Handle<Mesh>) -> Mesh2dHandle {
    Mesh2dHandle(handle.clone())
}
