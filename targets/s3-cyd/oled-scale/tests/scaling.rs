//! Host tests for [`ScaledOled`] — the point of the `*-core` crate pattern.
//!
//! Everything here runs on plain stable x86_64. No chip, no HAL, no espup toolchain: that IS
//! the proof that the scaling layer is chip-agnostic, and it is why this crate is separate from
//! the S3 firmware crate rather than a module inside it.

use embedded_graphics::{
    pixelcolor::{BinaryColor, Rgb565},
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};
use oled_scale::{ScaledOled, ScaledOled4x, CAP_4X, LOGICAL_H, LOGICAL_W};

const FG: Rgb565 = Rgb565::new(31, 63, 31); // white
const BG: Rgb565 = Rgb565::new(0, 0, 0); // black

fn fresh() -> ScaledOled4x {
    let mut d = ScaledOled::new(4, FG, BG);
    // `new` starts fully dirty on purpose (first flush must paint the whole panel);
    // consume that so each test observes only its own drawing.
    d.take_dirty();
    d
}

/// Raw value of one physical pixel.
fn phys(d: &ScaledOled4x, x: u32, y: u32) -> u16 {
    d.raw_row(y)[x as usize]
}

fn fg_raw() -> u16 {
    ((31u16) << 11) | ((63u16) << 5) | 31u16
}
fn bg_raw() -> u16 {
    0
}

// ── geometry ────────────────────────────────────────────────────────────────────────────

#[test]
fn origin_dimensions_reports_logical_72x40() {
    let d = fresh();
    // The seam's whole promise: smol's screens see the panel they were written for.
    assert_eq!(d.size(), Size::new(72, 40));
    assert_eq!(d.bounding_box(), Rectangle::new(Point::zero(), Size::new(72, 40)));
    assert_eq!(LOGICAL_W, 72);
    assert_eq!(LOGICAL_H, 40);
}

#[test]
fn physical_size_is_288x160_at_4x() {
    let d = fresh();
    assert_eq!(d.scale(), 4);
    assert_eq!(d.physical_size(), Size::new(288, 160));
    // 288*160 = 46,080 px = 92,160 bytes of RGB565 — the number the S3 backend must place.
    assert_eq!(CAP_4X, 46_080);
    assert_eq!(d.raw_pixels().len(), 46_080);
}

#[test]
fn scale_is_clamped_not_panicked_when_capacity_is_short() {
    // CAP sized for 2x (144*80 = 11,520); asking for 4x must clamp, never panic.
    let d: ScaledOled<11_520> = ScaledOled::new(4, FG, BG);
    assert_eq!(d.scale(), 2, "over-large scale must clamp down to what CAP holds");
    assert_eq!(d.physical_size(), Size::new(144, 80));

    // Zero is nonsense; it must become 1 rather than divide-by-zero somewhere later.
    let z: ScaledOled4x = ScaledOled::new(0, FG, BG);
    assert_eq!(z.scale(), 1);
}

// ── scaling correctness ─────────────────────────────────────────────────────────────────

#[test]
fn one_logical_pixel_becomes_exactly_one_4x4_block_with_no_bleed() {
    let mut d = fresh();
    Pixel(Point::new(3, 2), BinaryColor::On).draw(&mut d).unwrap();

    // The 4x4 block at physical (12..16, 8..12) is fg, all of it.
    for y in 8..12 {
        for x in 12..16 {
            assert_eq!(phys(&d, x, y), fg_raw(), "inside block at ({x},{y})");
        }
    }
    // And every neighbouring pixel is untouched — one ring around the block.
    for y in 7..13 {
        for x in 11..17 {
            let inside = (12..16).contains(&x) && (8..12).contains(&y);
            if !inside {
                assert_eq!(phys(&d, x, y), bg_raw(), "BLED into ({x},{y})");
            }
        }
    }
}

#[test]
fn corner_pixels_land_flush_against_the_image_edges() {
    let mut d = fresh();
    Pixel(Point::new(0, 0), BinaryColor::On).draw(&mut d).unwrap();
    Pixel(Point::new(71, 39), BinaryColor::On).draw(&mut d).unwrap();

    // Top-left block occupies exactly (0..4, 0..4).
    assert_eq!(phys(&d, 0, 0), fg_raw());
    assert_eq!(phys(&d, 3, 3), fg_raw());
    assert_eq!(phys(&d, 4, 0), bg_raw());
    assert_eq!(phys(&d, 0, 4), bg_raw());

    // Bottom-right block ends exactly at (287,159) — the last pixel of the buffer.
    assert_eq!(phys(&d, 287, 159), fg_raw());
    assert_eq!(phys(&d, 284, 156), fg_raw());
    assert_eq!(phys(&d, 283, 159), bg_raw());
    assert_eq!(phys(&d, 287, 155), bg_raw());
}

#[test]
fn out_of_bounds_logical_pixels_are_dropped_not_wrapped() {
    let mut d = fresh();
    // Mirrors CanvasOled::draw_iter's bounds check (rust/clock/src/lib.rs:167-175).
    // A wrap here would corrupt the opposite edge, which is far worse than a dropped pixel.
    for p in [
        Point::new(-1, 0),
        Point::new(0, -1),
        Point::new(72, 0),
        Point::new(0, 40),
        Point::new(1000, 1000),
    ] {
        Pixel(p, BinaryColor::On).draw(&mut d).unwrap();
    }
    assert!(d.raw_pixels().iter().all(|&p| p == bg_raw()), "an OOB pixel reached the buffer");
    assert_eq!(d.take_dirty(), None, "an OOB pixel must not dirty anything");
}

#[test]
fn logical_pixel_readback_round_trips() {
    let mut d = fresh();
    Pixel(Point::new(10, 5), BinaryColor::On).draw(&mut d).unwrap();
    assert_eq!(d.logical_pixel(10, 5), Some(true));
    assert_eq!(d.logical_pixel(11, 5), Some(false));
    assert_eq!(d.logical_pixel(72, 0), None, "out of bounds is None, not false");
}

// ── fg / bg mapping ─────────────────────────────────────────────────────────────────────

#[test]
fn on_maps_to_fg_and_off_maps_to_bg() {
    let red = Rgb565::new(31, 0, 0);
    let blue = Rgb565::new(0, 0, 31);
    let mut d: ScaledOled4x = ScaledOled::new(4, red, blue);
    d.take_dirty();

    let red_raw = 31u16 << 11;
    let blue_raw = 31u16;

    // A fresh buffer is bg everywhere.
    assert_eq!(phys(&d, 100, 100), blue_raw);

    Pixel(Point::new(0, 0), BinaryColor::On).draw(&mut d).unwrap();
    assert_eq!(phys(&d, 0, 0), red_raw, "On must map to fg");

    Pixel(Point::new(0, 0), BinaryColor::Off).draw(&mut d).unwrap();
    assert_eq!(phys(&d, 0, 0), blue_raw, "Off must map to bg");
}

#[test]
fn set_colors_recolours_in_place_and_marks_everything_dirty() {
    let mut d = fresh();
    Pixel(Point::new(1, 1), BinaryColor::On).draw(&mut d).unwrap();
    d.take_dirty();

    let green = Rgb565::new(0, 63, 0);
    let grey = Rgb565::new(8, 16, 8);
    d.set_colors(green, grey);

    // Logical content survives the recolour; only the palette moved.
    assert_eq!(d.logical_pixel(1, 1), Some(true));
    assert_eq!(d.logical_pixel(2, 1), Some(false));
    assert_eq!(phys(&d, 4, 4), 63u16 << 5, "lit pixel took the new fg");
    assert_eq!(phys(&d, 0, 0), (8u16 << 11) | (16u16 << 5) | 8u16, "unlit took the new bg");
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::zero(), Size::new(288, 160))),
        "a palette change must repaint the whole image"
    );
}

// ── dirty-rect minimality ───────────────────────────────────────────────────────────────

#[test]
fn drawing_nothing_leaves_an_empty_window() {
    let mut d = fresh();
    assert_eq!(d.take_dirty(), None, "no draw => no blit; the backend must skip entirely");
    assert_eq!(d.peek_dirty(), None);
}

#[test]
fn a_single_pixel_dirties_exactly_its_own_4x4_block() {
    let mut d = fresh();
    Pixel(Point::new(5, 7), BinaryColor::On).draw(&mut d).unwrap();
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::new(20, 28), Size::new(4, 4))),
        "dirty rect must be the minimal N*N block, not the whole image"
    );
    // …and taking it resets the tracker.
    assert_eq!(d.take_dirty(), None, "take must clear the tracker");
}

#[test]
fn two_pixels_dirty_their_bounding_box() {
    let mut d = fresh();
    Pixel(Point::new(1, 1), BinaryColor::On).draw(&mut d).unwrap();
    Pixel(Point::new(3, 4), BinaryColor::On).draw(&mut d).unwrap();
    // logical (1,1)..(3,4) -> physical (4,4)..(15,19) inclusive -> 12x16
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::new(4, 4), Size::new(12, 16)))
    );
}

#[test]
fn clear_dirties_the_full_image() {
    let mut d = fresh();
    d.clear(BinaryColor::Off).unwrap();
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::zero(), Size::new(288, 160))),
        "clear() routes through fill_solid over the whole bounding box"
    );
}

#[test]
fn peek_does_not_consume() {
    let mut d = fresh();
    Pixel(Point::new(0, 0), BinaryColor::On).draw(&mut d).unwrap();
    let a = d.peek_dirty();
    let b = d.peek_dirty();
    assert_eq!(a, b, "peek must be idempotent");
    assert_eq!(d.take_dirty(), a, "take must agree with peek");
}

#[test]
fn a_new_display_starts_fully_dirty() {
    // Otherwise the first flush pushes nothing and the panel shows power-on GRAM garbage.
    let mut d: ScaledOled4x = ScaledOled::new(4, FG, BG);
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::zero(), Size::new(288, 160)))
    );
}

// ── fill fast paths ─────────────────────────────────────────────────────────────────────

#[test]
fn fill_solid_paints_the_scaled_block_and_dirties_it_once() {
    let mut d = fresh();
    Rectangle::new(Point::new(2, 3), Size::new(4, 2))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(&mut d)
        .unwrap();

    // logical (2,3)..(5,4) -> physical (8,12)..(23,19)
    for y in 12..20 {
        for x in 8..24 {
            assert_eq!(phys(&d, x, y), fg_raw(), "fill missed ({x},{y})");
        }
    }
    assert_eq!(phys(&d, 7, 12), bg_raw());
    assert_eq!(phys(&d, 24, 19), bg_raw());
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::new(8, 12), Size::new(16, 8)))
    );
}

#[test]
fn fill_solid_clips_to_the_panel() {
    let mut d = fresh();
    // Straddles the right/bottom edges; must clip, not overflow the buffer.
    d.fill_solid(
        &Rectangle::new(Point::new(70, 38), Size::new(10, 10)),
        BinaryColor::On,
    )
    .unwrap();
    assert_eq!(phys(&d, 287, 159), fg_raw());
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::new(280, 152), Size::new(8, 8))),
        "clipped fill dirties only the on-panel part"
    );
}

#[test]
fn fill_solid_entirely_off_panel_is_a_no_op() {
    let mut d = fresh();
    d.fill_solid(
        &Rectangle::new(Point::new(200, 200), Size::new(5, 5)),
        BinaryColor::On,
    )
    .unwrap();
    assert_eq!(d.take_dirty(), None);
    assert!(d.raw_pixels().iter().all(|&p| p == bg_raw()));
}

#[test]
fn fill_contiguous_consumes_one_colour_per_area_pixel_in_row_major_order() {
    let mut d = fresh();
    // 3x2 checkerboard starting On.
    let colors = [
        BinaryColor::On,
        BinaryColor::Off,
        BinaryColor::On,
        BinaryColor::Off,
        BinaryColor::On,
        BinaryColor::Off,
    ];
    d.fill_contiguous(
        &Rectangle::new(Point::new(0, 0), Size::new(3, 2)),
        colors,
    )
    .unwrap();

    assert_eq!(d.logical_pixel(0, 0), Some(true));
    assert_eq!(d.logical_pixel(1, 0), Some(false));
    assert_eq!(d.logical_pixel(2, 0), Some(true));
    assert_eq!(d.logical_pixel(0, 1), Some(false));
    assert_eq!(d.logical_pixel(1, 1), Some(true));
    assert_eq!(d.logical_pixel(2, 1), Some(false));
}

#[test]
fn fill_contiguous_stays_in_phase_when_the_area_hangs_off_the_edge() {
    // The shearing trap: clipped pixels must still CONSUME their colour, or every
    // subsequent row slides sideways.
    let mut d = fresh();
    // Area x = 70..74, so columns 72 and 73 are off-panel and must be consumed-and-dropped.
    let colors = [
        BinaryColor::On,  // (70,0) on
        BinaryColor::Off, // (71,0) on
        BinaryColor::On,  // (72,0) OFF-PANEL
        BinaryColor::On,  // (73,0) OFF-PANEL
        BinaryColor::Off, // (70,1) on
        BinaryColor::On,  // (71,1) on
    ];
    d.fill_contiguous(
        &Rectangle::new(Point::new(70, 0), Size::new(4, 2)),
        colors,
    )
    .unwrap();

    assert_eq!(d.logical_pixel(70, 0), Some(true));
    assert_eq!(d.logical_pixel(71, 0), Some(false));
    assert_eq!(d.logical_pixel(70, 1), Some(false), "row 1 sheared — phase was lost");
    assert_eq!(d.logical_pixel(71, 1), Some(true), "row 1 sheared — phase was lost");
}

#[test]
fn fill_contiguous_tolerates_a_short_colour_iterator() {
    let mut d = fresh();
    d.fill_contiguous(
        &Rectangle::new(Point::new(0, 0), Size::new(10, 10)),
        [BinaryColor::On, BinaryColor::On],
    )
    .unwrap();
    assert_eq!(d.logical_pixel(0, 0), Some(true));
    assert_eq!(d.logical_pixel(1, 0), Some(true));
    assert_eq!(d.logical_pixel(2, 0), Some(false), "ran dry, rest untouched");
}

// ── app::Oled contract shape ────────────────────────────────────────────────────────────

#[test]
fn inherent_init_and_flush_exist_and_are_infallible() {
    // The app::Oled contract (rust/clock/src/app.rs:39-43): plugins call init/clear/flush
    // as inherent methods. CanvasOled (lib.rs:137-145) returns Infallible from both.
    let mut d = fresh();
    let r: Result<(), core::convert::Infallible> = d.init();
    assert!(r.is_ok());
    let r: Result<(), core::convert::Infallible> = d.flush();
    assert!(r.is_ok());
}

#[test]
fn a_smol_style_draw_sequence_round_trips() {
    // What menu.rs:226-228 actually does: clear, draw, flush.
    use embedded_graphics::{
        mono_font::{ascii::FONT_5X8, MonoTextStyleBuilder},
        text::Text,
    };
    let mut d = fresh();
    d.clear(BinaryColor::Off).unwrap();
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_5X8)
        .text_color(BinaryColor::On)
        .build();
    Text::new("smol", Point::new(2, 10), style).draw(&mut d).unwrap();
    d.flush().unwrap();

    // Text drew *something* lit, and the dirty rect covers the whole image because clear() ran.
    assert!(
        d.raw_pixels().iter().any(|&p| p == fg_raw()),
        "no lit pixels — the text never rendered"
    );
    assert_eq!(
        d.take_dirty(),
        Some(Rectangle::new(Point::zero(), Size::new(288, 160)))
    );
}
