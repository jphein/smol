# The dollar MMO — a shared-world game console, mesh, and OTA fleet on a $1 chip

*Community / positioning narrative for [#59](https://github.com/jphein/smol/issues/59) — the Hackaday writeup + RustConf/Supercon talk + esp-rs reference angle. Draft for JP's editorial review.*

> **⚠️ Editorial note (numbers).** The "$9 MMO" is smol's origin-story shorthand (and #59's
> title) — keep it as the *hook*, but the honest per-board cost is **$1.00 (headless ESP32-C3
> supermini) / $2.76 (with the 0.42″ OLED)**. Per JP's 2026-07-15 correction, *don't* print
> "$9 board" as a price. The real number is the *better* hook anyway: **a $1 computer as a
> mesh-MMO node.** Whether the title keeps "$9" is JP's call at draft review — the body below
> uses the real figures throughout.

---

## The pitch, in one breath

It started as a joke — *"can we make this $3 board into a tiny game player? can it run
Minecraft?"* Real Minecraft, no: an ESP32-C3 has **400 KB of RAM**, not gigabytes. But the
*soul* of it, yes — and from that joke grew something genuinely unusual:

**A fleet of ~$1–$3 boards running one `no_std` Rust firmware, talking directly to each other
over ESP-NOW (no router, no cloud), that you can update over the air with signed firmware —
even the WiFi-less nodes, over the mesh — and that hosts a living creature which hops from
board to board when you pull the power.** Games, a shared-world MMO, native Home Assistant
integration, remote config, observability, and self-healing OTA — all on a chip that costs
**less than a coffee.**

The claim we'll stand behind: **it is, as far as we know, the most complete `no_std`-Rust
ESP-NOW reference fleet in existence** — not a demo, but a real, hardware-proven system.

---

## The flagship demo: pull the plug, watch it jump

If you show one thing, show **the Mesh Familiar** ([#57](https://github.com/jphein/smol/issues/57),
shipped). **One creature lives on the whole fleet.** It inhabits a single board at a time —
its mood, hunger, and growth on that OLED — and when you **unplug the board it's on, it hops
to a neighbour** over the mesh and keeps living there. Every other board shows a Weasley-clock
pointer toward wherever it currently is. Exactly-one-holder arbitration, migration-on-loss,
and orphan re-election are all handled in the mesh layer.

**Human-verified on glass: pull the plug, watch it jump.** A shared-world creature that
migrates across dollar microcontrollers when you cut power is a genuinely novel thing to hold
in your hand — and it's the emotional hook that makes the technical story land.

---

## Why it's not a toy — the technical spine

Under the whimsy is a real distributed system, every piece hardware-proven on the id7/id8/id9
bench fleet:

- **Signed leaf-mesh-OTA** ([#40](https://github.com/jphein/smol/issues/40)). WiFi-less leaves
  update **over the mesh**: an elected gateway fetches an **ed25519-signed** ~1 MB image,
  relays it chunk-by-chunk over ESP-NOW (windowed-NAK), and the leaf **verifies the signature
  before it writes a byte**, then flashes an inactive A/B slot with brick-safe rollback. One
  runtime-NVS-node-id image serves the whole fleet.
- **Routed multi-hop mesh** ([#13](https://github.com/jphein/smol/issues/13)). A leaf out of
  direct range escalates to a **table-free, hop-limited managed flood** (Meshtastic lineage);
  its telemetry reaches home through a neighbour. On 2026-07-14 a gateway-deaf board delivered
  telemetry home through a neighbour — **the first routed frame in smol's history.**
- **Self-healing elections** ([#76](https://github.com/jphein/smol/issues/76) +
  [#204](https://github.com/jphein/smol/issues/204)). Single-gateway election validated across
  cascading-reboot / split-brain scenarios, with crown-handover heals and a coexist-deafness
  self-heal ladder.
- **Un-brickable runtime networking** ([#100](https://github.com/jphein/smol/issues/100)).
  Switch a node's WiFi, broker, or OTA host from a dashboard — with an **auto-revert** so a bad
  change can never strand a board.
- **One config frame for everything** ([#56](https://github.com/jphein/smol/issues/56)) + a
  **runtime IO registry** ([#72](https://github.com/jphein/smol/issues/72), "ESPHome
  inverted") — bind a button/relay/sensor to any free GPIO from Home Assistant, no reflash.
- **Observability + reproducible builds** ([#70](https://github.com/jphein/smol/issues/70) /
  [#44](https://github.com/jphein/smol/issues/44)) — a retained per-node health record + a
  byte-reproducible image whose sha256 is a verifiable identity.

And the games are real: **World Snake**, a shared 256×256 toroidal MMO world over the mesh with
a scrolling viewport, peers drawn by name, a mesh leaderboard, and treasure-powers
([#5](https://github.com/jphein/smol/issues/5)); plus a one-button arcade pack, an RSSI
treasure hunt, and a Bluetooth-controller Minecraft-ish digger.

---

## The angle for the Rust-embedded world (esp-rs reference positioning)

smol is a **worked answer to "what can `no_std` Rust + esp-hal actually do end-to-end?"** —
not a blink-an-LED sample, but a fleet with a radio protocol, OTA, HA integration, and games.
For the esp-rs / embedded-Rust community it's a **reference**:

- **The pure/driver split as a discipline.** The tricky logic (mesh flood decisions, the ETX
  link metric, the ledger hash-chain, the wire codec) lives in **dependency-free, `no_std`,
  HAL-free modules** that are **host-unit-tested** off-target — then the firmware wires them.
  This is *why* a hobby fleet can ship correct distributed-systems code: you test the brain on
  your laptop and the wiring on glass.
- **One image, many roles**, static plugin dispatch, no heap on the base build, a single
  radio time-shared between the mesh and a WiFi burst — the constraints are the interesting
  part, and they're all documented.

**Talk framing (RustConf / Supercon):** *"A dollar MMO: shipping a real `no_std`-Rust ESP-NOW
fleet — games, signed OTA, and a creature that migrates when you pull the plug."*

---

## We didn't just build it — we studied the design space

A distinguishing feature for a hobby project: a **research shelf** in-repo. Before adopting
heavyweight ideas, smol studied them and wrote down *why* it did or didn't borrow — the
recurring verdict being **"borrow the primitive, admire the protocol."**

- **[Althea / Babel (RFC 8966)](superpowers/research/althea-babel-study.md)** → borrow the ETX
  link metric, admire the routing protocol (a single-sink 2-hop mesh doesn't need any-to-any
  distance-vector).
- **[A mesh ledger](superpowers/research/mesh-ledger-study.md)** → adopt a hash-chained
  append-only log (tamper-evident fleet provenance); skip BFT — the elected crown is a free
  sequencer.
- **[RIOT OS's ESP-NOW netdev](superpowers/research/riot-espnow-study.md)** → validation by
  contrast: RIOT *forbids* the broadcast-flood-plus-WiFi combo smol is built on, confirming
  smol's flood-first design is genuine novelty.
- **[Inspirations coverage audit](superpowers/research/inspirations-coverage.md)** → of ~33
  borrows across 18 projects, ~79% shipped — a durable "did we build what we learned?" ledger.
- Plus design specs for the mesh-RPG, the visualizers, and mesh authentication.

That shelf is the "credible, not just cute" evidence a Hackaday or conference audience respects:
the project knows the literature and made deliberate calls.

---

## Play it in your browser — the interactive hook

The killer web hook is the **web emulator** ([#152](https://github.com/jphein/smol/issues/152),
in progress): compile smol's **real** pure game/render cores to **WASM** and run them on the
project site — a 72×40 canvas styled as the glowing OLED, your keyboard mapped to the button,
**the actual firmware plugin code drawing every pixel** (not a reimplementation). It's cheap to
build *because* of the pure/driver discipline above — the same host-testable cores that run on
your laptop run in the browser. "**Play the actual firmware, in your browser**" turns a
read-the-writeup visitor into a hands-on one.

🌐 Live site today: **https://jphein.github.io/smol/**

---

## The facts box (citable, honest)

| | |
|---|---|
| **Board** | ESP32-C3 SuperMini — **$1.00 headless / $2.76 with the 0.42″ (72×40) OLED** |
| **RAM / core** | ~400 KB SRAM, 160 MHz single-core RISC-V (rv32imc), no PSRAM |
| **Firmware** | one `no_std` Rust binary (esp-hal), static plugin dispatch, no-heap base build |
| **Radio** | ESP-NOW (connectionless, no router/cloud) + a time-shared WiFi burst for the gateway |
| **OTA** | dual-slot A/B, ed25519-signed, mesh-relayed to WiFi-less leaves (~1 MB over ESP-NOW), brick-safe rollback |
| **Proven** | on real hardware (id7/id8/id9 fleet): Familiar migration, routed multi-hop, signed leaf-OTA, self-healing elections |
| **One-of-a-kind claim** | the most complete `no_std`-Rust ESP-NOW reference fleet we know of |
| **The hook** | *a $1 computer as a mesh-MMO node* |

---

## Suggested next steps (for #59)

1. **Hackaday writeup** — lead with the Familiar (pull-the-plug jump) + the pocket-watch case;
   the honest-$1-board hook; link the web emulator once #152 lands.
2. **RustConf / Supercon talk** — smol as the esp-rs / `no_std` ESP-NOW reference; the
   pure/driver discipline as the transferable lesson.
3. **Position the repo + site** as that reference (README already does much of this).
4. **Timing:** the story is strongest *after* the web emulator (#152) ships — the interactive
   hook is what converts readers into players. The Familiar (#57) and the arc are already live.
