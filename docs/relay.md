# Relay bridge — ESP-NOW → internet telemetry (operator guide)

smol's relay lets a board that's **out of WiFi range** still get its short
telemetry to the internet, by hopping through a nearby board that **is** on WiFi.
This is the firmware-side operator guide; for the LAN receiver see
[`collector/README.md`](../collector/README.md).

**What it is — and isn't.** Single-hop, **short telemetry only** (the sensor line
+ last peer/label). It is **not** browsing or a general gateway: ESP-NOW's 250 B
frame limit and one-radio reality make bulk/interactive traffic impractical (the
full analysis is in `scratch/smol/nebula-espnow-gateway.md`; the wire frames are in
[protocol.md](protocol.md#relay--relayack--espnow--internet-telemetry)).

## Roles: leaf vs gateway (creds decide, automatically)

A board picks its role **at boot**, from whether it reached **DHCP** — **decoupled
from NTP** (N3c): a board that associates + gets a lease is a gateway *even if the
NTP sync misses*.

- **Gateway** — associated to an AP **and got a DHCP lease** at boot (`is_gateway`,
  `main.rs`). It **receives** RELAY fragments from leaves, reassembles them, and
  periodically bridges them to the collector over a WiFi burst.
- **Leaf** — no AP / no lease (out of range or no creds). It **emits** its telemetry
  as RELAY fragments over ESP-NOW and never flushes.

The role is logged at boot with its criteria, so it's auditable — e.g.
`GATEWAY (assoc+dhcp true, ntp ok)` or `leaf (assoc+dhcp false, ntp miss)`.

No configuration flag — the role follows the boot association. Put creds on the
board you want to be the gateway (see [BUILDING.md](BUILDING.md) → *Secrets*), keep
it in WiFi range, and leave the far boards credential-less (or out of range) as
leaves.

## The flush cycle

> **⚠️ v2 shipped — MQTT-native (hardware-verified, build 40 "Pressed Oven",
> 2026-07-08).** The gateway's LAN egress is now an **MQTT burst** straight to Home
> Assistant (step 4), and the **UDP collector is retired** (stopped/disabled, JSONL
> archived). Steps 1–2 (the ESP-NOW relay/reassembly) are unchanged and current;
> step 3 describes the **superseded** UDP flush, kept for context. One honest hold-out:
> leaf-side `SMOLv1 BATT`-frame *receipt* is still inferred (finding #15).

1. **Leaf emits** fresh telemetry every `RELAY_EMIT_INTERVAL_MS` (**15 s**) as up to
   `RELAY_MAX_FRAGS` (**4**) `SMOLv1 RELAY` fragments of `RELAY_CHUNK` (**64 B**)
   each — so up to **256 B** per message (longer telemetry is truncated).
2. **Gateway reassembles** by `(src_mac, msgid)` (bounded: `REASSEMBLY_SLOTS` = 3),
   and unicasts a `SMOLv1 RELAYACK` bitmap so the leaf **retransmits only the
   missing fragments** (`RELAY_RETX_MS` = 2 s wait, `RELAY_MAX_TRIES` = 3). Partial
   reassemblies older than `RELAY_STALE_MS` (10 s) are dropped.
3. **Gateway buffers** completed messages (bounded queue `GATEWAY_QUEUE` = 4) and,
   every `RELAY_FLUSH_INTERVAL_MS` (**30 s**) if the queue is non-empty (or at once
   when full), runs a **flush burst**: switch to WiFi-STA (the COEXIST arm),
   `run_udp_flush` UDP-sends each `"NNN <telemetry>"` datagram to the collector,
   then switches back to ESP-NOW ch 6. The flush **drains until the datagrams
   actually egress** the interface — bounded ~2 s (`ca5d985`, "finding N3") — **not
   a fixed post-send delay**: a warm interface flushes fast, a slow one still
   completes within the bound.
4. **v2 — the flush burst is now an MQTT burst** *(hardware-verified since build 40;
   current fleet build 45 "Oxidized Die" — leaf-BATT receipt inferred, #15)*. Instead of
   `run_udp_flush` → the UDP collector, the same WiFi window
   **connects to Home Assistant's Mosquitto broker** (the HA VM's leg on the boards'
   own VLAN — const in the gateway's git-ignored `secrets.rs`, see
   [BUILDING.md](BUILDING.md); plain TCP, hand-rolled MQTT 3.1.1 / QoS 0),
   **PUBLISHes** each queued telemetry to
   `smol/<id>/telemetry` (which become native HA entities via MQTT discovery),
   **SUBSCRIBEs** `smol/display/batt` **and** `smol/display/grid` to pick up HA's
   **retained** battery (6-segment: voltage + big SOC/charge pages, #16/#17) and
   grid-power payloads (the broker is the downlink cache — the latest is waiting even
   after 30 s away), then **broadcasts [`SMOLv1 BATT`](protocol.md#batt--ha-battery-snapshot)
   + [`SMOLv1 GRID`](protocol.md#grid--ha-grid-power-snapshot-16) frames** (gateway-only
   — its neighbour leaves cache them but never re-broadcast, so both are single-hop) and
   DISCONNECTs back to ch 6. The **UDP collector egress is retired** (as of build 40; the
   disks service is stopped/disabled and its JSONL archived — rollback = git). *(A pending
   firmware wave adds a third SUBSCRIBE — `smol/<id>/config/default_screen` for the
   [node manager](protocol.md#config--retained-per-node-default-screen-21-specd--firmware-pending),
   #21.)* Full byte contracts:
   [protocol.md → MQTT burst](protocol.md#mqtt-burst--the-lan-transport-that-retires-the-udp-collector).

> **Known follow-up (not a blocker):** each flush rebuilds the interface, so its
> *first* round-trip hits a **cold ARP cache**. The ~2 s egress bound occasionally
> loses that first cold-ARP edge → one `TX drain timed out`, after which backoff +
> retry lands the message (seen once on board 3 at wave 6; board 1 won the same
> race and flushed clean). Filed: a cold-ARP first-round retry / **pre-warming** the
> ARP entry so every flush delivers on the first attempt.

### The single-radio cost (honest)
A flush burst tunes the one PHY to the AP's channel, so **the mesh is deaf during
the burst** — ESP-NOW HELLO/TIME/SNK aren't heard for its duration. This is the
same TIME-SHARE trade-off boot NTP uses. It's acceptable because flushes are tens
of seconds apart and telemetry is loss-tolerant (the leaf's retransmit rides over
a missed window), but **don't run a high-rate game and a busy relay gateway on the
same board** expecting both to stay smooth.

The v2 **MQTT burst** replaces that fire-and-forget UDP `sendto` with a heavier
conversation — TCP 3-way handshake → CONNECT/CONNACK → SUBSCRIBE/SUBACK → PUBLISH
telemetry + discovery → retained downlink → DISCONNECT, then the `SMOLv1 BATT`
re-broadcast. **The honest news: it does *not* widen the deaf window.** Per the
Stage-3 code, the MQTT session is a **≤ 3 s sub-bound that runs *inside* the WiFi
association the flush already holds** (`net/wifi.rs`: `run_mqtt_burst` → `mqtt_session`)
— it spends part of the existing `RELAY_FLUSH_BUDGET` (**15 s**) rather than adding to
it, so the deaf-time **ceiling is unchanged**. The boot burst likewise folds a ≤ 3 s
MQTT downlink into the existing 30 s NTP window. It's still the same single-radio
trade-off (the mesh is deaf for the burst), still tens of seconds apart, still
loss-tolerant (a missed downlink just leaves the previous cached voltages on screen) —
but **don't pair a high-rate game with a busy MQTT gateway** and expect both smooth.
*(Hardware-verified on build 40: a wireless gateway CONNACKs and completes the
[MQTT burst](protocol.md#mqtt-burst--the-lan-transport-that-retires-the-udp-collector)
at both boot and flush, no panic; exact burst duration wasn't instrumented but it fits
inside the 15 s budget. Leaf-side `SMOLv1 BATT`-frame receipt remains inferred — #15.)*

## Freeze-on-failed-flush: the fix + backoff semantics
*(committed — `2ea7c4d` gateway liveness/dedup fix + `7b57216` live collector)*

**The bug (found in adversarial review — "Oracle findings"):** a gateway with queued
messages but a **dead/unreachable AP** would re-enter the *blocking* flush burst on a
tight cadence — each attempt stalled for the association timeout — so the whole device
(clock, game, LED) **froze** in repeated multi-second hangs.

**The fix (as committed):**
- **Bounded burst — `RELAY_FLUSH_BUDGET = 15 s`** (`net/wifi.rs`; tuned up from 6 s
  for real-AP DHCP, `652155b`), *separate from* the 30 s NTP `SYNC_BUDGET`: a flush's
  associate → DHCP → UDP is deadlined, so a dead AP **fails fast** instead of hanging
  the loop.
- **Unconditional backoff stamp** (`net/mode.rs`, "finding 1a"): `last_flush_ms` is
  stamped **before** the attempt and **regardless of success/failure**, so after a
  failed flush the next attempt is held off a full `RELAY_FLUSH_INTERVAL_MS`
  (**30 s** backoff) instead of spin-retrying every loop.
- **Full-queue fast-path gated on health**: the "flush immediately when the queue is
  full" shortcut fires **only while `flush_fails == 0`** — a failing gateway won't
  hammer a full queue against a dead AP.
- **Queue aging — `FLUSH_FAILS_BEFORE_DROP = 2`**: after 2 consecutive failed flushes
  the gateway sheds the **oldest** queued message (`drop_oldest`) on each further
  failure, so a gateway stuck against a dead AP **drains to empty** →
  `relay_ready_to_flush` goes false → the blocking bursts **stop**.
- **Dedup ring — `DONE_RING = 4`**: a lost RELAYACK makes a leaf retransmit an
  *already-complete* message; the gateway remembers the last 4 completed
  `(src_mac, msgid)` pairs and **re-ACKs without re-enqueuing**, so telemetry is never
  delivered to the collector twice.

Net: a gateway that can't reach its AP degrades to short (≤15 s), 30-s-spaced attempts
then goes quiet — no freeze, no duplicate delivery. All consts live in
`rust/clock/src/net/{wifi,mode}.rs`.

## Configure the collector target

> **Retired (v2, 2026-07-08).** This UDP-collector target has been **replaced** by the
> [MQTT burst](protocol.md#mqtt-burst--the-lan-transport-that-retires-the-udp-collector)
> to Home Assistant (hardware-verified on build 40); the disks collector is
> stopped/disabled and its JSONL archived. This section is kept for historical /
> rollback context (rollback = git).

Set where the gateway sends telemetry — compile-time consts in
`rust/clock/src/net/wifi.rs` (hardcoded IP, no DNS, mirroring `NTP_SERVER_IP`):

```rust
const RELAY_COLLECTOR_IP:   Ipv4Addr = Ipv4Addr::new(10, 0, 11, 117); // your collector host
const RELAY_COLLECTOR_PORT: u16      = 9999;
```

- **Point it at your collector host and reflash the gateway** — the relay does
  nothing until this matches a running collector.
- The deployed collector is on **disks `10.0.11.117:9999`**, which sits on the
  **same VLAN 11 /24 the boards DHCP onto**, so board → collector stays on one
  subnet (no inter-VLAN routing, no gatekeeper in the path — the ideal placement).
  *(This is now the in-tree default — set live in `7b57216`; change it only if you
  deploy the collector elsewhere.)*

## Run the collector (LAN side)

The receiver is a stdlib-only Python 3 service — see
[`collector/README.md`](../collector/README.md) for the code, tests, and deploy.
It's already running on disks as a user systemd service:

```
ssh disks 'systemctl --user status smol-collector'      # health
ssh disks 'tail -f ~/smol-collector/collector.jsonl'    # watch telemetry land
ssh disks 'curl -s localhost:9998/ | python3 -m json.tool'   # status page (localhost-only)
```
(Post-hardening the status page binds **`127.0.0.1` only** — view it via `ssh disks
curl localhost:9998`; it is not exposed on the LAN unless the collector is run with
`--status-host 0.0.0.0`. The relay's UDP `:9999` path is public and unaffected.)

## Out of scope (documented stubs)
- **Downlink** (server → leaf) — v2 introduces a **display-only** downlink: HA
  publishes a *retained* battery payload the gateway picks up in its
  [MQTT burst](protocol.md#mqtt-burst--the-lan-transport-that-retires-the-udp-collector)
  and re-broadcasts as a [`SMOLv1 BATT`](protocol.md#batt--ha-battery-snapshot) frame
  (HA→gateway→cache leg hardware-verified on build 40; the gateway→leaf `SMOLv1 BATT`
  frame *receipt* is inferred, not observed — finding #15). A *general* command/data downlink (per-leaf
  unicast fragmentation back to the leaf MAC) is still deferred.
- **Multi-hop** (leaf → relay → … → gateway) — needs a next-hop/TTL header + a
  loop-prevention seen-set + a shared-channel invariant (+200–400 LOC); this is
  single-hop uplink only.
- **Browsing / general IP** — physically impractical (see the gateway analysis).

## Status
🟢 **hardware-proven end-to-end + sustained.** On the wave-6 fleet (build 36
"Oxidized Spark", `bcafa7e`) the full chain runs: leaf id8 emits → gateway
reassembles → WiFi flush → **`node_id 8` telemetry accumulates in
`disks:~/smol-collector/collector.jsonl`, sustained ~02:06Z → 02:44Z** across a
firmware upgrade. Freeze/dup/liveness fixes (`2ea7c4d`, `652155b`, `ca5d985`) all
in. One non-blocking cold-ARP first-round nit remains (see the flush follow-up). The
LAN collector is committed (19→26 tests, hardened) + deployed. Wire frames:
[protocol.md](protocol.md#relay--relayack--espnow--internet-telemetry).

**v2 (hardware-verified — build 40 "Pressed Oven", 2026-07-08):** the LAN egress is now
an [MQTT burst](protocol.md#mqtt-burst--the-lan-transport-that-retires-the-udp-collector)
straight to Home Assistant (telemetry as native HA entities via discovery; a retained
battery downlink cached + re-broadcast as a [`SMOLv1 BATT`](protocol.md#batt--ha-battery-snapshot)
frame). The ESP-NOW relay/reassembly above is unchanged; only the gateway's internet
hop moved. Proven on real boards: wireless CONNACK (boot + flush), 31 B byte-exact
downlink, `sensor.smol_7…`/`sensor.smol_8…` live in HA (incl. a leaf's telemetry
relayed leaf→gateway→cloud). The **UDP collector is retired** (stopped/disabled, JSONL
archived). Still open: leaf-side BATT-frame *receipt* is inferred (finding #15), plus
one SNTP-starved-boot-MQTT edge (#15).
