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

A board picks its role **at boot**, from whether it associated to WiFi:

- **Gateway** — has valid WiFi creds *and* an AP in range (it associated during the
  boot NTP burst). It **receives** RELAY fragments from leaves, reassembles them,
  and periodically bridges them to the collector over a WiFi burst.
- **Leaf** — no AP (out of range, or no creds). It **emits** its telemetry as
  RELAY fragments over ESP-NOW and never flushes.

No configuration flag — the role follows the boot association. Put creds on the
board you want to be the gateway (see [BUILDING.md](BUILDING.md) → *Secrets*), keep
it in WiFi range, and leave the far boards credential-less (or out of range) as
leaves.

## The flush cycle

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
   then switches back to ESP-NOW ch 6.

### The single-radio cost (honest)
A flush burst tunes the one PHY to the AP's channel, so **the mesh is deaf during
the burst** — ESP-NOW HELLO/TIME/SNK aren't heard for its duration. This is the
same TIME-SHARE trade-off boot NTP uses. It's acceptable because flushes are tens
of seconds apart and telemetry is loss-tolerant (the leaf's retransmit rides over
a missed window), but **don't run a high-rate game and a busy relay gateway on the
same board** expecting both to stay smooth.

## Freeze-on-failed-flush: the fix + backoff semantics
*(committed — `2ea7c4d` gateway liveness/dedup fix + `7b57216` live collector)*

**The bug (found in adversarial review — "Oracle findings"):** a gateway with queued
messages but a **dead/unreachable AP** would re-enter the *blocking* flush burst on a
tight cadence — each attempt stalled for the association timeout — so the whole device
(clock, game, LED) **froze** in repeated multi-second hangs.

**The fix (as committed):**
- **Bounded burst — `RELAY_FLUSH_BUDGET = 6 s`** (`net/wifi.rs`), *separate from* the
  30 s NTP `SYNC_BUDGET`: a flush's associate → DHCP → UDP is deadlined at 6 s, so a
  dead AP **fails fast** instead of hanging the loop.
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

Net: a gateway that can't reach its AP degrades to short (≤6 s), 30-s-spaced attempts
then goes quiet — no freeze, no duplicate delivery. All consts live in
`rust/clock/src/net/{wifi,mode}.rs`.

## Configure the collector target

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
ssh disks 'curl -s localhost:9998/ | python3 -m json.tool'   # status page (VLAN-11 only)
```
(The status page on :9998 is reachable within VLAN 11; it's firewalled from VLAN 6
— a gatekeeper policy, flagged not changed. The relay's UDP :9999 path is
unaffected.)

## Out of scope (documented stubs)
- **Downlink** (collector → leaf) — needs a gateway-side poll/queue + unicast
  fragmentation back to the leaf MAC; deferred.
- **Multi-hop** (leaf → relay → … → gateway) — needs a next-hop/TTL header + a
  loop-prevention seen-set + a shared-channel invariant (+200–400 LOC); this is
  single-hop uplink only.
- **Browsing / general IP** — physically impractical (see the gateway analysis).

## Status
🟡 **compile-verified**: the relay path (leaf emit, reassembly, RELAYACK, gateway
flush) **and** the liveness/dedup fix are committed (`2ea7c4d`, `7b57216`) and build
clean across all 3 builds. **Not yet hardware-verified end-to-end** — that's the
final-flash test against the live collector on disks (`10.0.11.117:9999`). The LAN
collector side **is** committed (19 tests) and deployed. Wire frames: see
[protocol.md](protocol.md#relay--relayack--espnow--internet-telemetry).
