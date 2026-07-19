# smol Mesh Visualizers — Design Spec

*2026-07-18 · JP request · status: approved, building*

Two realtime Rust visualizers of the live smol mesh, sharing one ingest/model core:

- **meshscope** (#158) — an **instrument**: egui/eframe, dense, precise, forensic. The tool that
  makes nights like 07-14/07-15 five times faster to debug.
- **observatory** (#159) — a **showpiece**: Bevy, a living fantasy constellation of the fleet, for
  a wall display and the Hackaday hero footage.

Both are **pure MQTT listeners** — zero firmware changes, zero fleet risk. They read the same
broker topics the boards already publish.

## Guiding principles

1. **The broker already knows everything.** DIAG/STAT/PEERS/mesh-channel/ota/* carry crown,
   channel, per-peer RSSI, heap, uptime, build, OTA progress, elections. The visualizers derive
   100% of their world from retained + live topics. No new firmware telemetry required for v1.
2. **One model, two faces.** A shared `mesh-model` crate owns MQTT ingest + the topic→state fold
   + freshness aging. meshscope and observatory are thin render frontends over identical state.
   A bug fixed in the model fixes both; a new telemetry field lights up both.
3. **Listener, never actor.** These tools SUBSCRIBE only. They never publish to `smol/*` (no
   commands, no order-arming) — a diagnostic instrument must not perturb what it measures. (A
   future opt-in "operator" mode could add controls behind an explicit `--operator` flag; out of
   scope for v1.)
4. **Freshness is truth.** Every node/edge carries a last-heard timestamp; stale entities fade
   and age out (the F6 discipline). A visualizer that shows ghosts is worse than none — retained
   topics lie about liveness (the lesson that cost hours this week).

## Workspace layout

New top-level cargo workspace at `rust/viz/` — **separate from the embedded `rust/clock`
workspace** (different target, std, heavy host deps; must never entangle the no_std firmware
build or its lockfile).

```
rust/viz/
  Cargo.toml            # workspace: mesh-model, meshscope, observatory
  mesh-model/           # shared: ingest + state + fold + aging (lib, no UI deps)
  meshscope/            # bin: eframe/egui instrument
  observatory/          # bin: bevy showpiece
```

## `mesh-model` — the shared core

**Responsibility**: connect to the broker, subscribe `smol/#`, parse the wire formats, fold into
a `MeshWorld`, expose it to a frontend via a lock-free snapshot channel. No UI, no rendering.

- **Ingest**: `rumqttc` on its own thread; reconnect-with-backoff; `smol/#` wildcard. Creds from
  env (`SMOL_MQTT_HOST` default 10.0.6.108, `SMOL_MQTT_USER`, `SMOL_MQTT_PASS`) — **never
  committed, never logged**.
- **Parsers** (one per topic family; each a pure `&[u8] -> Option<Record>`, unit-tested against
  captured fixtures — reuse real DIAG/STAT lines from this week's logs):
  - `smol/<id>/diag` → the pipe-delimited DIAG (slot, rst, boot, ota, up, heap, hmin, net, brk,
    otah, fwd, dedup, hop, tage, ffl, fok, dlseq, cfg, io…). The field workhorse.
  - `smol/<id>/stat` → live screen/build.
  - `smol/<id>/peers` → the roster; **per-peer RSSI here → graph edges.**
  - `smol/mesh/channel` → `MC|owner|ch|seq` → crown + channel + election events (seq jumps).
  - `smol/<id>/ota/state` (JSON), `ota/diag`, `ota/relaydiag` → OTA progress + phase + fetch
    health (chunk/last_wb).
  - `smol/<id>/status`, `smol/<id>/config/*` → names, screens.
- **`MeshWorld` state**:
  - `nodes: Map<u8, Node>` — id, name, build, heap+hmin, uptime, boot, reset reason, net slot,
    otah, screen, last_heard, a ring buffer of (heap, rssi-to-crown, uptime) for sparklines.
  - `edges: Map<(u8,u8), Edge>` — RSSI, smoothed, last_heard (from PEERS; symmetric-merged).
  - `crown: Option<{owner, channel, seq, since}>` + a rolling election log.
  - `events: RingBuffer<Event>` — elections, OTA start/progress/finish/fail, reboots (boot++),
    fallbacks, toasts. The forensic ticker.
- **Aging**: a tick marks nodes/edges stale past a threshold (default 45 s ≈ 1.5 flush cycles),
  fades them, drops them past a longer TTL. Configurable.
- **Output**: `arc-swap` (or `watch` channel) of an immutable `MeshWorld` snapshot — frontends
  read the latest without blocking ingest. Model runs headless; frontends are optional consumers
  (a `--headless --json` mode dumps state for scripting/CI).

## meshscope (#158) — the instrument

egui/eframe. Immediate-mode is the right paradigm: repaint from the snapshot each frame, no
retained-widget/data desync. Layout:

- **Center: the graph canvas** (`egui::Painter`). Nodes as discs — crown ringed + badged,
  headless nodes hollow, stale nodes dimmed; label = name + build + heap. Edges drawn with width
  ∝ RSSI, dashed when stale. Simple deterministic layout v1 (fixed positions by id or a light
  spring); no physics needed for 4-30 nodes.
- **Right dock: selected-node inspector** — every DIAG field, live; `egui_plot` sparklines for
  heap/hmin/uptime/RSSI. Click a node to pin it.
- **Bottom: event ticker** — scrolling, color-coded (election=amber, OTA=cyan, reboot=red,
  toast=dim). Filterable. This is the forensic replay.
- **Top bar**: broker status, fleet build-uniformity (all-on-N? mixed?), crown+channel, node
  count, "N stale".
- **OTA overlay**: when any node is mid-fetch, a progress bar per node from ota/state +
  relaydiag (chunk k/n, last_wb) — the thing I watched by hand all week.

## observatory (#159) — the showpiece

Bevy. Same `mesh-model` snapshot; renders it as art per the realm-fantasy aesthetic:

- Nodes = glowing orbs in a **force-directed** constellation (bevy transform + a simple spring
  system); crown wears a visible crown/halo.
- Edges = luminous filaments, brightness ∝ RSSI, that flicker/thin as links weaken.
- **Elections** = the crown visibly travels from old owner to new (a comet/arc), the moment we
  spent all week reading as `MC|` seq numbers becomes a *scene*.
- **OTA feeds** = particle streams flowing crown→leaf along the filament, filling as the image
  transfers; a burst on completion.
- **Channel changes** = a color-temperature shift of the whole field.
- Ambient: gentle drift, bloom, the smol palette. Built for a wall display and for capturing the
  Hackaday hero clip. Heavier; correctness is secondary to beauty (meshscope is the source of
  truth).

## Build & run — familiar is the builder

familiar (24 cores, 31 G RAM) is the compile host — keeps the heavy std/Bevy builds off
katana (which is RAM-tight). rustup being installed now (minimal profile). Bevy needs a few
system libs (`libasound2-dev`, `libudev-dev`, `libwayland-dev`/X11 dev, `libxkbcommon-dev`) —
the build agent installs/validates them. Run targets: meshscope runs on katana (JP's desktop,
where the display is); observatory can run on either — a wall-display host TBD. Both later add a
`wasm32` target so meshscope embeds in the project site next to the #152 emulator (the site
becomes a *live* fleet dashboard, not just docs).

## Testing

1. **Parser fixtures**: real captured topic lines (this week's logs are full of them) → assert
   the fold. The parsers are the correctness surface; they get the coverage.
2. **Replay harness**: feed a recorded MQTT session (mosquitto_sub -v dump) through the model at
   speed → deterministic world reconstruction; also the demo/dev loop without a live fleet.
3. **Headless JSON**: `mesh-model --json` snapshots as a scriptable fleet-state probe (useful in
   its own right — a better `mosquitto_sub | grep` for future forensics).
4. Frontends are visually validated (screenshots / the artifact); the model carries the asserts.

## Phases

- **P0 — model + fixtures**: mesh-model parses every current topic, folds MeshWorld, ages
  correctly, replay harness green on this week's logs. (No UI — the foundation both faces need.)
- **P1 — meshscope MVP**: graph + inspector + ticker + OTA overlay on the live fleet. The
  instrument, usable.
- **P2 — observatory MVP**: constellation + crown-travel + OTA particles on the live fleet.
- **P3 — polish + WASM**: meshscope→site (live dashboard), observatory→wall-display/hero-footage;
  optional `--operator` mode discussion.

## HA dashboard parity (bidirectional — a first-class requirement)

The visualizers and the HA "Control Room" dashboard (luna's #58-era rebuild) must stay in
**feature parity, both directions** — every derived signal one surfaces, the other surfaces too.
The `mesh-model` fold is the single source of derivation truth; the HA dashboard renders the
same derivations rather than re-deriving them differently.

- **Anything the visualizers derive → into HA**: crown/channel, per-peer RSSI edges (graph),
  election events, OTA progress, fleet build-uniformity, stale-node aging. Where HA can't show a
  concept natively (an RSSI *graph*), `mesh-model` gains a small **`--publish` bridge mode**
  (opt-in, the ONE sanctioned exception to listener-only): it republishes derived state to
  retained `smol/derived/*` topics (e.g. `smol/derived/crown`, `smol/derived/uniformity`,
  `smol/derived/edge/<a>-<b>`) that HA MQTT sensors/cards consume. Derived topics only — never
  `smol/<id>/*` command topics.
- **Anything HA shows → into the visualizers**: the DIAG discovery sensors luna added, HA-side
  battery/solar (the BATT/GRID data on-glass), any HA template sensors — the visualizers read the
  same source topics so nothing is HA-exclusive.
- **Shared vocabulary**: node names, build/version labels, health thresholds (what counts as
  "stale", "low heap", "weak link") are defined ONCE in `mesh-model` and mirrored into the HA
  package, so the two never disagree on e.g. what RSSI is "weak".
- **Parity checklist** lives in the epic (#158/#159) and is checked each time either side gains a
  signal: "added to viz → add to HA; added to HA → add to viz." A `mesh-model` release that adds
  a derivation ships with the matching HA card/sensor in the same wave.

Coordination: the HA-side work is a luna lane in the `ha` project consuming `smol/derived/*`;
the viz-side is the dreamteam lanes below. The `--publish` bridge is the contract between them.

## Out of scope (v1)

Publishing/commanding from the tools (listener-only); historical DB/time-travel beyond the
in-memory ring buffers; auth beyond broker creds; mobile.

## Dreamteam plan

- Shared `mesh-model` (P0) lands FIRST, one agent, on its own branch — both frontends depend on
  it. Merge it before the frontends fork, or the two frontend agents collide on the crate.
- Then meshscope (P1) and observatory (P2) in parallel, separate agents, separate crates —
  genuinely independent once the model is fixed. Builds on familiar.
- Each: own worktree, branch `feat/158-meshscope` / `feat/159-observatory` /
  `feat/mesh-model`, clippy+build green (on familiar), PR, normal review (no HW gate — host tools).
- Creds discipline: env only, never in commits (public repo).
