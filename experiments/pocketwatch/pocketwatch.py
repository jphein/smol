#!/usr/bin/env python3
"""
Parametric 3D-printable POCKET WATCH case for the ESP32-C3 SuperMini + 0.42" OLED
with a 502030 LiPo (5 x 20 x 30 mm) behind the board AND a TP4056 USB-C charge
board stacked behind the battery (the SuperMini has no onboard charging; see
docs/power.md).

Two printable parts are exported to this directory:
    pocketwatch_body.stl   - round case body: front face, OLED window, two USB
                             slots (SuperMini @ 6 o'clock, TP4056 @ 9 o'clock),
                             TWO top-face button plunger bores (BOOT + RESET) and
                             TWO LED light-pipe holes (clear-PLA), bail
    pocketwatch_lid.stl    - press-fit back lid
    pocketwatch_assembly.stl - (preview only) body + lid nested together

Buttons + LEDs (layout)
-----------------------
    On THIS user's actual ESP32-C3 SuperMini 0.42" OLED board the two tactile
    buttons (BOOT + RESET) are at the OLED / antenna end -- NOT the USB-C end
    that generic pinout docs show. So in our case (OLED at +Y / 12 o'clock) both
    buttons are up at the +Y ("top of screen") end. The case cuts two straight
    VERTICAL plunger bores through the front face, one directly over each button,
    FLANKING the OLED window left/right in X -- press with a pin / spudger /
    short printed plunger. HONESTY: RESET reboots the chip, so in firmware only
    BOOT is a usable input; the case exposes/actuates both anyway (RESET = manual
    reboot). Two small LED light-pipe holes over the blue (GPIO8, "IO8") and
    power ("PWR") LEDs are cut through the face at the -Y (USB-C / 6 o'clock)
    end, one on EACH SIDE of the USB-C slot -- CONFIRMED from a board photo
    (PWR bottom-left, IO8 bottom-right) -- meant to be printed in / plugged with
    CLEAR PLA. All button/LED positions are parametric (revision-dependent).

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

TP4056 charger placement (see docs/power.md, section 3)
-------------------------------------------------------
    A 17 x 17 mm TP4056 module fits neither in-plane beside the 30 x 20 battery
    (that needs a ~49 mm circle) nor as a lateral rim pod (a ~9 mm bulge that ruins
    the round shape) at the current 43 mm diameter. The clean solution keeps the
    round 43 mm body and STACKS the TP4056 flat behind the battery, growing the
    total height by ~2.6 mm (18.6 -> 21.2 mm). The 17 x 17 footprint fits easily
    inside the 38 mm interior circle; it is nudged toward -X so its own USB-C port
    reaches a second rim slot at 9 o'clock (kept clear of the SuperMini's 6 o'clock
    port and the 3 o'clock BOOT hole). Diameter is UNCHANGED; only depth grows.
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

# --- TP4056 USB-C charge + protection board (see docs/power.md) ---
# The SuperMini has NO onboard LiPo charging; a TP4056 does the charging and
# feeds OUT+/OUT- into the SuperMini's 5V/GND (via a Schottky). It has its OWN
# USB-C port that must reach an outside edge. It stacks flat behind the battery.
TP_L = 17.0    # board length (radial in the case, points toward its USB-C @ -X)
TP_W = 17.0    # board width  (tangential, along Y)
TP_T = 2.0     # board thickness (bare PCB; the USB-C shell is taller, see TP_USB_H)
TP_CLEAR = 0.6 # clearance around the TP4056 footprint in its layer
TP_GAP = 0.6   # gap between the battery and the TP4056 (shelf)
TP_BACK_CLEAR = 0.6  # gap between the TP4056 and the back lid interior
TP_X_OFFSET = -5.0   # board centre X: nudged toward -X (9 o'clock) so its USB-C edge
                     # faces the charge slot. Bounded so the retention fence still
                     # clears the lid lip (fence far corner r<=18.5 < lip_inner 18.9).
# A thin retention fence around the TP4056 footprint, moulded on the LID floor
# (the lid is the actual floor the TP4056 rests on), so it can't slide under the
# battery. It is a 3-sided frame (+X, +Y, -Y walls); the -X USB-C edge is left
# open so the connector reaches the 9 o'clock rim slot.
TP_RIB = True        # add the retention fence on the lid floor
TP_RIB_H = 1.6       # height of the fence above the lid floor
TP_RIB_W = 1.2       # wall thickness of the fence

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

# --- SuperMini USB-C access slot (on the rim, at 6 o'clock, -Y) ---
# This port is for flashing / occasional powered use of the SuperMini itself.
USB_SLOT_W = 11.0   # width of the slot (USB-C connector ~9 mm + tolerance)
USB_SLOT_H = 5.0    # height of the slot (fits USB-C ~3.2 mm + cable relief)

# --- TP4056 charge USB-C slot (on the rim, at 9 o'clock, -X) ---
# The TP4056's own charge port. Deliberately at a DIFFERENT clock position and a
# different Z (down at the TP4056 layer) than the SuperMini port so the two USB-C
# connectors never collide. See docs/power.md: these are two separate ports.
TP_USB_W = 11.0        # width of the charge slot
TP_USB_H = 5.0         # height (USB-C shell ~3.2 mm + relief; shell is taller than the bare PCB)
TP_USB_ANGLE_DEG = 180 # 180 = 9 o'clock (-X). 6 o'clock (270) is the SuperMini port.

# ----------------------------------------------------------------------------
# BOARD BUTTONS + LEDs  -- physical layout (see README + comments)
# ----------------------------------------------------------------------------
# The two tactile buttons are BOOT (GPIO9, via RST2/R6) and RESET (tied to
# EN/CHIP_PU).
#
# BUTTON END -- CORRECTED BY THE OWNER OF THE ACTUAL BOARD:
#   Generic ESP32-C3 SuperMini docs (lastminuteengineers, espboards.dev, etc.)
#   put both buttons at the USB-C end. BUT on THIS user's actual 0.42" OLED
#   board the two buttons are at the OPPOSITE end -- the OLED / antenna end. So
#   in our case frame (OLED at +Y / 12 o'clock, USB-C at -Y / 6 o'clock) BOTH
#   buttons are up at the +Y (12 o'clock / OLED / "top of screen") end. The
#   plunger bores are placed there and FLANK the OLED window left/right in X.
#   This also matches the original "buttons at the top of the screen" intent.
#
# LEDs (blue user LED on GPIO8, active-LOW; power LED that lights on USB/VBUS):
#   CONFIRMED FROM A PHOTO OF THE ACTUAL BOARD -- the two LEDs are at the USB-C
#   (BOTTOM / -Y / 6 o'clock) end, FLANKING the USB-C connector. Silkscreen reads
#   "PWR" on the bottom-LEFT (-X) and "IO8" (the GPIO8 blue LED) on the bottom-
#   RIGHT (+X). So the two LED light-pipe holes go at the -Y end, one on each
#   side of the USB-C rim slot -- NOT at the OLED end. (Confirmed, not assumed.)
#
# Exact X/Y of every part VARIES BY BOARD REVISION, so all positions below are
# parameters. HONESTY CAVEAT (also in README): RESET reboots the chip, so in
# FIRMWARE only BOOT is a usable input; the case actuates BOTH as requested
# (RESET stays useful as a manual reboot / recovery / bootloader-combo press).
#
# Board coordinate convention (matches the OLED window math below):
#   board centred at XY origin; long axis (BOARD_L) along Y; short axis
#   (BOARD_W) along X; +Y = OLED / button end, -Y = USB-C end.

# The OLED window centre (+Y) and its half-extents, reused to place things that
# must FLANK or SIT ABOVE the window without overlapping it.
_WIN_Y = (BOARD_L / 2.0) - OLED_CENTER_FROM_END   # window centre Y (== win_y below)
_WIN_HALF_X = OLED_GLASS_H / 2.0                  # window half-width in X (face)

# --- Button physical positions on the PCB (board-frame, mm) ---
# Buttons are at the +Y (OLED) end and FLANK the window left/right. Their Y sits
# at the window centre; their X is pushed just outside the window edge with a
# comfortable bezel gap.
BTN_Y = _WIN_Y                       # button centres level with the window centre
BTN_X_FROM_WIN_EDGE = 2.6            # gap from the window edge out to the bore CENTRE
BTN_X = _WIN_HALF_X + BTN_X_FROM_WIN_EDGE   # each button this far off centre in X
# BOOT (GPIO9) and RESET (EN). Swap the signs if your board mirrors them.
BOOT_BTN_X = +BTN_X                  # BOOT to +X side of the window (3 o'clock-ish)
RESET_BTN_X = -BTN_X                 # RESET to -X side of the window (9 o'clock-ish)

# --- BOTH buttons routed to the TOP via vertical plunger bores in the FACE ---
# The buttons are at the +Y (OLED / top) end; the spec wants both actuatable
# from the TOP (the front face). The most printable, genuinely-actuatable
# approach is a straight VERTICAL through-hole in the front face directly ABOVE
# each button: drop in a pin, a spudger, or a short loose-fit printed plunger
# and press straight down onto the switch. (A bent channel would not print as a
# single clean bore; a vertical face bore prints cleanly face-down, no supports.)
BTN_ACTUATE = True          # cut the two vertical button bores in the face
BTN_BORE_R = 1.8            # bore radius (fits a ~3 mm pin / printed plunger)
BTN_BORE_CLEAR_Z = 0.4      # stop the bore this far ABOVE the button top so a
                            # plunger bottoms on the switch, not on the plastic
# Optional loose-fit printed plungers (a short peg per bore). Off by default --
# a toothpick/spudger works; enable to also export peg solids into the assembly.
BTN_PLUNGER_PREVIEW = False

# --- LED light-pipe holes (print in / plug with CLEAR PLA) ---
# Two small through-holes in the front face over the two onboard LEDs (blue
# GPIO8 + power LED), sized to take a clear-PLA light pipe or to be bridged with
# a clear-PLA plug so the LEDs are visible from the front. CONFIRMED from a board
# photo: the LEDs are at the USB-C (-Y / 6 o'clock) end, one on EACH SIDE of the
# USB-C connector -- "PWR" bottom-left (-X), "IO8"/GPIO8 blue bottom-right (+X).
LED_PIPE = True             # cut the two LED light-pipe holes in the face
LED_PIPE_R = 1.25           # radius -> 2.5 mm holes (light-pipe / clear-PLA plug)
# Y sits a little IN from the board's -Y (USB-C) short edge (LEDs are near, but
# inboard of, the connector so they stay clear of the rim USB-C slot).
LED_Y_FROM_USB_EDGE = 4.0   # gap from the -Y board edge inward to the LED CENTRE
LED_Y = -BOARD_L / 2.0 + LED_Y_FROM_USB_EDGE
# X flanks the USB-C connector: pushed just outside the rim slot's half-width so
# the light-pipe holes straddle the connector without touching the slot.
LED_X_FROM_USB_EDGE = 1.5   # gap from the USB-C slot edge out to each LED CENTRE
LED_X = USB_SLOT_W / 2.0 + LED_X_FROM_USB_EDGE   # each LED this far off centre in X
BLUE_LED_X = +LED_X                 # "IO8" GPIO8 blue LED -- bottom-RIGHT (+X)
PWR_LED_X = -LED_X                  # "PWR" power LED       -- bottom-LEFT  (-X)

# --- (Legacy) side-wall BOOT poke hole. Kept OFF: both buttons now go via the
#     top face bores above. Set True to ALSO get the old 3 o'clock side poke. ---
BOOT_SIDE_HOLE = False   # old radial side-wall poke at 3 o'clock (superseded)
BOOT_HOLE_R = 2.2   # radius of the (optional) side poke hole
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

# The interior circle (in the face plane) must fully contain ALL footprints:
#   - board rectangle:    BOARD_L x BOARD_W  (+ clearance)  -> needs dia >= diagonal
#   - battery rectangle:  BAT_L  x BAT_W     (+ clearance)  -> needs dia >= diagonal
#   - TP4056 rectangle:   TP_L   x TP_W      (+ clearance)  -> stacks BELOW the
#     battery, so it does not enlarge the circle (its 24 mm diagonal fits easily
#     inside the battery-driven 38 mm circle). Diameter stays battery-driven.
board_diag = math.hypot(BOARD_L + 2 * BOARD_CLEAR, BOARD_W + 2 * BOARD_CLEAR)
bat_diag = math.hypot(BAT_L + 2 * BAT_CLEAR, BAT_W + 2 * BAT_CLEAR)
tp_diag = math.hypot(TP_L + 2 * TP_CLEAR, TP_W + 2 * TP_CLEAR)
INNER_DIA = max(board_diag, bat_diag)   # tp_diag is smaller; it stacks, not spreads
INNER_R = INNER_DIA / 2.0

OUTER_R = INNER_R + WALL
OUTER_DIA = 2 * OUTER_R

# Interior depth budget (along Z), from inside-of-front-face down to lid floor.
# The TP4056 charge board is STACKED behind the battery, so it adds its own layer:
#   FRONT_CLEAR + BOARD_H + MID_CLEAR + BAT_T + TP_GAP + TP_T + TP_BACK_CLEAR
INNER_DEPTH = (FRONT_CLEAR + BOARD_H + MID_CLEAR + BAT_T
               + TP_GAP + TP_T + TP_BACK_CLEAR)

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
# TP4056 charge board sits below the battery (its own layer), against the lid.
Z_TP_TOP = Z_BAT_BOT - TP_GAP
Z_TP_BOT = Z_TP_TOP - TP_T


def _report(name, mesh):
    ext = np.round(mesh.extents, 2)
    print(f"  {name:28s} extents={ext.tolist()}  watertight={mesh.is_watertight}"
          f"  vol={mesh.volume/1000:.2f}cm3")


def union(meshes):
    return trimesh.boolean.union(meshes, engine='manifold')


def difference(a, bs):
    return trimesh.boolean.difference([a] + list(bs), engine='manifold')


def _board_y(y_from_usb_edge):
    """Convert a distance measured from the board's USB-C (-Y) short edge toward
    the OLED into a case-frame Y. Board is centred at the XY origin with its long
    axis (BOARD_L) on Y and the USB-C edge at -BOARD_L/2."""
    return -BOARD_L / 2.0 + y_from_usb_edge


def _face_bore(x, y, r, z_bottom):
    """A vertical cylinder cutter (axis = Z) that pierces the front face from
    just above the outer face down to ``z_bottom``. Used for the button plunger
    bores and the LED light-pipe holes. Overshoots the top by 1 mm for a clean
    cut and is centred at (x, y)."""
    top = Z_FACE_OUT + 1.0
    h = top - z_bottom
    c = cylinder(radius=r, height=h, sections=48)
    c.apply_translation([x, y, z_bottom + h / 2.0])
    return c


def _rim_slot(width, height, z, angle_deg):
    """A rectangular cutter that punches a USB-C-sized notch straight through the
    side wall at the given clock ``angle_deg`` (0=+X/3 o'clock, 90=+Y/12,
    180=-X/9, 270=-Y/6) and vertical centre ``z``. ``width`` is the tangential
    opening, ``height`` the vertical opening. Returned as a negative (cut) solid."""
    depth = (OUTER_R - INNER_R) + 4.0        # long enough to cut fully through the wall
    slot = box(extents=[width, depth, height])
    # box long axis is +Y; centre it straddling the wall along +Y, then rotate to angle.
    slot.apply_translation([0, INNER_R + depth / 2.0 - 2.0, 0])
    # rotate about Z from +Y (90 deg) to the requested angle.
    slot.apply_transform(trimesh.transformations.rotation_matrix(
        math.radians(angle_deg - 90.0), [0, 0, 1]))
    slot.apply_translation([0, 0, z])
    return slot


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

    # --- SuperMini USB-C slot on the rim at 6 o'clock (-Y) ---
    # Cut a rectangular notch through the side wall at the board's USB-C edge.
    # Centre it at the board's lower short edge (-Y) and at the board's Z level.
    usb_z = (Z_BOARD_TOP + Z_BOARD_BOT) / 2.0
    parts_neg.append(_rim_slot(USB_SLOT_W, USB_SLOT_H, usb_z, 270.0))

    # --- TP4056 charge USB-C slot on the rim at 9 o'clock (-X) ---
    # A SECOND, independent USB-C port for the charger. It sits down at the
    # TP4056's Z layer (behind the battery) and at a different clock angle, so it
    # never collides with the SuperMini port above it. See docs/power.md.
    tp_usb_z = (Z_TP_TOP + Z_TP_BOT) / 2.0
    parts_neg.append(_rim_slot(TP_USB_W, TP_USB_H, tp_usb_z, TP_USB_ANGLE_DEG))

    # --- BOTH buttons routed to the TOP: vertical plunger bores in the face ---
    # Two straight Z-bores through the front face, one directly above each
    # button (BOOT and RESET), stopping BTN_BORE_CLEAR_Z above the button top so
    # a pin / short printed plunger presses the switch itself, not the plastic.
    # The buttons are at the +Y (OLED) end, flanking the window in X; they sit at
    # the board top plane (same tall features as the OLED).
    if BTN_ACTUATE:
        btn_top_z = Z_BOARD_TOP                       # switch cap ~ board-top plane
        bore_stop = btn_top_z + BTN_BORE_CLEAR_Z      # leave a thin air gap
        for bx in (BOOT_BTN_X, RESET_BTN_X):
            parts_neg.append(_face_bore(bx, BTN_Y, BTN_BORE_R, bore_stop))

    # --- Two LED light-pipe holes in the face (clear-PLA) ---
    # Small through-holes over the blue GPIO8 LED and the power LED, punched all
    # the way through the face (down to the board-top plane) so a clear-PLA light
    # pipe / plug carries the glow to the front. See README: print/plug in CLEAR.
    if LED_PIPE:
        led_stop = Z_BOARD_TOP                        # pierce fully through the face
        for lx in (BLUE_LED_X, PWR_LED_X):
            parts_neg.append(_face_bore(lx, LED_Y, LED_PIPE_R, led_stop))

    # --- (Optional, legacy) BOOT button poke hole on the side wall @ 3 o'clock ---
    if BOOT_SIDE_HOLE:
        ang = math.radians(BOOT_ANGLE_DEG)
        boot_z = (Z_BOARD_TOP + Z_BOARD_BOT) / 2.0
        hole_len = (OUTER_R - INNER_R) + 6.0
        boot = cylinder(radius=BOOT_HOLE_R, height=hole_len, sections=48)
        # cylinder axis is Z; rotate to point radially outward in XY plane.
        boot.apply_transform(trimesh.transformations.rotation_matrix(math.pi / 2, [0, 1, 0]))
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

    # --- TP4056 retention fence on the lid floor ---
    # The TP4056 rests on the lid floor (this disc). Frame its footprint with a
    # low fence so it can't drift under the battery, leaving the -X (USB-C) edge
    # open so the connector reaches the 9 o'clock rim slot.
    if TP_RIB:
        fx = TP_X_OFFSET
        fw = TP_W + 2 * TP_CLEAR          # footprint Y span (tangential)
        fl = TP_L + 2 * TP_CLEAR          # footprint X span (radial)
        x_lo = fx - fl / 2.0              # -X edge (USB-C side) -> left OPEN
        x_hi = fx + fl / 2.0              # +X edge (toward centre)
        y_hi = fw / 2.0
        z0 = BACK_LIP_T                   # fence rises from the floor top
        rib_parts = []
        # +X wall (closes the side toward the case centre)
        w = box(extents=[TP_RIB_W, fw + 2 * TP_RIB_W, TP_RIB_H])
        w.apply_translation([x_hi + TP_RIB_W / 2.0, 0.0, z0 + TP_RIB_H / 2.0])
        rib_parts.append(w)
        # +Y and -Y walls (span from the open -X side to the +X wall)
        for sign in (+1, -1):
            wl = box(extents=[fl, TP_RIB_W, TP_RIB_H])
            wl.apply_translation([fx, sign * (y_hi + TP_RIB_W / 2.0),
                                  z0 + TP_RIB_H / 2.0])
            rib_parts.append(wl)
        lid = union([lid] + rib_parts)

    # Finger pry-notch: shave a shallow scallop on the outer rim so a thumbnail
    # can catch the lid edge.
    notch = cylinder(radius=3.0, height=BACK_LIP_T + 1.0, sections=32)
    notch.apply_translation([OUTER_R, 0, BACK_LIP_T / 2.0])
    lid = difference(lid, [notch])

    return lid


def make_plungers():
    """Optional loose-fit printed button plungers: a short peg per button bore,
    sitting in the bore with its tip near the switch. Preview / print-separately
    aid; a toothpick or spudger works just as well. Returns a list of meshes."""
    pegs = []
    if not (BTN_ACTUATE and BTN_PLUNGER_PREVIEW):
        return pegs
    peg_r = BTN_BORE_R - 0.25              # loose slip fit in the bore
    tip_z = Z_BOARD_TOP + BTN_BORE_CLEAR_Z # rests just above the switch cap
    peg_h = (Z_FACE_OUT + 1.5) - tip_z     # protrudes ~1.5 mm proud of the face
    for x in (BOOT_BTN_X, RESET_BTN_X):
        p = cylinder(radius=peg_r, height=peg_h, sections=32)
        p.apply_translation([x, BTN_Y, tip_z + peg_h / 2.0])
        pegs.append(p)
    return pegs


def make_assembly(body, lid):
    """Preview: nest the lid into the body at its seated position (no boolean)."""
    b = body.copy()
    l = lid.copy()
    # Lid floor bottom is at z=0 which is already the assembled position
    # (body open rim at Z_BODY_OPEN = BACK_LIP_T sits on lid floor top).
    parts = [b, l] + make_plungers()
    scene = trimesh.util.concatenate(parts)
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
    print(f"TP4056 footprint (w/clear): "
          f"{TP_L+2*TP_CLEAR:.1f} x {TP_W+2*TP_CLEAR:.1f} mm"
          f"  -> diagonal {tp_diag:.2f} mm  (stacks below battery, off-centre X={TP_X_OFFSET})")
    print(f"INNER diameter : {INNER_DIA:.2f} mm  (radius {INNER_R:.2f})  "
          f"[battery-driven; TP4056 diag {tp_diag:.2f} fits inside]")
    print(f"OUTER diameter : {OUTER_DIA:.2f} mm  (radius {OUTER_R:.2f})")
    # Net growth vs. the pre-charger design (which ended at ...+BAT_T+BACK_CLEAR,
    # BACK_CLEAR being 0.6, now re-used as TP_GAP): +TP_T + TP_BACK_CLEAR.
    depth_growth = TP_T + TP_BACK_CLEAR
    print(f"Interior depth : {INNER_DEPTH:.2f} mm  "
          f"(+{depth_growth:.1f} vs. no-charger design, for the TP4056 layer)")
    print(f"TOTAL height   : {TOTAL_H:.2f} mm  (excl. bail)")
    print(f"Z: face_out={Z_FACE_OUT:.2f} face_in={Z_FACE_IN:.2f} "
          f"board[{Z_BOARD_BOT:.2f},{Z_BOARD_TOP:.2f}] "
          f"bat[{Z_BAT_BOT:.2f},{Z_BAT_TOP:.2f}] "
          f"tp4056[{Z_TP_BOT:.2f},{Z_TP_TOP:.2f}] open={Z_BODY_OPEN:.2f}")
    print(f"USB slots: SuperMini @ 6 o'clock z={ (Z_BOARD_TOP+Z_BOARD_BOT)/2.0:.2f}; "
          f"TP4056 @ {TP_USB_ANGLE_DEG} deg (9 o'clock) z={(Z_TP_TOP+Z_TP_BOT)/2.0:.2f}")
    if BTN_ACTUATE:
        print(f"Button top-face bores (r={BTN_BORE_R}): "
              f"BOOT @ (x={BOOT_BTN_X:+.1f}, y={BTN_Y:+.1f}), "
              f"RESET @ (x={RESET_BTN_X:+.1f}, y={BTN_Y:+.1f})  "
              f"[both at +Y/OLED end, flanking window (half-width {_WIN_HALF_X:.1f}); "
              f"bore stops z={Z_BOARD_TOP+BTN_BORE_CLEAR_Z:.2f}]")
        print("  NOTE: RESET reboots the chip -> only BOOT is a firmware input; "
              "case actuates BOTH as requested.")
    if LED_PIPE:
        print(f"LED light-pipe holes (r={LED_PIPE_R}, CLEAR-PLA, at -Y/USB-C end, "
              f"flanking the USB-C slot; CONFIRMED from board photo): "
              f"IO8/GPIO8 blue @ (x={BLUE_LED_X:+.1f}, y={LED_Y:+.1f}), "
              f"PWR @ (x={PWR_LED_X:+.1f}, y={LED_Y:+.1f})")
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
