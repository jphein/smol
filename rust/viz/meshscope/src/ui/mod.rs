//! The eframe `App`: top status bar, central node graph, right detail panel, bottom
//! event ticker. Reads the shared [`Model`] under its lock once per frame.

pub mod graph;
mod opdock;
mod panel;
mod ticker;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::operator::Publisher;
use mesh_model::model::{ConnState, Model, SyncFreshness};
use graph::GraphLayout;

pub struct MeshscopeApp {
    model: Arc<Mutex<Model>>,
    layout: GraphLayout,
    selected: Option<u8>,
    start: Instant,
    /// `Some` ONLY in `--operator` mode — the publish path. `None` = pure listener
    /// (default), and the operator dock is never shown.
    operator: Option<Publisher>,
    op_state: opdock::OperatorState,
}

impl MeshscopeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        model: Arc<Mutex<Model>>,
        start: Instant,
        selected: Option<u8>,
        operator: Option<Publisher>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        MeshscopeApp { model, layout: GraphLayout::default(), selected, start, operator, op_state: opdock::OperatorState::default() }
    }
}

impl eframe::App for MeshscopeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep animating the force layout + live data.
        ctx.request_repaint_after(Duration::from_millis(33));
        let now_s = self.start.elapsed().as_secs_f64();

        // Decouple the guard from `self` by locking a cloned Arc.
        let model = self.model.clone();
        let m = model.lock().unwrap();

        // Default the inspector to the crown once data arrives (user clicks override).
        if self.selected.is_none() {
            if let Some(c) = m.crown {
                self.selected = Some(c.owner);
            }
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| top_bar(ui, &m, now_s));

        egui::TopBottomPanel::bottom("events")
            .resizable(true)
            .default_height(148.0)
            .min_height(60.0)
            .show(ctx, |ui| {
                ui.add_space(2.0);
                ui.label(RichText::new(format!("event ticker · {} msgs", m.msg_count)).strong().small());
                ticker::show(ui, &m, now_s);
            });

        egui::SidePanel::right("detail").resizable(true).default_width(310.0).min_width(240.0).show(ctx, |ui| {
            panel::show(ui, &m, self.selected, now_s);
        });

        // Operator dock (left) — ONLY in --operator mode. The command surface + the
        // confirmation modal for destructive/fleet actions. Default builds never see it.
        if let Some(publisher) = self.operator.as_ref() {
            egui::SidePanel::left("operator").resizable(true).default_width(290.0).min_width(250.0).show(ctx, |ui| {
                let sel_node = self.selected.and_then(|id| m.nodes.get(&id));
                opdock::show(ui, sel_node, publisher, &mut self.op_state);
            });
            opdock::confirm_modal(ctx, publisher, &mut self.op_state);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(clicked) = self.layout.draw(ui, &m, self.selected, now_s) {
                self.selected = Some(clicked);
            }
        });
    }
}

fn top_bar(ui: &mut egui::Ui, m: &Model, now_s: f64) {
    ui.add_space(3.0);
    ui.horizontal(|ui| {
        ui.heading(RichText::new("meshscope").strong());
        ui.separator();

        let (dot, label) = match m.conn {
            ConnState::Connected => (Color32::from_rgb(90, 210, 120), "connected"),
            ConnState::Connecting => (Color32::from_rgb(230, 200, 70), "connecting"),
            ConnState::Error => (Color32::from_rgb(220, 90, 90), "disconnected"),
        };
        ui.label(RichText::new("●").color(dot));
        ui.label(RichText::new(format!("{label}  ·  {}", m.broker)).small());
        ui.separator();

        match m.crown {
            Some(c) => ui.label(RichText::new(format!("👑 id{} · ch {} · seq {}", c.owner, c.channel, c.seq)).color(Color32::from_rgb(255, 205, 70))),
            None => ui.label(RichText::new("no crown seen").weak()),
        };
        ui.separator();

        // #204/#217 coexist-channel-health — the fleet-critical at-a-glance chip: the crown's
        // uplink AP channel MUST equal the mesh channel or the crown goes bulk-RX-deaf and OTA
        // dies. Mirrors luna-notify's HA coexist tile (green ==, red !=, amber weak-uplink).
        let cx = graph::crown_coexist(m);
        let cx_chip = match cx {
            graph::Coexist::Healthy { ch } => Some(format!("✓ coexist ch{ch}")),
            graph::Coexist::Weak { ch, rssi } => Some(format!("⚠ coexist ch{ch} · uplink {rssi} dBm")),
            graph::Coexist::Violated { ap_ch, mesh_ch } => Some(format!("✖ OFF-CHANNEL · AP ch{ap_ch} ≠ mesh ch{mesh_ch}")),
            graph::Coexist::Unknown => None,
        };
        if let Some(text) = cx_chip {
            ui.label(RichText::new(text).strong().color(graph::coexist_color(cx)));
            ui.separator();
        }

        let gateways = m.nodes.values().filter(|n| n.gateway).count();
        ui.label(format!("{} node(s) · {} gateway", m.nodes.len(), gateways));

        // Fleet build-uniformity + stale count (derived signals — HA parity).
        let builds: std::collections::BTreeSet<&str> = m.nodes.values().filter_map(|n| n.build()).collect();
        match builds.len() {
            0 => {}
            1 => {
                ui.separator();
                ui.label(RichText::new(format!("all v{}", builds.iter().next().unwrap())).color(Color32::from_rgb(120, 200, 150)));
            }
            _ => {
                ui.separator();
                let list = builds.iter().map(|b| format!("v{b}")).collect::<Vec<_>>().join("/");
                ui.label(RichText::new(format!("mixed: {list}")).color(Color32::from_rgb(230, 200, 70)));
            }
        }
        let stale = m.nodes.values().filter(|n| n.is_stale(now_s)).count();
        if stale > 0 {
            ui.separator();
            ui.label(RichText::new(format!("{stale} stale")).color(Color32::from_rgb(220, 130, 90)));
        }
        // Fleet NTP health (derived signal — HA parity): boards whose clock is stale/unsynced.
        let ntp_stale = m
            .nodes
            .values()
            .filter(|n| matches!(n.sync_freshness(), SyncFreshness::Stale | SyncFreshness::Unsynced))
            .count();
        if ntp_stale > 0 {
            ui.separator();
            ui.label(RichText::new(format!("{ntp_stale} NTP-stale")).color(Color32::from_rgb(224, 100, 90)));
        }

        // Right-aligned HA battery/grid readout if present.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(b) = &m.batt {
                ui.label(RichText::new(b.replace('|', " ")).small().monospace().color(Color32::from_rgb(150, 200, 160)));
            }
        });
    });
    ui.add_space(3.0);
}
