//! The scene's dramatic beats, every one triggered by a real model-state transition:
//!   * crown owner changes  → a gold comet arcs from the old crown to the new one
//!   * OTA `in_progress`     → a cyan particle stream flows crown → leaf; a burst on done
//!   * mesh channel changes  → the void's colour temperature shifts across the field

use std::collections::HashMap;
use std::f32::consts::TAU;

use bevy::prelude::*;

use crate::mesh::MeshHandle;
use crate::palette;
use crate::viz::{Comet, NodeId, Stream, VizState};

const COMET_DUR: f64 = 1.5;
const BURST_DUR: f64 = 0.7;

/// Diff the model against last frame's [`VizState`] and enqueue effects for every
/// transition. The renderer draws purely from this state, so the animations are always
/// caused by the mesh, never faked.
pub fn detect_transitions(mesh: Res<MeshHandle>, mut state: ResMut<VizState>) {
    let now = mesh.now();
    let (owner, channel, ota): (Option<u8>, Option<u8>, Vec<(u8, bool)>) = {
        let Ok(m) = mesh.model.lock() else { return };
        let owner = m.crown.map(|c| c.owner);
        let channel = m.crown.map(|c| c.channel);
        let ota = m
            .nodes
            .iter()
            .map(|(id, n)| (*id, n.ota.as_ref().map(|o| o.in_progress).unwrap_or(false)))
            .collect();
        (owner, channel, ota)
    };

    // Crown moved → launch a comet from old → new.
    if owner != state.crown_owner {
        if let (Some(from), Some(to)) = (state.crown_owner, owner) {
            if from != to {
                state.comet = Some(Comet { from, to, started: now });
            }
        }
        state.crown_owner = owner;
    }
    state.channel = channel;

    // OTA edges: false→true starts a stream (crown→leaf); true→false ends it (burst).
    for (id, active) in ota {
        let was = state.ota_active.get(&id).copied().unwrap_or(false);
        if active && !was {
            if let Some(crown) = owner {
                if crown != id {
                    state.streams.push(Stream { from: crown, to: id, started: now, ended: None });
                }
            }
        } else if !active && was {
            if let Some(s) = state.streams.iter_mut().find(|s| s.to == id && s.ended.is_none()) {
                s.ended = Some(now);
            }
        }
        state.ota_active.insert(id, active);
    }

    // Retire finished comet + spent streams.
    if state.comet.as_ref().is_some_and(|c| now - c.started > COMET_DUR + 0.2) {
        state.comet = None;
    }
    state.streams.retain(|s| match s.ended {
        Some(t) => now - t < BURST_DUR,
        None => now - s.started < 45.0, // safety TTL for a stream that never reports done
    });
}

fn positions(orbs: &Query<(&NodeId, &Transform)>) -> HashMap<u8, Vec2> {
    orbs.iter().map(|(n, t)| (n.0, t.translation.truncate())).collect()
}

/// A gold ring + slowly-rotating accents around the reigning crown, and the in-flight
/// crown-travel comet arcing between orbs.
pub fn draw_crown(
    time: Res<Time>,
    mesh: Res<MeshHandle>,
    state: Res<VizState>,
    orbs: Query<(&NodeId, &Transform)>,
    mut gizmos: Gizmos,
) {
    let now = mesh.now();
    let elapsed = time.elapsed_seconds();
    let pos = positions(&orbs);

    if let Some(owner) = state.crown_owner {
        if let Some(&c) = pos.get(&owner) {
            gizmos.circle_2d(c, 34.0, palette::crown_gold());
            for i in 0..6 {
                let a = elapsed * 0.6 + i as f32 * TAU / 6.0;
                gizmos.circle_2d(c + Vec2::new(a.cos(), a.sin()) * 40.0, 2.6, palette::crown_gold());
            }
        }
    }

    if let Some(cm) = &state.comet {
        if let (Some(&from), Some(&to)) = (pos.get(&cm.from), pos.get(&cm.to)) {
            let f = ((now - cm.started) / COMET_DUR).clamp(0.0, 1.0) as f32;
            let bow = (to - from).perp().normalize_or_zero();
            let head = comet_point(from, to, bow, f);
            gizmos.circle_2d(head, 6.0, palette::crown_gold());
            gizmos.circle_2d(head, 11.0, palette::crown_gold());
            // A short trailing tail behind the head.
            for k in 1..6 {
                let tf = (f - k as f32 * 0.05).max(0.0);
                gizmos.circle_2d(comet_point(from, to, bow, tf), (6.0 - k as f32).max(1.0), palette::crown_gold());
            }
        }
    }
}

/// Ease-out position along the bowed comet path.
fn comet_point(from: Vec2, to: Vec2, bow: Vec2, f: f32) -> Vec2 {
    let ease = 1.0 - (1.0 - f) * (1.0 - f);
    from.lerp(to, ease) + bow * ((ease * std::f32::consts::PI).sin() * 60.0)
}

/// OTA transfers: a train of cyan motes flowing crown→leaf while in progress, and an
/// expanding mana-burst at the leaf on completion.
pub fn draw_streams(mesh: Res<MeshHandle>, state: Res<VizState>, orbs: Query<(&NodeId, &Transform)>, mut gizmos: Gizmos) {
    let now = mesh.now();
    let pos = positions(&orbs);
    const MOTES: usize = 7;

    for s in &state.streams {
        let (Some(&from), Some(&to)) = (pos.get(&s.from), pos.get(&s.to)) else { continue };
        match s.ended {
            None => {
                let elapsed = (now - s.started) as f32;
                for k in 0..MOTES {
                    let frac = ((elapsed * 0.55) + (k as f32 / MOTES as f32)) % 1.0;
                    let p = from.lerp(to, frac);
                    gizmos.circle_2d(p, 3.0, palette::ota_stream());
                    gizmos.circle_2d(p, 6.0, palette::ota_stream());
                }
            }
            Some(t) => {
                let bf = ((now - t) / BURST_DUR).clamp(0.0, 1.0) as f32;
                gizmos.circle_2d(to, 8.0 + bf * 42.0, palette::ota_burst());
                gizmos.circle_2d(to, 4.0 + bf * 22.0, palette::ota_burst());
            }
        }
    }
}

/// Lerp the void's colour temperature toward the active channel's tint — a felt cue
/// that the whole mesh just hopped channels.
pub fn channel_shift(time: Res<Time>, state: Res<VizState>, mut clear: ResMut<ClearColor>) {
    let target = state.channel.map(palette::channel_tint).unwrap_or_else(palette::void);
    let cur = clear.0.to_linear();
    let tl = target.to_linear();
    let k = (time.delta_seconds() * 1.5).min(1.0);
    clear.0 = Color::linear_rgb(
        cur.red + (tl.red - cur.red) * k,
        cur.green + (tl.green - cur.green) * k,
        cur.blue + (tl.blue - cur.blue) * k,
    );
}
