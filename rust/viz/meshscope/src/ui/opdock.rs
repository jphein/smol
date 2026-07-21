//! Operator dock (#23) — the command surface, rendered ONLY in `--operator` mode. A
//! non-destructive control publishes on click; a destructive/fleet-wide one is routed
//! through [`confirm_modal`] first. All publishing goes through [`crate::operator`],
//! whose typed builders fix `retain` per command (transient reset/scan can't be retained).

use egui::{Color32, RichText};

use crate::operator::{self, HeraldAlign, HeraldSize, PublishReq, Publisher};
use mesh_model::model::Node;

/// Screen names = the exact `app.rs` `AppKind` wire spellings (single-source parity). Matches
/// the #24 HA dashboard select (Menu·Clock·Batt·Grid·Snake·Bench·MeshSnake·About·Custom) plus
/// `Familiar` (a screen boards report but the HA select currently lags — flagged to #25).
/// app.rs also has Watch/Hunt/Finder; omitted here until confirmed settable-as-default.
const APPKINDS: &[&str] =
    &["Menu", "Clock", "Batt", "Grid", "Snake", "Bench", "MeshSnake", "About", "Custom", "Familiar"];
const LED_MODES: &[&str] = &["status", "on", "off"];
/// (display label, wire token). The wire token is PIPE-separated `<F|C>|<24|12>` — the fw's
/// `units::from_wire` splits on `|` and matches the halves exactly, so a joined `F24` parses
/// to None and the board silently keeps its units (#46 clamp). Mirrors the HA payload (#25).
const UNITS: &[(&str, &str)] = &[("°C 24h", "C|24"), ("°C 12h", "C|12"), ("°F 24h", "F|24"), ("°F 12h", "F|12")];

/// Operator UI input state (text buffers + the pending confirmation + last-published).
#[derive(Default)]
pub struct OperatorState {
    screen_kind: String,
    screen_page: String,
    plugins_hex: String,
    /// #197 custom-screen composer: the message text + its size/align/duration/priority.
    custom: String,
    custom_size: HeraldSize,
    custom_align: HeraldAlign,
    custom_dur: String,
    custom_priority: bool,
    io_map: String,
    io_set: String,
    broker: String,
    ota_host: String,
    channel_hint: String,
    notify_msg: String,
    notify_dur: String,
    pending: Option<PublishReq>,
    last: Option<String>,
}

impl OperatorState {
    /// Apply a chosen action: destructive → queue for the modal; else publish now.
    fn dispatch(&mut self, publisher: &Publisher, req: PublishReq) {
        if req.destructive {
            self.pending = Some(req);
        } else {
            let s = req.summary();
            publisher.send(&req);
            self.last = Some(s);
        }
    }
}

/// Render the operator dock. Accumulates at most ONE action per frame, then dispatches
/// it after the widgets (keeps field-borrows disjoint from the &mut-self dispatch).
pub fn show(ui: &mut egui::Ui, sel: Option<&Node>, publisher: &Publisher, st: &mut OperatorState) {
    let mut action: Option<PublishReq> = None;

    ui.add_space(4.0);
    ui.label(RichText::new("⚡ OPERATOR — publishing armed").strong().color(Color32::from_rgb(240, 180, 60)));
    ui.label(RichText::new("Controls publish to smol/* on the live broker.").small().weak());
    ui.separator();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        // ---- Per-node ----
        match sel {
            None => {
                ui.label(RichText::new("Select a node for per-node controls.").weak().small());
            }
            Some(n) => {
                let id = n.id;
                ui.label(RichText::new(format!("id {} · {}", id, n.label())).strong());

                // OTA install (idempotent; show staged-vs-installed skew like HA's Update entity).
                if let Some(o) = &n.ota {
                    let behind = !o.latest.is_empty() && o.latest != o.installed;
                    ui.horizontal(|ui| {
                        ui.label(format!("build v{}", o.installed));
                        if behind {
                            ui.label(RichText::new(format!("→ v{} available", o.latest)).color(Color32::from_rgb(130, 200, 255)));
                        }
                    });
                    let label = if behind { format!("⬆ Install v{}", o.latest) } else { "Install (up to date)".to_string() };
                    if ui.add_enabled(behind, egui::Button::new(label)).clicked() {
                        action = Some(operator::install(id));
                    }
                    if n.ota_armed {
                        ui.label(RichText::new("🎯 install armed").small().color(Color32::from_rgb(210, 120, 235)));
                    }
                } else {
                    ui.label(RichText::new("no ota/state yet").weak().small());
                }
                ui.separator();

                // Default screen.
                ui.label(RichText::new("display").small().weak());
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("op_screen")
                        .selected_text(if st.screen_kind.is_empty() { "screen" } else { st.screen_kind.as_str() })
                        .show_ui(ui, |ui| {
                            for k in APPKINDS {
                                ui.selectable_value(&mut st.screen_kind, (*k).to_string(), *k);
                            }
                        });
                    ui.add(egui::TextEdit::singleline(&mut st.screen_page).desired_width(26.0).hint_text("pg"));
                    if ui.add_enabled(!st.screen_kind.is_empty(), egui::Button::new("Set")).clicked() {
                        let page = st.screen_page.trim().parse::<u8>().unwrap_or(0);
                        action = Some(operator::default_screen(id, &st.screen_kind.clone(), page));
                    }
                });

                // LED.
                ui.horizontal(|ui| {
                    ui.label("LED");
                    for mode in LED_MODES {
                        if ui.button(*mode).clicked() {
                            action = Some(operator::led(id, mode));
                        }
                    }
                });

                // Plugins (hex mask).
                ui.horizontal(|ui| {
                    ui.label("Plugins hex");
                    ui.add(egui::TextEdit::singleline(&mut st.plugins_hex).desired_width(54.0).hint_text("1f"));
                    let parsed = u16::from_str_radix(st.plugins_hex.trim(), 16).ok();
                    if ui.add_enabled(parsed.is_some(), egui::Button::new("Set")).clicked() {
                        if let Some(mask) = parsed {
                            action = Some(operator::plugins(id, mask));
                        }
                    }
                });

                // #197 Custom-screen composer — mirrors HA's herald composer; emits the fw #45
                // wire via `compose_custom` so a board renders identically from HA or here.
                ui.label(RichText::new("custom screen (composed)").small().weak());
                ui.add(egui::TextEdit::singleline(&mut st.custom).desired_width(170.0).hint_text("message"));
                ui.horizontal(|ui| {
                    ui.label("size");
                    ui.selectable_value(&mut st.custom_size, HeraldSize::Small, "S");
                    ui.selectable_value(&mut st.custom_size, HeraldSize::Medium, "M");
                    ui.selectable_value(&mut st.custom_size, HeraldSize::Large, "L");
                    ui.separator();
                    ui.label("align");
                    ui.selectable_value(&mut st.custom_align, HeraldAlign::Left, "L");
                    ui.selectable_value(&mut st.custom_align, HeraldAlign::Center, "C");
                    ui.selectable_value(&mut st.custom_align, HeraldAlign::Right, "R");
                });
                ui.horizontal(|ui| {
                    ui.label("secs");
                    ui.add(egui::TextEdit::singleline(&mut st.custom_dur).desired_width(28.0).hint_text("0"));
                    ui.checkbox(&mut st.custom_priority, "priority");
                });
                let cust_msg = st.custom.clone();
                let have_cust = !cust_msg.trim().is_empty();
                // WYSIWYG preview: the wrapped rows exactly as the fw renders them at this size.
                if have_cust {
                    let rows = operator::compose_rows(&cust_msg, st.custom_size);
                    ui.label(RichText::new("preview (on glass):").small().weak());
                    egui::Frame::NONE.fill(Color32::from_rgb(18, 20, 26)).inner_margin(3).show(ui, |ui| {
                        for r in &rows {
                            ui.label(RichText::new(r).monospace().color(Color32::from_rgb(215, 220, 230)));
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(have_cust, egui::Button::new("Set custom")).clicked() {
                        let dur = st.custom_dur.trim().parse::<u16>().ok();
                        let wire = operator::compose_custom(&cust_msg, st.custom_size, st.custom_align, dur, st.custom_priority);
                        action = Some(operator::custom(id, &wire));
                    }
                    // Empty retained payload clears the custom screen (retain-delete → noun fallback).
                    if ui.button("Clear").clicked() {
                        action = Some(operator::custom(id, ""));
                    }
                });

                // IO pin-map + output states.
                ui.horizontal(|ui| {
                    ui.label("IO map");
                    ui.add(egui::TextEdit::singleline(&mut st.io_map).desired_width(100.0).hint_text("0L;7B"));
                    if ui.add_enabled(!st.io_map.is_empty(), egui::Button::new("Set")).clicked() {
                        action = Some(operator::io_map(id, &st.io_map.clone()));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("IO set");
                    ui.add(egui::TextEdit::singleline(&mut st.io_set).desired_width(100.0).hint_text("0=1;10=0"));
                    if ui.add_enabled(!st.io_set.is_empty(), egui::Button::new("Set")).clicked() {
                        action = Some(operator::io_set(id, &st.io_set.clone()));
                    }
                });
                ui.separator();

                // Network / power (all confirm-gated except scan).
                ui.label(RichText::new("network / power  (⚠ confirm)").small().weak());
                ui.horizontal(|ui| {
                    ui.label("Broker");
                    ui.add(egui::TextEdit::singleline(&mut st.broker).desired_width(120.0).hint_text("host:port"));
                    if ui.add_enabled(!st.broker.is_empty(), egui::Button::new("Set")).clicked() {
                        action = Some(operator::broker(id, &st.broker.clone()));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("OTA host");
                    ui.add(egui::TextEdit::singleline(&mut st.ota_host).desired_width(120.0).hint_text("rfc1918 host"));
                    if ui.add_enabled(!st.ota_host.is_empty(), egui::Button::new("Set")).clicked() {
                        action = Some(operator::ota_host(id, &st.ota_host.clone()));
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("🔍 Scan").clicked() {
                        action = Some(operator::scan(id));
                    }
                    if ui.button(RichText::new("⟳ Reboot").color(Color32::from_rgb(235, 120, 90))).clicked() {
                        action = Some(operator::reboot(id));
                    }
                });
            }
        }

        ui.separator();
        // ---- Fleet (v1: units + channel_hint only) ----
        ui.label(RichText::new("FLEET  (⚠ confirm)").strong().color(Color32::from_rgb(230, 200, 70)));
        ui.horizontal(|ui| {
            ui.label("Units");
            for (label, token) in UNITS {
                if ui.button(*label).clicked() {
                    action = Some(operator::units(token));
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Channel hint");
            ui.add(egui::TextEdit::singleline(&mut st.channel_hint).desired_width(36.0).hint_text("6"));
            let ch = st.channel_hint.trim().parse::<u8>().ok();
            if ui.add_enabled(ch.is_some(), egui::Button::new("Set")).clicked() {
                action = Some(operator::channel_hint(ch));
            }
            if ui.button("Clear").clicked() {
                action = Some(operator::channel_hint(None));
            }
        });

        // ---- #197 herald: send a transient on-glass toast to one node or the whole fleet ----
        ui.separator();
        ui.label(RichText::new("message → glass (toast)").small().weak());
        ui.add(egui::TextEdit::singleline(&mut st.notify_msg).desired_width(170.0).hint_text("hello mesh"));
        let have_msg = !st.notify_msg.trim().is_empty();
        let msg = st.notify_msg.clone();
        ui.horizontal(|ui| {
            ui.label("secs");
            ui.add(egui::TextEdit::singleline(&mut st.notify_dur).desired_width(28.0).hint_text("5"));
            let dur = st.notify_dur.trim().parse::<u16>().ok();
            if let Some(n) = sel {
                if ui.add_enabled(have_msg, egui::Button::new(format!("Send → id{}", n.id))).clicked() {
                    action = Some(operator::notify(n.id, dur, &msg));
                }
            }
            // Fleet-wide (255) is destructive → routed through the confirm modal.
            if ui.add_enabled(have_msg, egui::Button::new(RichText::new("Send → ALL").color(Color32::from_rgb(230, 200, 70)))).clicked() {
                action = Some(operator::notify_fleet(dur, &msg));
            }
        });
        // WYSIWYG preview: exactly how the fw toast will wrap it (12 cols / 3 rows), on the
        // SANITIZED message that goes on the wire — what-you-type-is-what-renders.
        if have_msg {
            let lines = operator::wrap_preview(&msg);
            ui.label(RichText::new("preview (on glass):").small().weak());
            egui::Frame::NONE.fill(Color32::from_rgb(18, 20, 26)).inner_margin(3).show(ui, |ui| {
                for l in &lines {
                    ui.label(RichText::new(l).monospace().color(Color32::from_rgb(215, 220, 230)));
                }
            });
        }

        // ---- Last published ----
        if let Some(last) = &st.last {
            ui.separator();
            ui.label(RichText::new("last published").small().weak());
            ui.label(RichText::new(last).small().monospace().color(Color32::from_rgb(150, 200, 160)));
        }
    });

    if let Some(req) = action {
        st.dispatch(publisher, req);
    }
}

/// The confirmation modal for a queued destructive/fleet action. Shows the EXACT
/// topic + payload + retain before anything is published — the operator's last check.
pub fn confirm_modal(ctx: &egui::Context, publisher: &Publisher, st: &mut OperatorState) {
    let Some(req) = st.pending.clone() else {
        return;
    };
    let mut close = false;
    egui::Window::new(RichText::new("⚠ Confirm command").color(Color32::from_rgb(235, 150, 60)))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label("Destructive or fleet-wide. Confirm the exact publish:");
            ui.add_space(4.0);
            ui.label(RichText::new(req.summary()).monospace());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Confirm & publish").color(Color32::from_rgb(235, 120, 90))).clicked() {
                    let s = req.summary();
                    publisher.send(&req);
                    st.last = Some(s);
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
    if close {
        st.pending = None;
    }
}
