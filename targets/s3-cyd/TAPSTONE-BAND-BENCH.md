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

### Update 2026-09-22: the RENDERER half of that tension is gone

Luna established that the renderer is **band-invariant**, and proved it rather than built it —
byte-identical against a one-pass render at **2, 4, 7, 8, 9, 12, 16 and 24 bands**, across every
screen, layout, battlefield state and flourish frame. Seven and nine are the load-bearing ones:
they do not divide 240, so the last strip is short. A renderer that only worked for divisors would
have silently limited band choice to factors of 240, and nothing would have revealed it until a
memory budget pointed at seven.

**So band height is now a pure memory decision**, and the table above is the whole of it. The two
figures also reproduce from arithmetic in `band.rs` rather than being quoted between repositories,
with "a full frame does not fit" as a compile-time assertion — so revising the free-DRAM figure
upward past a frame stops the build instead of quietly passing. Same shape as the `Game` ceiling
in #543.

### ⚠️ And an invariant the firmware seam must not break

The reason band-invariance holds is narrow and one careless edit from being lost: **every entry
point positions from panel coordinates and never asks the target where it is.** There is no
`bounding_box()`, `size()` or `dimensions()` anywhere in the drawing code. A single one of those
would make each band draw its own copy of whatever was positioned from it — and **each band would
still look plausible in isolation**, so the bug would survive review and look like a rendering
quirk rather than a coordinate-space error.

That is why byte equality against a one-pass render is the right check and reading the diff is not.
Whoever implements the seam must keep that check running, because a firmware author adding an
innocuous helper has no reason to suspect the constraint exists.

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

**The first version of this section was a blind control, and it is worth leaving the correction
visible.** It proposed timing `Delay::delay_millis(50)` with the same `esp_hal::time::Instant` path
the other measurements use, and expecting ≈50 ms. That cannot fail for the reason the timer could
actually be wrong: the delay and the clock almost certainly derive from the **same source**, so a
misconfigured timebase scales both by the same factor and the control reads a perfect 50 ms while
every number below is wrong by that same ratio. It tests whether the timer agrees with *itself*.

That is exactly the failure Luna hit and disclosed on the renderer side — a negative control
drawing at a fixed absolute point, which the band translation corrects and clips, so it behaved
identically banded or not, and six passing invariance tests sat behind a control that could not
fail. **The generalisation worth carrying: a control must fail for the reason the mechanism admits,
not the reason that first comes to mind.** The mechanism here is not "the timer is broken" — it is
"the timer is fine and the timebase is wrong", which on this chip is a live possibility, because
`CpuClock` is configurable and a firmware/timer frequency mismatch is a real mode rather than a
hypothetical.

So two controls, both of which CAN fail:

**M0a — the physical floor, which needs no external reference.** A full 320×240 `Rgb565` frame is
`320 × 240 × 2 = 153,600 B = 1,228,800 bits`. The panel SPI is configured at **40 MHz**
(`spike-scry/src/main.rs:102`, and `BOARD.md`'s pin table agrees), so the wire alone cannot deliver
a frame in less than **30.7 ms**, before a single command byte of overhead. That floor is computed
from first principles and is *independent of the chip's own sense of time*. **A measured full-frame
blit below 30.7 ms is proof the timebase is wrong** — not that the panel is fast.

> **This control already has something to resolve.** The figure this document has been citing for
> the full-frame wire is **≈27 ms** (Nebula's, from explore-ember), which is **below the 40 MHz
> floor** and therefore not achievable as stated. One of these must be wrong: the 27 ms, the 40 MHz,
> or the assumption that it was a full 320×240 frame (the letterboxed 288×160 image is 92,160 B →
> 18.4 ms, which is not 27 either). At 80 MHz the floor would be 15.4 ms and 27 ms would be
> unremarkable. **Resolving this is M0a's first job**, and it is a good sign for the control that it
> found a discrepancy in the numbers this design was built on before being run once.

**M0b — an off-board reference, for the uniform scaling error M0a can only bound.** Print a marker
line, wait a long interval *by the board's own reckoning* (10 s), print another. The **host**
timestamps both lines. katana's clock is genuinely independent of the board's, and at 10 s the
USB-serial latency (~ms) is noise. If the board's 10 s takes 12.5 s of host time, the timebase is
off by 25% and every figure below is too.

If either control fails, stop. Nothing after it means anything — and note that this lane has now
had **four** instruments lie: a blind `nm` (unsourced `export-esp.sh`), a probe whose call the
optimiser had deleted, a lint arm that skipped its own primary subject, and a test suite grepping
colourised output. Every one was caught by a control; not one by reading.

### M1 — the wire floor, which doubles as M0a
One `fill_contiguous` of the full 320×240 from a pre-filled buffer (in PSRAM, since it must be 150
KB — this measurement is about the SPI wire, not about where the pixels came from).

**Expect ≥ 30.7 ms**, the computed floor, not the ≈27 ms this document previously cited — see
M0a. Below the floor: the timebase is wrong. Far above it (say > 45 ms): command overhead or the
SPI setup differs from the configured 40 MHz, and every other number needs re-reading before use.

### M2 — the rasterise overdraw factor
Draw 0027's real battlefield (Luna's geometry, via whatever subset of her renderer is portable at
the time) (a) once into a full-frame PSRAM buffer, and (b) clipped into N strips, for N in
{4, 8, 16}. The ratio is the **overdraw factor** — how much re-running the draw calls per band
costs. `TAPSTONE-SEAM.md` guessed 8× as an upper bound with no evidence; this replaces the guess.
Expect it to be well below N, since most draw calls are rejected cheaply by the clip rectangle.

Note this is now purely a **cost** measurement. Whether banding is *correct* at a given N is
settled — Luna's byte-equality proof covers 2 through 24 including non-divisors — so M2 cannot
change the design, only price it.

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
