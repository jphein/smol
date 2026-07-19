# meshscope

Realtime [egui](https://github.com/emilk/egui) instrument for the **smol** ESP-NOW
mesh (issue #158). A pure MQTT listener — **no firmware changes** — that subscribes
`smol/#`, folds the retained/live topics into a live world model, and draws it:

- **Node graph** (force-directed canvas): each node a disc coloured by role
  (👑 crown gold · gateway amber · leaf teal · stale grey · installing violet),
  RSSI-weighted edges from each node's `peers` record, a crown badge on the elected
  owner, node id + realm noun + build, and a tiny heap sparkline.
- **Detail panel** (click a node): DIAG health (uptime, heap free/min, reset reason,
  loss/rtt/rx/tx, time source, broker, hop, fwd/dedup/ttl, dlseq/dfwd, cfg echo),
  OTA state, and heap / uplink-RSSI sparklines, plus the peers it hears.
- **Event ticker**: crown changes, version flips, OTA installs + fetch retries, joins.

## Topics consumed (`smol/#`)

| Topic | Shape | Feeds |
|---|---|---|
| `smol/mesh/channel` | `MC\|<owner>\|<ch>\|<seq>` | crown / channel + crown-change events |
| `smol/<id>/peers` | `PEERS\|<G\|L>\|<ch>\|id,rssi,age,ch,flags;…` | role, channel, RSSI edges |
| `smol/<id>/diag` | `DIAG\|k=v\|…` | health fields + heap sparkline |
| `smol/<id>/status` | `STAT\|<screen>:<page>` | live screen |
| `smol/<id>/ota/state` | JSON (installed/latest/in_progress/title) | build, OTA state, version-flip events |
| `smol/<id>/ota/{install,diag,relaydiag,armdiag}` | text | install-armed + fetch-retry events |
| `smol/<id>/uplink` | int dBm | gateway RSSI-to-AP sparkline |
| `smol/<id>/telemetry` | sensor line | per-node readout |
| `smol/display/batt` \| `…/grid` | `BATT\|…` / `GRID\|…` | HA battery/grid readout |
| `homeassistant/+/smol<id>/+/config` | discovery JSON | realm noun (fallback: vendored `names`) |

Wire shapes pinned in [`docs/protocol.md`](../../docs/protocol.md) and
`rust/clock/src/net/wifi.rs` (the code is authoritative).

## Run

```sh
# live — reads SMOL_MQTT_HOST/USER/PASS (a local .env is auto-loaded)
cp .env.example .env    # then fill in the broker host + credentials
cargo run --release

# UI with synthetic data, no broker
cargo run --release -- --demo

# headless model check (parses sample payloads, prints, exits — for CI / build host)
cargo run --release -- --selftest
```

Credentials come from the environment only and the `.env` is git-ignored — **never
commit real values**. The broker is the HA-add-on Mosquitto leg on the boards' VLAN
(same one the gateway firmware targets — see `docs/protocol.md` → *MQTT burst*).

## Build

Own Cargo workspace, isolated from the embedded `rust/clock` crate. Host target
(x86_64), not the C3. Per issue #158 the release binary is built on `familiar` and
copied to `bin/` (git-ignored) for katana. eframe dlopens X11/wayland + GL at
runtime, so it needs a display to *run* (use `--demo` for a broker-less screenshot).

```sh
cargo build --release        # -> target/release/meshscope
cargo test                   # parser + model unit tests
```

## Future

- Render the retained `smol/<id>/screen` 1-bit display mirror on each node.
- WASM build for the site (pairs with #152).
