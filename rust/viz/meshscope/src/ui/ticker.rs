//! Bottom event ticker — the scrolling mesh log: crown changes, version flips, OTA
//! fetch/retries, joins. Newest at the bottom, auto-scrolled into view.

use egui::{Color32, RichText, ScrollArea};

use mesh_model::model::{EventKind, Model};

pub fn show(ui: &mut egui::Ui, model: &Model, now_s: f64) {
    ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
        if model.events.is_empty() {
            ui.label(RichText::new("no events yet").weak().small());
            return;
        }
        for ev in &model.events {
            let age = (now_s - ev.t_s).max(0.0) as u64;
            let (icon_col, _) = kind_style(ev.kind);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{:>4}s", age)).monospace().small().weak());
                ui.label(RichText::new(&ev.text).color(icon_col).small());
            });
        }
    });
}

fn kind_style(k: EventKind) -> (Color32, &'static str) {
    match k {
        EventKind::Crown => (Color32::from_rgb(255, 205, 70), "crown"),
        EventKind::Version => (Color32::from_rgb(130, 200, 255), "version"),
        EventKind::Ota => (Color32::from_rgb(210, 140, 235), "ota"),
        EventKind::Join => (Color32::from_rgb(120, 200, 150), "join"),
    }
}
