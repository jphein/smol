# Bench measurement design — band rendering vs a PSRAM frame

**Morpheus, 2026-09-22. A DESIGN, not a run.** Written so that when the lead asks JP for his board,
the ask is precise and short (§6 is that ask, three lines). Nothing here has been executed and
**nothing is to be flashed without JP's go** — see §5 for why that is more than protocol on this
particular bench.

Settles the provisional half of [`TAPSTONE-SEAM.md`](TAPSTONE-SEAM.md) §3/§5.3.

---

## 1. The actual question, which is narrower than "is band rendering fast enough"

Three lines of reasoning already agree that bands beat a 150 KB PSRAM frame (`spike-scry` never
buffers a frame by design; `BOARD.md`'s house rule is strip-rasterise + windowed `fill_contiguous`;
Luna's costing puts a lane column at ~35% of the frame budget as one window against ~97% split per
row). None is a measurement. But the thing to measure is not "bands: yes or no" — it is a **tension
between two constraints that pull in opposite directions**, and neither side of it is known:

- **Window count wants FEW bands.** Every `fill_contiguous` is a CASET/RASET/RAMWR round trip.
  `BOARD.md` measured per-cell windows at **2× slower than a full-screen repaint**, and Luna's 35%
  vs 97% is the same effect. Fewer, larger windows win.
- **Internal SRAM wants MANY bands.** A band is `320 px × 2 B × rows` = `640 × rows` bytes, and
  `budget.rs` puts the S3's free DRAM at **96,676 B** — out of which the stack floor already takes
  its share:

| bands | rows | strip buffer | of 96,676 B free DRAM |
|---|---|---|---|
| 1 (full frame) | 240 | 153,600 B | **impossible** — PSRAM only |
| 4 | 60 | 38,400 B | 40% |
| 8 | 30 | 19,200 B | 20% |
| 16 | 15 | 9,600 B | 10% |
| 30 (`spike-scry`'s `STRIP_ROWS = 8`) | 8 | 5,120 B | 5% |

**So the question is: what is the smallest band count that still hits the frame budget, and does
its buffer fit in internal SRAM?** If the answer is "4 bands or fewer", 38 KB of DRAM for one app's
scratch is a real cost and PSRAM deserves another look. If it is "16 bands is fine", bands win
outright and the PSRAM discussion is closed.

## 2. The reframing that probably makes this easy, and should be checked first

**0010's ~15 fps applies to the animated band, not to a full repaint.** A full battlefield redraw
is triggered by a *card being tapped on the reader* — human-paced, once every few seconds. 0010 says
"one animated band at a time", and 0027's geometry makes that band a lane column.

So the two requirements are very different:

| what | trigger | budget |
|---|---|---|
| **one animated band** | animation timer | **≤ 66 ms** (15 fps) — the real constraint |
| **full battlefield repaint** | a human tapping a card | ~100–150 ms is invisible |

If M4 below comes in comfortably, the full-frame number stops being load-bearing and the whole
PSRAM question is moot regardless of M2/M3. **Measure M4 first** — it is the cheapest measurement
and the most likely to end the discussion.

## 3. The measurements

Five numbers, plus a control that comes before all of them.

### M0 — the control, and it runs first
Before any figure is trusted: `Delay::delay_millis(50)` timed with the same
`esp_hal::time::Instant` path the other measurements use, and an empty-loop timing to get the
instrument's own overhead.

**Why this is not ceremony:** the timer is the instrument for every number below, and a plausible
wrong number here is unfalsifiable. This lane has already had two instruments lie inside one day —
an `nm` that reported zero symbols for *everything* because a shell had not sourced
`export-esp.sh`, and a probe that measured ~2 KB because the optimiser had const-folded the call it
was measuring away. Both were caught by a control and neither would have been caught by inspection.
If M0 does not read ≈50 ms, stop; nothing after it means anything.

### M1 — the wire floor
One `fill_contiguous` of the full 320×240 from a pre-filled buffer (in PSRAM, since it must be 150
KB — this measurement is about the SPI wire, not about where the pixels came from).
**Expect ≈27 ms** (Nebula's cited figure, explore-ember). Anything wildly different means the SPI
clock or the panel setup differs from that measurement's conditions, and every other number needs
re-reading.

### M2 — the rasterise overdraw factor
Draw 0027's real battlefield (Luna's geometry, via whatever subset of her renderer is portable at
the time) (a) once into a full-frame PSRAM buffer, and (b) clipped into N strips, for N in
{4, 8, 16}. The ratio is the **overdraw factor** — how much re-running the draw calls per band
costs. `TAPSTONE-SEAM.md` guessed 8× as an upper bound with no evidence; this replaces the guess.
Expect it to be well below N, since most draw calls are rejected cheaply by the clip rectangle.

### M3 — window-count sensitivity
The same full frame of pixels pushed as 1, 4, 8, 16 and 240 windows.
Validates `BOARD.md`'s 2× and Luna's 35%/97% on this board, and gives the curve that §1's tension
is actually about. This is the measurement that tells us which row of §1's table we are choosing
between.

### M4 — ⭐ the one that matters: a single animated band
Rasterise **and** blit one lane column per 0027's `v6-asym` geometry (36 px cells, three lanes as
columns), as one contiguous window. This is the number the animation budget is spent on.
**Pass: ≤ 66 ms. Comfortable: ≤ 30 ms.**

### M5 — the PSRAM control
Rasterise the battlefield **into a PSRAM full-frame buffer** with ordinary `embedded-graphics`
per-pixel writes, then blit. Octal PSRAM at 80 MHz has bandwidth, but `embedded-graphics` does
scattered per-pixel writes and PSRAM has latency that internal SRAM does not. This is the number
that says whether the PSRAM path is actually the cheap alternative it looks like on paper.

## 4. What result changes the recommendation

Stated in advance, so the measurement cannot be read to agree with the conclusion already written:

| result | consequence |
|---|---|
| **M4 ≤ 66 ms** at a band count whose buffer fits (≥ 8 bands) | **Bands win. PSRAM question closed.** Recommendation stands as written. |
| M4 ≤ 66 ms but only at ≤ 4 bands (38 KB+) | Bands still win on speed, but 38 KB of DRAM for one app is a real cost — re-open PSRAM, and check it against the stack floor before committing. |
| **M4 > 66 ms at every band count** | Bands cannot animate at 15 fps. Either a PSRAM frame with partial blits, or 0010's animation budget is renegotiated. **This is the result that would overturn §3 of the survey**, and it is why this is written as provisional. |
| M2 overdraw > ~4× **and** M5 shows PSRAM rasterise is cheap | The PSRAM frame becomes the simpler design at similar cost. Prefer it — fewer moving parts beats a marginal win. |
| M1 ≫ 27 ms | Something about this board's SPI differs from the cited measurement. **Stop and re-derive**; do not adjust the design around an unexplained number. |
| M3 shows window count barely matters | Surprising, and it would contradict three independent sources. Suspect the measurement before believing it — then use many small bands and take the SRAM saving. |

## 5. How to run it, and the part that is genuinely dangerous

### The harness already exists — do not build a new one
`targets/s3-cyd/spike` is the Phase-1 bring-up spike for **node 162, the dev board**. It already
brings the ILI9341V up in `Rgb565` via mipidsi, has a touch driver, and is its own Cargo workspace
root. Every measurement above is a `bench` subcommand added to it. No new firmware architecture, no
partition change, no OTA, nothing that touches the fleet image.

Build on familiar, per standing directive: `./build-remote.sh` (it rsyncs to `~/builds/spike-scry`'s
sibling, never the Syncthing-mirrored tree, and keeps `TMPDIR` off familiar's 512 MB tmpfs).

### ⛔ Flash node 162, via `spike/flash.sh`, and never `spike-scry/flash.sh`

This is the part to read twice.

- `targets/s3-cyd/spike/flash.sh` is armed byte-exact for **`14:C1:9F:D1:C8:10` — node 162, the dev
  board.** That is the correct target.
- `targets/s3-cyd/spike-scry/flash.sh` is armed for **`14:C1:9F:D1:CC:64` — node 163, the scry
  station**, which is a **live capability** (witnessed 2026-09-01; JP taps cards on it). Flashing a
  bench harness there takes a working thing out of service for a measurement. It is also explicitly
  not what that guard is for: it deny-lists 162 as "the spike's lane, not ours".
- The sealed **reliquary** unit is `14:C1:9F:D1:C3:C8` — same model, **first four octets identical
  to node 162**, and both contain "C8". The guard's own header is emphatic and correct: the
  comparison is byte-exact and **must never be widened to a prefix**, because a prefix on this bench
  matches the sealed board. If the guard says no port reports the serial, the answer is to find out
  why — never to loosen the match.

**One asymmetry found while writing this, worth fixing separately:** `spike-scry/flash.sh`
deny-lists node 162, but `spike/flash.sh` does **not** deny-list node 163. Safety still holds —
both guards resolve by a byte-exact *allow* list, so the deny list is belt-and-braces — but the
protection is one-directional where it reads as mutual. Not urgent, not part of this measurement,
and not something to change while arming a flash.

### Restore path
Node 162 is a dev board, not a deployed one, so the blast radius is its own firmware. Before
flashing: record what it is running (boot splash version name + `BUILD_NUMBER` off the serial).
After: reflash that image. Note that `flash.sh` erases otadata (`0xf000`, `0x2000`) on every run by
design, so the board always boots what was just written — and the proof of a good flash is the
`Loaded app from offset 0x20000` line, not the "completed" banner.

### Cost
One flash, then ~2 minutes of serial output. No network, no vault access, no partition table
change. It is reversible in one more flash.

## 6. The ask, in three lines

> May I borrow the s3-cyd dev board (**node 162**, `14:C1:9F:D1:C8:10`) for about five minutes?
> One USB flash of the existing `targets/s3-cyd/spike` harness with a `bench` subcommand, which
> prints five timing numbers and is undone by reflashing what it had. It answers whether the
> Tapstone shrine can render in bands out of internal SRAM or needs a 150 KB PSRAM framebuffer —
> currently decided on three lines of reasoning and no measurement.
>
> Not the scry station (node 163) and not anything sealed.

## 7. What this design does not cover

- **Touch latency and reader responsiveness under a render load.** Real, and issue 8's, not this.
- **Whether Luna's renderer is portable enough to be the M2/M4 subject.** It is host-bound today
  (`SimulatorDisplay<Rgb565>`, `String`); her genericisation task is what makes it usable here. If
  the measurement is wanted before that lands, M2/M4 can use a stand-in that draws 0027's
  geometry with the same primitive mix — a weaker but honest substitute, and it must be labelled
  as one.
- **Power and thermals** at a sustained 15 fps. Worth knowing eventually; irrelevant to the seam.
- **Any claim about fps as a property of the board.** These are five numbers about one draw path.
