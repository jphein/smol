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
*(fix wave landing — const names may shift one commit; I'll true this up next pass)*

**The bug (found in adversarial review):** a gateway with queued messages but a
**dead/unreachable AP** would re-enter the *blocking* flush burst on a tight
cadence — each attempt stalls for the association timeout — so the whole device
(clock, game, LED) **freezes** in repeated multi-second hangs.

**The fix (known design):**
- **Unconditional backoff stamp** — the gateway records the flush-attempt time
  **before** trying and **whether or not it succeeds**, so `relay_ready_to_flush`
  honors `RELAY_FLUSH_INTERVAL_MS` even after a failure instead of spin-retrying.
- **`RELAY_FLUSH_BUDGET` (~6 s)** — a flush burst is capped well under the 30 s NTP
  budget, so a single failed burst can't hang the loop for long.
- **Queue aging** — building on the committed `FLUSH_FAILS_BEFORE_DROP` (**2**):
  after 2 consecutive failed flushes the gateway sheds the **oldest** queued
  message on each further failure, so a gateway stuck against a dead AP **drains to
  empty** → `relay_ready_to_flush` goes false → the blocking bursts **stop**.

Net: a gateway that can't reach its AP degrades to occasional short, bounded
attempts and then goes quiet, instead of freezing. *(Committed consts today:
`RELAY_FLUSH_INTERVAL_MS`, `FLUSH_FAILS_BEFORE_DROP`, `RELAY_STALE_MS` in
`rust/clock/src/net/mode.rs`; the backoff stamp + `RELAY_FLUSH_BUDGET` land with
the fix wave.)*

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
  *(The in-tree default is still the `10.0.11.1` placeholder; the fix wave sets it
  to `10.0.11.117`.)*

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
🟡→ **firmware in progress**: the relay path (leaf emit, reassembly, RELAYACK,
gateway flush) is committed and compile-verified; the freeze-fix + backoff is
**landing now** (fix wave). **Not yet hardware-verified end-to-end** — that's the
final-flash test against the live collector on disks. The LAN collector side **is**
committed (19 tests) and deployed. Wire frames: see
[protocol.md](protocol.md#relay--relayack--espnow--internet-telemetry).
