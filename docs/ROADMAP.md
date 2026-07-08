# smol — roadmap + decision docket

The steering doc: what's **shipped**, what's **in flight**, what's **spec'd** and ready to
build, what's been **researched** (go/no-go), and the open decisions. Companion to the
GitHub tracking issue [#24](https://github.com/jphein/smol/issues/24) (this is the
in-repo narrative version; the issue is the living checklist).

**Honesty rule:** *shipped* means hardware-verified on the id7/8/9 fleet (current build
**45 "Oxidized Die"**); nothing here is overstated. Verification legend: 🟢 hardware-verified
· 🟡 compile/spec-verified, not fully exercised on hardware · ⚪ design only.

---

## 1. 🟢 SHIPPED — on the fleet

| What | Issue | Evidence |
|---|---|---|
| **MQTT-native display link** — collector retired; nodes ↔ HA directly over MQTT (retained downlink + discovery uplink) | #10/#11/#15 | Full leaf→gateway→MQTT→HA path proven on id7/8/9; commits `96f44d5`, `bb5092a` |
| **Batt screen + 6-segment SOC pages** — retained `smol/display/batt`; voltage overview + big per-battery SOC/charge detail pages (short-press to page) | #16/#17 | Both payloads cached on all 3 boards; big pages render on glass; commits `96f44d5`/`f6d56d2`/`b7fd71a` |
| **Grid screen** — retained `smol/display/grid` (yurt total + two phase clamps, watts) + `SMOLv1 GRID` mesh frame | #16 | Live HA mirror `sensor.smol_display_grid`; on-glass verified |
| **Default screen at boot** — compile-time `DEFAULT_APP`/`DEFAULT_PAGE` one-shot (long-press always escapes) | #18 | Default build byte-identical (const-false DCE); verified |
| **Per-board config file** — `NODE_ID`/`DEFAULT_APP`/`DEFAULT_PAGE` in a git-ignored `board.rs` (kills the per-board version-sigil "dirty" wart) | #19 | Committed `b7fd71a`; `board.rs.example` in tree |
| **UI responsive during WiFi sync** — defer-while-interacting + long-press abort + "Syncing…" spinner | #20 | Review CLEAN; six build/clippy gates green; commit `0ce1ce9` |
| **HA availability** — discovery `expire_after` so a node goes unavailable after several missed bursts | #12 (fw half) | Live in discovery JSON |
| **Node manager — HA publish/GUI half** — Lovelace + `input_select`/automations publishing retained `smol/<id>/config/default_screen`; mirror sensors | #21 (HA half) | Deployed live to HA (config topics left empty until the firmware consumes them) |
| **EPEver cloud-logger contained** (homelab infra) — the PE11 DIN converter was a hidden Hi-Flying cloud datalogger acting as a 2nd Modbus master; firewalled at the gateway | — | Bus corruption cut; the Batt SOC is sourced from the BMS, not EPEver |

---

## 2. 🟡 IN FLIGHT / NEXT WAVE

- **Node manager — firmware consume half** (#21). The HA half is live; the firmware side is
  three small sub-tasks that ride one wave: (1) SUBSCRIBE `smol/<id>/config/default_screen`
  + a **strict, panic-free** allowlist parse (a garbage retained payload that panics =
  boot-loop brick), reconcile per-node over the `board.rs` default, apply at the boot
  one-shot; (2) publish a retained `smol/<id>/mesh` roster (topology data — already computed
  for the Bench mesh-view); (3) publish a retained `smol/<id>/status` = `STAT|<screen>:<page>|<build>`
  (unlocks live current-screen reflection **and** the running-build read OTA needs).

---

## 3. 🟡 SPEC'D — ready to build

### 3a. OTA firmware updates (#6) — feasibility RESOLVED
The flashed image is ~590–620 KB ≈ **30 % of a 1.94 MB slot → ~3.3× headroom**; dual A/B +
otadata fits 4 MB with zero waste, hardware-validated (bootloader honors otadata; CRC
convention nailed). Build waves (~4–6 d): deps + `partitions-ota.csv` + `esp_app_desc!()` →
slot plumbing → OTA WiFi burst (HTTP GET → stream ≤4 KB chunks to the inactive slot →
running SHA-256 → verify) → activate + rollback + first-boot confirm → retained
`smol/ota/announce` parse → About-screen "update available / hold to install." Plus the LAN
image server + a publish script.

> ⚠️ **The gate:** a *broken* Rust app **cannot self-revert** — only the 2nd-stage
> bootloader can, and only if built with rollback enabled **and** a boot-fail actually
> resets. The hardware spike proved otadata *slot-selection*, **not** revert-on-boot-fail.
> Prove that on hardware before any unattended fleet-wide OTA; until then,
> **canary-one-board-at-a-time** is the only mass-brick defense.

### 3b. Node manager firmware + mesh console (#21) — protocol LOCKED
The three firmware sub-tasks in §2, then the GUI's remaining cards: mesh-topology
(picture-elements) + OTA panel (canary-then-rest). The command protocol + security contract
are locked; the wire is documented in [protocol.md](protocol.md#config--retained-per-node-default-screen-21-specd--firmware-pending)
and [home-assistant.md](home-assistant.md).

---

## 4. ⚪ RESEARCHED — go/no-go (nothing built)

- **4a. Retire the burst — WiFi + ESP-NOW co-channel coexist (biggest payoff, cheapest).**
  smol's ~15 s mesh-deaf flush window is a *conservative choice*, not a hardware limit — the
  firmware already builds a WiFi+ESP-NOW coexist arm. Staying associated + running mesh RX +
  MQTT concurrently on the AP channel would remove the deaf window and the boot assoc-freeze,
  collapsing a flush to a sub-second round-trip and making much of #20 moot. Zero new deps.
  Gated on (network) pinning the AP to the mesh channel (ch6) and (the one HW unknown) a soak
  test of coexist ESP-NOW RX reliability while associated — **the gating experiment.**
- **4b. BLE beacon + presence (#22).** Advertise-only iBeacon (be tracked): **YES**, cheap,
  room-level presence via external fixed anchors → Bermuda (HACS). Metric positioning / full
  BT-proxy / boards-track-themselves: **NO** (single radio + the multi-second WiFi hold
  preclude it; room-level is the honest ceiling). Its own small HW spike; shares the coexist
  gate.
- **4c. Multi-hop (#13) + self-healing gateway re-election (#14).** smol is single-hop-relay
  today (covers the 3-board star). Routed multi-hop + runtime re-election are future work;
  prior art exists (ZHNetwork does routed multi-hop ESP-NOW→MQTT→HA). **Defer** behind
  coexist + OTA.
- **4d. ESPHome / WLED lessons (#12 polish).** No Rust ESPHome firmware exists and the native
  API fights the burst model — **stay on MQTT** (proven strictly better on fit/effort/reuse).
  Steal from WLED (cheap, high-legibility): put every entity under **one HA device** `smol
  <id>`; split the single telemetry text line into **typed** discovery entities
  (`_voltage`/`_soc`/`_rssi`/`_role`); keep `expire_after` (NOT WLED's LWT-offline — it'd flap
  a healthy burst node offline every ~30 s). See [home-assistant.md](home-assistant.md).
  *Honest novelty framing:* the ESP-NOW→MQTT→HA substrate is commodity; smol's whole — a
  no_std Rust game-console mesh + single-radio burst time-share + retained→mesh-rebroadcast
  downlink to display-only leaves — is one-of-a-kind.

---

## 5. 🔵 DECISION DOCKET

Open decisions, ordered by leverage. **Recommendations, not decisions** — tick as JP
resolves them. **D1 is the fork that changes everything downstream — decide it first;**
D2–D5 are the OTA safety envelope; D6–D9 node-manager polish; D10–D12 new-capability go/no-go.

- [ ] **D1 — Coexist HW spike: retire the burst?** (§4a) · *Recommend GO — highest leverage,
  ~zero cost:* flash a `wifi+esp-now` co-channel build, pin the AP to ch6, measure mesh RX
  loss + flush latency. If green it reshapes #20 and every future burst. **Do this first.**
- [ ] **D2 — OTA fleet-wide: enable when?** (§3a) · *Run the bootloader-revert hardware test
  before ANY unattended fleet OTA. Until green: canary-one-board-only. Never fleet-flash blind.*
- [ ] **D3 — OTA authenticity** · *B (documented accepted-risk) for the isolated home LAN now
  — OTA authority == broker write, stated plainly; A (ed25519 image signing) if the broker is
  ever shared / internet-adjacent. Code must never imply sha256 = trust.*
- [ ] **D4 — OTA rollout targeting** · *Per-id `smol/<id>/ota/announce` → canary → roll out.
  Never unison (mass-brick risk while D2 is unproven).*
- [ ] **D5 — OTA physical long-press to accept** · *Arm-then-confirm — one long-press at the
  glass defeats remote mass-flash entirely; costs one screen. (`OTA_AUTO` compile flag for
  hands-off if ever wanted.)*
- [ ] **D6 — Node-manager config reach** · *All-gateway if you want all 3 settable from HA
  (all boards carry creds → all read MQTT config); otherwise leaves stay USB-config — honest,
  secure, MQTT-only, no unauth mesh command channel. (The fleet is currently all-gateway.)*
- [ ] **D7 — Node-manager apply semantics** · *Live-switch-if-on-default — idle boards flip on
  "set all", user-navigated screens defer; long-press→Menu always escapes (verified safe & reversible).*
- [ ] **D8 — Publish `smol/<id>/status`?** · *YES — one small retained publish unlocks live
  current-screen reflection AND the running-build read OTA no-downgrade needs. Best effort:payoff on the board.*
- [ ] **D9 — Mesh-topology render** · *picture-elements v1 (vanilla Lovelace, fine for a fixed
  3-board star); upgrade to a custom HACS card or a `site/` SVG mirror later for a dynamic graph.*
- [ ] **D10 — BLE beacon (#22)** · *Advertise-only GO as a small HW spike (room-level presence
  via external anchors). Full BT-proxy NO (single-radio precludes it). Shares the D1 coexist gate.*
- [ ] **D11 — Structured HA entities + device grouping (#12)** · *Split the telemetry line into
  typed `_voltage`/`_soc`/`_rssi`/`_role` under one `smol <id>` device. Cheap WLED-lesson win.*
- [ ] **D12 — Multi-hop #13 + self-healing #14** · *Defer behind coexist + OTA — single-hop
  covers the 3-board fleet; revisit if the mesh grows past one hop.*

---

*Statuses verified against the live tree (`git log`) + hardware findings, not asserted. The
byte-level wire contracts live in [protocol.md](protocol.md); the HA integration in
[home-assistant.md](home-assistant.md) + [`ha/README.md`](../ha/README.md).*
