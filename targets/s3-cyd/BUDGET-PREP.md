# BUDGET-PREP — the ES3C28P's measured `ChipBudget` row, prepared

**Purpose:** so that when phase 2 arrives, landing `ESP32S3: ChipBudget` in
`rust/clock/src/budget.rs` is a fill-in-the-blanks exercise instead of a research project.
Everything here is the *recipe* and a set of *preliminary* numbers off the phase-1 spike.

**This document is a HANDOFF to the `rust/clock` lane (smol-d8).** Nothing in it edits
`rust/clock`, `tools/`, or the spike. Written for smol#398 (sixth fleet target, node id 162,
first Xtensa). Prepared 2026-08-25 by nebula-smol.

> ## ⛔ The one thing to read before using any number below
>
> **Every number in §2 is the SPIKE's footprint, not smol's.** The spike is a four-file
> bring-up ladder; the phase-2 image is the whole `rust/clock` fleet tier. They differ by
> most of a firmware.
>
> Worse, and this is the #348/#300 lesson the C3 and C6 both paid for: **a boot-only
> measurement is the anti-pattern.** `.stack` is what the linker has left over after
> `.bss`/`.data`, and it **shrinks silently rather than failing to link** — so a successful
> link has never been evidence of a runnable image. The C6 row that arrived tonight
> (`scratch/convergence/c6-budget-row-from-watch-session.md`) says the same thing from the
> other side: its declared floor of 71,680 B is *below* the empirically observed clean line
> (61 KB = 5/5 boot panics, 73 KB = 0/5), so its own `dram_headroom()` overstates real safety
> by ~1.3 KB. **The row that lands in `budget.rs` must be measured on the real phase-2 image
> under a RADIO-UP soak**, because the failure only appears once the radio associates and
> the deep call paths (WiFi burst, crown duty) actually run.
>
> Use §2 to know the *shape* and the *order of magnitude*. Do not paste it into `budget.rs`.

---

## 1. The methodology, transposed from the C6 to Xtensa

The C6 row is the template of record. It was produced by: build at a known clean commit →
`readelf -SW` for sections → `espflash save-image` for the image → declare, with provenance
and caveats attached. All four steps transpose; only the memory map changes.

### 1.1 Toolchain preconditions (Xtensa-only, and they bite)

```bash
export PATH="$HOME/.cargo/bin:$PATH"      # espflash, cargo
source ~/export-esp.sh                    # LIBCLANG_PATH + xtensa-esp-elf GCC on PATH
cd <the crate directory>                  # rust-toolchain.toml resolves BY DIRECTORY
cargo build --release                     # release is MANDATORY on this board (PSRAM init)
```

Both env lines are required in **every** shell, and `spike/rust-toolchain.toml` documents why
at length: a missing environment disguises itself as a broken toolchain in two costumes
(`linker xtensa-esp32s3-elf-gcc not found`, and an unresolvable `#![no_std]` core when cargo
silently falls back to stable). Do not reinstall espup on either.

⚠️ **Build host: katana only.** #398 records that `familiar` has no `esp` toolchain channel —
a standing exception to the build-on-familiar preference. A phase-2 measurement cannot be
offloaded the way the C3's and C6's were.

### 1.2 `readelf -SW` — the exact invocation

Either binutils works; **the host `readelf` is sufficient** and is what produced §2:

```bash
readelf -SW <ELF>                                        # /usr/bin/readelf (binutils 2.46)
~/.espressif/tools/xtensa-esp-elf/*/xtensa-esp-elf/bin/xtensa-esp32s3-elf-readelf -SW <ELF>
```

Section headers are architecture-neutral, so the generic reader is not a shortcut — it reads
the same table. (Cross-checked: both produce identical sizes for this ELF.)

> **📌 Freeze the artifact before measuring it.** Copy the ELF somewhere stable and record its
> SHA-256 first, then take *every* number off that copy:
> ```bash
> cp target/xtensa-esp32s3-none-elf/release/<bin> /tmp/frozen.elf && sha256sum /tmp/frozen.elf
> ```
> This is not ceremony. It was learned tonight — see §4's anti-lesson.

### 1.3 The S3 memory map — which sections map to which field

**This is the part that does NOT transpose from the C3, and it is worth deriving rather than
assuming.** Sources, both in-tree and citable:

| fact | file:line |
|---|---|
| `dram_seg` / `dram2_seg` origins and lengths | `spike/target/xtensa-esp32s3-none-elf/release/build/esp-hal-*/out/memory.x:24-25` |
| `.stack` runs from `_stack_end` to `ORIGIN(RWDATA)+LENGTH(RWDATA)` | `…/out/stack.x:3-15` |
| `RWDATA` is aliased to `dram_seg` (**not** dram2) | `…/out/alias.x:5` |
| `dram2_seg` holds only `.dram2_uninit` | `…/out/dram2.x:1-6` |

Resolved, from `memory.x:24-25`:

```
dram_seg  (RW) : ORIGIN = 0x3FC88000, len = ORIGIN(dram2_seg) - 0x3FC88000 = 0x53700 = 341,760 B
dram2_seg (RW) : ORIGIN = 0x3FCDB700, len = 0x3FCED710 - 0x3FCDB700       = 0x12010 =  73,744 B
```

And from `stack.x`:

```
.stack (NOLOAD) : ALIGN(4) {
    _stack_end = ABSOLUTE(.);                      /* == end of .bss */
    . = ORIGIN(RWDATA) + LENGTH(RWDATA);           /* RWDATA == dram_seg  (alias.x:5) */
    _stack_start = ABSOLUTE(.);
}
```

➡️ **`.stack` is exactly the leftover of `dram_seg`.** Same semantic as the C3's
("linked `.stack` *is* the leftover DRAM", `budget.rs`'s `free_dram_bytes` doc) and the C6's
(`_stack_start - _bss_end`). So the field mapping is:

| `ChipBudget` field | S3 source |
|---|---|
| `chip` | the literal **`"esp32s3"`** — see §3.4, it is a join key, not a label |
| `free_dram_bytes` | the linked **`.stack`** size of the baseline image (`readelf -SW`) |
| `stack_floor_bytes` | **4/3 × the measured stack high-water** under a radio-up soak — *not* derivable from an ELF (§1.5) |
| `app_slot_bytes` | the `app` partition size from the board's **own** partition CSV (§1.6) |
| `baseline_image_bytes` | `espflash save-image` app size of the baseline (§1.4) |

And for a `FeatureCost` delta (same baseline, feature on → off):
`dram_bytes` = Δ(`.bss` + `.data` + `.data.wifi`) · `flash_bytes` = Δ(`.rodata` + `.rodata.wifi` + `.text` + `.rwtext` + `.rwtext.wifi`).

#### ⚠️ Three S3-specific traps the C3 recipe does not warn about

1. **There is a SECOND DRAM region the C3 has no analogue for.** `dram2_seg` is 73,744 B and
   is **outside** `.stack`. So on the S3, "free DRAM" and "the `.stack` region" are not the
   same quantity — `.stack` under-reports total free DRAM by up to ~72 KB. Keep
   `free_dram_bytes` = `.stack` anyway, because the field's *contract* is "DRAM available to
   a predicated feature's statics plus the runtime stack", and a static that lands in
   `dram_seg` is competing for `.stack`, not for `dram2_seg`. But **do not** describe the
   number as "the S3's free DRAM" in the doc comment — it is the `dram_seg` leftover.
   *(If a future esp-hal starts placing the heap or `.bss` overflow into `dram2_seg`, this
   field's derivation changes and the row must be re-derived, not patched.)*
2. **`.rwdata_dummy` is large and is not waste.** 44,388 B at the very bottom of `dram_seg` —
   DRAM reserved to back IRAM-resident code (`.rwtext`, `.rwtext.wifi`). It comes straight out
   of what `.stack` could have been, and it **grows when the radio stack grows**. This is the
   same accounting that produced #335's "flat −8,080 B DRAM tax" on the C3: the cost of
   esp-radio 0.18 is IRAM-resident *code*, not deeper call frames. Expect it to move between
   the spike and the fleet tier.
3. **The `.wifi` sections are separate.** `.rwtext.wifi`, `.data.wifi`, `.rodata.wifi` are
   distinct section names on this stack. Any delta arithmetic that greps only `.bss`/`.data`/
   `.rodata`/`.text` **silently omits `.data.wifi`** (496 B here — small, but a silently
   omitted term is a wrong method, not a rounding error). The field mapping above names all
   of them explicitly for this reason.

### 1.4 `baseline_image_bytes` — `espflash save-image`

```bash
espflash save-image --chip esp32s3 <ELF> <out.bin>
# → "App/part. size:  446,128/4,128,768 bytes"   ... take the LEFT number
```

Verified properties (measured, §2.3):
- **Deterministic** on a frozen ELF — three consecutive runs, byte-identical output.
- **`--flash-size` does NOT change the app size.** It changes only the partition denominator
  espflash prints (`4mb`→4,128,768 · `8mb`→8,323,072 · `16mb`→16,384,000). Do not reach for
  it hoping to correct the number; it corrects the *ratio*, which nothing consumes.
- The on-disk `.bin` equals the reported app size exactly.

This matches `repro_build.sh:299-300`, which is where the C3's number comes from:
```bash
"$espflash" save-image --chip esp32c3 "$tdir/${REPRO_TARGET}/release/clock" "$out"
```
➡️ Phase 2's equivalent line is `--chip esp32s3` with `REPRO_TARGET=xtensa-esp32s3-none-elf`.
**Note that `REPRO_TARGET` is currently a hardcoded scalar** (`repro_build.sh:36`), so the
packaging path itself needs a per-chip story before an S3 image can be *published* — a
separate concern from the budget row, flagged here because it is adjacent and easy to miss.

### 1.5 `stack_floor_bytes` — the field an ELF CANNOT give you

`free_dram_bytes` and `baseline_image_bytes` are properties of a link. **`stack_floor_bytes`
is not.** The C3's is `4/3 × 55,656 B = 74,208`, where 55,656 is the highest stack high-water
ever *measured on hardware* — id5, crown duty, 10/10 byte-identical reports, both
instrument-falsification checks passed (`budget.rs:170-206`).

The measurement instrument is the **`stack-paint` tier** (#300): paint the free stack with a
sentinel, run, report the true high-water. `budget.rs`'s own doc is explicit:

> *"**Re-derive this when the peak moves**, with `--features stack-paint` under live radio;
> idle numbers are meaningless."*

**For the S3 this means a phase-2 prerequisite that does not exist yet**: `stack-paint` is a
`rust/clock` tier, so it cannot be exercised until the chip de-pin lands and `rust/clock`
builds for `xtensa-esp32s3-none-elf`. Sequence:

1. chip de-pin → `rust/clock` links for esp32s3
2. build `--features stack-paint,<canonical>` for the S3
3. run on the ES3C28P **with the radio up and associated**, under the heaviest duty the board
   will really see, long enough to be believable (the C3's best evidence is 10/10 byte-identical)
4. `stack_floor_bytes = ceil(4/3 × peak)`
5. sanity-bracket it like the watch session did — find the region size at which boot actually
   panics, and if the floor sits below that line, **say so in the doc comment** rather than
   trusting the formula

⚠️ **Do not seed this field from the spike.** A four-file bring-up ladder's high-water says
nothing about a fleet image's, and a placeholder that looks like a measurement is precisely the
failure `budget.rs` names: *"a guessed budget is worse than an absent one."*

### 1.6 `app_slot_bytes` — a documented GAP

**The spike has no partition table at all.** Verified: no `.csv` in `spike/`, and
`spike/.cargo/config.toml`'s runner is `./flash.sh` with no `--partition-table` argument
(contrast the C3, whose runner passes `--partition-table partitions-ota.csv`).

So the `4,128,768` espflash printed is **espflash's built-in default single-app table for an
assumed 4 MB flash — it is not this board's geometry and must not be used.** The ES3C28P is
**N16R8: 16 MB flash** (`BOARD.md`). Three things must happen before this field has a value:

1. **An A/B OTA partition CSV for a 16 MB S3 must exist.** The C3's `partitions-ota.csv` is a
   zero-waste 4 MB layout (`ota_0`/`ota_1` = `0x1F0000` each) and **does not transfer** —
   #388 says the same thing for the C5 (*"Do not reuse the C3's `partitions-ota.csv`"*).
2. **Read the real geometry off the board, do not assume it.** The C5's was read out of its own
   flash. Precedent to copy: the C6's row cites `partitions.csv: ota_0/ota_1 are 0x600000 each`.
3. ⚠️ **Bootloader offset differs by chip.** C3 is `0x0`; C5/C6 are `0x2000`. **The S3's must be
   confirmed, not inferred from either** — this doc deliberately does not state a value.

⚠️ **A ROM-ceiling hazard inherited from the C6, worth checking early.** The C6 row's comment
records that the watch's `build.rs` (`widen_rom_region`, its #67) *rewrites esp-hal's generated
`memory.x` from a hardcoded 4 MiB to 6 MiB* — **without that hook the image does not LINK.**
The S3's generated `memory.x` declares `irom_seg`/`drom_seg` as `32M - 0x20`, so the S3 looks
unaffected. That is a **read, not a proof** — an S3 fleet image is far smaller than 32 MB, so
the ceiling should not bind, but confirm at first full-tier link rather than discovering it as
a mystery link error.

---

## 2. PRELIMINARY numbers — the phase-1 spike, 2026-08-25

### 2.1 Provenance of these numbers

| | |
|---|---|
| artifact | `targets/s3-cyd/spike/target/xtensa-esp32s3-none-elf/release/s3-cyd-spike` |
| frozen copy SHA-256 | `aac9f4d9e57cb98efce76dc2386363fd200044fef7e3fe5dca4d53c9c64cee22` |
| ELF size | 5,299,160 B · source mtime 2026-08-24 23:46:27 −0700 |
| **feature tier** | **`--features radio` (M3)** — inferred from the presence of `.rwtext.wifi` / `.data.wifi` / `.rodata.wifi` and `smoltcp`+`s3_cyd_spike::radio_dev` symbols in `.symtab`. **The fattest tier the spike has**, so these are its worst case, not its M1 case. |
| stack | esp-hal 1.1.2 (lock) / esp-radio 0.18.0 / esp-rtos 0.3.0 / esp-alloc 0.10.0 |
| profile | `opt-level="s"`, `lto="fat"`, `codegen-units=1`, `debug=2` |
| host | katana, espup `esp` channel, binutils readelf 2.46, espflash 4.5.0 |

⚠️ **The spike was being actively rebuilt by another lane while this was measured.** The
frozen copy is the mitigation; see §4's anti-lesson. Re-freeze before re-quoting.

### 2.2 Sections (`readelf -SW`, frozen copy)

| section | addr | size (B) | hex | notes |
|---|---|---:|---|---|
| `.rwdata_dummy` | `0x3fc88000` | **44,388** | `0xad64` | DRAM backing IRAM-resident code — grows with the radio stack |
| `.data` | `0x3fc92d68` | 15,352 | `0x3bf8` | |
| `.data.wifi` | `0x3fc96960` | 496 | `0x1f0` | ⚠️ separate section — do not omit from deltas |
| `.bss` | `0x3fc96b50` | 76,956 | `0x12c9c` | |
| `.noinit` | `0x3fca97ec` | 0 | — | |
| **`.stack`** | `0x3fca97ec` | **204,564** | `0x31f14` | ends at `0x3fcdb700` = `dram_seg` end |
| `.rwtext` | `0x40378400` | 11,240 | `0x2be8` | IRAM |
| `.rwtext.wifi` | `0x4037afe8` | 32,124 | `0x7d7c` | IRAM |
| `.vectors` | `0x40378000` | 1,024 | `0x400` | |
| `.rodata` | `0x3c000120` | 35,540 | `0x8ad4` | flash, XIP |
| `.rodata.wifi` | `0x3c008bf4` | 28,452 | `0x6f24` | flash, XIP |
| `.text` | `0x42010020` | 321,525 | `0x4e7f5` | flash, XIP |
| `.flash.appdesc` | `0x3c000020` | 256 | `0x100` | the `esp_app_desc!()` espflash 4.5 requires |

### 2.3 Derived, with the arithmetic shown twice

```
dram_seg              0x3FC88000 .. 0x3FCDB700   = 341,760 B
  statics below stack (rwdata_dummy + data + data.wifi + bss, incl. align padding)
                      0x3FC88000 .. 0x3FCA97EC   = 137,196 B
  .stack (the leftover)                          = 204,564 B

cross-check A:  _stack_start - _stack_end = 0x3FCDB700 - 0x3FCA97EC = 204,564 ✅ == .stack size
cross-check B:  dram_seg_len - statics    = 341,760 - 137,196       = 204,564 ✅ == .stack size
cross-check C:  _stack_start == ORIGIN(dram2_seg) == 0x3FCDB700     ✅ (alias.x:5 confirmed)

dram2_seg             0x3FCDB700 .. 0x3FCED710   =  73,744 B   ← OUTSIDE .stack, .dram2_uninit only

bss + data + data.wifi                           =  92,804 B   (the FeatureCost dram axis)
rodata + rodata.wifi + text + rwtext + rwtext.wifi = 428,881 B  (the FeatureCost flash axis)

espflash save-image --chip esp32s3                = 446,128 B  (deterministic, 3/3 identical runs)
  --flash-size {4mb,8mb,16mb} → app size UNCHANGED at 446,128; only the printed denominator moves
```

### 2.4 What these preliminary numbers do and do not tell you

✅ **They tell you the shape is comfortable.** 204,564 B of `.stack` on the S3 against the
C3's 106,464 B post-#233 and the C6's 80,272 B — the S3 has roughly **2× the C3's** DRAM
leftover with a radio tier linked. The flash axis is not remotely close to binding: 446 KB of
app against a 16 MB part. **The S3 is very unlikely to be DRAM-blocked the way #233 was on the
C3.** That is a genuine, useful prior for planning.

❌ **They are not the row.** The spike links four source files; the fleet tier links
`espnow,cast,io` across all of `rust/clock` — the Bard is out (#347 Phase 0) but the mesh,
election, OTA, MQTT, discovery, plugin registry and games are all in. `.bss` will grow
substantially and `.stack` will shrink by whatever that costs. Nothing here bounds by how much.

❌ **`stack_floor_bytes` has no preliminary value at all**, by design (§1.5). It is not an
ELF property and the spike cannot produce it.

---

## 3. The one-change-lands-together diff sketch

### 3.1 `tools/build-matrix.toml` — flip one word

```diff
 [chip.esp32s3]
-# JP's next fleet target. Not yet compilable from this tree: `esp-hal` is pinned with
-# `features = ["esp32c3"]`, `.cargo/config.toml` pins `build.target`, and xtensa needs the
-# esp Rust fork rather than the pinned upstream toolchain.
+# smol#398 — the ES3C28P, node id 162. Compilable from this tree since the chip de-pin.
 target     = "xtensa-esp32s3-none-elf"
-builds     = false
+builds     = true
 ships      = false
-blocked_on = "#331 multi-target; needs the esp fork toolchain and a per-chip esp-hal feature"
```

`ships` stays `false` — OpenWrt's `DEFAULT := n`, exactly as the file's own comment instructs:
*"Buildable is not shipped."*

⚠️ **`builds = true` is a commitment that CI actually compiles it.** Verified: with the flip,
`ci-matrix` emits a real new job —
```json
{"chip":"esp32s3","tier":"fleet","target":"xtensa-esp32s3-none-elf","features":"espnow,cast,io"}
```
— 11 jobs, up from 10, and **one axis at a time** (the S3 crosses only the canonical tier, not
every tier). The file's rule against a permanently-red arm applies: *"declaring a job that
always fails is worse than declaring none."* So do not flip this until `rust/clock` genuinely
links for xtensa **on the CI runner**, which additionally needs the espup toolchain provisioned
there — today it exists on katana only (#398).

### 3.2 `rust/clock/src/budget.rs` — the const, with TODO-measured placeholders

Written to `budget.rs`'s actual field contract (`budget.rs:76-105`), in the C6 row's house
style. **Every `TODO-MEASURED` must be replaced by a number from §1's recipe run against the
phase-2 image; do not let one ship.**

```rust
/// ES3C28P (ESP32-S3 N16R8), smol node id 162 — the first Xtensa target (#398).
///
/// ⚠️ MEASURE, DO NOT ADAPT. The values below come from the phase-2 canonical-tier image
/// built for `xtensa-esp32s3-none-elf`, NOT from the phase-1 spike and NOT scaled from the
/// C3. Recipe, S3 memory map, and preliminary spike figures: `targets/s3-cyd/BUDGET-PREP.md`.
///
/// `free_dram_bytes` is the linked `.stack` region — on this chip that is the leftover of
/// `dram_seg` specifically (`memory.x:25`, aliased to RWDATA by `alias.x:5`), NOT the chip's
/// total free DRAM: `dram2_seg` (~73,744 B) sits outside `.stack` and holds only
/// `.dram2_uninit`. Same *contract* as the C3's field, different region arithmetic.
///
/// ⚠️ `stack_floor_bytes` is `4/3 x` the high-water measured by `--features stack-paint`
/// **under live radio on the ES3C28P**, per `ESP32C3_STACK_FLOOR_BYTES`'s derivation note.
/// An idle or boot-only number is meaningless here; the C6 row records its floor sitting
/// ~1.3 KB BELOW the empirically observed clean line, so bracket it on hardware and write
/// down what you found.
pub const ESP32S3: ChipBudget = ChipBudget {
    // MUST be exactly "esp32s3" — this string is the join key `tools/build_matrix.py`
    // matches against the `[chip.esp32s3]` manifest row. A descriptive value such as
    // "esp32s3-es3c28p" fails the gate in BOTH directions at once. (Verified, see
    // BUDGET-PREP.md §3.4.)
    chip: "esp32s3",
    free_dram_bytes: 0,      // TODO-MEASURED: readelf -SW <phase2 ELF> -> .stack size
    stack_floor_bytes: 0,    // TODO-MEASURED: ceil(4/3 * stack-paint high-water, radio up)
    app_slot_bytes: 0,       // TODO-MEASURED: the app partition from the S3's own OTA CSV
                             //                (does not exist yet — BUDGET-PREP.md §1.6)
    baseline_image_bytes: 0, // TODO-MEASURED: espflash save-image --chip esp32s3, app size
};
```

And the cfg ladder, which currently sends any non-riscv32 bare-metal target to a
`compile_error!` (`budget.rs:310-316`):

```diff
+#[cfg(all(target_os = "none", target_arch = "xtensa"))]
+pub const CHIP: ChipBudget = ESP32S3;
+
-#[cfg(all(target_os = "none", not(target_arch = "riscv32")))]
+#[cfg(all(target_os = "none", not(target_arch = "riscv32"), not(target_arch = "xtensa")))]
 compile_error!("no ChipBudget is declared for this bare-metal target. …");
```

⚠️ `target_arch = "xtensa"` selects **any** Xtensa ESP part (S2, S3, the original ESP32), not
the S3 specifically — the same over-broad-cfg shape that `budget.rs:294-298` calls out for
riscv32. It is correct *today* because the S3 is the only Xtensa target, and it is the minimum
change. Prefer routing it through `SELF_CHIP` if the de-pin makes that available; if not, leave
a comment saying it is a placeholder for exactly the reason the riscv32 ladder already
documents, so the next Xtensa chip meets the friction rather than inheriting the S3's numbers.

### 3.3 ⚠️ CORRECTION — the gate does **not** go red on either change alone

The brief for this document assumed a symmetric two-way trip. **It is asymmetric for the S3,
and I verified this by running the checker against modified copies rather than reading it.**

| # | change | result | checker output |
|---|---|---|---|
| A | tree as-is | ✅ exit 0 | `build matrix: 10 jobs · chips builds=esp32c3` |
| **B** | **`ChipBudget` for the S3 ONLY** (no manifest change) | **✅ exit 0 — GREEN** | *(unchanged)* |
| **C** | **`builds = true` ONLY** (no budget row) | **❌ exit 1** | `FAIL chip esp32s3: builds = true but no ChipBudget in budget.rs — a buildable chip with no declared memory budget` |
| D | both together | ✅ exit 0 | `build matrix: 11 jobs · chips builds=esp32c3,esp32s3` |

**Why B is green:** `check()` step 3 (`build_matrix.py:127-141`) runs two loops.
`manifest_chips - declared_chips` fails **only if `builds` is true**; `declared_chips -
manifest_chips` fails on a budget row with no row in the manifest at all. **The S3 already has
a manifest row** (`[chip.esp32s3]`, `builds = false`) — so a budget const for it is matched by
an existing row and nothing fires.

➡️ **Consequence for sequencing, and it is a good one:** the `ChipBudget` const can land
**first, on its own, green**, ahead of the de-pin — the same #352 precedent that let the S3's
`BoardProfile` arm exist years before the silicon. The flip to `builds = true` is the change
that must land *with* it, never before it.

*(Contrast the C5, which has **no** `[chip.esp32c5]` row at all: for that chip a budget const
alone WOULD go red on the `declared - manifest` direction. The asymmetry is a property of the
S3's already-declared row, not of the checker.)*

### 3.4 The `chip:` string is a join key — verified failure mode

Writing anything other than `"esp32s3"` fails **both** directions simultaneously. Measured, with
`chip: "esp32s3-es3c28p"` and `builds = true`:

```
FAIL chip esp32s3: builds = true but no ChipBudget in budget.rs — a buildable chip with no declared memory budget
FAIL chip esp32s3-es3c28p: has a ChipBudget in budget.rs but no manifest row
```

The const's **Rust identifier** is free (the C6 row proposes `ESP32C6_WATCH`); the `chip:`
**field value** is not. Board identity belongs in `BoardProfile`'s model string, not here.

⚠️ One more scrape hazard to respect: `budget_chips()` counts `= ChipBudget {` initialisers and
requires a `chip:` string for each, **failing closed** if it finds fewer. So the const must be a
plain initialiser — do not build it behind a macro or a helper `const fn`, or the gate goes red
without the roster having changed.

### 3.5 What is NOT in this change set

- **`REPRO_TARGET`** (`repro_build.sh:36`) is a hardcoded scalar. Needed to *publish* an S3
  image; not needed for the budget row or the CI matrix. Separate change, separate review.
- **`.cargo/config.toml`'s `build.target`** and `Cargo.toml`'s `esp-hal features=["esp32c3"]` —
  the chip de-pin itself. This document assumes it has landed.
- **`ESP32C3_STACK_FLOOR_BYTES`'s shell-parser contract** (`repro_build.sh` greps that exact
  line): an S3 floor would need its own equivalent if the packaging gate is ever to enforce it
  per-chip. Today `repro_stack_floor` knows one chip. Flagged, not designed.

---

## 4. Honesty ledger

### ✅ Verified — I ran it

| claim | how |
|---|---|
| `.stack` = 204,564 B, `.bss` = 76,956 B, and the rest of §2.2 | `readelf -SW` on the frozen copy, twice, byte-identical |
| `.stack` is the leftover of `dram_seg` only | read `stack.x:3-15` + `alias.x:5` + `memory.x:24-25`; then confirmed by **two independent arithmetic cross-checks** that agree exactly (§2.3) |
| `dram2_seg` is outside `.stack` and holds only `.dram2_uninit` | `dram2.x` read in full |
| app image = 446,128 B, deterministic | `espflash save-image` ×3 on the frozen ELF, identical output + identical on-disk size |
| `--flash-size` does not change the app size | ran all four variants; only the denominator moved |
| the spike ELF is a **radio-tier** build | `.rwtext.wifi`/`.data.wifi`/`.rodata.wifi` present; `smoltcp` + `s3_cyd_spike::radio_dev` symbols in `.symtab` |
| the spike has no partition table | `ls *.csv` empty; `flash.sh` passes no `--partition-table` |
| **B is green / C is red / D is green / E fails both ways** (§3.3, §3.4) | ran `build_matrix.py check` against modified copies in `/tmp/bm-probe` — five scenarios, exit codes and messages quoted verbatim |
| `builds = true` emits exactly one new CI job | ran `build_matrix.py ci-matrix` on the modified manifest |
| the C3's image number comes from `espflash save-image` | `repro_build.sh:299-300` |

### 🔶 Inferred — reasoned, not executed

- **That the phase-2 image will fit comfortably.** The spike's 204,564 B `.stack` is ~2× the
  C3's post-#233 106,464 B, and the flash axis is nowhere near binding. But the spike links a
  fraction of `rust/clock`, and #233's C3 failure was *−6,720 B* — a margin this reasoning
  cannot resolve. **Directional prior, not a verdict.**
- **That the S3 is unaffected by the C6's `widen_rom_region` ROM-ceiling problem.** Read from
  `memory.x` (`irom_seg`/`drom_seg` = `32M - 0x20`); not proven by a full-tier link.
- **That `target_arch = "xtensa"` is a safe cfg discriminant today.** True while the S3 is the
  only Xtensa target; structurally the same over-broad shape `budget.rs` already warns about.
- **That the ELF measured is the M3 tier** — inferred from sections and symbols, not from a
  build log I produced. I did not build it; another lane did.

### ❌ Not established — do not let these become assumptions

- **`stack_floor_bytes` for the S3.** No value, no estimate, deliberately. It needs
  `stack-paint` on hardware with the radio up, and that tier cannot be built for this chip yet.
- **`app_slot_bytes`.** No S3 OTA partition CSV exists. The `4,128,768` espflash printed is its
  built-in 4 MB default and is **wrong for a 16 MB board**.
- **The S3 bootloader offset.** C3 is `0x0`, C5/C6 are `0x2000`. I did not confirm the S3's and
  have not written one down.
- **Whether `rust/clock` links for xtensa at all.** Untested; that is the de-pin's job.

### ⚠️ Anti-lesson recorded, because it nearly contaminated this document

I measured `espflash save-image --flash-size 16mb` at **465,280 B** and, ten minutes later,
the identical command at **446,128 B** — and my first instinct was that `--flash-size` changes
the app size. **It does not.** The spike was being rebuilt by a concurrent lane while I
measured: the ELF's mtime moved from 23:43 → 23:46 *between my two commands*. The instrument
moved, not the method.

Two things came out of it and both are now in §1.2 and §2.1: **freeze the artifact and record
its SHA-256 before measuring**, and **never compare two numbers taken from a live build tree at
different moments.** This is `suspect-the-instrument-first` arriving on schedule, and the only
reason it did not land in this document as a fabricated finding is that the two numbers
disagreed loudly enough to be checked. A quieter discrepancy would have shipped.

---

## §4 — ADDENDUM 2026-08-25 ~00:15 (orchestrator, after depin PR #405 went up)

Rulings from the depin lane that supersede parts of §3 — recorded here because this
document's author was rate-limited at the time:

- **§3.2's placeholder const was DECLINED, for a reason this document should have seen:**
  `budget.rs` already hands an undeclared chip the UNMEASURED **poison row**, which
  *refuses* budget-predicated features — a TODO-placeholder const would answer
  `fits_dram` with fiction instead of refusing. §2.4's own "do not let one ship" is
  honoured by never writing them. The sequencing insight survives in better form: PR
  #405 adds a **`checks` rung** (`ships ⇒ builds ⇒ checks`, machine-enforced both
  directions), and the S3's honest status is `checks = false` + 6 catalogued errors
  (2 TSENS + 4 `Cpu0*`/`Cpu*` naming variance).
- **§3.1's diff sketch is against a stale base** — its minus-lines quote a
  `[chip.esp32s3]` comment `bd26db1` already rewrote. Rebase any use of it on PR #405.
- **has-tsens measured truth: C3 ✓ C6 ✓ C5 ✗ S3 ✗** — the superset inference was wrong
  and the compiler said so. Do not inherit capability inferences; the compiler votes.
- Both §3.5 publish-blockers (REPRO_TARGET scalar, single-chip stack-floor grep) were
  verified true and ride in PR #405's blocker list.

---

## §5 — ADDENDUM 2026-08-25 (nebula-smol): §1.6's gap is CLOSED

§1.6 left `app_slot_bytes` unvalued because no S3 partition table existed and the bootloader
offset was unconfirmed. Both are now settled in
**[`PARTITIONS.md`](PARTITIONS.md)** + **[`partitions-ota-s3.csv`](partitions-ota-s3.csv)**.

- **Bootloader offset = `0x0`, the same as the C3** — verified three independent ways
  (espflash's chip table, espflash 4.5's merged-image bytes, and this board's own boot log,
  whose `entry 0x403c8924` matches the merged header byte-for-byte). §1.6 said *"C3 is `0x0`;
  C5/C6 are `0x2000`… the S3's must be confirmed"* — the confirmation is `0x0`, **and the C6
  half of that sentence turns out to be wrong**: espflash puts the C6 at `0x0` too; the P4 is
  the only `0x2000` in its table. (Inherited from #388 in good faith; PARTITIONS.md §1.4.)
- **`app_slot_bytes = 6_291_456`** (`0x600000`). Verified: espflash parses the CSV and resolves
  `ota_0` to exactly that, versus `4,128,768` (its 4 MB default) with no table.
- **The whole low-offset block carries over unchanged**, so
  **`espflash erase-region 0xf000 0x2000` is correct on this board verbatim** — no S3 variant of
  the `smol-espflash-erase-before-reflash` lesson is needed.

⚠️ **This does NOT become a `budget.rs` const.** §4's poison-row ruling governs: a partial row
with one real field and three placeholders would answer `fits_flash` with fiction where
`UNMEASURED` currently answers an honest *no*. `app_slot_bytes` is settled **in the document**
because it is the only one of the four fields that is a *declaration* rather than a
*measurement* — the other three are properties of a phase-2 link and run that do not exist yet.
All four land together, or none do.

Also closed while there: `docs/BUILDING.md:85`'s stated unknown — *"whether v4 flashes a current
esp-hal 1.1 image"* — is **answered yes for the S3 radio tier** by tonight's M2 flash log
(espflash 4.5.0 writing an esp-hal 1.1.2 image, board booted). The C3 case and the no-radio case
are still open, and the `wifi`-gated `esp_app_desc!()` predicts the latter will be *refused*.

---

## §6 — THE LINK ATTEMPT, 2026-08-25 (nebula-smol): it does NOT link, for two independent reasons

The rung above `checks`. Ran boardless on familiar against a **pristine rsync of main `5c3a9a0`**
into `/var/tmp/ftarget/s3link-src`, provisioned by `tools/ci_provision.sh` (throwaway CI values).

> ### Verdict in one line
> **`cargo build --release` for the canonical tier FAILS — two independent blockers, neither of
> them smol's source code.** Zero `error[E…]` diagnostics were produced by either. One is a
> **one-line config omission** (verified fix below); the other is an **Xtensa LLVM backend crash
> under `lto = "fat"`** that `lto = "thin"` walks around. With both worked around, the canonical
> tier links, produces a flashable image, and its **#349 descriptor reads `chip = 3` with a valid
> checksum** — closing #398's unchecked box.

### 6.0 ⚠️ An infrastructure finding that had to be dealt with first

`~/Projects/smol` on familiar — the Syncthing mirror every offloaded cargo invocation uses —
has a **`rust/clock/.cargo/config.toml` frozen at 2026-07-20 20:31**, while its sibling
directories updated today (`05:46`). It still carries the `[env] ESP_WIFI_CONFIG_*` block that
`b2537c4` (#233) removed, and has **neither the `riscv32imac` nor the `xtensa` target section**.
There is no `.stignore` in the folder root, so the cause is upstream of the tree.

**Why this mattered before a single byte was compiled:** `cargo check` never links, so #405/#407's
evidence is unaffected and remains valid. **A `build` is the first thing that would touch those
sections** — and a C5/C6 build from that mirror would silently lose `linker = "rust-lld"` and
`-Tlinkall.x` too. Rather than edit a synced tree, everything below was built from a **pristine
copy**, the `tools/gate.sh` #363 pattern. *(Flagged, not fixed — outside this lane.)*

### 6.1 Blocker A — `.cargo/config.toml`'s xtensa arm has no linker script. **129 undefined references.**

The **default** tier (`esp32s3,hw`) compiles and reaches `ld`, which emits **129 undefined
references** and zero source errors:

```
undefined reference to `_bss_start' / `_bss_end' / `_data_start' / `_sidata' / `_init_start'
undefined reference to `_stack_start_cpu0' / `_stack_end_cpu0' / `__stack_chk_guard'
undefined reference to `_rtc_fast_bss_start' … `_rtc_slow_persistent_end'
undefined reference to `__exception' / `NMI' / `level4_interrupt' / `Software0' / `Timer0' …
undefined reference to `AES' `GPIO' `SPI2' `UART0' `WIFI_MAC' `DMA_IN_CH0' … (the S3 vector table)
```

Those are **linker-script symbols** — `stack.x` defines `_stack_start`/`__stack_chk_guard`,
`esp32s3.x` `PROVIDE`s the interrupt vector defaults. They are all missing at once because the
script is never included. Compare the arms on `main`:

```toml
[target.riscv32imc-unknown-none-elf]      [target.xtensa-esp32s3-none-elf]
rustflags = [                              rustflags = [
    "-C", "linker-flavor=ld.lld",              "-C", "force-frame-pointers",
    "-C", "link-arg=-Tlinkall.x",   ← MISSING  # (comment explains the absent `linker` pin,
    "-C", "force-frame-pointers",              #  which IS correct — xtensa has no LLD backend)
]                                          ]
```

The section's comment correctly explains why there is no `linker`/`linker-flavor` pin. **It says
nothing about `-Tlinkall.x`, and that omission looks unintentional** — every other firmware target
in the file carries it.

**✅ Fix verified in the throwaway tree** (inserting one line, `"-C", "link-arg=-Tlinkall.x",`):

| default tier | undefined refs | result |
|---|---:|---|
| as `main` configures it | **129** | link fails |
| **+ `-Tlinkall.x`** | **0** | **links — 293,192 B ELF, `Finished in 30.41s`** |

**NOT APPLIED to the repo** (lane discipline). It is a one-line change to
`rust/clock/.cargo/config.toml` and it is the whole of Blocker A.

> **Why `checks` could not have caught this**, stated because it is the interesting part:
> `cargo check` does not link. The `checks` rung is honest about its scope — the script's own
> header says *"It does NOT link… a green run here says nothing about"* the `builds` rung. This
> is that sentence being paid out: four chips check clean and the S3's link was never once
> attempted. The rung did its job; the gap was real and one rung up.

### 6.2 Blocker B — `rustc-LLVM ERROR: Incomplete scavenging after 2nd pass`, and it is `lto = "fat"`

The **canonical** tier (`espnow,cast,io`) never reaches the linker at all. The entire dependency
graph and smol's own `libclock` rlib build; codegen of the final binary then crashes:

```
rustc-LLVM ERROR: Incomplete scavenging after 2nd pass
error: could not compile `clock` (bin "clock")
```

Exactly **one** error, **zero** source diagnostics. This is the Xtensa backend's register
scavenger failing to find a spare register (LLVM's PrologEpilogInserter), the classic symptom of
an over-large stack frame in a very large function.

**It is independent of Blocker A** — verified by re-running with `-Tlinkall.x` applied: identical
crash. And it is **specific to fat LTO**:

| canonical tier | result |
|---|---|
| `lto = "fat"` (the profile `rust/clock/Cargo.toml` declares) | ❌ `Incomplete scavenging after 2nd pass` |
| **`lto = "thin"`** (`CARGO_PROFILE_RELEASE_LTO=thin`, nothing else changed) | ✅ **links — 1,610,828 B ELF, 44.31s** |

⚠️ **`lto = "thin"` is NOT a fix, and must not be adopted casually.** Two reasons, both in the
profile's own comments: esp-radio *"strongly recommends"* LTO for the timing-sensitive radio
blobs, and the **`#32 build gate`** — a per-package `opt-level = "z"` for `ed25519-compact` whose
entire premise is *"under opt=s + fat-LTO, `double_scalarmult_vartime` inlines into a single
~1 MB function"*. Change the LTO mode and that gate's measured premise no longer describes the
build. A thin-LTO image is off-contract on both axes.

**It is a diagnostic, and it is a good one:** it localises the bug to fat-LTO codegen rather than
to smol, esp-hal, or the linker, and it produced the first linked canonical-tier S3 artifact.

### 6.3 What was measured once both blockers were worked around

Two frozen ELFs (own-rule: copy + hash before measuring):

| | `default-fat.elf` | `canon-thin.elf` |
|---|---|---|
| tier | `esp32s3,hw` | `esp32s3,espnow,cast,io` |
| LTO | **fat** (on-profile) | **thin** (⚠️ OFF-PROFILE) |
| sha256[0:20] | `e48a5deb5d96f1b6639d` | `5ac68841dc421a18b1f5` |
| ELF | 293,192 B | 1,610,828 B |

| section | default-fat | canon-thin |
|---|---:|---:|
| `.rwdata_dummy` | 6,100 | 42,504 |
| `.data` | 6,844 | 24,664 |
| `.data.wifi` | 0 | 496 |
| `.bss` | 100 | 155,100 |
| **`.stack`** | **328,716** | **118,996** |
| `.rodata` | 22,512 | 114,324 |
| `.rodata.wifi` | 0 | 29,704 |
| `.text` | 66,771 | 820,045 |
| `.rwtext` + `.rwtext.wifi` | 5,076 + 0 | 9,356 + 32,124 |
| `.flash.appdesc` | **absent** | 256 |

✅ **§1.3's memory-map derivation is confirmed on a real smol image, not just the spike:** both
`.stack` sections end at exactly `0x3FCDB700` = `dram_seg`'s end. The `.stack`-is-the-leftover
rule holds.

**Preliminary DRAM read, and it is encouraging:** the canonical tier links with **118,996 B** of
`.stack` — above the C3's 74,208 B floor with ~1.6× margin, and that is *with* thin LTO's
larger code. ⚠️ **This is not a verdict.** Fat LTO will change `.bss`/`.data`, the S3 has no
measured floor of its own (§1.5), and a linked region has never been evidence of a runnable
image (#300). **It is a shape, not a number.**

### 6.4 ✅ The `esp_app_desc` prediction — TESTED, and it was right

PARTITIONS.md §5 recorded this as *"predicted, not tried"*. It is now tried:

| tier | descriptor in ELF | `espflash save-image` |
|---|---|---|
| **default (no radio)** | **absent** | ❌ **rc=1 — refused**: *"You may need to add the `esp_bootloader_esp_idf::esp_app_desc!()` macro to your application"* |
| canonical (radio) | present (256 B) | ✅ rc=0 |

`esp_app_desc!()` is `wifi`-gated in `main.rs`, so the no-radio tier emits none and espflash 4.5
hard-requires one (`docs/RELEASES.md:129-130`). **A default-tier S3 image can be built but cannot
be packaged.** Worth knowing before someone reaches for the default tier as "the simple one".

### 6.5 ✅ #398's unchecked box — the #349 descriptor, CHECKED not assumed

`espflash save-image --chip esp32s3 --flash-size 16mb --partition-table targets/s3-cyd/partitions-ota-s3.csv`
→ **1,032,112 B image, 16.40% of the 6,291,456 B slot** — the first end-to-end use of
`partitions-ota-s3.csv` with a real smol image, and espflash resolved the slot from it.

Scanning that image for the `SMLT` magic and decoding all 16 bytes, checksum recomputed with
`target.rs`'s own FNV-1a/32:

```
@0x9dd8  raw=534d4c5401030f00010000006353d769
  desc_version = 1
  chip         = 3  ->  esp32s3          ✅  #398's box
  features     = 0x000f -> WIFI|ESPNOW|IO|CAST   (exactly the canonical tier; espnow ⇒ wifi)
  compat = 1 · min_from_compat = 0 · reserved = 0
  checksum     = 0x69d75363 ; recomputed 0x69d75363  ->  VALID ✅
```

⚠️ **Caveat that keeps this honest:** the image was built with **thin LTO** (§6.2). The
descriptor's *content* is a compile-time constant and is unaffected by the LTO mode, so
`chip = 3` is a real result. The image it was read out of is not the shippable one.

**A second `SMLT` at `0x25110` is a false positive — and it corroborates the design.** Its context
is `…MenuBattGridHunt` **SMLT** `…slot…`: the `MAGIC` constant itself, sitting in `.rodata` among
smol's string literals. Its checksum recomputes to `0x1c590e69` against a stored `0x746f6c73`
(ASCII `"slot"`) → rejected. `DescScan::feed_byte`'s comment names this exact case — *"a magic
that only matched by accident (**the scanner's own immediate**, say) fails here and scanning
continues"* — and here it is, in a real image, doing precisely that. **The hypothetical in the
comment is now an observed fact.**

*(One narrow gap noticed while reading it, not a finding: after a failed 16-byte candidate the
scanner resets `n = 0` and discards those bytes, so a genuine descriptor whose magic began inside
the failed candidate's 12-byte tail would be missed. It needs two `SMLT` within 12 bytes. Noted
for completeness, not raised as a defect.)*

### 6.6 The poison row — OBSERVED, not inferred

§4 recorded the ruling; this is the behaviour.

**On the canonical tier it is inert, as designed.** `CHIP`'s only consumers are the `bard`
predicates, and the canonical tier has no `bard` — smol's entire library compiled for the S3 with
`UNMEASURED` selected, and the build died in LLVM, not in `budget.rs`.

**On a budget-predicated tier it refuses, and says the right thing first.** `--features
esp32s3,bard,espnow,cast,io` (deliberately **without** `off-fleet`) produces three
`error[E0080]`s, and the *dedicated* one fires first:

> ``error[E0080]: evaluation panicked: `bard` is budget-predicated, and THIS CHIP HAS NO MEASURED
> ChipBudget row (src/budget.rs). The verdict is refused for want of data, NOT because the feature
> is too big — do not read the DRAM/FLASH messages below as applying here, and do not shrink the
> model to satisfy a budget nobody has measured.`` — `src/budget.rs:611`

The DRAM and FLASH asserts then also fire (all-zero row ⇒ `fits_*` false for every cost), exactly
as the poison row's doc predicts — and the first message pre-emptively tells you to disregard
them. **The design works as written.** Adding `off-fleet` waives all three and the tier checks
clean (rc=0), which is the `bard` tier's declared behaviour.

🐛 **One stale sentence found inside those messages.** The DRAM assert advises: *"build it for a
chip whose row has the room (#331 — **the S3 and C6 do**)"*. The S3 has **no row at all** — the
assert three lines above literally says so. Two statements in one file contradict each other, and
the reader has just been told the first one is the authoritative one. *(Not fixed — `rust/clock`
is not my lane. `budget.rs:~636`.)*

### 6.7 The stack-floor gate on a non-C3 — what fires

`repro_stack_floor()` greps for **one hardcoded chip name**:

```awk
/^pub const ESP32C3_STACK_FLOOR_BYTES: u32 =/ { ... }
```

It fails closed when it cannot read *that* line — but on an S3 build it reads it perfectly well
and hands `repro_stack_check` **the C3's floor (74,208 B)**. The S3's linked `.stack` is
118,996 B, so the gate would print `stack: 118996 B (floor 74208 B)` and **pass**.

⚠️ **That green is meaningless**, and it is worse than a red: the number it compared against
belongs to different silicon. This is the §3.5 blocker confirmed by running it rather than reading
it — the gate does not fail closed on the *chip* axis, only on the *readability* axis. Any S3
publish path needs a per-chip floor lookup before this gate means anything.

### 6.8 Error inventory — the work order

In the #407 catalogue format that made the last round tractable. **Note the shape has changed:
zero source errors. Both items are build-system/toolchain, not smol's code.**

| # | class | tier | error | status |
|---|---|---|---|---|
| **A** | **config** | default *and* canonical | 129 × `undefined reference` (linker-script + vector-table symbols) — `.cargo/config.toml`'s xtensa arm lacks `-C link-arg=-Tlinkall.x` | **root-caused; fix verified in a throwaway tree; ONE LINE; not applied** |
| **B** | **toolchain (LLVM)** | canonical only | `rustc-LLVM ERROR: Incomplete scavenging after 2nd pass` — Xtensa register scavenger, fat-LTO codegen of `bin "clock"` | **characterised: `lto=thin` links, `lto=fat` does not. No smol-side fix known.** |

**Suggested next steps, in order of cost:**
1. Apply A (one line) and re-run — it unblocks the default tier immediately and is needed
   regardless of B.
2. For B, bisect the pressure rather than the code: try `codegen-units > 1` under fat LTO, and
   `opt-level = "z"` globally, before concluding the profile must change. If fat LTO proves
   unusable on Xtensa, that is a **`builds`-rung blocker to record in `[chip.esp32s3]`** and
   probably an upstream (`esp-rs` / LLVM Xtensa) report — the crash is in the backend, not in
   anything smol can edit.
3. Only then is `baseline_image_bytes` measurable, because it must come from an **on-profile**
   (fat-LTO) canonical image. **1,032,112 B is NOT that number** — it is thin-LTO's.

### 6.9 Honesty ledger for §6

**✅ Verified — ran it:** the two failures and their exact messages; that A's fix works (129 → 0);
that B is LTO-specific and independent of A; both ELFs' sections (frozen + hashed); the
`espflash` refusal of the descriptor-less default tier; the canonical image size against the
designed slot; the descriptor's `chip = 3` with an independently recomputed checksum; the second
`SMLT`'s origin and rejection; all three poison-row asserts and their order; that `off-fleet`
waives them; that `repro_stack_floor` hands an S3 build the C3's number; familiar's stale mirror
config (mtime + content).

**🔶 Inferred:** that "Incomplete scavenging" means a register-scavenger failure on a large stack
frame — that is the standard reading of the LLVM message, not something I proved for this build.
That the missing `-Tlinkall.x` was unintentional rather than a deliberate choice with an
unwritten reason.

**❌ Not established:** the canonical tier has **never linked on-profile**, so no number here is a
candidate `baseline_image_bytes` or `free_dram_bytes`. Nothing was flashed. Whether `codegen-units`
or `opt-level` also move B is untested. Whether the C3/C5/C6 builds are affected by the same
missing-link-arg class was not checked — their arms *do* carry `-Tlinkall.x`, so they should not
be, but I did not build them.

**⚠️ Provisioning note:** built with `ci_provision.sh` throwaway values, so `.rodata`/`.data`
carry the example literals. `tools/build-matrix.toml` already documents that the same commit
measures differently across trees for exactly this reason — a few hundred bytes, and it changes
none of the verdicts above.
