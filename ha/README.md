# smol ↔ Home Assistant (MQTT-native)

The Home-Assistant end of the smol mesh's battery bridge (**team spec v2**). The
board's net stack is UDP-only (no_std smoltcp) and HA's REST API is TLS-only, so
the two meet on **MQTT** — Mosquitto (the HA add-on) speaks plain TCP on the LAN,
MQTT 3.1.1 QoS0 is small enough to hand-roll in no_std, and **retained** messages
turn the broker into a cache a burst-mode gateway can read in ~2 s.

This directory holds the **downlink** half (HA → displays). The uplink
(telemetry → HA) is created automatically by the firmware's retained MQTT-discovery
configs and needs no HA config (see *MQTT discovery* below).

## What's here

- `packages/smol_mesh.yaml` — an HA **package** with one automation that publishes
  a **retained**, display-ready payload to `smol/display/batt` on any source-sensor
  change, every 5 minutes (heartbeat), and at HA start.

Payload (LOCKED — the exact lines the firmware renders on the 72×40 OLED):

```
BATT|48V 52.8V|HV 391.9V|d 43mV
```

- pipe-separated, ≤3 lines, each ≤12 chars, ≤96 bytes total, ASCII
- `48V` = **`sensor.48v_battery1_voltage`** (48 V LFP bank, BMS-direct), `%.1f`V —
  fallback **`sensor.48v_battery2_voltage`**, else `--`
- `HV`  = `sensor.be_battery_voltage` (BMW i3 HV pack), `%.1f`V
- `d`   = `sensor.be_cell_voltage_delta` (i3 cell spread), `%.0f`mV
- an `unavailable`/`unknown`/**stale** source renders that value as `--`
  (e.g. `BATT|48V --|HV 391.9V|d 43mV`)

### ⚠️ Why NOT `sensor.epever_battery_voltage` for the 48V line

That entity is **chronically corrupted** and is deliberately avoided. HA history
for 2026-07-08 (1672 points) shows it bouncing **0.00 – 111.40 V** all day:
**695 drops below 10 V** and **6 spikes above 111 V**, updating every ~16 s with
garbage (suspected Modbus/PE11 bus contention — under separate investigation). The
BMS-direct `sensor.48v_battery1_voltage` (~52.8 V, stable; cross-checks against the
DessMonitor inverter's 52.5–52.8 V) is the truth source, with
`sensor.48v_battery2_voltage` as fallback. **Do not** reintroduce the epever entity
here until its root cause is fixed.

### Staleness (freshness gate)

The automation also fires every 5 min, and the template renders `--` for any
source whose **`last_reported`** is older than **30 min (1800 s)** — so a wedged
integration can't freeze a stale-but-live-looking value into the retained payload.

- `last_reported` (not `last_updated`) is used on purpose: it advances on **every**
  state write, so a live integration re-reporting an unchanged value stays "fresh",
  while a truly wedged one goes stale.
- **Why 30 min (team ruling 2026-07-08):** the BE HV/delta sensors publish
  on-change with a steady pack, so they legitimately go quiet up to **~22.6 min**
  (median ~17 s; measured from `/api/history`). A tighter window (e.g. 10 min)
  blanks *healthy* data and trains the reader to ignore `--`; 30 min covers the
  observed worst case with margin and still catches the real failure (dead MQTT /
  wedged integration). Per-entity windows were rejected as over-engineering for v1.
  It's a single `1800` constant in the macro if it ever needs tuning.
- **KNOWN LIMITATION:** the freshness gate only protects against a wedged
  *integration*. If **HA itself dies**, the retained payload persists on the broker
  and boards will keep showing it (with their own fetch-age, which stays small). A
  payload-embedded timestamp is a filed follow-up, not a v1 requirement.

## Manual override + dashboard

Set any custom message on every board without touching firmware. The package adds:

- `input_boolean.smol_display_override` — master toggle.
- `input_text.smol_display_line1/2/3` — three line **sources** (max 100 chars).
- `sensor.smol_display_batt` — an MQTT **mirror** of the retained topic, so the
  dashboard shows exactly what the boards fetch.

When the toggle is **ON**, the automation publishes `BATT|<l1>|<l2>|<l3>` (same
topic + envelope) instead of the battery reading — boards need **zero changes**.
When **OFF**, it returns to the live battery template.

**Line sources with `{entity_id}` placeholders.** Each line is literal text plus
placeholders, e.g. `48V {sensor.48v_battery1_voltage}V` → `48V 52.8V`, or
`PV {sensor.epever_pv_power}W`. Placeholders are resolved by **safe regex
substitution** (`regex_findall` on `\{([a-z0-9_.]+)\}`, each `{id}` →
`states('<id>')`) — the template never `eval`s user-typed Jinja (HA's sandbox
forbids it, and it would be an injection risk). A placeholder whose entity is
unknown/unavailable renders `--`.

**Refresh cadence.** Referenced entities are *not* triggers, so placeholder values
re-render on the automation's existing **5-min `time_pattern`** (plus on any
override-helper edit and at HA start). A value inside a placeholder can therefore
be **up to ~5 min stale** — expected; documented so it's not a surprise.

**Sanitization (server-side, after substitution):** each rendered line has `|`
characters stripped (they'd corrupt the pipe framing) and is clipped to 12 chars,
so the payload stays ≤96 B and can never break the wire format no matter what is
typed (a line of pure `|` collapses to empty). Blank/unknown sources render empty.

The ready-made Lovelace card is `dashboard/smol_display_card.yaml` (toggle + 3
source fields with the syntax hint + the mirror sensor).

## Broker (verified 2026-07-08)

Mosquitto runs on the HA VM, which is **quad-homed** (VLAN6 `10.0.6.108` / VLAN8
`10.0.8.111` / VLAN10 `10.0.10.222` / VLAN11 `10.0.11.110`) and binds `0.0.0.0`, so
**every leg is the same broker** — retention and topics are shared across all of
them. **The cross-VLAN CONNACK gotcha is real**, so target the leg that answers
from where the client lives:

| Broker leg | Role | Notes |
| --- | --- | --- |
| **`10.0.11.110:1883`** (HA VM, VLAN11) | ✅ **firmware target** (boards) | same subnet as the boards; no inter-VLAN routing; CONNACK 3/3 verified from a VLAN11 source |
| `10.0.6.108:1883` (HA VM, VLAN6) | ✅ **cross-VLAN-safe fallback** | answers even cross-VLAN (rc=0 verified); also the leg to use for **katana-side tests** (same subnet as katana) |
| `10.0.8.111:1883` (HA VM, VLAN8) | ❌ never | TCP connects but CONNACK silently drops cross-VLAN (asymmetric return path — reproduced) |
| `10.0.11.117:1883` (host **disks**) | ❌ never | a *different* Mosquitto (disks' own docker broker) — **not** the HA one |

Proven live from katana (VLAN6): raw MQTT CONNECT to `10.0.6.108:1883` → CONNACK
rc=0; to `10.0.8.111:1883` → hangs, no CONNACK. Retention is **broker-wide**, so a
retained message published via any leg is delivered to boards on the VLAN11 leg.
(Residual on-hardware check: a *wireless* VLAN11 board vs the wired VLAN11 host
tested — L3 is identical, so confidence is high; a flash-day CONNACK smoke test is
the last gate.)

> The older homelab note "IoT devices target Mosquitto at `10.0.8.111` on the same
> VLAN" holds **only for VLAN8 devices** (e.g. the Battery-Emulator). The smol
> boards are on VLAN11 → they use `10.0.11.110`.

**Creds:** username `jp`; the password is the Mosquitto/JuicePassProxy addon option
`mqtt_password` (the homelab standard), **not** a vault item. Read it the same way
`~/Projects/ha/tools/deploy_juicebox_beacon.sh` does — via the Supervisor API:

```bash
export HA_TOKEN="$(bw get password ha-llat)"
python3 ~/Projects/ha/tools/ha_supervisor.py GET /addons/e4069849_juicepassproxy/info \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['options']['mqtt_password'])"
```

**Never paste the password into committed files, findings, logs, or issue comments.**
Addon ACL: authed users have readwrite on `#`, so `smol/#` needs no ACL changes.

## Install (deployment is HELD for review — do NOT run yet)

`scp` doesn't work on the HAOS SSH add-on (no subsystem), so use the tee pattern:

```bash
# from katana, copy the package onto the HA VM
cat ha/packages/smol_mesh.yaml \
  | ssh jp@10.0.6.108 "sudo tee /homeassistant/packages/smol_mesh.yaml > /dev/null"

# packages load via !include_dir_named packages — confirm configuration.yaml has:
#   homeassistant:
#     packages: !include_dir_named packages

# validate config, then restart HA to load the automation
TOKEN=$(bw get password ha-llat)
curl -sX POST https://ha.jphe.in:8123/api/config/core/check_config \
  -H "Authorization: Bearer $TOKEN"
curl -sX POST https://ha.jphe.in:8123/api/services/homeassistant/restart \
  -H "Authorization: Bearer $TOKEN"
```

After restart the automation fires on HA start and publishes the retained payload
immediately, so `smol/display/batt` is warm before any gateway connects. (A real
rendered payload is already retained on the broker from verification — HA overwrites
it on start.)

Sanity-check the retained topic any time (no HA restart needed):

```bash
mosquitto_sub -h 10.0.6.108 -p 1883 -u jp -P "$MQTT_PW" \
  -t smol/display/batt -C 1 -v      # prints: smol/display/batt BATT|48V 52.8V|HV 391.9V|d 43mV
```

### Dashboard card (install via WS, not `.storage`)

The card in `dashboard/smol_display_card.yaml` must be added through the WebSocket
`lovelace/config/save` API — **do not** hand-edit `.storage/lovelace*`. HA caches
those files in memory and silently overwrites your edit on the next WS save (the
"`.storage` cache trap"). Fetch the current Lovelace config, append this card to a
view's `cards:`, and save it back:

```bash
# read current config, splice in the card, write it back (all over WS)
python3 ~/Projects/ha/tools/ha_ws.py lovelace/config/get > /tmp/lovelace.json   # or your WS helper
#   ...insert dashboard/smol_display_card.yaml (as JSON) into the target view's "cards"...
python3 ~/Projects/ha/tools/ha_ws.py lovelace/config/save --config @/tmp/lovelace.json
```

(Any WS client with the ha-llat token works — the point is the `lovelace/config/save`
command, not file edits. The helpers + mirror sensor come from the package, so they
exist as soon as HA restarts; only the card placement is a Lovelace change.)

## MQTT discovery (uplink — created by the firmware, not by this package)

On each MQTT burst the gateway publishes **retained** discovery configs to
`homeassistant/sensor/smol<id>/telemetry/config`. HA's MQTT integration
auto-creates one text sensor per node. **PINNED discovery scheme** (firmware and
this doc must agree — spec v2):

```jsonc
// topic: homeassistant/sensor/smol<id>/telemetry/config   (retained)
{
  "unique_id":   "smol<id>_telemetry",
  "state_topic": "smol/<id>/telemetry",
  "name":        "smol <id>",
  "device": { "identifiers": ["smol<id>"], "name": "smol <id> <noun>" }
}
```

- Resulting entity: `sensor.smol_<id>_telemetry`, grouped under an HA **device**
  `smol <id> <noun>` (the magical noun is appended to the device name;
  identifiers `["smol<id>"]`).
- The telemetry payload on `smol/<id>/telemetry` is the **bare** line — **no**
  legacy `NNN ` id prefix (the topic already carries the id). QoS0, not retained.
- Discovery configs are retained + idempotent, so they survive HA restarts.
- MQTT integration is confirmed loaded (HA 2026.6.3); zigbee2mqtt already proves
  discovery + retained-survives-restart on this exact broker.

## Collector retirement checklist (POST-hardware-verify only — not now)

The Python UDP collector on **disks** (`~/Projects/smol/collector/`, running as the
`smol-collector` user service at `10.0.11.117:9999`) is **replaced** by the MQTT
burst. Retire it only after the firmware's MQTT path is flashed and verified:

1. **Verify on hardware first:** a gateway publishes to `smol/<id>/telemetry`
   (entities appear in HA under device `smol <id>`) **and** renders the retained
   `smol/display/batt` on its OLED. Confirm leaves show the rebroadcast SMOLv1 BATT
   frame too.
2. Stop + disable the service on disks (user service, no sudo):
   ```bash
   ssh disks 'systemctl --user stop smol-collector && systemctl --user disable smol-collector'
   ```
3. Archive the telemetry log (run interactively — no timestamp interpolation in a
   committed script):
   `ssh disks 'mv ~/smol-collector/collector.jsonl ~/smol-collector/collector.jsonl.archived-<date>'`
4. Remove the deploy dir + unit if desired:
   `ssh disks 'rm -rf ~/smol-collector ~/.config/systemd/user/smol-collector.service && systemctl --user daemon-reload'`.
   (There is **no** `/etc/smol-collector.env` in v2 — no token lives on the
   collector host.)
5. **Node removal (ops):** to drop a node's discovered entity, publish an **empty
   retained** message to its config topic:
   `mosquitto_pub -h 10.0.11.110 -u jp -P "$MQTT_PW" -r -n -t homeassistant/sensor/smol<id>/telemetry/config`
6. **Rollback path:** the firmware change (UDP egress → MQTT burst) is a git revert
   + reflash; the collector code stays in the repo history for that fallback.

Until all of the above, leave the collector running — it and the MQTT bridge can
coexist (the firmware picks one egress at compile time).

## SMOLv1 BATT ESP-NOW frame (context; firmware owns it)

The gateway rebroadcasts the retained payload to leaves as a SMOLv1 BATT frame:
the 12-byte tag `SMOLv1 BATT ` followed by the **verbatim** payload including its
`BATT|` marker (e.g. `SMOLv1 BATT BATT|48V 52.8V|HV 391.9V|d 43mV`) — no length
byte (payload length = frame length − 12). Frame payload and the board's
`BattCache` are byte-identical (one memcpy).
