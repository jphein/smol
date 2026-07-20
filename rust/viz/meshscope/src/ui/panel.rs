//! Right-hand per-node detail panel: identity, DIAG health fields, OTA state, peer
//! links, and heap / uplink-RSSI sparklines (egui_plot).

use egui::{Color32, RichText};
use egui_plot::{Line, Plot, PlotPoints};

use mesh_model::model::{Model, Node, SyncFreshness, LOW_HEAP_B};
use mesh_model::parse::OtaSource;

pub fn show(ui: &mut egui::Ui, model: &Model, selected: Option<u8>, now_s: f64) {
    let Some(id) = selected.filter(|i| model.nodes.contains_key(i)) else {
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

    // #159 — the live screen ("the familiar") + NTP-sync freshness, up front.
    if let Some(screen) = node.screen() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("screen").weak());
            ui.label(RichText::new(screen).strong().color(Color32::from_rgb(150, 200, 235)));
        });
    }
    {
        let fresh = node.sync_freshness();
        let word = match fresh {
            SyncFreshness::Fresh => "fresh",
            SyncFreshness::Aging => "aging",
            SyncFreshness::Stale => "STALE",
            SyncFreshness::Unsynced => "unsynced",
        };
        let detail = match (node.time_src(), node.time_age()) {
            (Some(src), Some(age)) => format!("{src} · {} ago", fmt_dur(age)),
            (Some(src), None) => src.to_string(),
            (None, Some(age)) => format!("{} ago", fmt_dur(age)),
            (None, None) => "—".into(),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("time sync").weak());
            ui.label(RichText::new(format!("{detail}  ({word})")).color(super::graph::sync_color(fresh)));
        });
    }

    if let Some(t) = &node.telemetry {
        ui.add_space(2.0);
        ui.label(RichText::new(t).monospace().small());
    }

    ui.separator();

    // --- OTA ---
    if let Some(o) = &node.ota {
        ui.label(RichText::new("OTA").strong());
        // Show the firmware's own version title (carries the sigil word, e.g. "v342 Jig") when the
        // board publishes one; fall back to the bare number for pre-sigil builds / empty titles.
        let installed_disp = if o.title.trim().is_empty() {
            format!("v{}", o.installed)
        } else {
            o.title.clone()
        };
        kv(ui, "installed", &installed_disp);
        if o.latest != o.installed {
            kv(ui, "latest", &format!("v{} available", o.latest));
        }
        if o.in_progress {
            ui.label(
                RichText::new(format!("⟳ updating  v{} → v{}", o.installed, o.latest))
                    .strong()
                    .color(Color32::from_rgb(220, 140, 240)),
            );
        }
        // #188 live transfer progress + death-point (smol/<id>/ota/progress). The bytes/% is the
        // live "watch it happen" signal the old inspector couldn't show; a DIED line pins exactly
        // where a stalled transfer stopped.
        if let Some((frac, dead, prog)) = node.ota_progress_view(now_s) {
            let line = format!(
                "{} / {} KB  ({}%, {})",
                prog.done / 1024,
                prog.total / 1024,
                (frac * 100.0) as u32,
                prog.phase
            );
            if dead {
                ui.label(
                    RichText::new(format!("✖ transfer DIED @ {line}"))
                        .strong()
                        .color(Color32::from_rgb(255, 110, 110)),
                );
            } else {
                ui.label(
                    RichText::new(format!("↓ {line}"))
                        .monospace()
                        .color(Color32::from_rgb(120, 230, 200)),
                );
            }
        }
        if node.ota_armed {
            ui.label(RichText::new("🎯 install armed").color(Color32::from_rgb(210, 120, 235)));
        }
        // Latest transfer phase from smol/<id>/ota/diag ("fetch-failed retry=3", terminal
        // outcomes). No live % is published to MQTT, so this phase + the version flip on
        // completion are the honest "watch it happen" signals.
        if let Some(phase) = &node.ota_phase {
            ui.label(RichText::new(format!("phase: {phase}")).small().monospace().color(Color32::from_rgb(190, 165, 220)));
        }
        // #237 peer-sourcing — WHO served this node's last OTA. `gateway` is the normal WiFi
        // fetch; a peer `id<n>` means a HOLDER served it over ESP-NOW (the baton — the visible
        // outcome of the crown's ODEL delegation + the holder's ODON), a fetch the gateway never
        // had to make. The distinction is the metric that proves peer-sourcing saved a fetch.
        match node.ota_src {
            Some(OtaSource::Gateway) => {
                ui.label(
                    RichText::new("source: gateway fetch")
                        .small()
                        .color(Color32::from_rgb(150, 170, 195)),
                );
            }
            Some(OtaSource::Peer(pid)) => {
                ui.label(
                    RichText::new(format!("source: ⇄ peer id{pid}  (baton · ESP-NOW serve)"))
                        .strong()
                        .color(Color32::from_rgb(170, 220, 120)),
                );
            }
            None => {}
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
            row(ui, "uptime", d.u64("up").map(fmt_dur));
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
            // #190 group-HMAC transport auth (v345 train): mo = frames authenticated, mf =
            // frames rejected as forgeries / wrong group key. Absent on pre-v345 fw → row skipped.
            row(ui, "HMAC ok/fail", match (d.u64("mo"), d.u64("mf")) {
                (Some(ok), Some(fail)) => Some(format!("{ok} / {fail}")),
                _ => None,
            });
        });
        if d.u64("heap").is_some_and(|h| h <= LOW_HEAP_B) {
            ui.label(RichText::new("⚠ low heap").color(Color32::from_rgb(230, 120, 90)));
        }
        // #190: any nonzero HMAC-fail count is worth a look — a rejected forgery, or a board
        // running the wrong group key. The counter is monotonic, so it's a cumulative tally.
        if let Some(mf) = d.u64("mf").filter(|f| *f > 0) {
            ui.label(
                RichText::new(format!("⚠ {mf} HMAC failures — rejected forgeries / key mismatch"))
                    .color(Color32::from_rgb(230, 160, 90)),
            );
        }
        // #181/#249 mesh-ledger head (v345): the chain tip is a tamper canary; lgok=0 means this
        // node's own provenance chain failed self-verify. The crown additionally folds in the L2
        // anchor / L3 signed-tree-head (lgan present only on the gateway). Defensive: the whole
        // block is skipped on pre-v345 fw that doesn't publish `lgt`.
        if let Some(tip) = d.get("lgt") {
            let recs = d.u64("lgn").unwrap_or(0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("ledger").weak());
                ui.label(
                    RichText::new(format!("{tip} · {recs} rec{}", if recs == 1 { "" } else { "s" }))
                        .monospace()
                        .color(Color32::from_rgb(150, 200, 235)),
                );
                if d.u64("lgk") == Some(1) {
                    ui.label(RichText::new("· 🔑 key").small().weak());
                }
            });
            if d.u64("lgok") == Some(0) {
                ui.label(
                    RichText::new("✖ ledger self-verify FAILED — tamper canary tripped")
                        .strong()
                        .color(Color32::from_rgb(255, 110, 110)),
                );
            }
            if let Some(anchor) = d.get("lgan") {
                let signed = d.u64("lgsg") == Some(1);
                let (detail, col) = if signed {
                    (
                        format!(
                            "anchor {anchor} · epoch {} · {} leaves · ✍ signed",
                            d.u64("lgep").unwrap_or(0),
                            d.u64("lgsz").unwrap_or(0),
                        ),
                        Color32::from_rgb(150, 200, 150),
                    )
                } else {
                    (format!("anchor {anchor} · unsigned (L2)"), Color32::from_rgb(205, 190, 140))
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("↳ crown STH").weak().small());
                    ui.label(RichText::new(detail).small().color(col));
                });
            }
        }
        // #204: the crown's associated AP (which AP/channel/RSSI a deaf crown is on — the
        // forensics gap that cost hours of pcap), and crown dead-downstream health.
        if let Some(ap) = node.ap() {
            kv(ui, "AP", &format!("ch{} · {} dBm · {}", ap.channel, ap.rssi, fmt_bssid(ap.bssid)));
        }
        if let Some(cd) = node.crown_deaf() {
            let col = if cd.shed {
                Color32::from_rgb(224, 90, 90)
            } else if cd.streak > 0 {
                Color32::from_rgb(230, 190, 70)
            } else {
                Color32::from_rgb(120, 200, 150)
            };
            ui.label(
                RichText::new(format!(
                    "crown-deaf: streak {} · reassoc {} · shed {}",
                    cd.streak, cd.reassoc_cycles, if cd.shed { "yes" } else { "no" }
                ))
                .color(col),
            );
        }
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

fn fmt_bssid(b: [u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2], b[3], b[4], b[5])
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
