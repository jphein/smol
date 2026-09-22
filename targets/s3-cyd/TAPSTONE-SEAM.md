# Where the Tapstone shrine screen lives — survey + recommendation

**Morpheus, 2026-09-22. Read-only investigation; nothing built.** Requested by the lead after
tapstone decision 0010's amendment established that the framebuffer that ruling assumed does not
exist in `rust/clock`. Every claim is cited; §7 separates what I measured from what I am citing
from what I inferred.

Companion to [`DISPLAY-PACKAGE.md`](DISPLAY-PACKAGE.md) (Nebula, 2026-08-24), which surveyed the
same glass for a different question and left one unanswered that this one forces.

---

## TL;DR

**The choice is not the two options it was framed as, and this is really Nebula's §5 question 3 —
"does the S3 colour/touch UI belong to smol or to the watch codebase?" — asked on 2026-08-24 and
never answered.** Tapstone is the first thing that cannot proceed without an answer.

There are **three** candidate homes, not two, and the one with the right pixels today is a spike:

| home | panel | colour | renders locally | registry | status |
|---|---|---|---|---|---|
| `esp32c6-watch` + `board-esp32s3-cyd` | 320×240 ✓ | ✓ | **Slint** | ✓ | ships the scry station |
| `smol/rust/clock` | via `s3_oled` | ✗ 1-bit 72×40 | ✓ | ✓ | where the rules engine + `AppKind::Tapstone` now are |
| `smol/targets/s3-cyd/spike-scry` | 320×240 ✓ | ✓ | ✓ (helpers) | ✗ | a **spike**, own workspace |

**Recommendation: keep the app in `rust/clock` and give it an app-private colour surface, gated on
a declared capability.** Lift the panel path from `spike-scry` (proven on this exact glass) rather
than inventing it. Do **not** genericise smol's 1-bit UI — Tapstone does not reuse a single one of
those screens, so the expensive half of Nebula's Path B does not apply to it.

Two consequences worth having before anyone builds: the **150 KB framebuffer is avoidable**, so the
"PSRAM registered FIRST becomes load-bearing for an app" worry does not have to be paid; and the
**renderer port is the real work item**, ~12 signatures and ~20 `format!` calls, with a free
pixel-equality oracle.

---

## 1. "The GUI flavor" names two unrelated axes, and that is why this looked simpler than it is

`PARITY.md` and tapstone's protocol draft both route the shrine to "the s3-cyd GUI flavor". That
phrase conflates two independent things:

- **radio architecture** — held-association kiosk vs burst-WiFi/ESP-NOW-first fleet. This is the
  sense `PARITY.md` uses when it says the scry station's "Flavor = GUI by radio architecture (a 5 s
  HTTP kiosk wants a held association, the fleet flavor's burst-WiFi/ESP-NOW-first design would
  churn channels)".
- **rendering stack** — Slint vs `embedded-graphics`.

They are not the same partition, and the evidence is `spike-scry`: `grep -c slint` on its
`Cargo.toml` returns **0**. It is 2,253 lines of esp-hal with `mipidsi::ILI9341Rgb565` and
`embedded-graphics` at full 320×240 `Rgb565` (`spike-scry/src/main.rs:53-55,112`), and `PARITY.md`
files it under "GUI flavor" anyway — correctly, on the radio axis.

So "put it in the GUI flavor" does not resolve to a rendering decision. Read on the rendering axis
it means the **watch codebase**, which is Slint — and tapstone decision 0010 rejected Slint for this
screen **on measurement**: 74–79 ms per page flip, ~13 fps, against the framebuffer path's 30 fps
(`PARITY.md`, 2026-08-27 debug-console suite). That measurement is not disputed by anything here.

## 2. What each candidate actually costs

### (a) `esp32c6-watch` with `board-esp32s3-cyd`

This is where the scry station's shipped firmware lives (`PARITY.md`: "Firmware WITNESSED
2026-09-01 (PR esp32c6-watch#97)"), it has `src/board/esp32s3_cyd.rs`, and it has the app registry
the protocol draft's issue 3 was pointing at. It is also the only candidate with a real touch model.

Against it:

- **Its renderer is the one 0010 rejected.** Putting an `embedded-graphics` battlefield inside a
  Slint shell means two renderers in one binary and a composition question nobody has answered.
- **Its multi-board port is scaffolded, not finished.** Nebula checked and wrote it down:
  `build.rs:29` references `ui/cyd/shell.slint` and **`ui/cyd/` does not exist on disk**;
  `src/board/cyd_c5.rs` is 70 lines. Her own words: *"Do not cite this precedent as 'done'."*
- **Its own contract file states the limit**: *"satisfying these traits gets pixels on glass, not
  the watch UI"* (`panel.rs:56`) — the Slint layout is absolute-positioned for 410×502 portrait and
  the software renderer does not reflow.
- It is a **different repository**, so the rules engine, `VENDOR.sha256`, the vendor checker and the
  gate arms just landed in smol (#543) would need a second home or a second copy.

### (b) `rust/clock` — genericise the display seam (Nebula's Path B)

Priced already, and the price is a UI project rather than a display project: **171 `BinaryColor`
sites across 20 files**, `72`/`40` as bare magic numbers in layout arithmetic, plus `smol-core`
extraction (#347) and a touch input model that does not exist (`input.rs` is button `Press`).

**But Path B was scoped to port smol's existing screens to colour, and that is not what Tapstone
needs.** Tapstone brings its own renderer, its own geometry (decision 0027 `v6-asym`) and its own
palette. It reuses none of the 171 sites. Checked against Path B's four prerequisites:

| Path B prerequisite | applies to Tapstone? |
|---|---|
| genericise 171 `BinaryColor` sites | **no** — the other screens keep `Oled` untouched |
| `smol-core` extraction (#347) | **no** — `tapstone.rs` is a module of the `clock` binary, exactly like `bard`, and #543 established the vendoring precedent for the engine |
| a touch input model | **not blocking** — 0009 makes touch the fallback, 0018 removes lane-choice touch entirely, 0027 weights it lowest. The card reader is primary |
| the layout work | **already done** — 0027 rules the geometry, and `tapstone/rust/shrine-preview` is a working renderer at exactly it |

That is the central finding of this survey: **Path B's cost was mostly the cost of bringing smol's
1-bit screens along, and Tapstone does not ask for that.**

### (c) `targets/s3-cyd/spike-scry`

Has the right pixels *today*: mipidsi + `embedded-graphics` + `Rgb565` on the ES3C28P, with draw
helpers already generic over `D: DrawTarget<Color = Rgb565>` (`main.rs:366`, `scry.rs:232,250,276`),
hardware-witnessed 2026-09-01.

Against it as a *home*: its name is accurate. It is its own Cargo workspace root by design (same
esp-hal one-chip-feature reason as `sigil-names`), it has **no app or screen registry** (grep for
`enum .*(App|Screen|Kind)|REGISTRY` returns nothing), it is not a shipping target, and its colour
content is **streamed, not rasterised** — `scry.rs` blits 153,600 B of server-rendered `Rgb565` from
scry-glass. Decision 0010's amendment rules that path out for the shrine directly, and it is right
to: the design brief puts no server at the table.

**But it is the right thing to lift *from*.** Its panel bring-up, its `fill_contiguous` windowing
and its strip-buffer discipline are exactly the parts a shrine surface needs, proven on this glass.

## 3. The recommendation

**Keep `AppKind::Tapstone` in `rust/clock`. Add an app-private colour surface beside `Oled`, not
instead of it.**

- `app::Oled` stays *exactly* as it is — `DrawTarget<Color = BinaryColor>`, 72×40, all 171 sites
  untouched, the #152 "zero forked render code" gate intact.
- `Ctx` gains a **second, optional** display handle that only Tapstone reads, gated on a declared
  **capability** rather than a chip name — the model the watch wrote down and `budget.rs` endorses:
  *"predicate on a declared capability, never on a chip name"*. Something like `has-color-panel`,
  selected by `esp32s3` the way `has-psram` already is.
- The panel driver is **lifted from `spike-scry`**, not invented.

### This resolves the C3 question rather than deferring it

An obvious objection: the C3 fleet build then has `AppKind::Tapstone` with no colour surface. That
is not a wrinkle — **it is what issue 3 asked for.** Its third acceptance criterion is *"the c3-oled
fleet flavor shows a lane counter for a `MATCH` commit stream it only observes."* One app, two
surfaces, each appropriate to its glass: the 1-bit lane counter on the C3 (which is roughly what the
provisional page in `tapstone.rs` already draws), the colour battlefield on the S3. The capability
feature is what selects between them, and no chip name appears in the app.

### The 150 KB / PSRAM consequence is avoidable — this is the part to hear before it exists

The worry raised was that a 320×240 `Rgb565` framebuffer is 150 KB, so it lands in PSRAM and
`#445/#447`'s "PSRAM registered FIRST" ordering becomes load-bearing for an app rather than only for
the GUI flavor. **It does not have to.** Two independent precedents on this exact board say
otherwise:

- `spike-scry` **never buffers a frame**, by explicit design: *"THE BLIT IS STREAMED, never
  buffered: bytes land in a small strip buffer and go straight out as `fill_contiguous` windows.
  Buffering the whole frame would cost 150 KiB and add a copy for nothing"* (`scry.rs:19-22`).
  `STRIP_ROWS` is the tuning knob.
- `BOARD.md` states the house rule for this panel: *"rasterise in internal SRAM, blit as contiguous
  windowed writes (`fill_contiguous`); per-pixel `draw_iter` ≈ one SPI command per pixel"*, with the
  measured caveat that *"per-cell SPI windows are 2× slower than a full-screen repaint"*.

So the shape is **band rendering**: rasterise a horizontal strip into a few KB of internal SRAM,
blit it, repeat. Nebula's cited figures bound it — 320×240 rasterise ≈ **1.6 ms**, wire ≈ **27 ms**
(explore-ember). Even at an 8× rasterise overdraw from re-running draw calls per band, that is
≈ 13 ms + 27 ms ≈ 40 ms ≈ **25 fps**, against 0010's ~15 fps target. And 0010's "one animated band
at a time" maps onto dirty-rect strip updates almost too neatly — the same trick `s3_oled.rs`
already uses with its 360-byte buffer and dirty rect.

A third line of evidence arrived independently while this was being written: **Luna's costing of
0027's geometry puts a lane column at ~35% of the frame budget as one contiguous window, against
~97% if the same pixels are pushed split per row.** That is the same conclusion from a third
direction — the window *count* dominates, not the pixel count — and it matches `BOARD.md`'s measured
"per-cell SPI windows are 2× slower than a full-screen repaint".

> **⚠️ PROVISIONAL. Three independent lines of reasoning agree, and not one of them is a bench
> measurement.** `spike-scry`'s no-buffer design, `BOARD.md`'s house rule and Luna's window costing
> all point at bands, but the band count and the rasterise overdraw factor are arithmetic on cited
> numbers. What the three establish is that **the PSRAM dependency is a choice rather than a
> consequence** — enough to stop designing around it, not enough to promise a frame rate. Build on
> it, and do not quote 25 fps as a property of the board.
>
> The measurement that would settle it is designed in
> [`TAPSTONE-BAND-BENCH.md`](TAPSTONE-BAND-BENCH.md), including which results would change this
> recommendation. It is **not scheduled**: it needs JP's board, and that is his call to give, not
> this lane's to take.

## 4. The work item neither seam avoids: porting Luna's renderer

`tapstone/rust/shrine-preview` is a **working** `embedded-graphics` renderer at 0027's exact
geometry, with committed reference PNGs in `tapstone/preview/`. Its own description says
*"Host-only; never ships to the device."* Measured (`grep`, this morning):

| barrier | count | fix |
|---|---|---|
| `pub type Panel = SimulatorDisplay<Rgb565>` | 1 alias, **5** references | make each draw fn generic over `D: DrawTarget<Color = Rgb565>` |
| fns taking `&mut Panel` | **12** (8 battlefield, 3 screens, 1 flourish) | signature change, mechanical |
| fns already generic over `DrawTarget` | **0** | — |
| `format!` | **20** total, ~15 in device-relevant files | a fixed-buffer `core::fmt::Write` sink |
| `String` | 5 in device-relevant files | `heapless` or fixed buffers |
| `render()` constructing and returning a `Panel` | 1 | draw into a borrowed target instead |

Total ~3,625 LOC, of which `main.rs`/`export.rs`/`cost.rs` are host tooling that never ships.

**The port has a free oracle, and this is the reason to do it before choosing anything else.**
`SimulatorDisplay` implements `DrawTarget<Color = Rgb565>`, so after genericising, the host
simulator still compiles and its PNG exports must be **byte-identical** to the ones already
committed under `tapstone/preview/`. A pixel diff proves the port changed nothing — the same
structure as #543's golden replay, where the transcript checked the parser.

So my suggested order, whichever seam wins:

1. **Genericise the renderer in tapstone, in place, proven by PNG equality.** No seam decision
   needed. Nothing device-side changes. It stops the seam choice and the port being coupled.
2. Vendor it into smol the way the engine is vendored — `tools/tapstone_vendor.sh` already
   generalises to a second crate, and the manifest/tag/file-list machinery exists.
3. Then build the surface.

## 5. What needed deciding, and what was decided

1. **Nebula's §5 Q3: does the S3 colour screen belong to smol or to the watch?** — **OPEN, with
   JP.** My recommendation is smol/`clock` and §2 is the argument, but it is a call about two
   codebases and it is not this lane's. Note that the *rendering* axis may already be settled even
   though the ownership axis is not: 0010 rejected Slint on measurement, and the watch is the Slint
   renderer.
2. **Capability feature or chip feature?** — **DECIDED: capability.** `has-color-panel`, selected by
   `esp32s3` exactly as `has-psram` already is. This applies smol's own stated rule
   (*"predicate on a declared capability, never on a chip name"*, `budget.rs`) rather than
   inventing one; being the first *app* gated that way is an extension of an existing convention,
   not a new one.
3. **Band rendering or a PSRAM frame?** — **DECIDED: bands, PROVISIONALLY**, on the three
   converging lines in §3 and with the caveat there. The settling measurement is designed in
   [`TAPSTONE-BAND-BENCH.md`](TAPSTONE-BAND-BENCH.md) and is not scheduled.

A correction belonging to §1 rather than here, recorded because the doc it affects is upstream:
0010's amendment routed the shrine to "the GUI flavor", which §1 shows is not a way to name a
rendering decision. The requirement it meant is *a locally-rasterising colour surface on this
panel*; the lead is correcting the amendment to say that.

## 6. What this survey does not claim

- That any of it is fast enough. Nothing here was run on hardware by me. The fps arithmetic in §3 is
  arithmetic.
- That `spike-scry`'s panel code lifts cleanly. I read its interfaces, not its bring-up ordering.
- That the watch option is wrong — only that it is more expensive than "it already has the pixels"
  suggests, and that it splits the engine across two repositories.

## 7. Honesty ledger

**Measured by me (2026-09-21/22, on familiar):** `grep`/`wc` counts in §4; `spike-scry` has no
registry and no Slint; the watch repo has `board-esp32s3-cyd` and `src/board/esp32s3_cyd.rs`;
`shrine-preview` has zero fns generic over `DrawTarget`; `tapstone/preview/*.png` exist.

**Cited from others, not re-measured:** 171 `BinaryColor` sites, the 1.6 ms rasterise and 27 ms wire
figures, and the `ui/cyd/` gap (all `DISPLAY-PACKAGE.md`, Nebula 2026-08-24); 74–79 ms Slint page
flip and the scry-station witness (`PARITY.md`); the 30 fps framebuffer figure (tapstone 0010);
`panel.rs:56`'s self-assessment (read, but in the watch repo, not re-derived).

**Inferred, and flagged as such:** the band-rendering fps estimate; that `spike-scry`'s panel path
lifts into `clock` without surprises; that Path B's prerequisites drop away for Tapstone (this one
I am most confident in — it follows from Tapstone reusing none of smol's screens, which is checkable
— but it is still an inference about work not yet attempted).
