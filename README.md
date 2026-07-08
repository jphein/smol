# smol

A whole game console — and a pocket watch — on a **$3 ESP32-C3 SuperMini with a 0.42" (72×40) OLED**.

It started as *"can we make this into a tiny game player? can it run Minecraft?"* Answer: **real Minecraft, no** (400 KB RAM vs. gigabytes) — **but the soul of it, yes.** From there it grew into a little platform: multiple games, a unified Rust firmware with a menu, an ESP-NOW mesh (now with **time sync** and **magical node names**), a 3D-printable pocket-watch case, and a self-hosted project site. Now on hardware: all three boards run the full stack on build 45 *"Oxidized Die"* — the shared-world **MMO snake** with six **treasure-powers**, a **plugin framework** (About screen, scrolling menu, Bench mesh-view), the sigil **version name on the boot splash**, and a **native Home Assistant integration** over MQTT: battery **voltages + SOC** and **grid power** on every display, a **HA node manager** to set each board's default screen remotely, and a git-ignored **per-board config file** (#19). The **ESP-NOW→internet relay** is proven end-to-end; **OTA updates** are spec'd + hardware-de-risked and next.

🌐 **Live site:** https://jphein.github.io/smol/ &nbsp;·&nbsp; 🕹️ Hardware-verified on real boards.

## What runs on it (the "apps")
| App | What | Status |
|---|---|---|
| **Block Digger** | Minecraft-ish dig/build on a grid, Bluetooth **Stadia** controller (Bluepad32) | flashed ✓ |
| **Clock** | NTP + Pacific time, 12h AM/PM, big digits, chip-temp + battery readout | flashed ✓ |
| **Snake** | classic, one-button | flashed ✓ |
| **Mesh Snake** | two boards head-to-head over **ESP-NOW** | flashed ✓ (2-board verified) |
| **Benchmark** | live ESP-NOW link test — FPS / RTT / loss % / RSSI | in the Rust firmware |
| **atomic14 pack** | 5 single-button games built for this exact board | flashed ✓ |
| **Mesh time sync** | boards agree on the clock over ESP-NOW; newest NTP sync wins (loop-free) | flashed · **3-way mesh verified ✓** (adoption proven 3×, issue #4) |
| **Magical names** | every board shows a **realm-sigil** fantasy name (id7 *Draconic Dominion*, id8 *Eldritch Nexus*, id9 *Jade Herald*) | **flashed on all 3 boards ✓** |
| **World Snake (MMO)** | shared 256×256 world over the mesh, scrolling viewport, peers by name, mesh leaderboard | **flashed on all 3 boards ✓** · running (waves 3+4); live-play verify next (issue #5) |
| **Treasure powers** | 6 collectible powers for World Snake — phase / haste / shield / growth / reveal / rebirth | **in code · committed · flashed on 3 boards ✓** (issue #5) |
| **Relay bridge** | single-hop ESP-NOW→WiFi telemetry to the internet (not browsing — 250 B MTU) | **E2E verified & sustained on hardware ✓** (leaf→gateway→WiFi→collector) (issue #3) |
| **HA Batt** | Home Assistant battery **voltages + SOC** on **every** display — a voltage overview page plus **big per-battery SOC / charge detail pages** (short-tap to page, #17); the gateway pulls a retained 6-segment payload from HA's MQTT broker each WiFi burst and re-broadcasts a `SMOLv1 BATT` mesh frame so neighbour leaves cache it too (single-hop) | **flashed ✓ build 45** — HA round-trip + on-glass SOC pages verified (leaf-frame receipt still inferred, #15/#17) |
| **HA Grid** | Home Assistant **grid / consumption power** — yurt total + the two phase clamps (watts) — on a dedicated Grid screen; the exact twin of HA Batt (retained `smol/display/grid` + a `SMOLv1 GRID` mesh frame) | **flashed ✓ build 45** (#16) — HA round-trip verified |
| **Node manager** | set any node's **default screen + page remotely** from Home Assistant — no reflash; HA publishes a retained `smol/<id>/config/default_screen` the board reads on its next burst | **HA side deployed ✓** (Lovelace + retained publish, #21) · firmware consume side **pending** |
| **OTA updates** | over-the-air firmware via a WiFi burst — dual A/B slots + rollback, a retained `smol/ota/announce`, sigil **version names** | **spec'd + hardware-de-risked** (bootloader honors otadata; A/B fits 4 MB @ ~3.3× headroom) · version splash **live on glass** · build waves next (#6) |

The **unified Rust firmware** (`rust/clock/`, `no_std` esp-hal) ties Clock + Snake + Bench into **one binary** with a BOOT-button menu; the blue LED shows ESP-NOW peer state (off → blink = detected → solid = connected) in the background across all modes. Verified: two boards go **solid blue = connected**.

The mesh now also **shares the time** (a `SMOLv1 TIME` frame; a board adopts a peer's time only when the peer's NTP sync is more recent, so it's loop-free) and gives every board a **magical realm name** ported from [realm-sigil](https://github.com/jphein/realm-sigil) (display-only — id7 *Draconic Dominion*, id8 *Eldritch Nexus*, id9 *Jade Herald*). All **three boards are now flashed on the unified mesh firmware and verified**: id7 *Draconic Dominion* (MAC `ac:a7:04:b9:77:14`, the WiFi/NTP gateway), id8 *Eldritch Nexus* (`…ba:1f:24`, a leaf) and id9 *Jade Herald* (`10:00:3B:CE:95:CC`) — the leaves **adopt their mesh time over ESP-NOW**, a **3-way mesh proven 2026-07-07** (adoption seen 3×, logged on issue #4; compile-verified, `clippy -D warnings` clean across all 3 builds; default/wifi byte-identical).

A shared-world **MMO snake** (issue #5) just **landed and committed**: a 256×256 toroidal world, a scrolling viewport with no walls, 18-byte state frames at 5 Hz, dead-reckoned peers drawn by their magical name, and a mesh leaderboard (score = length). It — with six collectible **treasure-powers** (Wraith Veil, Zephyr Rune, Aegis Ward, Midas Sigil, Mothlight Lantern, Phoenix Ember) on top — is committed and, as of waves 3+4, **flashed and running on all three boards** (phasing fix `877b2af`, tested 52/52); live multiplayer verification is the next step. The boot splash shows the sigil VERSION name on real glass — `smol id7 "Draconic Dominion" · build 36 "Oxidized Spark" (bcafa7e)` — the third forge name in a single day as the code evolved (Pressed Jig → Riveted Jig → Oxidized Spark), the provenance system visibly working. A [player guide](docs/mesh-snake.md) covers the world, the powers and the leaderboard. The **ESP-NOW→internet relay** (issue #3) is now **proven end-to-end and sustained** (leaf→gateway→WiFi→collector, still flowing across a firmware upgrade — one follow-up: a cold-ARP first-round retry). The board now also talks to Home Assistant directly over **MQTT** (Mosquitto on the HA VM), **retiring the Python collector**: telemetry uplinks as native HA entities via MQTT discovery, and HA publishes a *retained* battery-voltage payload the gateway grabs in its burst and re-broadcasts as a new `SMOLv1 BATT` mesh frame so its neighbouring leaves show the solar-bank and EV-pack voltages too (single-hop from the gateway). **Flashed on build 45 "Oxidized Die" and verified end-to-end into HA** — wireless CONNACK, the retained downlink cached byte-exact, and a leaf's telemetry relayed leaf→gateway→HA — with leaf-side BATT-frame receipt still inferred (issue #15). The battery screen now carries a **6-segment payload** — a voltage overview plus **big per-battery SOC / charge detail pages** (short-tap to page, #17), the SOC coulomb-counted from the **BMS** (not the flaky EPEver bus) — and a twin **Grid** screen shows HA grid/consumption power (#16). The UI now also **stays responsive during WiFi-sync bursts** (defer-while-interacting + long-press abort + a "Syncing…" spinner, #20), each board's identity moved to a git-ignored **per-board config file** (`board.rs` — NODE_ID/DEFAULT_APP/DEFAULT_PAGE, #18/#19), and a **Home-Assistant node manager** (#21 — Lovelace + retained `smol/<id>/config/default_screen`) sets each board's default screen remotely (**HA side deployed**; the firmware consume side is the next wave). **OTA updates** (issue #6) are **spec'd + hardware-de-risked** (the bundled bootloader slot-selects; dual A/B fits 4 MB at ~3.3× headroom) with the version-name splash already live; and a **plugin framework** (issue #7) **landed on build 36** — an About screen, a 4-entry scrolling menu, and a **Bench mesh-view** (issue #8) — certified clean by an independent byte-exact Oracle gate (five review passes this session, every finding closed). See `docs/` and the scratch notes for the netcode, gateway, and OTA analysis.

## Repo layout
- `blockdigger/`, `games/snake/`, `games/snake-2p/` — Arduino/C++ games (U8g2 + Bluepad32)
- `rust/clock/` — the unified Rust firmware (Clock / Snake / Bench, `no_std` esp-hal); the ESP-NOW mesh lives in `src/net/` — WiFi/SNTP, time sync, magical names (realm-sigil), the shared-world MMO snake, relay bridge (hardening)
- `watch/` — Arduino smartwatch starter (NTP + weather; BLE-notification path stubbed)
- `oled_test/` — I²C + display sanity check
- `experiments/` — `pocketwatch/` (3D-printable case generator + STLs), `atomic14-games/`, `nes-c3/` (emulator base), `case-mod/`
- `site/` — the editable project website (tiny Python server + WYSIWYG; auto-deploys to GitHub Pages)
- `docs/` — research + guides (below)

## Docs
- **[docs/ROADMAP.md](docs/ROADMAP.md)** — status + steering: shipped / in-flight / spec'd / researched + the open-decision docket (start here)
- **[docs/BUILDING.md](docs/BUILDING.md)** — toolchain, flashing, pin map, and the gotchas that cost us time
- **[docs/protocol.md](docs/protocol.md)** — the SMOLv1 wire reference (every frame, byte-accurate, with verification badges)
- **[docs/home-assistant.md](docs/home-assistant.md)** — the Home Assistant integration (MQTT-native): Batt/Grid displays, the node manager, collector retirement, and why not ESPHome
- [docs/mesh-snake.md](docs/mesh-snake.md) — player guide: one button, a shared world, six treasure-powers, the leaderboard
- [docs/relay.md](docs/relay.md) — relay operator guide: leaf/gateway roles, the flush cycle, running the collector
- [docs/firmware-ideas.md](docs/firmware-ideas.md) — everything you can flash on a C3 (ESPHome, Meshtastic, Rust, BLE HID…)
- [docs/gaming-firmware.md](docs/gaming-firmware.md) · [docs/nes-on-c3.md](docs/nes-on-c3.md) — retro gaming + a real NES-on-C3 build plan (needs a color TFT)
- [docs/power.md](docs/power.md) — battery + TP4056 charging · [docs/sound.md](docs/sound.md) — piezo audio
- [docs/wearables.md](docs/wearables.md) · [docs/enclosure-resin.md](docs/enclosure-resin.md) — watch cases + an epoxy/real-watch build guide
- [docs/le-audio.md](docs/le-audio.md) — why Bluetooth LE Audio isn't happening on the C3
- [docs/board-repos.md](docs/board-repos.md) — other projects for this exact board · [docs/cases.md](docs/cases.md) — printable cases

## The pocket watch
`experiments/pocketwatch/` generates a parametric round case — **body + lid + crown** (3 printable STLs) — in the classic orientation: the chain **bail** and a removable **crown that covers the USB-C port** stack at the top; the OLED, buttons and clear-PLA LED light-pipes sit below; pockets inside for the board + a 502030 LiPo + a TP4056. The OLED is rotated 180° in firmware so it reads upright when hung. Regenerate renders with `render_previews.py`.

## Quick start
See **[docs/BUILDING.md](docs/BUILDING.md)**. TL;DR: Arduino games flash with `arduino-cli` (`esp32:esp32:esp32c3`); the Rust firmware builds with `cargo build --release --features espnow` and flashes with **espflash v3**. Run the site locally with `python3 site/server.py`.

## Board
The exact board: [ESP32-C3 SuperMini + 0.42" OLED (AliExpress)](https://www.aliexpress.us/item/3256807156068355.html).

---
*Built collaboratively with Claude Code — a fleet of agents did the research, flashing, CAD, and firmware while the build stayed in motion.*
