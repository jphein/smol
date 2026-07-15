# smol Mesh RPG — Design Spec

*2026-07-15 · brainstormed with JP (visual companion session) · status: approved pending spec review*

A massively-multiplayer fantasy RPG in the LitRPG tradition, running natively on the smol
mesh: isometric 1-bit world on 72×40 OLEDs, persistent shared state over ESP-NOW, no server.
Ultima Online's spirit at four orders of magnitude less hardware.

## Locked decisions (from the brainstorm)

| # | Decision | Choice |
|---|----------|--------|
| 1 | Play model | **Session adventures** — active play in a persistent shared world (World-Snake-style pick-up-and-play; world persists between sessions) |
| 2 | Art direction | **Full isometric 1-bit**, all biomes — ¾ view, diamond tiles, two-wall buildings, object density. The market-corner + paperdoll mockups are the craft bar ("art bible p.1-2") |
| 3 | World aliveness | **Slow heartbeat** — gentle world ticks while idle (respawn, wander, restock) on the crown's flush cadence; no continuous simulation |
| 4 | Character identity | **The board IS the character** — soul in NVS; broker checkpoint each flush enables resurrection onto a new board |
| 5 | Progression | **Use-based skills, no classes** (UO model): skills rise by doing, with diminishing returns; stat toasts (`[Herbalism +0.1]`) are the core feedback loop; professions emerge |
| 6 | Input | **Three tiers**: (1) one-button base — auto-walk + context actions, ships first, every board plays; (2) joystick+2-button addon card on the free GPIOs (0/1/3/7/10), auto-detected; (3) BLE Stadia pad via ESPHome bt-proxy → HA → live-MQTT relay. **Native BLE explicitly deferred** — the #22 refutation stands (blocking firmware wedges the C3's BT init); revisit only with the embassy rewrite |

## Architecture

New `rpg` cargo feature + plugin beside Clock/Snake/Bench. Five modules; every module has a
**host-testable pure core** (no HAL deps — the #13 wire-extraction pattern):

| Module | Purpose |
|--------|---------|
| `rpg/world.rs` | Chunked map, tile model, entity model, seed-procedural terrain + authored stamps + delta layer |
| `rpg/char.rs` | Character sheet, use-based skill table (fixed-point 0.0–100.0), inventory, NVS soul record (versioned, word-aligned — the NetCfg v3 lessons apply) |
| `rpg/render_iso.rs` | Iso renderer: stamp library, back-to-front draw order (occlusion for free), biome dither palettes, ambient animation variants, paperdoll compositor |
| `rpg/proto.rs` | Wire records riding `net::wire`/UP2 unchanged: INTENT (≤16B move/act), WORLD-DELTA (entity/tile/toast events), CHUNK-SYNC, CHAR-CHECKPOINT |
| `rpg/sim.rs` | Crown-side authority for loaded chunks + the slow heartbeat tick |

Transport, elections, flood, retained persistence: **used as-is** — zero changes to the
net stack. The RPG is a client of the mesh, not a modification of it.

## World model

- World: **512×512 tiles** = 32×32 chunks of 16×16 tiles.
- **Terrain is procedural from a world seed** — every board derives identical geography;
  zero terrain storage. Towns/dungeons are **hand-authored chunk stamps** compiled into flash
  (the art-bible scenes are literally map data).
- Only **deltas** persist: player-made changes (chopped tree, dropped item, opened chest) +
  live entities. A chunk's delta+entity checkpoint ≤ ~200 B — fits one ESP-NOW frame AND the
  broker's 512 B publish cap (retained topic per dirty chunk: `smol/rpg/chunk/<cx>/<cy>`).
- Entities per chunk capped (≈8) — monsters, NPCs, ground items; 8 B each packed.
- Characters: NVS soul (name, stats, skills, inventory, position) + retained checkpoint
  `smol/rpg/char/<node-id>` each flush. Board dies → new board resurrects from checkpoint.

## Authority, protocol, failure modes

- **Crown = world authority** for chunks with players in/near them; ticks the heartbeat on
  its existing flush cadence (~20–30 s): respawns, wander, shop restock. No players near a
  chunk → it freezes (decision 3).
- Active play: player board sends INTENT frames; crown validates, applies, floods
  WORLD-DELTA. **Local prediction**: own movement renders immediately; crown echo corrects.
- **Crown death is a designed non-event** (the stateless-crown principle proven by the OTA
  system): successor rebuilds authority from its own chunk caches + retained checkpoints.
  In-flight INTENTs are lost (acceptable; the player re-acts).
- Split-brain: standard election supersession applies; chunk deltas carry a lamport-ish tick
  so a stale crown's late floods lose.
- Mesh partitions: each partition's crown owns the chunks its players occupy; retained
  checkpoints reconcile on heal (last-tick-wins per chunk; characters are per-board so never
  conflict).

## Renderer & art system

- **Stamp library** in flash: 8×4 diamond ground tiles, wall/roof segments, props (barrel,
  crate, well, sign, lantern, awning), creature + player sprites (~3×5), all 1-bit.
- Draw back-to-front in iso order — occlusion falls out of ordering; full-frame redraw per
  tick (72×40 buffered = cheap).
- **Biome dither palettes**: per-region recipes for water/mist/ground texture at 4 brightness
  levels (d50/d25/d12/solid), per the art bible.
- **Ambient animation**: frame counter selects stamp variants — water shimmer, lantern
  flicker, firefly blink, dog trot.
- **Paperdoll screen**: figure + equipment-slot compositing + stat bars + XP edge — equipping
  loot visibly changes the doll.
- Headless boards (id5-class) play fully — they render to the buffer (harmless) and rely on
  toasts via HA/companion surfaces; a headless board is still a soul + a participant.

## LitRPG engine

- Skills: fixed-point 0.0–100.0, gain-by-use with UO-style diminishing returns; gain events
  queue **toasts** (one line, bottom strip, ~2 s each). Piezo chirp on level-up when #61 lands.
- Crafting: recipe table gated on skill thresholds (herbs→poultice, ore→ingot→blade).
- Loot: tiered drop tables; items are world entities (walk over to pick up).
- Death: LitRPG-appropriate sting without cruelty — drop carried loot at a gravestone entity,
  respawn at last shrine, skills untouched. (Gravestones are delta-layer entities — they
  persist, findable by others. Emergent economy of grave-robbing accepted and welcomed.)

## Testing strategy

1. **Host simulator** (the big one): compile `world`+`sim`+`proto` pure cores natively; N
   simulated boards run scripted sessions on katana; assert world convergence, checkpoint
   round-trips, crown-handover reconstruction, partition/heal reconciliation. Agents build
   most of the game against this — no fleet time burned.
2. Golden-frame render tests: render known scenes to buffers, compare against checked-in
   frames (the art bible as test fixtures).
3. HW canary gates (the proven pattern) for anything touching radio/flash/input timing.
4. Soak: the slow heartbeat runs on the live fleet for days before any wide release.

## Build phases (each = a HW-gated PR wave)

- **P0 — First light**: seed-world gen + iso renderer + one-button auto-walk, single board.
  *Exit: walking Lanternfall's market corner on real glass.*
- **P1 — The MMO moment**: chunk sync + INTENT/DELTA protocol + crown authority; two boards
  see each other move. *Exit: two players wave on one screen; crown-kill mid-walk is a non-event.*
- **P2 — The numbers**: souls, skills, toasts, combat, loot, paperdoll, death/gravestones.
- **P3 — The living town**: NPCs, shops, heartbeat economy, crafting chains, second biome.
- **P4 — Deluxe**: joystick card, Stadia-via-proxy input, piezo audio, third region, seasonal
  event hooks.

## Out of scope (v1)

PvP (flagged for a later decision), native BLE, world sizes beyond 512², off-board world
authority (the "oracle" content service may FEED the world via MQTT later but never owns it),
color/grayscale displays.

## Open questions (deliberately deferred to their phases)

- Exact auto-walk semantics for one-button (P0 prototyping decides).
- Live-MQTT input session lifecycle for tier-3 input (P4).
- Whether the HA dashboard grows a world-map spectator view (post-P1, luna-lane).
