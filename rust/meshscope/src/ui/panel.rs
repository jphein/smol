//! Right-hand per-node detail panel: identity, DIAG health fields, OTA state, peer
//! links, and heap / uplink-RSSI sparklines (egui_plot).

use egui::{Color32, RichText};
use egui_plot::{Line, Plot, PlotPoints};

use crate::model::{Model, Node};

pub fn show(ui: &mut egui::Ui, model: &Model, selected: Option<u8>, now_s: f64) {
    let Some(id) = selected.and_then(|i| if model.nodes.contains_key(&i) { Some(i) } else { None }) else {
        ui.add_space(8.0);
        ui.label(RichText::new("Select a node").weak());
        ui.label(RichText::new("Click a disc in the graph to inspect its health, OTA state and links.").weak().small());
        return;
    };
    let node = &model.nodes[&id];

    ui.add_space(4.0);
    let title = format!("id {} · {}", node.id, node.label());
    ui.heading(RichText::new(title).color(role_color(node, model)));
    let age = now_s - node.last_seen_s;
    let role = if node.gateway { "gateway" } else { "leaf" };
    ui.label(format!(
        "{role}{}  ·  ch {}  ·  seen {}s ago",
        if Some(node.id) == model.crown.map(|c| c.owner) { " · 👑 crown" } else { "" },
        node.channel.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
        age as u64
    ));

    if let Some(t) = &node.telemetry {
        ui.add_space(2.0);
        ui.label(RichText::new(t).monospace().small());
    }

    ui.separator();

    // --- OTA ---
    if let Some(o) = &node.ota {
        ui.label(RichText::new("OTA").strong());
        kv(ui, "installed", &format!("v{}", o.installed));
        if o.latest != o.installed {
            kv(ui, "latest", &format!("v{} available", o.latest));
        }
        if o.in_progress {
            ui.label(RichText::new("⏳ install in progress").color(Color32::from_rgb(210, 120, 235)));
        }
        if node.ota_armed {
            ui.label(RichText::new("🎯 install armed").color(Color32::from_rgb(210, 120, 235)));
        }
        ui.separator();
    }

    // --- DIAG health ---
    if let Some(d) = &node.diag {
        ui.label(RichText::new("health (DIAG)").strong());
        egui::Grid::new("diag_grid").num_columns(2).striped(true).show(ui, |ui| {
            let row = |ui: &mut egui::Ui, k: &str, v: Option<String>| {
                if let Some(v) = v {
                    ui.label(RichText::new(k).weak());
                    ui.label(v);
                    ui.end_row();
                }
            };
            row(ui, "uptime", d.u64("up").map(|s| fmt_dur(s)));
            row(ui, "boot #", d.u64("boot").map(|b| b.to_string()));
            row(ui, "reset", d.get("rst").map(|s| s.to_string()));
            row(ui, "heap free", d.u64("heap").map(fmt_bytes));
            row(ui, "heap min", d.u64("hmin").map(fmt_bytes));
            row(ui, "time src", d.get("tsrc").map(|s| s.to_string()));
            row(ui, "time age", d.u64("tage").map(|s| format!("{s}s")));
            row(ui, "loss", d.get("loss").map(|s| format!("{s}%")));
            row(ui, "rtt", d.get("rtt").map(|s| format!("{s}ms")));
            row(ui, "rx / tx", match (d.get("rx"), d.get("tx")) {
                (Some(r), Some(t)) => Some(format!("{r} / {t}")),
                _ => None,
            });
            row(ui, "led", d.led().map(|(m, on)| format!("{m}:{}", if on { "on" } else { "off" })));
            row(ui, "broker", d.get("brk").map(|s| s.to_string()));
            row(ui, "hop", d.get("hop").map(|s| s.to_string()));
            row(ui, "fwd/dedup/ttl", match (d.get("fwd"), d.get("dedup"), d.get("ttl")) {
                (Some(f), Some(de), Some(t)) => Some(format!("{f} / {de} / {t}")),
                _ => None,
            });
            row(ui, "dlseq/dfwd", match (d.get("dlseq"), d.get("dfwd")) {
                (Some(a), Some(b)) => Some(format!("{a} / {b}")),
                _ => None,
            });
            row(ui, "cfg echo", d.get("cfg").map(|s| s.to_string()));
        });
        ui.separator();
    }

    // --- sparklines ---
    if let Some(rssi) = node.uplink_rssi {
        kv(ui, "uplink RSSI", &format!("{rssi} dBm"));
    }
    sparkline_plot(ui, "heap free", &node.heap_hist, Color32::from_rgb(120, 200, 150));
    sparkline_plot(ui, "uplink RSSI", &node.rssi_hist, Color32::from_rgb(120, 170, 240));

    // --- peers heard ---
    if !node.links.is_empty() {
        ui.separator();
        ui.label(RichText::new(format!("hears {} peer(s)", node.links.len())).strong());
        for l in &node.links {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("id{}", l.id)).monospace());
                ui.label(RichText::new(format!("{} dBm", l.rssi)).color(super::graph::rssi_color(l.rssi)));
                ui.label(RichText::new(format!("· {}s", l.age_s)).weak().small());
                if l.connected {
                    ui.label(RichText::new("· linked").small().color(Color32::from_rgb(120, 200, 150)));
                }
            });
        }
    }
}

fn role_color(node: &Node, model: &Model) -> Color32 {
    if Some(node.id) == model.crown.map(|c| c.owner) {
        Color32::from_rgb(255, 205, 70)
    } else if node.gateway {
        Color32::from_rgb(230, 165, 60)
    } else {
        Color32::from_rgb(120, 200, 220)
    }
}

fn kv(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(k).weak());
        ui.label(v);
    });
}

fn sparkline_plot(ui: &mut egui::Ui, name: &str, hist: &std::collections::VecDeque<[f64; 2]>, color: Color32) {
    if hist.len() < 2 {
        return;
    }
    ui.label(RichText::new(name).small().weak());
    let pts: PlotPoints = hist.iter().copied().collect();
    Plot::new(name)
        .height(52.0)
        .show_axes([false, true])
        .show_grid(false)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .show(ui, |plot_ui| {
            plot_ui.line(Line::new(pts).color(color).width(1.5_f32));
        });
}

fn fmt_bytes(b: u64) -> String {
    if b >= 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{b} B")
    }
}

fn fmt_dur(s: u64) -> String {
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
    }
}
