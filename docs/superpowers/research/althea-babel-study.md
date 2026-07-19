# Althea firmware + Babel routing — a study, mapped to smol's mesh

**Issue:** [#163](https://github.com/jphein/smol/issues/163) · **Author:** morpheus (dream team) · **Date:** 2026-07-18
**Status:** study + design only — **no firmware changes in this lane.**

Study material (cloned OUT of the repo, into `~/Projects/althea-study/`, never committed):
- `jech/babeld` @ HEAD — the Babel reference implementation (C). License: **MIT** (Juliusz Chroboczek).
- `althea-net/althea_rs` @ HEAD — Althea's Rita agent monorepo (Rust). License: **Apache-2.0**.
- [RFC 8966](https://www.rfc-editor.org/rfc/rfc8966.txt) — *The Babel Routing Protocol* (the normative spec).

Both licenses are permissive: we may study, and port ideas or code with attribution.

---

## TL;DR — the verdict

**Babel is a beautiful, mature, loop-free distance-vector routing protocol for general
multi-hop wireless meshes. smol is not a general multi-hop mesh — it is a single-radio
*star* rooted at one elected gateway, with a 250-byte MTU and a mostly-all-hear fleet of
4–30 co-located boards. Adopting Babel wholesale would import a solution to a problem smol
does not have.** Two of Babel's *ideas*, however, are directly worth borrowing, and one of
them plugs a real gap (#155).

Ranked:

| Rank | Item | Verdict | Why |
|---|---|---|---|
| **1** | **ETX-style link metric** (from Hello-reception history) | **ADOPT** (borrow the idea) | smol already keeps per-peer Hello history for Connected/Detected; turning that into a 2-out-of-3 ETX cost gives smol the link-quality signal it currently lacks — the exact input #155 and relay-selection need. Cheap, high-value. → **issue filed.** |
| **2** | **Additive metric-blend** (Althea's price-as-cost trick) | **ADOPT-lite** | Althea proves you can fold an arbitrary per-link cost (their "price") into an additive metric without touching the routing core. smol can blend link-ETX + burst-health into one comparable channel/relay score. → informs #155. |
| **3** | **Feasibility + seqno loop-freedom** | **ADMIRE** | smol's BATT2/GRID2 `dl_seq` strict-newer gate is *already* a targeted slice of Babel's seqno. Full feasibility/source-tables are overkill for a 2-hop star. Revisit only if topology deepens. |
| **4** | **Route-selection hysteresis** (smoothed metric) | **ADMIRE** — already done | smol's `HopLatch` (K=3 escalate / K=2 un-latch) already embodies Babel's anti-flap instinct. Validated, no change. |
| **5** | **Babel wholesale** (route/source/neighbour tables, periodic Updates, any-to-any routing) | **DON'T ADOPT** | Solves any-to-any multi-hop; smol has one destination (the gateway) and ≤2 hops. The route-*installation* half is inapplicable (smol has no IP FIB). Airtime + MTU friction. Revisit only if smol grows a deep, many-destination mesh (P2P chat, multi-gateway, mesh-RPG world routing). |
| **6** | **Babel to fix #155** | **CATEGORY ERROR** | Babel routes *over* an assumed-working L2; it does not select radio channels. #155 is a single-radio spectrum/coexist problem. Babel can *inform* it (via #1's ETX) but cannot *solve* it. |

**One-line answer to the issue's core question** — *"does Babel's metric+feasibility beat
smol's flood+K-escalation?"*: **No, not for smol's current shape** — but Babel's *link
metric* beats smol's *absence of one*, and that metric is the adoptable prize.

---

## 1. Babel in one page (RFC 8966 + babeld)

Babel is a loop-free distance-vector protocol designed for lossy wireless. Every node
advertises, for each destination prefix, a `(seqno, metric)` distance; neighbours pick the
feasible next-hop with the lowest total metric.

- **Metric — additive ETX.** Link cost combines both directions of Hello reception. babeld's
  `rxcost = (0x8000 * ifp->cost) / (sreach + 1)` where `sreach` is smoothed reachability from
  a 16-bit Hello-history window (the classic **2-out-of-3** ETX), then `neighbour_cost`
  combines rx+tx (`neighbour.c:283–355`). Route metric accumulates additively per hop,
  `M(c, m) = c + m`, with **strict monotonicity `M(c,m) > m`** — this is what makes routes
  loop-free (RFC 8966 §3.5.2). Infinity = `0xFFFF` (retraction).
- **Feasibility condition (the loop-freedom core, §3.5.1).** A received `(seqno, metric)` is
  *feasible* iff it is a retraction, OR no source entry exists, OR `seqno > seqno_stored`, OR
  `seqno == seqno_stored AND metric < metric_stored`. A node only ever installs a feasible
  route, which guarantees the advertised feasibility distance strictly decreases toward the
  source — **no cycle can form** (single-originator case).
- **Sequence numbers (§3.2.1, §3.8).** 16-bit mod 2¹⁶. A node bumps its own seqno **only in
  response to a seqno request** (never spontaneously, and never by >1). Seqno requests are
  forwarded hop-by-hop to un-stick a node starved of a feasible route after a metric worsens.
- **Hysteresis (§route.c).** Optional exponential **smoothed metric** (`smoothing_half_life`,
  `route_smoothed_metric`) plus a switch-margin so a marginally-better route doesn't cause
  flapping.
- **Timers.** Wireless **Hello every 4 s** (`babeld.c:311`), IHU ≈ 3× Hello, periodic Update
  ≈ 4× Hello (~16 s), route expiry a small multiple of the interval. Steady-state traffic is
  a few small TLVs per neighbour every few seconds.
- **Wire format (§4).** 4-byte packet header (magic 42, version 2, 2-byte body length) + a
  sequence of `type(1)+len(1)+body` TLVs, with **stateful compression** (a running default
  prefix/next-hop/router-id so Updates only carry deltas). Key body sizes:

  | TLV | Body bytes | Note |
  |---|---|---|
  | Hello | 6 | flags(2)+seqno(2)+interval(2) |
  | IHU | 6 + addr(1–16) | rxcost(2)+interval(2)+… |
  | Router-Id | 10 | 8-byte router-id |
  | **Update** | 10 + prefix(0–16) | flags/plen/omitted + interval(2)+seqno(2)+metric(2)+prefix delta |
  | Seqno Request | 16 + prefix | 8-byte router-id + hop count |

  A Hello is ~8 bytes on the wire; a typical compressed Update is ~12–26 bytes. **But §4
  mandates a node be able to *receive* ≥512-byte packets** — an assumption smol's 250-byte
  ESP-NOW frame violates (see §4 below).
- **State (§3.2).** Per-interface (Hello seqno + timers), per-**neighbour** (~32 B: Hello
  histories, rx/tx cost, timers), per-**source** (~16 B: feasibility distance), per-**route**
  (~48 B: metric, seqno, next-hop, flags, expiry).

**Loop-freedom guarantee:** with a single originator per prefix, the feasibility condition
keeps the forwarding graph acyclic *at all times*; with multiple originators, transient loops
decay in time proportional to the loop diameter (§2.4–2.7). This is Babel's headline property
and the reason it's trusted on real lossy meshes.

---

## 2. Althea = stock(-ish) babeld + a Rust payment/policy overlay

The most important architectural finding: **Althea does not reimplement routing.** It runs
**babeld** (patched with a *price* extension) and layers its Rust agent **Rita** on top,
talking to the daemon over babeld's **local TCP management socket**.

- **`babel_monitor` crate** (`althea_rs/babel_monitor/src/lib.rs`) opens a `TcpStream` to the
  babel management socket (`open_babel_stream`, `:77–86`), issues text commands
  (`run_command`, `:180`), and parses the daemon's text output into `Route`/`Neighbor`/
  `Interface` structs (`parse_routes`, `parse_neighs`). A real installed-route line looks like:

  ```
  add route … prefix 10.28.7.7/32 … metric 1596 price 3072 fee 3072 refmetric 638
  full-path-rtt 22.805 via fe80::… if wlan0
  ```

  So a route carries **`metric`** (ETX-derived), **`price`/`fee`** (the payment overlay), and
  **`full-path-rtt`**. Rita **reads** routes/neighbours/prices; it **writes** only two knobs:
  `set_local_fee` (`:211`, this node's per-byte fee) and `set_metric_factor` (`:221`, the
  price-vs-quality blend: *"higher → prefer QoS, lower → prefer price"*).
- **The price extension is an additive metric contribution.** Althea patches babeld so a route's
  effective cost blends ETX *and* the summed downstream price; the routing algorithm
  (feasibility, seqno) is untouched. **This is the key transferable trick**: any per-link cost
  can ride an additive metric without redesigning the routing core (see adopt-lite #2).
- **What Rita adds (all overlay, none of it routing):** per-byte micro-payments / debt-keeping,
  exit tunnels (WireGuard), NAT/DHCP, policy, an on-chain settlement bridge. This is
  ISP-business-logic bolted above a routed IP network.
- **What is fundamentally Linux-bound:** the babeld daemon itself (a POSIX process with
  sockets/timers), the **kernel FIB** it installs routes into via **netlink** (`kernel_netlink.c`),
  **WireGuard** tunnels, **iptables/NAT**, and a full dual-stack IPv6 network. Althea targets
  **OpenWRT home routers** — tens-to-hundreds of MB RAM, MIPS/ARM, an OS network stack. None of
  that layer is portable to a microcontroller, and none of it needs to be: it's the ISP product,
  not the routing insight.

**Takeaway for smol:** the *interesting* half of Althea is a 4-second-Hello ETX router with a
pluggable additive cost. The rest is a Linux ISP appliance. smol wants the former's *idea*, not
the latter's *stack*.

---

## 3. smol's routing today (the thing we'd be replacing)

smol's mesh is **not** a routed IP network. It is app-level ESP-NOW frames on one channel,
with a **star** shape and a couple of broadcast games:

- **Topology:** leaves ↔ one **elected crown/gateway** (#76 election over retained
  `smol/mesh/channel` = `MC|owner|ch|seq`). Uplink telemetry (RELAY), downlink HA data
  (BATT/GRID), time sync (TIME flood-converge), plus broadcast SNK/FAM. **There is no
  any-to-any unicast need.**
- **Multi-hop (#13, `net/flood.rs`):** Meshtastic-lineage **managed flood** — a hop-limit
  (`MAX_HOP = 2`), an `(origin, msgid, frag)` **seen-set** (16-slot ring), a `forward_decision`,
  and a **`HopLatch`** escalation state machine. It is **table-free** (no routes, no metric, no
  per-neighbour cost) and **rides election/roam for free**. It engages **only for a genuinely
  stranded leaf**; in the all-hear case it is byte-identical to single-hop (`fwd = 0` canary).
- **Escalation hysteresis:** latch to multi-hop only after `ESCALATE_STREAK = 3` consecutive
  fully-un-ACKed messages; un-latch after `UNLATCH_STREAK = 2` direct-ACK probes. (This is
  smol's version of Babel's anti-flap hysteresis — same instinct, far cheaper.)
- **Channel selection (`ChannelPark`, #126 + the #155 pain):** a stranded leaf can't hear its
  owner's Hello, so it **blind-hops candidate channels**; `ChannelPark` parks it on whichever
  channel last drew a forward/ACK signal. The crown pins the mesh to **its own AP channel**
  (single-radio coexist), and **[#155](https://github.com/jphein/smol/issues/155)** is the
  recurring pain: *if the crown's AP is weak for the rest of the fleet, everyone's WiFi bursts
  degrade (DHCP timeouts, OTA deaths) and only an operator re-plant recovers it.* #155 explicitly
  wants **link-quality-aware channel selection**.

**smol independently reinvented pieces of Babel already:** the BATT2/GRID2 downlink re-flood is
gated by a monotonic **`dl_seq` strict-newer** rule — that is a targeted **seqno**. And
`HopLatch` is targeted **hysteresis**. smol arrived at Babel's *ideas* at the scale it needed,
without Babel's *machinery*. That is the whole story of this study.

---

## 4. C3 portability verdict — why Babel is overkill *here*

| Dimension | ESP32-C3 / smol reality | Babel fit |
|---|---|---|
| **Topology** | Star to **one** gateway; ≤2 hops; 4–30 mostly-all-hear boards | Babel computes routes to **every** prefix, any-to-any — solving a harder problem smol doesn't have. |
| **MTU** | ESP-NOW **250 B**, 10-frame RX queue, one-in-flight TX | Babel §4 requires nodes accept **≥512 B** packets. Individual TLVs are tiny (Hello ~8 B, Update ~12–26 B) so a frame *could* carry a few — but you'd run a non-RFC-compliant small-packet/fragmenting variant. Adaptable, not free. |
| **RAM** | ~74 KB free heap (build 338, live), 400 KB SRAM | **Not a blocker.** 30 nodes ≈ 30×(32 neigh + 48 route + 16 source) ≈ **~2.9 KB**. |
| **CPU** | single-core RV32IMC @ 160 MHz | Distance-vector selection is incremental + cheap. Not a blocker. |
| **Airtime / single radio** | Mesh is **deaf during WiFi bursts**; shared 10-deep RX queue; already carries HELLO(2 s)/TIME/BATT/RELAY/SNK/FAM | Babel adds **Hello(4 s)+IHU+Update** periodic control traffic competing on the same queue. Manageable at this scale but a real, permanent cost for routes that are almost always "→ the gateway, 1 hop." |
| **Forwarding model** | App-level ESP-NOW frames, addressed by the app; **no IP stack, no FIB** | Babel's entire *route-installation* half (netlink → kernel FIB) is **inapplicable**. Only the *decision* half (pick best next-hop) would port — and that half needs a destination set smol (star) doesn't have. |
| **Channel (#155)** | The actual hard problem: one radio, coexist-pinned to the crown's AP channel | Babel is **agnostic to L2/spectrum** — it can't retire #155. It can only *feed* a channel heuristic with a link metric. |

**Meshtastic corroboration:** smol's #13 is explicitly Meshtastic-lineage, and Meshtastic itself
evaluated table-based routing (Babel/AODV/OLSR) and **chose managed flood** for exactly these
reasons — tiny payloads, simple/dynamic topology, minimal overhead. smol is in good company; its
table-free flood is the *right* engineering call for its current scale and shape.

**When to revisit:** if smol ever grows a **deep, many-destination mesh** — multiple gateways,
node-to-node P2P (chat/file), or a routed mesh-RPG world where any board must reach any other over
several hops — Babel's feasibility/metric machinery would then *earn its keep*, and this study's
"admire" items become "adopt." Today they don't.

---

## 5. What to actually build (borrow the ideas)

The prize is **a link-quality metric**, not a routing protocol. smol already has the raw input
(per-peer Hello reception history for Connected/Detected) — it just never distills it into a cost.
That single addition unlocks better relay selection (#13) *and* the link-quality-aware channel
selection #155 is asking for, and it's the natural home for Althea's additive-blend trick.

Concrete proposals (filed as issues):

1. **[#164](https://github.com/jphein/smol/issues/164) — ETX-style per-peer link metric** from the
   existing Hello history (2-out-of-3 → a `0..255` cost), surfaced in DIAG/peers. **Foundational —
   everything else composes on it.**
2. **[#155](https://github.com/jphein/smol/issues/155) — link-quality-aware channel selection**
   (existing issue; [study comment](https://github.com/jphein/smol/issues/155#issuecomment-5013944557)):
   the crown scores candidate channels by *aggregate fleet ETX* (via leaf-reported link cost in
   telemetry), blending burst-health as an additive penalty (Althea's `metric_factor` pattern).
   Composes with #126 `ChannelPark`. **Depends on #164.**
3. **[#165](https://github.com/jphein/smol/issues/165) — best-relay selection for a stranded leaf**:
   when latched to multi-hop, prefer the neighbour with the best ETX to the crown instead of
   blind-flooding — a small, bounded win on the #13 throughput tail (currently ~1/N via
   channel-park). **Depends on #164.**
4. **(note, not filed — speculative) Generalize the `dl_seq` strict-newer re-flood** into a reusable
   seqno-gated flood primitive if/when smol adds more gateway→leaf downlink topics — this is Babel's
   source-table feasibility applied to flooding, at smol's scale. Left as a doc note (no issue) to
   avoid speculative backlog; promote when a second downlink type appears.

**Dependency chain:** #164 (metric) → { #155 (channel), #165 (relay) }.

---

## 6. Sources & licenses

- **RFC 8966** — *The Babel Routing Protocol*, J. Chroboczek & D. Schinazi, IETF, 2021.
- **babeld** — `github.com/jech/babeld`, **MIT**. Cited: `babeld.c` (timers), `neighbour.c`
  (ETX/cost), `route.c` (smoothed-metric hysteresis), `message.c` (wire format), `source.c`
  (feasibility/seqno).
- **althea_rs** — `github.com/althea-net/althea_rs`, **Apache-2.0**. Cited: `babel_monitor/src/`
  (`lib.rs`, `parsing.rs`) — the babeld management-socket interface + price/fee/metric-factor.
- **smol** — `docs/protocol.md`, `rust/clock/src/net/flood.rs` (#13), issues #13/#76/#126/#155.
- Clones live at `~/Projects/althea-study/` (outside the repo); full agent-free notes in
  `~/Projects/althea-study/findings/` if regenerated.

*Filed adoptable issues are linked at the top of §5 once created (see the issue tracker).*
