# Pocket-watch case for ESP32-C3 SuperMini + 0.42" OLED

A classic round **pocket-watch** enclosure for the ESP32-C3 SuperMini (with the
0.42" SSD1306 OLED at one end), a **502030 LiPo** (5 x 20 x 30 mm) stacked behind
the board, and a **TP4056 USB-C charge board** (~17 x 17 x 2 mm) stacked behind
the battery so the cell can actually be recharged — the SuperMini has **no onboard
charging** (see [`docs/power.md`](../../docs/power.md)). Generated procedurally by
[`pocketwatch.py`](./pocketwatch.py) with `trimesh` + the `manifold` boolean engine.

Prior research (see [`docs/cases.md`](../../docs/cases.md)) confirmed no existing
STL fits this board **plus** an internal 502030 cell, so this one is built from
primitives (cylinders, boxes, a torus) combined with boolean unions/differences.

## Parts

| File | Print? | What it is |
|---|---|---|
| `pocketwatch_body.stl` | **yes** | Round case body: front face + OLED window, **two** USB-C slots (SuperMini @ 6 o'clock, TP4056 charge @ 9 o'clock), BOOT hole, chain bail. |
| `pocketwatch_lid.stl`  | **yes** | Press-fit back lid with a pry-notch **and a 3-sided retention fence** that cradles the TP4056. |
| `pocketwatch_assembly.stl` | no | Preview only — body + lid nested. Do **not** slice this. |

## Final dimensions

Measured from the exported STLs (both verified **watertight**, single-body):

| Property | Value |
|---|---|
| **Outer diameter** | **43.1 mm** *(unchanged — see note)* |
| Inner (cavity) diameter | 38.3 mm |
| **Total height (assembled, excl. bail)** | **21.2 mm** *(was 18.6 — grew +2.6 mm for the charger)* |
| Body bounding box (incl. bail) | 43.1 x 47.1 x 19.6 mm |
| Lid bounding box | 42.9 x 43.1 x 5.6 mm |
| Side wall thickness | 2.4 mm |
| Front face (bezel) thickness | 2.0 mm |
| Interior depth (usable) | 17.6 mm *(was 15.0)* |
| OLED window | 12.0 x 12.0 mm (through the face) |
| SuperMini USB-C slot | 11.0 (w) x 5.0 (h) mm, at **6 o'clock** |
| **TP4056 charge USB-C slot** | **11.0 (w) x 5.0 (h) mm, at 9 o'clock** |
| BOOT poke-hole | 4.4 mm dia, at 3 o'clock |
| Bail chain hole | ~4.8 mm (fits most chains/split-rings) |
| Lid lip engagement | 4.0 mm, 0.20 mm/side press-fit gap |

Interior stack (front to back): 0.8 mm air gap -> board (8 mm reserved) ->
0.6 mm shelf -> 5 mm battery -> **0.6 mm shelf -> 2 mm TP4056 -> 0.6 mm gap** ->
lid floor.

### Did the diameter or depth have to grow?

- **Diameter: NO — still 43.1 mm.** A 17 x 17 mm TP4056 will *not* fit in-plane
  beside the 30 x 20 mm battery at this size (the free lateral strip is only
  ~9 mm per side; a true side-by-side layout would need a ~49 mm circle), and a
  lateral rim "pod" would mean an ugly ~9 mm bulge. So the charger is **stacked
  flat behind the battery** instead. Its 25.7 mm diagonal fits easily inside the
  battery-driven 38.3 mm interior circle, nudged toward -X (`TP_X_OFFSET`) so its
  own USB-C edge faces the 9 o'clock slot. **Diameter stays battery-driven.**
- **Height/depth: YES — grew +2.6 mm** (total 18.6 -> **21.2 mm**; interior
  15.0 -> 17.6 mm), the cost of the TP4056 layer (0.6 shelf + 2.0 board +
  0.6 back gap). This is the "grow the thickness, not the footprint" option from
  [`docs/power.md`](../../docs/power.md) section 3.

The diameter is **driven by the battery**: a 30 x 20 mm cell (+0.8 mm clearance)
needs a 38.3 mm inner circle to lie flat; the 24.8 x 20.45 mm board and the
17 x 17 mm TP4056 both fit inside that comfortably. Change the parameters at the
top of the script and re-run to resize for a different board/cell/charger.

## Orientation on the board (as designed)

Looking at the **front face**:

- **12 o'clock (+Y):** OLED window and the chain **bail**. Orient the board so
  the OLED end points up.
- **6 o'clock (-Y):** the **SuperMini's** USB-C slot (for flashing / occasional
  powered use). The board's USB-C edge faces down. This is *up* near the board's
  Z level (z ~ 14 mm).
- **9 o'clock (-X):** the **TP4056's** USB-C charge slot. This is *down* at the
  TP4056's Z layer (z ~ 3 mm), behind the battery. Two **different** USB-C ports,
  two **different** clock positions, at two **different** heights — they never
  collide (see [charging](#charging-read-this)).
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
  The lip and the TP4056 **retention fence** both rise straight up from the floor,
  so they print cleanly with no overhangs.

### Fit / tuning
- The lid is a **0.20 mm/side press fit**. If it's too tight, bump `LID_FIT_GAP`
  to `0.30`; too loose, drop to `0.15`, and re-run the script.
- If the OLED sits proud of the face, increase `FRONT_CLEAR`. If the window
  clips the display, widen `OLED_GLASS_W/H`.

## Assembly

The interior is a **three-layer stack** (front to back): SuperMini board -> 502030
cell -> TP4056 charger. Because you build it front-to-back but *load* it from the
open back, seat the layers in reverse — board first (deepest, against the front
face), then battery, then charger last (nearest the lid).

1. Solder up the electronics per **[`docs/power.md`](../../docs/power.md)**
   (wiring summary below). Do this **before** closing the case — you cannot get a
   soldering iron in once it's shut, and the TP4056 sits at the very bottom.
2. Seat the **board** against the front face: OLED toward the 12 o'clock window,
   the SuperMini's USB-C toward the 6 o'clock slot. A dab of foam tape or a thin
   gasket keeps it from rattling.
3. Lay the **502030 battery** in on top of the board (it occupies the middle
   layer). Route its JST-PH / tab leads with a little slack.
4. Set the **TP4056** into the **retention fence on the lid** (the 3-sided frame
   moulded on the lid floor), USB-C edge toward the fence's open -X side so it
   lines up with the 9 o'clock slot. Keep the `OUT+`/`OUT-` -> SuperMini leads and
   the `B+`/`B-` -> cell leads tidy; there's ~0.6 mm of gap around the board.
5. Press the **lid** on until the lip bottoms out — this brings the TP4056 up to
   the battery, sandwiching the whole stack. Use the rim pry-notch (a thumbnail or
   spudger) to pop it back off. Check the charger's USB-C aligns with the 9 o'clock
   slot before fully seating.
6. Thread a chain/split-ring through the **bail**.

> **Wiring note (from [`docs/power.md`](../../docs/power.md)):** the two USB-C ports
> do different jobs. **Charge the battery through the TP4056's port (9 o'clock)**;
> use the **SuperMini's port (6 o'clock)** for flashing / USB power. Never charge
> through the SuperMini port — it has no charge circuit. See the caveat below for
> the not-charging-while-heavily-loaded gotcha.

## Charging (read this)

**The classic ESP32-C3 SuperMini has _no_ onboard LiPo charging and no battery
connector.** You cannot just plug the 502030 in and charge it through the
board's USB-C. Per **[`docs/power.md`](../../docs/power.md)** you must add a
**TP4056/TP4057 USB-C charge + protection board** — which **this case now has a
dedicated internal pocket for** (behind the battery, USB-C out the 9 o'clock rim):

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

This revision **embeds the TP4056 inside** the case, so charging is self-contained:

- The **TP4056 sits flat behind the battery**, held by the 3-sided retention fence
  on the lid, with its own USB-C facing the **9 o'clock** rim slot.
- The **SuperMini's** USB-C keeps its **6 o'clock** slot for flashing / USB power.
- **Two ports, two jobs.** They are at different clock positions *and* different
  heights, so the connectors never interfere. Charge via **9 o'clock**; flash via
  **6 o'clock**.

**How the charger was fit (and the tradeoff):** a 17 x 17 mm TP4056 does not fit
in-plane beside the 30 x 20 mm cell at 43 mm diameter, and a lateral pod would be
an ugly ~9 mm bulge — so it is **stacked**, which grew the case **+2.6 mm taller
(18.6 -> 21.2 mm)** but kept the round 43 mm footprint. This matches
[`docs/power.md`](../../docs/power.md) section 3, option 3 ("grow the thickness,
not the footprint").

**Fitment caveats to plan for:**

- `docs/power.md` notes real TP4056 modules are **~4-5 mm tall with the USB-C
  shell**, while the pocket budgets `TP_T = 2.0 mm` for the *bare PCB* (the shell
  intrudes into the 0.6 mm back gap and the slot's 5 mm height accommodates it).
  If your module is a chunky one, bump `TP_T` (and re-run) — the case just gets a
  hair taller.
- Some modules are **~26 x 17 mm** rather than 17 x 17; set `TP_L`/`TP_W` to match
  and re-run. A 26 mm length still fits the 38 mm circle when stacked.
- Don't **charge while smol is drawing heavy current** — it confuses the TP4056's
  end-of-charge detection (power off or deep-sleep while charging).
- Alternatively, mount the TP4056 **externally** (set `TP_RIB = False`, ignore the
  9 o'clock slot) and charge the cell out of the case; or switch to a board with
  **onboard charging** (DFRobot Beetle C3, SuperMini expansion carrier — see
  [board variance in `docs/power.md`](../../docs/power.md)) to drop the TP4056
  entirely and reclaim the 2.6 mm.

## Regenerating / customizing

```bash
# from repo root, using the mesh venv
$VENV/bin/python experiments/pocketwatch/pocketwatch.py
```

All hardware dimensions and clearances are variables at the top of the script
(`BOARD_L/W/H`, `BAT_T/W/L`, `WALL`, `FACE_T`, window/USB/bail sizes, fit gap).
The **TP4056 charger** is fully parametric too:

| Variable | Meaning |
|---|---|
| `TP_L`, `TP_W`, `TP_T` | charger board length / width / thickness (default 17 x 17 x 2 mm) |
| `TP_CLEAR`, `TP_GAP`, `TP_BACK_CLEAR` | clearance around it / shelf above (to battery) / gap below (to lid) |
| `TP_X_OFFSET` | how far the board is nudged toward -X so its USB-C reaches the slot |
| `TP_USB_W/H`, `TP_USB_ANGLE_DEG` | charge slot size and clock position (180 = 9 o'clock) |
| `TP_RIB`, `TP_RIB_H`, `TP_RIB_W` | the lid retention fence (set `TP_RIB = False` to omit) |

The script prints the computed diameters/heights (and the TP4056 layer's Z range
and USB-slot positions), then verifies both parts are **watertight** before
exporting.
