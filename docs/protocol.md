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
| [BATT](#batt--ha-battery-snapshot) | `SMOLv1 BATT ` | ≤108 | broadcast | on-recv + periodic | espnow | 🟡 |
| [RELAY](#relay--relayack--espnow--internet-telemetry) | `SMOLv1 RELAY ` | ≤91 | broadcast | ~15 s (leaf) | espnow | 🟢 |
| [RELAYACK](#relay--relayack--espnow--internet-telemetry) | `SMOLv1 RELAYACK ` | 25 | unicast | reactive | espnow | 🟢 |
| [SNK](#snk--mmo-mesh-snake) | `SMOLv1 SNK ` | 18 | broadcast | 5 Hz jittered | espnow | 🟢 |

> **The battery downlink is two hops.** The [`SMOLv1 BATT`](#batt--ha-battery-snapshot)
> frame above is the *mesh* hop (gateway → leaves). It's **fed** by an
> [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) on the
> LAN (gateway ↔ Home Assistant's Mosquitto broker) — plain TCP, not a mesh frame,
> so that transport is documented in its own section below, where the old UDP
> collector egress used to live. (v2 pivot: MQTT-native, retiring the collector.)

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

## BATT — HA battery snapshot

**Purpose.** Carry Home Assistant battery voltages to **every** display over the
mesh. A **gateway** fetches a display-ready line set from HA (via the [MQTT
burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) below) and
broadcasts it as a BATT frame so **leaves** — which never touch WiFi — render it
from cache too. The **gateway is the sole broadcaster** (it's the single source from
HA): it emits **on receipt** of a fresh downlink, then **periodically re-emits**
(borrowing the TIME frame's tick) so a leaf that missed a burst still converges.
**Unlike TIME, leaves do *not* re-broadcast** — so BATT is **single-hop**: gateway →
its direct ESP-NOW neighbours only (see Cadence for why). (This is the *HA* battery
— distinct from a board's own on-board LiPo readout, `sensors::batt_v`, shown by the
Clock app.)

**Layout (≤ 108 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 12 | `b"SMOLv1 BATT "` | namespace (mirrors `b"SMOLv1 TIME "`) |
| `payload` | ≤ 96 | ASCII, display-ready | the `BATT\|`-marked display lines, **verbatim** from `smol/display/batt` |

> **Payload framing (LOCKED — spec v2 "Pinned byte-layouts", team-lead ruling
> 2026-07-08).** After the tag, the frame carries the **verbatim** retained
> `smol/display/batt` payload **including its `BATT\|` marker** — e.g.
> `SMOLv1 BATT BATT|48V 52.8V|HV 391.9V|d 43mV`. **No length byte:** payload length
> = `frame_len − 12`. Frame payload and `BattCache` are therefore **byte-identical**
> — one `memcpy`, no reformatting on either the broadcast or the receive side.

**Payload format (pinned).** The display lines are `BATT|<line1>|<line2>|<line3>`
— pipe-separated, **≤ 3 lines, ≤ 12 chars each, ≤ 96 B total**, no trailing pipe.
Numbers render `%.1f` (voltage) / `%.0f` (delta mV); unavailable source entities
render `--` with the label kept, e.g. `BATT|48V --|HV --|d --`. Default fresh
content: `BATT|48V 52.8V|HV 391.9V|d 43mV` (System A 48 V LFP bank · System B
BMW-i3 HV pack · System B cell-spread). Worst case on the wire: `12 + 96 = 108 B`,
well under the 250 B ESP-NOW limit.

**Cadence.** Broadcast **only by the gateway**, ~every **10 s**, gated on
`is_gateway && !cache.is_empty()` (the `main.rs` background block) — plus a fresh
emit whenever the MQTT burst pulls a new retained downlink. **Leaves are
receive-only: they cache what they hear and never originate _or_ re-broadcast a BATT
frame** — so reach is **single-hop** (gateway → its direct ESP-NOW neighbours only;
a leaf two hops out won't see it). This is the deliberate difference from TIME, which
every node re-floods: BATT carries **no freshness field**, so a leaf re-broadcast
could overwrite a fresher cache or loop. (Design call — morpheus-batt-firmware,
Stage 3.)

**Rule.** On RX: validate the `SMOLv1 BATT ` tag (and the `BATT|` marker in the
payload), copy the payload into the local `BattCache`, stamp `fetched_at_ms = now`.
The Batt plugin renders the cached lines + that age. The age is a **fetch-age** (when
*this node* last received a downlink), **not** the HA data's age — see the
[downlink staleness contract](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector)
for why that distinction matters. A stale cache just shows old numbers with a growing
age, never a crash.

**Flag.** espnow (the frame itself) — but the cache is populated only when a
`wifi`/`espnow` **gateway** is present to run the MQTT burst. A `default` build
neither fetches nor broadcasts.

**Security.** Unauthenticated, unencrypted — like every SMOLv1 frame: anything on
ch 6 can broadcast a forged `SMOLv1 BATT ` frame and paint bogus voltages on every
display. The data is non-secret, and crucially the HA **broker password never
rides the mesh** — it lives only in the gateway's git-ignored `secrets.rs`, used
solely for the LAN TCP CONNECT. Documented, not defended.

**Status.** 🟡 **compile-verified — pending hardware verify.** Frame layout + payload
format are **locked** to spec v2 (byte-layout box above); Stage 3 landed in tree and
passed all six build/clippy gates (per morpheus-batt-firmware). **No board has
broadcast or rendered a real BATT frame yet** — no flash.

**Source.** Firmware `rust/clock/src/net/mode.rs`: `BATT_PREFIX` (`:139`,
`b"SMOLv1 BATT "`), `broadcast_batt` (`:1259`, `memcpy(tag)` ++ `memcpy(cache.bytes())`),
RX `Frame::Batt(&[u8])` (`:227`, payload = `data[12..]`); render in `batt.rs`
(`BattCache`); broadcast gated in `main.rs`. Fed by the [MQTT
burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) in
`net/wifi.rs`; team spec `scratch/smol-ha-batt/spec.md` v2 (§ Architecture — Downlink).

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
by `RELAY_FLUSH_BUDGET ≈ 15 s` (tuned up from 6 s for real-AP DHCP, `652155b`), and a failed flush backs off a full interval and sheds
the oldest queued message so a dead AP can't freeze the node (finding-1 fix).
*v2 (compile-verified — pending hardware verify):* this UDP egress is being replaced by an
[MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) straight
to Home Assistant — the ESP-NOW RELAY frame here is **unchanged**, only the gateway's
internet hop moves.
**Out of scope (documented stubs):** downlink (server → leaf) and multi-hop
routing (needs a next-hop/TTL header, +200–400 LOC).
**Flag.** espnow. **Status.** 🟢 **hardware-proven end-to-end** — on the wave-6 fleet
(build 36 "Oxidized Spark", `bcafa7e`) leaf id8's telemetry reaches
`disks:~/smol-collector/collector.jsonl` as `node_id 8`, sustained ~02:06Z→02:44Z. The
flush's budget/backoff/dedup and the drain-until-egress path were hardened over five
Oracle passes (`2ea7c4d`, `652155b`, `ca5d985`; see `morpheus-oracle-fixes.md`). One
non-blocking cold-ARP first-round nit remains — see [relay.md](relay.md#the-flush-cycle).
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
| `flags` | 1 | u8 | **bit 0** alive · **bits 1–2** heading (0=U 1=R 2=D 3=L) · **bits 3–7** active-power (0=none, 1–6 = Phantom/Haste/Shield/Midas/Reveal/Phoenix, 7–31 reserved) — layout **final** |
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
**Flag.** espnow. **Status.** 🟢 **flashed on the fleet** — build 36 "Oxidized Spark" (`bcafa7e`),
all three boards; powers + leaderboard live (compile-clean across all 3 builds). The `flags`
bit-layout is **final** (bit 0 alive · bits 1–2 heading · bits 3–7 active-power, `POWER_COUNT = 6`),
defined in `mesh_snake/snake_core.rs`.
**Source.** `mmo-snake-netcode.md` (§1/§5) + `mmo-snake-design.md` (§7).

---

## MQTT burst — the LAN transport that retires the UDP collector

**What changed (v2 pivot).** v1 shipped a Python **collector** on disks that took
relay telemetry over UDP `:9999` and answered a `BATT?` query. **v2 retires it.**
The board now speaks **MQTT 3.1.1** (hand-rolled, QoS 0, no TLS) directly to Home
Assistant's **Mosquitto** broker — so telemetry lands as native HA entities and the
battery downlink is a retained broker message. No Python middleman in the end
state. *(The UDP collector path — [relay.md](relay.md) — is still what's flashed
today; it's being replaced, not yet removed. Rollback = git.)*

**Not a mesh frame.** This is plain **TCP** on the MQTT port (1883) to **the HA VM's
leg on the boards' own VLAN** — the exact address is a compile-time const in the
gateway's git-ignored `secrets.rs` (see [BUILDING.md](BUILDING.md) → *Secrets*), kept
out of this doc on purpose. Mosquitto binds `0.0.0.0`, so the **multi-homed** HA VM
answers on every VLAN leg; the gateway deliberately targets the leg on the **boards'
own subnet** so board → broker is one L2 hop — no inter-VLAN routing, no gatekeeper,
and the CONNACK asymmetry (below) simply doesn't arise. *(Aim it at the wrong leg — a
**cross-VLAN** leg of the same VM, or the **unrelated** broker that happens to run on
the `disks` host — and you get either the silent-CONNACK hang or the wrong broker
entirely; the const in `secrets.rs` is the source of truth.)* Spoken only by a
**gateway** during its WiFi burst — no `SMOLv1 ` namespace, no channel 6. The
mesh-side result is the [`SMOLv1 BATT`](#batt--ha-battery-snapshot) frame the gateway
broadcasts after it fetches the downlink.

**The burst (per flush, ~30 s when the queue is non-empty).**
1. Associate WiFi (the COEXIST arm — as the old flush) → open a TCP socket to the
   broker.
2. **CONNECT** — MQTT 3.1.1, client id `smol-<gwid>` (the **gateway's own** id —
   never a relayed leaf's), clean session, keepalive 0 (the burst is short-lived;
   no PINGREQ). Auth: username `jp` + password from the gateway's git-ignored
   `secrets.rs` (anonymous auth is **off** on the broker). Wait for **CONNACK**.
3. **SUBSCRIBE** `smol/display/batt` (QoS 0) — the broker immediately delivers its
   **retained** message (the battery downlink). *The broker is the cache:* even
   though the gateway was away for ~30 s, the latest payload is waiting.
4. **PUBLISH** telemetry — one message per queued leaf telemetry **and** the
   gateway's own — to `smol/<id>/telemetry` (QoS 0, **not** retained).
5. **PUBLISH** retained, idempotent **discovery** configs (below) so HA
   auto-creates entities — cheap to repeat every burst.
6. Store the downlink in `BattCache`, broadcast the [`SMOLv1 BATT`](#batt--ha-battery-snapshot)
   frame to the mesh, **DISCONNECT**, return to ESP-NOW ch 6.

**Topics & payloads (byte contracts).**

| Topic | Retained | QoS | Payload |
|---|---|---|---|
| `smol/<id>/telemetry` | no | 0 | the **bare** telemetry line (sensor line + last peer/label) — the same string the RELAY carried, **no** legacy `NNN ` id prefix (the topic already carries the id). *(LOCKED — spec v2 "Pinned byte-layouts".)* |
| `smol/display/batt` | **yes** | 0 | `BATT\|<l1>\|<l2>\|<l3>` — the display payload (≤ 3 lines, ≤ 12 ch each, ≤ 96 B, `--` on unavailable). Published by an **HA automation** (see [`ha/README.md`](../ha/README.md)), not by a node. |
| `homeassistant/sensor/smol<id>/telemetry/config` | **yes** | 0 | HA MQTT-discovery JSON (below) — published by the **gateway** on each connect. |

*Wire detail (`mqtt.rs::encode_publish`):* each PUBLISH's fixed-header byte 0 is
`0x30 | retain` — QoS 0, RETAIN bit (bit 0) set only on the gateway's **retained**
publishes (the discovery configs); telemetry PUBLISHes are `0x30` (not retained). The
`smol/display/batt` downlink is set retained by **HA**, not the gateway.

**Discovery config (PINNED scheme — spec v2).** Retained JSON on
`homeassistant/sensor/smol<id>/telemetry/config`, republished on every connect:

```json
{
  "unique_id": "smol<id>_telemetry",
  "state_topic": "smol/<id>/telemetry",
  "name": "smol <id>",
  "device": { "identifiers": ["smol<id>"], "name": "smol <id> <noun>" }
}
```

The entity `name` is `smol <id>`; the **`device.name` appends the node's magical
realm noun** (e.g. `smol 7 Draconic`). Retained + idempotent → HA creates one
registry-managed text sensor per known node, grouped under a `smol<id>` **device**,
with **no HA config edits**.
**Node removal:** publish an **empty** retained payload to the same config topic
(ops-checklist item — see [`ha/README.md`](../ha/README.md)).

**Downlink staleness (HA-side contract).** The HA automation re-renders the retained
`smol/display/batt` payload on every source-entity change **and** on a 5-minute
`time_pattern`, and renders `--` for any source entity whose **`last_reported`** is
older than **30 minutes (1800 s)** — so a wedged integration can't freeze a
stale-but-live-looking voltage into the retained message. (`last_reported`, not
`last_updated`, is deliberate: it advances on **every** publish even when the value is
unchanged, so a steady-but-healthy sensor stays fresh while only a truly wedged one
goes stale; 30 min is the team ruling — tight enough to catch a dead MQTT / wedged
integration, loose enough not to blank the infrequently-publishing BE sensors.)
**Known limitation (accepted for v1):** if **HA
itself** dies, the last retained payload persists on the broker, and a board that
fetches it shows it with a *fresh* fetch-age — because the age on the glass is a
**fetch-age** (when the node last *received* a downlink), **not** the HA data's age.
A payload-embedded timestamp is a filed follow-up.

**Cross-VLAN CONNACK gotcha (understood, and side-stepped).** A connect to the
*wrong* leg can complete the TCP handshake yet have its **CONNACK silently never
return** (asymmetric return path: the VM replies out its default-gateway leg). That
is exactly why the target is the HA VM's leg on the **boards' own VLAN** — a
same-subnet path with no such asymmetry. `nebula-ha-pipe` verified a real CONNACK
from that VLAN (3/3, 2026-07-08). Residual risk is one flash-day smoke test: the
tested host was *wired* on that VLAN; a *wireless* board's L3 is identical, so
confidence is high but unproven on glass.

**Security.** Plain TCP, no TLS, LAN-only, QoS 0. The broker password lives **only**
in the gateway's git-ignored `secrets.rs` (+ `.example` placeholder) — never in
this doc, logs, JSONL, or commits. Telemetry + voltages are non-secret. Same
"documented, not defended" posture as the rest of the mesh.

**Status.** 🟡 **compile-verified — pending hardware verify.** Topic / payload / byte
contracts are **locked** to spec v2 (including the two pinned byte-layouts); Stage 3
landed and passed all six build/clippy gates (per morpheus-batt-firmware). The broker
path is CONNACK-verified from the boards' VLAN, but **no board has run an MQTT burst**
— the wireless flash-day smoke test is the last check.

**Source.** Firmware `rust/clock/src/net/mqtt.rs` (hand-rolled MQTT 3.1.1 / QoS 0):
`encode_connect` (`:85`), `encode_publish` (`:105`, RETAIN = bit 0 of byte 0),
`encode_subscribe` (`:117`); driven by `mqtt_session` (`net/wifi.rs:502`), entered
from `run_mqtt_burst` (`net/wifi.rs:674`); broker addr/creds in `secrets.rs`. HA side
`ha/packages/smol_mesh.yaml` + [`ha/README.md`](../ha/README.md); team spec
`scratch/smol-ha-batt/spec.md` v2.

---

## Honesty caveats

- **Verification is per-frame and current as of 2026-07-07.** HELLO/ACK and **TIME
  (2-board adoption)** are hardware-verified. BEACON is compile-verified (runs in
  Bench mode, not accuracy-checked). RELAY/RELAYACK are **hardware-proven e2e** (sustained
  `node_id 8` telemetry to the collector, wave 6). SNK is **flashed on the fleet** (build 36).
- **BATT frame + MQTT burst are compile-verified, not flashed (v2 pivot).** The
  [`SMOLv1 BATT` frame](#batt--ha-battery-snapshot) and the
  [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) that
  feeds it (MQTT-native, retiring the UDP collector) are **fully byte-locked** to
  spec v2 (tag + verbatim-payload framing, bare telemetry line, discovery scheme) and
  **landed in Stage 3, all six build/clippy gates green** (per morpheus-batt-firmware).
  The broker path is CONNACK-verified from the boards' VLAN — but **no board has run
  a burst or broadcast a BATT frame**; the wireless flash-day smoke test is the last
  check. Honest caveat: an HA outage leaves a live-looking retained payload (boards
  show fetch-age, not data-age).
- **ESP-NOW airtime/throughput/RX-reliability under COEXIST** are unmeasured on
  hardware — reasoned from the `esp-wifi 0.15.0` API (see `nebula-espnow-gateway.md`),
  not a bench run.
- **The code is authoritative.** RELAY sizes/fields especially may move while the
  bridge lands — re-check `mode.rs` before depending on the exact bytes.

## Sources
- `rust/clock/src/net/mode.rs` — frame consts, `Frame` enum, encode/parse helpers, relay bridge section (read-only).
- `rust/clock/src/net/wifi.rs` — the MQTT burst (hand-rolled MQTT 3.1.1 client) + broker consts (v2); replaces the UDP collector egress.
- `ha/packages/smol_mesh.yaml` + `ha/README.md` — the HA automation that publishes the retained `smol/display/batt` downlink + install/discovery notes.
- `collector/collector.py` — the v1 UDP relay collector, **being retired** (see [relay.md](relay.md)); superseded by the MQTT burst above.
- `scratch/smol-ha-batt/spec.md` (v2) — the MQTT-native architecture (uplink/downlink, discovery, retained) + role boundaries.
- `mesh-time-sync-spec.md`, `relay-bridge-spec.md`, `mmo-snake-netcode.md`, `mmo-snake-design.md` — design specs (scratch).
- `nebula-espnow-gateway.md` — verified ESP-NOW limits (esp-wifi 0.15.0) + the gateway feasibility verdict.
- `lucid-hw-verify.md` / `board1-boot-ANNOTATED.md` — today's hardware boot capture.
