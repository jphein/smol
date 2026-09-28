//! Paperdoll sprites: the 48 px hero, per-slot gear overlays, and the surfaces that draw them.
//!
//! # PLACEHOLDER ART
//!
//! Decision 0014 wants hand-made pixel art and none exists. Everything drawn here is **placeholder
//! pixel art** built from rectangles in a 48×48 base grid — enough to prove the geometry, the
//! layering and the cost, and not a style proposal. The sheet and the manifest say so wherever
//! these frames appear.
//!
//! # How the art is structured, which is the part that is not placeholder
//!
//! * Every sprite is drawn in **base coordinates** (0..48) onto any `DrawTarget`. The paperdoll
//!   wraps the panel in [`Scaled`] at 2× (0032: "draws it at 2× (96 px), where the pixel art stays
//!   crisp"); the inventory draws the same overlay at 1×. One drawing, two sizes.
//! * Gear is a **per-slot overlay at the same base** (0032), drawn after the body in slot order, so
//!   an item is one 48 px layer and never a redrawn commander.
//! * Every overlay follows the breath pose via `lift`, so a worn item rises with the chest instead
//!   of floating — the one layering rule an overlay pack has to obey.
//! * An overlay's own extent is **measured, not declared** ([`Bounds`]): the inventory icon is the
//!   overlay cropped to what it actually draws and scaled up by the largest integer that fits.

use embedded_graphics::{
    Pixel,
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};
use tapstone_rules::cards::Faction;

use crate::commander::{self, Commander, Slot};
use crate::palette as pal;

/// 0014's hero sprite size, in base pixels.
pub const HERO: i32 = 48;
/// 0032: the paperdoll draws the hero at 2×.
pub const DOLL_SCALE: i32 = 2;
/// 0014's idle cycle, 1-2-3-2-1, as pose indices.
pub const BREATH: [u8; 5] = [0, 1, 2, 1, 0];

/// A target that draws each base pixel as a `k`×`k` block at `origin` on the inner target.
///
/// Positions absolutely (origin is a panel coordinate), so it keeps the crate's band contract: it
/// never asks the inner target where it is.
pub struct Scaled<'a, D> {
    pub inner: &'a mut D,
    pub origin: Point,
    pub k: i32,
}

impl<D: DrawTarget<Color = Rgb565>> Dimensions for Scaled<'_, D> {
    fn bounding_box(&self) -> Rectangle {
        // Base space, not the panel: this is what a sprite may draw into.
        Rectangle::new(Point::zero(), Size::new(HERO as u32, HERO as u32))
    }
}

impl<D: DrawTarget<Color = Rgb565>> DrawTarget for Scaled<'_, D> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let k = self.k;
        for Pixel(p, c) in pixels {
            let r = Rectangle::new(
                Point::new(self.origin.x + p.x * k, self.origin.y + p.y * k),
                Size::new(k as u32, k as u32),
            );
            let _ = self.inner.fill_solid(&r, c);
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let k = self.k;
        let r = Rectangle::new(
            Point::new(
                self.origin.x + area.top_left.x * k,
                self.origin.y + area.top_left.y * k,
            ),
            Size::new(area.size.width * k as u32, area.size.height * k as u32),
        );
        let _ = self.inner.fill_solid(&r, color);
        Ok(())
    }
}

/// A target that draws nothing and records the extent of what would have been drawn.
#[derive(Default)]
pub struct Bounds {
    pub min: Option<Point>,
    pub max: Point,
}

impl Bounds {
    pub fn rect(&self) -> Option<Rectangle> {
        self.min.map(|m| {
            Rectangle::new(
                m,
                Size::new((self.max.x - m.x + 1) as u32, (self.max.y - m.y + 1) as u32),
            )
        })
    }
}

impl Dimensions for Bounds {
    fn bounding_box(&self) -> Rectangle {
        Rectangle::new(Point::zero(), Size::new(HERO as u32, HERO as u32))
    }
}

impl DrawTarget for Bounds {
    type Color = Rgb565;
    type Error = core::convert::Infallible;
    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, _) in pixels {
            let m = self.min.get_or_insert(p);
            m.x = m.x.min(p.x);
            m.y = m.y.min(p.y);
            self.max.x = self.max.x.max(p.x);
            self.max.y = self.max.y.max(p.y);
        }
        Ok(())
    }
}

/// How the whole figure is tinted this frame. Palette, not pixels: the shape is the same sprite.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    #[default]
    Normal,
    /// Every pixel one colour: struck (warn) or healed (ok).
    Flash(Rgb565),
    /// Fading toward the fallen ghost, 1..=4 steps.
    Fade(u8),
    /// Fallen: the figure as a dim ghost.
    Ghost,
}

/// Linear mix of two Rgb565 colours, `t` of 4 toward `b`.
pub fn mix(a: Rgb565, b: Rgb565, t: u8) -> Rgb565 {
    let t = t.min(4) as u16;
    let l = |x: u8, y: u8| ((x as u16 * (4 - t) + y as u16 * t) / 4) as u8;
    Rgb565::new(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// The ghost's single hue: a figure that is present but not in play.
pub const GHOST: Rgb565 = pal::DIMMED;

fn ink(c: Rgb565, look: Look) -> Rgb565 {
    match look {
        Look::Normal => c,
        Look::Flash(f) => f,
        Look::Fade(k) => mix(c, GHOST, k),
        Look::Ghost => GHOST,
    }
}

// Placeholder art palette — sprite colours, not UI roles.
const SKIN: Rgb565 = Rgb565::new(0xe8 >> 3, 0xb8 >> 2, 0x90 >> 3);
const LEATHER: Rgb565 = Rgb565::new(0x6b >> 3, 0x4a >> 2, 0x2f >> 3);
const STEEL: Rgb565 = Rgb565::new(0xb8 >> 3, 0xc4 >> 2, 0xd4 >> 3);
const STEEL_DARK: Rgb565 = Rgb565::new(0x5a >> 3, 0x66 >> 2, 0x78 >> 3);
const CLOTH: Rgb565 = Rgb565::new(0x2a >> 3, 0x33 >> 2, 0x4a >> 3);
const EYE: Rgb565 = Rgb565::new(0x10 >> 3, 0x10 >> 2, 0x18 >> 3);

/// Pose parameters for one frame of the figure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pose {
    /// Breath pose, 0..=2 (0014's poses 1, 2, 3).
    pub breath: u8,
    pub look: Look,
    /// Stagger, in base pixels (±2 stays inside the 48 px box).
    pub dx: i32,
    /// Drop, in base pixels (falling).
    pub dy: i32,
    /// The free (left) hand raised, holding a card: while a draw is owed (0036).
    pub reach: Option<Held>,
}

/// The card in the raised hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Face down: a draw is owed and not yet tapped.
    Back,
    /// Face up in the drawn card's colour: the acknowledgement of a draw tap.
    Face(Rgb565),
}

fn r<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: i32, y: i32, w: i32, h: i32, c: Rgb565) {
    if w <= 0 || h <= 0 {
        return;
    }
    let _ = Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32))
        .into_styled(PrimitiveStyle::with_fill(c))
        .draw(d);
}

/// The chest lifts by the breath pose; legs stay planted. Base pixels.
pub const fn lift(breath: u8) -> i32 {
    match breath {
        0 => 0,
        1 => 1,
        _ => 2,
    }
}

/// The hero body, in base coordinates. **Placeholder art.**
pub fn hero<D: DrawTarget<Color = Rgb565>>(d: &mut D, faction: Faction, p: Pose) {
    let (ox, oy) = (p.dx, p.dy);
    let l = lift(p.breath);
    let accent = ink(pal::faction(faction), p.look);
    let skin = ink(SKIN, p.look);
    let leather = ink(LEATHER, p.look);
    let cloth = ink(CLOTH, p.look);
    let eye = ink(EYE, p.look);

    // Shadow stays on the ground whatever the body does, so it is not inked.
    if p.look != Look::Ghost {
        r(d, 14, 45, 20, 2, pal::PANEL);
    }
    // Cape behind the body; its hem sways with the breath.
    r(
        d,
        14 + ox,
        19 - l + oy,
        20,
        21 + l - (p.breath as i32 & 1),
        accent,
    );
    // Legs and boots: planted.
    r(d, 18 + ox, 33 + oy, 5, 10, cloth);
    r(d, 25 + ox, 33 + oy, 5, 10, cloth);
    r(d, 17 + ox, 42 + oy, 6, 3, leather);
    r(d, 25 + ox, 42 + oy, 6, 3, leather);
    // Tunic: its top rises with the breath, its hem does not.
    r(d, 16 + ox, 20 - l + oy, 16, 14 + l, cloth);
    r(d, 16 + ox, 31 + oy, 16, 2, leather); // belt
    // Arms. The left one rises to hold a card while a draw is owed (0036); the right keeps the
    // weapon, so a raised arm never hides gear.
    match p.reach {
        None => {
            r(d, 12 + ox, 21 - l + oy, 4, 12, cloth);
            r(d, 12 + ox, 33 - l + oy, 4, 3, skin);
        }
        Some(held) => {
            r(d, 11 + ox, 12 - l + oy, 4, 10, cloth);
            r(d, 12 + ox, 21 - l + oy, 4, 3, cloth); // shoulder
            r(d, 10 + ox, 10 - l + oy, 4, 3, skin);
            // The card: 7×10 base px, its top at base y 2 at the full breath lift, inside the box.
            let (fill, edge) = match held {
                Held::Back => (ink(pal::DIMMED, p.look), ink(pal::TEXT_DIM, p.look)),
                Held::Face(c) => (ink(c, p.look), ink(pal::TEXT, p.look)),
            };
            r(d, 5 + ox, 4 - l + oy, 7, 10, edge);
            r(d, 6 + ox, 5 - l + oy, 5, 8, fill);
        }
    }
    r(d, 32 + ox, 21 - l + oy, 4, 12, cloth);
    r(d, 32 + ox, 33 - l + oy, 4, 3, skin);
    // Head.
    r(d, 19 + ox, 9 - l + oy, 10, 10, skin);
    r(d, 18 + ox, 7 - l + oy, 12, 4, accent); // hood / hair band in the faction hue
    r(d, 21 + ox, 13 - l + oy, 2, 2, eye);
    r(d, 25 + ox, 13 - l + oy, 2, 2, eye);
}

/// The hooded silhouette shown when a shrine has never seen a commander (0032).
pub fn hooded<D: DrawTarget<Color = Rgb565>>(d: &mut D, p: Pose) {
    // A pointed hood over a flared cloak. The first draft was a rounded block with a square
    // void, and at 2× it read as a padlock — the same shape as the locked socket beside it.
    let l = lift(p.breath);
    let robe = ink(pal::DIMMED, p.look);
    let edge = ink(pal::TEXT_DIM, p.look);
    let void = pal::BG;
    r(d, 14, 45, 20, 2, pal::PANEL);
    r(d, 13, 38, 22, 7, robe); // hem, flared
    r(d, 15, 22 - l, 18, 17 + l, robe); // cloak
    r(d, 12, 24 - l, 4, 12, robe); // sleeves
    r(d, 32, 24 - l, 4, 12, robe);
    r(d, 23, 3 - l, 2, 2, robe); // hood tip
    r(d, 21, 5 - l, 6, 2, robe);
    r(d, 19, 7 - l, 10, 2, robe);
    r(d, 17, 9 - l, 14, 13, robe);
    r(d, 20, 12 - l, 8, 8, void); // the face you have not met yet
    r(d, 21, 15 - l, 1, 1, edge);
    r(d, 26, 15 - l, 1, 1, edge);
    r(d, 23, 22 - l, 2, 16, edge); // cloak seam
}

/// Item overlay tints: steel with the item's faction on the trim.
fn trim(it: &commander::Item, look: Look) -> Rgb565 {
    ink(pal::faction(it.faction), look)
}

/// One gear overlay at the hero's base, following the breath lift. **Placeholder art.**
pub fn overlay<D: DrawTarget<Color = Rgb565>>(d: &mut D, it: &commander::Item, p: Pose) {
    let (ox, oy) = (p.dx, p.dy);
    let l = lift(p.breath);
    let steel = ink(STEEL, p.look);
    let dark = ink(STEEL_DARK, p.look);
    let t = trim(it, p.look);
    match it.slot {
        // One silhouette per weapon: under 0034 most gear is a look, so two weapons that draw
        // the same are two items the player cannot tell apart. Placeholder shapes, keyed by the
        // item's name only because the placeholder table has no art field yet.
        Slot::Weapon if it.name.ends_with("Trident") => {
            // A long shaft and three prongs. Topmost pixel at base y 2, so the full breath lift
            // (2) keeps it inside the 48 px box.
            r(d, 38 + ox, 8 - l + oy, 1, 30, dark);
            r(d, 35 + ox, 7 - l + oy, 7, 1, t);
            r(d, 35 + ox, 3 - l + oy, 1, 4, steel);
            r(d, 38 + ox, 2 - l + oy, 1, 5, steel);
            r(d, 41 + ox, 3 - l + oy, 1, 4, steel);
        }
        Slot::Weapon => {
            // Held in the right hand: blade up, guard at the fist.
            r(d, 37 + ox, 12 - l + oy, 3, 20, steel);
            r(d, 38 + ox, 10 - l + oy, 1, 2, steel);
            r(d, 35 + ox, 31 - l + oy, 7, 2, t);
            r(d, 37 + ox, 33 - l + oy, 3, 4, dark);
        }
        Slot::Armour if it.name.ends_with("cloak") => {
            // A hooded cloak over the shoulders, open at the front: a look, not a breastplate.
            r(d, 13 + ox, 19 - l + oy, 7, 22 + l, t);
            r(d, 28 + ox, 19 - l + oy, 7, 22 + l, t);
            r(d, 18 + ox, 18 - l + oy, 12, 3, t);
            r(d, 22 + ox, 20 - l + oy, 4, 2, steel); // clasp
        }
        Slot::Armour => {
            // Breastplate and pauldrons over the tunic.
            r(d, 17 + ox, 21 - l + oy, 14, 9, steel);
            r(d, 17 + ox, 29 - l + oy, 14, 1, dark);
            r(d, 11 + ox, 19 - l + oy, 7, 4, t);
            r(d, 30 + ox, 19 - l + oy, 7, 4, t);
            r(d, 23 + ox, 23 - l + oy, 2, 5, t);
        }
        Slot::Trinket => {
            // A pendant at the throat.
            r(d, 21 + ox, 19 - l + oy, 6, 1, dark);
            r(d, 22 + ox, 20 - l + oy, 4, 4, t);
            r(d, 23 + ox, 21 - l + oy, 2, 2, steel);
        }
    }
}

/// The whole paperdoll in base coordinates: body, then each worn overlay in slot order.
pub fn doll<D: DrawTarget<Color = Rgb565>>(d: &mut D, c: Option<&Commander<'_>>, p: Pose) {
    match c {
        None => hooded(d, p),
        Some(c) => {
            hero(d, c.faction, p);
            for s in commander::SLOTS {
                if let Some(it) = c.worn(s) {
                    overlay(d, it, p);
                }
            }
        }
    }
}

/// The measured extent of an item's overlay at rest, in base pixels.
pub fn overlay_bounds(it: &commander::Item) -> Rectangle {
    let mut b = Bounds::default();
    overlay(&mut b, it, Pose::default());
    b.rect()
        .unwrap_or(Rectangle::new(Point::zero(), Size::new(1, 1)))
}

pub const ICON_MAX_SCALE: i32 = 3;

/// Draw an item as an inventory icon: its overlay cropped to what it draws, scaled by the largest
/// integer that fits `box_px`, centred at `centre`. The icon *is* the overlay — no second asset.
pub fn icon<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    it: &commander::Item,
    centre: Point,
    box_px: i32,
    look: Look,
) {
    let b = overlay_bounds(it);
    let (w, h) = (b.size.width as i32, b.size.height as i32);
    // Capped at 3×: a trinket's overlay is a few pixels at the throat, and scaled to fill the
    // cell it read as a crate. Three times is still clearly the same object.
    let k = (box_px / w.max(h)).clamp(1, ICON_MAX_SCALE);
    let origin = Point::new(
        centre.x - (w * k) / 2 - b.top_left.x * k,
        centre.y - (h * k) / 2 - b.top_left.y * k,
    );
    let mut s = Scaled {
        inner: d,
        origin,
        k,
    };
    overlay(
        &mut s,
        it,
        Pose {
            look,
            ..Pose::default()
        },
    );
}

/// The loot chest, `stage` 0 closed .. 3 open with the glow. Drawn inside a 40 px box. **Placeholder.**
pub fn chest<D: DrawTarget<Color = Rgb565>>(d: &mut D, at: Point, stage: u8) {
    let (x, y) = (at.x, at.y);
    let wood = LEATHER;
    let band = pal::WARM;
    // Glow behind an open lid.
    if stage >= 2 {
        r(d, x + 8, y + 2, 24, 14, mix(pal::WARM, pal::PANEL, 2));
    }
    // Body.
    r(d, x + 4, y + 20, 32, 16, wood);
    r(d, x + 4, y + 26, 32, 2, band);
    r(d, x + 18, y + 24, 4, 6, band);
    // Lid: closed, ajar, open.
    match stage {
        0 => r(d, x + 4, y + 12, 32, 8, wood),
        1 => r(d, x + 4, y + 9, 32, 7, wood),
        _ => r(d, x + 4, y + 2, 32, 5, wood),
    }
    if stage == 0 {
        r(d, x + 4, y + 18, 32, 2, band);
    }
}
