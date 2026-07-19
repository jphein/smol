# RIOT OS ESP-NOW netdev + 6LoWPAN — design notes for smol

**Issue:** [#200](https://github.com/jphein/smol/issues/200) · **Lineage:** [#163 Babel](althea-babel-study.md) · [#181 ledger](mesh-ledger-study.md) · [#189 coverage](inspirations-coverage.md) · **Status:** read-only study, no firmware · **Author:** nebula-babel · **Date:** 2026-07-19

> **Bottom line up front.** RIOT is smol's road-not-taken made concrete: it wraps the *exact*
> radio primitive smol uses (ESP-NOW, 250 B) as a `netdev`, then runs a **full IPv6 /
> 6LoWPAN / RPL** stack over it. The comparison is mostly *validation-by-contrast* — RIOT
> confirms several smol calls were right, and on two points **smol is actually ahead**. There
> is exactly **one clean ADOPT**.
>
> **The one ADOPT — unicast delivery truth.** RIOT sends ESP-NOW unicast and consumes the
> hardware **TX-status callback** (`esp_now_register_send_cb`) — per-frame "did it get ACKed?"
> ground truth. smol registers **no send callback**: it fires `esp_now.send` and *infers*
> delivery (the app-layer RELAYACK bitmap) or doesn't check at all (OTAN, #26 Cast last-hop
> unicast). Adopting the send-status callback on smol's unicast paths would turn today's
> *inferred* delivery into *observed* delivery — directly relevant to the #26 last-hop-unicast
> blackout scar tissue. → filed.
>
> **The verify-don't-assume catch (Q2).** The issue asks "they solved ESP-NOW's ~20-peer
> limit — how?" **They didn't.** RIOT's peer table is plain `esp_now_add_peer` + an
> is-exist check, with **no eviction, no del_peer, no cap handling** — it hits the same ~20
> wall smol did pre-#28. **smol's #28 LRU eviction is ahead of RIOT here.** (Same pattern as
> the #163 ed25519 "already in-tree" half-truth — the premise didn't survive the code.)
>
> **The big validation (Q6/Q3).** RIOT *hard-codes* the constraint smol learned the hard way:
> when WiFi-STA is used, ESP-NOW's channel **follows the STA channel** (you can't set it
> independently) — smol's exact coexist invariant (#40, and the #155 pain root). And RIOT
> goes further: `#error If module esp_wifi is used, module esp_now has to be used in unicast
> mode` — **RIOT structurally forbids the broadcast-flood + WiFi-STA combination smol is
> built on.** smol's flood-first-broadcast mesh *with* a WiFi gateway is off RIOT's map;
> smol makes it work via the burst time-share / #40 co-channel coexist. That's genuine smol
> novelty, not an oversight.

---

## 0. Provenance & method (verify, don't assume)

Read directly from `RIOT-OS/RIOT` @ HEAD (**LGPL-2.1**, kept OUT of the repo in
`~/Projects/riot-study/`, never committed), cross-checked against smol source:

| RIOT file | What it gave |
|---|---|
| `cpu/esp_common/esp-now/esp_now_netdev.h` | the 1-byte L2 header, MTU (249), device descriptor, single-deep RX buffer |
| `cpu/esp_common/esp-now/esp_now_netdev.c` (765 LoC) | peer scan/add/RX-learn, `_send` unicast/broadcast, the `#error` coexist constraint, STA-channel-follow, send-status callback |
| `cpu/esp_common/esp-now/esp_now_gnrc.c` | the SIXLO flag set/read + handoff to the 6LoWPAN thread |
| `cpu/esp_common/esp-now/esp_now_params.h` | defaults: **ch 6**, 10 s scan period, key=NULL (unauth), SoftAP SSID prefix |
| smol tree | `net/wire.rs` (UP2/RELAY framing), `net/mode.rs` (#28 roster eviction, `send_to`, no send-cb), the #26/07-14 unicast scar tissue |

Every claim about what RIOT "solved" was checked against the code, not the issue's framing
(Q2 is why — see below).

---

## 1. RIOT's ESP-NOW model in one page

RIOT exposes ESP-NOW as a standard `netdev` and layers its normal stack on top:

```
   GNRC UDP / IPv6 / RPL routing        ← full IP stack (RFC 6550 RPL, RFC 6282 IPHC)
   GNRC 6LoWPAN  (frag/reassembly, IPHC header compression)   ← ALL fragmentation here
   esp_now_gnrc  (1-byte flag: SIXLO bit; prepend dst MAC)     ← the glue
   esp_now_netdev (esp_now_send/recv, peer table, TX-status)   ← the L2 driver
   ESP-NOW radio primitive (≤250 B frames)                     ← same as smol
```

Defining choices (all verified):
- **L2 header = 1 byte.** `esp_now_pkt_hdr_t { uint8_t flags; }`, one flag bit
  (`ESP_NOW_PKT_HDR_FLAG_SIXLO`). MTU = 250 − 1 = **249**. *No sequence, no fragment field at
  L2* — fragmentation is entirely 6LoWPAN's job.
- **Peers via WiFi AP-scan.** Each node runs a **SoftAP** (STA+AP mode, SSID prefix +
  shared WPA2 pass); nodes **scan for those APs every 10 s** and `esp_now_add_peer` each one.
  On RX, an unknown sender MAC is auto-added as a peer too.
- **Unicast-first, with hardware ACK.** `_send` resolves a 6-byte dst MAC (all-`0xff` ⇒
  broadcast), calls `esp_now_send`, then **busy-waits on the send-status callback**
  (`_esp_now_sending`) — synchronous per-frame delivery status. Broadcast mode exists **only
  when WiFi is absent** (shared-MAC).
- **Coexist = STA + SoftAP, ESP-NOW rides the STA channel.** When `MODULE_ESP_WIFI` (real
  STA connectivity) is on, `esp_now_set_channel` returns `ESP_ERR_NOT_SUPPORTED` — the channel
  is owned by the WiFi driver and ESP-NOW follows it. No burst/time-share.
- **Single-deep RX buffer** + a recv-cb reentrancy guard (runs in the WiFi thread); drops on
  overflow — the same shallow-RX starvation surface smol has.
- **An L2 address filter** (`l2filter_pass`) — an allow/deny list ≈ smol's `DEAF_MACS`.

---

## 2. Q1 — L2 framing / fragmentation (priority)

**RIOT:** the netdev does **no fragmentation** — `_send` rejects a payload > 250 with
`-EBADMSG`. All fragmentation/reassembly is done by the **6LoWPAN layer** (RFC 4944 frag
headers, 4–5 B/fragment) above the 1-byte flag. Reassembly buffers, IPHC decompression, and
the datagram tag/offset bookkeeping all live in the mature `gnrc_sixlowpan` stack.

**smol:** no 6LoWPAN, so smol **reinvented fragmentation at the app layer** — `RELAY` splits
telemetry into `RELAY_CHUNK` (64 B) fragments with `(msgid, frag, count)`, the gateway
reassembles by `(src_mac, msgid)`, and a `RELAYACK` bitmap drives selective retransmit; #124
then consolidated the multi-hop family into the single **UP2 envelope** (23 B overhead) with
its own field-boundary clamp.

**Verdict: ADMIRE the layering; the actionable steal is small — and it *validates* the UP2
consolidation.** RIOT's "1-byte type flag + one reassembly layer" is the clean version of what
smol is converging toward: UP2 already replaced the per-family RELAY2/RELAYACK2 fat headers
with one envelope. The RIOT lesson for **#68-class continuation** (multi-hop observability) is
*don't add more fat per-family headers* — keep consolidating onto one tiny-tagged envelope +
one reassembly path, rather than a 6LoWPAN import (which needs the whole IPv6 stack smol
doesn't want, §5). No new issue; a design note on the #124/#68 lineage.

---

## 3. Q2 — neighbor management (the verify-don't-assume catch)

**RIOT's peer lifecycle:** discover by 10 s WiFi AP-scan → `esp_now_add_peer` each SoftAP;
auto-add unknown senders on RX; peers keyed by 6-byte MAC. **That's it.** Grep confirms the
*only* peer-table calls are `esp_now_is_peer_exist` + `esp_now_add_peer` + a debug
`esp_now_get_peer_num` — **no `esp_now_del_peer`, no eviction, no cap logic anywhere.**

**So RIOT does *not* solve ESP-NOW's ~20-peer limit** — the issue's premise is false. Past ~20
scanned/heard nodes, `esp_now_add_peer` silently fails and the 21st peer is unreachable by
unicast — exactly smol's pre-#28 ceiling. **smol's #28 `ensure_peer` two-tier LRU eviction
(ghost-first, then value-weighted by id-known → connected → RSSI → age, protecting broadcast +
active-OTA target) is strictly ahead of RIOT here.**

Two more contrasts, both favoring smol for its use case:
- **Discovery cost.** RIOT's periodic WiFi AP-scan is airtime- and power-expensive (an active
  scan every 10 s); smol learns peers **passively** from HELLOs it already broadcasts —
  decisively better for battery leaves.
- **Parallel worth noting:** RIOT's `l2filter` allow/deny list is the same idea as smol's
  `DEAF_MACS` rig hook (#13 Stage C).

**Verdict: SKIP** (smol already better). The finding is the correction itself: cite #28 as
*ahead of the reference implementation*.

---

## 4. Q3 — broadcast vs unicast (priority) — and the one ADOPT

**RIOT is unicast-first with hardware delivery status.** In unicast mode it sends to a resolved
peer MAC and **consumes the ESP-NOW send-status callback** (`esp_now_register_send_cb` →
`_esp_now_sending` busy-wait) — per-frame "was it ACKed?" truth. Broadcast mode exists only
when WiFi is absent (all nodes share one MAC). And the hard line:

```c
#if MODULE_ESP_WIFI && !ESP_NOW_UNICAST
#error If module esp_wifi is used, module esp_now has to be used in unicast mode
#endif
```

**RIOT forbids broadcast ESP-NOW while WiFi-STA is up.** smol's entire architecture — a
broadcast **managed-flood** mesh *with* a WiFi gateway — is the combination RIOT compiles out.
smol makes it work by not doing both at once (the burst time-share: mesh-deaf during the flush)
or by the #40 co-channel coexist bet. So smol's flood-first choice (which the **07-14 unicast
pathology** reinforced — [memory: `smol-ap-path-unicast-pathology`]) is the *opposite* of
RIOT's unicast-first, and both are internally consistent:

| | RIOT (unicast-first) | smol (flood/broadcast-first) |
|---|---|---|
| Per-frame delivery status | **yes** (hardware ACK + send-cb) | no (inferred via app-layer RELAYACK bitmap) |
| Peer-table dependency | every dst must be a peer (hits ~20 cap) | broadcast needs no peer; unicast paths (RELAYACK/OTAN) do |
| WiFi-STA + mesh together | allowed **only** unicast | broadcast flood + STA (burst/#40) — RIOT forbids this |
| Reliability model | link-layer ACK per frame | redundancy (flood) + app-layer selective retransmit |

**On the 07-14 pathology specifically:** smol's disease was **WiFi-STA unicast through the AP
fabric** (board re-ARPs its gateway, unicast replies lost across a 9-AP roaming fabric) — a
*different layer* than RIOT's peer-to-peer ESP-NOW unicast (no AP in the path). RIOT's model
wouldn't hit smol's AP-fabric disease, but RIOT's ESP-NOW unicast still hard-depends on the
peer being scanned **and on-channel** — and RIOT's own "ESP-NOW follows the STA channel" +
single-deep RX buffer are the reliability-shaped design decisions it *did* document.

**ADOPT (the one clean steal): consume the ESP-NOW unicast TX-status callback on smol's
unicast paths.** smol registers **no** send callback (verified: `net/mode.rs` uses
`esp_now.send`/`send_to` and checks only the immediate queued-OK `Result`, never the async
delivery status). Its unicast paths — `RELAYACK`, `OTAN`, and the #26 Cast last-hop unicast —
would gain *observed* delivery instead of *inferred*/none. This is small, targeted, and points
straight at the #26 last-hop-unicast-blackout scar tissue: a send-status callback would have
made that blackout self-evident instead of a forensics hunt. **→ filed** (verify esp-wifi-rs
exposes the status; if it does, wire it into the unicast TX paths + a DIAG counter).

**Verdict: ADMIRE the unicast+ACK model as the road-not-taken; smol's flood-first is validated
as correct for a broadcast game mesh + battery leaves + WiFi coexist; ADOPT the send-status
callback for delivery truth on the unicast paths smol *does* have.**

---

## 5. Q4 — 6LoWPAN-over-ESP-NOW cost — ADMIRE

6LoWPAN's job is squeezing IPv6 onto tiny frames:
- **IPHC (RFC 6282):** compresses the 40 B IPv6 header to as little as **2–3 B** by *eliding*
  addresses that can be **derived from the L2 MAC** (link-local IID = MAC), plus a compressed
  next-header. Carry nothing you can reconstruct.
- **Frag (RFC 4944):** 4–5 B per fragment (datagram size + tag + offset).
- **RAM cost:** reassembly buffers, an IPv6 neighbor cache, and (with RPL) a routing table —
  the standard GNRC footprint, comfortably more than smol's ~72 KB-heap budget wants to spend
  on transport.

**Verdict: ADMIRE** (as expected — full IPv6/6LoWPAN is Babel-wholesale-class overkill for a
1-byte-node-id mesh). **But the IPHC address-elision *principle* validates smol's header
minimalism** — it's the same move I recommended in the [ledger study](mesh-ledger-study.md)
(omit the author field; the node-id/MAC already identifies the sender) and that UP2 already
uses (origin id, not a full address). smol's 1-byte node-id **is** its IPHC — carrying nothing
the receiver can derive. The compression discipline informs any future header work; the stack
does not port.

---

## 6. Q5 — RPL routing — ADMIRE, and a *better-shaped* admire than Babel

RPL (RFC 6550) builds a **DODAG** — a Destination-Oriented Directed Acyclic Graph, i.e. a tree
**rooted at a border router** — with rank computation, DIO/DAO/DIS control messages, and
trickle-timer-paced upkeep. It optimizes **many-to-one** (nodes → root) and **one-to-many**
(root → nodes).

**This is a meaningfully better fit for smol's topology than Babel was.** [#163](althea-babel-study.md)
ruled Babel a *category error* because Babel is **any-to-any** and smol is a **single-sink
star**. RPL is **tree-to-a-root** — which *is* smol's shape: leaves → crown (uplink) + crown →
leaves (downlink). smol's #76-elected **crown is already a DODAG root**, and the ledger study's
crown-ordered tree-head ([#183](mesh-ledger-study.md)) is a mini-DODAG-root idea.

**But it still doesn't earn its keep at smol's scale.** RPL's rank/DODAG/DAO machinery + control
traffic is built for *deep* multi-hop (many hops, tens–hundreds of nodes); smol is ≤2 hops,
4–30 co-located nodes, where managed flood + the seen-set is loop-free and cheaper. **Verdict:
ADMIRE now, and this refines the #163 verdict:** if smol's topology ever *deepens* (3–5 hops,
50+ nodes), **RPL — not Babel — is the right upgrade target**, because RPL's tree-rooted-at-a-
border-router model already matches smol's crown-rooted star. Until then, flood wins.

---

## 7. Q6 — coexist — strong VALIDATION of #40

RIOT runs **STA + SoftAP concurrently** and, when real STA connectivity is used, makes
**ESP-NOW follow the STA's channel** (setting the ESP-NOW channel independently returns
`ESP_ERR_NOT_SUPPORTED`). No burst, no time-share — continuous co-channel coexist.

**That is precisely smol's #40 coexist end-state** (mesh channel = the crown's AP channel,
co-channel, burst retired). A mature production OS arriving independently at the same design is
strong validation that #40's co-channel bet is sound. Two honest deltas:
- RIOT **sidesteps** the hard part by mandating unicast — it never has to make *broadcast*
  survive coexist. smol runs **broadcast flood** co-channel with STA, which is harder, and
  #40 proved it works. That's smol past the edge of RIOT's map.
- RIOT accepts "ESP-NOW follows the STA channel" as an immutable constraint. smol's **#155**
  tries to make the crown's channel *choice* smarter (link-quality-aware) — smol *extending*
  beyond RIOT's fixed acceptance. RIOT confirms the constraint is real; #155 is the value-add
  on top of it.

**Verdict: VALIDATES #40** (nothing to adopt — smol already there); note RIOT's constraint as
independent confirmation of the coexist invariant that #155 addresses.

---

## 8. Convergent design / parallels

smol and RIOT independently landed on several of the same primitives — reassuring for both:

| Concept | RIOT | smol |
|---|---|---|
| L2 allow/deny filter | `l2filter_pass` | `DEAF_MACS` (#13 Stage C) |
| Default mesh channel | ch 6 | ch 6 |
| Learn peer on RX | auto-add unknown sender | roster `heard()` from HELLO |
| Address elision | IPHC (IID from MAC) | 1-byte node-id (carry nothing derivable) |
| Crown / root | DODAG root (border router) | #76 elected crown / ledger tree-head (#183) |
| Unauthenticated default | key = NULL | `lmk:None, encrypt:false` (→ the #190 gap) |
| Mesh channel = STA channel | enforced (`ESP_ERR_NOT_SUPPORTED`) | #40 coexist invariant / #155 pain |

Where they diverge, it's smol's use case (broadcast game mesh, battery leaves, WiFi gateway)
pushing past RIOT's unicast-first assumptions — and on peer eviction, smol is simply ahead.

---

## 9. Ranked ADOPT / ADMIRE / SKIP

**ADOPT**
1. **ESP-NOW unicast TX-status callback** → delivery truth on smol's unicast paths
   (RELAYACK / OTAN / #26 Cast). smol registers no send-cb today; RIOT's model turns inferred
   delivery into observed. Ties to the #26 last-hop-unicast scar tissue. → **issue**.

**ADMIRE** (correct for RIOT, wrong to port — documented)
2. **6LoWPAN IPHC address-elision** — validates smol's node-id/omit-author header minimalism;
   the stack itself is overkill (full IPv6). Informs header work, doesn't port.
3. **1-byte-flag + single-reassembly-layer discipline** — validates the UP2 consolidation;
   the #68/#124 lineage should keep consolidating onto one tiny-tagged envelope, not import
   6LoWPAN. Design note, not a new issue.
4. **RPL DODAG-rooted-at-a-border-router** — the *right-shaped* multi-hop upgrade target
   (unlike Babel's any-to-any) **if** smol's topology ever deepens past ~2 hops / ~30 nodes.
   Refines #163. Admire until the scale trigger.

**SKIP** (smol already better, or wrong tool)
5. **RIOT's peer-table** — no eviction, hits the ~20 cap; **smol #28 is ahead**. The Q2
   premise was false.
6. **AP-scan peer discovery** — airtime/power-expensive vs smol's passive HELLO-learn; bad for
   battery leaves.
7. **Full 6LoWPAN / IPv6 / RPL stack** — Babel-wholesale-class overkill for a 1-byte-node-id,
   ≤2-hop, single-sink mesh.

**VALIDATES** (confirms smol's calls, no action)
- **#40 co-channel coexist** — RIOT runs exactly this (STA+AP, ESP-NOW follows STA channel).
- **Flood-first broadcast** — RIOT *forbids* broadcast+WiFi; smol's harder path is genuine novelty.
- **Unauth default** — RIOT ships key=NULL too (the #190 mesh-auth gap is an industry-wide default, not a smol oversight).

---

## 10. Follow-up issues
- **R1 — [#202](https://github.com/jphein/smol/issues/202)** — consume the ESP-NOW unicast TX-status callback for delivery truth on RELAYACK/OTAN/#26 Cast.

(ADMIRE/SKIP items 2–7 are deliberately not filed — items 3 and 4 are captured as design notes
on the #68/#124 and #164/#165 lineages respectively.)

---

## 11. Executive summary
RIOT OS proves the road smol didn't take — ESP-NOW as a `netdev` under a full IPv6/6LoWPAN/RPL
stack — and the honest verdict is that smol chose well: 6LoWPAN/IPv6/RPL are the same
Linux-router-class overkill Babel was, RIOT's peer table is actually *behind* smol's #28
eviction (the "they solved the ~20-peer limit" premise didn't survive the code), and RIOT
*structurally forbids* the broadcast-flood-plus-WiFi architecture that is smol's whole novelty
— which it makes work via the burst/#40 coexist. Two things transfer: RIOT independently
enforcing "ESP-NOW follows the STA channel" is strong **validation of #40's co-channel bet**
(and confirms the #155 constraint is real), and RIOT's IPHC address-elision **validates smol's
1-byte-node-id header minimalism**. The single clean **ADOPT** is the ESP-NOW **unicast
TX-status callback**: RIOT consumes per-frame hardware delivery status, smol infers it — wiring
it into smol's unicast paths (RELAYACK/OTAN/#26 Cast) converts inferred delivery into observed,
aimed squarely at the last-hop-unicast blackout scar tissue. And if smol's mesh ever deepens
past two hops, **RPL — not Babel — is the correctly-shaped routing target**, because its
tree-rooted-at-a-border-router model already matches smol's crown-rooted star.
