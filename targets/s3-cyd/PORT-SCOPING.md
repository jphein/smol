# PORT-SCOPING — ES3C28P (s3-cyd, node id 162) as a full smol target

Pattern of record: `~/Projects/cyd-c5/PORT-SCOPING.md` (the #388 C5 precedent), adapted
to Xtensa. Decisions carry their conditions; when a premise moves, the decision is
re-examined, not inherited.

## Goal

Make the ES3C28P a **full smol fleet target**: first a fleet *member* from a spike image
(the #331/#388 two-phase pattern, phase 1), then a first-class chip target of the one
smol binary (phase 2). JP's directive of record (2026-08-24): *"jam everything together
into the smol project with multiple targets"* — this board is the sixth roster entry and
the first Xtensa silicon.

## Verdicts (evidence beside each)

| question | verdict | evidence |
|---|---|---|
| Which physical board? | **A new blank ES3C28P, `14:C1:9F:D1:C8:10`, node id 162** — *not* the Ember satellites (#331, id 160), *not* emberburrito's terminal (id 161) | JP 2026-08-24 ("new and blank", "same one we use in emberburrito"); passive bus-diff 23:03; `docs/protocol.md` id block |
| Does smol's stack run on this silicon? | **YES — proven, executor ON.** burrito-fw ships esp-hal **1.1.2 by lockfile** (its manifest/README say 1.1.1 — caret req, docs drifted) / esp-radio 0.18.0 / esp-rtos 0.3.0 (embassy) / esp-bootloader 0.5.0 on this exact board. smol main's lock is 1.1.1, so this spike (also locked 1.1.2) is one patch ahead of smol and level with the only build known to drive this panel — the right side for bring-up | `burrito-fw/Cargo.lock` + `spike/Cargo.lock`, checked 2026-08-24 |
| Xtensa toolchain? | **BOTH hosts** as of 2026-08-24 ~23:20: JP directed xtensa builds onto familiar, retiring the katana-only exception. familiar's espup toolchain is **pinned to 1.95.0.0** — byte-parity with katana verified (rustc 95e5bda86 / GCC esp-15.2.0_20250920 / clang esp-20.1.1_20250829). **Condition on the pin: upgrade both hosts in one motion or not at all.** | `espup install -v 1.95.0.0 -t esp32s3` on familiar; parity re-read from both `~/export-esp.sh` |
| Does `wifi + esp-now` compile for esp32s3? | **YES — proven 2026-08-24 23:2x**: full release build+link on familiar with `--features radio` (esp-radio 0.18.0 `["esp32s3","wifi","esp-now","esp-alloc","unstable"]`), `radio_dev.rs`'s ESP-NOW API usage compiled in; `libesp_radio`/`libesp_wifi_sys_esp32s3`/`libesp_rtos` rlibs confirmed in the dep graph. `coex` deliberately NOT enabled (build-script hard-error without `ble`; smol's WiFi↔ESP-NOW coexistence is same-radio channel management). **M3 is unblocked.** | `spike/build-remote.sh --features radio`, 15.5 s |
| Chip identity in smol's build? | **Already solved.** `xtensa-esp32s3` is an unambiguous triple → chip id 3, no `SMOL_CHIP` needed (unlike the C5/C6 riscv32imac collision). `chip_name()` → `"esp32s3"`, BoardProfile arm + profile_verify case exist. | `rust/clock/build.rs` (6f900a6), `net/profile.rs` (fd7cca7) |
| HA model label? | **BLOCKED-BY-DESIGN on smol#396** — every S3 announces as `"smol ESP32-S3 Ember"` today; the variant axis (leaning NVS product field beside the node id) is smol-d8's lane. Until then the spike hand-writes a **distinct** model string. | #396; profile.rs `(CHIP_ESP32S3, _)` arm |
| Group-MAC trailer? | Phase-1 spike may join un-MAC'd today (`MAC_ENFORCE = false`) but **dies silently at the enforce flip** — a known expiry, accepted for the spike, mandatory for phase 2 (free via `wire.rs` + the real `GROUP_KEY`). Match SMOLv1 replies on the 14 B **prefix** — on-air frames carry +9 B. | `docs/protocol.md` §MAC_ENFORCE; #388's M3 trap |
| ESP-NOW channel? | Mesh is ch 6 (`ESP_NOW_FIXED_CHANNEL`). A board that must *also* hold a WiFi association needs its AP co-channel — network topology, not firmware. The spike's M3 runs ESP-NOW-only (no association), so it dodges this until M4/phase 2. | protocol.md single-radio rule; `smol-ota-crown-offchannel-blocker` |
| Workspace shape for phase 2? | **A riscv32 crate and an xtensa crate cannot share a cargo workspace** (esp-hal takes one chip feature; cargo unifies workspace features). Proven pattern: chip-agnostic zero-dep `*-core` crates consumed by path, each its own `[workspace]` (burrito-fw's osk-core/swype). Relayed to the feat/347-depin lane 2026-08-24. | `burrito-fw/Cargo.toml` comment + tree structure |
| Board variants as cargo features? | **NEVER** — #352's standing rule (closed, decided). The variant axis stays runtime (#396). | smol#352 |

## Architecture

- **One codebase → one image per CHIP → runtime board profiles** (the cyd-c5/JP rule).
  The S3 image is a *chip* target; ES3C28P-vs-any-future-S3-board is a runtime/NVS axis
  (#396), never a feature.
- Identity: node id **162** in NVS (`SMOL_NODE_ID=162` at provisioning — the factory
  default is 7 and a fresh board lands there). OTA never touches NVS.
- Fleet name: **`eldritch-insignia`** (sigil MAC-derived via the `sigil-id` crate — never
  by hand; two hand-derivations of the C5's sigil were both wrong). Row landed in
  `esp32c6-watch` `feat/cyd-c5-target` @ `ba46f74` with the dual-contract test; the
  watch session cherry-picks to main + reflashes when M3 is imminent (ping them first —
  until then live watches don't know this name). ⚠️ Speech collision, not protocol:
  `eldritch-insignia` shares its adjective with `eldritch-lantern` (JP's primary watch) —
  full sigils and MQTT topics are unambiguous, but "the eldritch one" now means two
  devices; never identify a board by adjective when debugging by ear.
- ⚠️ **The MAC-fold id for this unit is 150 — it must stay out of every allocation table
  forever.** The firmware honours `config id != 42` over the fold (42 = the
  never-explicitly-chosen sentinel), so provisioning 162 is one config write; but if 150
  were ever allocated to another board, an unprovisioned unit of this board would
  collide with it. The watch repo's test enforces 150 stays unmapped; allocation lists
  live elsewhere — hence this line.
- The spike (`spike/`) is **throwaway by design**: it proves milestones and produces the
  measured numbers phase 2 needs (ChipBudget row, stack floor). It is not the product.

## Phases

### Phase 0 — tree-side identity (smol repo) — ✅ ALREADY LANDED
Chip const, `chip_name()`, unambiguous triple mapping, BoardProfile arm, profile_verify
case, id-block row (162). Landed via 6f900a6 + fd7cca7 + the 2026-08-24 protocol.md
id-block generalization. Remaining tree-side identity work is #396 (smol-d8's lane).

### Phase 1 — bring-up spike, four falsifiable milestones (this directory's lane)
| M | proves | status |
|---|---|---|
| **M1** | esp-hal 1.1.x (lock: 1.1.2) boots on *this unit*; PSRAM octal 8 MiB mapped; ILI9341V paints (MADCTL 0x28); backlight; button | **flashed + running 2026-08-24 23:2x** (serial heartbeat live, node 162); display orientation awaiting JP's eyeball |
| **M2** | WiFi STA associates (2.4 GHz only — no band trap on S3), DHCP lease | not started |
| **M3** | ESP-NOW round-trip: `SMOLv1 HELLO 162` broadcast heard by a live C3 fleet witness (roster flip = the proof), ACK matched on 14 B prefix | **unblocked** — radio compiles (verdict above); needs bench time |

M3 hard rule (from the C6 watch session, a full day lost to it): **esp-radio 0.18's
`SendWaiter::wait()` is an unbounded, non-yielding spin — and its `Drop` runs the same
spin** — so one lost TX completion pins the CPU forever and presents as a frozen board.
**Bound every ESP-NOW send** (`select(send_async(..), Timer)`; the watch uses
`TX_WAIT_MS = 30` in `esp32c6-watch/src/net/smol_mesh.rs::send_bounded`). smol's own
`send_to` carries the same raw `wait()` — logged fleet-side as a phase-2/3 fix item.
| **M4** | MQTT + retained HA discovery under id 162, distinct model string, `expire_after` set; telemetry on `smol/162/telemetry` (bare line) | not started |

M3 witness protocol (synced with the cyd-c5 session, whose M3/M4 are complete and
glass-verified): witness = **id50** (`AC:A7:04:B9:77:14`), which at the C5's M3 ran the
#391 executor canary and offers **three independent channels** — the `smol/50/peers`
roster flip, the `mf=` MAC-observe counter (exact frame-count corroboration), and the
mesh LED. **Coordinate a listen window with smol-d8 before transmitting** — stray HELLO
frames contaminate any #391 capture in flight (the C5's window was logged in theirs).
Send spike frames **without** the #190 trailer: observe-mode soft-accepts and *counts*
them, which is itself evidence. Confirm id50 is powered/audible with smol-d8 first.

M4 network facts (glass-verified at the C5's M4): the board joins `jplovescl` (VLAN 8)
→ broker is the HA VM's **same-subnet leg `10.0.8.111:1883`** — cross-VLAN legs
silently drop CONNACK (smol `ha/README` broker table). WiFi vault item:
`"Homelab jplovescl WiFi (jplovescl SSID)"`; MQTT user `jp`, whose password is
*currently* the same secret as the PSK — carries a rotation caveat in
`build-remote.sh`, mirrored from the C5's script. Note the C5 (peer 176) is
**temporarily off the mesh** — its board is running watch-port smoke builds — so its
HA entity reads unavailable by design (`expire_after` working); don't read that as a
discovery-contract failure when comparing against it.

M1–M2 are *de-risked* (burrito-fw proves the board class) but still run on this unit —
per-unit verification is the point of a spike. **Do not duplicate morpheus-burrito's
work**: id-161 membership for the emberburrito terminal lives in the emberburrito repo.

### Phase 2 — full smol firmware target (rides other lanes; this dir contributes numbers)
Ordered by dependency:
1. **feat/347-depin** lands the per-chip build arms (morpheus-depin's lane) — phase 2
   builds on its PR, never competes with it. The xtensa workspace constraint (above) is
   input to that design.
2. **smol#396** variant axis — hard prerequisite of the S3 arm being *used* (two S3
   products + this dev board would otherwise collide in HA).
3. **Measured ChipBudget row** — `budget.rs` `compile_error!`s on any non-riscv32 target
   until an S3 row with MEASURED numbers exists. Produce it the way the C6 row was
   (`scratch/convergence/c6-budget-row-from-watch-session.md`): readelf -SW sections,
   espflash save-image size, stack high-water under live radio.
4. **`[chip.esp32s3] builds = true`** in `tools/build-matrix.toml` — a one-word change,
   but it and the ChipBudget row must land **together**: `build_matrix.py check` compares
   the two rosters and goes red on either side alone (mechanical, not convention). The
   row's `blocked_on` string is already half-stale — update it when flipping.
5. `esp_app_desc!()` + partition table + `tools/verify_image.sh` confirming the #349
   descriptor reads `chip = 3` — checked, never assumed.
6. Group key: a phase-2 image reuses `wire.rs` and gets the #190 trailer free **only if
   built with the real `secrets.rs`** — a spike-provisioned example key joins today and
   falls off the mesh at the enforce flip.
7. smol-core (#347 phase 2) gains this target as consumer #4.

## Operational rules (in force now)

- **Flashing:** `spike/flash.sh` only — serial-pinned to `14:C1:9F:D1:C8:10`,
  refuse-by-default, deny-list carrying all five sibling ES3C28Ps + the C6 watches +
  every never-flash device; no `--baud` (L8); never kill a port holder; no override flag.
  **`cargo-espflash` stays uninstalled on katana** — installing it silently bypasses every
  runner guard on this machine (re-verified absent 2026-08-24).
- **Identity is passive:** `udevadm ID_SERIAL_SHORT` only. Opening the port resets the
  target. `espflash board-info` is a deliberate, logged act, not an identification step.
- **Builds go to familiar** via `spike/build-remote.sh` (JP directive 2026-08-24, retiring
  the earlier katana-only exception *in writing, as its condition required*): espup was
  installed on familiar the same night, **toolchain pinned to 1.95.0.0** for byte-parity
  with katana — upgrade both hosts in one motion or not at all. Flashing stays local
  (the board and the guard are on katana's bus). katana's toolchain remains valid for
  local builds when familiar is asleep. ⚠️ familiar's `/tmp` is a 512 MB tmpfs — it
  filled during the espup install and impersonated a compile error (same class as the
  #363 gate lesson); anything staged remotely uses `TMPDIR=/var/tmp`.
- **Shell setup, both halves or nothing works:**
  `export PATH="$HOME/.cargo/bin:$PATH" && source ~/export-esp.sh`
  (missing first half = `cargo: command not found`; missing second = a "linker
  xtensa-esp32s3-elf-gcc not found" that impersonates a broken toolchain).
- **Release builds only** (the esp-hal PSRAM path requires it).
- **Bring-up method (the cyd-c5 `bringup-lessons` distillation — their four wrong turns
  were ALL frame-of-reference or naming errors, none arithmetic):** establish one
  **frame-free anchor measurement** early (finger-and-dot-in-one-glance, on-screen
  markers in display coordinates — never "top-left" in prose) and reconcile every later
  claim against it; make firmware **self-assert its calibration** (boot-time anchor test
  printing PASS/FAIL) so correctness is checkable from the log; name flags for what they
  AFFECT, not what they derive from; **never cite a library default from memory** — open
  the file or call it folklore; verify a fix against the anchored measurement BEFORE
  flashing; and state a number's scope conditions as explicitly as its provenance.
- **PSRAM+DMA is per-chip, not per-family:** the S3 *has* `dma_can_access_psram`
  (`DmaExtMemBKSize`) — the C5's "DMA staging must be internal" rule is C5-specific and
  must not be inherited. burrito-fw's internal-SRAM staging was a measured *choice*
  (32-byte alignment cost), not a hardware impossibility. Paint arithmetic still rules:
  a 320×240 RGB565 full frame is ~27–30 ms on the wire at 40 MHz — dirty-rect
  discipline is mandatory regardless.
- **Clippy must run BOTH invocations** — `cargo clippy --release` is structurally blind to
  `radio_dev.rs` (it's `cfg(feature = "radio")`), so a cheerful default pass has never
  looked at the radio module; `--features radio` found and fixed two real defects on its
  first run. Any gate on this crate runs both or it isn't a gate.
- **Never copy `emberburrito/burrito-fw/wifi.local.toml`** into this tree — it holds a
  live admin-VLAN credential. WiFi creds arrive via env at build time (Vaultwarden →
  env, the cyd-c5 `build-remote.sh` convention), never on disk here.
- **Lanes (agreed 2026-08-24 with smol-d8):** this directory is the s3-cyd session's;
  `rust/clock` + `docs/protocol.md` changes route through smol-d8; id-161 membership is
  morpheus-burrito's (emberburrito repo); per-chip Cargo arms are morpheus-depin's
  (feat/347-depin).

## Status — dated

- **2026-08-24 23:0x** — Directory created (JP's scaffold). Board plugged in, identified
  `14:C1:9F:D1:C8:10` (⚠️ same-batch near-miss with reliquary's sealed `…C3:C8`). Recon
  complete (three reports under `~/.claude/projects/-home-jp/scratch/s3-cyd-target/`).
  BOARD.md + this file written. Spike M1 drafting in flight; `radio` compile verdict
  pending. smol#396 filed (smol-d8). **Target issue: smol#398** (this directory's work
  of record — update its checkboxes as milestones land).
- **2026-08-24 23:2x** — JP: xtensa builds move to familiar. espup installed there,
  pinned 1.95.0.0 (parity verified); `spike/build-remote.sh` added. **First remote
  xtensa build green in 34.5 s; `--features radio` green in 15.5 s → `wifi + esp-now`
  on esp32s3 is PROVEN at compile/link level. M3 unblocked**, needs bench time + a
  fleet witness. Board sigil: `eldritch-insignia` (watch repo `ba46f74`).
