# Snake — 2-player, networked over ESP-NOW

Two **smol** boards (ESP32-C3 SuperMini + 0.42" SSD1306 OLED, 72×40) each run one
snake and stay in sync over **ESP-NOW** — Espressif's connectionless 2.4 GHz
radio protocol (no Wi-Fi router, no pairing). Every board renders **both**
snakes and shares one piece of food. Eat to grow; crash into a wall, your own
tail, or the *other* snake and the round ends for both players.

This is the multiplayer sibling of [`../snake/snake.ino`](../snake/snake.ino). It
reuses that sketch's U8g2 display constructor (`U8G2_SSD1306_72X40_ER_F_HW_I2C`,
SDA=5/SCL=6), the `TILE=4` grid sizing (→ 18×10 cells on this panel), the
ring-buffer snake body, and the `millis()` movement tick.

---

## What you need

- **2× smol boards** (ESP32-C3 SuperMini + 0.42" OLED), each with a working
  OLED (run [`../../oled_test/oled_test.ino`](../../oled_test/oled_test.ino)
  first if unsure).
- Arduino IDE (or `arduino-cli`) with:
  - **esp32** core by Espressif (Boards Manager) — *the plain core, not the
    Bluepad32 fork*. Tested against **3.3.10**.
  - **U8g2** library by olikraus.

> No extra networking library is needed — ESP-NOW and Wi-Fi ship with the esp32
> core.

## Build & flash (do this for BOTH boards)

The sketch is **identical on both boards** — there is nothing per-board to edit.
They auto-negotiate who is "host" at runtime (see *How sync works* below).

**FQBN:**

```
esp32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M
```

**Arduino IDE:** select **ESP32C3 Dev Module**, set *USB CDC On Boot =
Enabled* and *Flash Size = 4MB*, then Upload. Repeat for the second board.

**arduino-cli:**

```bash
# compile
arduino-cli compile --fqbn "esp32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M" ./snake-2p.ino

# flash board 1, then swap USB and flash board 2 (adjust the port to yours)
arduino-cli upload  --fqbn "esp32:esp32:esp32c3:CDCOnBoot=cdc,FlashSize=4M" -p /dev/ttyACM0 ./snake-2p.ino
```

Power up both boards near each other. Each shows `link..` until it hears the
other, then the match starts automatically.

### Same channel on both boards

ESP-NOW peers must be on the **same Wi-Fi channel**. It is pinned near the top
of the sketch:

```cpp
static const uint8_t WIFI_CHANNEL = 1;   // MUST match on both boards
```

Since both boards run the same sketch this is already consistent. Only change it
if channel 1 is congested in your area — and if you do, change it on **both**.

---

## Controls — the onboard BOOT button (GPIO9)

There is **one button per player: the onboard BOOT button (GPIO9)**.

| Action | Effect |
|---|---|
| **Tap** (in play) | Turn **right** (rotate heading 90° clockwise). Tap 3× = a left turn. |
| **Tap** (game over) | Vote **ready** for the next round. When both players are ready — or after a ~6 s failsafe — a new round starts, synced. |

One button reaches every direction because each tap rotates you clockwise:
`right → down → left → up → right …`. A 180° reversal into your own neck is
rejected.

### Why a button and not the Bluetooth gamepad? (honest caveat)

The single-player Snake steers with a **Bluepad32 (BLE)** controller. This
2-player build deliberately does **not**, and here's the trade-off:

- The ESP32-C3 is **single-core** with **one shared 2.4 GHz radio**. Running
  **BLE (Bluepad32 / BTstack) and Wi-Fi (ESP-NOW) at the same time** makes the
  two stacks time-share that one front-end and competes for tight RAM. It's
  doable but **fragile and finicky** — exactly what you don't want in a demo.
- The two also come from **different board packages**: Bluepad32 ships its own
  radio stack (`esp32-bluepad32:esp32:…`), while ESP-NOW wants the plain
  espressif core (`esp32:esp32:…`). Mixing them is extra friction.

So for a **robust, compile-and-go** networked demo, each player steers with the
**BOOT button** and ESP-NOW gets the radio to itself. This is also the idiomatic
input for this exact board — see atomic14's single-button C3 games
([`docs/gaming-firmware.md`](../../docs/gaming-firmware.md)).

**If you really want a gamepad:** keep Bluepad32 for input, build against the
`esp32-bluepad32` board package instead, and expect the coexistence quirks
above (occasional stutter, longer boot, possible ESP-NOW init hiccups). That
path is intentionally not taken in this sketch.

---

## How it looks

- **Your snake:** solid/filled tiles.
- **Opponent's snake:** hollow/outlined tiles.
- **Food:** a small centered square (shared by both boards).
- **HUD (top-left):** `you:opp` scores while playing; `WIN` / `LOSE` / `DRAW`
  plus a `tap:rdy you/opp` ready indicator at game over. Before the link is up
  it shows `link..`; if the peer goes silent, `link lost`.

---

## How the sync works (brief)

- **Transport:** ESP-NOW **broadcast** to `FF:FF:FF:FF:FF:FF` on the fixed
  channel. `WiFi.mode(WIFI_STA)`, no AP association. One ~219-byte packet
  (well under ESP-NOW's 250-byte limit — enforced by a `static_assert`) carries
  a snake's full body every tick.
- **Roles (host vs client):** decided with **zero negotiation messages** — the
  board with the numerically **lower Wi-Fi MAC becomes host** (player 0, drawn
  starting on the left); the other is the client (player 1, starting on the
  right). Deterministic, so both boards always agree.
- **Each board simulates only its own snake** and broadcasts its full body; the
  opponent's snake is a *shadow* rebuilt from received packets and only drawn.
- **Food is host-authoritative:** only the host runs the RNG and picks the next
  food cell; it ships the cell + a sequence counter, and the client copies it.
  No shared-seed drift.
- **Rounds:** the host owns the round id (`gameId`). It bumps it when both
  players are ready (or after a failsafe), and the client adopts the new id from
  the next host packet — so restarts stay in step.
- **Body packing:** each occupied cell is sent as **one byte** (`x*ROWS + y`, a
  linear cell index, max 179 on the 18×10 grid), so even a snake filling the
  whole board fits in a single frame.

## Known limitations / caveats (honest)

- **Two players only.** Broadcast could carry more, but roles, rendering, and
  collision are written for exactly two snakes.
- **Best-effort, unencrypted, LAN-local radio.** ESP-NOW here is unencrypted
  broadcast; anyone on the channel with the same protocol tag could inject
  packets. Fine for a toy game, not a security boundary.
- **Latency artifacts.** The opponent is drawn from the last packet received, so
  under packet loss it may briefly lag or jump a cell. The head-to-tail grazing
  rule ignores tails to keep near-misses fair, but a genuinely simultaneous
  head-on-head crash can be scored slightly differently on each board for one
  frame before the shared alive-flags reconcile.
- **Receive-callback concurrency.** Incoming packets are applied from the Wi-Fi
  task while `loop()` reads the same state. On this single-core part it's
  effectively cooperative and benign for a demo (worst case: one frame of a
  half-updated shadow snake, corrected next tick); it is *not* hardened with a
  lock/queue.
- **Core-version-specific callback signatures.** The ESP-NOW receive/send
  callbacks use the **v3.x** core signatures (`esp_now_recv_info_t*` /
  `wifi_tx_info_t*`). On an older **2.x** core, switch them to the
  `const uint8_t *mac` forms noted in the code comments.
- **72×40 panel assumed for the 1-byte cell packing.** On a much larger panel
  (e.g. 128×64 → 561 cells) the linear index would exceed one byte; bump the
  packing to two bytes (and re-check the packet stays ≤250 B) if you port it.
