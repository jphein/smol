# Building & flashing smol

Everything needed to build and flash the firmware in this repo, plus the
hardware facts and the non-obvious gotchas we hit (so you don't have to).

## Hardware (read off the chip with esptool)

- **MCU:** ESP32-C3 (QFN32) rev **v0.4** — single-core RISC-V @ 160 MHz, ~400 KB SRAM, **no PSRAM**
- **Flash:** 4 MB embedded (XMC)
- **Radio:** Wi-Fi + **Bluetooth 5 LE only** (no Bluetooth Classic → no A2DP/HFP, no USB-HID; BLE HID is fine)
- **USB:** native USB **Serial/JTAG** (enumerates as `/dev/ttyACM0`) — not USB-OTG
- **Display:** 0.42" **SSD1306, 72×40**, 1-bit, I²C addr **0x3C**
- **Board:** ESP32-C3 SuperMini + 0.42" OLED, **USB-C**. Buttons (RST + BOOT) at the OLED/antenna end; the two LEDs (PWR + IO8) flank the USB-C connector at the other end.

### Pin map
| Pin | Use |
|---|---|
| GPIO5 / GPIO6 | I²C SDA / SCL (OLED) |
| GPIO8 | onboard **blue LED** (IO8, active-LOW); also a strapping pin |
| GPIO9 | **BOOT** button (input, active-low); strapping pin |
| GPIO4 | free ADC1 channel — used for **battery voltage** (needs a divider) |
| GPIO10 | suggested **piezo** buzzer (see docs/sound.md) |
| GPIO3/4/6/7/10 | if adding an ST7735 color TFT for NES (see docs/nes-on-c3.md) |

## Toolchain setup

### Arduino (games: Block Digger, Snake, 2-player Snake, atomic14 pack)
```bash
# arduino-cli in ~/.local/bin
curl -fsSL https://raw.githubusercontent.com/arduino/arduino-cli/master/install.sh | sh
arduino-cli config init
arduino-cli config add board_manager.additional_urls \
  https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json \
  https://raw.githubusercontent.com/ricardoquesada/esp32-arduino-lib-builder/master/bluepad32_files/package_esp32_bluepad32_index.json
arduino-cli core update-index
arduino-cli core install esp32:esp32              # ~1–2 GB toolchain
arduino-cli core install esp32-bluepad32:esp32    # NOTE: HYPHEN, not underscore
arduino-cli lib install U8g2
```

### Rust (the unified firmware: Clock + Snake + Bench)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
rustup target add riscv32imc-unknown-none-elf     # C3 is a stock upstream RISC-V target
# espflash v3 (v4 refuses esp-hal 1.0.0-rc.0 images — see gotchas):
CC=gcc cargo install espflash --version "^3"
```

## Flashing

The port is `root:dialout`; if your user isn't in `dialout`, run this before every upload/monitor:
```bash
sudo chmod a+rw /dev/ttyACM0
```

### Arduino games
```bash
FQBN=esp32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M          # Block Digger uses the -bluepad32 core
arduino-cli compile --fqbn "$FQBN" games/snake
arduino-cli upload  --fqbn "$FQBN" -p /dev/ttyACM0 games/snake
```
Block Digger needs the Bluepad32 core: `esp32-bluepad32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M`.

### Rust unified firmware (`rust/clock/`)
```bash
cd rust/clock
cp src/board.rs.example   src/board.rs        # then set NODE_ID + DEFAULT_APP/DEFAULT_PAGE (per board) — git-ignored, ALL builds (#18/#19)
cp src/secrets.rs.example src/secrets.rs      # then edit WIFI_SSID / WIFI_PASS — git-ignored, wifi/espnow only
# For an --features espnow build, also set GROUP_KEY (32 bytes) in secrets.rs — the shared #190
# mesh-auth key. Generate one with `openssl rand -hex 32` and give the WHOLE FLEET the same key
# (+ same GROUP_KEY_EPOCH). It's authenticity, not confidentiality; NEVER commit it (repo is public).
ESP_LOG=info cargo build --release --features espnow   # full build: Clock + Snake + Bench
espflash flash --port /dev/ttyACM0 target/riscv32imc-unknown-none-elf/release/clock
```
Feature tiers: default = Clock + Snake · `--features wifi` = + NTP · `--features espnow` = + ESP-NOW peer LED/mesh + Bench.

## Gotchas (the ones that cost us time)

- ⚠️ **After ANY OTA, a USB flash silently lands in the slot that will not run.** The OTA left otadata pointing at `ota_1`; a subsequent `espflash` write goes to `ota_0`, succeeds, and the board keeps booting the OTA'd image. It reads as a brick or a failed flash and is **neither** — you flashed fine, into the slot the bootloader is not selecting. Clear otadata first, then reset, then flash:
  ```bash
  espflash erase-region 0xf000 0x2000   # otadata ONLY — spares `nvs`, so the node id survives
  ```
  **Check the `Loaded app from offset` line after every flash** — it names the slot that actually ran, and it is the only cheap confirmation you flashed the thing you are about to debug. *(Cost an hour on 2026-07-28; it had been living in operator memory and no `docs/` file.)*
- **espflash v4 won't flash** esp-hal `1.0.0-rc.0` images (wants an ESP-IDF app descriptor). Use **espflash v3**. *(Untested for esp-hal 1.1 images such as the `dream/feat-embassy` branch — v4 may accept those; do not assume either way while recovering a board.)*
- **`esp-wifi` pins to esp-hal internals:** it needs **`esp-hal = "=1.0.0-rc.0"`** exactly (newer rc.1/1.0 changed `Rng::new()` and break the build despite passing semver). Full working pin-set is in `rust/clock/Cargo.toml` + comments.
- **Rust serial logs go over USB-JTAG:** build with `ESP_LOG=info` (level is compile-time) and view with `espflash monitor` — plain `cat /dev/ttyACM0` won't show them, and the monitor needs a real TTY (fails under a pipe).
- **`espflash monitor` reset mode matters on this native-USB C3:** `--before default-reset` (the UART-bridge DTR/RTS reset) **fails silently** — it drops the chip into download/stub mode, so you get the monitor banner and then nothing. Use **`--before usb-reset`** (the USB-JTAG-Serial peripheral reset) to actually reboot the app and catch its boot log. The firmware only logs at **boot + state changes**, so a silent idle board looks identical to a broken capture — you must catch the boot.
- **Capture logs through a PTY, not a pipe:** espflash block-buffers when stdout isn't a terminal, so `espflash monitor | tee` stalls. Wrap it in a pseudo-terminal: `timeout <N> script -qec "espflash monitor --port <port> --before usb-reset" <capfile>`. Kill it by **exact** name only (`pgrep -x espflash` / `pkill -x espflash`) so you don't nuke unrelated processes.
- **Identify boards by USB vendor / MAC, never by `ttyACMx`:** the number isn't stable — a board re-enumerates on replug (we saw `ttyACM2 → ttyACM0`, same MAC) and other USB-serial devices can squat a lower number. Espressif boards are **`303a:…`** (`lsusb`; `303a:1001` = the USB-JTAG/serial peripheral); pin the exact unit by **MAC** (`udevadm info /dev/ttyACM* | grep -i serial`, or read it from the boot log). On this box `ttyACM1 = 1209:2201` is a **Dygma keyboard** — opening it as if it were the board is a real mistake, so match `303a:` first.
- **Broken `cc` shim on PATH** on this box → prefix cargo installs with `CC=gcc`.
- **`CDCOnBoot=cdc`** is required in the Arduino FQBN for Serial over USB-Serial/JTAG.
- **Bluepad32 package is `esp32-bluepad32`** (hyphen), not `esp32_bluepad32`.
- **Display 180°:** the pocket-watch case hangs from the USB-C end, so the firmware sets `DisplayRotation::Rotate180`. On a bare board with USB-C down it reads upside-down — flip it USB-up (or set `Rotate0` for bench use).
- **Secrets:** real WiFi creds **and the #190 `GROUP_KEY`** (the fleet-shared mesh-auth HMAC key, `espnow` builds) live only in git-ignored `rust/clock/src/secrets.rs` (the repo is public). Every board must share one `GROUP_KEY` + `GROUP_KEY_EPOCH`, or a MAC-mismatch drops its frames (in observe it just bumps `mf=` in DIAG; in a future enforce release it partitions).

## Multi-board / ESP-NOW mesh
Give each board a **distinct peer id**. ⚠️ **This is no longer a source literal.** It used to be an argument to `mode::start(...)` in `main.rs`; identity now lives in a **runtime NVS record** (`main.rs` passes `node_id()`), which is what lets **one image serve the whole fleet** and is why OTA never touches identity. Set it per board in the git-ignored `src/board.rs` (`cp src/board.rs.example src/board.rs`), or provision it on-device. Distinct ids let the blue-LED handshake and the Bench link stats work between boards (same id can be filtered as self-echo). Boards auto-pair over ESP-NOW on the AP's channel; watch the blue LED go slow-blink (detected) → solid (connected).

Each id maps to a deterministic **magical name**. Since `8e47325` the corpus comes from
**lexicon's `fleet` group** through the realm-sigil Rust binding — not a hand-copied table — and the
mapping is `adj = fleet.adjectives[seed % 32]`, `noun = fleet.nouns[(seed >> 8) % 32]`,
`seed = id * 2_654_435_761`.

> ⏳ **Two mappings — the rename is on `main`, your board probably is not.** The chain merged
> 2026-07-28 (`5934682`, all five of `f63dbea` `8e47325` `af7d678` `53ee511` `f091a00` now ancestors of
> `main`), so **`main` produces the RIGHT column** — but **a board keeps answering to the left column
> until it is flashed with a build containing `f63dbea`.** Check the board's own SIGIL or About screen,
> not this file.

| id | what a board flashed **before** `f63dbea` shows | what `main` produces **now** |
|---|---|---|
| 5 | Spectral **Aegis** | Obsidian **Aegis** |
| 7 | Draconic **Dominion** | Radiant **Obelisk** |
| 8 | Eldritch **Nexus** | Eldritch **Jewel** |
| 9 | Jade **Herald** | Seraphic **Dominion** |
| 42 | Celestial **Herald** | Gilded **Quartz** |
| 50 | Kindled **Ember** | Mystic **Chalice** |
| 51 | Primal **Sigil** | Ashen **Vigil** |
| 122 | Celestial **Crown** | Somber **Vigil** |
| 236 | Radiant **Herald** | Hollow **Lantern** |

Right column regenerated from merged `main`, not copied from the branch:
`cargo run -q --example board_names --all` in `rust/viz/mesh-model` (a pure function of the id — needs
no fleet contact).

**Delete the left column when the FLEET HAS BEEN ROLLED onto a build containing `f63dbea` — the
trigger is the roll, not the merge.** ⚠️ This note previously said *"on merge, delete the left column"*,
which was **wrong, and the mistake is worth keeping**: merging changes what `main` *produces*; only a
flash changes what a board *answers to*. Two different events, and a bench doc lives entirely in the
second one.

**Uniqueness is now a compile-time guarantee rather than a hope:** `is_injective_over_u8` enumerates all 256
ids during **const evaluation**, so a colliding namespace **does not compile**. The 32×32 size lock and
the reserved-word exclusion are const assertions too. Full reasoning:
[lexicon's node-identity design](https://github.com/jphein/lexicon.realm.watch) →
`docs/superpowers/design/node-identity-namespace.md`.

> ⚠️ **These names changed on 2026-07-28.** Previously id7 *Draconic
> Dominion*, id8 *Eldritch Nexus*, id9 *Jade Herald*, id50 *Kindled Ember*, id51 *Primal Sigil*,
> id122 *Celestial Crown*. The old corpus was 20×20 and could not produce unique names — **93
> collisions across 256 ids** — and six of its nouns were project vocabulary (`crown` is the gateway
> **role**; `beacon` is a **wire frame**; `forge` is the **version-name** realm). Expanding the corpus
> re-maps every id, because indices are `% len`; that is a one-time cost paid deliberately with
> headroom rather than repeatedly.
>
> **`Eldritch Nexus` is the dangerous stale one** — id8's *adjective* is still `Eldritch` and only the
> noun changed to `Jewel`, so the old name looks plausible instead of obviously wrong. It is the one
> that survives a proofread.

> 🔴 **Only IDENTITY moved to lexicon. Two other namespaces stay pinned inside smol, deliberately:**
> - **Version** names keep the pinned **20×20 forge** table. Upstream's `forge` realm is a
>   *non-superset* 14/14, so adopting it would rename **every past build** — v345 would stop being
>   "Riveted Furnace" — and version names are **historical record**.
> - **Creature** names (the Familiar) keep the pinned `fantasy` corpus — **on `main` today.**
>   ⏳ **Pending:** `53ee511` gives creatures their own lexicon namespace (`creature`, 24×24, disjoint
>   from `fleet` and `reserved` and compile-time checked) and **deletes `FANTASY` — smol's last
>   hand-copied word list.** At that point `names.rs` holds **zero corpora and a checker**, and
>   `familiar/mod.rs`'s *"distinct from any node's name"* comment can no longer be written without being
>   true. **Not yet on `main`** (verified: not an ancestor of HEAD), so the line above is what the tree
>   does now. When it merges, delete the pending note and the creature row's "pinned" wording.
>   ⚠️ **It renames existing familiars once** — seeds are frozen for life. Unlike boards, creature names
>   appear in no doc, memory or entity id, so the rename is invisible to everything except a person
>   watching a screen.
>
> So *"smol sources its names from lexicon"* is **half true** and worth not writing.

The name is that board's identity in the mesh: it shows on peers' World-Snake screens and the
leaderboard.

### ⚠️ A name mapping is not a board assignment — and id7/id9 have moved

**These are two different facts and conflating them cost real time on 2026-07-28.** *"id 7 = Draconic
Dominion"* is a **pure function**, permanently true. *"id 7 is a board on the bench"* is a **hardware
assignment**, and it changed:

> **The hardware formerly running as id7 (Dominion) and id9 (Herald) has run as id50 (Ember) and
> id51 (Sigil) since 2026-07-22**, re-provisioned as the **#198 Phase-2 measurement boards** (the
> commit calls them *"board-B beacon + dominion DUT"*). **id7 and id9 do not exist on the air.**

Two consequences that trip people up:

1. **An OTA will not restore the old ids.** Identity lives in the **NVS record**, and **OTA never
   touches NVS** — that is deliberate, it is what lets one image serve the whole fleet. Only
   **re-provisioning** changes an id. Anyone who assumes "update the firmware and the naming will sort
   itself out" will wait forever.
2. **HA still has the dead entity families.** `sensor.smol_7_*` / `smol_9_*` and
   `update.smol_7_dominion` are alive in Home Assistant and answer queries, for node ids nothing has
   broadcast in months. **A per-node entity existing is not a node existing** — see
   [DOC-UPKEEP](DOC-UPKEEP.md) §2. Cleanup of those families is tracked separately.

**Live roster, 2026-07-28** (from the crown's ESP-NOW peer attribute + fresh telemetry, not retained
values): **id8 Nexus**, **id5 Aegis**, **id50 Ember**, **id51 Sigil**, plus **id122** — a rig id whose
purpose is unidentified, and which the formula names *Celestial **Crown***. ⚠️ Note the collision: smol
calls its elected gateway "the crown", so a log line mentioning *Crown* may be a **node name**, not a
**role**. Prefer "the bench fleet" in prose and name a node only where a claim was verified on it —
[DOC-UPKEEP](DOC-UPKEEP.md) *never enumerate the fleet*.

### "Which board am I holding?" — identify by name / MAC, not the port
With several identical boards on the bench, don't trust the `ttyACMx` number (it's not stable, and a keyboard can squat a low one — see the espflash gotchas above). Instead:
- **On-screen:** the board prints its name at boot (`smol: I am Eldritch Jewel (id 8)`) and shows it in the mesh UI — read the OLED to know which physical unit you're holding.
- **By USB vendor/MAC:** Espressif boards are `303a:…` (`lsusb`); pin the exact unit by MAC (`espflash board-info`, `udevadm info /dev/ttyACM* | grep -i serial`, or the boot log). Keep an id ↔ MAC ↔ name map for your fleet. **Historical, verified 2026-07 *before* the Phase-2 re-provisioning** (§above): `ac:a7:04:b9:77:14` was id 7 *Draconic Dominion* (then the WiFi/NTP root), `ac:a7:04:ba:1f:24` = id 8 *Eldritch Nexus*, `10:00:3b:ce:95:cc` was id 9 *Jade Herald*. **The MACs are still those boards; the ids on two of them are not** — the first and third now answer as id50/id51, and for the *names* those two ids show, read the two-mapping table above rather than trusting any name written here (they were renamed again on 2026-07-28). This is exactly why the map is worth keeping *per-MAC*: **the MAC is the board; the id is a setting, and the name is a function of the id.**
- **Final-flash flow:** confirm the target unit by MAC/`board-info` first, flash with its intended id (via `src/board.rs` / on-device provisioning — **not** the retired `mode::start(…, <id>, …)` literal), then watch the boot log echo the expected name — that name+id on the OLED is your confirmation you flashed the right physical board.

The mesh wire protocol — exact byte layouts, cadence and per-frame verification status — is documented in **[docs/protocol.md](protocol.md)**: HELLO/ACK, BEACON, TIME, BATT/GRID, **CFG**, **DIAG**, **SCAN**, RELAY/RELAYACK, **RELAY2/RELAYACK2** (routed multi-hop, #13), **BATT2/GRID2**, SNK (**shipped**, not design-stage — #5 closed 2026-07-08), **FAM** (the Familiar, #57) and the **leaf mesh-OTA** frames (#40). Treat protocol.md as the list; enumerating frames here just goes stale.
