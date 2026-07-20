# Best-relay selection for a stranded leaf via ETX — design/spec (#165)

**Issue:** [#165](https://github.com/jphein/smol/issues/165) · **Builds on:** #164 ETX metric (`net/etx.rs`, wired), #13/#124 managed flood + UP2 (`net/flood.rs`) · **Lineage:** the #163 Althea/Babel study (`docs/superpowers/research/althea-babel-study.md`) · **Status:** design/spec only — no firmware · **Author:** nebula-babel · **Date:** 2026-07-20

> **Bottom line up front.** Today a stranded leaf (out of direct ESP-NOW range of the elected crown) reaches home by **managed flood** (#13/#124: H-limited broadcast + `(origin,msgid)` seen-set). Flood is robust but pays the "**multi-hop throughput tail**" (#13) — every neighbor rebroadcasts. #165 layers **directed routing** over the flood: each node maintains a **distance-vector gradient to the crown** (`etx_to_crown`, built from the #164 per-peer link cost), advertises it in its HELLO, and a stranded leaf **unicasts its UP2 to the single best relay** (min path-ETX) instead of broadcasting it. This is a **Babel-lite gradient-to-the-sink** — the exact thing #164's ETX was built for. Crucially it is an **optimization, not a replacement**: when no feasible gradient exists (all-peers-bad, un-converged, crown just moved) it **falls back to the #13 flood**, which always finds a path. Loops/flap are handled by Babel's **feasibility condition** + a **hop ceiling** + **hysteresis** + **HELLO-freshness retraction**.

Verification legend: 🟢 grounded in current firmware · ⚪ design judgement.

---

## 1. The problem (what #13 flood leaves on the table)

`net/flood.rs` (#13): *"smol's uplink relay is single-hop today: a leaf out of direct ESP-NOW range of the elected gateway is stranded. #13 adds Meshtastic-lineage managed flood: a hop-limit (H) + an (origin, msgid) seen-set + a forward path."* 🟢 A stranded leaf's UP2 (#124 envelope) is **broadcast** and rebroadcast by every neighbor up to `H` hops, deduped by the seen-set. That guarantees delivery but every neighbor spends a TX per fragment — the **throughput tail**. It also can't *prefer* a good link: flood treats a −45 dBm, 0-loss neighbor and a −85 dBm, 40 %-loss neighbor identically.

`net/etx.rs` (#164, **wired**, `pub mod etx` espnow-gated) already gives the missing quantity: `LinkQuality::cost()` — a per-peer ETX cost `0` (perfect) … `253` (very lossy) … `INFINITY = 255` (unheard), exposed on every roster entry as `NodeView.etx` (`n.lq.cost()`). Its own module doc names **#165 best-relay** as the consumer. What's missing is not the *link* cost — it's each neighbor's *cost onward to the crown* (the gradient).

---

## 2. The metric — a distance-vector gradient to the crown

Treat the **elected crown** (`elected_owner`, the lowest-id owner) as the single routing **sink**. Every node maintains one value:

```
etx_to_crown(self) = 0                                          if self is the crown
                   = min over live neighbors R of              otherwise
                       sat_add( lq.cost(R), R.etx_to_crown )
                   = INFINITY (255)                            if no neighbor has a finite route
```

- `lq.cost(R)` is the #164 per-peer link cost (already in `Node.lq`). `R.etx_to_crown` is R's advertised gradient (§3). `sat_add` saturates at `INFINITY`.
- A node that **hears the crown's HELLO directly** has a candidate `lq.cost(crown) + 0` — the gradient bootstraps from the crown outward, one HELLO interval per hop.
- **Keyed to the crown's identity.** The gradient is meaningless across a crown change, so it is stored + advertised as the pair `(crown_id, etx_to_crown)`; a consumer ignores any advertisement whose `crown_id ≠` its current `elected_owner` (§6 crown-change).

This is `O(neighbors)` integer work per HELLO tick — the same cadence #164 already ticks `LinkQuality`. Pure, host-testable.

---

## 3. Wire delta — advertise the gradient in HELLO

HELLO is today `b"SMOLv1 HELLO " + NNN` (3-digit id) — the only frame **every** node broadcasts periodically (owner every ~2 s; leaves on their cadence). That makes it the natural DV carrier. Append two fields:

```
SMOLv1 HELLO NNN CCC E
  NNN = sender id (unchanged)
  CCC = crown_id the sender is routing toward (3-digit; 255 = "no crown / none")
  E   = sender's etx_to_crown (u8: 0..=254, 255 = INFINITY/no-route)
```

- **Back-compat.** A pre-#165 parser reads `NNN` and ignores the tail (the HELLO parse strips the prefix + reads the id); a #165 node that hears a tail-less HELLO treats that neighbor's gradient as **unknown** — it can still route *directly* to the crown if it hears the crown's own HELLO, and otherwise won't select that neighbor as a relay. So a mixed fleet degrades to "route through #165-capable neighbors, flood through the rest" — never worse than today.
- **Cost:** ~8 ASCII bytes on HELLO (well within MTU). A binary `CCC`+`E` (2 bytes) is the compact alternative; ASCII keeps HELLO sniffer-greppable, consistent with the frame zoo. ⚪ pick at implementation.
- **No new frame** — this is the #164 design's stated "add the advertisement alongside the first consumer." (Two-way txcost — babeld's IHU echo — remains the separate #164 follow-up; §8.)

---

## 4. Best-relay selection (the stranded leaf)

Slot into the existing escalation. `HopLatch` (`flood.rs`) already decides *stranded vs not*; #165 changes **what a latched leaf does** — route before flood:

```
on uplink while HopLatch.latched():           // stranded: no fresh direct crown link
  candidates = live neighbors R where
      R.crown_id == my elected_owner           // same-sink advertisements only
      ∧ R.etx_to_crown != INFINITY             // R has a route
      ∧ R.etx_to_crown < my etx_to_crown       // FEASIBILITY (Babel): strictly downhill → loop-free
      ∧ R.lq.cost() fresh (HELLO not stale)
  if candidates non-empty:
      relay = argmin_R sat_add(R.lq.cost(), R.etx_to_crown)   // min path-ETX
      apply hysteresis vs the current relay (§6)
      UNICAST UP2 to `relay` (directed; H from HopLatch)      // NOT a broadcast flood
  else:
      FALL BACK to #13 broadcast flood (H=2)                   // all-peers-bad / un-converged (§7)
```

The **feasibility condition** (only route to a strictly-lower-cost neighbor) is what makes a distance-vector safe on a mesh without full path state — you can never form a loop by sending "uphill." Delivery truth is the existing **RELAYACK2**: a unicast UP2 that isn't acked → drop `relay`, try next-best, then flood (§7).

---

## 5. The relay's forward decision (route toward the crown)

A node receiving a UP2 it must forward already runs `flood::forward_decision(is_gateway, hop, already_seen)` + the `SeenSet` dedup. #165 refines the *forwarded* case: instead of **rebroadcast**, the relay **forwards toward the crown via its own best relay** (the §4 pick from *its* vantage) — or delivers directly if `is_gateway`/it hears the crown. `H` decrements per hop (v1 ceiling 2; 3-hop is a follow-up, already noted in flood.rs). The `SeenSet` still guards against dup/loop as defence-in-depth beneath the feasibility condition. If the relay itself has no feasible next hop, it **reverts that fragment to broadcast flood** — the fallback is available at every hop, not just the origin.

---

## 6. Stability — loops, staleness, oscillation, crown moves

- **Loops → feasibility + hop ceiling.** The strictly-downhill feasibility rule (§4) prevents routing loops structurally; `H` bounds depth as a hard backstop; the `SeenSet` catches any residual dup. (Classic DV count-to-infinity is bounded by `H` + INFINITY=255 with a tiny diameter — a 4–30-node fleet is ~2–3 hops.)
- **ETX staleness → freshness + retraction.** A neighbor's advertised `etx_to_crown` is only trusted while its HELLOs are fresh (the #164 `LinkQuality` decays toward `INFINITY` as HELLOs stop, and the roster's HELLO-staleness already expires peers). A node that **loses** its route advertises `E = INFINITY` (poisoned reverse) so downstream leaves retract within ~1 HELLO interval rather than routing into a black hole.
- **Oscillation → hysteresis.** Two relays with near-equal path-ETX would flap every interval (UP2 reordering + churn). Switch the current relay only if a challenger is better by a **margin ΔETX** (e.g. ≥ ~1/8 of current cost, tunable) **and** has held that lead for **K intervals** (minimum dwell). Prefer the incumbent on ties. This is babeld's metric-hysteresis lineage, integer-only.
- **Crown moves (election) → keyed reset.** The gradient is stored/advertised as `(crown_id, etx_to_crown)`. On an `elected_owner` change, all gradients keyed to the *old* crown are dropped to `INFINITY`; the new crown advertises `0` and the gradient re-converges outward over the next few HELLO intervals. During re-convergence a stranded leaf has no feasible relay → **flood fallback** carries it (no delivery gap).

---

## 7. Failure modes (explicit)

| Mode | Handling |
|---|---|
| **All-peers-bad** (no neighbor has a finite same-crown gradient) | **Fall back to #13 broadcast flood** (H=2). Routing is the optimization; flood is the always-available safety net. Never worse than today. |
| **ETX staleness** (a relay's advertised route is stale / it lost uplink) | HELLO-freshness expiry (#164 decay) + `E=INFINITY` poisoned-reverse retraction + the feasibility check re-evaluated every interval. A unicast that goes unacked (no RELAYACK2) drops the relay immediately. |
| **Oscillation / flap** between near-equal relays | Hysteresis: margin ΔETX + min dwell K + incumbent-wins-ties (§6). |
| **Crown moved mid-transit** | Gradient keyed to `crown_id`; old-crown routes reset to INFINITY; flood fallback bridges re-convergence (§6). |
| **Asymmetric link** (leaf hears R well, R can't hear the leaf's unicast) | v1 ETX is one-way (rxcost) so this is possible; the **unacked UP2 (no RELAYACK2) → next-best → flood** ladder recovers it. The clean fix is two-way txcost ETX (the #164 IHU-echo follow-up, §8) + #202 TX-status truth. |
| **Mixed fleet** (some nodes pre-#165, no gradient tail) | Tail-less HELLO → gradient unknown → those neighbors are used only for direct-crown / flood, #165-capable neighbors route. Degrades gracefully. |

---

## 8. Scope / non-goals
- **Two-way ETX (txcost).** v1 routes on one-way rxcost (`LinkQuality`); babeld's IHU-echoed txcost (defeats asymmetric links) is the #164-noted follow-up — needs the wire echo, deliberately out of #165 v1 (the unacked-UP2 ladder covers the common case).
- **3-hop (H>2).** v1 keeps flood.rs's H=2 ceiling; deeper diameters are a follow-up (a 4–30-node fleet is shallow).
- **Multiple sinks / anycast crowns.** Single-sink gradient (the one elected crown); not general any-to-any routing.
- **No firmware in this issue** — this is the spec; the impl is a pure `best_relay()` + gradient-update core (the flood/etx pattern) plus the HELLO field wiring.

---

## 9. Grounding in current firmware
- `net/etx.rs` — `LinkQuality::cost()` (per-peer rxcost; `INFINITY=255`), wired, exposed as `NodeView.etx`. The routing metric input.
- `net/flood.rs` — `SeenSet` (`seen_or_insert`, loop/dup guard), `forward_decision(is_gateway, hop, already_seen) -> ForwardAction`, `HopLatch` (stranded escalation + `should_probe`/`origin_hop`). #165 changes the *latched* action from broadcast to directed-unicast + adds the gradient; the seen-set/H stay as backstops.
- `net/mode.rs` — `HELLO` frame (`b"SMOLv1 HELLO " + NNN`, the gradient carrier), the `UP2` (#124) envelope + `RELAYACK2` (the unicast delivery ack), `elected_owner` (the sink id), `Roster`/`Node.lq` (per-peer cost) + `NodeView`.

## 10. Implementation order + host tests
1. **Pure core** `net/relay.rs` (or fold into `flood.rs`): `etx_to_crown` gradient update + `fn best_relay(neighbors: &[(id, link_cost, crown_id, their_etx)], my_crown, my_etx) -> Option<u8>` with the feasibility + hysteresis rules. Inert until wired (the #164/#182 pattern).
2. HELLO `(crown_id, etx_to_crown)` encode/parse (back-compat tail).
3. Wire: `HopLatch` latched → `best_relay` → unicast UP2, else flood; relay-side forward-toward-crown.
4. **Host tests** `experiments/165_bestrelay_verify` (mirrors `etx_verify`): gradient convergence (crown outward), feasibility loop-freedom (never routes uphill), all-peers-bad → flood-fallback signal, staleness → INFINITY retraction, hysteresis (no flap under near-equal costs), crown-change reset. `cargo run` **and** `cargo test` (the false-green lesson from #185).

**Receiver/flood seen-set/H ceiling/election: unchanged. #165 is additive — routed when it helps, flood when it must.**
