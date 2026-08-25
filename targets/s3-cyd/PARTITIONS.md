# PARTITIONS — the ES3C28P's OTA table, and the bootloader offset it rests on

Companion to [`partitions-ota-s3.csv`](partitions-ota-s3.csv) and closes the two gaps
[`BUDGET-PREP.md`](BUDGET-PREP.md) §1.6 left explicitly unvalued: **the S3 bootloader offset**
and **`app_slot_bytes`**. Written 2026-08-25 by nebula-smol for smol#398.

**Status: DESIGNED, VALIDATED AGAINST TOOLING, NOT YET FLASHED.** No board was touched. Every
offset is checked against espflash 4.5.0's own output and against a real boot log from this
board; none is checked against a board running *this* table.

---

## 1. The bootloader offset — **0x0**, verified three ways

I refused to guess this in BUDGET-PREP. Here is the answer with citations, and a correction to
a claim already circulating in this repo.

### 1.1 espflash's own chip table (source)

`espflash-3.3.0/src/targets/mod.rs:183-203` — `Esp32Params::new(boot_addr, app_addr, app_size,
chip_id, flash_freq, bootloader)`, where the constructor body hardcodes the rest:

```rust
Self { boot_addr, partition_addr: 0x8000, nvs_addr: 0x9000, nvs_size: 0x6000,
       phy_init_data_addr: 0xf000, phy_init_data_size: 0x1000, app_addr, app_size, … }
```

The per-chip `PARAMS` constants (first positional argument = `boot_addr`):

| chip | file:line | **boot_addr** | app_addr | chip_id |
|---|---|---:|---:|---:|
| ESP32 | `esp32.rs:178` | `0x1000` | — | — |
| ESP32-S2 | `esp32s2.rs:24` | `0x1000` | `0x10000` | 2 |
| **ESP32-S3** | **`esp32s3.rs:21`** | **`0x0`** | `0x10000` | **9** |
| ESP32-C3 | `esp32c3.rs:26` | `0x0` | `0x10000` | 5 |
| ESP32-C6 | `esp32c6.rs:21` | `0x0` | `0x10000` | 13 |
| ESP32-H2 | `esp32h2.rs:21` | `0x0` | `0x10000` | 16 |
| ESP32-P4 | `esp32p4.rs:21` | **`0x2000`** | `0x10000` | 18 |

⚠️ **Version caveat, stated because it is the weak link in this citation:** the installed
binary is **espflash 4.5.0**; the source I read is **3.3.0** (the newest vendored in
`~/.cargo/registry`). §1.2 and §1.3 exist to close that gap with direct 4.5.0 observations, and
they agree. I did not read 4.5.0's source.

### 1.2 espflash 4.5.0's own merged output (direct observation)

`espflash save-image --chip esp32s3 --merge --flash-size 16mb <elf> merged.bin`, then read the
image at each candidate offset:

```
0x00000:  e9 03 02 40  24 89 3c 40  ee 00 00 00  09 00 00 00
          ^^ ESP image magic 0xE9   ^^^^^^^^^^^ entry 0x403c8924   ^^ chip_id 9 = ESP32-S3
0x01000:  " 4-byte aligned\n"   <- bootloader .rodata; nothing is PLACED here
0x08000:  aa 50 01 02  00 90 00 00  00 60 00 00  6e 76 73 00
          ^^^^^ partition-table magic 0x50AA     nvs @0x9000 len 0x6000, label "nvs"
0x20000:  e9 05 02 40  54 88 37 40  ee 00 00 00  09 00 00 00
          ^^ the APP image, at ota_0                          ^^ chip_id 9
```

**The second-stage bootloader begins at offset 0x0.** Nothing occupies 0x1000 or 0x2000 as a
start address. The merged file is exactly 16,777,216 B.

### 1.3 The board itself (hardware corroboration)

From tonight's M2 flash log (`…/scratchpad/m2-flash.log`, 2026-08-25 06:48 UTC, unit
`14:c1:9f:d1:c8:10`):

```
Chip type: esp32s3 (revision v0.2)   Flash size: 16MB
entry 0x403c8924
I (27) boot: ESP-IDF v5.5.1-838-gd66ebb86d2e 2nd stage bootloader
I (43) boot.esp32s3: SPI Flash Size : 16MB
I (60) boot:  0 nvs        WiFi data    01 02 00009000 00006000
I (66) boot:  1 phy_init   RF data      01 01 0000f000 00001000
I (73) boot:  2 factory    factory app  00 00 00010000 00fa0000
I (211) boot: Loaded app from partition at offset 0x10000
```

**`entry 0x403c8924` in the boot log is byte-identical to the entry point in the merged image's
header at offset 0x0.** That is the same bootloader, and the ROM found it at 0x0. Independent
of espflash's source, on this physical board.

It also confirms two facts the CSV depends on: this unit really is **16 MB**, and espflash's
generated default table puts `nvs` at `0x9000` / `phy_init` at `0xf000` / a single `factory`
app at `0x10000` — i.e. exactly the `Esp32Params` constants, chip-independent.

### 1.4 ⚠️ Correction: #388's "C6/C5 convention" is wrong about the C6

smol#388 states: *"Bootloader at **0x2000** (C6/C5 convention), not the C3's 0x0."*

**espflash puts the C6 at `0x0`** (`esp32c6.rs:21`), the same as the C3, the H2 and the S3. The
only chip in espflash's table at `0x2000` is the **P4**. Espressif documents the C5 at `0x2000`
as well, but I could **not** verify that here — espflash 3.3.0 predates the C5 and has no
`esp32c5.rs` at all, so I have no local citation for it and am not asserting one.

**Net effect on this document: none** — the S3 is `0x0` by three independent routes. Flagged
because the C6 half of that sentence is load-bearing elsewhere, and BUDGET-PREP §1.6 repeated
it in good faith. **Not corrected in #388 by me** (outside this lane); worth a comment there.

### 1.5 ✅ Consequences that carry over from the C3, now verified rather than assumed

| C3 practice | carries to the S3? | why |
|---|---|---|
| `nvs` at `0x9000`, size `0x6000` | ✅ **yes** | chip-independent in `Esp32Params::new`; confirmed on-board in §1.3 |
| `otadata` at `0xf000`, size `0x2000` | ✅ **yes** | the offset is smol's own choice (it displaces espflash's `phy_init`), and nothing chip-specific constrains it |
| **`espflash erase-region 0xf000 0x2000`** | ✅ **VERBATIM** | the otadata-only erase. Same offsets ⇒ the incantation in `docs/BUILDING.md:82`, `docs/ota.md:313` and `docs/RELEASES.md:148` is correct on this board **unchanged** |
| `phy_init` relocated to `0x11000` | ✅ yes | it is displaced by otadata on the C3 for the same reason here |
| first app partition at `0x20000` | ✅ yes | 64 KiB alignment requirement + the 56 K gap after `phy_init` |

> **This is the single most valuable outcome of the offset question.** Had the S3 been a
> `0x2000`-bootloader part, the low offsets would have had to move, and the
> `erase-region 0xf000 0x2000` muscle memory — recorded in three docs, a memory
> (`smol-espflash-erase-before-reflash`) and the published site — would have become a
> *silently wrong* command on one board in the fleet: it would succeed, erase the wrong
> region, and the failure would surface later as a board running the old image. Because the
> offset is `0x0`, **nothing has to be re-taught.**

---

## 2. Slot sizing — 6 MiB, and why not 4 or 7.9

### 2.1 The constraint that dominates: this is a one-way destructive migration

`rust/clock/partitions-ota.csv`'s own header calls the move onto it *"a DESTRUCTIVE, one-way
migration"*, and smol never updates a partition table over OTA. So **every number here is
effectively permanent for every board flashed with it.** Under-sizing costs a fleet-wide USB
re-flash; over-sizing costs flash on a part that has flash to spare. The asymmetry is the whole
argument.

### 2.2 The three candidates, costed

Available below `0x20000` on a 16 MiB part: `0xFE0000` = 16,646,144 B.

| | ota_0 / ota_1 each | remaining for data | headroom over a realistic image |
|---|---:|---:|---|
| **A** 4 MiB | 4,194,304 | 7.875 MiB | ~0.9× the C6 watch's measured image — **does not fit it** |
| **B** ✅ **6 MiB** | **6,291,456** | **3.875 MiB** | 1.35× the C6 watch's measured image |
| **C** 7.9 MiB (zero-waste) | 8,323,072 | **0** | 1.78×, but forecloses a data partition forever |

All three are exactly zero-waste; the tail simply goes to a different owner.

### 2.3 Why **B**

**The decisive datum is a measurement, not a preference.** The only other display-carrying
member of the smol family with a *measured* image is the **esp32c6-watch: 4,668,784 B**
(`scratch/convergence/c6-budget-row-from-watch-session.md`, `readelf` + `espflash save-image` at
watch HEAD `a4a86a3`). **Option A cannot hold it.** The phase-2 S3 image is the smol fleet tier
*plus* a display/touch package on a chip with an Xtensa code-size penalty — the same shape of
image as the watch's. Choosing a slot smaller than a known sibling's shipping image, when the
correction requires re-flashing every board by hand, is not a defensible trade.

Three supporting reasons:

1. **The S3 has no 4 MiB ROM ceiling, so 6 MiB slots are actually usable here.** `budget.rs`'s
   C6 row notes smol's C6 is capped at 4 MiB *"until it carries `widen_rom_region`"* — the
   watch's `build.rs` hook that rewrites esp-hal's hardcoded 4 MiB ROM region. **Verified for
   the S3:** esp-hal's generated `memory.x:34-35` declares `irom_seg`/`drom_seg` as
   `32M - 0x20`. No hook needed, no ceiling to fight. *(This is a genuine S3 advantage over
   the C6 and it is what makes B different from wishful sizing.)*
2. **3.875 MiB of reserved data is not a token.** It is ~14× the Bard's current 285 KB
   model blob — ample for the §3 option without pretending to have designed it.
3. **C's extra headroom buys nothing real.** An image approaching 8 MiB would be an
   unshippable OTA long before it was an unflashable one (smol fetches the whole image over
   WiFi, and relays it over ESP-NOW to WiFi-less leaves). The binding limit on image size is
   transfer, not slot.

**The honest cost of B:** if a future S3 image ever exceeds 6 MiB, this table is wrong and
fixing it is a fleet-wide USB re-flash. That is the risk being accepted, and it is accepted
because 6 MiB is 1.35× the largest comparable image anyone has actually built.

### 2.4 The data partition — reserved, deliberately NOT designed

Row: `reserved, data, undefined, 0xC20000, 0x3E0000` (3,968 KiB).

**Why it exists at all rather than being left unallocated:** a partition cannot be added later
without the same destructive migration (§2.1). Leaving the tail unnamed does not keep the option
open — it *spends* it. Reserving costs nothing and is reversible in the only direction that
matters (a later table can subdivide the region; it cannot conjure one).

**Why `undefined` and not `spiffs`/`fat`/`littlefs`:** all four are available
(`esp-bootloader-esp-idf-0.5.0/src/partitions.rs:455-480`), and picking a filesystem now would
be designing it — which this task explicitly scoped out. `DataPartitionSubType::Undefined` is
the crate's own name for *"a data partition with unspecified subtype"*. It reserves the address
range and asserts nothing about the contents.

**The option, noted and not designed:** on the C3 the Bard's 285 KB model lives in `.rodata`,
*inside the app image*, executed in place from memory-mapped flash. That works, and it has a
cost that grows badly: **the model is re-transferred on every OTA.** A 16 MB S3 could carry a
much larger model, and at that size an in-image blob would make OTA impractical while a
model in a data partition would not be touched by an update at all. That is the argument for
using this region if a bigger Bard ever lands on the S3 — **and it is an argument, not a
design.** It needs a mapping strategy, a provisioning path, an integrity story, and a decision
about whether `smol/ota` grows a second channel. None of that is settled here.

---

## 3. Cross-checks against known espflash 4.5 behaviours

### 3.1 The v4-refuses-`rc.0`-images trap — does not apply, and the docs are stale about it

`docs/BUILDING.md:85` records it and already flags itself as drifted: espflash v4 refuses
esp-hal `1.0.0-rc.0` images because it wants an ESP-IDF **app descriptor**, which is why the
install line pins `^3` — but *"this tree no longer builds 1.0.0-rc.0 images"* (#233 / PR #361),
and the workstation has 4.5.0 installed. BUILDING.md's stated unknown was *"whether v4 flashes
a current esp-hal 1.1 image successfully — that needs a board, and it has not been tried."*

➡️ **That unknown is now ANSWERED, on this board, in the affirmative.** The M2 flash log (§1.3)
is espflash **4.5.0** writing an **esp-hal 1.1.2** image to the ES3C28P: *"Flashing has
completed!"*, followed by the bootloader loading it and the app printing its banner. The spike
carries `esp-bootloader-esp-idf 0.5.0` and calls `esp_app_desc!()`, so the descriptor v4 demands
is present — visible in the ELF as `.flash.appdesc` (256 B at `0x3c000020`).

⚠️ **Caveat, and it matters:** this proves v4-on-1.1 for the **S3 spike**, not for the C3 fleet
image. `docs/RELEASES.md:129-130` records the sharp edge — `esp_app_desc!()` is **`wifi`-gated**
in `main.rs`, so a **no-radio build emits no descriptor and espflash 4.5 hard-requires one**. A
default-tier S3 image would hit exactly that. *(Not fixed here — outside this lane. Worth
BUILDING.md:85 gaining the S3 half of its answer.)*

### 3.2 `erase-region` — transfers verbatim, and the reason is now checkable

Covered in §1.5. Restated because it is the operational payoff: **`espflash erase-region 0xf000
0x2000`** is correct on this board with **no change**, because `nvs` occupies `0x9000..0xf000`
and `otadata` occupies exactly `0xf000..0x11000` in the table above. The node id survives; the
slot selector is cleared. The memory `smol-espflash-erase-before-reflash` needs no S3 variant.

⚠️ The *other* half of that lesson still applies in full, and applies harder on a board with a
6 MiB slot: after any OTA the board runs from `ota_1`, a plain USB flash writes `ota_0`,
succeeds, and the board silently keeps running the old image. Read the
`Loaded app from offset` line after every flash — §1.3's log shows exactly where it appears.

### 3.3 Does `save-image --flash-size 16mb` + this CSV agree on the app slot? — **yes, exactly**

```
$ espflash save-image --chip esp32s3 --flash-size 16mb \
    --partition-table targets/s3-cyd/partitions-ota-s3.csv <elf> app.bin
App/part. size:    446,672/6,291,456 bytes, 7.10%
                           ^^^^^^^^^ = 0x600000 = the designed ota_0 size
```

espflash parses the CSV, selects `ota_0`, and resolves the slot to **6,291,456 B** — the
designed number, not a default. Contrast the **absence** of the table, which is what BUDGET-PREP
§1.6 warned about:

| invocation | slot espflash reports | what it is |
|---|---:|---|
| no `--partition-table` | 4,128,768 | espflash's built-in **4 MB-flash default** — wrong for this board |
| no table, `--flash-size 16mb` | 16,384,000 | a generated **single-`factory`** table — not an OTA layout at all |
| **with this CSV** | **6,291,456** | **the designed `ota_0`** ✅ |

Also verified: `--flash-size` does **not** change the app size, only the denominator; and the
merged image places bootloader/table/app at `0x0`/`0x8000`/`0x20000` (§1.2).

---

## 4. `app_slot_bytes` — the value, and where it may and may not go

**`app_slot_bytes = 6_291_456`** (`0x600000`), for a phase-2 image built against
`partitions-ota-s3.csv`.

### ⚠️ It does NOT go into a `budget.rs` const yet — reconciling two instructions

I was asked to *"fill BUDGET-PREP's `app_slot_bytes` TODO with the designed value."* Since that
was written, BUDGET-PREP gained its §4 addendum, which **declined the placeholder const**: an
unmeasured chip selects the **`UNMEASURED` poison row** (`budget.rs:444-450`, every field `0`),
so `fits_dram`/`fits_flash` answer *no* to everything and `chip: "unmeasured"` reads as a bug on
sight. A partial const — one real field and three zeros or TODOs — would replace an honest
refusal with **fiction that computes**. That ruling is right and it outranks the earlier phrasing.

So the TODO is filled **in the document, not in the code**:

| field | status |
|---|---|
| `chip` | `"esp32s3"` — settled (join key, BUDGET-PREP §3.4) |
| **`app_slot_bytes`** | **6,291,456 — SETTLED HERE**, pending review of this table |
| `free_dram_bytes` | still unmeasured — needs the phase-2 image |
| `stack_floor_bytes` | still unmeasured — needs `stack-paint` on hardware, radio up |
| `baseline_image_bytes` | still unmeasured — needs the phase-2 image |

**Why `app_slot_bytes` is legitimately knowable now while the other three are not:** it is the
only one of the four that is a **declaration** rather than a **measurement**. The other three are
properties of a link or a run that does not exist yet. This one is a number *we choose* and then
encode in a CSV — and once the CSV is agreed, the CSV is its source of truth. It is exactly the
#352 pattern: state the fact that is decidable now, host-checkable now, years before the
hardware path closes.

➡️ **When the row is finally written, all four fields land together, all measured or settled,
and `UNMEASURED` stops being selected for the S3 in the same commit.** Never before.

---

## 5. Honesty ledger

### ✅ Verified — I ran it or read it

| claim | evidence |
|---|---|
| S3 bootloader offset is `0x0` | espflash source table (§1.1) + 4.5.0 merged-image bytes at `0x0` (§1.2) + boot-log `entry 0x403c8924` matching that header byte-for-byte (§1.3) — **three independent routes** |
| `nvs`/`phy_init`/`partition_addr` are chip-independent constants | `espflash-3.3.0/src/targets/mod.rs:183-203` constructor body, and the on-board table in §1.3 |
| espflash C6 boot_addr is `0x0`, not `0x2000` | `esp32c6.rs:21`; P4 is the only `0x2000` in the table |
| this unit is 16 MB | espflash detection **and** the 2nd-stage bootloader banner, independently |
| the CSV parses and resolves `ota_0` = 6,291,456 B | ran `save-image --partition-table` against the committed file (§3.3) |
| bootloader/table/app land at `0x0`/`0x8000`/`0x20000` | `xxd` on the merged image: `0xE9` magic + chip_id 9, `0x50AA` table magic (§1.2) |
| all offsets aligned (app 64 K, data 4 K) and 16 MiB exactly accounted | computed; merged image is 16,777,216 B |
| the S3 has no 4 MiB ROM ceiling | esp-hal generated `memory.x:34-35` — `irom_seg`/`drom_seg` = `32M - 0x20` |
| espflash 4.5 flashed an esp-hal 1.1.2 image successfully | the M2 log — closes BUILDING.md:85's stated unknown, **for the S3 radio tier only** |
| the C6 watch's measured image is 4,668,784 B | `scratch/convergence/c6-budget-row-from-watch-session.md` |
| four available data subtypes | `esp-bootloader-esp-idf-0.5.0/src/partitions.rs:455-480` |

### 🔶 Inferred — reasoned, not executed

- **That a phase-2 S3 image will land between the C3's ~1.15 MB and the watch's 4.67 MB.** The
  slot sizing rests on this. It is an argument from similarity (display package + Xtensa code
  size), not a measurement, and it is the assumption most worth attacking before this table is
  flashed to anything.
- **That `reserved` is better than an unallocated tail.** Rests on partition tables not being
  OTA-updatable in smol — true today, and true by design, but it is a design fact I read rather
  than a law of the chip.

### ❌ Not established

- **No board has been flashed with this table.** Everything in §1.2/§3.3 is espflash's offline
  behaviour. The first flash is the real test, and per §3.2 it should be read with the
  `Loaded app from offset` line in hand.
- **The ESP32-C5's bootloader offset.** Espressif documents `0x2000`; espflash 3.3.0 has no
  `esp32c5.rs`, so I have no local citation and assert nothing. §1.4 corrects only the **C6**
  half of #388's sentence.
- **espflash 4.5.0's source.** Read 3.3.0 and corroborated 4.5.0 by observation, not by source.
- **Whether a no-radio (default-tier) S3 image flashes under espflash 4.5.** §3.1 predicts it
  will be *refused* for a missing app descriptor, because `esp_app_desc!()` is `wifi`-gated.
  Predicted from `docs/RELEASES.md:129-130`, **not tried.**

### ⚠️ The measurement that keeps moving — a standing caution

The spike's app size across this session, all `espflash save-image --chip esp32s3` on frozen
copies, as the depin lane iterated:

| sha256[0:16] | app size |
|---|---:|
| `aac9f4d9e57cb98e` | 446,128 B |
| `a6b5a926e9bccc96` | 465,968 B |
| `017300987117624a` | 446,672 B |

**`--flash-size` never changed any of these** — the ELF did. This is BUDGET-PREP's anti-lesson
recurring exactly as written: freeze the artifact, record the hash, and never compare two
numbers taken from a live build tree at different moments. **None of these is
`baseline_image_bytes`** — that field wants the phase-2 fleet image, and the spike is not it.
