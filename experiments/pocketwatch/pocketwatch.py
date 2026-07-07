#!/usr/bin/env python3
"""
Parametric 3D-printable POCKET WATCH case for the ESP32-C3 SuperMini + 0.42" OLED
with a 502030 LiPo (5 x 20 x 30 mm) behind the board.

Two printable parts are exported to this directory:
    pocketwatch_body.stl   - round case body with front face, OLED window, USB slot, bail
    pocketwatch_lid.stl    - press-fit back lid
    pocketwatch_assembly.stl - (preview only) body + lid nested together

Geometry is built procedurally from primitives (cylinders, boxes, a torus) combined
with boolean unions/differences using the `manifold` engine, which is robust for
watertight CSG.

Run with the project's mesh venv:
    $VENV/bin/python pocketwatch.py

Coordinate convention
----------------------
    +Z = case axis, pointing OUT of the front face (toward the viewer / the OLED).
    Front face is the high-Z end; the back lid closes the low-Z end.
    The board's OLED end points toward +Y (12 o'clock); USB-C is at -Y (6 o'clock).
    The bail ring sits at 12 o'clock (+Y).
"""

import os
import math
import numpy as np
import trimesh
from trimesh.creation import cylinder, box, torus, annulus

# ----------------------------------------------------------------------------
# PARAMETERS  (all millimetres)  -- tweak these to fit your exact hardware
# ----------------------------------------------------------------------------

# --- Board: ESP32-C3 SuperMini + 0.42" OLED PCB ---
BOARD_L = 24.8    # long axis of the PCB
BOARD_W = 20.45   # short axis of the PCB
BOARD_H = 8.0     # tallest point (OLED glass + headers). Reserve this much depth.
BOARD_CLEAR = 0.6 # clearance around the board footprint in the pocket

# --- 0.42" OLED (module sits at ONE short end of the board) ---
# Lit area ~8.5 x 5 mm; the glass is larger. The window is cut to the glass size
# so the whole display is visible and slightly recessed/protected.
OLED_GLASS_W = 12.0   # glass width  (along board long axis / vertical on the face)
OLED_GLASS_H = 12.0   # glass height (along board short axis / horizontal on face)
# Distance from the board's OLED-end short edge to the centre of the glass.
# The 0.42" module glass centre sits a few mm in from the board edge.
OLED_CENTER_FROM_END = 6.5

# --- 502030 LiPo cell (thickness x width x length) ---
BAT_T = 5.0    # thickness (stacks along the case axis, Z)
BAT_W = 20.0   # width
BAT_L = 30.0   # length
BAT_CLEAR = 0.8  # clearance around the battery footprint

# --- Case shell ---
WALL = 2.4          # side (radial) wall thickness
FACE_T = 2.0        # front face thickness (the bezel around the window)
BACK_LIP_T = 1.6    # thickness of the lid's floor
FRONT_CLEAR = 0.8   # air gap between board top (OLED) and inside of front face
MID_CLEAR = 0.6     # gap/shelf between board bottom and battery
BACK_CLEAR = 0.6    # gap between battery and the back lid interior

# --- Press-fit lid interface ---
LID_LIP_H = 4.0     # how far the lid's lip reaches up into the body
LID_LIP_WALL = 1.4  # radial thickness of the lid lip wall
LID_FIT_GAP = 0.20  # radial clearance for a snug press fit (per side)

# --- USB-C access slot (on the rim, at 6 o'clock, -Y) ---
USB_SLOT_W = 11.0   # width of the slot (USB-C connector ~9 mm + tolerance)
USB_SLOT_H = 5.0    # height of the slot (fits USB-C ~3.2 mm + cable relief)

# --- BOOT button access hole (GPIO9). Small hole on the side wall. ---
# Angle is measured CCW from +X: 0 = 3 o'clock, 90 = 12 o'clock (bail),
# 180 = 9 o'clock, 270 = 6 o'clock (USB). Keep it clear of the bail (+Y) and
# USB slot (-Y); 3 o'clock (0 deg) is a clean spot on the side wall.
BOOT_HOLE_R = 2.2   # radius of the poke hole
BOOT_ANGLE_DEG = 0  # position around the rim (0 = +X / 3 o'clock side)

# --- Bail / chain loop at 12 o'clock ---
BAIL_TUBE_R = 1.6   # tube (minor) radius of the torus ring
BAIL_RING_R = 4.0   # ring (major) radius -> chain hole ~ 2*(RING_R-TUBE_R)

# --- Mesh resolution ---
CYL_SECTIONS = 160  # facets around cylinders (smooth round body)

OUTDIR = os.path.dirname(os.path.abspath(__file__))

# ----------------------------------------------------------------------------
# DERIVED DIMENSIONS
# ----------------------------------------------------------------------------

# The interior circle (in the face plane) must fully contain BOTH footprints:
#   - board rectangle:   BOARD_L x BOARD_W  (+ clearance)  -> needs dia >= diagonal
#   - battery rectangle: BAT_L  x BAT_W     (+ clearance)  -> needs dia >= diagonal
board_diag = math.hypot(BOARD_L + 2 * BOARD_CLEAR, BOARD_W + 2 * BOARD_CLEAR)
bat_diag = math.hypot(BAT_L + 2 * BAT_CLEAR, BAT_W + 2 * BAT_CLEAR)
INNER_DIA = max(board_diag, bat_diag)
INNER_R = INNER_DIA / 2.0

OUTER_R = INNER_R + WALL
OUTER_DIA = 2 * OUTER_R

# Interior depth budget (along Z), from inside-of-front-face down to lid floor:
#   FRONT_CLEAR + BOARD_H + MID_CLEAR + BAT_T + BACK_CLEAR
INNER_DEPTH = FRONT_CLEAR + BOARD_H + MID_CLEAR + BAT_T + BACK_CLEAR

# Total external height of the assembled watch (front face + interior + lid floor):
TOTAL_H = FACE_T + INNER_DEPTH + BACK_LIP_T

# Z planes (z=0 at the lid-floor OUTER surface; +Z toward the front face).
Z_BODY_BOTTOM = 0.0                 # where the body's own bottom rim sits (open end)
# The body is a cup: front face on top, open at the bottom where the lid plugs in.
# Body external spans from z = BACK_LIP_T (lid floor top / body open rim) up to TOTAL_H.
Z_BODY_OPEN = BACK_LIP_T            # body's open (bottom) rim; lid lip enters here
Z_FACE_OUT = TOTAL_H               # outer surface of the front face
Z_FACE_IN = Z_FACE_OUT - FACE_T    # inside surface of the front face

# Board sits just under the front face.
Z_BOARD_TOP = Z_FACE_IN - FRONT_CLEAR
Z_BOARD_BOT = Z_BOARD_TOP - BOARD_H
# Battery sits below the board.
Z_BAT_TOP = Z_BOARD_BOT - MID_CLEAR
Z_BAT_BOT = Z_BAT_TOP - BAT_T


def _report(name, mesh):
    ext = np.round(mesh.extents, 2)
    print(f"  {name:28s} extents={ext.tolist()}  watertight={mesh.is_watertight}"
          f"  vol={mesh.volume/1000:.2f}cm3")


def union(meshes):
    return trimesh.boolean.union(meshes, engine='manifold')


def difference(a, bs):
    return trimesh.boolean.difference([a] + list(bs), engine='manifold')


def make_body():
    """Round case body: solid cylinder, hollowed into a cup, with front-face
    window, board & battery pockets, USB slot, BOOT hole, lid recess, and bail."""

    parts_pos = []   # additive
    parts_neg = []   # subtractive

    # --- Outer solid cylinder for the whole body ---
    body_h = Z_FACE_OUT - Z_BODY_OPEN
    outer = cylinder(radius=OUTER_R, height=body_h, sections=CYL_SECTIONS)
    outer.apply_translation([0, 0, Z_BODY_OPEN + body_h / 2.0])

    body = outer

    # --- Main interior cavity (holds board + battery) ---
    # From the inside of the front face down to the body open rim.
    cav_h = Z_FACE_IN - Z_BODY_OPEN
    cavity = cylinder(radius=INNER_R, height=cav_h + 1.0, sections=CYL_SECTIONS)
    cavity.apply_translation([0, 0, Z_BODY_OPEN + (cav_h + 1.0) / 2.0 - 0.5])
    # (extends 0.5 below the open rim so the cut is clean through the bottom)
    parts_neg.append(cavity)

    # --- Lid recess: widen the bottom of the cavity so the lid lip's wall fits.
    # The lid lip is an annular wall of radial thickness LID_LIP_WALL that sits
    # against the inner face of the body wall. We carve a shallow counter-bore
    # so the body wall is locally thinner over LID_LIP_H, giving a ledge the lid
    # seats against. Recess outer radius:
    recess_r = INNER_R + LID_LIP_WALL + LID_FIT_GAP
    recess_h = LID_LIP_H + 0.5
    recess = cylinder(radius=recess_r, height=recess_h, sections=CYL_SECTIONS)
    recess.apply_translation([0, 0, Z_BODY_OPEN + recess_h / 2.0 - 0.25])
    parts_neg.append(recess)

    # --- OLED window through the front face ---
    # Board OLED end points to +Y. Glass centre is BOARD_L/2 - OLED_CENTER_FROM_END
    # from the board centre, along +Y.
    win_y = (BOARD_L / 2.0) - OLED_CENTER_FROM_END
    win = box(extents=[OLED_GLASS_H, OLED_GLASS_W, FACE_T + 2.0])
    win.apply_translation([0, win_y, Z_FACE_OUT - (FACE_T + 2.0) / 2.0 + 1.0])
    parts_neg.append(win)

    # --- USB-C slot on the rim at 6 o'clock (-Y) ---
    # Cut a rectangular notch through the side wall at the board's USB-C edge.
    # Centre it at the board's lower short edge (-Y) and at the board's Z level.
    usb_z = (Z_BOARD_TOP + Z_BOARD_BOT) / 2.0
    usb_depth = (OUTER_R - INNER_R) + 4.0   # cut fully through the wall
    usb = box(extents=[USB_SLOT_W, usb_depth, USB_SLOT_H])
    # place straddling the wall at -Y
    usb_y = -(INNER_R + (usb_depth / 2.0) - 2.0)
    usb.apply_translation([0, usb_y, usb_z])
    parts_neg.append(usb)

    # --- BOOT button poke hole on the side wall ---
    ang = math.radians(BOOT_ANGLE_DEG)
    boot_z = (Z_BOARD_TOP + Z_BOARD_BOT) / 2.0
    hole_len = (OUTER_R - INNER_R) + 6.0
    boot = cylinder(radius=BOOT_HOLE_R, height=hole_len, sections=48)
    # orient along radial (X by default is along its own Z after rotation);
    # cylinder axis is Z; rotate to point radially outward in XY plane.
    boot.apply_transform(trimesh.transformations.rotation_matrix(math.pi / 2, [0, 1, 0]))
    # now axis is along X. rotate around Z to the desired angle.
    boot.apply_transform(trimesh.transformations.rotation_matrix(ang, [0, 0, 1]))
    bx = math.cos(ang) * (INNER_R + (OUTER_R - INNER_R) / 2.0)
    by = math.sin(ang) * (INNER_R + (OUTER_R - INNER_R) / 2.0)
    boot.apply_translation([bx, by, boot_z])
    parts_neg.append(boot)

    # Apply all cuts.
    body = difference(body, parts_neg)

    # --- Bail / chain loop at 12 o'clock (+Y), a torus ring standing up in the
    #     plane of the case axis so a chain can pass through. ---
    ring = torus(major_radius=BAIL_RING_R, minor_radius=BAIL_TUBE_R,
                 major_sections=64, minor_sections=24)
    # torus default lies in XY plane (hole faces Z). Rotate so hole faces X
    # (chain passes side-to-side, ring stands proud above +Y edge).
    ring.apply_transform(trimesh.transformations.rotation_matrix(math.pi / 2, [1, 0, 0]))
    # position: just outside the rim at +Y, centred on the front-face-ish Z.
    ring_y = OUTER_R + BAIL_RING_R - BAIL_TUBE_R - 0.5   # overlap into wall a touch
    ring_z = Z_FACE_OUT - (BAIL_RING_R + BAIL_TUBE_R)    # keep within body height
    if ring_z < BAIL_RING_R:
        ring_z = TOTAL_H / 2.0
    ring.apply_translation([0, ring_y, ring_z])

    # A small neck connecting ring to body so it prints as one solid piece.
    neck = box(extents=[2 * BAIL_TUBE_R + 1.0, BAIL_RING_R + 2.0, 2 * BAIL_TUBE_R + 1.5])
    neck.apply_translation([0, OUTER_R + (BAIL_RING_R) / 2.0 - 1.0, ring_z])

    body = union([body, neck, ring])

    return body


def make_lid():
    """Press-fit back lid: a floor disc + an upstanding annular lip that plugs
    into the body's recess. A finger-notch on the rim helps prying it off."""

    # Floor disc: same outer radius as body, thickness BACK_LIP_T,
    # sitting from z=0 to z=BACK_LIP_T.
    floor = cylinder(radius=OUTER_R, height=BACK_LIP_T, sections=CYL_SECTIONS)
    floor.apply_translation([0, 0, BACK_LIP_T / 2.0])

    # Lip: annular wall that rises from the floor top into the body recess.
    # Outer radius matches the recess (minus fit gap), inner radius = outer - wall.
    lip_outer = INNER_R + LID_LIP_WALL       # nominal; fit gap handled on body side
    lip_inner = INNER_R - LID_FIT_GAP        # inner face flush-ish with cavity
    if lip_inner < 1.0:
        lip_inner = lip_outer - LID_LIP_WALL
    lip = annulus(r_min=lip_inner, r_max=lip_outer, height=LID_LIP_H,
                  sections=CYL_SECTIONS)
    lip.apply_translation([0, 0, BACK_LIP_T + LID_LIP_H / 2.0])

    lid = union([floor, lip])

    # Finger pry-notch: shave a shallow scallop on the outer rim so a thumbnail
    # can catch the lid edge.
    notch = cylinder(radius=3.0, height=BACK_LIP_T + 1.0, sections=32)
    notch.apply_translation([OUTER_R, 0, BACK_LIP_T / 2.0])
    lid = difference(lid, [notch])

    return lid


def make_assembly(body, lid):
    """Preview: nest the lid into the body at its seated position (no boolean)."""
    b = body.copy()
    l = lid.copy()
    # Lid floor bottom is at z=0 which is already the assembled position
    # (body open rim at Z_BODY_OPEN = BACK_LIP_T sits on lid floor top).
    scene = trimesh.util.concatenate([b, l])
    return scene


def main():
    print("=" * 74)
    print("POCKET WATCH CASE GENERATOR")
    print("=" * 74)
    print(f"Board footprint (w/clear): "
          f"{BOARD_L+2*BOARD_CLEAR:.1f} x {BOARD_W+2*BOARD_CLEAR:.1f} mm"
          f"  -> diagonal {board_diag:.2f} mm")
    print(f"Battery footprint (w/clear): "
          f"{BAT_L+2*BAT_CLEAR:.1f} x {BAT_W+2*BAT_CLEAR:.1f} mm"
          f"  -> diagonal {bat_diag:.2f} mm")
    print(f"INNER diameter : {INNER_DIA:.2f} mm  (radius {INNER_R:.2f})")
    print(f"OUTER diameter : {OUTER_DIA:.2f} mm  (radius {OUTER_R:.2f})")
    print(f"Interior depth : {INNER_DEPTH:.2f} mm")
    print(f"TOTAL height   : {TOTAL_H:.2f} mm  (excl. bail)")
    print(f"Z: face_out={Z_FACE_OUT:.2f} face_in={Z_FACE_IN:.2f} "
          f"board[{Z_BOARD_BOT:.2f},{Z_BOARD_TOP:.2f}] "
          f"bat[{Z_BAT_BOT:.2f},{Z_BAT_TOP:.2f}] open={Z_BODY_OPEN:.2f}")
    print("-" * 74)

    print("Building BODY ...")
    body = make_body()
    _report("body (raw)", body)
    if not body.is_watertight:
        body.merge_vertices()
        body.fill_holes()
        body.fix_normals()
        _report("body (repaired)", body)

    print("Building LID ...")
    lid = make_lid()
    _report("lid (raw)", lid)
    if not lid.is_watertight:
        lid.merge_vertices()
        lid.fill_holes()
        lid.fix_normals()
        _report("lid (repaired)", lid)

    # Export
    body_path = os.path.join(OUTDIR, "pocketwatch_body.stl")
    lid_path = os.path.join(OUTDIR, "pocketwatch_lid.stl")
    body.export(body_path)
    lid.export(lid_path)
    print("-" * 74)
    print(f"WROTE {body_path}")
    print(f"WROTE {lid_path}")

    # Assembly preview
    try:
        asm = make_assembly(body, lid)
        asm_path = os.path.join(OUTDIR, "pocketwatch_assembly.stl")
        asm.export(asm_path)
        print(f"WROTE {asm_path}  (preview, not for printing)")
    except Exception as e:
        print("assembly preview skipped:", e)

    print("=" * 74)
    print("FINAL")
    print(f"  BODY: extents {np.round(body.extents,2).tolist()} mm  "
          f"watertight={body.is_watertight}")
    print(f"  LID : extents {np.round(lid.extents,2).tolist()} mm  "
          f"watertight={lid.is_watertight}")
    ok = body.is_watertight and lid.is_watertight
    print(f"  ALL WATERTIGHT: {ok}")
    print("=" * 74)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
