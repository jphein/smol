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
then the radio is pinned to **ch 6** for the mesh. The [relay bridge](#relay--relayack--esp-now--internet-telemetry)
resurrects a COEXIST/WiFi-return flush — and **the mesh is deaf during that burst**
(single radio). Everything in steady state rides ch 6.

Verified ESP-NOW limits: **250 B** max payload/frame, RX queue **10 frames deep
(drops oldest)**, **synchronous one-in-flight TX** (`send()` → `waiter.wait()`).
Every SMOLv1 frame stays well under 250 B — *including* its group-MAC trailer, which
`should_group_mac` refuses to append if it would push the frame over.

> **Re-verified 2026-08-24 against `esp-radio 0.18.0`**, the crate smol pins today:
> `ESP_NOW_MAX_DATA_LEN = 250` and `RECEIVE_QUEUE_SIZE = 10`, with `pop_front()` on
> overflow — hence "drops oldest". These numbers were originally read out of
> `esp-wifi 0.15.0` (see `nebula-espnow-gateway.md`); `esp-wifi` was **renamed to
> `esp-radio`** at 0.16.0, so that attribution named a crate the tree no longer
> depends on. The values are unchanged; only the source is restated.

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
- **Reserved node ids (#314).** **`42` is not a node — it is the C6 watch's
  unset-config sentinel.** Every watch boots with `watch_cfg.node_id == 42`;
  esp32c6-watch `#34` remaps that to a MAC-derived id (the live `id122` / `id236`),
  so `42` is a transient alias that **two different watches can publish under, at
  different times**, and it **recurs by design** — a reappearing `smol/42/*` means
  *a watch booted unprovisioned*, never *a dead node came back*. It must never be
  an OTA target (`tools/ota_publish.sh install 42` refuses), never be counted as a
  fleet member, and its ghost must never be read as a resurrection. Contrast `13`,
  a genuinely retired identity that is **not** expected to recur.
- **Allocated id blocks (#388 · 2026-08-24).** Beyond the C3 fleet's `1–99`:
  `160–175` = the S3 board class (ES3C28P family — ids are per-NODE, never per-product;
  the product lives in the BoardProfile model string): `160` = Ember satellite 1 (#331,
  ember.realm.watch) · `161` = the emberburrito hearth terminal · `162` = the s3-cyd dev
  board (targets/s3-cyd, the blank ES3C28P JP supplied 2026-08-24) · `163+` free ·
  `176–191` = C5 CYD (#388, the
  NM-CYD-C5 → `176`; it first spoke on the mesh under that id on 2026-08-24) ·
  `192–223` = reserved for MAC-derived C6-watch ids (today's `122`/`236` stay put
  until esp32c6-watch #34's derivation folds in) · `240–254` = bench/rig ids, never
  fleet members, never OTA targets · `0`/`255` = never · **`150` = never-allocate**: it is
  the MAC-FOLD of the s3-cyd dev board (`14:C1:9F:D1:C8:10`) — an unprovisioned unit of that
  board lands on 150 via the watch-lineage fold, so a node allocated there would collide
  with the ghost (same hazard class as the `42` sentinel and the `7` factory default; the
  watch repo's test asserts 150 stays unmapped — esp32c6-watch `ba46f74`). The
  factory-default baked id
  is **7** — always pass `SMOL_NODE_ID=<n>` when provisioning, or a fresh board
  lands on 7.
- **Security.** ESP-NOW here is **unencrypted**, and until #190 it was also
  unauthenticated — any device on the channel could inject any frame (a bogus
  far-future `synced_at` hijacks every mesh clock; a forged RELAYACK stalls a leaf).
  **#190 added a truncated group-MAC trailer** to every non-OTA `SMOLv1 ` frame; it is
  currently in **OBSERVE** mode, so those injection paths are *measured but still open*.
  See [the group-MAC trailer](#the-group-mac-trailer-190--every-byte-count-above-omits-it).
- **Every byte count in this document is PRE-TRAILER.** The `Bytes` column below and
  each frame's own layout table describe the frame as the encoder builds it. `send_to`
  then appends **9 more bytes** to nearly all of them. A reader sizing a buffer, and
  especially anyone implementing SMOLv1 outside this repo, needs both numbers —
  again, see [the group-MAC trailer](#the-group-mac-trailer-190--every-byte-count-above-omits-it).

---

## Frame summary

| Frame | Tag | Bytes | Cast | Cadence | Flag | Status |
|---|---|---|---|---|---|---|
| [HELLO](#hello--led-handshake) | `SMOLv1 HELLO ` | 16 | broadcast | ~2 s | espnow | 🟢 |
| [ACK](#ack--led-handshake) | `SMOLv1 ACK ` | 14 | unicast | reactive | espnow | 🟢 |
| [BEACON](#beacon--bench-link-stats) | `SMOLv1 BEACON ` | 29 | broadcast | ~2 s (Bench) | espnow | 🟡 |
| [TIME](#time--mesh-time-sync) | `SMOLv1 TIME ` | 37 | broadcast | ~2 s | espnow | 🟢 |
| [BATT](#batt--ha-battery-snapshot) | `SMOLv1 BATT ` | ≤108 | broadcast | on-recv + periodic | espnow | 🟡 |
| [GRID](#grid--ha-grid-power-snapshot-16) | `SMOLv1 GRID ` | ≤108 | broadcast | on-recv + periodic | espnow | 🟡 |
| [BATT2](#batt2--grid2--downlink-freshness-13-stage-b) | `SMOLv1 BATT2 ` | ≤120 | broadcast | on-change + strict-newer re-flood | espnow | 🟢 |
| [GRID2](#batt2--grid2--downlink-freshness-13-stage-b) | `SMOLv1 GRID2 ` | ≤120 | broadcast | on-change + strict-newer re-flood | espnow | 🟢 |
| [RELAY](#relay--relayack--esp-now--internet-telemetry) | `SMOLv1 RELAY ` | ≤91 | broadcast | ~15 s (leaf) | espnow | 🟢 |
| [RELAYACK](#relay--relayack--esp-now--internet-telemetry) | `SMOLv1 RELAYACK ` | 25 | unicast | reactive | espnow | 🟢 |
| [UP2](#up2--relayack2--routed-multi-hop-uplink-13124) | `SMOLv1 UP2 ` | ≤250 (23 B envelope + ≤227 inner) | broadcast (flood) | ~15 s (stranded leaf) | espnow | 🟡 |
| [RELAYACK2](#up2--relayack2--routed-multi-hop-uplink-13124) | `SMOLv1 RELAYACK2 ` | 32 | broadcast (flood) | reactive | espnow | 🟢 |
| ~~RELAY2~~ | ~~`SMOLv1 RELAY2 `~~ | — | — | — | — | ⛔ **RETIRED** — replaced by `UP2` (#124). No longer emitted or parsed; see [below](#up2--relayack2--routed-multi-hop-uplink-13124). |
| [SNK](#snk--mmo-mesh-snake) | `SMOLv1 SNK ` | 18 | broadcast | 5 Hz jittered | espnow | 🟢 |
| [OTAM](#leaf-mesh-ota-frames-40) | `SMOLv1 OTAM ` | ≤178 | gw→leaf broadcast | per session | espnow | 🟢 |
| [OTAD](#leaf-mesh-ota-frames-40) | `SMOLv1 OTAD ` | ≤250 | gw→leaf broadcast | windowed burst | espnow | 🟢 |
| [OTAN](#leaf-mesh-ota-frames-40) | `SMOLv1 OTAN ` | ≤27 | leaf→gw unicast | per window | espnow | 🟢 |
| [LDBG](#leaf-mesh-ota-frames-40) | `SMOLv1 LDBG ` | 21 | leaf→broadcast | per fetch | espnow | 🟢 |
| [CFG](#cfg--keyed-per-node-config-channel-56) | `SMOLv1 CFG ` | ≤79 | gw→leaf broadcast | per config change / ~10 s re-arm | espnow | 🟢 |
| [DIAG](#diag--retained-per-node-health-record-704974100) | `SMOLv1 DIAG ` | ≤250 | leaf→broadcast | ~10 s | espnow | 🟢 |
| [SCAN](#scan--on-demand-wifi-scan-uplink-71) | `SMOLv1 SCAN ` | ≤250 | leaf→broadcast | on-demand (#71) | espnow | 🟢 |
| [FAM](#fam--the-mesh-familiar-57) | `SMOLv1 FAM ` | 29 | holder broadcast | ~1.5 s (holder only) | espnow | 🟢 |
| [ELECT](#elect--the-mesh-rendezvous-channel-announcement-278269) | `SMOLv1 ELECT ` | **61** (fixed) | crown broadcast | burst ×6 @120 ms, then ~2 s | espnow | 🟡 |
| [STAT](#stat--leaf-render-state-uplink-50b) | `SMOLv1 STAT ` | ≤79 | leaf→broadcast | ~10 s (leaf only) | espnow | 🟢 |

> **The battery downlink is two hops.** The [`SMOLv1 BATT`](#batt--ha-battery-snapshot)
> frame above is the *mesh* hop (gateway → leaves). It's **fed** by an
> [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) on the
> LAN (gateway ↔ Home Assistant's Mosquitto broker) — plain TCP, not a mesh frame,
> so that transport is documented in its own section below, where the old UDP
> collector egress used to live. (v2 pivot: MQTT-native, retiring the collector.)

---

## The group-MAC trailer (#190) — every byte count above omits it

**Read this before implementing any frame.** Every `Bytes` figure in the table above, and
every per-frame layout table below, is the frame **as the encoder builds it**. On the air,
`send_to` appends a **9-byte authentication trailer** to nearly all of them.

**Admission rule** (`wire.rs::should_group_mac`) — a frame gets the trailer iff all three hold:

1. it starts with `b"SMOLv1 "`,
2. it is **not** OTA-family (those carry their own ed25519 gate), and
3. `frame.len() + 9 <= 250` (it still fits the ESP-NOW payload cap).

**Trailer layout (9 B), appended after the frame:**

| Field | Bytes | Meaning |
|---|---|---|
| `key_epoch` | 1 | selects the group key; **covered by the MAC**, so a flipped epoch fails |
| `tag` | 8 | `truncate(HMAC-SHA256(GROUP_KEY[epoch], frame_bytes ‖ key_epoch), 8)` |

Truncating to 64 bits makes forgery **online-only** (truncation does not weaken the HMAC key),
i.e. ~2⁻⁶⁴ per attempt. The epoch byte enables key rotation over a two-release overlap window:
add `(epoch+1, NEW_KEY)` to the accepted set, ship, then drop the old.

**⚠️ It is in OBSERVE mode today — and that is a dated trap, not a permanent state.**
`mode.rs::MAC_ENFORCE` is **`false`**: the firmware appends the MAC on TX, verifies on RX,
counts `mac_ok`/`mac_fail`, and **soft-accepts** frames that are un-MAC'd or fail the check, so
a mixed fleet never partitions. When the observe soak shows `mac_fail ≈ 0` on a quiet fleet,
`MAC_ENFORCE` flips to `true` and a failing frame is **dropped before the parser**.

> **What that means for a non-Rust implementation** (#331's ESPHome component, the C5 spike):
> a bare, un-MAC'd HELLO **works today and stops working at the flip**, with no warning and no
> code change on either side. The failure mode then is leaf HELLO-drop → owner churn, *not*
> crown deafness — `mac_fail` is the counter that tells those apart. **Implement the trailer
> up front**, or accept a known expiry date. Either is fine; not knowing is not.

**The group key is a secret.** `GROUP_KEY` / `GROUP_KEY_EPOCH` live in the gateway's
git-ignored `secrets.rs` (see [BUILDING.md](BUILDING.md) → *Secrets*) and are deliberately
absent from this doc. A non-Rust fleet member needs the same key provisioned by hand.

**Source.** `net/wire.rs`: `MAC_TAG_LEN` (8), `MAC_TRAILER_LEN` (9), `should_group_mac`,
`append_group_mac`, `verify_group_mac`; policy + counters in `net/mode.rs` (`MAC_ENFORCE`,
the verify-then-parse arm). **Status.** 🟡 shipped in OBSERVE; ENFORCE not yet flipped.

---

## SMOLv1 for non-Rust fleet members

Not every fleet member runs smol's firmware. The **C6 watch** joined from its own codebase;
**#331**'s S3 Ember is to join as an ESPHome external component in C++; the **C5 CYD** joins
from a minimal Rust image that is not this crate. This section is the **normative subset** for
all of them, so an implementer does not have to read the other ~1,300 lines and guess which
frames are load-bearing.

The design goal is that **fleet membership is cheap**. It is: two frames and 30 bytes of wire
format.

### Tier 1 — MUST. This *is* membership.

| frame | bytes | cast | cadence |
|---|---|---|---|
| [HELLO](#hello--led-handshake) | 16 = 13 B tag + 3 B ASCII id | broadcast | ~2 s |
| [ACK](#ack--led-handshake) | 14 = 11 B tag + 3 B ASCII id | **unicast** to the HELLO's source MAC | reactive |

Rules: on RX of a HELLO → mark the peer `Detected`, register it as a unicast peer if new, and
reply with a **unicast ACK echoing the sender's id**. On RX of an ACK whose id is *yours* →
`Connected`. ACKs addressed to other ids are peer-to-peer chatter; ignore them. Decay peers
`Connected → Detected → Idle` using `PEER_STALE_MS` = **3000 ms** on monotonic-ms timestamps.

> ⚠️ **Do not alter the HELLO/ACK wire format** — it is the hardware-verified LED path. Get
> these two byte-exact or nothing else will work.

### Tier 2 — SHOULD. Useful, but each carries a caveat.

- **[DIAG](#diag--retained-per-node-health-record-704974100)** — a partial record is fine and
  better than none. It is a pipe-delimited key set, not a fixed struct, and the Home Assistant
  entity table keys off the fields that are present.
- **[TIME](#time--mesh-time-sync)** — **implement the authority rule exactly or do not send
  it at all.** Adopt a peer's time **iff `peer.synced_at > my.synced_at`** (strictly greater),
  and on adopt **inherit the peer's `synced_at`, not `now`**. Equal → ignore. That is what
  makes `A→B→C→A` non-inflating.
  > A mains-powered, NTP-synced member will usually be the *authority* here, and the frame is
  > unauthenticated in OBSERVE mode — so a wrong `synced_at` does not degrade one node, it
  > **hijacks every clock in the mesh**. Listening without transmitting is always safe;
  > transmitting with the wrong rule is not. If in doubt, listen only.
- **[STAT](#stat--leaf-render-state-uplink-50b)** — only if the member has no MQTT of its own.
  A member that publishes its own telemetry does not need it.

### Tier 3 — DON'T, unless you know you need it

`RELAY`/`RELAYACK`, `UP2`/`RELAYACK2`, `BATT`/`GRID`/`BATT2`/`GRID2`, `CFG`, `SNK`, `FAM`,
`SCAN` and the leaf mesh-OTA family. The relay/uplink family exists to serve boards that
**cannot reach the broker**; a mains-powered member on WiFi can, and publishes its own MQTT.
Implementing RELAY there adds a redundant second path to the same destination plus a
reassembly state machine to maintain.

`ELECT` is **crown-only** — a member that will never be elected gateway must not announce a
channel decision.

⛔ **`RELAY2` is retired** (#124 replaced it with the `UP2` envelope; it is gone from the
firmware). It is still findable in older prose — do not implement it.

### Three things that are not frames, and will bite

1. **The [group-MAC trailer](#the-group-mac-trailer-190--every-byte-count-above-omits-it).**
   A bare, un-MAC'd HELLO works **today** and stops working when `MAC_ENFORCE` flips. Decide
   deliberately; the trailer needs the shared group key provisioned out of band.
2. **The MQTT + discovery contract**, which is what actually makes the member *appear* — see
   [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector). Publish
   `smol/<id>/telemetry` (**bare** line, no `NNN ` prefix — the topic carries the id) and the
   retained discovery config. **A discovery config that does not fit one MQTT publish is
   dropped silently and the entity is simply never created**; the firmware guards that with
   compile-time asserts that a C++ or YAML implementation does not inherit, so **write a test
   that measures your own longest config**.
3. **The node id.** It is a `u8` rendered as 3-digit zero-padded ASCII on every frame, so the
   space is 0–255. Get one assigned; do not pick one. **`42` is not a node** — it is the C6
   watch's unset-config sentinel and it recurs by design (see
   [Reserved node ids](#shared-conventions)).

### The single-radio precondition — check it before promising a date

A member with one 2.4 GHz radio that must **stay associated to WiFi** (a voice satellite, a
display that fetches assets) can only run ESP-NOW in the **COEXIST** arm — mesh channel ==
its AP's channel. **So its ESP-NOW works only if its AP is on the mesh channel**
(`ESP_NOW_FIXED_CHANNEL` = 6). If the AP is elsewhere, the member loses either its uplink or
the mesh; no firmware can resolve it. This is a **network-topology precondition, not a bug** —
see [the single-radio constraint](#the-single-radio-constraint-read-this-first).

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
id 8 (then *Eldritch Nexus*; renamed *Eldritch Jewel* on 2026-07-28) started at `synced_at = 0` and **adopted** id 7's exact
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
its direct ESP-NOW neighbours only (see Cadence for why). *(A freshness-gated multi-hop twin,
[BATT2](#batt2--grid2--downlink-freshness-13-stage-b), reaches a **stranded** leaf via a relay —
#13 Stage B.)* (This is the *HA* battery
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
> `SMOLv1 BATT BATT|48V 52.8V|HV 391.9V|d 43mV|48V 69%|HV 99%|Chg 4.1A`. **No length
> byte:** payload length = `frame_len − 12`. Frame payload and `BattCache` are
> therefore **byte-identical** — one `memcpy`, no reformatting on either the broadcast
> or the receive side.

**Payload format (pinned; extended to 6 segments — #16/#17).** The payload is
`BATT|<v1>|<v2>|<v3>[|<s1>|<s2>|<s3>]` — pipe-separated, **≤ 6 segments, ≤ 12 chars
each, ≤ 96 B total** (verified worst case ≈ 62 B), no trailing pipe. **Segments 1-3 =
the VOLTAGE overview page** (title `Batt`); **segments 4-6 = the SOC/charge DETAIL
segments** (#17), which the firmware renders as **big per-battery full-window pages**
(short-tap to page; boards flashed before #17 do `split('|').take(3)` and ignore 4-6 →
**fully backward-compatible**). An unavailable/stale source renders `--` with the label
kept (e.g. `BATT|48V --|HV 391.9V|d 43mV|48V 69%|HV 99%|Chg --`).
- **Voltage page:** `48V %.1fV` (System A 48 V LFP bank) · `HV %.1fV` (System B BMW-i3
  HV pack) · `d %.0fmV` (cell spread).
- **Detail segments** — **⚠️ big-render contract (co-designed with the firmware):** the
  small **label** is the text before the FIRST space, the **big value** everything
  after, so each label is a single token: `48V %.0f%%` (48 V bank SOC — **BMS,
  coulomb-counted**, not EPEver) · `HV %.0f%%` (HV pack SOC) · `Chg %.1fA` (total solar
  charge current into the 48 V bank).

Default fresh content: `BATT|48V 52.8V|HV 391.9V|d 43mV|48V 69%|HV 99%|Chg 4.1A`. Worst
case on the wire: `12 + 96 = 108 B`, well under the 250 B ESP-NOW limit. The exact HA
source entities + per-segment staleness windows live in [ha/README.md](../ha/README.md).

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

**Status.** 🟡 **partly hardware-verified — build 45 "Oxidized Die", 2026-07-08.** The
gateway *acquires* the (now 6-segment, #16/#17) HA payload and renders **both** the
voltage overview and the big SOC/charge detail pages on its own screen on real
hardware (via the 🟢 [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector)
— cached byte-exact). But the **ESP-NOW BATT frame's over-the-air delivery to a
leaf is *inferred*, not observed**: the mechanism is identical to the
hardware-verified [TIME](#time--mesh-time-sync) broadcast/adoption, yet frame *receipt*
isn't logged (finding #15) and the fleet currently runs **all-gateway**, so no leaf has
cached a BATT frame yet. Layout is byte-locked and the gateway TX path is gated + runs,
but **no leaf-side BATT render has been observed.**

**Source.** Firmware `rust/clock/src/net/mode.rs`: `BATT_PREFIX` (`:139`,
`b"SMOLv1 BATT "`), `broadcast_batt` (`:1259`, `memcpy(tag)` ++ `memcpy(cache.bytes())`),
RX `Frame::Batt(&[u8])` (`:227`, payload = `data[12..]`); render in `batt.rs`
(`BattCache`); broadcast gated in `main.rs`. Fed by the [MQTT
burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) in
`net/wifi.rs`; team spec `scratch/smol-ha-batt/spec.md` v2 (§ Architecture — Downlink).

## GRID — HA grid-power snapshot (#16)

**Purpose.** The **exact twin of [BATT](#batt--ha-battery-snapshot)** for grid/
consumption power. A gateway fetches a display-ready line set from HA's retained
`smol/display/grid` on its [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector),
renders it on its **Grid** screen, and re-broadcasts it single-hop as a GRID frame so
its neighbour leaves cache it too. All the shared mechanics — verbatim framing, no
length byte, gateway-only single-hop broadcast, receive-only leaves, fetch-age
staleness, security posture — are identical to BATT. *(Its freshness-gated multi-hop twin is
[GRID2](#batt2--grid2--downlink-freshness-13-stage-b), #13 Stage B — same `dl_seq` mechanism.)*

**Layout (≤ 108 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 12 | `b"SMOLv1 GRID "` | namespace (diverges from `b"SMOLv1 BATT "` at byte 8) |
| `payload` | ≤ 96 | ASCII, display-ready | the `GRID\|`-marked lines, **verbatim** from `smol/display/grid` |

**Payload format (pinned).** `GRID|<total>|<L1>|<L2>` — pipe-separated, **≤ 3 lines,
≤ 12 chars each, ≤ 96 B total**, no trailing pipe. All three lines are in **watts**
(a value that would exceed 5 digits of watts renders `X.XXkW` for that line). Default
content: `GRID|963W|L1 177W|L2 786W` — line 1 (no label) = **total consumption** (the
"yurt consumption" HA sensor, sourced in kW and scaled ×1000 to W); lines 2-3 = the two
PJ2101A phase clamps `L1`/`L2`. Unavailable/stale (> 30 min `last_reported`) → `--`.
**Single page** — unlike Batt there is no optional second trio, so a short-tap is a
plain no-op. Exact HA sources in [ha/README.md](../ha/README.md).

**Status.** 🟡 **build 45 "Oxidized Die", 2026-07-08.** The gateway acquires
`smol/display/grid` and renders it on real hardware (🟢 via the MQTT burst; live HA
mirror `sensor.smol_display_grid`). Leaf-side GRID frame receipt is **inferred** (same
all-gateway-fleet caveat as BATT — no leaf has cached one yet).

**Source.** `rust/clock/src/net/mode.rs`: `GRID_PREFIX` (`:155`, `b"SMOLv1 GRID "`),
`broadcast_grid` (`:1374`); RX `Frame::Grid`; render + `GridCache` in `grid.rs`; fed by
`GRID_TOPIC` (`net/wifi.rs:81`, `smol/display/grid`) in the same burst as BATT.

## CFG — keyed per-node config channel (#56)

**Purpose.** One frame carries *every* per-node runtime knob over the mesh, no reflash.
HA publishes a **retained MQTT** topic per node/key; the gateway relays each as a
`SMOLv1 CFG` ESP-NOW frame to the leaf. This supersedes the original MQTT-only
"default screen" spec (#21) — the keyed channel (#56) shipped it as a real relayed frame,
and #45/#48/#43/#55/#52/#71/#100 hung the rest of the family off the same wire.

**Frame layout (≤79 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 11 | `b"SMOLv1 CFG "` | namespace (`CFG_PREFIX`) |
| `target` | 3 | ASCII `NNN` | destination leaf id, or `255` = `CFG_TARGET_ALL` (fleet-global) |
| `key` | 1 | ASCII | which knob (table below) |
| `value` | ≤64 | bytes | key-specific payload, `CFG_VALUE_MAX = 64` |

**Key-less (legacy) frame.** A frame carrying only `target` with an **empty tail** (no `key` byte)
is read as an **empty-value clear on the `S` (screen) key** — the pre-#56 wire shape, kept for
back-compat (`mode.rs` `classify`). A conformant parser MUST treat "no key byte" as `key = 'S'`,
`value = ""` (screen → board default), **not** as a malformed frame.

**The key family (`CFG_APPLY_KEYS`, merged main @ `95747b1` — 12 keys).** A key a leaf's
firmware predates is DROPPED (forward-compat, #46). Parse is panic-free/heap-free/bounded
(a LAN-writable broker + a payload that panicked would re-deliver every boot → boot-loop brick).

| Key | Issue | What it sets | Cached + re-armed? | Apply |
|---|---|---|---|---|
| `S` | #21/#18 | boot **default screen** `<AppKind>:<page>` | yes | live, no reboot |
| `L` | #48 | **LED** mode (`status`/`on`/`off`) | yes | live |
| `U` | #43 | **display units** — °F/°C + 12/24h (fleet-global via `CFG_TARGET_ALL`) | yes | live |
| `P` | #55 | **plugin visibility** bitmask per node | yes | live |
| `Y` | #45 | **Custom screen** layout (the largest value → drives `CFG_VALUE_MAX = 64`) | yes | live |
| ~~`N`~~ | #100 S1b → **#142** | **RETIRED** — WiFi-slot switch removed (single-network). A received `N` is drained + ignored | (n/a) | none |
| `B` | #100 S2 | **broker** leg override (IPv4 + port) (`b08204d`, #110) | yes | **edge-triggered reboot** |
| `O` | #100 S3 | **OTA-host** override — one RFC1918 host appended to the fetch allowlist (`b08204d`, #110) | yes | live, **no reboot** |
| `R` | #52 | **remote reboot** | **NO** — transient, never cached/retained | reboots (that is its function) |
| `W` | #71 | on-demand **WiFi scan** trigger | **NO** — transient (a *cached* scan = a periodic off-channel excursion, the coexist hazard) | live one-shot, no reboot |
| `G` | #72 | **IO pin-map** — the whole per-node driver map (see below) (`5689b1b`, #113) | yes | live, **no reboot** (edge-triggered re-bind) |
| `g` | #72 | **IO output states** — commanded output levels (see below) (`5689b1b`, #113) | yes | live, no reboot |
| `T` | #303 | **Bard story prompt** — the opening the on-device LLM continues; empty = the node's built-in persona. **The LEAF validates** against the model's own 512-token vocabulary (bytes with no token are refused with an offset; merely-unknown ASCII is accepted but fragments) and keeps its previous prompt on refusal — validation cannot live gateway-side because it needs the tokenizer, which lives with the model | yes | live, no reboot (next story uses it) |
| `V` | #302 | **Bard delivery** — `<ms_per_char>:<mode>[:<font>]`, e.g. `160:inf` or `80:page`. ⚠️ **A third `font` field was added in `9210ef4`** and is not optional to *readers* — a parser written from the two-field shape will silently ignore the panel geometry. `inf` narrates for ever (continuous scroll); `page` writes one screenful of new text then waits for a button press. Empty = defaults (`160:inf`); an empty FIELD keeps that field (`:page`, `80:`); the colon is required otherwise. **The LEAF clamps** the pace to `20..=500` ms/char (a 0 would peg the reveal loop) and logs that it did; a malformed value is refused with the previous setting kept | yes | live, no reboot — takes effect **mid-tale**, on the next tick |

- **Reboot vs live.** Live-apply (no reboot): `S L U P Y O W G g T V`. Reboot on apply: `B` (broker-leg
  change) — **edge-triggered** (only when the value changes, so the ~10 s re-arm never
  reboot-loops); `R` reboots by design. `O` takes effect on the next OTA fetch without a reboot.
  (`N` was reboot-on-apply pre-#142; retired.)
- **Cached + re-armed vs transient.** `S L U P Y B O G g T V` live in the gateway's `cfg_cache` and are
  **re-broadcast every ~10 s** (`broadcast_cached_configs`), so a rebooted leaf re-arms within ~10 s
  with **no leaf-side NVS** — except the network overrides (`B`/`O`), which *also* persist in the
  NVS net record below (needed to reach the broker at all before the first relay). A retained `N` is
  still relayed but **inert** (drained + ignored, #142). `R`/`W` are one-shot and never cached (an
  anti-reboot-loop / anti-off-channel invariant).
- **`S` default-screen value.** `<AppKind>:<page>` — `AppKind` is the exact `app.rs` spelling
  (`Menu Clock Batt Grid Snake Bench MeshSnake About`; wire `Snake` → MeshSnake on espnow); `page`
  is one digit, out-of-range clamps to 0; empty value clears → compile-time `board.rs`
  `DEFAULT_APP`/`DEFAULT_PAGE` (#18/#19). **Foreign devices** — a non-fleet-app implementation that
  doesn't share this `AppKind` enum (e.g. the esp32c6-watch) — should take the **`page` digit after
  the last `:`** (clamped to their own range) and **ignore the `AppKind` token**: `page` is the
  portable part of the value, so an `S` config authored for the fleet still selects a sensible
  screen on a foreign node rather than being rejected.
- **Retained MQTT feed.** Each key is fed by a retained per-node topic (`smol/<id>/config/…`); the
  gateway self-applies its own id via the credentialed MQTT path and relays the rest single-hop
  (leaves never re-broadcast → no flood/loop).
- **Status.** 🟢 **shipped + hardware-proven.** `mode.rs` `CFG_PREFIX`/`CFG_APPLY_KEYS`;
  `wifi.rs` `CFG_KEY_*`/`CfgCache`.

### The IO/component registry — keys `G` + `g` (#72)

ESPHome inverted: every digital driver is compiled into one image; a **runtime pin-map** (relayed
over `G`, never rebuilt) selects which driver binds to which GPIO at boot — no recompile. Shipped
`5689b1b` (#113); digital-only in v1 (ADC/RMT/WS2812 deferred — `Flex` re-types digital in/out
cleanly, those don't). Source: `io.rs` (`PinMap`/`apply_wire`/`apply_set`), `wifi.rs`
`CFG_KEY_IO`/`CFG_KEY_IO_SET`.

- **`G` = the pin-map descriptor** — `;`-separated `<pin><kind>` tokens (e.g. `0L;7B;10R`), ≤64 B.
  Kinds: **`B`** Button (input, debounced), **`S`** BinarySensor (input, level), **`R`** Relay
  (output), **`L`** Led (output) → HA `event`/`binary_sensor`/`switch`/`light`. Retained per-node on
  **`smol/<id>/config/io`**; applied by (re)binding the free GPIOs via `io::apply_wire`,
  **edge-triggered** on a *change* of the map (a re-read of the same retained value is a no-op).
  Writes **NO NVS** (zero flash wear — the nvs partition is full); survives reboot purely via the
  ~10 s config re-relay (leaf re-arms within ~10 s of the gateway being up).
- **`g` = the output states** — `;`-separated `<pin>=<0|1>` (e.g. `0=1;10=0`), ≤64 B. Retained per-node
  on **`smol/<id>/io/set`**; applied via `io::apply_set` (no-op on an unbound/input slot). Retained
  (not a one-shot command) so a lamp/relay **holds its commanded level** across reboot/relay-loss,
  re-asserted after a re-relay or a `G` re-bind. Writes NO NVS.
- **Pin budget (`io.rs`).** Bindable **`FREE_PINS = [0, 1, 3, 7, 10]`**; a descriptor naming a
  **`RESERVED_PIN` `[2, 4, 5, 6, 8, 9, 20, 21]`** (OLED I²C 5/6, batt ADC 4, LED 8, BOOT 9, strapping
  2, USB-serial 20/21) is rejected and surfaced in DIAG, never bound.
- This is the **dollhouse per-room lamp + button foundation** (#75): a room LED on `7L`, a doll's
  button on `10B`, driven/observed entirely from HA.

### The NVS net record — 28-byte v3 (#100 → #142)

The network override state set by `CFG-B/O` persists in NVS **sector 5** (`0x5000`), so a leaf can
reach the broker before the first relay arrives. Brick-safe: any read failure/corruption → `None` →
the caller defaults to the single baked network, no overrides. #142 retired the slot machinery;
bytes 5–7 are now reserved-zero (same layout, so a pre-#142 v2 record still parses — see compat).

| Bytes | Field | Notes |
|---|---|---|
| 0–3 | magic `"SMn1"` | `NET_MAGIC` |
| 4 | version | v1 = 10-B core; v2 = 28 B (#100 slot+overrides); **v3 = 28 B** (#142, written today) |
| 5 | ~~`active`~~ reserved (0) | was WiFi slot; **#142: reserved-zero** (nonzero in a legacy v2 record) |
| 6 | ~~`commanded`~~ reserved (0) | was last CFG-`N` slot; **#142: reserved-zero** |
| 7 | ~~`fallback`~~ reserved (0) | was the un-brick fallback flag; **#142: reserved-zero** |
| 8 | `b5 ^ 0xFF` | core complement guard (over byte 5) |
| 9 | `(b5^b6^b7)+0x5A` | core checksum guard *(v1 core ends here — 10 B)* |
| 10 | broker present (1/0) | ext (Stage 2) |
| 11 | `broker_fallback` | override auto-disabled after repeated CONNACK fails → `brk=fb` |
| 12–15 | broker IPv4 octets | RFC1918, gated at CFG apply |
| 16–17 | broker port | u16 LE |
| 18 | ota_host present (1/0) | ext (Stage 3) |
| 19–22 | ota_host IPv4 octets | RFC1918 |
| 23 | ext checksum | `sum(rec[10..23]) + 0x5A` |
| 24 | ext checksum `^ 0xFF` | ext complement guard |
| 25–27 | zero pad | word-alignment only — esp-storage `WRITE_SIZE = 4`, so 25→**28** (`1b52456`); outside the checksum |

- **Migration (v1/v2/v3).** #142 fw reading a **v2** record MIGRATES it: keep the broker/OTA-host
  override fields, discard the (now-meaningless) slot bytes — so a deployed board that was never
  flash-erased keeps its overrides across the OTA. A **v1** record → no overrides. **v3** → native.
  Unknown version / bad checksum / erased → `None` → safe defaults. No forced boot-time rewrite
  (flash wear): the first genuine CFG-`B`/`O` change upgrades the record to v3 in place
  (verify-after-write gated). A pre-#142 (v2-reader) fw reading a v3 record rejects it → safe default
  (same one-way rollback posture as v1↔v2).
- **Why 28, not 25.** A non-word-aligned write returns `NotAligned`; the swallowed error had made the
  record silently never persist (HW-canary find 2026-07-12 — the edge-trigger fired, then
  verify-after-write aborted every apply). Source: `ota.rs` `NetCfg`/`encode_net_cfg`/`parse_net_cfg`.

## STAT — leaf render-state uplink (#50b)

**Purpose.** A **leaf** has no MQTT, so it cannot tell Home Assistant what is on its
screen. It broadcasts its own live render-state as a `SMOLv1 STAT` frame; the
**gateway** caches it (`stat_cache`) and republishes it retained to
`smol/<id>/status`. Uplink twin of [`CFG`](#cfg--keyed-per-node-config-channel-56),
which is the downlink.

**Layout (≤79 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 12 | `b"SMOLv1 STAT "` | namespace |
| `id` | 3 | ASCII `NNN` | the **SENDER's** id — *not* a target (this is the mirror-image of CFG) |
| `value` | ≤64 | bytes, verbatim | the live render-state, `<AppKind>:<page>` (empty = none) |

`value` is truncated to `CFG_VALUE_MAX` (**64 B**), so the frame is at most
`12 + 3 + 64 = 79 B` — plus the [9 B group-MAC trailer](#the-group-mac-trailer-190--every-byte-count-above-omits-it) on the air.

**Cadence.** ~10 s, broadcast. `main` gates the call on `!is_gateway` — a gateway
publishes its own status over MQTT directly and never emits STAT.
**Rule.** Fire-and-forget. **Single-hop**: the gateway caches a leaf STAT and never
re-broadcasts it, so there is no flood and no loop. It is also passed to
`relay_wrap_observability`, so a stranded leaf's STAT can ride the multi-hop uplink.

**Tag disambiguation, and why it is in the source as a comment.** STAT diverges from
[`SNK`](#snk--mmo-mesh-snake) at **byte 8** (`'T'` vs `'N'`) and from
[`DIAG`](#diag--retained-per-node-health-record-704974100) at **byte 7** (`'S'` vs
`'D'`), so `strip_prefix` can never confuse the three. Anyone adding a `SMOLv1 S…`
tag must re-check that property.

**Flag.** espnow. **Status.** 🟢 **hardware-verified** — the retained
`smol/<id>/status` topic is populated by leaves on the bench fleet.
**Security.** Unauthenticated in OBSERVE mode (see the
[group-MAC trailer](#the-group-mac-trailer-190--every-byte-count-above-omits-it)).
Low blast radius by construction: the worst case is a wrong screen **string** on a
status topic — never code execution — and it self-corrects on the next cadence.
**Source.** `net/mode.rs` `STAT_PREFIX` (`:258`), `RadioManager::broadcast_stat`;
gateway side `stat_cache` → `net/wifi.rs` retained republish.

## DIAG — retained per-node health record (#70/#49/#74/#100)

**Purpose.** One retained record per node makes a silent rollback / a failed network switch /
config drift visible in HA at a glance. A leaf **broadcasts** `SMOLv1 DIAG NNN<record>`; the gateway
caches it (`diag_cache`) and republishes it retained to `smol/<id>/diag`.

**Record (merged main @ `0b83714`, built by `RadioManager::diag_record` — #13 added the
`fwd`/`dedup`/`ttl`/`hop`/`dlseq`/`dfwd` tail + the `mesh-test` `deaf`/`ddrops`):**

```
DIAG|slot=<bootslot>|rst=<reset-reason>|boot=<bootcount>|ota=<outcome[:build]>|up=<sec>|heap=<free>
    |hmin=<heap-min>|btn=<short>|btnl=<long>|fok=<flush-ok>|ffl=<flush-fail>|vok=<verify-ok>
    |vfl=<verify-fail>|loss=<pct>|rtt=<ms>|rx=<n>|tx=<n>|led=<mode>:<on|off>|tage=<sec-since-sync>
    |tsrc=<ntp|mesh|none>|net=0:ok|brk=<baked|ovr|fb>|otah=<slot|ovr>
    |fwd=<uplink-fwds>|dedup=<dup-drops>|ttl=<ttl-drops>|hop=<1|2>|dlseq=<last-adopted>|dfwd=<downlink-refloods>
    [|cfg=<applied-config>][|io=<pin>:<count>,…][|deaf=<n>|ddrops=<n>]
    [|ap=<ch>:<rssi>:<bssid>]|cc=<0|1>|degraded=<0|1>|cdeaf=<streak>:<reassocs>:<shed>
    |etx=<worst-peer-cost>|apch=<ch>[|blrev=<token>][|brst=<gap>:<ms>:<kind>][|sog=<octal>]
    [|shed=<n>][|cut=<bytes>]
```

(one line on the wire; wrapped here for reading.)

### The appended-last block (#204/#217/#153) — documented 2026-07-28

Everything below is appended **after** the positional block, deliberately, so a positional parse of the
fixed fields stays intact as fields are added. **Field list dated because it grows** — re-date it when it
changes, and see the truncation warning.

| Field | Always? | Meaning |
|---|---|---|
| `ap=<ch>:<rssi>:<bssid>` | **conditional** — skipped off-association | the HAL's **observed** association. Absence keeps the string byte-identical for a non-associated board |
| `apch=<ch>` | **always** | #217 — the AP channel coexist **believes** it is on. `0` = unknown / not yet scanned |
| `cc=<0\|1>` · `degraded=<0\|1>` | **always** (0 on a leaf) | #217 rung-3 coexist health. `cc=1` = crown is **co-channel** (live AP channel == mesh channel) — the direct OTA-capability / off-ch6-starvation signal. `degraded=1` = strand-guard latched off-channel (MQTT alive, OTA off, never crownless) |
| `cdeaf=<streak>:<reassocs>:<shed>` | **always** (`0:0:0` healthy, and on any leaf) | #204 crown dead-downstream telemetry. The shed flag latches `1` after a 2b deaf-shed until re-lock |
| `blrev=<token>` | conditional | the bootloader **auto-revert probe** — present when `ota::bl_revert_token()` has an answer. This is what settles the long-standing *"is revert-on-boot-fail enabled?"* question (see [ROADMAP §3a](ROADMAP.md)) |
| `etx=<cost>` | **always** (positional) | #164 — worst per-peer link cost. It was **absent from this block entirely** until 2026-07-28 despite being on the wire |
| `brst=<gap>:<ms>:<kind>` | conditional | 🔴 **THREE parts, not one.** #153's worst app-service gap **in ms**, the **burst duration** that produced it, and a **one-byte kind tag**: `f` flush · `n` ntp · `r` re-election · a SelfOta kind (`6f952d7`). `0` = nothing measured yet. A **`+`** suffix means a `u16` **saturated** at 65.5 s — `brst=65535:65535:o+` honestly reads *"at least 65.5 s"*, not *"exactly"*. It is a **max-since-report**, not a sample (DIAG cadence is 60 s, a burst is seconds, so an instantaneous read would miss the event JP feels), and it resets at publish |
| `sog=<octal>` | conditional, **sheddable** | `a35c5b0` — why a crown did **not** self-install, as an octal bitfield: **bit 0** `leaf_ota_pending` · **bit 1** `leaf_installs_outstanding` · **bit 2** armed-in-RAM. `sog=4` = armed and waiting on nothing (it will fire); `sog=6` = armed but **held** |
| `shed=<n>` | conditional | fields dropped from the **sheddable** tail to stay inside budget (`7a4823e`) — with a `log::warn!`. Absent = nothing shed |
| `cut=<bytes>` | conditional | bytes lost to truncation — see below |

> ⚠️ **TWO different caps, and they are now handled in opposite ways** *(updated 2026-07-28,
> `6133675` + `7a4823e`)*. The most important thing to know about this record:
>
> | Path | Budget | State |
> |---|---|---|
> | **Gateway → MQTT** (self-publish) | **495 B** | ✅ **cliff made unreachable** (`7a4823e`). It was **reachable**: an over-budget record is *not* truncated — `encode_publish` returns `None` and **the record does not go out at all**, so a board that grew by 33 B became **indistinguishable from a dead board** |
> | **Leaf → ESP-NOW** (`broadcast_diag`, ONE frame) | **`RELAY_VALUE_MAX` = 232 B** | 🔴 **still breached** — a realistic leaf record **offers 307 B, sends 232, drops 75 B.** Loss is now *announced* (`cut=`) but not fixed |
>
> **The true gateway worst case was 574 B against 495 — over by 79.** That figure is bounded **by type**,
> not observed: every term is the maximum decimal width a field's type implies (u8→3, u16→5, u32→10) or
> the longest member of a fixed string set, so **no board state can be milder than it.** The earlier
> **462 B** was a *realistic* case — and a realistic number is exactly what lets a cliff stay reachable.
>
> **How it was made unreachable — three moves, and the third is the durable one:**
> 1. **`up=` narrowed u64 → u32** — 20 digits of bound to express a value that cannot exceed 10; u32
>    seconds is 136 years of uptime. **10 B of provable headroom for zero information.**
> 2. **The tail is split PROTECTED vs SHEDDABLE.** `brst=` · `blrev=` · `apch=` are appended
>    **unconditionally and first** — the freeze metric, the bootloader answer, and the off-channel signal
>    behind a proven OTA blocker. The rest is appended only while budget allows, in a documented priority
>    order — **first to go is least missed**: `cc`/`degraded` (`cc` is derivable HA-side as `apch == 6`, so
>    it is nearly redundant with a *protected* field) → `io` (bound-input counters, absent on most boards)
>    → `cdeaf` (`0:0:0` on a healthy board or any leaf) → `cfg` (reconstructible from the command topics
>    HA already holds) → `ap` (its channel is approximated by the protected `apch=`). *An earlier version
>    of this doc listed that order wrongly; it is `mode.rs`'s, not a guess.* The record stamps **`|shed=<n>`** plus a
>    `log::warn!` when it drops any. **Reordering is free because appended fields are parsed by KEY** —
>    the positional contract on the fixed block is untouched.
> 3. **A compile-time assertion that the unsheddable core fits:** `DIAG_CORE_MAX` (444 B) ≤ `DIAG_BUDGET`
>    (495 B), leaving **51 B of proven slack** for the sheddable tail. **A new positional field costs its
>    type-max against that slack and breaks the BUILD when it runs out** — so the record can never become
>    unpublishable. *This is the pattern worth copying: make the failure unrepresentable rather than
>    documented.*
>
> **What a leaf still loses:** the 232 B cut lands mid-`dlseq=`, taking the tail of the positional block
> (`dfwd`, `etx`) and the appended fields that don't fit. **`brst=` and `blrev=` are protected in the
> gateway budget but a leaf frame is less than half of it**, so on leaves they may still be absent —
> and `blrev=` is invisible on exactly the boards that get OTA'd. **Why this hid for months: the crown
> self-publishes the full record, so the one board anybody looks at is the one that works.**
>
> `cut=<bytes>` (+ `log::warn!`) makes any loss impossible to miss — 8 B of the very budget that is short,
> which is the right trade: **a reader who cannot see the field it wants must at least be able to tell it
> was cut.**
>
> **So a parser must treat an absent `brst=`/`blrev=`/`shed=` as `unknown`, never `0`.** Per
> [DOC-UPKEEP](DOC-UPKEEP.md) §2: a check whose operand may be absent prints *unknown* and **skips**. See
> [#306](https://github.com/jphein/smol/issues/306).
>
> 🔎 **Two naming decisions in this record are load-bearing, not style.** `apch=` is **not** `ap=` because
> *"an unanchored grep for `ap=` matches the tail of `heap=42040`"* (`mode.rs:3316`) — a real harness bug
> that read free heap as a channel. And `cdeaf=` is **not** `deaf=` because the latter is the
> mesh-test-only ESP-NOW deaf-**list** count. **When you add a DIAG field, check its name is not a
> substring-tail of an existing one.**
>
> Note also that `apch=` is **always present with `0` = unknown** rather than conditional — deliberately,
> *"so a harness can read it unconditionally and `0` is an unambiguous 'no answer yet' rather than a
> missing field."* That is the cleanest available answer to the absent-operand problem: **make absence
> impossible and encode ignorance in the value.**

| Field | Meaning | Since |
|---|---|---|
| `slot`/`rst`/`boot`/`ota` | boot slot · reset reason · boot count · last-OTA outcome | #70 |
| `ota` values | `none` · `confirmed:<build>` · `rolled-back`. **#323: `confirmed` carries the build it vouches for.** The scope was always real (firmware refuses the token unless the recorded build is the running one) but was invisible on the wire, so `confirmed`-for-912 and `confirmed`-for-913 were the same string — which made `ota_verify.sh`'s `none\|rolled-back → confirmed` transition test fail on a *successful* OTA, the normal state of every board from its second consecutive OTA (the token is `rtc_fast` and only a power cycle clears it). A **bare `confirmed` stays valid input** — it is what a pre-#323 image emits — so a parser must accept both and split on `:`. `rolled-back` deliberately gained **no** suffix: it describes the image rolled away FROM, and its reader is the good slot, a different build by construction. | #323 |
| `up`/`heap`/`hmin` | uptime s · free heap · heap low-water | #70/#49 |
| `btn`/`btnl` | BOOT short-/long-press counters (HA fires events on change) | #49 |
| `fok`/`ffl` | gateway MQTT bursts to CONNACK vs failed (leaf-side 0) | #49 |
| `vok`/`vfl` | OTA image verify ok/fail | #49 |
| `loss`/`rtt`/`rx`/`tx` | mesh link-quality set (packet-loss % · RTT ms · rx/tx counts) | #49/#74 |
| `led`/`tage`/`tsrc` | commanded LED mode:lit-state · seconds since sync · time source | #74 |
| `net=0:ok` | **FROZEN** constant (#142 single-network — no slot/fallback; `0` = the historical primary baseline). Kept only so the dashboard's positional DIAG parse stays stable; a dashboard follow-up repurposes/drops it | #100 S1b → #142 |
| `brk=<baked\|ovr\|fb>` | broker: baked-in · override active · override auto-disabled | #100 S2 (#110) |
| `otah=<slot\|ovr>` | OTA host: slot allowlist · runtime override appended | #100 S3 (#110) |
| `fwd` | **uplink** `UP2` envelope re-broadcasts forwarded by this node (`RELAY2` before #124) — the C0 byte-identical invariant: **must be 0 fleet-wide in all-hear** (increments at exactly one site, the `UP2` forward arm) | #13 |
| `dedup` | inbound `UP2` envelopes dropped as already-seen (the `(origin,env_msgid,0)` seen-set — bounds the flood) | #13 |
| `ttl` | `UP2` frames dropped with hop budget exhausted (`hop ≤ 1` at a non-gateway) | #13 |
| `hop` | this leaf's **current origin hop**: `1` = single-hop (normal / byte-identical), `2` (= `MAX_HOP`) = escalated & emitting `UP2`. A gateway always reads `1` | #13 |
| `dlseq` | last-adopted BATT2/GRID2 downlink `dl_seq` (rig P4: advances on a value change, unchanged on a replayed/older seq). `0` on a gateway (source) or a v1-only leaf | #13 |
| `dfwd` | **downlink** BATT2/GRID2 strictly-newer re-floods — **nonzero by design** (like TIME), kept separate from `fwd` so the uplink invariant stays machine-checkable | #13 |
| `cfg=<applied-config>` | **optional** (espnow): the applied-config echo (≤40 B, `DIAG_CFG_MAX`) HA plain-string-compares against its command topics for **config-drift** detection | #74 S2 (`b1c2c5c`, #109) |
| `io=<pin>:<count>,…` | **optional** (`io` feature, appended after `cfg=`): per-bound-**input** press/edge counters, so a doll's button push is visible in HA. Byte-identical omission on a non-io build | #72 (`5689b1b`, #113) |
| `deaf=<n>\|ddrops=<n>` | **optional** (`mesh-test` feature only — the #13 rig): active deaf-list entries · frames dropped by it, so a leftover test entry is visible. **Absent entirely on a production build** | #13 |

**Counter & uptime semantics (authoritative — #13).**
- **`fwd` vs `dfwd` are split on purpose.** `fwd` = **uplink** `UP2` forwards only (the C0
  invariant, 0 fleet-wide in all-hear); `dfwd` = **downlink** BATT2/GRID2 re-floods (nonzero by
  design). The split keeps the uplink invariant machine-checkable, so any nonzero `fwd` in all-hear
  is always a real uplink signal (e.g. storm-tail residue from a pre-quiet convergence window),
  never a downlink artifact.
- **The `DiagCounters` (`fwd`/`dfwd`/`dedup`/`ttl`) are plain RAM — reset to 0 on every boot**
  (not `noinit`). Absolute values are same-boot accumulation, so **score invariants off the delta**
  over the observation window, not the absolute count (which can hold same-boot storm residue).
- **`boot` (`boot_count`) is the sole authoritative reboot marker** — NVS-persisted, +1 per boot.
  **`up` is systimer-based and *survives* `software_reset`** (it continues across an OTA / soft
  reboot) — it is **not** a reboot indicator. A genuine reboot shows `boot`+1 **and** `fwd`→0
  together.

**Companion (#74 S2, `51052fc`):** the gateway's OLED is also mirrored to HA as an MQTT image via
the Cast tee (`smol/<id>/screen`) — the **display-mirror** — so a node's actual screen is visible in
the dashboard beside its DIAG. **Status.** 🟢 shipped.

## SCAN — on-demand WiFi scan uplink (#71)

**Purpose.** A node scans nearby APs **only on request** (CFG-`W`) and uplinks the result — never a
periodic background scan (that would take the radio off-channel and go mesh-deaf). Leaf broadcasts
`SMOLv1 SCAN NNN<value>`; the gateway republishes to `smol/<id>/scan`.

**Value:** literal `SCAN` then up to `SCAN_MAX_APS = 5` `|`-separated groups
`<ssid>,<bssid-3oct-hex>,<ch>,<rssi>` (SSID truncated to `SCAN_SSID_MAX = 12`, `|`/`,` stripped).
**Status.** 🟢 shipped. Source: `mode.rs` `SCAN_PREFIX`.

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
*v2 (hardware-verified — build 40, 2026-07-08):* this UDP egress is now **replaced** by
the [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector) straight
to Home Assistant (the collector is retired) — the ESP-NOW RELAY frame here is
**unchanged**, only the gateway's internet hop moved.
**Multi-hop (shipped, #13).** The "next-hop/TTL header" stub that used to live here is
now real: a stranded leaf escalates plain `RELAY` → [`UP2`](#up2--relayack2--routed-multi-hop-uplink-13124)
(a hop-limited managed flood of a wrapped inner frame; `RELAY2` before #124) so its telemetry reaches the gateway **through a relay**, and
[`BATT2`/`GRID2`](#batt2--grid2--downlink-freshness-13-stage-b) carry the downlink the other way
with a freshness gate. The plain `RELAY`/`RELAYACK` frames above are **unchanged** — a
non-escalated leaf never emits `UP2`, so the all-hear case is byte-identical to pre-#13.
**Flag.** espnow. **Status.** 🟢 **hardware-proven end-to-end** — on the wave-6 fleet
(build 36 "Oxidized Spark", `bcafa7e`) leaf id8's telemetry reaches
`disks:~/smol-collector/collector.jsonl` as `node_id 8`, sustained ~02:06Z→02:44Z. The
flush's budget/backoff/dedup and the drain-until-egress path were hardened over five
Oracle passes (`2ea7c4d`, `652155b`, `ca5d985`; see `morpheus-oracle-fixes.md`). One
non-blocking cold-ARP first-round nit remains — see [relay.md](relay.md#the-flush-cycle).
**Security.** Unauthenticated → a forged RELAYACK can stall a leaf's retransmit.
**Source.** `mode.rs` relay-bridge section (`RELAY_PREFIX`, `RELAYACK_PREFIX`,
`Relay`/`RelayTx`/reassembly); spec `relay-bridge-spec.md`.

## UP2 / RELAYACK2 — routed multi-hop uplink (#13/#124)

> ⛔ **`RELAY2` is retired — do not implement it.** #13 shipped this path as `SMOLv1 RELAY2 `; **#124
> replaced that tag with the `UP2` envelope** and `RELAY2` is **gone from the firmware** — no
> `RELAY2_PREFIX`, no `encode_relay2`, no `parse_relay2` (`grep -rn 'SMOLv1 RELAY2' rust/` returns
> nothing; `wire.rs:177` states it outright). **This section documented the retired frame, with a
> 🟢 badge, until 2026-07-28.** Recorded rather than quietly deleted: a second implementation of this
> protocol exists (the C6 watch, #231), and anyone who read the old text needs to know *which* claim
> was wrong.

**Purpose.** Carry a **stranded** leaf's traffic home when it is out of direct ESP-NOW range of the
elected gateway — the single-hop [RELAY](#relay--relayack--esp-now--internet-telemetry) above can't.
#13 adds **Meshtastic-lineage managed flood**: a hop-limit (`H`) + an `(origin, msgid)` seen-set + a
forward path, rooted at the #76-elected owner and **table-free** (so it rides roam/re-election for
free). A leaf only ever emits `UP2` **after it escalates** (see the latch below); in the ordinary
all-hear case no `UP2` frame is ever on the wire.

**What #124 changed, and why it is not cosmetic.** `RELAY2` was a telemetry frame with a chunk field
— it could carry *only* telemetry. `UP2` is a **generic envelope around any verbatim inner SMOLv1
frame** (`RELAY`, `STAT`, `DIAG`, `SCAN`), so a stranded leaf's **observability** reaches home too:
before #124 the one node you most needed to diagnose was the one that could not tell you anything.

**UP2 layout (23 B envelope + ≤227 B inner = ≤250 B = `ESP_NOW_MTU`).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 11 | `b"SMOLv1 UP2 "` | namespace (`UP2_PREFIX`) |
| `origin` | 3 | ASCII `OOO` | **originating** leaf id — stamped by the true source, **survives every hop** (unlike a src MAC, which changes per hop) |
| ` ` | 1 | space | |
| `env_msgid` | 5 | ASCII `MMMMM` (u16) | the **envelope's own** per-origin rolling id — the **flood-dedup key** |
| ` ` | 1 | space | |
| `hop` | 1 | ASCII `H` | hop-limit remaining — originated at `MAX_HOP = 2`, each relay decrements by 1 |
| ` ` | 1 | space | |
| `inner` | ≤227 | bytes | a **verbatim** inner SMOLv1 frame (`UP2_INNER_MAX = ESP_NOW_MTU − UP2_OVERHEAD = 250 − 23`) |

⚠️ **`env_msgid` is NOT the inner frame's `msgid`, and conflating them is the implementer bug this
layout invites.** The **envelope** msgid keys the **flood seen-set**; the **inner `RELAY`'s** msgid
stays the **reassembly + ACK** key (`RELAYACK2` acks the *inner* msgid). A fragmented telemetry
message therefore emits **one UP2 per inner fragment, each with a fresh `env_msgid`** while all of
them share the inner `msgid` (`mode.rs:3838-3844`). A relay **re-wraps the same inner verbatim at
`hop − 1` and preserves `env_msgid`** (`mode.rs:5812-5815`) — which is exactly what makes the
dedup key work across hops.

⚠️ **Clamping ends on a field boundary, not at byte 227.** `clamp_inner_field_boundary` cuts at the
last `|` at or before the limit, so a wrapped `DIAG`/`SCAN` never truncates mid-`key=val`; the tail
that falls off (`cfg=`/`io=`/`dlseq=`/`dfwd=`, appended last) is what a prefix-tolerant parser would
ignore anyway. `encode_up2` also applies a raw clamp so **the frame can never exceed the MTU**.

**RELAYACK2 layout (32 B), broadcast — flooded leaf-ward (NOT unicast).** The gateway can't
unicast an ACK back to a leaf it can't hear directly, so `RELAYACK2` is **flooded** back at
`MAX_HOP` and rides the same relay path in reverse.

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 17 | `b"SMOLv1 RELAYACK2 "` | namespace (diverges from `RELAYACK` at byte 15) |
| `target` | 3 | ASCII `TTT` | the **origin** id being acked |
| ` ` | 1 | space | |
| `msgid` | 5 | ASCII `MMMMM` | message being acked |
| ` ` | 1 | space | |
| `bitmap` | 3 | ASCII `BBB` (u8) | bit *i* set = fragment *i* received |
| ` ` | 1 | space | |
| `hop` | 1 | ASCII `H` | hop-limit remaining (flooded back at `MAX_HOP`) |

**The managed flood (loop-free by construction).** A node deciding what to do with an inbound
`UP2` uses `flood::forward_decision(is_gateway, hop, already_seen)`. Since #124 the forwarding guard
keys on **`(origin, env_msgid, 0)`** — each `UP2` is one atomic frame, so the `frag` slot is always
`0` and fragmentation now lives *inside* the envelope (`mode.rs:1685`):
- **already in the seen-set** → drop, bump `dedup`. The `(origin, msgid, frag)` seen-set
  (16-slot `.bss` ring, drop-oldest) is what makes the flood **terminate**. It is keyed
  **per-fragment** on purpose — a fragmented message shares one `msgid`, so a per-`(origin,msgid)`
  key would mark the whole message "seen" on fragment 0 and a relay would drop fragments 1..N →
  a multi-fragment message could never reassemble. *(This was the one correctness fix to the
  inherited host-tested core; `flood_verify` gained a multi-fragment regression assert.)*
- **`is_gateway`** → **reassemble** the fragment, re-keyed by a **synthetic origin MAC**
  `00:00:00:00:00:<origin>` (a real Espressif STA MAC is never all-zero, so it can't alias a
  single-hop leaf's real MAC), and on completion **flood a `RELAYACK2` back**. A gateway never
  re-forwards (it's the sink).
- **relay, `hop > 1`** → re-broadcast the **same inner verbatim** as `UP2` at `hop − 1` (`env_msgid`
  preserved), bump `fwd`, record in the seen-set.
- **relay, `hop ≤ 1`** → hop budget exhausted, drop, bump `ttl`.

**Escalation latch + hysteresis (`flood::HopLatch`).** A leaf starts single-hop (`H = 1`, plain
`RELAY`) and only latches into multi-hop under **genuine** strandedness — the hysteresis is what
keeps the byte-identical `fwd = 0` invariant intact under ordinary packet loss:
- **Escalate (down→up):** latch to `UP2` at `MAX_HOP` only after `ESCALATE_STREAK = 3`
  **consecutive fully-un-ACKed** messages (~45 s at the 15 s cadence). **Any** ACK — a fully- or
  even partially-ACKed message — **resets the streak to 0**, so a single transient full-loss in a
  healthy all-hear mesh never escalates. The streak is fed both by retry-exhaustion
  (`RELAY_MAX_TRIES`) **and** by message **supersession** — a totally-deaf leaf pinned in blocking
  recovery bursts yields zero ACKs and would otherwise starve the retry-exhaustion path, so the
  next `msgid` superseding an un-ACKed one also feeds the streak (the #13 P1 fix, `e0554a7`).
- **Un-latch (up→down):** while latched, emit a 1-in-`PROBE_EVERY = 8` **direct** `H = 1` probe —
  but **only** while the owner's HELLO is heard **directly + fresh** (else the leaf is definitely
  still stranded and a probe would just waste airtime). After `UNLATCH_STREAK = 2` consecutive
  **direct**-ACK probes succeed, drop back to single-hop. A latched leaf whose `UP2` is ACKed
  *via the flood* is still stranded on the direct path, so a flooded ACK does **not** un-latch.

**Best-effort ACK — a documented v1 limit.** Multi-hop uplink is **best-effort**. In a
single-relay topology a lost `RELAYACK2` isn't recovered within the message (the relay's seen-set
suppresses the retransmit forward) — telemetry is loss-tolerant, and the next `msgid` is a fresh
flood. `RELAYACK2` loop-safety rides the `MAX_HOP = 2` hop decrement, not a seen-set; a 3-hop
follow-up would add a `RELAYACK2` seen-set. **Scan-hop throughput** is the other v1 limit: a
stranded leaf never locks a channel (it never hears its owner's HELLO), so it keeps hopping
ch 1/6/11 while scanning and its `UP2` lands on the relay's channel only ~⅓ of the time — the
rig saw R forward ~1 of ~30. **Pre-#13 the delivered count was ZERO**, so v1's bar (a stranded
leaf *can* reach home) is met. **[#126](https://github.com/jphein/smol/issues/126)** (latched-leaf
channel parking) raises that rate and **landed** — closed *completed* 2026-07-15;
`flood.rs:262+` implements `ChannelPark` (park on a signal — a relay echoing our own `UP2`, or a
`RELAYACK2` for us — and reset when the leaf is no longer stranded). ⚠️ **The ~⅓ figure above is
the pre-parking measurement**; the post-parking rate has not been re-measured on the rig.

**The byte-identical invariant (the uplink safety gate).** A non-escalated leaf emits ONLY plain
`RELAY` (`H = 1`), so in the all-hear case **no `UP2`/`RELAYACK2` frame ever exists and nobody
forwards** — behaviour is byte-for-byte identical to pre-#13. The machine-checkable measurement of
this is canary gate **C0 = `fwd = 0` on every node** across a normal all-hear window. Proven
byte-free by cfg-gating, not ELF equality; and a permanent bidirectional wire-compat guard
(`experiments/relay_compat`) `#[path]`-includes the real codec and asserts a new-code frame parses
under a vendored **pre-#13** parser and an old `RELAYACK` parses under the new matcher — the
mixed-fleet / [#124](https://github.com/jphein/smol/issues/124) `UP2`-migration checkpoint.

**Mixed-fleet compat.** All new tags are **additions** (not appends to the fixed-offset
`RELAY`/`RELAYACK` headers): old firmware `classify()`s them to `None` (harmless) and cannot relay
anyway — no flag-day flash. A leaf that needs `H ≥ 2` was, by definition, already stranded pre-#13,
so no rolling-upgrade window can strand a leaf that worked before. #13 only ever **adds** reach.

**Flag.** espnow. **Status.** 🟢 **routed multi-hop is hardware-verified — the first routed frame in
smol's history** — but ⚠️ **that run predates the envelope, and the badge does not transfer to `UP2`.**
On the deaf-list rig (G = id7 crown ↔ F = id9 mutually deaf, R = id8 relaying), F latched at
exactly 3 emit cycles (`escalated to multi-hop (RELAY2) — gateway unreachable, 3 un-ACKed msgs`
— quoted verbatim; ⚠️ **that firmware log line still names the retired frame**, `mode.rs:3879`),
emitted at hop 2, R forwarded, and at **2026-07-14 11:03:19 `smol/9/telemetry` arrived at the
broker from a leaf that provably cannot hear its crown.** The pure decision core
(`SeenSet`/`forward_decision`/`HopLatch`) is host-tested in `experiments/flood_verify` (ALL PASS,
incl. the multi-fragment + hysteresis regressions).

**`UP2`'s own evidence class is 🟡, and the gap is worth naming.** The 2026-07-14 run carried
`RELAY2`; #124 then replaced the frame. `UP2` is verified by **host-tested golden bytes**
(`experiments/relay_compat`: wrap → unwrap → re-parse-inner → clamp) and by every consumer written
since — but **no record shows a `UP2` envelope observed on the fleet.** Observability-via-relay
(`/stat`+`/diag`+`deaf=`) is the thing #124 *delivered*, not a follow-up: closed *completed*
2026-07-15. **The open bench question is therefore: has a `UP2` frame ever flown?** Until that is
answered, 🟡 is the honest badge for the envelope and 🟢 belongs to the capability.

**Source.** `net/wire.rs` (pure codec: `UP2_PREFIX`/`RELAYACK2_PREFIX`, `encode_up2`/`parse_up2`,
`clamp_inner_field_boundary`, `encode_relayack2`/`parse_relayack2`, `synth_origin_mac`); `net/flood.rs` (pure
decision core: `MAX_HOP`/`ESCALATE_STREAK`/`UNLATCH_STREAK`/`PROBE_EVERY`, `SeenSet`,
`forward_decision`, `HopLatch`); live path in `net/mode.rs` (`UP2` service arm + `HopLatch`
drive). Design: `scratch/13-multihop/scope-proposal.md`.

## BATT2 / GRID2 — downlink freshness (#13 Stage B)

**Purpose.** Reach a **stranded** leaf with the HA battery/grid downlink the same way `UP2`
reaches the gateway on the uplink. Plain [BATT](#batt--ha-battery-snapshot)/[GRID](#grid--ha-grid-power-snapshot-16)
are **single-hop** (leaves never re-broadcast — no freshness field, so a re-flood could loop or
overwrite a fresher cache). `BATT2`/`GRID2` add a monotonic freshness counter (`dl_seq`) so a leaf
can **re-flood strictly-newer** data safely — the same strict-newer template `TIME`'s `synced_at`
already uses.

**Layout (13 B tag + 10 B `dl_seq` + 1 B space + ≤96 B payload = ≤120 B).**

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 13 | `b"SMOLv1 BATT2 "` / `b"SMOLv1 GRID2 "` | namespace |
| `dl_seq` | 10 | ASCII (full u32, zero-padded) | downlink freshness — see below |
| ` ` | 1 | space | |
| `payload` | ≤96 | ASCII, display-ready | the **verbatim** `BATT\|`/`GRID\|` payload — byte-identical to the v1 [BATT](#batt--ha-battery-snapshot)/[GRID](#grid--ha-grid-power-snapshot-16) frames |

**`dl_seq` semantics.** The gateway sets `dl_seq` = the **unix-second of the last value change**
(the `TIME synced_at` template — survives a gateway reboot, never wraps within a u32). It is bumped
**only on an actual change**, so an unchanged periodic re-broadcast carries the same `dl_seq` and is
**not** re-flooded (no churn).

**Rules.**
- A leaf **adopts + re-floods** a frame **only when its `dl_seq` is strictly newer** than the one it
  holds — this reaches a stranded leaf via a relay while dropping replays and loops (a stale or
  equal `dl_seq` is ignored). This is the bounded exception to "leaves don't re-flood": it re-floods,
  but only strictly-newer, and only on a real value change (so the uplink `fwd = 0` invariant is
  unaffected — downlink re-floods are counted separately as `dfwd`).
- A **gateway ignores inbound** `BATT2`/`GRID2` — it **is** the source, so it never adopts a downlink
  frame (prevents a relayed copy from racing the source).
- A leaf still parses v1 `BATT`/`GRID` from an **old** gateway (mixed-fleet safe — the tags diverge
  at byte 8, `BATT2` vs `BATT `).
- The freshness is **an unauthenticated transport replay/loop guard only** — a spoofed `dl_seq` is a
  display-DoS at worst, exactly like every other SMOLv1 frame (see Shared conventions → Security). It
  never gates anything safety-critical (contrast #40 OTA's *authed* `fresh_floor` anti-rollback).

**Cadence.** Emitted by the gateway on a value change (fresh `dl_seq`) + the periodic re-emit;
re-flooded onward by a leaf only on strictly-newer adopt. DIAG surfaces the last-adopted `dlseq`
per node (rig P4 watches it advance on a change and stay put on a replay).

**Flag.** espnow. **Status.** 🟢 **hardware-verified end-to-end** (per the #13 close sign-off).
Closes the #10 HA-staleness gap for a multi-hop leaf. The leaf **data-age render** (showing the
downlink's own age on glass) is a small deferred follow-up in the screen domain — this stage
delivers reach + freshness; the existing render shows the value.

**Source.** `net/wire.rs` (`BATT2_PREFIX`/`GRID2_PREFIX`, `encode_dl`/`parse_dl`, `write_u10`/
`parse_u10`); adopt/re-flood + gateway-ignore arms in `net/mode.rs` (`self.batt.dl_seq`).

## mesh-test deaf-list rig hook (#13 Stage C, test-only)

**Purpose.** Force a 2-hop topology (G→R→F) on today's co-located 3-board fleet **without physical
RF attenuation**, so routed multi-hop is validatable before a 4th board / a real out-of-range
placement exists. A node drops inbound frames from a configured set of source MACs at the very top
of `service()` (before OTA dispatch, roster, and any `Frame` parse) — a deterministic stand-in for
"out of RF range."

- **Compile-time, not runtime.** The deaf-list is a per-board constant `board::DEAF_MACS:
  [Option<[u8; 6]>; 4]`, gated behind the **default-OFF** `mesh-test` cargo feature
  (`mesh-test = ["espnow"]`). It is **test scaffolding, per-board like `NODE_ID`**, set in the
  git-ignored `board.rs` (the tracked `board.rs.example` template stays empty; **never commit real
  MACs**). *(Note: an earlier rig design proposed a runtime CFG key `D`; the shipped implementation
  is this simpler compile-time list — there is no CFG-`D` key.)*
- **Never-retained anti-hazard (stronger than a transient CFG key).** Because the list is
  compile-time, it **cannot** exist as a retained-MQTT ghost, and a **production** (non-`mesh-test`)
  build has **no deaf field and no hook at all** — it *physically cannot* be made deaf. A leftover
  entry is cleared only by **re-flashing with an empty list**, and DIAG surfaces `deaf=<active>` /
  `ddrops=<frames-dropped>` (mesh-test-gated) so any leftover is visible in HA at a glance.
- **DIAG.** `deaf`/`ddrops` are appended **only** on a `mesh-test` build → the DIAG string is
  byte-identical on a production build.

**Flag.** `mesh-test` (default off). **Status.** 🟢 the rig that **caught** (and #123 fixed) two
escalation-trigger bugs the green build + host math could not — the C0 over-eager single-message
latch (fixed: K=3 hysteresis) and the P1 under-eager total-deafness starvation (fixed: supersession
feed) — then delivered the first routed frame. **Source.** `board.rs.example` (`DEAF_MACS` template
+ setup notes); drop hook + `deaf`/`ddrops` in `net/mode.rs` (`#[cfg(feature = "mesh-test")]`); rig
design `scratch/smol-dreamteam/13-rig-design.md` (30-min run protocol + P1–P7 gates).

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

**Versioning (`ver`) + forward-compat.** The parser is **length-tolerant**: a frame with the `SNK`
prefix and **≥ 18 B** decodes the stable 18 B v1 core regardless of any trailing bytes. A **v2 = 19 B**
frame appends a `score` byte at `[18]` (`SNK_FRAME_LEN_V2`, defined on **both** the C3 fleet and the
esp32c6-watch); **`ver = 1` implies `score == length`**, so a v1 receiver loses nothing. As of this
writing **no board emits `ver = 2`** — the v2 slot is *reserved-but-inactive*. Who bumps `ver` to 2
(and thus what a `score` that diverges from `length` means), or whether to drop the dead definition,
is the open decision in **#232**. A conformant parser reads `[0..18)` unconditionally and reads
`[18]` **only** when `ver ≥ 2` **and** the frame is ≥ 19 B.

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

## FAM — the Mesh Familiar (#57)

**Purpose.** One living creature inhabits **exactly one board at a time** and migrates across the
mesh when the holder is unplugged. Only the current holder beats `SMOLv1 FAM`; every other board
reconstructs a Weasley-clock pointer ("@ &lt;holder&gt;") from the last beat it heard.

**Frame layout (`FAM_FRAME_LEN = 29 B`, `familiar/mod.rs`).**

| Field | Bytes | Meaning |
|---|---|---|
| tag | 11 | `b"SMOLv1 FAM "` (byte 7 = `F`, unique vs TIME/OTA) |
| `kind` | 1 | `H` heartbeat · `X` handoff (full state) · `C` call ("come here") |
| `holder` | 1 | id of the board the creature is on |
| `target` | 1 | handoff/call destination id |
| `seq` | 2 | u16 LE — monotonic beat counter (RFC-1982 wrap-aware) |
| `seed` | 4 | u32 — identity: derives name + species deterministically on every node (`seed != 0`) |
| `birth` | 4 | u32 — birth time (age/growth stage) |
| `fed` | 4 | u32 — last-fed time (hunger) |
| `mood` | 1 | holder-computed mood token |

- **Exactly-one invariant.** Only the holder beats (~1.5 s, phase-jittered). A holder that hears a
  newer `seq` (or equal `seq` + lower holder-id) yields; a handoff is a type-`X` beat then silence;
  orphan takeover fires after `FAM_LOST_MS` (~12 s) → the nearest survivor re-births from the cached
  seed/birth (same creature, same age). Migration is human-verified on glass.
- **Flag.** espnow. **Status.** 🟢 **merged + on-glass** (#57, PR #99). Source: `familiar/mod.rs`
  (`FAM_PREFIX`, `encode_fam`/`parse_fam`); state machine in `net/mode.rs` (`fam_tick`).

---

## ELECT — the mesh rendezvous-channel announcement (#278/#269)

**Purpose.** Every other frame here says *who* is on the mesh. ELECT says **where the mesh
meets**: the crown announces which channel is the rendezvous, so a fleet can migrate off a
bad channel instead of being pinned to `ESP_NOW_FIXED_CHANNEL` forever. It is the
channel-plane twin of [`HELLO`](#hello--led-handshake), and it beats at the same rate for
exactly that reason.

**Layout (61 B, always — a frame of any other length is not an ELECT frame).**

```text
"SMOLv1 ELECT " <id:3> ' ' <epoch:10> ' ' <ch:2> ' ' <gw:3> ' ' <w:26>
 └── 13 ──────┘                                                 └ 13×2 ┘
```

| Field | Bytes | Encoding | Meaning |
|---|---|---|---|
| tag | 13 | `b"SMOLv1 ELECT "` | namespace; byte 7 = `'E'`, unused by any other tag (`H A B T G C S D R U F`) |
| `id` | 3 | ASCII `NNN` | announcing crown's node id |
| ` ` | 1 | space | |
| `epoch` | 10 | ASCII (full u32) | decision epoch — **higher epoch wins** |
| ` ` | 1 | space | |
| `ch` | 2 | ASCII `CC` | announced rendezvous channel (1–13) |
| ` ` | 1 | space | |
| `gw` | 3 | ASCII `NNN` | id of the gateway the decision names |
| ` ` | 1 | space | |
| `w` | 26 | 13 × 2 ASCII | per-channel weight vector, one zero-padded 2-digit value per 2.4 GHz channel |

Plus the [9 B group-MAC trailer](#the-group-mac-trailer-190--every-byte-count-above-omits-it)
→ **70 B on the air**, ~280 bit/s from the single crown at the idle rate.

**Fixed width is a security property here, not a convenience.** Because smol elects a
*channel* rather than a BSSID, the candidate set is a constant — the 13 channels of the
2.4 GHz band. So the frame has no length field, no repetition and no bound to enforce: a
malformed or hostile frame is simply **not 61 bytes**, or fails a digit check. There is
nothing for an attacker to grow.

**The weight vector is honestly mostly zeros.** smol does not run per-channel scans on the
announce path — that would cost the association — so the only channel it can truthfully
weigh is the one its own AP is on. Every other slot is a **measured zero, not a guess**, and
`weight()` returns 0 below `USABLE_MIN_DBM` (−82 dBm) so an unusable AP contributes nothing
rather than a small lie.

**Cadence.** A burst of `ANNOUNCE_BURST` = **6** frames spaced `ANNOUNCE_GAP_MS` = **120 ms**,
then steady-state `ANNOUNCE_IDLE_MS` = **2 s**. The burst buys ~600 ms of coverage against
collision and queue loss; it buys **nothing** for a leaf parked on another channel, which is
deaf for a full `DWELL_MS` (1500 ms) and would miss every frame of it. Those leaves are
recovered by the recovery ladder, never by repeats.

**Who sends.** Crown only — `elect_tick` early-returns on a leaf, because the mesh channel
*is* the crown's channel. A demoted ex-crown under `silent_until_relock` stops announcing, the
same abdication rule HELLO follows.

**⚠️ ANNOUNCING is on; ACTING on an announcement is not.** Following is gated behind
`net::election::FOLLOW_ENABLED`, deliberately: a frame nobody emits cannot be proven on
hardware, so #278 landed the emitter first and made the flip criterion *evidence from a real
fleet*, not a code review. Read `elect=` in the [DIAG record](#diag--retained-per-node-health-record-704974100)
to see what a board heard — and note that a **missing** `elect=` means the record went over
budget and the field was shed, **never** "this board never heard the crown".

**🔒 ELECT is the one SMOLv1 frame that is authenticated by design.** An unauthenticated
channel announcement is a remote fleet-stranding primitive — *"everybody move to channel N"*
with no way back — so it must carry the group MAC. The send path is a **design invariant, not
a style note**: `SealedElect` has no byte accessor, its only exit is `emit`, and the only
`GroupMacSink` implementation routes to `send_to` (the choke that appends the trailer).
`tools/check_elect_send_path.py` machine-checks the shapes the type system cannot.

**Key constants** (`net/mesh_elect.rs`): `N_CHANNELS` 13 · `RENDEZVOUS_CHANNEL` 6 ·
`SETTLE_MS` 30 s · `PROBATION_MS` 45 s · `MARGIN_FLOOR` 12 · `OBS_STALE_MS` 60 s ·
`USABLE_MIN_DBM` −82 dBm · `WEIGHT_CEIL_DBM` −35 dBm.

**Flag.** espnow. **Status.** 🟡 **compile-verified** — the frame, the epoch/anti-flap core
and the send-path invariant are merged and host-tested verbatim by
`experiments/election_verify`, and `tools/check_elect_send_path.py` gates the emitter. The
**follow** path is observe-only and its on-fleet evidence is #278's open flip criterion, so
this row is deliberately not 🟢: nothing in this section was captured off the wire.
**Source.** `net/mesh_elect.rs` (`wire::ELECT_PREFIX`, `ELECT_LEN`, `encode`/`parse`,
`SealedElect`); announce path `net/mode.rs::broadcast_elect` (`:2960`) + `elect_tick`;
follow gate `net/election.rs::FOLLOW_ENABLED`.

---

## Leaf mesh-OTA frames (#40)

Four frames deliver a **signed firmware image to an ESP-NOW-only leaf over the mesh** — the
gateway is the leaf's OTA proxy (it fetches the staged image over WiFi into its own inactive
slot, then relays it). **Canary-one-leaf:** exactly one leaf id is targeted; no image is ever
broadcast to the whole mesh. Full operator flow in
[ota.md](ota.md#leaf-mesh-ota--updating-esp-now-only-leaves-40).

**OTAM** — `SMOLv1 OTAM ` — gateway→leaf, the signed session announce.

| field | bytes | meaning |
|---|---|---|
| tag | 12 | `b"SMOLv1 OTAM "` |
| target | 3 | ASCII leaf id (the canary target) |
| session | 2 | LE session id (this transfer instance) |
| mlen | 1 | manifest length |
| manifest `M` | ≤96 | `"build\|size\|sha256hex"` — the exact signed bytes |
| sig | 64 | Ed25519 signature over `M` |

The leaf **verifies the signature before it flashes anything** — a bad sig changes no state.

**OTAD** — `SMOLv1 OTAD ` — gateway→leaf, one image chunk (max **250 B** = the ESP-NOW MTU).

| field | bytes | meaning |
|---|---|---|
| tag | 12 | `b"SMOLv1 OTAD "` |
| target | 3 | ASCII leaf id |
| session | 2 | LE session id |
| seq | 2 | LE chunk index; image bytes land at `seq · 231` |
| payload | ≤231 | image bytes |

Every chunk is bounds-checked against the *signed* `size` before any write, and the writer is
partition-scoped — an out-of-range `seq` physically cannot reach the active slot or `otadata`.

**OTAN** — `SMOLv1 OTAN ` — leaf→gateway (**unicast**), the windowed NAK.

| field | bytes | meaning |
|---|---|---|
| tag | 12 | `b"SMOLv1 OTAN "` |
| target | 3 | ASCII leaf id (the sender) |
| session | 2 | LE session id |
| window | 2 | LE window base (chunk index of the window start) |
| bitmap | 8 | one bit per chunk in the 64-chunk window; **set = still missing** |

The gateway retransmits only the set bits. An **all-zero bitmap = "window complete, advance"** —
the only positive ack. The **last** window gets no advance-ack (the leaf finalizes + reboots), so
the gateway treats last-window exhaustion as a *confirm*, not a failure.

**LDBG** — `SMOLv1 LDBG ` — leaf→broadcast, a fixed **21-byte** OTA receive-side self-report (diagnostic).

| field | bytes | meaning |
|---|---|---|
| tag | 12 | `b"SMOLv1 LDBG "` |
| id | 3 | ASCII leaf id |
| otam_heard | 2 | LE count of OTAMs the leaf received (rx>0 proves it's online) |
| verdict | 1 | receive-side outcome code |
| otan_sent | 2 | LE count of NAKs the leaf sent |
| ch | 1 | the leaf's current channel; `ch≠6` = it drifted off ch6 during the fetch |

LDBG names *why* a `relay-failed` had `otan=0` (a leaf that heard no OTAM = an RX problem, not a
dead leaf). It surfaces on the retained `smol/<leaf>/ota/relaydiag` topic.

## Image target descriptor — what an image is FOR (#349)

Not a frame: a **16-byte record embedded in the firmware image itself**, so a board can decide
whether an image it has just downloaded is *for a board like it*. The OTA chain already proves
the image is **authentic** (ed25519 over `build|size|sha256`) and **intact** (readback SHA-256);
this is the missing third property, **suitability**. `smol/ota/staged` is retained fleet-wide, so
every board sees every announcement — with the fleet spanning C3-OLED / C3-SuperMini / C6 / S3
that stops being theoretical.

Cross-*chip* is already caught by the second-stage bootloader (`esp_image_header_t.chip_id`) —
but only by boot-looping into rollback. A cross-*tier* image passes every check smol had **and
boots**.

**Layout** — little-endian, 16 bytes, found by a **linear scan for the magic** (no fixed offset:
where `.rodata` lands is a linker detail). Source: `rust/clock/src/net/target.rs`.

| off | bytes | field | meaning |
|---|---|---|---|
| 0 | 4 | magic | `"SMLT"` (`0x53 0x4D 0x4C 0x54`) |
| 4 | 1 | desc_version | descriptor FORMAT version (currently **1**) |
| 5 | 1 | chip | `1`=ESP32-C3 `2`=ESP32-C6 `3`=ESP32-S3; `0`=unknown. Derived by `build.rs` **from the target triple**, so it cannot misreport |
| 6 | 2 | features | LE capability bitset: `1<<0` wifi · `1<<1` espnow · `1<<2` io · `1<<3` cast · `1<<4` wled · `1<<5` bard · `1<<6` mesh-test · `1<<7` coexist-soak · `1<<8` stack-paint |
| 8 | 1 | compat | the persistent-state (NVS record) layout this image writes |
| 9 | 1 | min_from_compat | the **oldest** running `compat` this image will install over |
| 10 | 2 | reserved | 0 (inside the checksum) |
| 12 | 4 | checksum | LE FNV-1a/32 over bytes 0..12 |

The checksum is what lets a 4-byte magic be trusted after a scan over ~600 KB of arbitrary
bytes — a real 1,156,864 B fleet image contains **two** `SMLT` occurrences and only **one** that
checksums (the other is the scanner's own constant), so scanning resumes past a failed candidate.

**The five refusals** (`net::target::decide`, in order). All are DATA compared against DATA;
nothing names a board, and a new legitimate cross-flash is a bitset, never a branch:

| reason | token | rule |
|---|---|---|
| unreadable | `tgt-descver` | `image.desc_version > 1` — cannot read its fields, so cannot judge it |
| wrong silicon | `tgt-chip` | `image.chip != running.chip` |
| state too old | `tgt-compat` | `running.compat < image.min_from_compat` |
| strands the board | `tgt-featloss` | image drops a feature in `wifi\|espnow` that this board has. **espnow counts**: `run_ota_fetch` is `cfg(espnow)`, so a wifi-only image can never self-update again |
| bench tier | `tgt-bench` | image carries a `mesh-test`/`coexist-soak`/`stack-paint` bit this board does not itself run |
| no descriptor | `tgt-absent` | no valid record anywhere in the image — fails **closed** |

**Where it is checked.** In `ImageWriter::finalize` / `LeafImageWriter::finalize`, over the
**slot readback** that already computes the integrity SHA — one pass, two consumers, so the
identity judged is provably the identity hashed. Scanning the incoming *stream* instead would
break on a #267-resumed fetch, which skips a prefix already on flash. `otadata` is untouched at
that point, so a refusal costs a download and nothing else.

**Where it surfaces.** Self-fetch → retained `smol/<id>/ota/diag` as `at=<token>` (fail-point
codes `13 + reason`, alongside the existing `assoc`/`stall`/`verify`). Leaf mesh-OTA → the LDBG
`verdict` byte as `8 + reason` (`8` absent … `13` bench), i.e. `smol/<leaf>/ota/relaydiag`'s
`V<n>`. Release images are serial-silent, so these are the only account of a refusal.

**For a non-Rust fleet member** (#331 Phase 1 keeps Ember on ESPHome) the emittable form is:

```c
struct __attribute__((packed)) smol_target_desc_t {
  uint32_t magic;         // 0x544C4D53  ("SMLT" little-endian)
  uint8_t  desc_version;  // 1
  uint8_t  chip;          // 1=C3 2=C6 3=S3
  uint16_t features;      // bitset above
  uint8_t  compat;
  uint8_t  min_from_compat;
  uint16_t reserved;      // 0
  uint32_t checksum;      // FNV-1a/32 over the first 12 bytes
};
```

**Not in here, on purpose:** the board **variant** (OLED vs SuperMini). That is detected at
runtime (`crate::headless()`); a variant that is not a build artifact needs no OTA guard, no CI
job, and no row in a table.

### The signed manifest carries the target too (`OTA2|`)

The descriptor above says what an image is *after* you have downloaded it. The staged announce now
says the same thing *before*, so an unsuitable image costs nothing instead of ~1.1 MB.

| generation | staged payload | signed M |
|---|---|---|
| #32 legacy | `OTA\|build\|size\|sha256\|sig\|url` | `build\|size\|sha256` |
| #349 | `OTA2\|build\|size\|sha256\|**target**\|sig\|url` | `build\|size\|sha256\|**target**` |

`target` is the **same 16 bytes as the embedded descriptor**, lowercase hex (32 chars) — not a
second, tidier encoding. `ota_publish.sh` *extracts* it from the binary it just built rather than
recomputing it from build flags, so the manifest and the image cannot disagree by construction.
Worst-case M is **119 B** (two 10-digit `u32`s), against `SIGNED_MSG_MAX = 128`; `ota_mesh.rs` now
carries real `const` asserts that `OTAM`/`ODEL` still fit the 250 B ESP-NOW MTU, a bound that until
now lived only in a prose comment.

The target sits **inside** M, before the signature field, for two reasons: M stays a contiguous
prefix of the payload (so it can be rebuilt from exact wire bytes with no re-serialization), and an
unauthenticated target could be stripped or rewritten by anyone with broker write access — a
suitability check on unsigned data is theatre.

Versioning is **by prefix, never by field shape**. `strip_prefix("OTA|")` cannot match `"OTA2|"`,
so pre-#349 firmware ignores a new line *cleanly* rather than mis-slicing it into a bogus fetch.
That property is asserted in `experiments/target_guard_verify`, not assumed.

Refusals here are the same `TargetReject` tokens, surfaced as `Reject::Target(..)`. A legacy line
states no target and is therefore **never refused pre-fetch** — `None` means "this staging did not
say", not "suitable for anyone".

### Per-chip staged topic

`smol/ota/staged/<chip>` (e.g. `smol/ota/staged/esp32c3`), alongside the fleet-wide
`smol/ota/staged`. One retained line arming every board was fine while the fleet was one chip; with
C3/C6/S3 it is a correctness bug independent of any device-side guard.

**Chip only, not chip+tier.** A "tier" here is a feature *bitset*, not a name; putting it in a topic
segment would mint a new topic whenever a feature moves and make each board's subscription brittle
— the per-env explosion WLED's maintainers say did not scale. Chip is small, closed and stable, and
it is the axis where a wrong image is worst. Every finer distinction rides the signed target tuple
and is re-checked against the image's own descriptor.

### Migration — which step needs what

This is the part to get right, because only one step can strand a board. **A merged `main` is not a
rolled fleet**: boards run whatever was last installed, so the reader only exists in the field after
boards carry it.

| step | precondition | why |
|---|---|---|
| 1. firmware parses **both** generations, subscribes both topics | **merged main** | purely additive; a board that never sees an `OTA2\|` line behaves exactly as before |
| 2. publisher **dual-stages** (legacy line unchanged + `OTA2\|` per-chip) | **merged main** | old firmware keeps reading the legacy topic it always read; new firmware prefers the per-chip line. Nothing is taken away, so nothing can be stranded |
| 3. retire the fleet-wide `smol/ota/staged` line | **a ROLLED fleet** | the moment it stops being published, any board that cannot parse `OTA2\|` stops seeing updates — silently, since its symptom is simply never updating |

Step 3 has a second, less obvious dependency: a crown relays the **signed M verbatim** to leaves
over `OTAM`, so once a crown installs from an `OTA2` line it relays a 119 B M that a pre-#349 leaf
rejects (`m_len > SIGNED_MSG_MAX`). Leaf mesh-OTA to un-upgraded leaves therefore also depends on
the legacy path. Both reasons point the same way: **keep dual-staging until every board reports a
build that carries the `OTA2` parser.**

**Still not done:** no publisher-side `--force-target-mismatch` escape hatch (issue item 6) and no
device-side override. Today's escape hatch is a USB flash — which is also the honest one, since a
board that refuses an image is not reachable by the mechanism you would use to override it.

## MQTT burst — the LAN transport that retires the UDP collector

**What changed (v2 pivot).** v1 shipped a Python **collector** on disks that took
relay telemetry over UDP `:9999` and answered a `BATT?` query. **v2 retires it.**
The board now speaks **MQTT 3.1.1** (hand-rolled, QoS 0, no TLS) directly to Home
Assistant's **Mosquitto** broker — so telemetry lands as native HA entities and the
battery downlink is a retained broker message. No Python middleman. *(As of build 40
the UDP collector path — [relay.md](relay.md) — is **retired**: stopped/disabled on
disks, JSONL archived. Rollback = git.)*

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
| `smol/<id>/status` | **yes** | 0 | the leaf's render-state, republished by the gateway from a [`SMOLv1 STAT`](#stat--leaf-render-state-uplink-50b) frame. |
| `smol/<id>/diag` | **yes** | 0 | the [DIAG](#diag--retained-per-node-health-record-704974100) record, republished by the gateway. |
| `smol/<id>/uplink` | no | 0 | the **gateway's own** uplink RSSI (dBm), #325. |
| `homeassistant/…/config` | **yes** | 0 | HA MQTT-discovery JSON — **three families**, see *Discovery config* below. Published by the **gateway** on each connect. |

*Wire detail (`mqtt.rs::encode_publish`):* each PUBLISH's fixed-header byte 0 is
`0x30 | retain` — QoS 0, RETAIN bit (bit 0) set only on the gateway's **retained**
publishes (the discovery configs); telemetry PUBLISHes are `0x30` (not retained). The
`smol/display/batt` downlink is set retained by **HA**, not the gateway.

**Discovery config.** Retained JSON, republished on every connect. The scheme below
**supersedes the "pinned spec v2" single-config form** this section used to describe: that
was one text sensor on `homeassistant/sensor/smol<id>/telemetry/config`, and it has not been
what the firmware emits since #12 → #309 → #325 → #352.

There are now **three families**, all retained and idempotent:

| family | topic | count | carries the device extras? |
|---|---|---|---|
| telemetry (#12) | `homeassistant/sensor/smol<id>/<field>/config` | 3 — `temp`, `voltage`, `status` | **only `status`** |
| gateway uplink (#325) | `homeassistant/sensor/smol<id>/uplink/config` | 1 (gateway's own) | **yes** |
| DIAG (#309) | `homeassistant/<component>/smol<id>/<object_id>/config` | one per DIAG row | no |

The three telemetry sensors all read the **same** `smol/<id>/telemetry` line and split it with
a `value_template` (`temp` → `p[0]` less its `F`, `voltage` → `p[1]` less its `V`, `status` →
the remainder). Shape of one, with the substituted fields marked:

```json
{
  "unique_id": "smol<id>_<field>",
  "object_id": "smol_<id>_<field>",
  "has_entity_name": true,
  "name": "<Field>",
  "state_topic": "smol/<id>/telemetry",
  "value_template": "<jinja>",
  "expire_after": 300,
  "device": {
    "identifiers": ["smol<id>"],
    "name": "smol <id> <adjective> <noun>",
    "model": "smol ESP32-C3 OLED",
    "manufacturer": "jphein",
    "sw_version": "v<build> <sigil-word>"
  }
}
```

**`model`/`manufacturer` are the [`BoardProfile`](../rust/clock/src/net/profile.rs) fragment
(#352)** — one of `smol ESP32-C3 OLED` · `smol ESP32-C3 SuperMini` · `smol ESP32-C6 Watch` ·
`smol ESP32-S3 Ember`, chosen at runtime from `(chip, has_display)`.

⚠️ **They ride exactly ONE telemetry config (`status`), on purpose.** HA merges device blocks
across discovery messages sharing `identifiers`, so they only need to arrive once — and
repeating them in all three cost 3 × 48 B and pushed **29 of the 256 possible ids over the
512 B packet buffer**. Over-budget is *not* truncation: `encode_publish` returns `None`, the
config is never sent, and **the entity is silently never created**. `status` carries them
because it has the shortest template and no `extra`, so it has the most room. See
`DISCOVERY_BUDGET` and the `const` asserts around it.

⚠️ **A gateway publishes discovery for every id whose telemetry it relays — including C6
watches — so it must NOT attach its own device extras to those.** Doing so made a C3 crown
declare `model: smol ESP32-C3` for a watch (2026-07-28). The extras are for *self* only.

**Node removal:** publish an **empty** retained payload to the config topic — for **every**
config the node has, not just one (ops-checklist item — see [`ha/README.md`](../ha/README.md)).

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

**Status.** 🟢 **hardware-verified — build 40 "Pressed Oven" (`190c2bf`), 2026-07-08.**
Flashed on real boards, no panics. A **wireless** gateway on the boards' VLAN completes
TCP + **CONNACK** at both boot and flush; the retained `smol/display/batt` downlink is
received and cached (**31 B, byte-exact vs HA**, twice); telemetry PUBLISH auto-creates
HA entities via discovery (`sensor.smol_7…` live), and a **live leaf's** telemetry rode
`leaf → gateway → MQTT → HA` (`sensor.smol_8…`) — the full uplink path proven on
silicon. The v1 UDP collector is **retired** (stopped/disabled, JSONL archived). Rough
edge: on a boot where SNTP exhausts the burst budget the boot-MQTT downlink is starved
and recovers on the next flush (finding #15).

**Source.** Firmware `rust/clock/src/net/mqtt.rs` (hand-rolled MQTT 3.1.1 / QoS 0):
`encode_connect` (`:85`), `encode_publish` (`:105`, RETAIN = bit 0 of byte 0),
`encode_subscribe` (`:117`); driven by `mqtt_session` (`net/wifi.rs:502`), entered
from `run_mqtt_burst` (`net/wifi.rs:674`); broker addr/creds in `secrets.rs`. HA side
`ha/packages/smol_mesh.yaml` + [`ha/README.md`](../ha/README.md); team spec
`scratch/smol-ha-batt/spec.md` v2.

---

## Honesty caveats

- **Verification is per-frame and current as of 2026-07-14.** HELLO/ACK and **TIME
  (2-board adoption)** are hardware-verified. BEACON is compile-verified (runs in
  Bench mode, not accuracy-checked). RELAY/RELAYACK are **hardware-proven e2e** (sustained
  `node_id 8` telemetry to the collector, wave 6). SNK is **flashed on the fleet** (build 36).
- **Routed multi-hop (#13) shipped 2026-07-14 (PR #123, `0b83714`).** `UP2`/RELAYACK2 +
  BATT2/GRID2 are hardware-verified end-to-end (as `RELAY2`, the tag #124 later replaced) — the **first routed frame** (a crown-deaf leaf's
  telemetry home via a relay) landed at the broker 2026-07-14 11:03:19. Honest v1 limits: uplink
  ACK is **best-effort**, and a stranded leaf's scan-hopping capped throughput (R forwarded ~1 of
  ~30) until [#126](https://github.com/jphein/smol/issues/126) (channel parking) **landed**
  2026-07-15 — post-parking rate not re-measured. Observability-via-relay
  (`/stat`+`/diag`+`deaf=`) **shipped** with the [#124](https://github.com/jphein/smol/issues/124)
  `UP2` envelope, also 2026-07-15; ⚠️ `UP2` itself is 🟡 (host-golden), not fleet-observed. The all-hear byte-identical invariant (`fwd = 0`) is machine-checked (C0)
  and guarded executable (`relay_compat`).
- **MQTT burst is hardware-verified; the BATT frame's leaf delivery is not (build 40,
  2026-07-08).** The [MQTT burst](#mqtt-burst--the-lan-transport-that-retires-the-udp-collector)
  ran on real boards — wireless CONNACK, retained downlink cached (31 B byte-exact),
  discovery entities live, and a leaf's telemetry relayed `leaf → gateway → MQTT → HA`;
  the v1 UDP collector is retired. The [`SMOLv1 BATT` frame](#batt--ha-battery-snapshot)
  is byte-locked and the gateway broadcasts + self-renders it, but **leaf-side frame
  receipt is inferred, not observed** (same mechanism as the hardware-verified TIME
  adoption; receipt unlogged — finding #15 — and the fleet is currently all-gateway).
  Two more honest caveats: an HA outage leaves a live-looking retained payload (boards
  show fetch-age, not data-age); and a boot where SNTP exhausts the budget starves the
  boot-MQTT downlink until the next flush (#15).
- **ESP-NOW airtime/throughput/RX-reliability under COEXIST** are unmeasured on
  hardware — reasoned from the ESP-NOW API (see `nebula-espnow-gateway.md`, which read
  `esp-wifi 0.15.0`; the tree now pins its renamed successor `esp-radio 0.18`), not a
  bench run.
- **The code is authoritative.** RELAY sizes/fields especially may move while the
  bridge lands — re-check `mode.rs` before depending on the exact bytes.

## Sources
- `rust/clock/src/net/mode.rs` — frame consts, `Frame` enum, encode/parse helpers, relay bridge section (read-only).
- `rust/clock/src/net/wire.rs` — the **pure** relay-family wire codec (#13): RELAY/RELAYACK + RELAY2/RELAYACK2 + BATT2/GRID2 encode/parse + ASCII field helpers; host-testable, no esp-hal deps.
- `rust/clock/src/net/flood.rs` — the **pure** managed-flood decision core (#13): `SeenSet`, `forward_decision`, `HopLatch` + the `MAX_HOP`/`ESCALATE_STREAK`/`UNLATCH_STREAK`/`PROBE_EVERY` consts. Host-tested in `experiments/flood_verify`; wire-compat guarded by `experiments/relay_compat`.
- `rust/clock/src/net/wifi.rs` — the MQTT burst (hand-rolled MQTT 3.1.1 client) + broker consts (v2); replaces the UDP collector egress.
- `ha/packages/smol_mesh.yaml` + `ha/README.md` — the HA automation that publishes the retained `smol/display/batt` downlink + install/discovery notes.
- `collector/collector.py` — the v1 UDP relay collector, **retired** as of build 40 (see [relay.md](relay.md)); superseded by the MQTT burst above.
- `scratch/smol-ha-batt/spec.md` (v2) — the MQTT-native architecture (uplink/downlink, discovery, retained) + role boundaries.
- `mesh-time-sync-spec.md`, `relay-bridge-spec.md`, `mmo-snake-netcode.md`, `mmo-snake-design.md` — design specs (scratch).
- `nebula-espnow-gateway.md` — verified ESP-NOW limits (read from `esp-wifi 0.15.0`; re-verified unchanged against `esp-radio 0.18.0` on 2026-08-24) + the gateway feasibility verdict.
- `lucid-hw-verify.md` / `board1-boot-ANNOTATED.md` — today's hardware boot capture.
