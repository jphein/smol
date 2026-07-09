# smol ↔ Home Assistant (MQTT-native)

How the smol mesh talks to Home Assistant — and why it's **MQTT**, not ESPHome or a
custom API. This is the architecture overview; the **operational half** (exact HA
entities, YAML, deploy steps, broker legs, creds) lives in
[`ha/README.md`](../ha/README.md), and the byte-level wire frames in
[protocol.md](protocol.md).

Verification legend: 🟢 hardware-verified · 🟡 compile/spec-verified, not fully on
hardware · ⚪ design.

## Why MQTT (and not ESPHome / the native API)

smol is a **battery, single-radio, burst** device: its radio sits on the ESP-NOW mesh
channel and only briefly switches to WiFi (~2 s per burst) to reach the LAN. That rules
out the two "richer-looking" options:

- **ESPHome native API** needs the device to be a **persistent TCP server** Home
  Assistant dials into and holds open — incompatible with a radio that's off-WiFi ~28 s
  of every 30 s (HA would see it perpetually offline). There is also **no Rust ESPHome
  firmware** — ESPHome is Python→C++ codegen. (Full analysis:
  `scratch/smol-ha-batt/rust-esphome-research.md`.)
- **MQTT discovery + retained messages** fit perfectly: the broker (Mosquitto on the HA
  VM) is the **cache**. The gateway connects for ~2 s, publishes/reads, disconnects; a
  **retained** message survives the gap and is delivered on the next burst. This is the
  same pattern WLED's MQTT interface uses, minus the always-on assumptions.

## The two directions

### Uplink — telemetry → HA (MQTT discovery)
On each burst the gateway publishes retained **MQTT-discovery** configs, so each node
appears in HA as a native `sensor.smol_<id>_*` entity with **zero HA-side YAML**. Leaf
telemetry is relayed leaf→gateway over ESP-NOW ([RELAY](protocol.md#relay--relayack--espnow--internet-telemetry)),
then the gateway publishes it. 🟢 hardware-verified (build 45). *Follow-up (#12): group a
node's entities under one HA **device** and split the single telemetry line into typed
`_voltage`/`_soc`/`_rssi` entities — the WLED-legibility lesson.*

### Downlink — HA → every display (retained + mesh re-broadcast)
HA automations publish **retained**, display-ready payloads that the gateway grabs in its
burst and **re-broadcasts single-hop** over ESP-NOW so leaves render them too:

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
`smol/<id>/config/default_screen` = `<AppKind>:<page>`; once the firmware consume side lands, the board will read it on its next
burst and apply it (empty payload = clear → the board.rs compile-time default). The
control surface is HA **Lovelace** (not an on-device web UI — a burst radio can't host
one; the node manager IS smol's WLED-web-UI analog, relocated to where a burst device is
reachable). "Set all" writes every per-node topic — there is **no broadcast topic**, and
**no ESP-NOW command relay** (the unauthenticated mesh must never become a command
channel). **Status:** 🟡 HA publish/GUI side **deployed**; the firmware consume side
(strict, panic-free parse) is the next wave. Protocol: [protocol.md → CONFIG](protocol.md#config--retained-per-node-default-screen-21-specd--firmware-pending);
GUI/entities: [`ha/README.md`](../ha/README.md).

## OTA (#6) — retained announce (spec'd)
Firmware updates ride the same MQTT-native pattern: a retained
`smol/ota/announce` = `OTA|build|size|sha256|url`; the board fetches the image over
HTTP to its inactive A/B slot, verifies (sha256), and activates it. Recovery is
**app-side self-rollback + canary-one-board-at-a-time** — the bundled bootloader
slot-selects, but **revert-on-boot-fail is OFF** (unproven/likely disabled), so a bad
image is contained by pushing to one board at a time (never fleet-unison), not by an
automatic bootloader revert. 🟡 engine landed (integrity SHA gate + app-side rollback +
monotonicity); the fetch trigger + hardware run are next
(`scratch/smol-ha-batt/ota-plan.md`).

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
