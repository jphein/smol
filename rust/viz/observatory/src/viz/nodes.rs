//! Node orbs: spawn one glowing disc per mesh node, colour + size it by role and
//! freshness, breathe it gently, and trail a name label beneath it.

use bevy::prelude::*;
use bevy::sprite::MaterialMesh2dBundle;

use crate::mesh::MeshHandle;
use crate::palette;
use crate::viz::{mesh2d, ring_start_pos, LabelFor, NodeId, NodeIndex, OrbMesh, Radius, Velocity};

/// Core colour + radius (px) for a node's role/freshness.
fn role_visual(gateway: bool, crowned: bool, stale: bool) -> (Color, f32) {
    if stale {
        (palette::stale_core(), 11.0)
    } else if crowned {
        (palette::crown_core(), 26.0)
    } else if gateway {
        (palette::gateway_core(), 21.0)
    } else {
        (palette::leaf_core(), 16.0)
    }
}

/// Reconcile orb + label entities to the model's node set; recolour/resize existing
/// ones each frame. (The model never drops nodes in v1, so there is no despawn path —
/// stale nodes fade in place, which is the intended "ghost" behaviour.)
#[allow(clippy::type_complexity)]
pub fn sync_nodes(
    mut commands: Commands,
    mesh: Res<MeshHandle>,
    orb_mesh: Res<OrbMesh>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut index: ResMut<NodeIndex>,
    mut orbs: Query<(&NodeId, &Handle<ColorMaterial>, &mut Radius)>,
    mut labels: Query<(&LabelFor, &mut Text)>,
) {
    let now = mesh.now();
    let views: Vec<(u8, Color, f32, String)> = {
        let Ok(m) = mesh.model.lock() else { return };
        let owner = m.crown.map(|c| c.owner);
        m.nodes
            .iter()
            .map(|(id, n)| {
                let (color, r) = role_visual(n.gateway, owner == Some(*id), n.is_stale(now));
                (*id, color, r, n.label().to_string())
            })
            .collect()
    };

    // Update existing orbs (colour + target radius).
    for (nid, mat_handle, mut radius) in orbs.iter_mut() {
        if let Some(v) = views.iter().find(|v| v.0 == nid.0) {
            if let Some(mat) = materials.get_mut(mat_handle) {
                mat.color = v.1;
            }
            radius.0 = v.2;
        }
    }

    // Update label text (handles a late HA-discovery noun replacing the vendored one).
    for (lf, mut text) in labels.iter_mut() {
        if let Some(v) = views.iter().find(|v| v.0 == lf.0) {
            if text.sections[0].value != v.3 {
                text.sections[0].value = v.3.clone();
            }
        }
    }

    // Spawn orbs (+ labels) for newly-seen nodes.
    for (id, color, r, label) in &views {
        if index.0.contains_key(id) {
            continue;
        }
        let mat = materials.add(ColorMaterial::from(*color));
        let pos = ring_start_pos(*id);
        let e = commands
            .spawn((
                NodeId(*id),
                Velocity::default(),
                Radius(*r),
                MaterialMesh2dBundle {
                    mesh: mesh2d(&orb_mesh.0),
                    material: mat,
                    transform: Transform::from_translation(pos.extend(1.0)).with_scale(Vec3::splat(*r)),
                    ..default()
                },
            ))
            .id();
        index.0.insert(*id, e);

        commands.spawn((
            LabelFor(*id),
            Text2dBundle {
                text: Text::from_section(
                    label.clone(),
                    TextStyle { font_size: 15.0, color: Color::srgb(0.82, 0.86, 0.96), ..default() },
                ),
                transform: Transform::from_translation(pos.extend(3.0)),
                ..default()
            },
        ));
    }
}

/// Ease each orb's scale toward its target radius and add a slow breathing pulse —
/// the crown breathes a touch more strongly. This is the "alive" idle motion.
pub fn pulse_and_size(time: Res<Time>, mesh: Res<MeshHandle>, mut orbs: Query<(&NodeId, &Radius, &mut Transform)>) {
    let dt = time.delta_seconds();
    let elapsed = time.elapsed_seconds();
    let crown = mesh.model.lock().ok().and_then(|m| m.crown.map(|c| c.owner));
    for (nid, radius, mut t) in orbs.iter_mut() {
        let cur = t.scale.x.max(0.01);
        let eased = cur + (radius.0 - cur) * (10.0 * dt).min(1.0);
        let amp = if Some(nid.0) == crown { 0.09 } else { 0.05 };
        let pulse = 1.0 + amp * (elapsed * 2.2 + nid.0 as f32 * 0.7).sin();
        t.scale = Vec3::splat(eased * pulse);
    }
}

/// Keep each name label sitting just beneath its orb.
#[allow(clippy::type_complexity)]
pub fn sync_labels(
    orbs: Query<(&NodeId, &Transform), Without<LabelFor>>,
    mut labels: Query<(&LabelFor, &mut Transform), With<LabelFor>>,
) {
    for (lf, mut lt) in labels.iter_mut() {
        if let Some((_, ot)) = orbs.iter().find(|(nid, _)| nid.0 == lf.0) {
            let r = ot.scale.x;
            lt.translation = ot.translation + Vec3::new(0.0, -(r + 16.0), 3.0 - ot.translation.z);
            lt.translation.z = 3.0;
        }
    }
}
