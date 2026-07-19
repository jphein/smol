//! A restrained HUD over the constellation: title, a live status bar (broker · crown ·
//! channel · node/stale counts · build uniformity), the latest event, and a legend.

use std::collections::BTreeSet;

use bevy::prelude::*;

use mesh_model::model::ConnState;

use crate::mesh::MeshHandle;

#[derive(Component)]
pub struct HudStatus;

#[derive(Component)]
pub struct HudTicker;

/// Reduce a model event string to plain ASCII (the events carry emoji + arrows that
/// the embedded font can't render). Keeps the arrow's meaning as `->`.
fn ascii_ticker(s: &str) -> String {
    s.replace('\u{2192}', "->") // → (version flip)
        .chars()
        .filter(|c| c.is_ascii())
        .collect::<String>()
        .trim()
        .to_string()
}

fn style(top: f32, left: f32) -> Style {
    Style { position_type: PositionType::Absolute, top: Val::Px(top), left: Val::Px(left), ..default() }
}

pub fn setup_hud(mut commands: Commands) {
    let dim = Color::srgb(0.55, 0.60, 0.72);
    let bright = Color::srgb(0.86, 0.90, 1.0);

    commands.spawn(
        TextBundle::from_section(
            "observatory",
            TextStyle { font_size: 26.0, color: Color::srgb(0.92, 0.82, 0.55), ..default() },
        )
        .with_style(style(14.0, 18.0)),
    );
    commands.spawn(
        TextBundle::from_section(
            "the smol mesh, as a living constellation",
            TextStyle { font_size: 13.0, color: dim, ..default() },
        )
        .with_style(style(44.0, 19.0)),
    );

    commands.spawn((
        HudStatus,
        TextBundle::from_section("connecting…", TextStyle { font_size: 16.0, color: bright, ..default() })
            .with_style(style(70.0, 19.0)),
    ));

    commands.spawn((
        HudTicker,
        TextBundle::from_section("", TextStyle { font_size: 14.0, color: Color::srgb(0.70, 0.78, 0.92), ..default() })
            .with_style(style(96.0, 19.0)),
    ));

    // ASCII-only markers: Bevy's embedded default font has no emoji/symbol glyphs
    // (they render as tofu), so the HUD stays plain-text and legible on a wall display.
    commands.spawn(
        TextBundle::from_section(
            "(+) crown    o gateway    . leaf    faded = stale\nfilament brightness = RSSI     cyan motes = OTA     gold comet = election",
            TextStyle { font_size: 12.0, color: dim, ..default() },
        )
        .with_style(Style { position_type: PositionType::Absolute, bottom: Val::Px(14.0), left: Val::Px(19.0), ..default() }),
    );
}

#[allow(clippy::type_complexity)]
pub fn update_hud(
    mesh: Res<MeshHandle>,
    mut status_q: Query<&mut Text, (With<HudStatus>, Without<HudTicker>)>,
    mut ticker_q: Query<&mut Text, (With<HudTicker>, Without<HudStatus>)>,
) {
    let now = mesh.now();
    let (status, ticker) = {
        let Ok(m) = mesh.model.lock() else { return };

        let conn = match m.conn {
            ConnState::Connecting => "connecting...",
            ConnState::Connected => "LIVE",
            ConnState::Error => "OFFLINE",
        };

        let crown = match &m.crown {
            Some(c) => format!("crown {} ch{} seq{}", mesh_model::names::noun_for_id(c.owner), c.channel, c.seq),
            None => "no crown".to_string(),
        };

        let node_count = m.nodes.len();
        let stale = m.nodes.values().filter(|n| n.is_stale(now)).count();

        let builds: BTreeSet<&str> = m.nodes.values().filter_map(|n| n.build()).collect();
        let uniformity = match builds.len() {
            0 => "build ?".to_string(),
            1 => format!("all on v{}", builds.iter().next().unwrap()),
            k => format!("mixed ({k} builds)"),
        };

        let status = format!(
            "{conn}   {}    |    {crown}    |    {node_count} nodes ({stale} stale)    |    {uniformity}",
            m.broker
        );
        // The model's event strings carry emoji (lifted verbatim from meshscope);
        // strip them to ASCII so the ticker reads cleanly in the emoji-less font.
        let ticker = m.events.back().map(|e| ascii_ticker(&e.text)).unwrap_or_default();
        (status, ticker)
    };

    if let Ok(mut t) = status_q.get_single_mut() {
        t.sections[0].value = status;
    }
    if let Ok(mut t) = ticker_q.get_single_mut() {
        t.sections[0].value = ticker;
    }
}
