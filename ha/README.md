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

- `packages/smol_mesh.yaml` — an HA **package** with **two** automations, each
  publishing a **retained**, display-ready payload on any source-sensor change,
  every 5 minutes (heartbeat), and at HA start:
  - `smol/display/batt` — the battery screen (voltages + SOC page).
  - `smol/display/grid` — the grid/consumption screen (issue #16).
  Plus two MQTT **mirror** sensors (`sensor.smol_display_batt` / `sensor.smol_display_grid`)
  so a dashboard card shows exactly what the boards fetch.

### `smol/display/batt` payload (LOCKED — the exact lines the firmware renders on the 72×40 OLED)

```
BATT|48V 52.8V|HV 391.9V|d 43mV|48V 69%|HV 99%|Chg 4.1A
```

- pipe-separated, **≤6 segments**, each ≤12 chars, ≤96 bytes total, ASCII (worst case ≈62 B).
- **Segments 1-3 = VOLTAGE overview page** (title `Batt`); **segments 4-6 = SOC/charge
  DETAIL segments** (issue #17). The board pages through them; boards flashed before
  #17 do `split('|').take(3)` so they ignore 4-6 — **fully backward-compatible**.
- **⚠️ Big-render contract (co-designed with the firmware 2026-07-08):** the firmware
  renders each DETAIL segment (4-6) as its own full-window page — a small **label**
  (text before the FIRST space) + a **big value** (everything after). So each detail
  segment is `<shortlabel> <value>` with **no interior space before the value**
  (`48V 69%` → label `48V`, value `69%`). Labels are single tokens.
- seg 1 `48V` = **`sensor.48v_battery1_voltage`** (48 V LFP bank, BMS-direct), `%.1f`V —
  fallback **`sensor.48v_battery2_voltage`**, else `--`
- seg 2 `HV`  = `sensor.be_battery_voltage` (BMW i3 HV pack), `%.1f`V
- seg 3 `d`   = `sensor.be_cell_voltage_delta` (i3 cell spread), `%.0f`mV
- seg 4 `48V` = **48V bank SOC** `sensor.48v_battery1_battery` (**BMS, coulomb-counted**),
  `%.0f`% — fallback `sensor.48v_battery2_battery`. **Deliberately the BMS, NOT EPEver**
  (team ruling 2026-07-08 — see below).
- seg 5 `HV`  = **HV pack SOC** `sensor.be_soc`, `%.0f`% — fallback `sensor.battery_state_of_charge`
- seg 6 `Chg` = **charge current** `sensor.epever_charging_current` (TOTAL solar charge
  into the 48V bank), `%.1f`A. Kept on EPEver, not a BMS current: the per-battery BMS
  currents (`48v_battery{1,2}_current`, ~12 A + ~10 A) are half-the-bank each and can't
  be summed in the macro; EPEver reads the true total bank charge current (~16 A).
- an `unavailable`/`unknown`/**stale** source renders that value as `--`
  (e.g. `BATT|48V --|HV 391.9V|d 43mV|48V 69%|HV 99%|Chg --`)
- **Override mode** (see below) owns segments 1-3 when active; the detail trio (4-6) is
  still appended, so the board can page to live SOC/charge even mid-override.

### `smol/display/grid` payload (issue #16, v2.2 EXTENSION)

```
GRID|1118W|L1 150W|L2 970W
```

- same envelope: pipe-separated, ≤96 B, each line ≤12 chars, ASCII.
- line 1 (no label) = **TOTAL** `sensor.yurt_consumption` — the "yurt consumption"
  sensor JP watches (a calculated sum over the two PJ2101A clamps). It reports in
  **kW**, so it is scaled **×1000 → watts** for unit consistency with the legs.
  (`sensor.total_grid_power` is the same value already in W, but JP watches "yurt
  consumption", so that is the source of truth.)
- line 2 `L1` = `sensor.l1_power_clamp` (W) · line 3 `L2` = `sensor.l2_power_clamp` (W)
- all three lines in **watts**; a value that would exceed 5 digits of watts renders
  as `X.XXkW` for that line (safety valve — a yurt won't reach 100 kW).
- per-line `--` on unavailable/unknown/stale (30-min gate, same as the voltage lines).

### ⚠️ Why NOT `sensor.epever_battery_voltage` for the 48V line

That entity is **chronically corrupted** and is deliberately avoided. HA history
for 2026-07-08 (1672 points) shows it bouncing **0.00 – 111.40 V** all day:
**695 drops below 10 V** and **6 spikes above 111 V**, updating every ~16 s with
garbage (suspected Modbus/PE11 bus contention — under separate investigation). The
BMS-direct `sensor.48v_battery1_voltage` (~52.8 V, stable; cross-checks against the
DessMonitor inverter's 52.5–52.8 V) is the truth source, with
`sensor.48v_battery2_voltage` as fallback. (The underlying EPEver corruption was
root-caused & fixed 2026-07-08 — a hidden PE11 vendor-cloud agent acting as a second
Modbus master, blocked at the firewall — but the BMS-direct entity remains the
authoritative voltage source and the EPEver bus still occasionally flaps
`unavailable` on a device reboot.)

### ⚠️ Why the 48V **SOC** (segment `A`) is the BMS, not EPEver

Team ruling 2026-07-08: segment `A` sources the **BMS** SOC
(`sensor.48v_battery1_battery`, fallback `sensor.48v_battery2_battery`), **not**
`sensor.epever_battery_soc`. Two reasons: (1) the BMS **coulomb-counts**, while
EPEver's SOC is a crude voltage estimate; (2) the EPEver bus flaps `unavailable`, so
sourcing SOC from it would blank the SOC page on every EPEver reboot. Using the BMS
— the same source we already trust for the voltage line — keeps the SOC page off the
flaky EPEver path entirely. (The observed SOC estimates disagreed: EPEver ~80 % /
BMS ~69 % / DessMonitor ~60 %; the coulomb-counted BMS wins.) The charge-current
line (segment `C`) still reads EPEver `sensor.epever_charging_current`, so it blanks
`C --` when the EPEver bus is down — acceptable, since it is current, not SOC.

### Staleness (freshness gate)

The automation also fires every 5 min, and the template renders `--` for any
source whose **`last_reported`** is older than its **per-segment window** — so a
wedged integration can't freeze a stale-but-live-looking value into the payload.

- `last_reported` (not `last_updated`) is used on purpose: it advances on **every**
  state write, so a live integration re-reporting an unchanged value stays "fresh",
  while a truly wedged one goes stale. (Verified live 2026-07-08:
  `sensor.48v_battery1_battery` re-reports every ~25 s at a steady value —
  `last_reported` age 25 s vs `last_updated` 46 min — so it correctly reads as fresh.
  ⚠️ The REST `/api/states` `last_reported` field can disagree with the template
  engine's — **validate freshness via `/api/template`**, which is what the automation
  actually runs.)
- **Per-segment windows (team ruling 2026-07-08 — approved):**
  - **Voltage segments + 48V BMS SOC + EPEver charge current + all GRID lines:
    30 min (1800 s)** — the `cell()`/`pcell()` default. The BE HV/delta sensors
    publish on-change with a steady pack, so they legitimately go quiet up to
    **~22.6 min** (median ~17 s; measured from `/api/history`). A tighter window
    (e.g. 10 min) blanks *healthy* data and trains the reader to ignore `--`; 30 min
    covers the observed worst case with margin and still catches the real failure.
  - **HV pack SOC (segment `B`, `sensor.be_soc`): 6 h (21600 s).** SOC at rest changes
    glacially, so this on-change MQTT source legitimately goes silent for tens of
    minutes to hours (**measured 50 min silent at a steady 99 %** on 2026-07-08). A
    30-min window would blank a perfectly valid SOC almost the whole time the pack is
    at rest, gutting the SOC page; 6 h shows the real value yet still catches a truly
    dead Battery-Emulator. (48V BMS SOC stays at 30 min because the BMS *polls* and
    re-reports every ~25 s — it doesn't need the wide window, and the tight window
    keeps it honest.)
  - Each window is a per-call `maxage` argument on the `cell()`/`pcell()` macro — one
    constant per segment if it ever needs tuning.
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

## Node manager (issue #21) — set each node's default screen remotely

The HA **publish/GUI half** of the node manager. Set a node's boot **default screen +
page** from HA; it is published as **retained** MQTT and the board applies it on its
next burst — no reflash, no per-board `board.rs` edit. Protocol is LOCKED in
`nodemgr-design.md §2`; this package builds it verbatim. **The firmware CONSUME side
(subscribe + panic-free parse) is a later wave** — until it lands, publishing to a real
node's topic has no effect (harmless; the retained value simply waits).

**Command topic (one per node — the single source of truth):**

```
smol/<id>/config/default_screen   (retain: true, qos: 0)   payload: <AppKind>:<page>
```

- **AppKind tokens** = the EXACT `app.rs` enum spellings (case-sensitive), full espnow
  set: `Menu Clock Batt Grid Snake Bench MeshSnake About`. The firmware ignores/clamps
  any token it can't build in its tier (never crashes). `Snake` maps to `MeshSnake` on
  espnow via `SNAKE_KIND`; both are accepted.
- **page** = one digit; flat `0`/`1` (only `Batt` has 2 pages today). Firmware clamps
  an invalid/out-of-range page to `0`.
- **Apply** publishes the retained value (broker retention = persistence). **Reset**
  publishes an **empty** retained payload (retain-delete) = clear → the node falls back
  to its `board.rs` compile-time `DEFAULT_APP`/`DEFAULT_PAGE` (design §2.5).
- **Set all** fans out to **every** per-node topic (id7/id8/id9) — there is **no
  broadcast topic** (design ruling #3; a broadcast couldn't reach cred-less leaves over
  the unauthenticated mesh anyway — security R-P3).
- **Why the GUI can't emit a bad payload:** the `input_select` options are a closed set
  of valid tokens, so HA always publishes a well-formed `<AppKind>:<page>`. The firmware
  still parses **panic-free** because the broker is LAN-writable by anything (a retained
  payload that panicked would boot-loop-brick the board — security §2, the umbrella MUST).

**Helpers / entities this package adds:**

- `input_select.smol_all_screen` / `_all_page` + `input_button.smol_all_apply` (set-all).
- Per node (7/8/9): `input_select.smol_<id>_screen` / `_<id>_page` +
  `input_button.smol_<id>_apply` / `_<id>_reset`.
- `sensor.smol_<id>_config` — mirrors the retained command topic (the **commanded**
  value shown on the card).

**Deferred (NOT built here — need firmware follow-ups / JP decisions, per the design):**

- **Live** current-screen preview + running build → needs firmware `smol/<id>/status`
  (design F4). v1 reflects the **commanded** config only, not the live actual screen.
- **Mesh-topology** (hub/spoke) panel → needs firmware `smol/<id>/mesh` roster (design §4).
- **OTA push panel** → JP-decisions F5 (signing vs accepted-risk) / F6 (physical
  long-press accept) / F7 (per-node announce topic) + the firmware status publish.
- Node **reach**: credential-less leaves never open MQTT, so they use their `board.rs`
  default until given creds (JP decision F2). The card notes this.

**The node-status rows** on the card use the EXISTING discovery entities
(`sensor.smol_<id>_<noun>_smol_<id>`) — that part is live data today.
⚠️ Those entity_ids are **doubled/ugly** (`sensor.smol_7_dominion_smol_7`) — a
discovery-naming quirk worth cleaning up under the #12 device-grouping work (see the
WLED research memo: one device `smol <id>` with structured child entities).

### Deploy (HELD for JP review — do NOT run yet)

Same proven pass; because this adds **new `input_*` + `mqtt:` sensors**, use
**`reload_all`** (not `automation.reload`):

```bash
cat ha/packages/smol_mesh.yaml | ssh jp@10.0.6.108 "sudo tee /homeassistant/packages/smol_mesh.yaml > /dev/null"
TOKEN=$(bw get password ha-llat)
curl -sX POST https://ha.jphe.in:8123/api/config/core/check_config -H "Authorization: Bearer $TOKEN"
curl -sX POST https://ha.jphe.in:8123/api/services/homeassistant/reload_all -H "Authorization: Bearer $TOKEN"
# then install ha/dashboard/smol_nodemgr_card.yaml via WS lovelace/config/save (see below)
```

**Do NOT publish to the real `smol/<id>/config/*` topics until the firmware consume side
ships** — a retained config a board can't yet parse just sits there (harmless), but keep
the surface clean. (Validated instead against a throwaway `smol/test/config/...` topic.)

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

## Install / redeploy (DEPLOYED & LIVE 2026-07-08 — this is the update procedure)

`scp` doesn't work on the HAOS SSH add-on (no subsystem), so use the tee pattern:

```bash
# from katana, copy the package onto the HA VM
cat ha/packages/smol_mesh.yaml \
  | ssh jp@10.0.6.108 "sudo tee /homeassistant/packages/smol_mesh.yaml > /dev/null"

# packages load via !include_dir_named packages — confirm configuration.yaml has:
#   homeassistant:
#     packages: !include_dir_named packages

# validate config, then apply the MINIMAL reload (see below)
TOKEN=$(bw get password ha-llat)
curl -sX POST https://ha.jphe.in:8123/api/config/core/check_config \
  -H "Authorization: Bearer $TOKEN"
```

**Which reload? (verified 2026-07-08 — avoid a full restart when you can):**

| Change | Minimal reload |
| --- | --- |
| Automation template / trigger edits only | `POST /api/services/automation/reload` — re-reads all YAML incl. package-merged automations |
| **Added/removed a `mqtt:` sensor or an `input_*` helper** | `POST /api/services/homeassistant/reload_all` — automation.reload does **not** register new manually-configured MQTT entities |
| New top-level integration/domain | full `POST /api/services/homeassistant/restart` |

```bash
# e.g. after a template edit:
curl -sX POST https://ha.jphe.in:8123/api/services/automation/reload -H "Authorization: Bearer $TOKEN"
# after adding a mqtt mirror sensor (as the grid one was):
curl -sX POST https://ha.jphe.in:8123/api/services/homeassistant/reload_all -H "Authorization: Bearer $TOKEN"
```

Both automations fire on HA start (and after a reload, trigger them once to warm the
topics), so `smol/display/batt` and `smol/display/grid` are retained on the broker
before any gateway connects. Confirm the mirror sensors reflect the new payloads:
`sensor.smol_display_batt` (6 segments) and `sensor.smol_display_grid`.

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
