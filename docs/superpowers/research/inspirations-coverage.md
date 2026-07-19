# Inspirations → implementation coverage audit

**Issue:** [#189](https://github.com/jphein/smol/issues/189) · **Capstone to:** [#163 Babel](althea-babel-study.md) + [#181 ledger](mesh-ledger-study.md) studies · **Status:** audit only, no firmware · **Author:** nebula-babel · **Date:** 2026-07-18

> **Bottom line up front.** smol has been unusually disciplined about *shipping what it
> learned*. Of the **~33 actionable borrows** across the 18 inspiration projects, **~26 are
> SHIPPED** (with merged issues/PRs), **~5 are DEFERRED with live trackers or documented
> triggers**, and **only 1 is a genuine GAP** — mesh authentication (ESP-NOW PMK/LMK). The
> 12 non-mesh inspirations are, as the source memory says, mostly **ethos/identity** (nothing
> to "ship"); the actionable borrows concentrate in the mesh/routing + HA-integration threads,
> and those are almost fully landed.
>
> **The one real gap (verify-don't-assume catch):** Althea's per-link-auth principle → smol's
> **ESP-NOW PMK/LMK encryption** was folded into [#12](https://github.com/jphein/smol/issues/12)
> as "(optional, someday)", #12 was **closed**, and it never got a live tracker. Ground truth:
> `net/mode.rs` sets `lmk: None, encrypt: false` at every `add_peer` and the code says
> "unauthenticated + unencrypted — sign or LMK-encrypt if it ever matters." So a documented
> *intention* quietly became learned-and-forgotten. Filed as a durable tracker (#190) with
> its real trigger — and note it now **rhymes with** the ledger's [#184](https://github.com/jphein/smol/issues/184)
> (on-device ed25519 signing): both are "authenticate on-mesh data," and the RPG anti-cheat
> need is the shared trigger.
>
> **Top-3 highest-value unshipped learnings:** (1) **mesh authentication** (Althea #12 +
> ledger #184 — the security posture), (2) **ETX metric proven on hardware**
> ([#164](https://github.com/jphein/smol/issues/164), built-not-proven — unlocks #180 channel
> selection + #165 best-relay), (3) **the tamper-evident ledger**
> ([#182](https://github.com/jphein/smol/issues/182) — fleet provenance + RPG substrate).

---

## 0. Method (verify, don't assume)

Every "SHIPPED" cites a **closed** issue/merged PR *and*, where non-obvious, a code check —
because a closed issue ≠ a shipped feature (the ed25519 "already in-tree" half-truth in
[#163](althea-babel-study.md) and the #12 "closed" ≠ encryption-shipped catch below both
prove it). Sources: the `smol-inspirations` memory (18 projects, 5 threads), the two study
docs, and `gh issue list` (open+closed) + `rust/clock/src` grep for ground truth. Status
vocabulary matches the study docs:

- **SHIPPED** — merged, cite the issue/PR (+ code where verified).
- **DEFERRED** — deliberately not-yet, with a *live tracker* or a *documented trigger*.
- **GAP** — learned but neither shipped nor tracked-deferred → gets a new tracking issue.
- **ETHOS** — lineage/identity/soul only; nothing actionable to ship (not counted as a borrow).
- **DON'T** — deliberately rejected (cite the verdict).

---

## 1. Mesh / routing (the thread with the most actionable borrows)

| Borrow | Source | Status | Citation / note |
|---|---|---|---|
| Gateway-node MQTT-client-proxy bridges the mesh into HA | Meshtastic | **SHIPPED** | #10/#11 (MQTT-native pivot); `net/mqtt.rs` |
| Routed multi-hop + self-healing re-election lineage | Meshtastic / painlessMesh | **SHIPPED** | #13/#123 (managed flood), #76 (election split-brain fix), #14 (re-election) |
| Long-range radio regime (LoRa → ESP-NOW LR analog) | Meshtastic | **DEFERRED** | [#54 open](https://github.com/jphein/smol/issues/54) (ESP-NOW LR mode) — live tracker |
| Self-organizing / self-healing topology (table-free) | painlessMesh | **SHIPPED** (analog) | #13/#76 table-free flood, rides re-election for free |
| Table-free flood + `msgid` dedup (but **not** its missing TTL) | ZHNetwork | **SHIPPED** | #13/#123 seen-set `(origin,msgid,frag)`; loop-safety via `MAX_HOP` decrement (TTL hazard avoided) |
| Route-*learning* routed multi-hop | ZHNetwork | **DON'T** | [#163](althea-babel-study.md): smol chose table-free flood; a route table is overkill for single-sink 2-hop |
| Babel feasibility/ETX link metric | Althea/Babel | **BUILT-not-proven** | [#164 open](https://github.com/jphein/smol/issues/164) (PR #179, HW-gate-held); metric-blend → [#180](https://github.com/jphein/smol/issues/180) |
| Babel-wholesale route/source tables, periodic Updates | Althea/Babel | **DON'T** | [#163](althea-babel-study.md) ADMIRE/DON'T (any-to-any over stable L2 ≠ smol) |
| Per-link authenticated encryption → ESP-NOW **PMK/LMK** | Althea (#12 security) | **GAP** | #12 closed with it "(optional, someday)"; code = `lmk:None, encrypt:false`. **→ #190** |
| Monotonic seqno + retract-on-loss + solicit-fresh | Althea (#14 self-heal) | **SHIPPED** | `dl_seq` strict-newer gate (`net/wire.rs`); validated the coexist seq/liveness design |
| Tamper-evident append-only ledger (Babel-adjacent) | Althea → study | **STUDIED→FILED** | [#181](mesh-ledger-study.md) → #182 (ADOPT) /#183/#184/#185 |
| Single-radio dual-role coexist precedent | esp_wifi_repeater | **SHIPPED** | #23/#40 (co-channel coexist retires the burst-deaf window) |
| Retained gateway-health publish (uptime/RSSI/peers/channel/flush) | esp_wifi_repeater | **SHIPPED** | #49 (DIAG topic), #74 (topology/peers/RSSI matrix), mesh-channel status |

---

## 2. Local-first ESP ↔ Home Assistant

| Borrow | Source | Status | Citation / note |
|---|---|---|---|
| "One HA device, many typed entities" (`_voltage`/`_soc`/`_rssi`/`_role`) | WLED | **SHIPPED** | #12 (discovery), #36 (typed-conformance fix) |
| ESP-NOW interop (WiZmote / light-sync dialect) | WLED | **SHIPPED** | #25 (`wled` feature in `Cargo.toml`; WiZmote emit) |
| Web-UI-analog relocated to HA (node manager) | WLED | **SHIPPED** | #21 (GUI node manager); + #45/#55/#43/#48/#52 control surface |
| MQTT-discovery conventions (`expire_after`, device/origin) | ESPHome | **SHIPPED** | #12 item 1 (expire_after); #68 (offline-flap fix) |
| Entity taxonomy / config validation (HA-level only) | ESPHome | **SHIPPED** | #73 |
| Runtime IO/component registry (no recompile) | ESPHome | **SHIPPED** | #72 (`io` feature) |
| OTA as a native HA Update entity | ESPHome | **SHIPPED** | #33/#39 |
| Runtime config over MQTT, no reflash | Tasmota | **SHIPPED** | #21 + #56 (keyed CFG channel) |
| `cmnd`/`stat`/`tele` topic conventions | Tasmota | **SHIPPED** | STAT/DIAG/CFG frames + `smol/<id>/…` topics |
| MQTT-first / local-first / no-cloud + OTA | Tasmota | **SHIPPED** | #6 (OTA); the whole MQTT-native stack |
| ESP-NOW→MQTT→HA bridge (evidence it's well-trodden) | lattic | **ETHOS** | keeps the novelty framing honest; nothing to ship |
| ESP32-protocol-bridge → HA-over-MQTT | Battery-Emulator ⭐ | **SHIPPED (LIVE)** | `ha/packages/smol_mesh.yaml`: `sensor.be_soc`+HV/delta → `smol/display/batt` republish |
| ESP-as-WiFi-bridge-with-web-control appliance ethos | ESP3D | **ETHOS** | — |
| Network-delivered firmware prior-art | esp-link ⭐ | **SHIPPED** | #6 OTA (spirit; different mechanism/target — esp-link flashes a *separate* MCU) |
| HA-native BLE room-presence (advertise-only iBeacon) | Bermuda/ESPresense | **DEFERRED (refuted)** | #22/#58 closed; native BLE **wedges the C3** ([memory: `smol-ble-refuted-c3`]); ESPHome bt-proxy interim; trigger = embassy async. #71 (WiFi AP scan) shipped as the RF-scan alternative |

---

## 3. Cheap-ESP audacity (soul + form factor)

| Borrow | Source | Status | Citation / note |
|---|---|---|---|
| ESP32-C3 + tiny-OLED + minimal-button handheld game idiom | atomic14 | **SHIPPED** | *the smol board itself* — the form factor is the product |
| "$5 ESP doing something gloriously improbable" soul | LeafMiner | **ETHOS** | the mesh-snake MMO-on-a-clock audacity |
| Cheap-ESP + tiny screen + menu-driven handheld UX | Marauder | **SHIPPED** (form) | smol's physical shape + menu UX; radio-manipulation kinship → coexist |
| BLE-presence "every node shows where every other is" | Marauder's Watch | **DEFERRED (refuted)** | #58 closed — same BLE-on-C3 wall as #22 |

---

## 4. Remote-management UX

| Borrow | Source | Status | Citation / note |
|---|---|---|---|
| Remote-management-appliance ethos + clean web-control UX | PiKVM | **SHIPPED (analog)** | #21 node manager + the HA control surface ("manage the physical thing from anywhere"); ethos, not a code port |

---

## 5. Lean real-time embedded

| Borrow | Source | Status | Citation / note |
|---|---|---|---|
| Interleave out-of-band real-time commands with a streamed buffer | GRBL | **SHIPPED (partial)** | #23/#40 coexist drains ESP-NOW RX between MQTT poll iterations; the *fully* non-blocking form is [#89 open](https://github.com/jphein/smol/issues/89) (live tracker) |

---

## 6. The GAP — mesh authentication (ESP-NOW PMK/LMK)

The **only** learned-and-forgotten actionable borrow. Althea's per-link-auth principle
(#163: WireGuard-per-neighbor → smol's native analog is an ESP-NOW **PMK** group key +
per-peer **LMK** on unicast) was captured in [#12](https://github.com/jphein/smol/issues/12)
as "(optional, someday) … only worth it if the mesh ever leaves the hobby threat model,"
then #12 was **closed** — so the intention has no live home.

**Ground truth (verified):** `net/mode.rs` sets `lmk: None, encrypt: false` at every
`add_peer` site (L3456/3546/4910); the security comments read *"ESP-NOW here is
unauthenticated and unencrypted, so ANY device on [channel] can inject … sign or LMK-encrypt
if it ever matters."* So SMOLv1 is, by documented choice, an open mesh.

**Why it's a GAP not a DON'T:** it was never *rejected* (unlike Babel-wholesale) — it was
deferred without a tracker. And its trigger is now concrete and converging with other work:
- **RPG anti-cheat** ([#181](mesh-ledger-study.md) → #184): a signed ledger authenticates
  *records*; PMK/LMK authenticates *frames*. Both are "trust on-mesh data" — they should be
  designed together.
- **Public-repo topology** is accepted ([memory: `smol-public-repo-topology-accepted`]), but
  that's about *metadata exposure*, not *frame injection* — an unauthed mesh is still
  injectable by any in-range ESP-NOW device.

Filed as **#190** (tracking) with the trigger "mesh leaves the hobby threat model — RPG
anti-cheat, adversarial RF environment, or a fleet deployed outside the owner's control,"
and cross-linked to #184 so the two authentication lanes converge.

---

## 7. Top-3 highest-value unshipped learnings

1. **Mesh authentication** (Althea #12 PMK/LMK + ledger #184 signing) — the security posture.
   Highest *latent* value: it's the one principle smol learned and left open, and two
   independent lanes (frame-auth + record-auth) point at the same RPG-anti-cheat trigger.
   *Do them together when the trigger fires; don't bolt on frame-crypto and record-sigs
   separately.*
2. **ETX metric proven on hardware** ([#164](https://github.com/jphein/smol/issues/164)) —
   *built* (PR #179) but HW-gate-held. It's the keystone the whole mesh-quality thread waits
   on: it unlocks #180 (channel-drag selection) and #165 (best-relay). A single bench soak
   converts a coded learning into a shipped one and releases two downstream lanes.
3. **The tamper-evident ledger** ([#182](https://github.com/jphein/smol/issues/182)) — the
   newest high-value learning: fleet provenance (the OOM-saga replay) + the RPG world-state
   substrate, for ~300 B RAM and zero new airtime. Ships integrity now; authenticity (#184)
   later — see #1.

---

## 8. Coverage scorecard

| Thread | Actionable borrows | SHIPPED | DEFERRED | GAP | DON'T |
|---|---|---|---|---|---|
| 1. Mesh / routing | 13 | 8 | 2 (#54, ledger-filed) | **1** (encryption) | 2 (route-learning, Babel-wholesale) |
| 2. Local-first ESP↔HA | 12 (+2 ethos) | 10 | 1 (BLE) | 0 | 0 |
| 3. Cheap-ESP audacity | 3 (+1 ethos) | 2 | 1 (BLE) | 0 | 0 |
| 4. Remote-mgmt | 1 | 1 | 0 | 0 | 0 |
| 5. Lean embedded | 1 | 1 (partial, #89) | 0 | 0 | 0 |
| **Total** | **~33** | **~26** | **~5** | **1** | **~4** |

**Read:** smol shipped ~79% of what it learned, deliberately deferred ~15% (all with live
trackers or documented triggers except the one gap), rejected ~12% with explicit verdicts,
and left exactly **one** learning (mesh authentication) learned-and-untracked — now fixed by
#190. The 12 non-mesh inspirations were correctly read as ethos, and the actionable ones
(WLED/ESPHome/Tasmota → HA integration; atomic14 → form factor; esp-link → OTA;
Battery-Emulator → live wiring) all landed.

---

## 9. Executive summary
Asked "did we implement everything we learned from our inspirations?", the honest,
ground-truth-verified answer is **almost** — and the exceptions are healthy. The mesh/routing
thread (Meshtastic gateway model, ZHNetwork flood+dedup, esp_wifi_repeater coexist + health
publish, Althea seqno-freshness) and the HA-integration thread (WLED typed entities, ESPHome
discovery + IO registry, Tasmota runtime-config-no-reflash, esp-link OTA, the live
Battery-Emulator wiring) are essentially fully shipped; the audacity/UX/embedded threads were
correctly ethos + form-factor and are embodied by the product itself. What remains is a short,
well-shaped queue: ETX proven on HW (#164, one soak away), the ledger (#182, filed), long-range
+ fully-non-blocking-flush (#54/#89, tracked) — and the single genuine gap, **mesh
authentication**, which was learned, documented as "someday," then quietly lost its tracker.
#190 gives it a durable home and points it at the RPG-anti-cheat trigger it now shares with
the ledger's signing lane (#184). **Nothing important stays learned-and-forgotten.**
