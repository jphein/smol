# Pocket-watch case for ESP32-C3 SuperMini + 0.42" OLED

A classic round **pocket-watch** enclosure for the ESP32-C3 SuperMini (with the
0.42" SSD1306 OLED at one end) and a **502030 LiPo** (5 x 20 x 30 mm) stacked
behind the board. Generated procedurally by [`pocketwatch.py`](./pocketwatch.py)
with `trimesh` + the `manifold` boolean engine.

Prior research (see [`docs/cases.md`](../../docs/cases.md)) confirmed no existing
STL fits this board **plus** an internal 502030 cell, so this one is built from
primitives (cylinders, boxes, a torus) combined with boolean unions/differences.

## Parts

| File | Print? | What it is |
|---|---|---|
| `pocketwatch_body.stl` | **yes** | Round case body: front face + OLED window, board & battery pockets, USB-C slot, BOOT hole, chain bail. |
| `pocketwatch_lid.stl`  | **yes** | Press-fit back lid with a pry-notch. |
| `pocketwatch_assembly.stl` | no | Preview only — body + lid nested. Do **not** slice this. |

## Final dimensions

| Property | Value |
|---|---|
| **Outer diameter** | **43.1 mm** |
| Inner (cavity) diameter | 38.3 mm |
| **Total height (assembled, excl. bail)** | **18.6 mm** |
| Side wall thickness | 2.4 mm |
| Front face (bezel) thickness | 2.0 mm |
| Interior depth (usable) | 15.0 mm |
| OLED window | 12.0 x 12.0 mm (through the face) |
| USB-C slot | 11.0 (w) x 5.0 (h) mm, at 6 o'clock |
| BOOT poke-hole | 4.4 mm dia, at 3 o'clock |
| Bail chain hole | ~4.8 mm (fits most chains/split-rings) |
| Lid lip engagement | 4.0 mm, 0.20 mm/side press-fit gap |

Interior stack (front to back): 0.8 mm air gap -> board (8 mm reserved) ->
0.6 mm shelf -> 5 mm battery -> 0.6 mm gap -> lid floor.

The diameter is **driven by the battery**: a 30 x 20 mm cell (+0.8 mm clearance)
needs a 38.3 mm inner circle to lie flat; the 24.8 x 20.45 mm board fits inside
that comfortably. Change the parameters at the top of the script and re-run to
resize for a different board/cell.

## Orientation on the board (as designed)

Looking at the **front face**:

- **12 o'clock (+Y):** OLED window and the chain **bail**. Orient the board so
  the OLED end points up.
- **6 o'clock (-Y):** **USB-C** slot (for charging/flashing). The board's USB-C
  edge faces down.
- **3 o'clock (+X):** **BOOT** button poke-hole (GPIO9).

## Printing

- **Material:** PLA or PETG. PETG if it'll ride in a warm pocket.
- **Layer height:** 0.2 mm. **Walls:** 3 perimeters. **Infill:** 15-20% is plenty
  (walls carry the load). Rough solid volume is ~9 cm3 total (~11 g PLA solid;
  much less at low infill).

### Body
- **Print face-down** (front face on the bed, open end up). The flat circular
  face gives the best window edges and needs **no supports** — every internal
  overhang (pockets, lid recess) faces upward and prints cleanly.
- The **bail** is the only real overhang. Face-down it prints as a small
  horizontal loop off the rim; enable **supports "touching buildplate" only**,
  or just let a short bridge span the ring hole (it's only ~5 mm). If your
  slicer struggles, print the body **face-up** instead and support the window +
  bail — but face-down gives the nicer display bezel.

### Lid
- **Print floor-down** (the flat disc on the bed, lip pointing up). **No supports.**

### Fit / tuning
- The lid is a **0.20 mm/side press fit**. If it's too tight, bump `LID_FIT_GAP`
  to `0.30`; too loose, drop to `0.15`, and re-run the script.
- If the OLED sits proud of the face, increase `FRONT_CLEAR`. If the window
  clips the display, widen `OLED_GLASS_W/H`.

## Assembly

1. Solder up the electronics per **[`docs/power.md`](../../docs/power.md)**
   (charging caveat below).
2. Drop the **battery** into the body first (it sits at the back, against where
   the lid floor will be).
3. Seat the **board** on top, OLED toward the 12 o'clock window, USB-C toward
   the 6 o'clock slot. A dab of foam tape or a thin gasket keeps it from
   rattling; the front face + battery sandwich it in place.
4. Press the **lid** on until the lip bottoms out. Use the rim pry-notch (a
   thumbnail or spudger) to pop it back off.
5. Thread a chain/split-ring through the **bail**.

## Charging caveat (read this)

**The classic ESP32-C3 SuperMini has _no_ onboard LiPo charging and no battery
connector.** You cannot just plug the 502030 in and charge it through the
board's USB-C. Per **[`docs/power.md`](../../docs/power.md)** you must add a
**TP4056/TP4057 USB-C charge + protection board**:

```
USB-C -> TP4056 (B+/B- to cell) -> OUT+ --[Schottky]--> SuperMini 5V/VBUS -> onboard LDO -> 3V3
                                    OUT- ------------->  SuperMini GND
```

Key points from `docs/power.md`:

- **Never** wire the raw 4.2 V cell to the `3V3` pin (that's the LDO *output*;
  the ESP32-C3 max is 3.6 V — you'll kill it).
- Feed the protected cell into **`5V`/`VBUS`** and let the onboard ME6211 LDO
  make 3.3 V.
- Use a TP4056 with the **DW01 + FS8205A protection** pair, and **swap the
  program resistor** so it charges at ~**125 mA (0.5C)** — the stock 1 A is 4C
  for a 250 mAh cell and is unsafe.
- Add a **Schottky** on `OUT+ -> 5V` so USB into the SuperMini can't back-feed
  the cell, and don't charge while smol is drawing heavy current (it confuses
  the TP4056's end-of-charge detection).

### This case and charging

The single USB-C slot on the rim is aligned to the **SuperMini's** USB-C port
(for flashing / occasional powered use). If your **TP4056** is mounted inside,
you'll want to route its USB-C to the same slot instead, or widen/duplicate the
slot. The interior depth (15 mm) has a little slack behind the 5 mm cell for a
small TP4056, but it's tight — plan the wiring before you close the lid. If it
won't fit, mount the TP4056 externally on the chain or charge the cell out of
the case. See [board variance](../../docs/power.md) — some SuperMini variants and
the official expansion board *do* include charging, which would remove this
whole problem.

## Regenerating / customizing

```bash
# from repo root, using the mesh venv
$VENV/bin/python experiments/pocketwatch/pocketwatch.py
```

All hardware dimensions and clearances are variables at the top of the script
(`BOARD_L/W/H`, `BAT_T/W/L`, `WALL`, `FACE_T`, window/USB/bail sizes, fit gap).
The script prints the computed diameters/heights and verifies both parts are
**watertight** before exporting.
