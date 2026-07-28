# smol ↔ Home Assistant (MQTT-native)

How the smol mesh talks to Home Assistant — and why it's **MQTT**, not ESPHome or a
custom API. This is the architecture overview; the **operational half** (exact HA
entities, YAML, deploy steps, broker legs, creds) lives in
[`ha/README.md`](../ha/README.md), and the byte-level wire frames in
[protocol.md](protocol.md).

Verification legend: 🟢 hardware-verified · 🟡 compile/spec-verified, not fully on
hardware · ⚪ design.

## Why MQTT (and not ESPHome / the native API)

smol is a **single-radio** device, and the reasoning below was written when it was also a
**burst** device — the radio left the mesh for WiFi and the mesh went deaf. ⚠️ **#23 retired that
burst** (2026-07-12): the radio now stays up through a WiFi sync via co-channel coexist, so the
~15 s mesh-deaf window is gone. **The conclusion still holds, for a narrower reason** — see the
ESPHome bullet — so the analysis is kept rather than deleted. What has *not* changed is that a
single radio cannot be a persistent TCP server and a mesh node at once. That rules out the two
"richer-looking" options:

- **ESPHome native API** needs the device to be a **persistent TCP server** Home
  Assistant dials into and holds open. Pre-#23 the killer was that the radio was off-WiFi ~28 s of
  every 30 s, so HA saw it perpetually offline. Post-#23 the window is gone but the objection
  survives: a **leaf** never associates at all (only the elected gateway does), so a per-node
  persistent socket is still not a thing this topology can offer — and MQTT's retained-message
  cache is what lets a leaf get its data second-hand over the mesh. There is also **no Rust ESPHome
  firmware** — ESPHome is Python→C++ codegen. (The full analysis was a
  scratch note that has since been pruned; the conclusion is what survived.)
- **MQTT discovery + retained messages** fit perfectly: the broker (Mosquitto on the HA
  VM) is the **cache**. The gateway connects for ~2 s, publishes/reads, disconnects; a
  **retained** message survives the gap and is delivered on the next burst. This is the
  same pattern WLED's MQTT interface uses, minus the always-on assumptions.

## The two directions

### Uplink — telemetry → HA (MQTT discovery)
On each burst the gateway publishes retained **MQTT-discovery** configs, so each node
appears in HA as a native `sensor.smol_<id>_*` entity with **zero HA-side YAML**. Leaf
telemetry is relayed leaf→gateway over ESP-NOW ([RELAY](protocol.md#relay--relayack--espnow--internet-telemetry)),
then the gateway publishes it. 🟢 hardware-verified. **#12 (everything-release):** each node
groups under one HA **device** with **typed child entities** (`object_id` + `expire_after`) instead of
a single packed telemetry line — verified on-wire on id7, **2026-07-12**. ⚠️ **The split is only
partial:** live discovery on **2026-07-27** carried **3 `_voltage` and 3 `_rssi` entities and zero
`_soc`**, so SOC (and `_role`) still ride the packed line. Docket item **D11** stays open for that
reason. A one-time HA retained-clear removed the legacy build-56 config (a stale-retained artifact,
not a firmware defect; #36 closed as a no-op).

⚠️ **A retained ghost is only cosmetic while it is telemetry.** The same mechanism on a **command**
topic is an instruction the fleet keeps obeying — a retained `smol/<dead-id>/ota/install` suppresses the
crown's own OTA indefinitely and reports nothing. Registry-derived dashboards stop *displaying* ghosts
without stopping them *acting*: see [ota.md § when nothing installs and there is no
error](ota.md#-when-nothing-installs-and-there-is-no-error--a-dead-boards-retained-order-308) for the
mechanism and `tools/ghost_reconcile.sh` for the check.

### Downlink — HA → every display (retained + mesh re-broadcast)
HA automations publish **retained**, display-ready payloads that the gateway grabs in its
burst and **re-broadcasts** over ESP-NOW so leaves render them too — single-hop normally, or through a relay since #13 shipped routed multi-hop (`BATT2`/`GRID2` carry the downlink behind a strictly-newer freshness gate):

| Topic | Screen | Payload | Mesh frame | Status |
|---|---|---|---|---|
| `smol/display/batt` | Batt | `BATT\|48V 52.8V\|HV 391.9V\|d 43mV\|48V 69%\|HV 99%\|Chg 4.1A` (6-seg: voltage overview + big SOC/charge detail pages, #16/#17) | `SMOLv1 BATT` | 🟢 on-glass (gateway); 🟡 leaf receipt inferred |
| `smol/display/grid` | Grid | `GRID\|963W\|L1 177W\|L2 786W` (yurt total + 2 phase clamps, watts, #16) | `SMOLv1 GRID` | 🟢 on-glass (gateway); 🟡 leaf receipt inferred |

Both are ≤96 B, ≤12 chars/segment, with per-segment `--` on unavailable/stale sources
(30-min `last_reported` windows; HV pack SOC 6 h because it changes glacially at rest).
See [protocol.md](protocol.md#batt--ha-battery-snapshot) for the frames and
[`ha/README.md`](../ha/README.md) for the exact source entities + staleness rationale.

## Node manager (#21) — remote screen config

Set each node's **default screen + page** from HA, no reflash. HA publishes a **retained**
`smol/<id>/config/default_screen` = `<AppKind>:<page>`; the board reads it on its next
burst and applies it (empty payload = clear → the board.rs compile-time default). The
control surface is HA **Lovelace** (not an on-device web UI — a burst radio can't host
one; the node manager IS smol's WLED-web-UI analog, relocated to where a burst device is
reachable). "Set all" writes every per-node topic — there is **no broadcast topic**, and
**no ESP-NOW command relay** (the unauthenticated mesh must never become a command
channel). **Status:** 🟢 **shipped** — gateway-self default-screen **verified on glass** (id7), plus
**leaf-relay** (a gateway relays a leaf's screen over a SMOLv1 CFG frame; strict, panic-free allowlist
parse). Protocol: [protocol.md → CFG](protocol.md#cfg--keyed-per-node-config-channel-56);
GUI/entities: [`ha/README.md`](../ha/README.md).

## OTA (#6) — stage, then install per node (🟢 hardware-proven)
Firmware updates ride the same MQTT-native pattern, but **not via an `announce` topic** — that
act-path was **retired at the Model-A #32 closure** and there is deliberately **no fleet-push
topic**. Two topics, and only two:

| Topic | Retained | What it does |
|---|---|---|
| `smol/ota/staged` | yes | **Arms** every board's HA Update entity. **No board fetches anything.** |
| `smol/<id>/ota/install` | yes | **Per-node** install order — the wire behind HA's Install button. Idempotent (the gate is `staged.build > running`), so a re-fire never re-installs. |

The payload carries an **ed25519 signature (`sighex`)** alongside build/size/sha256/url, and the
leaf **verifies that signature before it writes a byte** (#32). sha256 is the image *identity*
used against reproducible builds (#44) — **never the trust gate**. Recovery is
**app-side self-rollback + canary-one-board-at-a-time** — the bundled bootloader
slot-selects, but **revert-on-boot-fail is OFF** (unproven/likely disabled), so a bad
image is contained by pushing to one board at a time (never fleet-unison), not by an
automatic bootloader revert. 🟢 engine + publish tooling + HA panel + native HA **Update entity** (#33)
are landed and **OTA is hardware-proven**: on **2026-07-10** a canary self-updated build 58→59 over
the air in ~17 s (fetch → verify → boot `ota_1` → `Valid`), and #40 has since delivered full ~1 MB
images to WiFi-less leaves **over the mesh**. *(Dating the run matters: undated, "58→59" reads as the
fleet's current build forever — it is a historical measurement, not a status.)* The first attempt had failed for an **infra** reason (a missing firewall
allow-rule to reach the image host, since added; [#37](https://github.com/jphein/smol/issues/37) resolved)
— **not a firmware bug**. Rollout stays canary-one-board-at-a-time. See [ota.md](ota.md).

## Collector retirement
The MQTT link **retires the old Python UDP collector** (`collector/`, which ran on
`<host>`). Telemetry now goes straight to HA; the collector is kept in git history only as
a rollback path. The retirement checklist (stop/disable the service, archive the JSONL) is
in [`ha/README.md`](../ha/README.md#collector-retirement-checklist-post-hardware-verify-only--not-now).

## Broker (one line; detail in ha/README)
Mosquitto runs on the HA VM and binds `0.0.0.0`; boards target its **`<broker-ip>:1883`**
leg on the boards' own subnet (no cross-VLAN routing). Creds are
the Mosquitto addon option, never on the mesh. Full broker-leg table + gotchas:
[`ha/README.md`](../ha/README.md#broker-verified-2026-07-08).
