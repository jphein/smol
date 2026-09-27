//! The nine screens that are not the battlefield.
//!
//! Five of them are the battlefield wearing a different hat — targeting, combat resolution,
//! sudden death, the spectator variant and the disconnect overlay are all states of the same
//! screen, exactly as `docs/design/shrine-ux.md` specifies ("No screen change — this is the
//! battlefield with a locked prompt line"). Those are `Opts` on `battlefield::render`, not
//! separate drawing code, so a layout change cannot move them out of step with the board.
//!
//! Four are standalone: idle, pairing, setup/mulligan and result. Where a screen has state to
//! show, it comes from a real `Game` rather than invented numbers — the result screen reads the
//! winner, the surviving castle life and the round from a played-out match.
//!
//! Chrome constants are shared with the battlefield so the 24 px HUD bands, the type tiers and
//! the 44 px touch floor mean the same thing on every screen.

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle},
    text::Alignment,
};
use tapstone_rules::state::{Game, Winner};

use crate::battlefield::{fill, line_h, name_of, text, tier};
use crate::geom;
use crate::palette as pal;

/// Clear the target to the ground colour. Screens draw onto a surface the caller owns.
fn blank<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
    fill(
        d,
        Rectangle::new(Point::zero(), Size::new(geom::W as u32, geom::H as u32)),
        pal::BG,
    );
}

/// A touch target drawn at or above the doc's 44 px minimum, with its label centred.
///
/// Buttons are the one place the 44 px rule is not negotiable even though touch is the fallback
/// input: a button nobody can hit is not a fallback, it is a dead end.
fn button<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: Rectangle, label: &str, accent: Rgb565) {
    debug_assert!(
        r.size.height >= geom::TOUCH_MIN as u32,
        "a button below the 44 px touch floor is unusable"
    );
    fill(d, r, pal::PANEL);
    let _ = r
        .into_styled(PrimitiveStyle::with_stroke(accent, 1))
        .draw(d);
    text(
        d,
        label,
        Point::new(
            r.top_left.x + r.size.width as i32 / 2,
            r.top_left.y + (r.size.height as i32 - line_h(tier::BODY)) / 2,
        ),
        tier::BODY,
        pal::TEXT,
        Alignment::Center,
    );
}

/// The status corners the idle screen carries: mesh count left, battery and clock right.
fn status_line<D: DrawTarget<Color = Rgb565>>(d: &mut D, left: &str, right: &str) {
    let y = geom::H - line_h(tier::STATUS) - 3;
    text(
        d,
        left,
        Point::new(4, y),
        tier::STATUS,
        pal::TEXT_DIM,
        Alignment::Left,
    );
    text(
        d,
        right,
        Point::new(geom::W - 4, y),
        tier::STATUS,
        pal::TEXT_DIM,
        Alignment::Right,
    );
}

/// 1. Idle / castle face — the resting orb, drawn locally rather than streamed.
///
/// The doc's idle face comes from scry over WiFi (`GET /screen-idle`); feasibility red flag 2
/// says a no-server table needs it cached or drawn locally. This draws it locally from the same
/// recipe: starfield, orb, crest of the last deck seen, two lines of type.
pub fn idle<D: DrawTarget<Color = Rgb565>>(d: &mut D, faction_name: &str, accent: Rgb565) {
    blank(d);

    // Starfield: deterministic, so the screen is reproducible rather than randomly sprinkled.
    let mut seed: u32 = 0x9e37_79b9;
    for _ in 0..40 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let x = (seed >> 16) as i32 % geom::W;
        let y = (seed >> 8) as i32 % (geom::H - 60);
        fill(
            d,
            Rectangle::new(Point::new(x, y), Size::new(1, 1)),
            pal::DIMMED,
        );
    }

    let cx = geom::W / 2;
    let cy = 84;
    for (r, c) in [(40, pal::PANEL), (30, pal::BG)] {
        let _ = Circle::with_center(Point::new(cx, cy), r as u32)
            .into_styled(PrimitiveStyle::with_fill(c))
            .draw(d);
    }
    let _ = Circle::with_center(Point::new(cx, cy), 40)
        .into_styled(PrimitiveStyle::with_stroke(accent, 2))
        .draw(d);
    text(
        d,
        faction_name,
        Point::new(cx, cy - line_h(tier::STATUS) / 2),
        tier::STATUS,
        accent,
        Alignment::Center,
    );

    text(
        d,
        "T A P S T O N E",
        Point::new(cx, 148),
        tier::VALUE,
        pal::TEXT,
        Alignment::Center,
    );
    text(
        d,
        "tap a card to summon - tap a deck to duel",
        Point::new(cx, 176),
        tier::STATUS,
        pal::TEXT_DIM,
        Alignment::Center,
    );
    status_line(d, "mesh 3", "73%   22:41");
}

/// 2. Pairing — finding the other shrine.
///
/// RSSI fills the dots left to right; only a shrine also pairing and *near* is offered. The
/// ruleset line is decision 0004's image hash agreement, which is a match gate, not a warning.
pub fn pairing<D: DrawTarget<Color = Rgb565>>(d: &mut D, rival: &str, rssi: u8, ruleset_ok: bool) {
    blank(d);
    text(
        d,
        "SEEKING A RIVAL",
        Point::new(geom::W / 2, 14),
        tier::TURN,
        pal::TEXT,
        Alignment::Center,
    );

    let y = 76;
    for (cx, label, solid) in [(58, "me", true), (262, "?", false)] {
        let c = Circle::with_center(Point::new(cx, y), 44);
        let _ = c
            .into_styled(PrimitiveStyle::with_stroke(
                if solid { pal::TIDE } else { pal::TEXT_DIM },
                if solid { 2 } else { 1 },
            ))
            .draw(d);
        text(
            d,
            label,
            Point::new(cx, y - line_h(tier::STATUS) / 2),
            tier::STATUS,
            if solid { pal::TEXT } else { pal::TEXT_DIM },
            Alignment::Center,
        );
    }
    // RSSI dots between the two orbs.
    let lit = (rssi.min(10)) as i32;
    for i in 0..10 {
        let x = 92 + i * 14;
        fill(
            d,
            Rectangle::new(Point::new(x, y - 2), Size::new(4, 4)),
            if i < lit { pal::TIDE } else { pal::DIMMED },
        );
    }

    text(
        d,
        rival,
        Point::new(geom::W / 2, 132),
        tier::BODY,
        pal::TEXT,
        Alignment::Center,
    );
    text(
        d,
        "they must tap their deck to accept",
        Point::new(geom::W / 2, 150),
        tier::STATUS,
        pal::TEXT_DIM,
        Alignment::Center,
    );

    let (msg, colour) = if ruleset_ok {
        ("ruleset a91c ok", pal::HEALTH_OK)
    } else {
        ("ruleset differs - no match", pal::WARN)
    };
    text(
        d,
        msg,
        Point::new(4, 176),
        tier::STATUS,
        colour,
        Alignment::Left,
    );
    button(
        d,
        Rectangle::new(Point::new(196, 192), Size::new(120, 44)),
        "cancel",
        pal::TEXT_DIM,
    );
}

/// 3. Setup / mulligan — the coin, the rules, and one prompt.
///
/// The shrine cannot see a hand, so the mulligan is physical; the screen only reports what was
/// decided without negotiation (the coin is a hash of both node ids and the synced time).
pub fn setup<D: DrawTarget<Color = Rgb565>>(d: &mut D, game: &Game, near: u8, they_ready: bool) {
    blank(d);
    let far = 1 - near;
    text(
        d,
        name_of(game.seats[near as usize].castle_design),
        Point::new(4, 6),
        tier::STATUS,
        pal::TEXT,
        Alignment::Left,
    );
    text(
        d,
        name_of(game.seats[far as usize].castle_design),
        Point::new(geom::W - 4, 6),
        tier::STATUS,
        pal::TEXT,
        Alignment::Right,
    );
    text(
        d,
        "vs",
        Point::new(geom::W / 2, 6),
        tier::STATUS,
        pal::TEXT_DIM,
        Alignment::Center,
    );

    let first = game.active == near;
    text(
        d,
        if first {
            "you go FIRST"
        } else {
            "they go FIRST"
        },
        Point::new(geom::W / 2, 66),
        tier::VALUE,
        if first { pal::HEALTH_OK } else { pal::TEXT },
        Alignment::Center,
    );
    let r = &game.rules;
    text(
        d,
        &crate::txt!(
            "draw {} - mulligan once - life {} - sudden death T{}",
            r.hand,
            r.castle_life,
            r.pressure_from
        ),
        Point::new(geom::W / 2, 98),
        tier::STATUS,
        pal::TEXT_DIM,
        Alignment::Center,
    );

    button(
        d,
        Rectangle::new(Point::new(40, 140), Size::new(190, 48)),
        "tap castle card = READY",
        pal::TIDE,
    );
    text(
        d,
        if they_ready {
            "they: ready"
        } else {
            "they: ..."
        },
        Point::new(240, 156),
        tier::STATUS,
        if they_ready {
            pal::HEALTH_OK
        } else {
            pal::TEXT_DIM
        },
        Alignment::Left,
    );
    // The deck hashes are placeholders; the count is the table's own deck_size (30 since #147).
    let n = game.rules.deck_size;
    status_line(
        d,
        &crate::txt!("deck 7f3a - {n} cards"),
        &crate::txt!("deck b210 - {n}"),
    );
}

/// 8. Result — who won, and the match's own title.
///
/// Driven from a real finished `Game`: the winner, the surviving castle life and the round are
/// read from the engine, not typed in.
pub fn result<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    game: &Game,
    near: u8,
    rounds: u8,
    taps: usize,
    sigil: &str,
) {
    blank(d);
    let won = matches!(game.winner, Some(Winner::Seat(s)) if s == near);
    let drawn = matches!(game.winner, Some(Winner::Draw)) || game.winner.is_none();
    let (word, colour) = if drawn {
        ("D R A W", pal::TEXT)
    } else if won {
        ("V I C T O R Y", pal::HEALTH_OK)
    } else {
        ("D E F E A T", pal::WARN)
    };
    text(
        d,
        word,
        Point::new(geom::W / 2, 44),
        tier::VALUE,
        colour,
        Alignment::Center,
    );

    let loser = if won { 1 - near } else { near };
    text(
        d,
        &crate::txt!(
            "{} falls on turn {}",
            name_of(game.seats[loser as usize].castle_design),
            rounds
        ),
        Point::new(geom::W / 2, 78),
        tier::BODY,
        pal::TEXT_DIM,
        Alignment::Center,
    );
    text(
        d,
        &crate::txt!(
            "{} life left - {} taps - replay sigil {}",
            game.seats[near as usize].castle.life,
            taps,
            sigil
        ),
        Point::new(geom::W / 2, 108),
        tier::STATUS,
        pal::TEXT_DIM,
        Alignment::Center,
    );

    button(
        d,
        Rectangle::new(Point::new(8, 160), Size::new(148, 48)),
        "rematch: tap deck",
        pal::TIDE,
    );
    button(
        d,
        Rectangle::new(Point::new(164, 160), Size::new(148, 48)),
        "done: tap castle",
        pal::TEXT_DIM,
    );
}

/// A thin hazard rule, used by the sudden-death band.
pub(crate) fn hazard<D: DrawTarget<Color = Rgb565>>(d: &mut D, y: i32) {
    let mut x = 0;
    while x < geom::W {
        let _ = Line::new(Point::new(x, y), Point::new(x + 4, y))
            .into_styled(PrimitiveStyle::with_stroke(pal::BG, 1))
            .draw(d);
        x += 8;
    }
}
