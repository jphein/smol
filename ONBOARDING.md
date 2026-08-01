# Onboarding — contributing to smol

A cold-clone guide to the **architecture** this project runs on. For the *product* story read
[`README.md`](README.md); for *current status* read [`docs/ROADMAP.md`](docs/ROADMAP.md) + the wave
changelog. This file is the **map**: how the pieces fit and where to go next. It links the deep docs
rather than repeating them.

**What smol is:** one `no_std` Rust binary that turns a **$3 ESP32-C3 SuperMini + 0.42″ (72×40)
SSD1306 OLED** into a handheld game console *and* an ESP-NOW mesh node that reports to (and takes
display data from) Home Assistant over MQTT. Single-core RISC-V @160 MHz, ~400 KB SRAM (no PSRAM),
4 MB flash, WiFi + BLE5, one shared 2.4 GHz radio.

## Repo layout (top level)
| Dir | What |
|---|---|
| `rust/clock/` | **the firmware** — the unified `no_std` esp-hal binary (start here) |
| `ha/` | Home Assistant integration — MQTT packages, Lovelace cards (`ha/README.md`) |
| `site/` | the project website (auto-deploys to GitHub Pages) |
| `docs/` | architecture + operator guides (indexed in `docs/README.md`) |
| `games/`, `blockdigger/` | Arduino/C++ games (U8g2 + Bluepad32) — the pre-Rust lineage |
| `experiments/` | pocket-watch case (CAD/STL), atomic14 game pack, NES-on-C3 base |

---

## 1. The firmware (`rust/clock/`, `no_std` esp-hal)

**Dependency quartet (pinned EXACT — don't bump casually):** `esp-hal =1.0.0-rc.0`,
`esp-wifi =0.15.0`, `smoltcp =0.12.0` (+ `esp-alloc`); OTA adds `esp-storage =0.7.0` +
`esp-bootloader-esp-idf =0.2.0`. Target `riscv32imc-unknown-none-elf`, `build-std = [core, alloc]`.
The pins are a matched set — a version bump is its own project (see `Cargo.toml` notes).

**The mental model — 3 feature tiers** (this is the thing to internalize):
| Feature | Adds | Why it's tiered |
|---|---|---|
| `default` (no flags) | Clock on the OLED, **no radio** | the **guaranteed-green baseline**; `cargo build --release` must always pass, and panic stays stock `loop{}` |
| `wifi` | WiFi STA + SNTP + **the whole OTA stack** (flash/otadata crates, `esp-backtrace/custom-halt` = panic→`software_reset`) | rollback semantics only matter once there's a radio to fetch over |
| `espnow` | ESP-NOW peer messaging + WiFi↔ESP-NOW switching (builds on `wifi`) | the full mesh; **this is the fleet build** |

**The app / plugin model:** each screen is an *app* (`app.rs::AppKind`) in an enum-delegation
registry; the **BOOT-button menu** (`menu.rs`) switches between them (short-tap = cycle, long-press =
enter/escape). Apps: Clock, Snake, Bench (mesh-view), Batt, Grid, About, mesh-snake (the MMO).
Adding an app = add an `AppKind` variant + its module + a registry entry.

**Module map** (`rust/clock/src/`):
- `main.rs` — entry + the main loop (mode tick, OTA offer pickup, redraw).
- `app.rs` / `menu.rs` — the plugin registry + BOOT-menu.
- `clock.rs` `snake.rs` `bench.rs` `batt.rs` `grid.rs` `about.rs` `mesh_snake/` — the apps.
- `net.rs` + `net/` — the network layer: `mode.rs` (RadioManager, mode-switch, gateway election,
  relay), `wifi.rs` (WiFi burst, MQTT burst, OTA fetch, coexist), `mqtt.rs` (hand-rolled MQTT 3.1.1
  QoS0), `names.rs` (realm-sigil node names + forge-realm version names).
- `ota.rs` — the OTA engine (announce parse/gate, `ImageWriter`, `activate`, `boot_confirm`).
- `board.rs` / `secrets.rs` — **git-ignored per-board config + creds** (see §4).
- `input.rs` `led.rs` `sensors.rs` — buttons, the peer-state LED, sensors.

---

## 2. The ESP-NOW mesh (SMOLv1)

Wire protocol: **[`docs/protocol.md`](docs/protocol.md)** — every frame byte-accurate with honest
per-frame verification badges — HELLO/ACK, BEACON, TIME, BATT/GRID, CFG, DIAG, SCAN, RELAY/RELAYACK,
RELAY2/RELAYACK2 (routed multi-hop), BATT2/GRID2, SNK, FAM and the leaf mesh-OTA frames, plus the MQTT
topic map. Read the list there rather than here; an enumerated copy goes stale every wave. Operator guide: **[`docs/relay.md`](docs/relay.md)**.

- **Roles decide themselves at boot from DHCP:** a board that associates + gets a lease is a
  **gateway** (bridges to HA); one without creds / out of range is a **leaf** (pure ESP-NOW). Put
  creds on the board you want as gateway.
- **Single active gateway, elected by best connection** (self-healing #14). The election runs **over
  the broker** — a retained `smol/mesh/channel` = `MC|owner|channel|seq` record — so it can't
  fragment (gateways on different channels still share one broker). The `seq`/liveness field is
  load-bearing: a stale record → the next-best board takes over.
- **Operator channel lever** (#155): publish a **retained** `smol/mesh/channel_hint` = a decimal
  channel (e.g. `6`) and the crown **honors it at claim time** — a board whose AP channel ≠ the
  hint refuses the crown, so the mesh converges onto a board already on that channel (the coexist
  radio can only park the mesh on the crown's *own* AP channel, so the lever steers *which* board
  is crown). A sitting crown on the wrong channel yields (goes HELLO-silent) so a hinted-channel
  board takes over. **Clear it** by publishing an empty retained payload → normal election resumes.
  This replaces the old manual seq-forged `MC` plant. ⚠️ A hint no board can satisfy leaves the
  mesh crownless until you clear it — it's a deliberate operator control, not automatic.
- **No-burst coexist** (#23): the gateway stays associated and runs ESP-NOW + WiFi concurrently on
  one channel, so a telemetry flush no longer tears the radio down (no multi-second mesh-deaf
  window). Leaves scan 1/6/11 to find the gateway's channel + re-lock on silence.
- **Maturity is honest** — coexist + self-heal are HW-verified *same-channel*. **#31
  (broker-dead-HELLOing) closed 2026-07-12; cross-channel roam #35 is still open**, and the live
  residual to know about is **#204** — a crown under bulk unicast RX goes downstream-deaf. Check the
  ROADMAP before assuming a mesh feature is fully proven.

---

## 3. MQTT-native Home Assistant integration

Architecture + rationale (why MQTT, not ESPHome/native-API): **[`docs/home-assistant.md`](docs/home-assistant.md)**.
Operational half (entities, YAML, deploy): `ha/README.md`.

- **Uplink** — on each WiFi burst the gateway publishes retained **MQTT-discovery** configs, so each
  node appears as a native `sensor.smol_<id>_*` entity (zero HA-side YAML). Leaf telemetry is relayed
  leaf→gateway over ESP-NOW first.
- **Downlink** — HA automations publish **retained** display payloads (`smol/display/batt`,
  `smol/display/grid`); the gateway grabs them in its burst and **re-broadcasts a SMOLv1 frame** so
  leaves render them too (the broker is the cache; the mesh is the last hop).
- **Node manager** (#21) — HA publishes a retained `smol/<id>/config/default_screen`; the board
  **consumes** it on its burst (strict, panic-free parse) and applies the screen — no reflash.
- **OTA** — a native **Update** entity (#33) + a canary flow that is **structural, not procedural**.
  Two topics, and only two: `tools/ota_publish.sh stage` publishes retained **`smol/ota/staged`**,
  which *arms* every board's Update entity but makes **no board fetch anything**; then
  `install <id>` publishes retained **`smol/<id>/ota/install`** — **per-node**, mirroring HA's
  Install button. **There is deliberately NO fleet-push topic.** The old
  `announce/<id>` / `announce/all` act-path was **retired at the Model-A #32 closure**, precisely so
  that "update everything at once" is not expressible. If you find yourself looking for a fleet
  topic, that is the design working — install one board, confirm its version advances, then the
  next.
  The leaf **verifies an ed25519 signature before it writes a byte** (#32); sha256 is the image
  *identity* used against reproducible builds (#44), never the trust gate. Then activate →
  first-boot self-test → app-side rollback.
  🟢 OTA is **hardware-proven**, including ~1 MB images delivered to WiFi-less leaves over the mesh
  (#6/#40/#32). What remains unproven is **bootloader revert-on-boot-fail** — espflash's bundled
  bootloader ships with app-rollback *off* — so **app-side rollback + canary-one-board-at-a-time are
  the mass-brick defense**, and that has not changed. **Operator guide + safety model:
  [`docs/ota.md`](docs/ota.md).**

---

## 4. Build, flash, contribute

Full toolchain + gotchas: **[`docs/BUILDING.md`](docs/BUILDING.md)**. TL;DR for a cold clone:

1. **Per-board config (git-ignored):** `cp src/board.rs.example src/board.rs` (sets `NODE_ID`,
   `DEFAULT_APP`, `DEFAULT_PAGE`) and `cp src/secrets.rs.example src/secrets.rs` (WiFi/MQTT creds —
   **placeholders in the example; never commit real values**). Per-board flashes edit only these
   ignored files, so every board stamps the same clean commit hash.
2. **Build tiers and the gate: `tools/gate.sh`.** Run it before you push; CI (`.github/workflows/
   fw-gate.yml`) runs the *same script*, so there is nothing here to fall out of step with it.
   `tools/gate.sh` (or `host` / `fw` for one half) covers: `cargo check` and `clippy -D warnings`
   across **every tier**, the host `experiments/*_verify` suites, the crate's own `tests/*.rs`
   suites, and the #300 **stack floor** (printed on every PR).

   **Since #350 the tier and chip lists are DERIVED from `tools/build-matrix.toml`**, which also
   separates *what CI builds* from *what the release ships* — the S3 is declared and not shipped,
   OpenWrt's `DEFAULT := n`. Add a tier or a chip **there**, not in `gate.sh`, and not here.

   **This list deliberately lives in the script, not here.** Until #338 it lived only in prose, and
   the prose was wrong without anyone noticing: `main` sat red on its own `-D warnings` rule, and a
   feature was added with no matrix to add it to. Add a tier to `gate.sh`, not to this paragraph.

   **Known gaps** (`gate.sh` states them too): `mesh-test` needs a real board's `DEAF_MACS`; image
   packaging and anything needing hardware are out of scope. The stack check bounds the linked
   **region**, *not* runtime high-water — green there is not evidence of headroom.
   (`clippy -D` covered only the canonical tier until #343; it now runs on **every** tier.)

   The `default` build must also stay behaviourally unaffected (prove via cfg-gating, **not** ELF
   byte-equality — `build.rs` stamps a per-commit git hash, so the default ELF changes every commit
   by design).
3. **Version identity:** `build.rs` embeds `BUILD_HASH` + `BUILD_NUMBER` →
   `names.rs::version_name()`, a forge-realm sigil name. The number is the OTA monotonicity gate;
   the name is the boot-splash reveal. Two things to get right:
   * **`BUILD_NUMBER` is the committed ratchet in `rust/clock/version.txt`** (currently `345`), or a
     `SMOL_BUILD_NUMBER` override. `git rev-list --count HEAD` is only build.rs's *fallback* when
     neither exists, and it reads much higher.
   * **The name is computable, so a mismatched pair is a bug:** `noun = FORGE.nouns[n % 20]`,
     `adj = FORGE.adjectives[(n / 20) % 20]`. So **345 → "Riveted Furnace"** and **56 → "Carbonized
     Quench"** — *not* "Hammer", which this line said until 2026-07-27, because **#218 replaced the
     old `>>8` formula with direct modulo.** Any sigil name written before 2026-07-20 needs
     recomputing.
4. **Flash (USB):** from `rust/clock/`, `cargo run --release --features espnow` — the runner is
   `espflash flash --monitor --partition-table partitions-ota.csv` (lays down the dual-A/B OTA
   table). Board identity comes from that board's own `board.rs`/`secrets.rs`.
5. **Canary discipline (OTA):** **never fleet-push.** Push to one board, confirm it boots `Valid`
   (serial + boot-splash version name), then the next. Details + the seatbelt: `docs/ota.md`.
6. **USB recovery:** a bricked board is recovered exactly like a first flash (step 4) with the OTA
   partition table — see `docs/ota.md` → *USB recovery*.

---

## Where to read next
- **Status / what's proven:** [`docs/ROADMAP.md`](docs/ROADMAP.md) + the wave changelog (honest
  per-item maturity — don't assume "verified" without checking).
- **Wire protocol:** [`docs/protocol.md`](docs/protocol.md) · **Mesh/relay:** [`docs/relay.md`](docs/relay.md)
- **HA:** [`docs/home-assistant.md`](docs/home-assistant.md) · **OTA:** [`docs/ota.md`](docs/ota.md)
- **Build/flash:** [`docs/BUILDING.md`](docs/BUILDING.md) · **Play:** [`docs/mesh-snake.md`](docs/mesh-snake.md)

*Honesty is the house style: docs mark what's hardware-verified vs built-but-unproven, and name every
residual. When you add a feature, badge it the same way — built ≠ verified until the hardware says so.*
