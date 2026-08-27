# The capability matrix — every feature, every board

**Generated 2026-08-27 against `55c7ffe`** (main, immediately after PR #470's c6-watch subtree
refresh landed). This is the source of truth for "does board X do Y", and the README + the
website carry compact views of it.

> ## ⚠️ Verify against the tree, not against this file
>
> Every cell below was read out of the repository on the date above. A capability matrix is the
> **most rot-prone document a project can own**: a cell is cheap to write, expensive to re-check,
> and reads as current forever. So three rules apply.
>
> 1. **The tree wins.** Where this file and the source disagree, the source is right and this file
>    is a bug. Six cells below already exist because the tree contradicted a prose doc — each is
>    marked *(tree wins)* with what the doc said.
> 2. **Every non-✅ cell states its reason.** A matrix of bare icons is unmaintainable, because
>    nothing records *why* a cell is amber and therefore nothing can tell you when it stops being
>    amber. The reason is the content; the icon is the index.
> 3. **Inference is labelled as inference.** Several GUI-flavor cells are "the same source compiles
>    for this board, and nobody has watched it run there". Those are 🔶, never ✅ — smol's own rule
>    is that budgets and behaviours are *measured, never inherited* (`rust/clock/src/budget.rs`).

### Ground-truth sources actually read for this file

| Source | What it settled |
|---|---|
| `rust/clock/Cargo.toml` `[features]` | the fleet flavor's feature graph, the `has-*` silicon-capability layer, the `hal-*` API-fact layer, the per-chip arms |
| `tools/build-matrix.toml` | the `checks ⇐ builds ⇐ ships` ladder per chip, `blocked_on` reasons, tier roster, `[tier_exclusive]` module gating |
| `rust/clock/src/budget.rs` | per-chip `ChipBudget` rows, stack floors, `FloorProvenance` grades, the feature-cost consts |
| `rust/clock/src/net/profile.rs`, `net/target.rs` | per-chip Home Assistant `model` strings, the `CHIP_*` ids, the image target descriptor |
| `rust/clock/src/led.rs`, `board_s3.rs`, `s3_oled.rs` | the S3's WS2812/RMT driver and its 4×-scaled ILI9341V display backend |
| `targets/*/target.toml` | the artifact matrix — `artifact` / `gui_artifact` / `gui_board` and the written reason on every `false` |
| `targets/*/README.md`, `targets/s3-cyd/PARITY.md`, `BOARD.md` | the per-board hardware truth and the verified-done / hardware-allows / documented-exclusion framing this file's cell vocabulary comes from |
| `targets/c6-watch/Cargo.toml`, `src/board/*.rs`, `src/apps/registry.rs`, `src/net/smol_mesh.rs` | the GUI flavor's `board-*` arms, per-board `HAS_*` const contracts, app hardware-gating, and its observe-only election |
| `docs/protocol.md` | SMOLv1 frames, the universal/role/feature/chip-conditional split, the CFG key table, ELECT, the leaf mesh-OTA family |
| `docs/ota.md`, `docs/RELEASES.md`, `docs/home-assistant.md` | the OTA paths, the artifact/credential/reproducibility rules, the HA discovery + Update-entity contract |
| `.github/workflows/release-targets.yml` | that the Xtensa artifacts are built by an espup-provisioning release job, separate from the `builds` CI arm |
| `gh release view nightly` (live, 2026-08-27) | **which files are actually published** — the one cell class where the artifact list beats the docs |

---

## Reading a cell

| | meaning |
|---|---|
| ✅ | **integrated** — in the tree for this board and exercised (the reason column says how, where it is not obvious) |
| 🔶 | **partial** — the cell text says exactly what stands |
| 🛠 | **hardware supports it, firmware does not do it yet** — a task, not a limit |
| ❌ | **hardware (or the HAL) cannot** — a documented exclusion, not a gap |
| 📋 | **planned** — with its issue |
| — | **not applicable to this flavor** |

## The two flavors, and why parity is judged per flavor

Two firmwares share this tree, and a board can run either or both:

- **fleet** — `rust/clock/`, the `no_std` esp-hal binary: the app registry, SMOLv1, signed OTA,
  election, the Bard, the runtime IO registry. One image per chip; a board announces what it *is*
  at runtime (`BoardProfile` + an I²C display probe), so two boards on the same silicon run the
  same binary on purpose.
- **GUI** — `targets/c6-watch/`, a Slint/Embassy touch-UI workspace subtree'd from the
  [esp32c6-watch](https://github.com/jphein/esp32c6-watch) repo. Here a **board *is* a build**:
  panel geometry is compile-time, so a `board-*` feature supplies both the chip and that board's
  hardware capabilities.

Those are genuinely different machines with different app sets, so **every table below is
per (target, flavor)** — the framing `targets/s3-cyd/PARITY.md` established. Six columns:

| column | target | chip · triple | flavor |
|---|---|---|---|
| **c3** | [`c3`](../targets/c3/) — headless SuperMini, ~$1 | ESP32-C3 · `riscv32imc` | fleet |
| **c3-oled** | [`c3-oled`](../targets/c3-oled/) — 72×40 OLED, ~$2.76 | ESP32-C3 · `riscv32imc` | fleet (*the same image*) |
| **s3 fleet** | [`s3-cyd`](../targets/s3-cyd/) — ES3C28P 2.8″ | ESP32-S3 · `xtensa` | fleet |
| **s3 GUI** | `s3-cyd`, `board-esp32s3-cyd` | ESP32-S3 · `xtensa` | GUI |
| **c6 GUI** | [`c6-watch`](../targets/c6-watch/) — Waveshare AMOLED watch | ESP32-C6 · `riscv32imac` | GUI |
| **c5 GUI** | [`c5-cyd`](../targets/c5-cyd/) — NM-CYD-C5 2.8″ | ESP32-C5 · `riscv32imac` | GUI |

**`c5-cyd` has no fleet column** because it has no fleet image: `rust/clock` `cargo check`s clean
for the C5 but there is no measured `ChipBudget` row, so `[chip.esp32c5] builds = false` and an
unmeasured chip is handed `budget.rs`'s poison row. *"It compiles" is a real claim and a much
weaker one than "it builds."*

---

## 0 · Build posture, downloads and memory budget

| | c3 | c3-oled | s3 fleet | s3 GUI | c6 GUI | c5 GUI |
|---|---|---|---|---|---|---|
| **`cargo check` clean** (fleet source) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ — all four chips check clean since #398's `has-tsens` fallback |
| **CI `builds` a linked image** | ✅ canonical chip | ✅ same image | ❌ runner has no espup `esp` channel — the *only* remaining blocker | — | ❌ needs the watch's `widen_rom_region` hook before the 6 MB slot budget is honest | ❌ no measured budget row (#388) |
| **CI `ships`** | ✅ the only `ships = true` row | ✅ via `alias_of` | ❌ follows `builds` | — | ❌ | ❌ |
| **Published download** | ✅ `smol-c3` | 🔶 no second build — `alias_of = "c3"`, flash the c3 image | ✅ `smol-s3-cyd` — built by the espup-provisioning release job, *not* by the `builds` arm | ✅ `smol-s3-cyd-gui` | ✅ `smol-watch-c6` | ✅ `smol-c5-cyd-gui` — **a download while its fleet arm is still checks-only**; the two axes are independent keys for exactly this case |
| **`ChipBudget` row** | ✅ `ESP32C3` | ✅ shares it | ✅ `ESP32S3_CYD` (#411) | ✅ same row | ✅ `ESP32C6_WATCH` | ❌ none — this is what pins `builds = false` |
| **Stack floor** | 74,208 B | 74,208 B | 72,004 B | 72,004 B | 71,680 B | 🔶 inherits the C6's 71,680 B assert, labelled **PROVISIONAL … a stand-in, not a fact** |
| **Floor provenance** | ✅ **`Derived`** — from a measured on-hardware peak (55,656 B) with a compile-time 4/3 assertion coupling floor to peak. The strongest grade in the project | ✅ same | 🔶 **`ObservedSufficient`** — the largest region proven to run clean; no high-water number, because `stack-paint`'s sentinel is trampled by boot-era machinery on xtensa | 🔶 same | 🔶 **`BootAssert`** — a firmware contract, not a measurement, and it sits ~1,320 B *below* the empirical boot line (permissive in a known, signed direction) | ❌ none of its own |
| **Byte-reproducible image** | ✅ within one (chip, profile) pair | ✅ | ✅ (opt-level 2 is part of this chip's sha lineage) | ❌ measured non-reproducible: two cold builds of one commit differ in 654,632 bytes, **cause open** — a GUI artifact's identity is its git hash + the published file's sha256 | ❌ same | ❌ same |
| **Credentials in the published image** | placeholder, *including the mesh group key* — re-key to own your mesh (#394) | placeholder | placeholder | **absent, not placeheld** — the GUI ruling: fake placeholders were considered and rejected as worse than empty | absent | absent |

**Verified live, 2026-08-27**: the `nightly` release (2026-08-26, `ga3d7302`) actually carries all
five images — `smol-c3`, `smol-s3-cyd`, `smol-s3-cyd-gui`, `smol-watch-c6`, `smol-c5-cyd-gui` —
each with a `NOTES.md` beside it. The per-target matrix is not a plan; it is shipping.

---

## 1 · Mesh membership and fleet services

Everything in this section is `#[cfg(feature = "espnow")]` on the fleet flavor — the `default` and
`wifi` builds send no ESP-NOW frames at all. The GUI flavor carries its own SMOLv1 implementation
(`targets/c6-watch/src/net/smol_mesh.rs`) that is **wire-compatible**: same `SMOLv1 ` namespace,
same HELLO/ACK/TIME/CFG/RELAY/FAM/SNK tags, plus `PING`/`PINGACK`/`SAY` which the fleet flavor does
not speak.

| | c3 | c3-oled | s3 fleet | s3 GUI | c6 GUI | c5 GUI |
|---|---|---|---|---|---|---|
| **SMOLv1 membership** (HELLO/ACK) | ✅ canonical — the hardware-verified LED path | ✅ | ✅ leaf id 162, 36/36 acks on glass 2026-08-26 | ✅ 20/20 acks | ✅ shipped v0.2.0; mesh needs only the STA PHY, no WiFi creds | ✅ **first non-C3 silicon ever heard on smol's mesh**, peer 176, 2026-08-24 (#388) |
| **Mesh time authority** (TIME, newest-NTP-wins) | ✅ | ✅ | ✅ `[NTP] synced` | ✅ | ✅ runs its own NTP; both adopts and serves | 🔶 shared GUI source; not separately witnessed on this board |
| **Crown / gateway election** | ✅ single-gateway election validated across cascading-reboot and split-brain (#76), with handover-standoff heals (#114) | ✅ | ✅ election on ch6, glass-verified | 🔶 the GUI flavor computes and gossips an election but **does not act**: `ELECT_ENFORCE = false` | 🔶 same — observe-only by design (watch #64/#75) | 🔶 same |
| **ELECT channel-plane announce** | 🔶 **announcing is on; acting is not** — `net::election::FOLLOW_ENABLED` is off, and the flip criterion is deliberately evidence from a real fleet, not a code review (#278/#269) | 🔶 same | 🔶 same | 🔶 `ELECT_ENFORCE = false` | 🔶 same | 🔶 same |
| **Gateway: WiFi STA + NTP** | ✅ | ✅ | ✅ | ✅ | ✅ plus in-firmware roaming (multi-pass BSSID pin; no 802.11r) | 🔶 shared source; **HA deliberately dark** on this board during its diagnostics lane |
| **Gateway: MQTT → Home Assistant** | ✅ | ✅ | ✅ `[MQTT] published` | ✅ | ✅ + the `esp32c6_watch` HA custom component (v0.3.0) | 🔶 as above |
| **HA MQTT discovery entities** | ✅ retained discovery; the device registry *is* the self-reported fleet manifest. 🔶 the typed-child split is only partial — live discovery carried 3 `_voltage`, 3 `_rssi` and **zero `_soc`** (docket D11 open) | ✅ | ✅ per-chip `model` = `smol ESP32-S3 Ember` | 🔶 the GUI flavor publishes its own `watch/*` topics, not smol's `sensor.smol_<id>_*` discovery blocks | 🔶 same | 🔶 same |
| **HA Update entity + `<id>/ota/install`** | ✅ native Update entity (#33); the install topic is idempotent (`staged.build > running`) | ✅ | ✅ | 🔶 the watch's OTA is driven by its own retained MQTT announce + `tools/ota_push.sh`, not smol's Update entity | 🔶 same | 🔶 same |
| **Keyed-CFG channel** (per-node config, no reflash) | ✅ every knob over one frame: `S` screen · `L` LED · `U` units · `P` plugins · `Y` custom screen · `B` broker · `O` OTA host · `R` reboot · `W` scan · `G`/`g` IO map · `T` Bard prompt. (`N` retired by #142 — drained and ignored) | ✅ | ✅ | 🔶 the GUI flavor parses `CFG` but its settings live in its own config record (v6) — RELAY/CFG downlink parity is 📋 (watch #36) | 🔶 same | 🔶 same |
| **Custom screens** (CFG `Y`) | ✅ merged (#45) | ✅ | ✅ | ❌ **GUI-flavor boards do not render smol custom screens** — the scene set is compile-time Slint, not a byte renderer | ❌ | ❌ |
| **Runtime IO registry** (CFG `G`/`g`) | ✅ one image holds every digital driver; a relayed pin-map binds button/contact/relay/LED to any free GPIO (`0/1/3/7/10` on the C3) | ✅ | 🔶 the `io` feature is chip-agnostic and rides the fleet tier; the **free-pin table is C3 board truth** (`FREE_PINS` is per-board) | — | — | — |
| **DIAG observability record** | ✅ uptime, boot count, reset reason, boot slot, last-OTA outcome, heap, link quality, `net=`/`brk=`/`otah=`, the applied-config echo `cfg=`, bound-input counters `io=` | ✅ | ✅ | 🔶 the watch emits its own `DIAG` shape (pipe-delimited, partial records legal by protocol) | 🔶 same | 🔶 same |
| **Routed multi-hop flood** (UP2/RELAYACK2) | ✅ hop-limited managed flood, table-free; **2026-07-14 a gateway-deaf smol delivered telemetry home through a neighbour** (#13). Honest v1 limit: uplink ACK is best-effort | ✅ | 🔶 `relays` confirmed on glass; multi-hop *escalation* not separately exercised on this board | 🔶 as above | 🔶 as above | 🔶 as above |
| **Mesh Familiar** (FAM) | ✅ exactly-one-holder arbitration, migration on power loss, orphan re-election — verified on glass (#57) | ✅ | ✅ | ✅ the watch implements FAM and shows the Weasley-clock pointer | ✅ | 🔶 shared source; not witnessed here |
| **Group-MAC trailer** | 🔶 present and **observe-only** — `MAC_ENFORCE` is off | 🔶 | 🔶 | 🔶 | 🔶 | 🔶 |
| **On-demand WiFi scan** (CFG `W`) | ✅ on-demand only — never a periodic background scan that would go mesh-deaf (#71) | ✅ | ✅ | 📋 watch #1 — Radio Scan v2 | 📋 | 📋 |
| **Reserved node-id block** | 1–99 | 1–99 (shared) | 162 | 162 | 122 / 236 | 176–191 |
| **HA `model` string** | `smol ESP32-C3 SuperMini` | `smol ESP32-C3 OLED` | `smol ESP32-S3 Ember` | *(watch topics)* | `smol ESP32-C6 Watch` | `smol ESP32-C5 CYD` |

> **Single-radio precondition, and it is topology, not firmware.** A board that must stay
> WiFi-associated can only run ESP-NOW in the COEXIST arm — pinned to the AP's channel. If the AP
> is not on the mesh channel (`ESP_NOW_FIXED_CHANNEL = 6`) the member loses either its uplink or
> the mesh, and **no firmware can resolve it**. smol's default is TIME-SHARE: a WiFi burst at boot,
> then ch6, and the mesh is deaf during the burst.

---

## 2 · OTA

| | c3 | c3-oled | s3 fleet | s3 GUI | c6 GUI | c5 GUI |
|---|---|---|---|---|---|---|
| **HTTP / push self-OTA** (gateway role) | ✅ canary-one-board, role-aware self-test (a WiFi board confirms by reaching DHCP), app-side rollback | ✅ | ✅ | ✅ inherits the watch's HTTP OTA | ✅ **zero-touch, end-to-end** — pull *and* push, surviving a mid-download reconnect; the running slot comes from the **MMU, never `otadata`**, behind a range-checking flash guard (watch #55 bricked a watch before that fix) | 🔶 shared source; `tools/ota_push.sh --url` exists precisely because a per-seat announce URL can be firewall-dead from the target's VLAN |
| **Mesh-OTA receive** (leaf, over ESP-NOW) | ✅ full ~1 MB images to WiFi-less leaves; leaf self-tests by **hearing a mesh frame**, not DHCP | ✅ | ✅ **345 → 1405 over the air, 2026-08-26 — the first cross-architecture OTA in smol's history**; slot flip + `ota=confirmed:1405`, self-test passed, no rollback, ~40 s for 1 MB | 🔶 `mesh-ota` compiled (ed25519-**dalek** on xtensa — `ed25519-compact` crashes that backend) — **not proven on this flavor** | 🔶 `mesh-ota` is on for all three GUI boards and the tags are wire-compatible (OTAM/OTAD/OTAN), but **mesh-OTA is unproven on the C6 specifically** | 🔶 compiled, unproven |
| **Mesh-OTA relay / serve** (crown → one leaf) | ✅ canary-one-leaf, 231-byte chunks, 64-chunk windowed NAK; the crown suppresses its own self-OTA while a relay is in flight | ✅ | 🔶 proven as a **client** (dual-path fetch: crown relay + self); serving another board not exercised | 🔶 compiled | 🔶 compiled | 🔶 compiled |
| **Peer-sourced mesh-OTA** (an updated leaf sources the next) | 📋 the one worthwhile extension — retires the crown's WiFi-fetch window; gated on the esp-radio-0.18 coex root-cause finding (#198/#204) | 📋 | 📋 | 📋 | 📋 | 📋 |
| **Ed25519 signed-image verify** | ✅ the offline-signed manifest `M = build\|size\|sha256hex` binds `build` into the signature; **a leaf verifies before it flashes one byte**. `sha256` alone is never the trust gate | ✅ | ✅ | 🔶 signing crate compiled; not exercised on this flavor | ❌ **the watch's HTTP OTA has no signature gate** — its guards are a monotonic-build gate, an MMU-derived running slot and range-checked writes | ❌ same |
| **A/B slots + rollback self-test** | ✅ 2 × 0x1F0000; ⚠️ the bundled bootloader's revert-on-boot-fail is **OFF**, so recovery is app-side self-rollback + canary-one-board | ✅ | ✅ 2 × 6 MiB (`partitions-ota-s3.csv`), flashed and exercised | ✅ same table | ✅ 2 × 6 MB (needs `widen_rom_region`) | 🔶 same table ports unchanged (16 MB flash), unexercised here |
| **Image target descriptor** (`SMLT` suitability) | ✅ a 16-byte record in the image; six refusals, all data-vs-data — `tgt-chip`, `tgt-compat`, `tgt-featloss`, `tgt-bench`, `tgt-descver`, and `tgt-absent` which **fails closed**. Per-chip staged topic `smol/ota/staged/<chip>` | ✅ variant (OLED vs SuperMini) is deliberately *runtime*-detected and absent from the descriptor | ✅ chip byte `3` | — | — descriptor is a fleet-flavor mechanism | — |
| **Signed-freshness floor + build monotonicity** | ✅ blocks downgrade/replay | ✅ | ✅ | 🔶 | ✅ monotonic build gate | 🔶 |

> **The otadata trap, on every board.** After any OTA, a USB flash silently lands in the slot the
> bootloader will not select. Clear it first — `espflash erase-region 0xf000 0x2000` — and read the
> `Loaded app from offset` line, which names the slot that actually ran. This cost real time on the
> S3 en route to its first roll.

---

## 3 · Display, input and output

| | c3 | c3-oled | s3 fleet | s3 GUI | c6 GUI | c5 GUI |
|---|---|---|---|---|---|---|
| **Display panel** | ❌ no panel — headless by design, and the display is simply *not answered* at the boot I²C probe | ✅ SSD1306 72×40 1-bit, I²C @400 kHz, rotated 180° (the case hangs from the USB-C end) | ✅ ILI9341V 320×240 via `mipidsi` over SPI2 @40 MHz — smol's logical 72×40 screens scaled **4× lazily inside the flush**, on a 360-byte framebuffer (not the staged 92 KB design: `.stack` is whatever DRAM is left) | ✅ ILI9341V, landscape, full cyd scene set — JP on glass | ✅ CO5300 AMOLED 410×502, QSPI + DMA; two-line RGB565 strips straight to panel GRAM, so the ~202 KB framebuffer is gone from boot | ✅ ST7789 320×240, classic SPI, landscape-native — no reset GPIO (tied to SoC RESET), inversion OFF, BGR |
| **Touch** | ❌ | ❌ | ❌ the fleet flavor has **no touch driver at all** — touch is a GUI-flavor capability | ✅ FT6336U capacitive (`has-cap-touch`) — taps, swipes, wake-on-tap, JP on glass: *"the s3 touch works really well"* | ✅ FT3168 capacitive (`has-cap-touch`), ≥44 px hit areas, partial-render v2 | 🔶 XPT2046 **resistive** — no `has-cap-touch`, needs pressure threshold + calibration, and **poll-only: no IRQ line is wired**. Swipe work in flight in the watch lane |
| **Display idle-sleep + wake** | — | 🔶 not a documented feature of the fleet flavor | 🔶 | ✅ JP witnessed | ✅ AOD light-sleep | 🔶 shared source |
| **Backlight dimming** | — | — | 🛠 LEDC on GPIO45 — run as a **bare GPIO** today; the GUI's brightness slider is a threshold, not a dim (PARITY gap 6) | 🛠 same | ✅ brightness slider | 🛠 plain GPIO25, no PWM driver |
| **Buttons / physical input** | 🔶 BOOT only — that *is* the whole UI: one button drives the app registry | 🔶 same | ✅ BOOT key (`HAS_BOOT_KEY`) | ✅ | ✅ BOOT/POWER short- and long-press, user-mappable from *Settings › Buttons*; the power button always wakes first, so a press in the dark can never fire an unseen action. AXP2101 4-s hardware failsafe intact | 🔶 shared source |
| **Status LED** | ✅ onboard LED shows ESP-NOW peer state (off → blink = detected → solid = connected), settable via CFG `L` | ✅ | ✅ **WS2812 ×1 on GPIO42, GRB, driven over RMT directly** — `esp-hal-smartled` 0.17 wants esp-hal ~1.0 and is incompatible with 1.1.x, so `led.rs` encodes the frame itself *(tree wins: `PARITY.md` gap 5 still lists this as open; §"the cyd-c5 half" in the same file says DONE, and the driver is in the tree)* | 🛠 `WS2812_GPIO = 42` is declared in `board/esp32s3_cyd.rs` with **no consumer in the GUI tree** | ❌ no WS2812 on this board | 🛠 `WS2812_GPIO = 27` declared in `board/cyd_c5.rs`, **no consumer** *(tree wins: `PARITY.md` implies a C5 status light exists; only the constant does)* |
| **smol Cast** (mirror the screen to a WLED matrix as UDP pixels) | 🔶 in the fleet tier; nothing to mirror without a panel | ✅ hardware-verified (#26) | 🔶 **known bug: the Cast mirror is blank on the S3** (#398 follow-up, PARITY gap 7) | 🔶 the GUI flavor has its own opt-in `cast` feature (default off, so shipped builds stay byte-identical); pixel-correctness needs a matrix on the bench | 🔶 same | 🔶 same |
| **WLED WiZmote emit** (impersonate a linked remote) | 🔶 `wled` is a build tier, not in the canonical fleet features | 🔶 | 🔶 | 🔶 a `WLED` app tile exists in the GUI registry | 🔶 | 🔶 |

---

## 4 · Apps

The fleet flavor's app set is a **build tier, not a runtime setting** — the canonical fleet tier is
`espnow,cast,io`. The GUI flavor's is a 17-entry registry whose rows are never removed (an index is
a persistence contract) but are filtered by `hardware_present()`.

| | c3 | c3-oled | s3 fleet | s3 GUI | c6 GUI | c5 GUI |
|---|---|---|---|---|---|---|
| **One-button app registry** | ✅ BOOT-button menu, static dispatch, no heap on the base build | ✅ | ✅ | ✅ 17-app launcher + edge gestures + app switcher + shade | ✅ | 🔶 shared source; layout constants for this panel are **PLACEHOLDER** and story playback is gated off until they land |
| **Clock · Snake · Benchmark · atomic14 pack** | ✅ flashed (no glass) | ✅ | ✅ | ✅ Snake/2048/Tetris/Flappy | ✅ | 🔶 |
| **Mesh Snake** (2-board head-to-head) | ✅ | ✅ | ✅ | 🔶 mesh multiplayer games are 📋 on the GUI side (watch #36); `SNK` is mirrored | 🔶 same | 🔶 |
| **World Snake (MMO)** | ✅ shared 256×256 toroidal world, mesh leaderboard, 6 treasure-powers — running fleet-wide (#5) | ✅ | ✅ | ✅ single-player World Snake tile | ✅ | 🔶 |
| **Marauder's Watch · Treasure Hunt** (ESP-NOW roster RSSI, no BLE) | ✅ merged (#58 / #60) | ✅ | ✅ | ✅ `Hunt` tile | ✅ | 🔶 |
| **The Bard** (260K-param on-device transformer) | 🔶 **proven on glass and NOT in the fleet image.** #300/#302: 67-token story, 202 ms/tok, port proven bit-for-bit against a Python reference; endless mode 224–274 ms/tok on id8. But `bard` is budget-predicated and its 39,072 B of DRAM is **6,720 B short** of the C3's 32,352 B headroom, so `--features bard` is a *compile error* on the C3 without the `off-fleet` waiver — and `repro_build_bin` refuses to package anything naming `off-fleet` | 🔶 same | 🔶 ran on this unit under the `stack-paint` composition (which carries `off-fleet`); against the declared row it is **14,400 B short** (24,672 B headroom vs 39,072 B) | 🔶 the GUI flavor has its own opt-in `bard` (default off; the 277 KB blob lands only with the feature) | 🔶 same — and against `ESP32C6_WATCH` the Bard is **30,480 B short** (8,592 B headroom) | 🔶 same |
| **Stories / LitRPG reader** | — | — | — | 🔶 `story` is opt-in and off by default — ROM is a non-issue (2,098,526 B free) but it ships off until the app has been on glass once; ~5 KB `.bss` against 9,408 B measured headroom | 🔶 same | ❌ gated off — the C5 layout constants are placeholders |
| **TTS / read-aloud** | — | — | — | 🔶 `tts` is on by default, but the runtime default is `SpeakMode::OnDemand` — the feature being present and the watch talking are two different switches. Playback rides `has-audio-out`, whose first listen on this board is still bench-gated | ✅ `tts` on by default; measured cost 328 B of stack against ~8.6 KB of margin | ❌ no `has-audio-out` on this arm |
| **Voice push-to-talk / STT** | — | — | — | ❌ needs `has-audio-in`; the app tile is correctly absent (`AppState::Voice => cfg!(feature = "has-audio-in")`) | ✅ live ES7210 capture streamed to a LAN STT gateway; a press before the link is up latches and fires itself when WiFi lands | ❌ |
| **Sound meter / FFT spectrum** | — | — | — | 🔶 the tile is **present** (`Sound => has-audio-out`) — but a level meter wants capture, which is phase 2 here | ✅ live dB meter, waveform, 12-band log-spaced FFT, digital gain stepper | ❌ |
| **HA Batt / HA Grid pages** | 🔶 nothing to draw | ✅ on-glass round-trip verified **on the gateway** (#16/#17); 🔶 the **leaf** leg is inferred, not observed — protocol.md logs no leaf-side receipt and the fleet is all-gateway, so the path is unexercised | ✅ | ✅ `Energy` / `Climate` tiles | ✅ | 🔶 |
| **Ping** (delivery-confirmed nudge) | — fleet flavor does not speak `PING` | — | — | ✅ full-screen pulse, four-note arpeggio, timestamped shade card | ✅ | 🔶 |
| **Block Digger** (Bluetooth Stadia controller) | 🔶 the **Arduino/C++** build (Bluepad32), not the Rust fleet firmware | 🔶 | — | — | — | — |

---

## 5 · Sensors, audio, power

| | c3 | c3-oled | s3 fleet | s3 GUI | c6 GUI | c5 GUI |
|---|---|---|---|---|---|---|
| **Die temperature** (TSENS) | ✅ `has-tsens` — measured, not assumed | ✅ | ❌ **`esp_hal::tsens` is unresolved for `xtensa-esp32s3-none-elf` on esp-hal 1.1.1.** The biggest chip does not expose the sensor the smallest one does; `Reading.chip_c` is `Option<f32>` so a no-TSENS chip **omits the field rather than fabricating it** | ❌ no `has-die-temp` | ✅ `has-die-temp` | ❌ esp-hal 1.1.x exposes no TSENS on the C5 either |
| **IMU** | ❌ | ❌ | ❌ no QMI8658 on the ES3C28P. ⚠️ **`[IMU] OK` on this board is a vacuous log line** — init is ungated and `let _ =` swallows the NACK (PARITY gap 3: fix by printing per result and gating consumers) | ❌ no `has-imu`; the tilt-driven `Maze` tile is correctly absent | ✅ QMI8658 6-axis with a **hardware pedometer engine** that keeps counting while the IMU is idle; wrist-raise wake | ❌ no IMU chip |
| **Audio out** | ❌ | ❌ | 🛠 the ES8311 codec + 3 W amp are on the board and the codec already ACKs, but the fleet flavor has no audio path (PARITY gap 1, the biggest) | 🔶 **`has-audio-out` is ON as of #470** — ES8311 playback, option B (MCLK-from-pin @16 k on GPIO4, `AUDIO_BCLK_DERIVED = false`), amp on **GPIO1 ACTIVE-LOW — inverted vs the C6's GPIO6**. ⚠️ **acoustic proof pending**: no speaker on the bench connector, and the Cargo.toml comment itself says *"Bench-gated: first-listen must confirm the codec locks to SoC MCLK on GPIO4"* | ✅ `has-audio-out` — `play_pcm()` substitutes samples into an always-running silent-clock ring so the mic's clock master never stops for a beep; amp+codec powered only while a clip plays | ❌ `HAS_AUDIO = false` — the NM-CYD-C5's speaker header is **un-scoped**; flip it with real pins if the lane brings it up |
| **Audio in** (mic) | ❌ | ❌ | 🛠 an LMA2718B381 mic is on the board | 📋 **phase 2** — the S3's mic is the ES8311's own ASDOUT, a different capture topology from the C6's separate ES7210 ADC. `has-audio-in` deliberately withheld | ✅ ES7210 ADC on its own ALDO1 rail — playback and capture are **two different chips sharing one I²S clock domain**; the ES8311's own ADC is not wired to the SoC | ❌ |
| **Battery / fuel gauge** | ❌ | ❌ | 🛠 no PMU, but the board **has** a battery ADC — GPIO9 with a 2:1 divider. Needs a `has-batt-adc` arm feeding the same battery UI the PMU feeds on the C6; would kill the 0 % cosmetic honestly (PARITY gap 2) | 🛠 `HAS_BATT_ADC = true`, `BATT_ADC_GPIO = 9` are **declared as of #470 with no reader** — the only reference outside the board file is a comment warning that binding a button there samples ~2 V | ✅ AXP2101 — charger profile, power-key latch, battery monitoring, live per-subsystem current estimation | ❌ `HAS_BATT_ADC = false` — none known; USB-powered bench board |
| **PMU** (charge control, power-key latch) | ❌ | ❌ | ❌ no AXP2101 — a documented exclusion, not a gap; `has-pmu` correctly off and #448 gates the AXP sites | ❌ | ✅ AXP2101 | ❌ |
| **Light sleep / AOD** | 🔶 not a fleet-flavor feature | 🔶 | 🛠 esp-hal *does* expose `rtc_cntl` sleep on the S3 (unlike the C5) — worth enabling for AOD power, but it **needs a bench current check, not just a compile** (PARITY gap 4) | 🛠 no `has-light-sleep` | ✅ `has-light-sleep` + the RC_FAST/REF_TICK calibration machinery, with a never-panic gate after watch #43 | ❌ the C5's esp-hal 1.1.x has **no `rtc_cntl::sleep`** — a board without the capability replaces the AOD light-sleep poll with a plain timer wait: same cadence, more current, no chip-specific code |
| **PSRAM** | ❌ none on the C3 | ❌ | 🔶 `has-psram` is a **marker that links nothing** — PSRAM mode is a runtime `PsramConfig` field, not a feature. ⚠️ `esp-radio`'s heap must **never** live in PSRAM: the radio blobs use atomics that do not work against it, and the failure is not a panic, it is wrong data | ✅ 8 MB octal PSRAM, **registered first** (#445/#447) | ❌ no PSRAM — the C6 uses an RGB332 half-res framebuffer in SRAM because of it | 🔶 8 MB present; the C6's heap story does **not** port (its reclaimed-pool scarcity and 256-SceneTexture ceiling are C6 *measurements*) |

---

## 6 · Radios

| | c3 | c3-oled | s3 fleet | s3 GUI | c6 GUI | c5 GUI |
|---|---|---|---|---|---|---|
| **WiFi 2.4 GHz** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ dual-band WiFi 6 silicon; smol uses the 2.4 GHz mesh channel |
| **ESP-NOW** | ✅ the mesh transport on every tier with a radio | ✅ | ✅ | ✅ | ✅ | ✅ |
| **802.15.4** (Zigbee / Thread) | ❌ no radio | ❌ | ❌ no radio | ❌ | 🛠 native 802.15.4 on the silicon; 📋 watch #2 (single-image WiFi + Zigbee/Thread) and #1 (802.15.4-2015 frame parsing). ⚠️ **`esp-radio`'s build script hard-panics when `esp-now` and `ieee802154` are both enabled** — so on the fleet flavor `has-ieee802154` is a marker that deliberately links nothing, and enabling a C6's headline feature would break the C6. A Zigbee tier needs a mesh transport that is *not* ESP-NOW: a protocol decision, not a feature flag | 🛠 same silicon capability, same mutual exclusion. The **Zigbee-bridge role is back-burnered by JP (2026-08-25)** and is anyway a two-chip design (ESP32-H2 companion over UART), not a C5 feature (#399) |
| **BLE** | ❌ **refuted on hardware** — blocking BLE init wedges the C3 in ROM (btdm busy-wait), and `esp-radio`'s build script errors on `coex` without `ble`. smol's WiFi↔ESP-NOW coexistence is *same-radio* channel management, not cross-radio arbitration, so BLE is not pulled in (#22; revisit gated on Embassy) | ❌ | 🔶 silicon has BLE; the fleet flavor links none | 🔶 same | ✅ BLE GATT server (`trouble-host`), efuse-derived identities, sigil advertising — radios are **off at boot** and toggled from the watchface | 🔶 silicon has it; not built |
| **Bluetooth audio / LE Audio** | ❌ | ❌ | ❌ | ❌ | ❌ **permanent, forensically closed (watch #62).** The C6 has no ISO link layer: ESP-IDF gates LE Audio on `SOC_BLE_ISO_SUPPORTED` (H4 only), the shipped controller blob's `ble_ll_iso` object is 464 bytes with **zero ISO symbols**, and the upstream IDF request was closed *"Won't do"*. Unicast CIS *and* Auracast both need that layer, so **no earbud model changes this**; classic A2DP was never possible either | ❌ |

---

## Where this file corrects a prose doc

Six cells above exist because the tree contradicted a document. Recorded here so the docs get
fixed rather than the contradiction being re-discovered.

1. **S3 WS2812 status LED.** `PARITY.md` gap 5 lists it as open ("smol-native parity with C3/C5
   status light; RMT driver"), while §"the cyd-c5 half" of the *same file* says it is DONE. The tree
   settles it: `rust/clock/src/led.rs` carries the full WS2812 RMT frame encoder and
   `board_s3.rs` declares `PIN_WS2812 = 42`. Gap 5 should be struck like gaps 8 and 9.
2. **C5 status light.** `PARITY.md` decomposes the C5 into "(c) WS2812 status light", implying the
   board drives one. Only the constant exists — `WS2812_GPIO = 27` in `board/cyd_c5.rs`, with no
   consumer anywhere in the GUI tree.
3. **"The S3 and C6 have the DRAM to carry the Bard as an ordinary smol feature"**
   (`rust/clock/Cargo.toml`, at the `bard` feature). Not against today's rows: `dram_headroom()` is
   `free_dram − stack_floor`, so the S3 has 24,672 B and the C6 8,592 B against a 39,072 B cost —
   short by 14,400 B and 30,480 B. **No declared chip currently fits the Bard without `off-fleet`.**
4. **"the S3 has NO row yet — #398"** in `budget.rs`'s own bard-refusal message. `ESP32S3_CYD`
   landed with #411; the message is one merge stale.
5. **S3 first A/B OTA roll.** `README.md` says "the first A/B OTA roll is still ahead of it" and the
   site says "it hasn't taken an OTA yet". Both are stale: id162 took build 345 → 1405 over the air
   on 2026-08-26. This file's README/site edits fix those two lines.
6. **`ELECT_ENFORCE` "does not exist in smol source"** (#148). True as the annotation is *scoped*
   (`grep-absent ELECT_ENFORCE rust/clock/src`) and misleading as prose: it is defined at
   `targets/c6-watch/src/net/smol_mesh.rs:124`, which is in this repository.

## Known gaps in this file

- **The `c5-cyd` GUI column is the weakest.** Many cells are 🔶 "shared GUI source, not witnessed on
  this board". That is the honest state, not a placeholder: the board is mesh-proven and its HA leg
  is deliberately dark during its diagnostics lane, so most of its surface has an argument behind it
  and no observation.
- **The GUI flavor's own capability layer is richer than these tables** (per-board `HAS_*` const
  contracts in `targets/c6-watch/src/board/*.rs`). Where a capability is a *const* rather than a
  cargo feature, this file reads the const — see the S3 battery-ADC row.
- **No cell here re-measures anything.** Sizes, floors and byte counts are quoted from the tree's own
  measured values with their provenance; nothing was built or flashed to produce this file.
