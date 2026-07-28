# smol — docs

Docs for the **smol** ESP32-C3 fleet, gathered and written during the build. Every doc in
`docs/` is indexed here; if you add one, add it here too — [DOC-UPKEEP.md](DOC-UPKEEP.md) says
why.

- **[ROADMAP.md](ROADMAP.md)** — the steering doc: what's **shipped / in flight / spec'd /
  researched**, plus the decision docket. **Start here for status.** The living GitHub checklist
  is [#148](https://github.com/jphein/smol/issues/148); the original tracking issue
  [#24](https://github.com/jphein/smol/issues/24) closed 2026-07-12.
- **[DOC-UPKEEP.md](DOC-UPKEEP.md)** — how to keep all of this true: where each kind of truth
  lives, how to verify a claim, and the traps that have produced stale docs before.

## Firmware, protocol & operations
- **[BUILDING.md](BUILDING.md)** — toolchain, flashing, pin map, the "which board am I holding?"
  name/MAC guide, and the gotchas that cost us time.
- **[protocol.md](protocol.md)** — the canonical **SMOLv1** wire reference: every ESP-NOW frame
  byte-accurately (HELLO/ACK, BEACON, TIME, BATT/GRID, CFG, DIAG, SCAN, RELAY/RELAYACK,
  RELAY2/RELAYACK2, BATT2/GRID2, SNK, FAM, the leaf mesh-OTA frames) plus the MQTT topic map —
  each carrying an honest per-frame verification badge. **The most reliably current doc in the
  repo; when another doc disagrees with it, protocol.md is usually the one that's right.**
- **[ota.md](ota.md)** — OTA operator guide: stage/install, ed25519 signing, canary discipline,
  leaf mesh-OTA, reproducible builds.
- **[home-assistant.md](home-assistant.md)** — the MQTT-native **Home Assistant** integration:
  Batt (voltage + SOC) and Grid displays, the node manager, collector retirement, and why not
  ESPHome / the native API.
- **[relay.md](relay.md)** — operator guide for the **ESP-NOW → internet relay**: leaf vs
  gateway roles, the flush cycle, election and recovery behaviour.
- **[mesh-snake.md](mesh-snake.md)** — how to play **World Snake**, the shared-world MMO:
  one-button controls, the six treasure-powers, the leaderboard, joining a mesh.

The firmware itself lives in **`rust/clock/`** — one `no_std` esp-hal binary for the whole
fleet: the apps, the ESP-NOW mesh (`src/net/`), the Familiar (`src/familiar/`), the Bard
(`src/bard/`), OTA (`src/ota.rs` + `src/ota_mesh.rs`) and Cast (`src/net/cast.rs`).

## Research — the C3 landscape
- **[firmware-ideas.md](firmware-ideas.md)** — the broad survey of what you can flash on an
  ESP32-C3 (ESPHome, OpenMQTTGateway, Rust, BLE HID… and why USB BadUSB is *out*).
- **[gaming-firmware.md](gaming-firmware.md)** — can retro emulators run on the C3? (Verdict:
  display-limited; custom 1-bit games are the sweet spot.)
- **[nes-on-c3.md](nes-on-c3.md)** — a concrete plan to actually run NES on the C3 (needs a
  colour ST7735 TFT + ESP-IDF; a genuine port).
- **[board-repos.md](board-repos.md)** — other projects built for this exact board + OLED.
- **[power.md](power.md)** — battery, regulation and runtime research. **§4 now opens with the
  first measured figure:** 0.2 W at the 5 V input on id8 during Bard `inf` narration ⇒ ~40 mA ⇒
  **~5 h** on a 250 mAh cell. ⚠️ Everything *below* that row is borrowed third-party data, and most
  of it describes **BLE modes this firmware never enters** (#22 refuted native BLE on the C3) —
  the flag is in the doc. Idle, `page` mode and a never-associating leaf remain unmeasured.
- **[sound.md](sound.md)** · **[le-audio.md](le-audio.md)** ·
  **[walkie-talkie.md](walkie-talkie.md)** — audio feasibility: piezo and I²S output, why LE
  Audio is out of reach on this silicon, and the push-to-talk-over-ESP-NOW design study.
- **[wearables.md](wearables.md)** — the smartwatch / wearable form-factor survey.

## Enclosures
- **[cases.md](cases.md)** — existing printable cases for this board.
- **[enclosure-resin.md](enclosure-resin.md)** — resin/SLA enclosure notes.

## Design specs & as-built plans
- **`superpowers/specs/`** and **`superpowers/plans/`** — per-feature design specs and their
  as-built execution logs, amended in place as hardware findings land. The best source for
  **measured numbers** (RAM geometry, timings, verification outcomes) — but ⚠️ **a spec holds two
  kinds of number and the formatting does not distinguish them:** the design body is *pre-build
  projection*, the `✏️ AMENDMENT` blocks are *measurements*, and the amendments routinely contradict
  the body, which is left standing on purpose as a record of what was expected. Quote the
  amendments. Full rule in [DOC-UPKEEP.md](DOC-UPKEEP.md) §2 — it is how a stale `~3.3×` slot
  figure got "corrected" to a projected `~2.3×` when the measurement was `1.42×`.

**Hardware:** ESP32-C3 SuperMini · 0.42″ SSD1306 OLED (72×40, I²C `0x3C`, SDA=GPIO5 /
SCL=GPIO6) · Bluetooth 5 LE (unused — see #22) · 4 MB flash · single-core RISC-V @160 MHz, no
PSRAM.

**The rest of the tree:** `blockdigger/` + `games/` (Arduino/C++ games, Bluepad32) ·
`oled_test/` (hardware sanity check) · `ha/` (the Home Assistant packages + dashboard) ·
`tools/` (OTA publish, reproducible build, image verify) · `site/` (the editable project site,
auto-deployed to GitHub Pages) · `experiments/` (`pocketwatch/`, `atomic14-games/`, `nes-c3/`,
`case-mod/`).
