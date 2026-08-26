# smol

**A platform that fits in 400 KB.** One `no_std` Rust firmware, a registry of apps behind a single button, and a routerless mesh that elects its own gateway and updates itself over the air — now running across **five targets on four ESP32 chip families**.

It started on a **$3 ESP32-C3 SuperMini with a 0.42" (72×40) OLED**, as *"can we make this into a tiny game player? can it run Minecraft?"* Answer: **real Minecraft, no** (400 KB RAM vs. gigabytes) — **but the soul of it, yes.** From that joke it grew into something genuinely unusual: a fleet of $1–$3 boards running one binary, talking to each other directly over **ESP-NOW** (no router, no cloud), that you can **update over the air with signed firmware** — even the WiFi-less nodes, over the mesh — and that hosts a **living creature which hops from board to board**. Games, a shared-world MMO, a native Home Assistant integration, remote config, observability, and OTA — all on a chip that costs less than a coffee.

Then the platform outgrew the chip. The same source tree now compiles for **RISC-V and Xtensa**, drives a 72×40 monochrome OLED *and* a 2.8" colour touchscreen, and the boards on both ends of that range speak the same **SMOLv1** wire format to each other. See **[the five targets](#the-five-targets)**.

## 🔮 The Mesh Familiar — the flagship

**One creature lives on the whole fleet.** It inhabits a single board at a time — showing its mood, hunger, and growth on that OLED — and when you **unplug the board it's on, it hops to a neighbour** over the mesh and carries on living there. Non-holder boards show a Weasley-clock pointer toward wherever the Familiar currently is; you can greet it, call it, and feed it. Exactly-one-holder arbitration, migration on loss, and orphan re-election are all handled in the mesh layer (`crate::familiar` + the `SMOLv1 FAM` frame). **Human-verified on glass** — pull the plug, watch it jump. *(#57 — merged, PR #99.)*

> A shared-world creature that migrates across $3 microcontrollers when you pull power is, as far as we know, one-of-a-kind for a `no_std`-Rust ESP-NOW fleet.

🌐 **Live site:** https://jphein.github.io/smol/ &nbsp;·&nbsp; 🕹️ Hardware-verified on real boards (the bench fleet).

## The five targets

`targets/` **is** the roster — one folder per board, each with a machine-readable `target.toml` that declares its chip, its firmware flavor, and whether it produces a download. Adding a folder + manifest adds a target; nothing else has a list to maintain.

Two firmware **flavors** share the tree. The **fleet** flavor is `rust/clock/` — the `no_std` esp-hal binary this README is mostly about (apps, SMOLv1 mesh, signed OTA, election). The **GUI** flavor is `targets/c6-watch/` — a Slint/Embassy touch-UI workspace, subtree'd from the [esp32c6-watch](https://github.com/jphein/esp32c6-watch) repo, that speaks the same SMOLv1 frames and today carries three board arms (`board-waveshare-c6`, `board-cyd-c5`, `board-esp32s3-cyd`). Some boards run one flavor; the S3 runs both.

| target | chip | flavor | status |
|---|---|---|---|
| **[`c3`](targets/c3/)** — the headless node, ~$1 | ESP32-C3 · RISC-V | fleet | 🟢 **shipping** — the reference fleet. Every other board's numbers are measured against it. |
| **[`c3-oled`](targets/c3-oled/)** — the node with a face, ~$2.76 | ESP32-C3 · RISC-V | fleet | 🟢 **shipping** — *the same image* as `c3`; the 72×40 SSD1306 is simply answered at boot. |
| **[`s3-cyd`](targets/s3-cyd/)** — 2.8" ILI9341V + capacitive touch | ESP32-S3 · Xtensa | fleet **and** GUI | 🟢 **glass-verified (2026-08-26)** — boots, paints, takes touch, meshes, and reaches NTP + MQTT + Home Assistant, in *both* flavors, on real hardware. Not yet building in CI; the first A/B OTA roll is still ahead of it. |
| **[`c6-watch`](targets/c6-watch/)** — the Waveshare AMOLED smartwatch | ESP32-C6 · RISC-V | GUI | 🟢 **shipping in its own repo**, in-tree here as a full-history subtree. Live on the mesh; `rust/clock` compiles clean for the chip but does not yet link a fleet image for it. |
| **[`c5-cyd`](targets/c5-cyd/)** — the NM-CYD-C5 2.8" touch board | ESP32-C5 · RISC-V | GUI today | 🟡 **mesh-proven** — the first non-C3 silicon ever heard on smol's mesh (2026-08-24). For `rust/clock` it is **checks-only**: the source compiles for the chip, but there is no measured memory budget row and no linked image yet. Zigbee-bridge role back-burnered. |

**How to read those labels**, because the difference has cost real time here: **checks-only** means `cargo check` is clean for the chip — a real claim, and a much weaker one than *builds*. **Builds** means CI can produce a linked, budget-gated artifact. **Glass-verified** means a human watched it work on the bench. The declarations live in `tools/build-matrix.toml` (`checks` ⇐ `builds` ⇐ `ships`, enforced by a checker in both directions, so a stale pessimistic row fails the gate exactly like a stale optimistic one) and in `rust/clock/src/budget.rs`, and the two must agree or the build fails.

## What runs on it (the apps)

Every **fleet-flavor** board runs the **unified Rust firmware** (`rust/clock/`, `no_std` esp-hal) — one binary, a BOOT-button menu, static plugin dispatch, no heap on the base build. The blue LED shows ESP-NOW peer state in the background (off → blink = detected → solid = connected). Which plugins are present is a **build tier**, not a runtime setting, and the tiers stack: the cheapest board and the fleet gateway run the same code paths with a different slice compiled in. *(The touch boards' GUI flavor carries its own app set — see [`targets/c6-watch/README.md`](targets/c6-watch/README.md).)*

| App | What | Status |
|---|---|---|
| **The Mesh Familiar** | a living creature that migrates across the fleet as boards come and go (see above) | 🟢 **on glass** — migration verified (#57) |
| **The Bard** | a real transformer LLM (260K-param TinyStories, int8, XIP from flash) that **narrates without end, fully on-device** — a sliding-window KV cache means it just keeps going, typewriter-style on the 72×40 OLED. Only **5 lines × 14 chars** are on the glass at once, so the screen is a **window the story moves past**, which is what makes an unbounded tale possible on 400 KB of RAM. Every node has its own protagonist (id8 tells owl tales); the **opening** (CFG-`T`, #303) and the **pace + `inf`/`page` delivery** (CFG-`V`, #302) are settable per node from Home Assistant with no reflash | 🟢 **on glass** — #300: 67-token story, 202 ms/tok, port proven **bit-for-bit** vs an independent Python reference. #302: endless mode measured on id8 at **224–274 ms/tok** (max 281 — 10–35% slower than bounded, attention now spans the full window), `V` changed 160→120 ms/char **mid-narration** with no restart, and stack high-water **byte-identical at 55,440 B across 5 consecutive reports** spanning many tale boundaries — the window does not creep. #303 merged + HW-verified. **Fleet not rolled**, and `inf` is near-continuous compute so it is the worst-case power draw — no battery-life claim implied |
| **World Snake (MMO)** | shared 256×256 toroidal world over the mesh, scrolling viewport, peers drawn by name, mesh leaderboard, 6 **treasure-powers** | 🟢 flashed + running fleet-wide (#5) |
| **Marauder's Watch** | every node shows where every other node is, by **ESP-NOW roster RSSI** (near/far EWMA — no BLE) | ✅ merged (#58) |
| **Treasure Hunt** | RSSI warmer/colder game over the mesh | ✅ merged (#60) |
| **Custom screen** | per-node user-defined text/entities, authored from the HA dashboard (HA resolves `{entity}` refs to plain text; the leaf just renders bytes) | ✅ merged (#45) |
| **HA Batt / HA Grid** | live battery **voltages + SOC** (big per-battery pages) and **grid power** on every display, mirrored from Home Assistant over MQTT + re-broadcast to leaves as mesh frames | 🟢 on-glass round-trip verified **on the gateway** (#16/#17) · 🟡 the **leaf** leg is inferred, not observed — protocol.md logs no leaf-side BATT/GRID receipt, and the fleet is all-gateway so the path is unexercised |
| **smol Cast** | stream a board's display to a network **WLED** matrix as realtime UDP pixels | 🟢 HW-verified (#26) |
| **Clock · Snake · Mesh Snake · Benchmark · atomic14 pack** | NTP clock, one-button Snake, 2-board head-to-head, a live ESP-NOW link tester, and 5 single-button games | 🟢 flashed |
| **Block Digger** | Minecraft-ish dig/build with a Bluetooth **Stadia** controller (Bluepad32; the Arduino build) | 🟢 flashed |

Mesh time-sync (loop-free, newest-NTP-wins) and **magical realm names** — a pure function of the node id, so two boards agree on what to call each other with no handshake (via [realm-sigil](https://github.com/jphein/realm-sigil), corpus from [lexicon](https://github.com/jphein/lexicon.realm.watch)'s `fleet` group; **uniqueness across all 256 ids is a compile-time guarantee** — a colliding namespace does not build). The mapping is derivable; the id↔board *assignment* is not — see [BUILDING.md](docs/BUILDING.md) run under all of it; the boot splash shows the sigil version name.

## The fleet: config, OTA, observability & mesh

This is where smol stops being a toy. The elected **gateway** briefly bursts onto WiFi to reach Home Assistant; the rest are **ESP-NOW-only leaves** the gateway serves.

- **Signed leaf-mesh-OTA (#40).** WiFi-less leaves update **over the mesh**: the gateway fetches an **ed25519-signed** image, relays it chunk-by-chunk over ESP-NOW (windowed-NAK), and the leaf **verifies the signature before it writes a byte**, then flashes into its inactive A/B slot with brick-safe rollback. A single **runtime-NVS node-id** image serves the whole fleet (identity lives in NVS, which OTA never touches). 🟢 hardware-proven — full ~1 MB images delivered over the mesh. Gateways still self-OTA over WiFi (canary-one-board, app-side rollback). *(builds on #6 OTA + #32 signing.)*
- **Routed multi-hop mesh (#40's sibling — #13).** A leaf out of direct ESP-NOW range of the gateway is no longer stranded: it escalates to a **hop-limited managed flood** (Meshtastic-lineage — a hop-limit + an `(origin, msgid, frag)` seen-set, **table-free** so it rides roam/re-election for free) and its telemetry reaches home **through a neighbour**; the battery/grid downlink flows back the other way behind a strictly-newer freshness gate. Escalation is hysteretic (3 consecutive un-ACKed messages to latch, 2 direct-ACK probes to un-latch), so the ordinary all-hear case stays **byte-identical** to single-hop. 🟢 hardware-proven — **on 2026-07-14 a gateway-deaf smol delivered telemetry home through a neighbour: the first routed frame in smol's history.** Honest v1 limits: uplink ACK is best-effort and a stranded leaf's channel-scanning caps throughput. Both honest-v1 follow-ups have since closed: #126 (latched-leaf channel parking) and #124 (the UP2 observability envelope). *(builds on the #76 election.)*
- **Keyed-CFG channel (#56) — the whole config family.** One `SMOLv1 CFG <id><KEY><value>` frame carries every per-node knob over the mesh, all editable from the HA dashboard, no reflash: **default screen** (#21, key `S`), **LED** mode (#48, `L`), **display units** °F/°C + 12/24h (#43, `U`), **plugin visibility** (#55, `P`), **Custom screen** layout (#45, `Y`), **broker** override (#100, `B`), **OTA-host** override (#100, `O`), **remote reboot** (#52, `R`, transient), **WiFi scan** trigger (#71, `W`, transient), the **IO pin-map + output control** (#72, keys `G`/`g` — wire a digital sensor/button/relay/LED to any free GPIO and drive it, all from HA), and the **Bard's story prompt** (#303, `T` — validated leaf-side against the model's own 512-token vocabulary). Most apply live; `B` edge-triggers a reboot; `R`/`W` are one-shot and never cached. ✅ merged. *(The `N` WiFi-slot switch shipped with #100 and was **retired by #142** — the fleet is single-network now and a received `N` is drained and ignored.)*
- **The dashboard discovers the fleet itself.** The HA Control Room no longer carries a hand-written list of nodes: it reads **Home Assistant's device registry**, which the *firmware* populates — every node publishes retained MQTT-discovery configs whose device block carries `identifiers:["smol<id>"]`, `name:"smol <id> <Noun>"` and `sw_version`. **That makes the registry a self-reported fleet manifest** — ids, sigil names and running builds, with no list to maintain anywhere. Plug in a board and it appears; re-provision one and it renames itself. The sigil is *read* from the device name rather than recomputed, deliberately: the naming formula already changed once (#218) and stranded four names in the docs. ✅ merged.
- **Runtime IO registry (#72).** ESPHome inverted: one image holds every digital driver, and a per-node pin-map relayed over CFG-`G` binds a **button, contact, relay or LED** to any of the free GPIOs (`0/1/3/7/10`) — no reflash, no NVS wear — with output states held across reboot via CFG-`g`. This is the **dollhouse per-room lamp + button foundation** (#75). ✅ merged.
- **Runtime networking overrides (#100 → #142).** Point a node at a different **MQTT broker leg** or **OTA-image host** from the HA dashboard, persisted in a 28-byte NVS net record, with no reflash. The OTA-host override can only ever *add* one RFC1918 host to the fetch allowlist, so a bad value refuses a fetch rather than bricking a board. ✅ merged (Stage 1b–3). **Honest correction:** the original #100 also shipped a **dual-slot WiFi switch with an un-brickable auto-revert** — **#142 retired it** in favour of single-network operation (CFG-`N` is drained and ignored; the net record's slot/fallback bytes are reserved-zero). Don't cite the slot switch or its auto-revert as a current feature.
- **Per-node observability (#70/#49/#74).** A retained DIAG record per node: uptime, boot-count, reset-reason, boot-slot, last-OTA-outcome, heap, flush/verify counters, link-quality, time-sync, **network state** (`net=`/`brk=`/`otah=`), and the **applied-config echo** (`cfg=`) that HA compares against its command topics to flag **config drift** — the **display-mirror** (the gateway OLED as a live HA image), and **bound-input press counters** (`io=`, #72) — so a silent rollback, a drifted node, or a doll's button push is visible in HA at a glance. ✅ merged.
- **On-demand WiFi scan (#71).** Each node can scan nearby APs on request and publish them to HA (on-demand only — never a periodic background scan that would go mesh-deaf). ✅ merged.
- **Mesh hardening.** Value-weighted ESP-NOW peer-table eviction → **~20-node capable** (#28); a channel fast-path so a leaf pre-tunes after a gateway roam (#29); single-gateway election validated across cascading-reboot / split-brain scenarios (#76), with crown-handover-standoff + seq-race heals (#114). ✅ merged.
- **Reproducible builds (#44).** The release image is byte-reproducible for a fixed commit (path-remap + `SOURCE_DATE_EPOCH`), so an image's sha256 is a verifiable identity you can check against a board before/after a flash. ✅ merged. Note the scope: shas are comparable **within one (chip, profile) pair** — the S3 builds at a different opt-level to work around an LLVM issue, and that legitimately produces different (equally correct) bytes.
- **The Embassy re-platform (#335) — phase 1 merged.** The fleet tier now runs under the **`esp-rtos` async executor** beside the superloop: `main` is `#[esp_rtos::main] async fn`, `embassy-time`'s driver comes from `esp-rtos`, and evidence-bearing radio sends have learned bounded async waits instead of spinning on a callback that may never fire. The motivation is a measurement, not an aesthetic: on a two-board bench, a held WiFi window that left the mesh **deaf for ~15 s** under the blocking transport left it deaf for **169 ms** under the async one — ~89×, against a 279 ms ambient floor. ⚠️ **Read the scope of that number honestly:** it was taken during the port campaign's Phase 2, with less running than the fleet image carries, and re-running the harness against the merged tree is still an open item. 🟢 merged (PR #391). **Step T** — moving the seven `(controller, station)` transport pairs to one declared owner — is scoped in [`docs/embassy/T-SCOPE.md`](docs/embassy/T-SCOPE.md) and **in progress**, not landed.

## Repo layout
- `rust/clock/` — the **unified Rust firmware** (`no_std` esp-hal, the *fleet* flavor): apps + the ESP-NOW mesh (`src/net/`), the Familiar (`src/familiar/`), the Bard (`src/bard/`), OTA (`src/ota.rs` + `src/ota_mesh.rs`), Cast (`src/net/cast.rs`), the per-chip memory budgets (`src/budget.rs`)
- `targets/` — **the target roster and the artifact matrix**: one folder per board, each with a `target.toml` manifest. `targets/c6-watch/` is a subtree of the [esp32c6-watch](https://github.com/jphein/esp32c6-watch) repo and carries the *GUI* flavor's whole workspace
- `blockdigger/`, `games/snake/`, `games/snake-2p/` — Arduino/C++ games (U8g2 + Bluepad32)
- `watch/` — Arduino smartwatch starter · `oled_test/` — I²C + display sanity check
- `experiments/` — `pocketwatch/` (3D-printable case generator + STLs), `atomic14-games/`, `nes-c3/`, `case-mod/`
- `ha/` — the Home Assistant integration (MQTT packages + dashboard) · `tools/` — OTA publish + reproducible-build + image-verify scripts
- `site/` — the editable project website (tiny Python server + WYSIWYG; auto-deploys to GitHub Pages)
- `docs/` — research + guides (below)

## Docs
- **[docs/ROADMAP.md](docs/ROADMAP.md)** — status + steering (start here)
- **[docs/DOC-UPKEEP.md](docs/DOC-UPKEEP.md)** — how these docs and the website are kept true: where truth lives, how to verify a claim, the traps
- **[docs/BUILDING.md](docs/BUILDING.md)** — toolchain, flashing, pin map, the gotchas that cost us time
- **[docs/protocol.md](docs/protocol.md)** — the SMOLv1 wire reference (every frame, byte-accurate, with verification badges) — including the allocated **node-id blocks** per board class
- **[docs/RELEASES.md](docs/RELEASES.md)** — what is published and what it is *for*: nightlies vs versioned releases, **per-target downloads**, the placeholder-credential rule and how to re-key
- **[docs/ota.md](docs/ota.md)** — OTA operator guide: stage/install, signing, canary, leaf mesh-OTA, reproducible builds
- **[docs/embassy/](docs/embassy/)** — the async re-platform campaign: the port spec, the delta map, the risk register, and `T-SCOPE.md`
- **[docs/home-assistant.md](docs/home-assistant.md)** — the MQTT-native HA integration: Batt/Grid, node manager, why not ESPHome
- [docs/mesh-snake.md](docs/mesh-snake.md) · [docs/relay.md](docs/relay.md) — MMO player guide · relay/gateway operator guide
- [docs/firmware-ideas.md](docs/firmware-ideas.md) · [docs/gaming-firmware.md](docs/gaming-firmware.md) · [docs/nes-on-c3.md](docs/nes-on-c3.md) — the C3 landscape + retro-gaming builds
- [docs/power.md](docs/power.md) · [docs/sound.md](docs/sound.md) · [docs/wearables.md](docs/wearables.md) · [docs/enclosure-resin.md](docs/enclosure-resin.md) · [docs/le-audio.md](docs/le-audio.md) · [docs/board-repos.md](docs/board-repos.md) · [docs/cases.md](docs/cases.md)

## The pocket watch
`experiments/pocketwatch/` generates a parametric round case — **body + lid + crown** (3 printable STLs) — in the classic orientation: a chain **bail** and a removable **crown covering the USB-C port** at the top; the OLED, buttons and clear-PLA LED light-pipes below; pockets for the board + a 502030 LiPo + a TP4056. The OLED is rotated 180° in firmware so it reads upright when hung.

## Quick start

### Put smol on a bare board

Prebuilt images live on the [releases page](https://github.com/jphein/smol/releases). Today that is the C3 fleet image from `nightly-2026-08-24`; **per-target downloads are landing now** (#413) — `tools/release_targets.sh` walks the `targets/*/target.toml` manifests and packages every target that declares `artifact = true`, through the same reproducible-build calls the OTA publish path uses. Each artifact ships a `NOTES.md` beside it carrying its chip's **stack-floor provenance** in plain words and the (chip, profile) sha-lineage rule.

```bash
sha256sum -c SHA256SUMS
# ⚠️ If this board has EVER taken an OTA, clear otadata first, or the flash silently
#    lands in the slot the bootloader will not select. This spares nvs (and your node id).
espflash erase-region --port /dev/ttyACM0 0xf000 0x2000
espflash write-bin --port /dev/ttyACM0 0x0 <image>.bin
```

Then check the `Loaded app from offset` line — it names the slot that actually ran.

> ### 🔑 Two rules that surprise everyone
>
> **1. Downloads are for NEW hardware. Fleet boards update over signed mesh OTA, never from GitHub.** The ed25519-signed `smol/ota/staged` path is the only sanctioned way a deployed board changes firmware.
>
> **2. Published images carry PLACEHOLDER credentials — including the mesh group key — on purpose,** so the artifact stays byte-reproducible. A downloaded board boots, drives its display and runs the menu, the games and the Bard; it will **not** join WiFi, reach a broker, or talk to an existing mesh. And a key baked into a public binary *is public*: two boards flashed from the same download share a mesh key anyone can extract. **To own your mesh, re-key** — regenerate `GROUP_KEY` in `rust/clock/src/secrets.rs` (32 random bytes, start from `secrets.rs.example`), rebuild, reflash. Placeholder-key boards can mesh only with other placeholder-key boards, and can never join a re-keyed fleet. *(#394.)*
>
> Full detail: **[docs/RELEASES.md](docs/RELEASES.md)**.

### Build it yourself
See **[docs/BUILDING.md](docs/BUILDING.md)**. TL;DR: the Rust firmware builds with `cargo build --release --features espnow` (exactly one chip feature per invocation — the triple and the features are chosen together) and flashes with **espflash**; Arduino games flash with `arduino-cli` (`esp32:esp32:esp32c3`). The Xtensa S3 arm needs the espup `esp` toolchain. Run the site locally with `python3 site/server.py`.

## Boards
The original: [ESP32-C3 SuperMini + 0.42" OLED (AliExpress)](https://www.aliexpress.us/item/3256807156068355.html) — and four more in [`targets/`](targets/), each folder documenting its own hardware truth.

---
*Built collaboratively with Claude Code — a fleet of agents did the research, flashing, CAD, and firmware while the build stayed in motion.*
