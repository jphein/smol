//! The node-graph canvas: a light force-directed layout (deterministic seed per id,
//! so it settles the same way every run) drawn on an egui `Painter`. Discs coloured
//! by role/liveness, RSSI-weighted edges, a crown badge on the elected owner, and a
//! tiny heap sparkline under each node.

use std::collections::HashMap;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

use crate::model::{Model, WEAK_LINK_DBM};

const GOLDEN_ANGLE: f32 = 2.399_963_2; // radians, spreads initial placement
const K_REP: f32 = 90_000.0; // repulsion strength
const K_SPRING: f32 = 0.045; // edge attraction
const K_CENTER: f32 = 0.012; // gentle pull to origin
const DAMPING: f32 = 0.82;

pub struct GraphLayout {
    pos: HashMap<u8, Pos2>,
    vel: HashMap<u8, Vec2>,
    cam_center: Pos2, // sim-space point mapped to rect center
    cam_scale: f32,
    seeded: u32,
}

impl Default for GraphLayout {
    fn default() -> Self {
        GraphLayout {
            pos: HashMap::new(),
            vel: HashMap::new(),
            cam_center: Pos2::ZERO,
            cam_scale: 1.0,
            seeded: 0,
        }
    }
}

impl GraphLayout {
    /// Advance the physics a few substeps. Idempotent-ish: converges under damping.
    pub fn step(&mut self, model: &Model) {
        // Ensure a deterministic seed position for every current node; drop ghosts.
        let ids: Vec<u8> = model.nodes.keys().copied().collect();
        self.pos.retain(|k, _| model.nodes.contains_key(k));
        self.vel.retain(|k, _| model.nodes.contains_key(k));
        for (i, &id) in ids.iter().enumerate() {
            self.pos.entry(id).or_insert_with(|| {
                let a = GOLDEN_ANGLE * (self.seeded + i as u32) as f32;
                Pos2::new(220.0 * a.cos(), 220.0 * a.sin())
            });
            self.vel.entry(id).or_insert(Vec2::ZERO);
        }
        self.seeded = self.seeded.wrapping_add(ids.len() as u32);

        let edges = model.edges();
        let crown = model.crown.map(|c| c.owner);

        for _ in 0..4 {
            let mut force: HashMap<u8, Vec2> = ids.iter().map(|&id| (id, Vec2::ZERO)).collect();

            // Repulsion (all pairs).
            for i in 0..ids.len() {
                for j in (i + 1)..ids.len() {
                    let (a, b) = (ids[i], ids[j]);
                    let pa = self.pos[&a];
                    let pb = self.pos[&b];
                    let mut d = pa - pb;
                    let mut len2 = d.length_sq();
                    if len2 < 1.0 {
                        // Deterministic jitter to break exact overlaps (no RNG).
                        d = Vec2::new(((a as f32) - (b as f32)).signum().max(0.1), 0.3);
                        len2 = d.length_sq();
                    }
                    let f = d.normalized() * (K_REP / len2);
                    *force.get_mut(&a).unwrap() += f;
                    *force.get_mut(&b).unwrap() -= f;
                }
            }

            // Springs along edges — stronger RSSI => shorter rest length => closer.
            for e in &edges {
                if !self.pos.contains_key(&e.a) || !self.pos.contains_key(&e.b) {
                    continue;
                }
                let rest = rssi_rest_len(e.rssi);
                let pa = self.pos[&e.a];
                let pb = self.pos[&e.b];
                let d = pb - pa;
                let len = d.length().max(0.01);
                let f = d.normalized() * ((len - rest) * K_SPRING);
                *force.get_mut(&e.a).unwrap() += f;
                *force.get_mut(&e.b).unwrap() -= f;
            }

            // Center gravity; crown pulled harder so it anchors the middle.
            for &id in &ids {
                let p = self.pos[&id];
                let pull = if Some(id) == crown { K_CENTER * 6.0 } else { K_CENTER };
                *force.get_mut(&id).unwrap() += (Pos2::ZERO - p) * pull;
            }

            // Integrate.
            for &id in &ids {
                let v = (self.vel[&id] + force[&id]) * DAMPING;
                let v = v.clamp(Vec2::splat(-60.0), Vec2::splat(60.0));
                self.vel.insert(id, v);
                let np = self.pos[&id] + v;
                self.pos.insert(id, np);
            }
        }
    }

    fn fit_camera(&mut self, rect: Rect) {
        if self.pos.is_empty() {
            return;
        }
        let (mut min, mut max) = (Pos2::new(f32::MAX, f32::MAX), Pos2::new(f32::MIN, f32::MIN));
        for p in self.pos.values() {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        let span = (max - min).max(Vec2::splat(1.0));
        let pad = 90.0;
        let target_scale = ((rect.width() - pad) / span.x)
            .min((rect.height() - pad) / span.y)
            .clamp(0.15, 2.2);
        let target_center = min + span / 2.0;
        // Smooth toward target so the camera doesn't jitter with the physics.
        self.cam_center += (target_center - self.cam_center) * 0.12;
        self.cam_scale += (target_scale - self.cam_scale) * 0.12;
    }

    fn to_screen(&self, rect: Rect, p: Pos2) -> Pos2 {
        rect.center() + (p - self.cam_center) * self.cam_scale
    }

    /// Draw the whole graph. Returns a node id if the user clicked one.
    pub fn draw(&mut self, ui: &mut egui::Ui, model: &Model, selected: Option<u8>, now_s: f64) -> Option<u8> {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click());
        let rect = response.rect;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(16, 18, 24));

        self.step(model);
        self.fit_camera(rect);

        let crown = model.crown.map(|c| c.owner);

        // Edges under nodes.
        for e in model.edges() {
            let (Some(&pa), Some(&pb)) = (self.pos.get(&e.a), self.pos.get(&e.b)) else {
                continue;
            };
            let sa = self.to_screen(rect, pa);
            let sb = self.to_screen(rect, pb);
            let mut col = rssi_color(e.rssi);
            // Fade very-stale links.
            if e.age_s > 30 {
                col = col.gamma_multiply(0.45);
            }
            let w = 1.0 + ((e.rssi + 90) as f32 / 60.0).clamp(0.0, 1.0) * 3.0;
            // Weak links are dashed (a derived "weak link" signal — HA parity).
            if e.rssi <= WEAK_LINK_DBM {
                painter.extend(egui::Shape::dashed_line(&[sa, sb], Stroke::new(w, col), 7.0, 5.0));
            } else {
                painter.line_segment([sa, sb], Stroke::new(w, col));
            }
            // RSSI label at midpoint.
            let mid = sa + (sb - sa) * 0.5;
            painter.text(
                mid,
                Align2::CENTER_CENTER,
                format!("{}", e.rssi),
                FontId::proportional(9.0),
                col.gamma_multiply(1.4),
            );
        }

        let mut clicked = None;
        let hover = response.hover_pos();

        // Nodes.
        for (&id, node) in &model.nodes {
            let Some(&p) = self.pos.get(&id) else { continue };
            let sp = self.to_screen(rect, p);
            let stale = node.is_stale(now_s);
            let is_gw = node.gateway || Some(id) == crown;
            let r = if is_gw { 20.0 } else { 15.0 };

            let mut fill = if stale {
                Color32::from_rgb(96, 98, 110)
            } else if Some(id) == crown {
                Color32::from_rgb(255, 205, 70)
            } else if is_gw {
                Color32::from_rgb(230, 165, 60)
            } else {
                Color32::from_rgb(90, 190, 214)
            };
            if node.ota_armed && !stale {
                fill = Color32::from_rgb(210, 120, 235); // installing / armed
            }

            let hovered = hover.map(|h| (h - sp).length() < r + 5.0).unwrap_or(false);
            painter.circle_filled(sp, r, fill);
            painter.circle_stroke(sp, r, Stroke::new(1.5_f32, Color32::from_black_alpha(140)));
            if Some(id) == selected {
                painter.circle_stroke(sp, r + 4.0, Stroke::new(2.5_f32, Color32::WHITE));
            } else if hovered {
                painter.circle_stroke(sp, r + 3.0, Stroke::new(1.5_f32, Color32::from_white_alpha(160)));
            }

            // Node id in the disc.
            painter.text(
                sp,
                Align2::CENTER_CENTER,
                format!("{id}"),
                FontId::proportional(if is_gw { 15.0 } else { 12.0 }),
                Color32::from_rgb(20, 22, 28),
            );

            // Crown badge.
            if Some(id) == crown {
                painter.text(sp + Vec2::new(0.0, -r - 10.0), Align2::CENTER_CENTER, "👑", FontId::proportional(15.0), Color32::from_rgb(255, 215, 90));
            }

            // Label: noun + build.
            let build = node.build().map(|b| format!("  v{b}")).unwrap_or_default();
            let label = format!("{}{}", node.label(), build);
            painter.text(
                sp + Vec2::new(0.0, r + 11.0),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(12.0),
                if stale { Color32::GRAY } else { Color32::from_rgb(222, 226, 236) },
            );

            // Tiny heap sparkline beneath the label.
            draw_sparkline(&painter, sp + Vec2::new(-24.0, r + 22.0), Vec2::new(48.0, 12.0), &node.heap_hist, Color32::from_rgb(120, 200, 150));

            if hovered && response.clicked() {
                clicked = Some(id);
            }
        }

        if model.nodes.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "waiting for mesh traffic on smol/#…",
                FontId::proportional(16.0),
                Color32::from_gray(120),
            );
        }

        clicked
    }
}

/// Strong RSSI => short rest length (nodes cluster); weak => long.
fn rssi_rest_len(rssi: i32) -> f32 {
    let t = ((rssi + 90) as f32 / 60.0).clamp(0.0, 1.0); // 1 strong, 0 weak
    260.0 - t * 150.0
}

/// Green (strong) -> amber -> red (weak).
pub fn rssi_color(rssi: i32) -> Color32 {
    let t = ((rssi + 90) as f32 / 60.0).clamp(0.0, 1.0);
    let (r, g, b) = if t > 0.5 {
        let u = (t - 0.5) * 2.0;
        lerp3((230, 200, 70), (90, 200, 110), u)
    } else {
        let u = t * 2.0;
        lerp3((210, 80, 80), (230, 200, 70), u)
    };
    Color32::from_rgb(r, g, b)
}

fn lerp3(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

/// Small polyline sparkline of a history ring (auto-scaled to its own min/max).
pub fn draw_sparkline(painter: &egui::Painter, origin: Pos2, size: Vec2, hist: &std::collections::VecDeque<[f64; 2]>, color: Color32) {
    if hist.len() < 2 {
        return;
    }
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for p in hist {
        lo = lo.min(p[1]);
        hi = hi.max(p[1]);
    }
    let range = (hi - lo).max(1.0);
    let n = hist.len();
    let pts: Vec<Pos2> = hist
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let x = origin.x + size.x * (i as f32 / (n - 1) as f32);
            let y = origin.y + size.y * (1.0 - ((p[1] - lo) / range) as f32);
            Pos2::new(x, y)
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.2_f32, color)));
}
