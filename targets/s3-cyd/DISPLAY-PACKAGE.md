# S3 display package — how smol's UI reaches a 320×240 colour panel

**Survey + recommendation for smol#398 phase 2. Nebula, 2026-08-24.**
Read-only research: no code written, no commits, nothing edited outside `targets/s3-cyd/`.
Every claim below is cited to a file:line I read. The honesty ledger is §6.

> **Naming caveat, carried from `scratch/s3-cyd-target/explore-cyd-c5.md`:** the board this
> directory is named for is unconfirmed. The S3 hardware in JP's world is the **LCDWIKI ES3C28P**
> (Ember, emberburrito, reliquary — three units), which is **ILI9341V + capacitive I²C touch**,
> *not* an ST7789/XPT2046 CYD. This document assumes the ES3C28P. If a true S3 CYD arrives, §2b's
> transfer analysis changes and §4's recommendation does not.

---

## TL;DR

smol's UI is **1-bit, 72×40, and not resolution-independent** — 171 `BinaryColor` sites across
20 files, with `72` and `40` appearing as bare magic numbers in layout arithmetic. But the display
**seam is already a proven extension point**: `app::Oled` is a type alias with **three** concrete
backends today, and adding a fourth is an established move (#152 and #26 each did it).

**Recommendation: a 4th `app::Oled` backend — a `BinaryColor→Rgb565` scaling adapter over a
`PixBuf`.** It puts smol's real menu and clock on the glass at 4× integer scale (288×160 letterboxed
in 320×240), touches **zero** of the 171 `BinaryColor` sites, and blocks on **nothing** — not #347,
not smol-core extraction. The full-fidelity native-320×240 path is a different project: it is a new
UI, not a new backend, and the watch repo has already written down why (§2a).

---

## 1. What smol's UI layer actually is today

### The seam: one type alias, three backends

`rust/clock/src/app.rs` defines exactly one concrete display type, selected by cargo feature:

| feature | `app::Oled` resolves to | cite |
|---|---|---|
| `hw`, not `cast` | `Ssd1306<I2CInterface<I2c<'static, Blocking>>, DisplaySize72x40, BufferedGraphicsMode<…>>` | `app.rs:34-38` |
| `hostsim` (#152) | `crate::hostsim::CanvasOled` — a `[u8; 72*40]` canvas framebuffer | `app.rs:44` |
| `cast` (#26) | `crate::net::cast_oled::CastOled` — a tee-wrapper | `app.rs:53` |

**This is the single most important fact in this survey.** A second display backend is not a new
architecture; it is the third repetition of an existing one. `app.rs:39-43` describes the hostsim
case in exactly the terms an S3 backend would need:

> under `feature = "hostsim"` the one concrete `Oled` becomes a canvas-backed 72×40 framebuffer
> that impls the SAME `DrawTarget<Color = BinaryColor>` + inherent `clear()`/`flush()`/`init()`
> the plugins already call — so `snake.rs` / `clock.rs` draw through it UNCHANGED (zero forked
> render code, the #152 gate).

### What a backend must implement — the full contract

1. `embedded_graphics::DrawTarget<Color = BinaryColor>` + `OriginDimensions` (`lib.rs:156-176`)
2. inherent `init() -> Result<…>` (`lib.rs:138`)
3. inherent `flush()` — may be a no-op (`lib.rs:110-113`: *"`flush()` is a no-op (the host reads
   the buffer every frame)"*)
4. inherent `clear(BinaryColor)` — the `DrawTarget` provided method suffices

That is the whole surface. It is **not** a trait smol defines; it is structural conformance to
what the plugins call. `embedded-graphics` is pinned `=0.8.2` and, unlike `ssd1306`, is
**always-on** — `Cargo.toml:95-98`:

> `ssd1306` is the concrete panel driver — HAL-facing (I2C), so it rides `hw`.
> `embedded-graphics` is pure (target-agnostic) and stays ALWAYS-on.

**So the portable half of smol's UI is already isolated behind a target-agnostic crate.** That is
the pre-existing work an S3 package builds on.

### Direct draw, no framebuffer of smol's own

There is no smol-owned framebuffer or compositor. Screens draw straight at the target and flush:

```rust
ctx.display.clear(BinaryColor::Off).ok();      // menu.rs:226
…
ctx.display.flush().ok();                       // menu.rs:228
```

Glyphs and layout come entirely from `embedded-graphics`' built-in mono fonts — **three of them,
fleet-wide**: `FONT_5X8` (52 uses), `FONT_6X10` (29), `FONT_10X20` (16). No custom glyph system,
no text shaper, no layout engine. Layout is hand-computed pixel arithmetic.

### Two draw-site shapes (this matters for portability)

- **Generic helpers** — portable over the target, pinned to the colour type:
  ```rust
  pub(crate) fn draw_clock<D>(…) where D: DrawTarget<Color = BinaryColor>   // clock.rs:76-83
  ```
- **Concrete sites** — bound to the alias:
  ```rust
  fn draw(&self, display: &mut crate::app::Oled, mask: u16)                 // menu.rs:116
  ```

Both work with a new backend, because both ultimately resolve through `app::Oled`.

### ⚠️ Where it is *not* portable — the honest part

- **`BinaryColor` is pervasive: 171 occurrences across 20 files** (`familiar/mod.rs` 28,
  `mesh_snake/mod.rs` 15, `ota_screen.rs` 13, `finder.rs` 13, `bench.rs` 10, `menu.rs` 9,
  `snake.rs` 8, …). It is in type annotations, style builders, and fill styles. There is no
  colour-generic layer.
- **72 and 40 are magic numbers in layout arithmetic**, not shared constants:
  - `snake.rs:52-54` — *"4 px divides 72 evenly (18 columns)"*, grid hardcoded 18×10
  - `batt.rs:61` — *"Screen width in FONT_5X8 glyphs: 72 px / 6 px-advance = 12 chars"*
  - `batt.rs:306` — `let x = ((72 - w) / 2).max(1);`
  - `custom.rs:12,110` — align *"within the 72 px row"*
  - `finder.rs:39`, `bench.rs:55`, `grid.rs:63`, `rssi.rs:135` — same pattern
  - The only **named** `WIDTH`/`HEIGHT` constants (`lib.rs:115,117`) live **inside the `hostsim`
    module** and are not referenced by firmware code.

**Consequence, stated plainly:** smol's UI cannot be *reflowed* to 320×240 by changing a constant.
Nothing reads a constant. A native-resolution port is a rewrite of every screen's layout — which
is precisely the conclusion the watch repo reached independently (§2a).

---

## 2. Precedents, compared honestly

### (a) esp32c6-watch — Slint as its own package

**Topology** (`esp32c6-watch/Cargo.toml`):
- Workspace `members = [".", "crates/*"]` with **`exclude = ["crates/i-slint-renderer-software"]`**
  (`:1-5`). The vendored Slint renderer fork is a **`[patch.crates-io]` source, not a member**
  (`:302-303`) — comment: *"keeps `cargo test --workspace` (host crates) untouched."*
- `slint = "1.17.1"` (`:281`), `slint-build = "1.17.1"` (`:289`), `embedded-graphics = "0.8.2"`
  (`:241`).
- UI is **`.slint` files compiled by `build.rs`** (`build.rs:38-40`,
  `EmbedResourcesKind::EmbedForSoftwareRenderer`); 10 `.slint` files under `ui/slint/`.

**Why the fork exists** (`Cargo.toml:298-301`): one local patch — even-grid dirty-region alignment
for the CO5300's CASET/RASET window restriction (#18), because Slint 1.17 has no LVGL-rounder hook.

**🔑 The watch is ALREADY doing a multi-board port — to a 320×240 CYD panel.** This is the closest
precedent that exists, and it is live:

- `Cargo.toml:44-56` defines `board-waveshare-c6` **and `board-cyd-c5`** features.
- `src/board/{mod.rs, waveshare_c6.rs, cyd_c5.rs}` — a board-selection seam with an XOR
  `compile_error!` (`board/mod.rs:24-26`).
- `build.rs:13-14` selects `ui/cyd/shell.slint` (320×240 landscape) vs `ui/slint/shell.slint`
  (410×502 portrait).

**The architectural model it documents is directly applicable** (`Cargo.toml:37-43`):

> A board feature carries (a) the chip feature for every esp-* dep, and (b) the CAPABILITY features
> for hardware that board actually has. Capabilities gate code (`#[cfg(feature = "has-pmu")]`),
> boards select capabilities — **the OpenWrt model smol's budget.rs documents: predicate on a
> declared capability, never on a chip name.**

**The artifact worth stealing outright: `src/drivers/panel.rs`** — a normative `PanelDriver` /
`TouchDriver` contract, written 2026-08-24, extracted *"from the two pixel paths that actually
exist, not invented"* (`:3`). Surface: `init`, `set_addr_window(x,y,w,h)`, `begin_pixels`,
`push_pixels(&[u16])`, `end_pixels`, `fill_screen(Rgb565)`. Three decisions in it are hard-won:

- **`&[u16]` not `&[u8]`**, byte order is the driver's problem — *"Panel byte order is a per-panel
  electrical fact… A `&[u8]` contract would have forced every caller to know every panel's byte
  order, which is the seam leaking"* (`:76-88`). It records the counter-measurement honestly: on
  the ST7789 a `&[u8]` path is *cheaper*, and names the condition to reopen the trade.
- **`end_pixels` is required, not defaulted** — a found bug in the contract's first draft. On a
  **shared** bus its omission is two silent failures: the next command clocks into the still-open
  RAMWR stream and lands in GRAM as pixels, and a following touch read asserts touch CS while LCD
  CS is still low — two devices driving one MISO (`:89-105`).
- **Brightness/power stay OUT of the trait** — the CO5300 does brightness by command, the CYD by a
  backlight GPIO the display driver does not own (`:41-47`).

**⛔ And its cost, which the file states itself** (`panel.rs:49-56`):

> The entire Slint layout is absolute-positioned for 410x502 PORTRAIT (`Theme.safe-side`, every
> `y:`, the hit-test rectangles in main.rs). The CYD is 320x240 LANDSCAPE, and the software renderer
> does not reflow. A working CYD build needs its own layout set (or a deliberately reduced shell);
> **satisfying these traits gets pixels on glass, not the watch UI.**

**Status check — the port is scaffolded, not finished:** `build.rs:29` references
`ui/cyd/shell.slint`, but **`ui/cyd/` does not exist** on disk. `src/board/cyd_c5.rs` is 70 lines.
Do not cite this precedent as "done".

### (b) cyd-c5's `watch-port` — what transfers to an ES3C28P

`cyd-c5/watch-port/src/drivers/` is a hand-rolled `SharedSpiBus` + `St7789Display`, deliberately
name- and signature-compatible with the watch's `QspiBus`/`Co5300Display` so the watch's
`TwoLineFlusher` compiles *"with a type swap and nothing else"* (`spi_bus.rs:5-20`).

**What transfers to the S3 — honestly, almost none of the code:**

| watch-port concern | ES3C28P reality | transfers? |
|---|---|---|
| hand-rolled ST7789 driver | **`mipidsi` crate**, `ILI9341Rgb565` model | ❌ replaced by a crate |
| `SharedSpiBus` (3 CS lines, per-device reconfig) | display SPI is **dedicated**; touch is **I²C** | ❌ hazard does not exist |
| XPT2046 shared-bus + `end_pixels` CS discipline | separate I²C bus — no CS interleave | ❌ moot |
| per-device clock reconfig (20 MHz vs 2.5 MHz) | one rate, 40 MHz | ❌ moot |
| ST7789 has **no** even-alignment rule | ILI9341 also has none | ✅ conclusion carries |
| **`board.rs` citation discipline** | — | ✅ **the method transfers wholesale** |
| **`Rotation` enum with MADCTL as the discriminant** | ILI9341 MADCTL differs but shape identical | ✅ pattern transfers |

**The transferable asset is the method, not the artifact.** `board.rs` cites every constant to a
vendor file:line, encodes hazards at the constant (`PIN_SD_CS`: *"park it HIGH or the SD card
corrupts display transactions"*), and labels unmeasured values **PLACEHOLDER** with the measurement
procedure named. `cyd-panel-facts.md` adds explicit confidence tiers ("high-confidence-but-verify"
with the ten-second test that settles it). That discipline is exactly what an S3 board module needs.

⚠️ Also note `watch-port` **does not currently compile** — empty `lib.rs`, `mod.rs` declares a
`xpt2046` module whose file is absent, `src/bin/smoke.rs` declared in `Cargo.toml` and missing. It
is another session's live WIP: a design reference, not a template.

### (c) burrito-fw — PixBuf + `fill_contiguous`, and it is already on the S3

`emberburrito/burrito-fw/src/canvas.rs:1-8` states the problem and the fix:

> mipidsi overrides `fill_contiguous`/`fill_solid`, but its `draw_iter` is **one SPI command per
> pixel**. Filled `embedded-graphics` primitives and glyph runs drawn straight at the display would
> crawl. So everything rasterises into RAM here and goes out as a single windowed write.

```rust
pub struct PixBuf<const CAP: usize> { px: [Rgb565; CAP], w: u32, h: u32 }   // canvas.rs:13-17
impl<const CAP: usize> DrawTarget for PixBuf<CAP> { … }                      // canvas.rs:82
target.fill_contiguous(…)                                                    // canvas.rs:69
```

**`PixBuf` is itself an `embedded-graphics` `DrawTarget<Color = Rgb565>`.** That is the hinge on
which §4's recommendation turns: smol draws into an `embedded-graphics` target; `PixBuf` *is* one;
only the colour type differs.

Measured discipline (from the orchestrator's `explore-ember.md` §2, numbers not re-derived by me):
Hearth Grid 6 blits ≈ 107 KiB ≈ 21 ms · boot flame ~26 KiB ≈ 5 ms at 40 MHz, board idle >95% ·
full-screen rasterise ≈1.6 ms, wire ≈27 ms · **per-cell SPI windows measured 2× slower than a full
repaint** · band buffers must stay in **internal SRAM** (PSRAM DMA needs 32-byte alignment).

Two design notes worth carrying: capacity is a `const` parameter while `w`/`h` are runtime, so one
buffer type serves differently-sized screens without const-generic arithmetic (`canvas.rs:8-9`);
and over-capacity is **clamped, not panicked**, because *"a panic on the board is a black screen and
a reboot loop"* (`canvas.rs:21-23`).

---

## 3. What an "S3 display package" concretely contains

Mirroring #331's framing (display/touch/audio stays an Ember package the way Slint stays a watch
package), and given §1 and §2:

```
<s3 firmware crate>/                     ← its OWN [workspace]. Non-negotiable, see below.
├── Cargo.toml                           board-* feature + capability features (watch model)
├── rust-toolchain.toml                  channel = "esp"          (Xtensa fork)
├── .cargo/config.toml                   xtensa-esp32s3-none-elf; build-std = ["core","alloc"]
├── build.rs                             git stamp + sigil name (burrito-fw precedent)
├── flash.sh                             serial-pinned guard, wired as cargo `runner`
└── src/
    ├── board.rs                         ★ pin/geometry constants, each vendor-cited
    ├── drivers/
    │   ├── panel.rs                     ★ PanelDriver + TouchDriver contract (from the watch)
    │   ├── ili9341.rs                   mipidsi wrapper satisfying PanelDriver
    │   └── touch.rs                     I²C touch satisfying TouchDriver
    ├── canvas.rs                        PixBuf<CAP>: DrawTarget<Rgb565> (burrito-fw)
    └── shim.rs                          ★ BinaryColor→Rgb565 scaling adapter  ← §4
```

**Where it lives — the workspace constraint is hard and already documented.**
`burrito-fw/Cargo.toml:14-17`:

> INTENTIONALLY its own cargo workspace root. Do NOT add this crate to the repo-root workspace:
> esp-hal accepts exactly one chip feature, and cargo unifies features across workspace members,
> so a host crate and an esp32s3 crate cannot coexist.

So the S3 firmware crate **cannot** join smol's root workspace, and shared logic **must** arrive as
**path dependencies on `no_std` `*-core` crates that declare their own `[workspace]`**. burrito-fw
already does exactly this with `osk-core`, and records the trap: cargo only absorbs path
dependencies residing *inside* the workspace root's directory, so an outside crate with its own
`[workspace]` stays independent — *verified rather than assumed* (`Cargo.toml:56-66`).

The watch confirms the same shape from the other side: 16 `crates/*` members are host-testable
`no_std` logic crates, and the one target-coupled vendored crate is `exclude`d and injected via
`[patch.crates-io]`.

**Implication for smol:** whatever smol UI logic the S3 consumes must first become a path-consumable
`no_std` crate. Today it is not — it lives in `rust/clock/src/*.rs` inside the firmware binary
crate. **That is the #347/smol-core dependency, and it is real** — but §4's recommended path is
specifically chosen to *not* need it yet.

---

## 4. Recommendation

### ✅ Path A — the smallest honest path: a 4th `app::Oled` backend (RECOMMENDED)

**A `BinaryColor → Rgb565` integer-scaling adapter.** It implements
`DrawTarget<Color = BinaryColor>` + `init`/`flush`/`clear`, and writes into a `PixBuf<Rgb565>`
which blits via one windowed `fill_contiguous`.

```
smol screens (unchanged, still BinaryColor)
        ↓  DrawTarget<Color = BinaryColor>        ← the existing seam, app.rs:34/44/53
   ScaledOled  (On→fg Rgb565, Off→bg, ×N, offset) ← the only new UI code
        ↓  DrawTarget<Color = Rgb565>
   PixBuf<CAP>                                     ← burrito-fw canvas.rs
        ↓  fill_contiguous, one windowed write
   mipidsi ILI9341Rgb565 → panel
```

**Geometry:** 72×40 at **4× = 288×160**, centred in 320×240 (16 px side, 40 px top/bottom
letterbox). 5× would be 360×200 — too wide. Integer scale means nearest-neighbour with no
interpolation: crisp, and cheap enough to be irrelevant. Full-frame cost is bounded by
explore-ember's measured 320×240 rasterise ≈1.6 ms + wire ≈27 ms, and smol only repaints on flush.

**Why this is the honest recommendation:**
- **Touches zero of the 171 `BinaryColor` sites.** Every screen — menu, clock, snake, bench,
  finder, bard — renders unchanged. This is #152's exact gate (*"zero forked render code"*)
  applied a third time.
- **Blocks on nothing.** No #347 de-pin, no smol-core extraction, no crate surgery. It is a new
  `#[cfg]` arm on an alias that already has three.
- **The 72×40 magic numbers stay correct**, because the logical panel stays 72×40. The layout
  arithmetic in `snake.rs`/`batt.rs`/`custom.rs` is never wrong — it is scaled.
- It is **honest about what it is**: smol's actual UI, legible and large, not a native-resolution
  redesign. A 288×160 letterboxed image on a 320×240 panel reads as deliberate at 4×.

**Costs, stated:** ~62 % of the panel used. Three fonts at 4× (FONT_5X8 → 20×32 px) — very legible,
coarse. Colour is two values, so the panel's colour buys nothing yet. Touch is unused (smol's input
model is `Press`, `input.rs`). None of these are defects of the approach; they are the price of not
rewriting the UI, and each is independently improvable later.

**Extension that stays cheap:** because `ScaledOled` owns the mapping, `fg`/`bg` can become
per-screen colours — a colour clock, a red DIAG toast — without a single screen learning about
`Rgb565`. That is the first real use of the panel, and it costs one field.

### 🔶 Path B — full-fidelity native 320×240

A real layout for the panel: colour, touch, larger type, more rows.

**What it requires, honestly:**
1. **Colour-generic or Rgb565-native screens** — 171 `BinaryColor` sites either genericised over
   `PixelColor` or ported. Each screen's layout arithmetic re-derived against named constants that
   **do not exist today** (§1: the only `WIDTH`/`HEIGHT` are inside `hostsim`).
2. **smol-core extraction (#347)** — the S3 crate is its own workspace (§3), so every screen it
   reuses must live in a path-consumable `no_std` crate. Today they are modules of the firmware
   binary.
3. **A touch input model** — smol has none; `input.rs` is button `Press`.
4. **The layout work itself** — the watch already priced this and wrote it down:
   *"satisfying these traits gets pixels on glass, not the watch UI"* (`panel.rs:56`).

**This is a UI project, not a display-backend project.** It should be scoped and scheduled as one.

### Sequencing

**A then B, and A is not throwaway.** The `PixBuf`, the `PanelDriver`/`TouchDriver` impls,
`board.rs`, the flash guard, and the build plumbing are **100 % shared** between the two paths —
Path A exercises and proves all of them against real glass. What A adds and B eventually retires is
one adapter file. That is a cheap bridge, and it means the panel is lit and verified long before
#347 lands.

**One decision to raise before either:** §2a shows the watch has *already* built a `board-*` +
capability-feature multi-board seam, a normative `PanelDriver` contract, and a 320×240 Slint layout
slot. If the goal is "a rich touch UI on a 320×240 panel", the watch's architecture is further along
than smol's — and *"one codebase → one image per CHIP → runtime board profiles"* (cyd-c5
`PORT-SCOPING.md`, JP's directive of record) does not by itself say which codebase owns a
colour-touch screen. Worth an explicit answer before Path B is scoped.

---

## 5. Blocking questions for the orchestrator

1. **Which board is this?** (§0 caveat) ES3C28P assumed. Unconfirmed.
2. **Path A or B** — i.e. "smol's UI, legible, this week" vs "a real 320×240 UI, after #347".
3. **Does the S3 colour/touch UI belong to smol or to the watch codebase?** (§4 sequencing note.)
4. Is `smol-core` extraction (#347) already scoped enough to depend on, or is Path A's
   independence from it a feature rather than a workaround?

---

## 6. Honesty ledger

**Verified — files I opened and read:**
`rust/clock/src/app.rs` (1-90), `src/lib.rs` (100-176), `src/clock.rs`, `src/menu.rs`,
`src/net/cast_oled.rs` (1-70), `rust/clock/Cargo.toml`; the `BinaryColor` census
(`grep -rc`, 171 across 20 files) and the 72/40 magic-number sites (`snake.rs`, `batt.rs`,
`custom.rs`, `finder.rs`, `bench.rs`, `grid.rs`, `rssi.rs`); the font census (52/29/16);
`esp32c6-watch/Cargo.toml`, `build.rs`, `src/board/mod.rs`, `src/drivers/panel.rs`,
`src/ui/slint_platform.rs` (1-40), and its crate listing; `emberburrito/burrito-fw/src/canvas.rs`
+ `Cargo.toml`; `cyd-c5/watch-port/src/{board.rs, drivers/mod.rs, drivers/st7789.rs,
drivers/spi_bus.rs}` headers.

**Verified by absence:** `esp32c6-watch/ui/cyd/` does not exist though `build.rs:29` names it;
`cyd-c5/watch-port/src/drivers/xpt2046.rs` and `src/bin/smoke.rs` do not exist though both are
declared.

**Taken from the orchestrator's `explore-ember.md` §2, NOT re-derived by me:** every ES3C28P
hardware fact (ILI9341V, pins, 40 MHz, MADCTL `0x28`, INVON, BGR, GPIO45 backlight) and every
measured render number (107 KiB/21 ms, 26 KiB/5 ms, 1.6 ms/27 ms, the 2×-slower per-cell result).
I confirmed `PixBuf`/`fill_contiguous` exist in code; I did **not** re-measure anything.

**Inferred, not verified:**
- That the 4× scale factor is the right choice — arithmetic is sound (288×160 ≤ 320×240, 5×
  overflows), but nothing was rendered.
- That a `BinaryColor→Rgb565` adapter compiles cleanly against `embedded-graphics 0.8.2`'s
  `DrawTarget`. The trait shape makes it straightforward and `CanvasOled`/`CastOled` are two
  existing proofs of the pattern — but **I wrote no code and compiled nothing.**
- Full-frame timing for Path A: extrapolated from explore-ember's 320×240 numbers, not measured
  for a scaled 72×40 source.

**Not investigated:** audio; how `ota_screen.rs` behaves under a scaled target (it is one of the
heaviest `BinaryColor` users at 13 sites); whether `mesh_snake`/`familiar` (28 sites) have
timing assumptions that a slower flush would disturb — **this is the most likely place Path A
surprises someone, and it deserves a look before committing.**
