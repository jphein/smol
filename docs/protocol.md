# SMOLv1 — the smol mesh wire protocol

The canonical reference for every frame smol sends over ESP-NOW. The frame zoo
has outgrown what the code comments can carry, so this is the single source of
truth: exact byte layout, cadence, staleness rules, which feature flag compiles
it, and — honestly labelled — how far each frame has actually been verified.

> Source of truth is the code: `rust/clock/src/net/mode.rs` (frame consts,
> `Frame` enum, `encode_*`/`parse_*` helpers). Design-only frames cite their
> scratch spec. Where this doc and the code disagree, **the code wins** — fix
> this doc.

## Verification legend (honest-status discipline)

| Mark | Means |
|---|---|
| 🟢 **hardware-verified** | observed working on real boards today |
| 🟡 **compile-verified** | builds clean (`cargo build` + `clippy -D warnings`), not (fully) exercised on hardware |
| 🔵 **in progress** | code in tree but **uncommitted**, implementation still moving |
| ⚪ **design** | specified in a scratch doc, **not yet in code** |

---

## The single-radio constraint (read this first)

*(condensed from the `mode.rs` header — the reason the protocol looks the way it does)*

The ESP32-C3 has exactly **one 2.4 GHz radio and one PHY**, tunable to **one
channel at a time**. WiFi (infrastructure STA) and ESP-NOW are **not two radios**
— they are two ways of using the same PHY. ESP-NOW frames are vendor-specific
WiFi *action* frames, so a receiver only hears them on the channel it is
currently tuned to. Two consequences drive every design choice below:

- **COEXIST** — stay associated to the AP and pin ESP-NOW to the **AP's**
  channel. WiFi (NTP, relay-flush) stays available, but every peer must discover
  and match the AP's channel (which can change, e.g. band-steering).
- **TIME-SHARE** — drop the WiFi association and pin the PHY to a **fixed**
  ESP-NOW channel all peers agree on (`ESP_NOW_FIXED_CHANNEL = 6`). Deterministic
  and low-power, but there is **no WiFi while in ESP-NOW mode**.

**smol's default is TIME-SHARE:** a WiFi burst at boot (associate → DHCP → SNTP),
then the radio is pinned to **ch 6** for the mesh. The [relay bridge](#relay--relayack--espnow--internet-telemetry)
resurrects a COEXIST/WiFi-return flush — and **the mesh is deaf during that burst**
(single radio). Everything in steady state rides ch 6.

Verified ESP-NOW limits (from `esp-wifi 0.15.0` source — see `nebula-espnow-gateway.md`):
**250 B** max payload/frame, RX queue **10 frames deep (drops oldest)**,
**synchronous one-in-flight TX** (`send()` → `waiter.wait()`). Every SMOLv1 frame
stays well under 250 B.

---

## Shared conventions

- **Namespace.** Every frame begins `b"SMOLv1 "` (7 B) + a tag word — keeps the
  mesh greppable in a serial sniffer and namespaced off other ESP-NOW traffic on
  the channel.
- **Encoding discipline.** HELLO / ACK / BEACON / TIME and the RELAY *header* +
  RELAYACK are **fixed-width zero-padded ASCII** (human-readable). **SNK** breaks
  to **binary-after-prefix** for density (justified by its 5 Hz rate).
- **Addressing.** All frames are **broadcast** except **ACK** and **RELAYACK**,
  which are **unicast** to a known peer MAC (the peer is auto-registered via
  `add_peer` on first HELLO/RELAY).
- **Staleness idiom.** Monotonic-ms timestamps; `PEER_STALE_MS = 3000 ms`. Link
  state decays `Connected → Detected → Idle` as frames stop arriving. Reused by
  every layer that tracks a peer.
- **Feature gating.** The mesh exists **only under `--features espnow`** — the
  *entire* frame set below is `#[cfg(feature = "espnow")]`. The `default` and
  `wifi` builds send no ESP-NOW frames.
- **Security.** ESP-NOW here is **unauthenticated and unencrypted** — any device
  on the channel can inject any frame (a bogus far-future `synced_at` can hijack
  every mesh clock; a forged RELAYACK can stall a leaf). Acceptable for a hobby
  mesh on a private fixed channel; harden with a signed payload or an ESP-NOW LMK
  if it ever matters. Documented, not defended.

---

## Frame summary

| Frame | Tag | Bytes | Cast | Cadence | Flag | Status |
|---|---|---|---|---|---|---|
| [HELLO](#hello--led-handshake) | `SMOLv1 HELLO ` | 16 | broadcast | ~2 s | espnow | 🟢 |
| [ACK](#ack--led-handshake) | `SMOLv1 ACK ` | 14 | unicast | reactive | espnow | 🟢 |
| [BEACON](#beacon--bench-link-stats) | `SMOLv1 BEACON ` | 29 | broadcast | ~2 s (Bench) | espnow | 🟡 |
| [TIME](#time--mesh-time-sync) | `SMOLv1 TIME ` | 37 | broadcast | ~2 s | espnow | 🟢 |
| [RELAY](#relay--relayack--espnow--internet-telemetry) | `SMOLv1 RELAY ` | ≤91 | broadcast | ~15 s (leaf) | espnow | 🟡 |
| [RELAYACK](#relay--relayack--espnow--internet-telemetry) | `SMOLv1 RELAYACK ` | 25 | unicast | reactive | espnow | 🟡 |
| [SNK](#snk--mmo-mesh-snake) | `SMOLv1 SNK ` | 18 | broadcast | 5 Hz jittered | espnow | 🟡 |

---

## HELLO — LED handshake

**Purpose.** Periodic "I'm here" advertisement. Hearing any HELLO proves a peer
is in range (`Detected`); it also registers the sender as a unicast peer and
triggers an ACK back.

**Layout (16 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 13 | `b"SMOLv1 HELLO "` | namespace |
| `id` | 3 | ASCII `NNN` (000–255) | sender's peer id |

**Cadence.** Broadcast every ~2 s (the HELLO tick, `main.rs`).
**Rule.** On RX: `last_hello_ms = now` → `Detected`; `add_peer(src)` if new; reply
with a unicast **ACK** echoing the sender's id.
**Flag.** espnow. **Status.** 🟢 **hardware-verified** — two boards reach solid-blue
`Connected` (LED handshake), confirmed again today (board 1 Idle→Connected on bench).
**Source.** `mode.rs` `HELLO_PREFIX`, `encode_id_frame`, `parse_frame`.

## ACK — LED handshake

**Purpose.** "I heard you, `<id>`." An ACK carrying **our** id proves the link is
bidirectional (`Connected`).

**Layout (14 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 11 | `b"SMOLv1 ACK "` | namespace |
| `id` | 3 | ASCII `NNN` | the id being acknowledged |

**Cadence.** Reactive — **unicast** to the source MAC of each heard HELLO.
**Rule.** On RX with `acked_id == my id`: `last_ack_for_us_ms = now` → `Connected`.
ACKs for other ids are peer-to-peer chatter, ignored.
**Flag.** espnow. **Status.** 🟢 **hardware-verified** (same handshake as HELLO).
**⚠️ Do not alter** the HELLO/ACK wire format — it is the hardware-verified LED path.

## BEACON — bench link stats

**Purpose.** Bench-mode link telemetry layered on top of the handshake: RTT,
per-second TX/RX rate, packet loss, RSSI.

**Layout (29 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 14 | `b"SMOLv1 BEACON "` | namespace |
| `id` | 3 | ASCII `NNN` | sender id |
| ` ` | 1 | space | |
| `seq` | 5 | ASCII `SSSSS` (mod 100000) | sender's outbound sequence |
| ` ` | 1 | space | |
| `echo` | 5 | ASCII `EEEEE` | highest peer seq the sender had heard |

**Cadence.** **Bench mode only**, on the ~2 s tick (in addition to HELLO).
**Rule.** RTT = `now − send_time[echo]` when `echo` matches a seq we sent; loss
from forward gaps in the peer's `seq`; RSSI from `rx_control.rssi`. Also counts as
`Detected`.
**Flag.** espnow (Bench). **Status.** 🟡 **compile-verified**; runs in the Bench
mode of the flashed firmware (link numbers rendered on the OLED), not
independently bench-validated for accuracy today.
**Source.** `mode.rs` `BEACON_PREFIX`, `encode_beacon`, `BenchTracker`.

## TIME — mesh time sync

**Purpose.** Let boards agree on the clock over the mesh. A board that never
reached WiFi picks up correct time from a meshed board that did; among synced
boards, the older-sync one converges onto the newer one and then they stop.

**Layout (37 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 12 | `b"SMOLv1 TIME "` | namespace |
| `id` | 3 | ASCII `NNN` | sender id |
| ` ` | 1 | space | |
| `unix` | 10 | ASCII (full u32) | sender's current Unix-time estimate |
| ` ` | 1 | space | |
| `synced_at` | 10 | ASCII (full u32) | Unix time of sender's **last authoritative NTP sync** (0 = never) |

**Cadence.** Broadcast on the ~2 s HELLO tick.
**Authority model (loop-free).** Adopt a peer's time **iff `peer.synced_at >
my.synced_at`** (strict); on adopt, **inherit the peer's `synced_at`** (not
`now`). Equal → ignore (prevents ping-pong). Freshness travels with the time, so
no node's `synced_at` can exceed the origin NTP node's → `A→B→C→A` cannot inflate;
the mesh converges and stops. Predicate: `should_adopt(mine, peer) = peer > mine`.
A TIME frame also counts as `Detected`.
**Flag.** espnow. **Status.** 🟢 **hardware-verified — 2-board adoption verified
2026-07-07.** Built clean (`cargo` + `clippy -D warnings`, all 3 builds) and flashed;
id 8 *Eldritch Nexus* (started at `synced_at = 0`) **adopted** id 7's exact
`synced_at = 1783467581` over ESP-NOW, then **re-converged** when id 7's stamp
advanced on reboot (…8465). Zero panics. (Committed in `76b19e4`.)
**Security.** Unauthenticated → a forged far-future `synced_at` hijacks every clock
(see Shared conventions).
**Source.** `mode.rs` `TIME_PREFIX`, `encode_time`, `write_u10`/`parse_u10`,
`TimeTracker`; `main::should_adopt`; spec `mesh-time-sync-spec.md`.

## RELAY / RELAYACK — ESP-NOW → internet telemetry

**Purpose.** Single-hop message-relay bridge. A **leaf** (in ESP-NOW range of a
gateway but out of WiFi range) fragments its **short telemetry** (sensor line +
last peer/label) into RELAY frames and broadcasts them. A **gateway** (associated
to an AP at boot) reassembles keyed by `(src MAC, msgid)`, unicasts a RELAYACK
bitmap so the leaf resends only missing fragments, buffers completed messages, and
periodically runs a WiFi flush burst to UDP them to a collector.
**Browsing is explicitly out** — 250 B MTU, <100 kbps lossy goodput, one radio
(see `nebula-espnow-gateway.md`). This is telemetry, not a general gateway.

**RELAY layout (27 B header + ≤64 B chunk = ≤91 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 13 | `b"SMOLv1 RELAY "` | namespace |
| `src_id` | 3 | ASCII `NNN` | originating leaf id |
| ` ` | 1 | space | |
| `msgid` | 5 | ASCII `MMMMM` (u16) | per-source rolling message id |
| ` ` | 1 | space | |
| `frag` | 1 | ASCII `F` | fragment index (0 … count−1) |
| ` ` | 1 | space | |
| `count` | 1 | ASCII `C` | total fragments (1 … `RELAY_MAX_FRAGS`) |
| ` ` | 1 | space | |
| `chunk` | ≤64 | bytes | telemetry payload fragment |

**RELAYACK layout (25 B), unicast leaf-ward.**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 16 | `b"SMOLv1 RELAYACK "` | namespace |
| `msgid` | 5 | ASCII `MMMMM` | message being acked |
| ` ` | 1 | space | |
| `bitmap` | 3 | ASCII `BBB` (u8) | bit *i* set = fragment *i* received |

**Sizing constants** (`mode.rs`): `RELAY_CHUNK = 64`, `RELAY_MAX_FRAGS = 4` →
`RELAY_MAX_MSG = 256 B` max reassembled telemetry (bigger telemetry is truncated —
this is short-telemetry relay, not bulk transfer).
**Timing constants:** leaf emits every `RELAY_EMIT_INTERVAL_MS = 15 s`; waits
`RELAY_RETX_MS = 2 s` for a fuller RELAYACK before resending gaps, bounded to
`RELAY_MAX_TRIES = 3`; reassembly evicts on completion or `RELAY_STALE_MS = 10 s`;
gateway flushes buffered messages to the collector every `RELAY_FLUSH_INTERVAL_MS
= 30 s`.
**Gateway flush.** Switches to `WifiSta` (COEXIST arm — resurrected here), UDP-sends
`"<src_id> <telemetry>"` to `RELAY_COLLECTOR_IP:RELAY_COLLECTOR_PORT` — the **disks**
host `10.0.11.117:9999` on VLAN 11 (same-subnet L2, no gatekeeper hop), mirroring
`NTP_SERVER_IP`'s hardcoded-IP style — then switches back to ESP-NOW ch 6. The burst
**stalls display + input + mesh** for its duration (single radio); it is hard-bounded
by `RELAY_FLUSH_BUDGET ≈ 6 s`, and a failed flush backs off a full interval and sheds
the oldest queued message so a dead AP can't freeze the node (finding-1 fix).
**Out of scope (documented stubs):** downlink (collector → leaf) and multi-hop
routing (needs a next-hop/TTL header, +200–400 LOC).
**Flag.** espnow. **Status.** 🟡 **compile-verified** — `Frame::Relay`/`Frame::RelayAck`,
the reassembly tables, and the gateway flush are **committed** (`76b19e4`) and build
clean across all 3 builds (`cargo build` + `clippy -D warnings`). **Not yet exercised
on hardware** (needs a gateway + leaf + a UDP collector). The flush's failure-backoff
and post-completion dedup were hardened after an adversarial review — see
`scratch/smol/morpheus-oracle-fixes.md`.
**Security.** Unauthenticated → a forged RELAYACK can stall a leaf's retransmit.
**Source.** `mode.rs` relay-bridge section (`RELAY_PREFIX`, `RELAYACK_PREFIX`,
`Relay`/`RelayTx`/reassembly); spec `relay-bridge-spec.md`.

## SNK — MMO mesh snake

**Purpose.** Every board's snake shares **one world**; a scrolling viewport
follows your own head (no walls). Each node is the **sole authority over its own
snake** and broadcasts an **absolute, stateless** head snapshot; peers
**reconstruct** the body by dead-reckoning the observed head path — the body is
**never on the wire** (a full 144-segment body would be 288 B > 250 B, so it
*cannot* be sent; head-only is forced, and is also the loss-tolerant choice).

**Layout (18 B, binary-after-prefix).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 11 | `b"SMOLv1 SNK "` | namespace (sniffer-greppable) |
| `ver` | 1 | u8 | frame format version (=1) |
| `id` | 1 | u8 | sender snake id |
| `tick` | 1 | u8 (wrapping) | step counter — ordering + dead-reckon base |
| `flags` | 1 | u8 | **heading (2 bits) + alive (1 bit) + 5-bit active-power field** (0..31, 0 = none; design v2, in flight) |
| `head_x` | 1 | u8 | world cell X (toroidal) |
| `head_y` | 1 | u8 | world cell Y (toroidal) |
| `length` | 1 | u8 | segment count; **body dead-reckoned, not sent** |

**World / coords.** `u8` per axis → up to **256×256 toroidal** world (design may
mask to 128/axis via `x & 0x7F`); world size is a game knob. `MAX_PEERS = 16`.
**Cadence.** **5 Hz (200 ms)**, one snapshot per movement step, **per-id
phase-jittered** — fire at `step_boundary + (id % Nmax)·(period/Nmax)`. Jitter is
**mandatory above N≈8**: the shared mesh clock aligns naive ticks, which
burst-overflows the 10-deep RX queue.
**Rules.** Absolute snapshot (not delta) — every frame fully refreshes a peer, so
it survives 10–30 % loss. Between frames, advance a peer's head along its heading
by `min(elapsed/STEP_MS, 3)` cells (clamped). `tick` wrap-order drops stale
frames. Despawn via the `PEER_STALE_MS` idiom. Dead snakes still announce
(`alive=0`) so peers clear them fast.
**Playable N.** smooth ≤ 8, good 12–16 (jittered), graceful degradation beyond.
**Flag.** espnow. **Status.** 🟡 **committed + compile-verified** (`6baea36`; builds clean
across all 3 builds, not yet hardware-exercised). The `flags` byte is **design v2** (heading 2 b +
alive 1 b + a **5-bit active-power field**, 0..31); the active-power bits may still shift as
treasures/powers land — confirm against the current build before relying on the exact bit layout.
**Source.** `mmo-snake-netcode.md` (§1/§5) + `mmo-snake-design.md` (§7).

---

## Honesty caveats

- **Verification is per-frame and current as of 2026-07-07.** HELLO/ACK and **TIME
  (2-board adoption)** are hardware-verified. BEACON is compile-verified (runs in
  Bench mode, not accuracy-checked). RELAY/RELAYACK are **committed + compile-verified**
  (not yet hardware-exercised). SNK is **committed** (see its own section).
- **ESP-NOW airtime/throughput/RX-reliability under COEXIST** are unmeasured on
  hardware — reasoned from the `esp-wifi 0.15.0` API (see `nebula-espnow-gateway.md`),
  not a bench run.
- **The code is authoritative.** RELAY sizes/fields especially may move while the
  bridge lands — re-check `mode.rs` before depending on the exact bytes.

## Sources
- `rust/clock/src/net/mode.rs` — frame consts, `Frame` enum, encode/parse helpers, relay bridge section (read-only).
- `mesh-time-sync-spec.md`, `relay-bridge-spec.md`, `mmo-snake-netcode.md`, `mmo-snake-design.md` — design specs (scratch).
- `nebula-espnow-gateway.md` — verified ESP-NOW limits (esp-wifi 0.15.0) + the gateway feasibility verdict.
- `lucid-hw-verify.md` / `board1-boot-ANNOTATED.md` — today's hardware boot capture.
