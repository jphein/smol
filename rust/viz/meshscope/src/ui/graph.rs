//! The node-graph canvas: a light force-directed layout (deterministic seed per id,
//! so it settles the same way every run) drawn on an egui `Painter`. Discs coloured
//! by role/liveness, RSSI-weighted edges, a crown badge on the elected owner, and a
//! tiny heap sparkline under each node.

use std::collections::HashMap;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

use mesh_model::model::{Model, SyncFreshness, WEAK_LINK_DBM};
use mesh_model::parse::OtaSource;

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
                col, // (was gamma_multiply(1.4) — factor>1 panics ecolor's debug assert)
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

            // #159 — live screen ("the familiar") under the label.
            if let Some(screen) = node.screen() {
                painter.text(
                    sp + Vec2::new(0.0, r + 22.0),
                    Align2::CENTER_CENTER,
                    screen,
                    FontId::proportional(10.5),
                    if stale { Color32::from_gray(120) } else { Color32::from_rgb(150, 200, 235) },
                );
            }

            // #159 — NTP-sync freshness dot at the top-right of the disc.
            let ndot = sp + Vec2::new(r * 0.72, -r * 0.72);
            painter.circle_filled(ndot, 4.0, sync_color(node.sync_freshness()));
            painter.circle_stroke(ndot, 4.0, Stroke::new(1.0_f32, Color32::from_black_alpha(130)));

            // #190/#249 — fleet-visible security alert at the top-LEFT (mirrors the sync dot).
            // Red = ledger tamper canary tripped (lgok=0); amber = HMAC forgeries rejected (mf>0).
            // Pulses so it reads across the whole graph without opening the inspector; tamper wins.
            if let Some(d) = &node.diag {
                let tamper = d.u64("lgok") == Some(0);
                let forgery = d.u64("mf").is_some_and(|f| f > 0);
                if tamper || forgery {
                    let pulse = (0.5 + 0.5 * (now_s * 5.0).sin() as f32).clamp(0.3, 1.0);
                    let col = if tamper {
                        Color32::from_rgb(240, 70, 70)
                    } else {
                        Color32::from_rgb(235, 175, 80)
                    };
                    painter.text(
                        sp + Vec2::new(-r * 0.72, -r * 0.72),
                        Align2::CENTER_CENTER,
                        "⚠",
                        FontId::proportional(13.0),
                        col.gamma_multiply(if stale { 0.45 } else { pulse }),
                    );
                }
            }

            // #188 — LIVE OTA transfer. A real progress ARC (%+phase) now that the firmware publishes
            // smol/<id>/ota/progress, OR a LOUD death-point when that record goes stale mid-flight
            // (the transfer stopped AT `done` bytes — exactly the diagnostic we lacked). Falls back to
            // the old indeterminate pulse only when a node reports in_progress with no live progress
            // record yet (pre-first-publish / older firmware).
            if let Some((frac, dead, prog)) = node.ota_progress_view(now_s) {
                let ring_r = r + 5.0;
                if dead {
                    // Death-point: a loud red pulsing full ring + the exact byte count it died at.
                    let pulse = (0.55 + 0.45 * (now_s * 6.0).sin() as f32).clamp(0.15, 1.0);
                    painter.circle_stroke(sp, ring_r, Stroke::new(3.5_f32, Color32::from_rgb(240, 60, 60).gamma_multiply(pulse)));
                    painter.text(
                        sp + Vec2::new(0.0, -r - 12.0),
                        Align2::CENTER_CENTER,
                        format!("✖ DIED {}k/{}k", prog.done / 1024, prog.total / 1024),
                        FontId::proportional(11.0),
                        Color32::from_rgb(255, 120, 120),
                    );
                } else if !stale {
                    // Live: a cyan-green arc sweeping `frac` of the ring, clockwise from the top.
                    let segs = 40usize;
                    let sweep = frac.max(0.02) * std::f32::consts::TAU;
                    let col = Color32::from_rgb(70, 210, 170);
                    let mut prev: Option<Pos2> = None;
                    for i in 0..=segs {
                        let a = -std::f32::consts::FRAC_PI_2 + sweep * (i as f32 / segs as f32);
                        let pt = sp + Vec2::new(a.cos() * ring_r, a.sin() * ring_r);
                        if let Some(pv) = prev {
                            painter.line_segment([pv, pt], Stroke::new(3.0_f32, col));
                        }
                        prev = Some(pt);
                    }
                    if Some(id) != crown {
                        painter.text(
                            sp + Vec2::new(0.0, -r - 12.0),
                            Align2::CENTER_CENTER,
                            format!("OTA {}% {}", (frac * 100.0) as u32, prog.phase),
                            FontId::proportional(10.5),
                            Color32::from_rgb(120, 230, 200),
                        );
                    }
                }
            } else if let Some(o) = &node.ota {
                if o.in_progress && !stale {
                    let pulse = (0.5 + 0.4 * ((now_s * 4.0).sin() as f32 * 0.5 + 0.5)).clamp(0.2, 1.0);
                    painter.circle_stroke(sp, r + 5.0, Stroke::new(2.5_f32, Color32::from_rgb(210, 120, 235).gamma_multiply(pulse)));
                    if Some(id) != crown {
                        painter.text(
                            sp + Vec2::new(0.0, -r - 11.0),
                            Align2::CENTER_CENTER,
                            format!("OTA →v{}", o.latest),
                            FontId::proportional(10.5),
                            Color32::from_rgb(222, 150, 240),
                        );
                    }
                }
            }

            // #237 peer-source baton: tag a node whose last OTA was served by a peer HOLDER
            // (src=id<n>) over ESP-NOW — a small "⇄id<n>" to the right of the disc so a
            // peer-sourced board reads as visually distinct from a plain gateway fetch (which is
            // the default and stays untagged). The visible outcome of the ODEL→serve→ODON baton.
            if let Some(OtaSource::Peer(pid)) = node.ota_src {
                painter.text(
                    sp + Vec2::new(r + 6.0, r * 0.55),
                    Align2::LEFT_CENTER,
                    format!("⇄id{pid}"),
                    FontId::proportional(10.5),
                    if stale { Color32::from_gray(120) } else { Color32::from_rgb(170, 220, 120) },
                );
            }

            // Tiny heap sparkline beneath the screen line.
            draw_sparkline(&painter, sp + Vec2::new(-24.0, r + 33.0), Vec2::new(48.0, 12.0), &node.heap_hist, Color32::from_rgb(120, 200, 150));

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
/// Coexist-channel-health (#204/#217): the crown's WiFi-uplink AP channel MUST equal the
/// elected ESP-NOW mesh channel, or the crown goes bulk-RX-deaf mid-fetch and OTA dies (the
/// ch1-AP-vs-ch6-mesh bug that cost a night of pcap). `Weak` = channels match but the uplink
/// is at/under `WEAK_LINK_DBM` (−80 dBm), the stacked factor seen in the disease. `Unknown` =
/// no crown seen yet, or the crown hasn't published its `ap=` association. Shared by the
/// top-bar chip and the crown detail line so both read identically — and by design matched to
/// the HA coexist tile (luna-notify): green ==, red !=, amber weak.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coexist {
    Healthy { ch: u8 },
    Weak { ch: u8, rssi: i32 },
    Violated { ap_ch: u8, mesh_ch: u8 },
    Unknown,
}

/// Classify the CURRENT crown's coexist health. The crown is the MC `owner`; we read that
/// node's DIAG `ap=` and compare its channel to the elected mesh channel. (A future fw `cc=`
/// flag — morpheus — could override this as the board's own verdict; the computed comparison
/// is the ground truth available today.)
pub fn crown_coexist(m: &Model) -> Coexist {
    let Some(crown) = m.crown else {
        return Coexist::Unknown;
    };
    let Some(ap) = m.nodes.get(&crown.owner).and_then(|n| n.ap()) else {
        return Coexist::Unknown;
    };
    if ap.channel != crown.channel {
        Coexist::Violated { ap_ch: ap.channel, mesh_ch: crown.channel }
    } else if ap.rssi <= WEAK_LINK_DBM {
        Coexist::Weak { ch: ap.channel, rssi: ap.rssi }
    } else {
        Coexist::Healthy { ch: ap.channel }
    }
}

pub fn coexist_color(c: Coexist) -> Color32 {
    match c {
        Coexist::Healthy { .. } => Color32::from_rgb(90, 210, 120),
        Coexist::Weak { .. } => Color32::from_rgb(230, 200, 70),
        Coexist::Violated { .. } => Color32::from_rgb(224, 90, 90),
        Coexist::Unknown => Color32::from_rgb(130, 130, 140),
    }
}

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

/// NTP-sync freshness colour (shared with the detail panel): green fresh → amber aging
/// → red stale → grey unsynced.
pub fn sync_color(f: SyncFreshness) -> Color32 {
    match f {
        SyncFreshness::Fresh => Color32::from_rgb(90, 210, 120),
        SyncFreshness::Aging => Color32::from_rgb(230, 200, 70),
        SyncFreshness::Stale => Color32::from_rgb(224, 100, 90),
        SyncFreshness::Unsynced => Color32::from_rgb(130, 130, 142),
    }
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

#[cfg(test)]
mod coexist_tests {
    use super::*;
    use mesh_model::model::Model;

    fn crown_model(mc: &str, crown_diag: &str) -> Model {
        let mut m = Model::new("test".into());
        m.ingest(1.0, "smol/mesh/channel", mc.as_bytes());
        m.ingest(1.0, "smol/7/diag", crown_diag.as_bytes());
        m
    }

    #[test]
    fn healthy_when_ap_channel_matches_mesh() {
        let m = crown_model("MC|7|6|10", "DIAG|boot=1|heap=40000|ap=6:-58:a1b2c3d4e5f6");
        assert_eq!(crown_coexist(&m), Coexist::Healthy { ch: 6 });
    }

    #[test]
    fn violated_on_channel_mismatch() {
        // The exact ch1-AP-vs-ch6-mesh bug that made the crown bulk-RX-deaf → OTA-dead.
        let m = crown_model("MC|7|6|10", "DIAG|boot=1|heap=40000|ap=1:-58:a1b2c3d4e5f6");
        assert_eq!(crown_coexist(&m), Coexist::Violated { ap_ch: 1, mesh_ch: 6 });
    }

    #[test]
    fn weak_when_matched_but_faint_uplink() {
        // Channels agree but the uplink is at/under WEAK_LINK_DBM (−80) — the stacked factor.
        let m = crown_model("MC|7|6|10", "DIAG|boot=1|heap=40000|ap=6:-82:a1b2c3d4e5f6");
        assert_eq!(crown_coexist(&m), Coexist::Weak { ch: 6, rssi: -82 });
    }

    #[test]
    fn unknown_without_crown_or_ap() {
        // No crown record → unknown.
        let mut m = Model::new("test".into());
        m.ingest(1.0, "smol/7/diag", b"DIAG|boot=1|heap=40000|ap=6:-58:a1b2c3d4e5f6");
        assert_eq!(crown_coexist(&m), Coexist::Unknown);
        // Crown known but the crown node hasn't published an ap= yet → unknown, not a false green.
        let m2 = crown_model("MC|7|6|10", "DIAG|boot=1|heap=40000");
        assert_eq!(crown_coexist(&m2), Coexist::Unknown);
    }
}
