# smol

A whole game console — and a self-updating mesh, and a pocket watch — on a **$3 ESP32-C3 SuperMini with a 0.42" (72×40) OLED**.

It started as *"can we make this into a tiny game player? can it run Minecraft?"* Answer: **real Minecraft, no** (400 KB RAM vs. gigabytes) — **but the soul of it, yes.** From that joke it grew into something genuinely unusual: a fleet of $3–$9 boards running one **`no_std` Rust** firmware, talking to each other directly over **ESP-NOW** (no router, no cloud), that you can **update over the air with signed firmware** — even the WiFi-less nodes, over the mesh — and that hosts a **living creature which hops from board to board**. Games, a shared-world MMO, a native Home Assistant integration, remote config, observability, and OTA — all on a chip that costs less than a coffee.

## 🔮 The Mesh Familiar — the flagship

**One creature lives on the whole fleet.** It inhabits a single board at a time — showing its mood, hunger, and growth on that OLED — and when you **unplug the board it's on, it hops to a neighbour** over the mesh and carries on living there. Non-holder boards show a Weasley-clock pointer toward wherever the Familiar currently is; you can greet it, call it, and feed it. Exactly-one-holder arbitration, migration on loss, and orphan re-election are all handled in the mesh layer (`crate::familiar` + the `SMOLv1 FAM` frame). **Human-verified on glass** — pull the plug, watch it jump. *(#57 — merged, PR #99.)*

> A shared-world creature that migrates across $9 microcontrollers when you pull power is, as far as we know, one-of-a-kind for a `no_std`-Rust ESP-NOW fleet.

🌐 **Live site:** https://jphein.github.io/smol/ &nbsp;·&nbsp; 🕹️ Hardware-verified on real boards (the id7/id8/id9 bench fleet).

## What runs on it (the apps)

Every board runs the **unified Rust firmware** (`rust/clock/`, `no_std` esp-hal) — one binary, a BOOT-button menu, static plugin dispatch, no heap on the base build. The blue LED shows ESP-NOW peer state in the background (off → blink = detected → solid = connected).

| App | What | Status |
|---|---|---|
| **The Mesh Familiar** | a living creature that migrates across the fleet as boards come and go (see above) | 🟢 **on glass** — migration verified (#57) |
| **World Snake (MMO)** | shared 256×256 toroidal world over the mesh, scrolling viewport, peers drawn by name, mesh leaderboard, 6 **treasure-powers** | 🟢 flashed + running fleet-wide (#5) |
| **Marauder's Watch** | every node shows where every other node is, by **ESP-NOW roster RSSI** (near/far EWMA — no BLE) | ✅ merged (#58) |
| **Treasure Hunt** | RSSI warmer/colder game over the mesh | ✅ merged (#60) |
| **Custom screen** | per-node user-defined text/entities, authored from the HA dashboard (HA resolves `{entity}` refs to plain text; the leaf just renders bytes) | ✅ merged (#45) |
| **HA Batt / HA Grid** | live battery **voltages + SOC** (big per-battery pages) and **grid power** on every display, mirrored from Home Assistant over MQTT + re-broadcast to leaves as mesh frames | 🟢 on-glass round-trip verified (#16/#17) |
| **smol Cast** | stream a board's display to a network **WLED** matrix as realtime UDP pixels | 🟢 HW-verified (#26) |
| **Clock · Snake · Mesh Snake · Benchmark · atomic14 pack** | NTP clock, one-button Snake, 2-board head-to-head, a live ESP-NOW link tester, and 5 single-button games | 🟢 flashed |
| **Block Digger** | Minecraft-ish dig/build with a Bluetooth **Stadia** controller (Bluepad32; the Arduino build) | 🟢 flashed |

Mesh time-sync (loop-free, newest-NTP-wins) and **magical realm names** (id7 *Draconic Dominion*, id8 *Eldritch Nexus*, id9 *Jade Herald*, from [realm-sigil](https://github.com/jphein/realm-sigil)) run under all of it; the boot splash shows the sigil version name.

## The fleet: config, OTA, observability & mesh

This is where smol stops being a toy. The elected **gateway** briefly bursts onto WiFi to reach Home Assistant; the rest are **ESP-NOW-only leaves** the gateway serves.

- **Signed leaf-mesh-OTA (#40).** WiFi-less leaves update **over the mesh**: the gateway fetches an **ed25519-signed** image, relays it chunk-by-chunk over ESP-NOW (windowed-NAK), and the leaf **verifies the signature before it writes a byte**, then flashes into its inactive A/B slot with brick-safe rollback. A single **runtime-NVS node-id** image serves the whole fleet (identity lives in NVS, which OTA never touches). 🟢 hardware-proven — full ~1 MB images delivered over the mesh. Gateways still self-OTA over WiFi (canary-one-board, app-side rollback). *(builds on #6 OTA + #32 signing.)*
- **Keyed-CFG channel (#56) + the config quartet.** One `SMOLv1 CFG <id><KEY><value>` frame carries every per-node knob over the mesh: **LED** mode (#48, key `L`), **display units** °F/°C + 12/24h (#43, key `U`), **plugin visibility** per node (#55, key `P`), **remote reboot** (#52, key `R`, transient/never-retained), **Custom screen** layout (#45, key `Y`), **WiFi scan** trigger (#71, key `W`) — all editable from the HA dashboard, no reflash. ✅ merged.
- **Per-node observability (#70/#49/#74).** A retained DIAG record per node: uptime, boot-count, reset-reason, boot-slot, last-OTA-outcome, heap, flush/verify counters, link-quality + time-sync — so a silent rollback is visible in HA at a glance. ✅ merged.
- **On-demand WiFi scan (#71).** Each node can scan nearby APs on request and publish them to HA (on-demand only — never a periodic background scan that would go mesh-deaf). ✅ merged.
- **Mesh hardening.** Value-weighted ESP-NOW peer-table eviction → **~20-node capable** (#28); a channel fast-path so a leaf pre-tunes after a gateway roam (#29); single-gateway election validated across cascading-reboot / split-brain scenarios (#76). ✅ merged.
- **Reproducible builds (#44).** The release image is byte-reproducible for a fixed commit (path-remap + `SOURCE_DATE_EPOCH`), so an image's sha256 is a verifiable identity you can check against a board before/after a flash. ✅ merged.

## Repo layout
- `rust/clock/` — the **unified Rust firmware** (`no_std` esp-hal): apps + the ESP-NOW mesh (`src/net/`), the Familiar (`src/familiar/`), OTA (`src/ota.rs` + `src/ota_mesh.rs`), Cast (`src/net/cast.rs`)
- `blockdigger/`, `games/snake/`, `games/snake-2p/` — Arduino/C++ games (U8g2 + Bluepad32)
- `watch/` — Arduino smartwatch starter · `oled_test/` — I²C + display sanity check
- `experiments/` — `pocketwatch/` (3D-printable case generator + STLs), `atomic14-games/`, `nes-c3/`, `case-mod/`
- `ha/` — the Home Assistant integration (MQTT packages + dashboard) · `tools/` — OTA publish + reproducible-build + image-verify scripts
- `site/` — the editable project website (tiny Python server + WYSIWYG; auto-deploys to GitHub Pages)
- `docs/` — research + guides (below)

## Docs
- **[docs/ROADMAP.md](docs/ROADMAP.md)** — status + steering (start here)
- **[docs/BUILDING.md](docs/BUILDING.md)** — toolchain, flashing, pin map, the gotchas that cost us time
- **[docs/protocol.md](docs/protocol.md)** — the SMOLv1 wire reference (every frame, byte-accurate, with verification badges)
- **[docs/ota.md](docs/ota.md)** — OTA operator guide: stage/install, signing, canary, leaf mesh-OTA, reproducible builds
- **[docs/home-assistant.md](docs/home-assistant.md)** — the MQTT-native HA integration: Batt/Grid, node manager, why not ESPHome
- [docs/mesh-snake.md](docs/mesh-snake.md) · [docs/relay.md](docs/relay.md) — MMO player guide · relay/gateway operator guide
- [docs/firmware-ideas.md](docs/firmware-ideas.md) · [docs/gaming-firmware.md](docs/gaming-firmware.md) · [docs/nes-on-c3.md](docs/nes-on-c3.md) — the C3 landscape + retro-gaming builds
- [docs/power.md](docs/power.md) · [docs/sound.md](docs/sound.md) · [docs/wearables.md](docs/wearables.md) · [docs/enclosure-resin.md](docs/enclosure-resin.md) · [docs/le-audio.md](docs/le-audio.md) · [docs/board-repos.md](docs/board-repos.md) · [docs/cases.md](docs/cases.md)

## The pocket watch
`experiments/pocketwatch/` generates a parametric round case — **body + lid + crown** (3 printable STLs) — in the classic orientation: a chain **bail** and a removable **crown covering the USB-C port** at the top; the OLED, buttons and clear-PLA LED light-pipes below; pockets for the board + a 502030 LiPo + a TP4056. The OLED is rotated 180° in firmware so it reads upright when hung.

## Quick start
See **[docs/BUILDING.md](docs/BUILDING.md)**. TL;DR: Arduino games flash with `arduino-cli` (`esp32:esp32:esp32c3`); the Rust firmware builds with `cargo build --release --features espnow` and flashes with **espflash v3**. Run the site locally with `python3 site/server.py`.

## Board
The exact board: [ESP32-C3 SuperMini + 0.42" OLED (AliExpress)](https://www.aliexpress.us/item/3256807156068355.html).

---
*Built collaboratively with Claude Code — a fleet of agents did the research, flashing, CAD, and firmware while the build stayed in motion.*
