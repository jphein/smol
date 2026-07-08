# World Snake — the smol MMO mesh game

Every smol board running the mesh firmware shares **one snake world**. Your 72×40
OLED is a **scrolling window** that follows your own head — there are **no walls**,
just a big world you roam, with other boards' snakes crossing your view, each
labelled by its **magical name**. It's a massively-multiplayer snake that runs on
$3 microcontrollers talking directly to each other over ESP-NOW — no server, no
internet.

> **Status (2026-07-07):** the world, movement, scrolling viewport, dead-reckoned
> peers, the leaderboard **and all six treasure-powers** are **landed + committed**
> (`6baea36`, plus power fix `877b2af`) and **compile-verified** (all 3 builds
> clean). It is **not yet flashed / hardware-verified** — that lands at the final
> flash. Powers exist in code (`mesh_snake/snake_core.rs`: `POWER_PHANTOM…PHOENIX`,
> `POWER_COUNT = 6`); durations are first-pass and tunable.

## How to play — one button

The board has one usable button (the other is RESET), so the whole game is one
control, exactly like the classic Snake mode:

- **Short tap → turn clockwise.** Up → Right → Down → Left → Up. Time your taps;
  you're always moving.
- **Long-press → back to the BOOT menu.** (Not used for anything in-game.)
- You are the **solid** snake, always centred; the camera follows your head. Eat
  food to grow. Run into a body (yours or a peer's) and you die, then respawn.

## The shared world

- One **256×256-cell** world, **toroidal** — walk off an edge and you wrap around;
  there are no walls to crash into, only snakes.
- **Food** spawns deterministically — every board computes the *same* food
  locations from the shared mesh clock, so nobody has to negotiate. Eat it to
  grow by one.
- **Peers**: any snake near your head is drawn inside your viewport as a body of
  cells, with its **magical name** (e.g. *Draconic Dominion*, *Eldritch Nexus*)
  floating by it. Peers are reconstructed by *dead-reckoning* — each board only
  broadcasts its head + heading + length 5×/second, and everyone else extrapolates
  the body between updates. A briefly-missed update self-heals on the next one.
- Up to **~16 snakes** share a world comfortably (smooth ≤8, good 12–16).

## The six treasure-powers  *(in firmware — compile-verified, HW pending)*

Beyond food, **rare treasures** spawn (roughly 1 every 45–60 s, patchy). Grab one
and you gain **one timed magical power** — only **one at a time** (a new pickup
replaces the old). Treasures are deterministic like food (every board agrees where
each is and which power it grants), so there's zero negotiation; only *who grabbed
it* is local. A treasure on the field is a **slow-blinking 4 px star/diamond**,
distinct from food's small static dot.

| Treasure | Power | What it does | Lasts | How to spot it on the 1-bit screen |
|---|---|---|---|---|
| **Wraith Veil** | *Phantom* | Phase through **all** bodies — you can't die by collision, and you're **harmless to others** | ~6 s | the whole snake **flickers** (~3 Hz blink) — the one power peers *must* see, since it changes who can kill whom |
| **Zephyr Rune** | *Haste* | ~1.75× faster | ~5 s | a **chevron** on your head (peers see a small spark) |
| **Aegis Ward** | *Shield* | Absorbs the **next** lethal hit, then clears | ≤10 s | a 1 px **halo ring** around your head |
| **Midas Sigil** | *Golden Growth* | Food gives **+3 length** instead of +1 (so it still counts as score — see leaderboard) | ~8 s | a **sparkle** on your head + a HUD glyph |
| **Mothlight Lantern** | *Reveal* | Your compass shows **all** treasures + nearest peers | ~10 s | a **lantern** glyph in the HUD |
| **Phoenix Ember** | *Rebirth* | Die while it's active → **instantly respawn keeping your length** (once) | ≤10 s | an **ember** dot on your head |

**Reading powers on a tiny mono screen:** your *own* power is always legible as a
HUD pill (a little icon + a shrinking timer bar). For *peers*, only **Phantom** is
drawn distinctly (the flicker) because it's the only one that changes whether they
can kill you; every other peer power collapses to one generic 2 px "empowered
spark" over their head — the specifics only matter to the owner. Two snakes on one
treasure? **Both get it** — the wire format holds exactly one power per snake, so
nothing stacks or exploits.

## The leaderboard  *(landed — score ≡ length)*

Your **score is your length** — no separate points, no extra data on the wire.
Every board sorts all the snakes it can hear (plus you) by length, so **everyone
sees the same ranking of magical names**:

- **Always on:** your rank rides the HUD (e.g. `#2 L:14 P:5` — rank #2, length 14).
- **Top-3 on your death/respawn screen** (the natural pause): e.g.
  `1 Dominion 22 / 2 Nexus 14 / 3 Herald 9`, with you highlighted.
- A dead snake keeps its place for a moment, then drops to `L:3` on respawn; a
  snake that goes silent falls off the board after ~5 s.

## Joining a mesh

To play together, boards just need to be **near each other** and agree:

1. **Same firmware** — flash the mesh build (`--features espnow`) on each board.
2. **Same ESP-NOW channel** — the firmware pins a fixed channel (ch 6) in its
   time-share mode, so boards find each other automatically; no pairing step.
3. **A distinct id per board** — set at flash time (`mode::start(…, id, …)`); each
   id maps to a deterministic **magical name** shown on screen, so you can tell
   who's who. (See [BUILDING.md](BUILDING.md) → *Multi-board / ESP-NOW mesh*.)

Boards auto-discover over the ESP-NOW handshake (the blue LED goes blink =
detected → solid = connected). Once linked, your snakes share one world.

## Under the hood

The wire protocol (the 18-byte `SMOLv1 SNK` frame, 5 Hz phase-jittered, head-only
with dead-reckoned bodies, and the `flags` byte that carries the active power in 5
bits at **zero** extra size) is documented in
[protocol.md](protocol.md#snk--mmo-mesh-snake-design). Why head-only: a full
144-segment body would be 288 bytes — over the 250 B ESP-NOW frame limit — so the
body *can't* be sent; broadcasting head+heading+length and reconstructing the rest
is both necessary and naturally loss-tolerant.

---
*Player-facing guide. Mechanics + powers from the committed World Snake (`6baea36`,
power fix `877b2af`); compile-verified, not yet flashed. Honest status at top.*
