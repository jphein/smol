# smol · Control Room — Home Assistant dashboard

A single phosphor-CRT Lovelace view for the ESP-NOW mesh: live topology, per-node
screen/firmware/telemetry boxes, the shared glass, power sources, and the OTA forge.
It **auto-discovers** smol nodes and scales as sigils join.

### Files
| File | Role |
|------|------|
| `dashboard/build_control_room.py` | **Generator (source of truth).** Discovers nodes from `binary_sensor.smol_<id>_online`, builds one node box each, generates the topology SVG, and saves the view via the HA WebSocket API. |
| `dashboard/smol-control-scaffold.yaml` | Static frame (banner, section kickers, set-all, glass/power/forge, footer) with placeholders `TOPO` / `LEGEND` / `FLEET` that the generator fills. |
| `dashboard/smol_control_room.yaml` | **Exported reference** of the built view (example only — the generator is authoritative). |
| `themes/smol.yaml` | Phosphor-green theme (dark + light) + card-mod `@font-face` loading VT323 / IBM Plex Mono from `/local/luna-fonts/`. Scoped to `theme: smol`. |
| `www/luna-cards/smol-topology.svg` | Generated mesh-topology graphic, served at `/local/luna-cards/smol-topology.svg`. |

### Dependencies (HACS / registered Lovelace resources)
- **layout-card** — `custom:grid-layout` (the 12-col edge-to-edge view grid).
- **card-mod** — per-card styling + the theme's font `@font-face`.
- **mushroom** — `custom:mushroom-template-card` (the live node-box header + mini-OLED).
- Fonts `vt323.woff2` + `ibmplexmono.woff2` in `/config/www/luna-fonts/` (referenced by the theme).

### Build / deploy
```
HA_TOKEN=<long-lived-token> python3 dashboard/build_control_room.py
```
Set the HA host in `build_control_room.py` (`URI` = WS endpoint; `HA` = `user@homeassistant.local`
for the SSH `tee` that serves the SVG to `/config/www/luna-cards/`). Re-run after flashing a new
board — it appears automatically. Install the theme to `/config/themes/smol.yaml` and reload themes.

> **Placeholders:** home-energy source entities/names are generic
> (`sensor.battery_bank_soc`, `sensor.ev_battery_soc`, `sensor.house_load`,
> `sensor.solar_charge_current`, "battery bank" / "EV HV" / "house load"). Remap to your own
> entities in `smol-control-scaffold.yaml`. All `smol_*` entities are the project's own.

### ⚠️ Gotcha: never nest `custom:grid-layout` as a *card*
`custom:grid-layout` works as a **view `type:`** only. Used as a *nested card* (a grid-layout card
inside a view holding other cards) it renders **silently empty** — the children never paint, leaving
a blank gap that looks exactly like a stale cache. Place child cards **directly in the view grid**
via `view_layout: {grid-column: "span N"}`. The generator splices node boxes into the view's card
list for this reason (the `FLEET` placeholder is replaced in-place, not wrapped in a nested grid).

### Live current screen (shipped — #50)
Each node box shows **two rows**: the **commanded** default (`sensor.smol_<id>_config`, the
retained `smol/<id>/config/default_screen`) and the **live actual** screen
(`sensor.smol_<id>_screen`). The firmware publishes the live screen as
`smol/<id>/status = STAT|<screen>:<page>|<build>` — the gateway self-reads; leaves broadcast it
over ESP-NOW and the gateway decodes + republishes — so the live row tracks manual BOOT-menu
navigation, not just the command. (Shipped fleet-wide 2026-07-10 as #50; this was formerly the
`unknown`-until-published "design F4" placeholder. The generator prefers the live value and falls
back to commanded only while `sensor.smol_<id>_screen` is `unknown`.)
