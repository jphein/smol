//! RSSI filaments — luminous threads between nodes that hear each other. Brightness
//! (not width) encodes link strength; a slow shimmer keeps them alive; weak/aged links
//! fade toward the void.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::mesh::MeshHandle;
use crate::palette;
use crate::viz::NodeId;

pub fn filaments(time: Res<Time>, mesh: Res<MeshHandle>, orbs: Query<(&NodeId, &Transform)>, mut gizmos: Gizmos) {
    let edges = {
        let Ok(m) = mesh.model.lock() else { return };
        m.edges()
    };
    if edges.is_empty() {
        return;
    }

    // id → current world position.
    let pos: HashMap<u8, Vec2> = orbs.iter().map(|(n, t)| (n.0, t.translation.truncate())).collect();
    let elapsed = time.elapsed_seconds();

    for (k, e) in edges.iter().enumerate() {
        let (Some(&a), Some(&b)) = (pos.get(&e.a), pos.get(&e.b)) else { continue };
        // Age fade: fresh links at full strength, links unheard for a while dim out.
        let age_fade = (1.0 - (e.age_s as f32 / 60.0)).clamp(0.15, 1.0);
        // Per-edge shimmer so the whole web subtly breathes.
        let shimmer = 0.78 + 0.22 * (elapsed * 1.6 + k as f32 * 1.3).sin();
        let alpha = age_fade * shimmer;
        gizmos.line_2d(a, b, palette::rssi_color(e.rssi, alpha));
    }
}
