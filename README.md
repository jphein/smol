# smol

A whole game console — and a pocket watch — on a **$3 ESP32-C3 SuperMini with a 0.42" (72×40) OLED**.

It started as *"can we make this into a tiny game player? can it run Minecraft?"* Answer: **real Minecraft, no** (400 KB RAM vs. gigabytes) — **but the soul of it, yes.** From there it grew into a little platform: multiple games, a unified Rust firmware with a menu, an ESP-NOW mesh (now with **time sync** and **magical node names**), a 3D-printable pocket-watch case, and a self-hosted project site. In flight: an **ESP-NOW→internet relay** and a shared-world **MMO snake**.

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
| **Mesh time sync** | boards agree on the clock over ESP-NOW; newest NTP sync wins (loop-free) | in tree · **2-board adoption verified ✓** (2026-07-07) |
| **Magical names** | every board shows a **realm-sigil** fantasy name (id7 *Draconic Dominion*, id8 *Eldritch Nexus*) | in tree · compile-verified |
| **Relay bridge** | single-hop ESP-NOW→WiFi telemetry to the internet (not browsing — 250 B MTU) | in progress (issue #3) |
| **World Snake (MMO)** | shared 256×256 world over the mesh, scrolling viewport, peers by name | design (issue #5) |

The **unified Rust firmware** (`rust/clock/`, `no_std` esp-hal) ties Clock + Snake + Bench into **one binary** with a BOOT-button menu; the blue LED shows ESP-NOW peer state (off → blink = detected → solid = connected) in the background across all modes. Verified: two boards go **solid blue = connected**.

The mesh now also **shares the time** (a `SMOLv1 TIME` frame; a board adopts a peer's time only when the peer's NTP sync is more recent, so it's loop-free) and gives every board a **magical realm name** ported from [realm-sigil](https://github.com/jphein/realm-sigil) (display-only — id7 *Draconic Dominion*, id8 *Eldritch Nexus*, id9 *Jade Herald*). The WiFi/NTP **root** board — id 7 *Draconic Dominion* (MAC `ac:a7:04:b9:77:14`) — is **flashed** with both today (compile-verified, `clippy -D warnings` clean across all 3 builds); id 8 *Eldritch Nexus* (`…ba:1f:24`) then **adopted its mesh time over ESP-NOW** — 2-board time-adoption **verified 2026-07-07** (a third board is pending a data cable). A single-hop **ESP-NOW→internet relay** (issue #3) and a shared-world **MMO snake** (issue #5) are in progress/design — see `docs/` and the scratch notes for the netcode and gateway analysis.

## Repo layout
- `blockdigger/`, `games/snake/`, `games/snake-2p/` — Arduino/C++ games (U8g2 + Bluepad32)
- `rust/clock/` — the unified Rust firmware (Clock / Snake / Bench, `no_std` esp-hal); the ESP-NOW mesh lives in `src/net/` — WiFi/SNTP, time sync, magical names (realm-sigil), relay bridge (in progress)
- `watch/` — Arduino smartwatch starter (NTP + weather; BLE-notification path stubbed)
- `oled_test/` — I²C + display sanity check
- `experiments/` — `pocketwatch/` (3D-printable case generator + STLs), `atomic14-games/`, `nes-c3/` (emulator base), `case-mod/`
- `site/` — the editable project website (tiny Python server + WYSIWYG; auto-deploys to GitHub Pages)
- `docs/` — research + guides (below)

## Docs
- **[docs/BUILDING.md](docs/BUILDING.md)** — toolchain, flashing, pin map, and the gotchas that cost us time
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
