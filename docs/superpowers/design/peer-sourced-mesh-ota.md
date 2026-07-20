# Peer-sourced leaf-mesh-OTA — implementation spec (#237, extends #40)

**Issue:** [#237](https://github.com/jphein/smol/issues/237) · **Extends:** #40 leaf-mesh-OTA · **Depends on:** #217 rung-2 (land first — §6) · **Motivated by:** [#54 mesh-OTA study](../research/reliable-mesh-ota-architectures.md), [#53 coexist physics](../research/coexist-disease-esp-radio-018-study.md), [#204 pcap](https://github.com/jphein/smol/issues/204#issuecomment-5018759525) · **Status:** design/spec only — no firmware · **Author:** nebula-babel · **Date:** 2026-07-19

> **Bottom line up front.** Today every leaf-mesh-OTA image originates from the crown's **WiFi bulk fetch** — the one operation the coexist disease makes hostile (bulk-RX-deaf; the crown blackholes its own download, [#204](https://github.com/jphein/smol/issues/204#issuecomment-5018759525)), and the [#53 study](../research/coexist-disease-esp-radio-018-study.md) proves that stays true on the esp-radio-0.18 stack. Peer-sourcing removes that fetch for every node after the first: once **one** node holds a verified build, the crown **delegates serving** to it, and it relays the **byte-identical signed image** to the next leaf over ESP-NOW using the **unchanged #40 receiver path**. The crown stays the sole canary arbiter (one target at a time, structural). No new trust: the holder is untrusted; the receiver still verifies the ed25519 sig **before any flash write**, so a bad holder can only DoS, never brick. Serving is coexist-safe by construction (ESP-NOW TX-heavy, no WiFi fetch → the disease can't fire).

> ### ⏩ Shipping posture — this is critical path (2026-07-20)
> JP chose "wait for #237 peer-sourcing" over plugging boards / RF changes: the fleet stays mixed
> (343/342) until this ships, so **implementability-now beats completeness.** The receiver is
> untouched, so the smallest *source-side* handoff that safely ships wins. This spec is therefore
> split into a **v1-minimal slice** ([§8](#8-first-implementation-slice-v1-minimal--smallest-safe-pr-slice-1--65)) — the smallest PR that lets a
> holder serve the fleet — and **v2 deferrals** (flagged inline as `▷ v2` and listed in §8.3).
> The safety floor is constant across v1/v2: **fallback to the #40 gateway fetch means worst-case
> v1 is exactly today's behavior.** See [§9](#9-bootstrap-sequencing--the-one-more-roll-irony) for the one-more-gateway-roll bootstrap reality.

---

## 0. Scope

**In:** the source-side handoff — how the crown learns a peer holds a verified build, how it delegates serving one-target-at-a-time, how the holder re-serves, and the failure/observability surface.

**Out (unchanged from #40, spec'd here only as invariants that must be preserved):** the receiver reassembly/verify/flash pipeline (`OtaFrame` parse → verify-sig-over-M → HOLE-3 bounds → `LeafImageWriter` → readback-sha → `activate`), the `OTAM/OTAD/OTAN` chunk-transport wire, the staged-manifest MQTT flow (`OTA|build|size|sha256hex|sighex|url`), and the A/B slot / freshness-floor / self-test engine.

**Explicit non-goals:** multi-hop epidemic spread (an updated node auto-serving many peers unbidden). The `OtaRxView.hops` field is already reserved for a future true multi-hop, but this spec keeps serving **crown-arbitrated and single-target** — epidemic spread is incompatible with structural canary and is deferred.

---

## 1. Source-selection — how the crown learns a peer holds verified build N

> **▶ Shipping design is [§8](#8-first-implementation-slice-v1-minimal--smallest-safe-pr-slice-1--65) (slice-1).** This section describes the fuller **v2/general** source model — a `HOLD`-based inventory the crown queries to auto-select and load-spread a source. **Slice-1 uses a strictly simpler subset and supersedes the §1.x parts flagged below:** the crown carries `(M, sig)` in the `ODEL` (holder persists **nothing**) and selects the source by **baton** (the node it just finished serving), *not* by the `HOLD` inventory. Read §1 for the v2 model; read §8 for what ships first. **Where they differ, §8 wins.**

### 1.1 What "holds" means (precise)
A node is a **valid source for build N** iff it can reproduce the exact signed image on demand:
- it has build N in a **readable slot** (its **active** slot if it is running N, or its **inactive** slot if it fetched/received N but hasn't activated), **and**
- it has the signed manifest tuple `M = "build|size|sha256hex"` (≤86 B) + the 64-byte ed25519 `sig` — **in slice-1 the crown supplies these in the `ODEL` (§8.1), so the holder retains nothing**; the v2 self-advertise path (§1.2) instead has the holder *retain* `(M, sig)` so it can offer without a live crown handoff, **and**
- a **self-readback-sha of `slot[..size]` matches** the manifest sha (cheap pre-flight — in slice-1 run at serve time when the `ODEL` arrives; a source must never serve a copy it can't itself verify).

> **⚠ Superseded for slice-1 by §8:** the "holder **retains `(M, sig)`** in a `.bss` record" model does **not** apply to slice-1 — a holder only becomes a source *after* it activates and **reboots** into build N, which clears `.bss`. Slice-1 therefore has the **crown** carry `(M, sig)` in the `ODEL` (it staged the build, so it has them). Holder-side retention is a **v2** concern and would need **durable NVS** (not `.bss`), only for crownless self-serve.

### 1.2 The HOLDS signal `▷ v2`
> **Slice-1 skips the HOLD frame entirely (§8).** Its source-selection is the **baton** — the crown delegates to the node it *just finished serving* (one variable, `last_confirmed_holder`), seeding the first target the #40 way. No HOLD inventory, **no DIAG scan** (an earlier draft used DIAG-running-build; §8 supersedes it with the simpler baton). A **serve-time readback-sha self-check** still gates the actual serve (on mismatch → `ODON=self-slot-verify-failed` → crown falls back to gateway fetch, §5.2). `HOLD` is the **v2** upgrade: an explicit "can-serve" broadcast inventory (retains-`(M,sig)` + on-ch6 + pre-verified) for **auto-selection + load-spread** across many holders once the fleet is large. The rest of §1.2 specs that v2 frame.

Add a new broadcast self-report — **`SMOLv1 HOLD `** (holder→broadcast, ~10 s cadence, piggybackable on the existing DIAG tick):

```
tag[12] "SMOLv1 HOLD " · id[3] · build[u32 LE] · flags[1]
flags bit0 = slot-readback-sha VERIFIED   bit1 = on ch6 (serve-ready)   bits2-7 reserved
```

The crown accumulates a **source table** `{node_id → (build, serve_ready)}` from HOLD frames (same drain path as DIAG). A node is a **candidate** for delegating build N iff `build == N ∧ flags.verified ∧ flags.on_ch6`. This reuses the roster/DIAG plumbing the crown already runs; no new subscription, no MQTT dependency (leaves have none).

> **Why not derive it from DIAG's running-build alone?** DIAG tells the crown a node is *running* N, but not that it *retains (M,sig)* or that its slot readback still verifies. HOLD is the explicit "I can serve N right now" contract; deriving it implicitly would let the crown delegate to a node that has since dropped its manifest or drifted off ch6.

### 1.3 Source preference order (crown-side) `▷ v2`
> **Slice-1 uses the baton (§8), not this order.** The list below is the **v2** preference, once a `HOLD` inventory exists to pick among.

When the crown (v2) needs to update leaf T to build N and T is not the seed, it picks a source in this order:
1. a **candidate holder** on ch6 with the fewest in-flight obligations (load-spread across holders as the fleet fills in),
2. else **itself** via the #40 gateway path (fetch-into-inactive → serve) — the **seed** fetch, or the fallback when no holder qualifies.

The very first node of any new build has no holder → the crown seeds it by fetch (§6 explains why #217 must harden exactly this fetch). *(Slice-1's baton reduces this to: source = `last_confirmed_holder`, else self-seed.)*

---

## 2. Session wire changes vs #40 — source-side handoff only

**The `OTAM/OTAD/OTAN` transport and the entire receiver are UNCHANGED.** A holder emits byte-identical `OTAM {target, session, m, sig}` / `OTAD {target, session, seq, payload}` and consumes `OTAN {target, session, window_base, bitmap}` — the leaf's `parse_ota_frame` + session state machine can't tell a holder from the gateway, because it verifies `sig` over `m` regardless of sender. That source-agnosticism is the load-bearing property; peer-sourcing adds only crown↔holder arbitration frames.

### 2.1 New arbitration frames (crown↔holder unicast)

**`SMOLv1 ODEL `** — crown→holder **delegate-to-serve** (the only thing that authorizes a serve):
```
tag[12] "SMOLv1 ODEL " · target[3] (leaf T) · build[u32 LE] · session[2] · term[u16 LE] · M_len[1] · M[M_len] · sig[64]
```
- `session` is minted by the crown (globally the one in-flight session — §4), so the holder serves under the crown's session, and T binds to it (§5.3).
- **`M` + `sig` ride the ODEL** (the crown staged the build, so it has them): ≈174 B total, under the 250 B MTU. This is what lets the holder serve with **zero persisted manifest** — it pairs the crown-supplied `(M, sig)` with image bytes from its own slot and emits a byte-identical `OTAM` (§8.1). The sig is still the offline-signed one, so the holder cannot forge.
- `term` = the crown's election term/epoch (see §5.3 split-brain). A holder rejects an ODEL whose `term` is older than the highest term it has seen → a dethroned crown cannot delegate.
- **Auth of ODEL:** ODEL only *starts a serve of an already-signed image*; it cannot cause a flash (the leaf still verifies the image sig). So ODEL needs only **replay/stale protection** (`term` + `session`), not a signature. (If group-HMAC #190 lands, ODEL rides it for free; noted, not required.)

**`SMOLv1 ODON `** — holder→crown **serve-outcome** (so the crown advances or falls back):
```
tag[12] "SMOLv1 ODON " · target[3] · build[u32 LE] · session[2] · result[1]
result: 0=OK (all windows served, last-window exhaustion = the #40 confirm)  1=target-unreachable  2=aborted  3=self-slot-verify-failed
```

That is the **entire** new wire surface: two crown↔holder unicast frames + the §1.2 HOLD broadcast. Three frames, none on the receiver's hot path.

### 2.2 Chosen shape: crown-delegated, holder-served (vs. holder-served-with-arbitration)
Two shapes were considered:
- **(A) Crown delegates, holder serves** *(chosen)* — crown mints the session + picks source & target, holder does the byte-pushing, holder reports ODON. Single arbiter, holders are pure workers.
- (B) Holder serves directly, crown only arbitrates conflicts — holders self-select targets, crown vetoes. Rejected: it distributes the canary decision across holders, multiplying split-brain surface and complicating "exactly one in flight."

(A) keeps **all** sequencing decisions in one place (the crown), which is exactly what canary needs (§4) and what makes split-brain a single, closable hole (§5.3).

---

## 3. Integrity chain — the holder re-serves the SAME signed image

The security model is **identical to #40 and requires no extension** — this is the spec's most important property:

1. The holder's `OTAM` carries the original `M` + original 64-byte `sig`. The holder **cannot** alter build/size/sha without invalidating `sig` (it does not have the offline signing key — #32).
2. The leaf **verifies `sig` over `m` at OTAM receipt, BEFORE any flash write** (#40 invariant 1). A holder serving a *different* image fails this instantly.
3. Every OTAD chunk is **bounds-checked against the signed size** (`seq·231+len ≤ size`, HOLE-3) and written only through the partition-scoped `LeafImageWriter` (`FlashRegion` `OutOfBounds` guard). A corrupt/hostile chunk stream physically cannot escape the inactive slot.
4. `finalize` recomputes the **readback-sha of `slot[..size]`** and compares to the signed sha (TOCTOU-safe — hashes the bytes that will boot). A holder whose copy is subtly corrupt fails here → the image is discarded, the bootable slot untouched.
5. The **signed-freshness floor** still gates activation (`sig ok ∧ build > running ∧ build > fresh_floor ∧ size/sha ok`).

**Net trust delta: zero.** The holder is fully untrusted. Its worst case is **denial-of-service** (waste a session with a copy that fails verify) — never a bad flash. This is why peer-sourcing is safe to add over an unauthenticated mesh: #40 already assumed the *sender* is untrusted (the mesh is open RF); a holder is just another untrusted sender of an image whose authenticity rides its own signature.

---

## 4. Canary preservation — one target at a time stays STRUCTURAL

Peer-sourcing changes **who sources**, never **how many update at once**:
- The crown holds **exactly one live `session`** at a time. It mints a session, issues **one** ODEL (one target, one source), and **will not mint the next** until the current session closes (ODON received *and* the target self-confirms healthy — the #40 "hear-a-mesh-frame" self-test) or times out.
- There is **still no fleet-fetch topic** and no broadcast image push. ODEL is unicast to a single holder naming a single target. HOLD frames are inventory, not triggers — a holder **never serves unbidden**.
- The crown continues to **suppress its own self-OTA while any session (its own fetch or a delegated serve) is in flight** (the #40 "leaves first, gateway last" ordering generalizes: *no two OTA sessions overlap, whoever sources them*).
- Sequencing is unchanged from the operator's view: `install <leaf>` per board / HA Update button per board; the crown just routes the bytes through a holder instead of fetching them itself.

The bootloader-revert-is-OFF premise that makes canary structural is untouched; peer-sourcing does not widen the blast radius because the count-in-flight invariant (=1) is enforced at the single arbiter.

---

## 5. Failure modes

### 5.1 Holder dies mid-relay
Symptom: the target's `OTAN` NAKs stop being answered; the target session stalls and hits `LEAF_OTA_MAX_RETRIES`. The crown observes **no ODON** within the serve deadline (and/or the holder's HOLD/DIAG goes stale). Recovery: the crown **re-delegates** the same `(target, build)` to another candidate holder under a **new session**; if none qualifies, it **falls back to the gateway fetch-and-serve** (#40 path). The target's partially-filled inactive slot is inert (never activated; verify/activate is end-of-transfer only), so a mid-relay death is always safe to retry.

### 5.2 Holder's copy corrupt (sha mismatch at the receiver)
Two catches, in order:
- **Pre-flight:** a holder readback-sha-verifies its **own** `slot[..size]` before serving — in **slice-1** at `ODEL` receipt (serve-time; fail → `ODON=self-slot-verify-failed`), in **v2** before advertising `HOLD flags.verified` (§1.1). This keeps most bad copies from ever being offered.
- **Backstop:** if a copy corrupts after the offer, the **receiver's `finalize` readback-sha fails** → the leaf rejects the image (slot untouched-as-bootable) and NAKs/aborts → crown gets `ODON result=aborted` or a timeout → **falls back to the trusted gateway fetch** (re-pulls from the HTTP source of truth). A corrupt holder can never advance a leaf to a bad image; it can only cost one wasted session before fallback.

### 5.3 Split-brain double-source
Risk: two sources serve the same target (two crowns after an election split, or a stale ODEL from a dethroned crown). Three independent guards:
1. **Single arbiter:** the election (#76 lineage) guarantees one crown; only the crown mints sessions and issues ODEL. Shape (A) keeps this decision undistributed.
2. **Session binding at the target:** the leaf binds to the `(source, session)` of the **first valid `OTAM` it accepts** and ignores OTAD for any other session until the current one completes or times out. A second source on a different session is a no-op at the receiver.
3. **Term-stamping:** ODEL carries the crown's `term`; a holder rejects an ODEL with a `term` older than the highest it has seen, so a dethroned crown's delegation is refused at the source. (A newly-elected crown bumps `term`, invalidating any in-flight stale delegation.)

Combined: even under a transient two-crown window, the target flashes at most one image (still signature-verified), and a stale delegate is refused before a byte is served.

### 5.4 Holder off-ch6 / goes mesh-deaf while serving
A holder must be on ch6 to serve (HOLD `flags.on_ch6`); if it drifts, its HOLD stops advertising serve-ready and the crown won't pick it. A holder that drifts **mid-serve** stalls like §5.1 → re-delegate/fallback. Note serving is **not** subject to the coexist disease: it is ESP-NOW TX + tiny OTAN RX, and the [#53 study](../research/coexist-disease-esp-radio-018-study.md) shows TX + small-frame RX survive; only WiFi **bulk RX** goes deaf. So a serving holder is coexist-safe (it just monopolizes its own radio for the serve duration, by design — the same maintenance-op posture as the #40 gateway relay).

### 5.5 No qualified holder
Degenerate but normal at the start of every new build: fall back to the gateway seed fetch (§1.3). Peer-sourcing is a *strict improvement* — worst case it is exactly #40.

---

## 6. Rollout ordering with #217 — #217 lands first

**#217 (rung-2) ships before #237, and this is deliberate.**
- Peer-sourcing removes the fetch from every node **except the seed** — but a **seed fetch is unavoidable** (someone must pull each new build from HTTP once). #217 (2A crown-assumption AP gate, 2B pre-fetch AP gate, 2C fetch-leg-deaf→reassoc-before-retry — task #61) is precisely what makes that seed fetch **reliable**: don't fetch on a bad/ch-mismatched AP, and sense fetch-time deafness and reassoc before retrying (the exact fetch-leg coverage gap the [#204 pcap](https://github.com/jphein/smol/issues/204#issuecomment-5018759525) exposed, where `cdeaf` never tripped).
- #217 is **already in build** (task #61) and is a smaller, radio-level change; #237 is a larger architecture change. Landing the seed-fetch fix first means peer-sourcing is built on a fetch that works.
- They are **complementary, not competing**: #217 hardens the *one* fetch that remains; #237 removes *all the others*. After both land, a fleet roll has exactly one WiFi bulk fetch (the seed, now reliable) followed by N coexist-safe ESP-NOW serves.

Sequence: **#217 (seed-fetch reliability) → #237 (eliminate non-seed fetches).** #237 should also carry a config/DIAG-observable **fallback-to-gateway-fetch** path from day one so it degrades to #40 if a serve fails (§5).

---

## 7. DIAG / armdiag additions for observability

Everything below rides existing retained topics (gateway-flushed) + the DIAG/LDBG broadcasts; no new transport:

- **Source inventory:** publish the crown's HOLD-derived source table — e.g. `smol/<id>/ota/holds` = `build,verified,ch6` per node — so meshscope/HA shows *who can serve which build*.
- **Per-session source, not just target:** extend the existing `smol/<leaf>/ota/diag` / `…/relaydiag` (which today report the relay phase + headless progress%) with the **source id** and **session**, so an operator sees `id5 → id7 build 343 (session 0x1a) 62%` rather than an anonymous relay.
- **`LDBG` source id:** add the source node id to the leaf receive-side `LDBG` self-report (the leaf knows who its `OTAM` came from), so a mis-served leaf is attributable.
- **ODON outcomes → armdiag:** the crown surfaces the delegate outcome + any **fallback** (`served-by-peer` vs `fell-back-to-gateway-fetch`) so a roll's method is visible after the fact — this is also the metric that proves peer-sourcing is actually saving fetches.
- **Ground-truth ordering still applies** (see [ota.md §Ground truth during a roll](../../ota.md)): the pcap on the image host will now show **zero** fetch traffic for peer-served nodes — that *absence* is the success signal, cross-checked against the ODON `served-by-peer` count.

---

## 8. First implementation slice (v1-minimal) — smallest safe PR (slice-1 / #65)

**Goal of the slice:** the smallest PR that lets **Nexus (id8), already on the target build with a verified image, serve the remaining leaves (Herald / Aegis / Dominion) over ESP-NOW with zero gateway bulk-fetch for those three** — degrading to exactly #40 on any hiccup. Everything not on this list is deferred (§8.3). This section is the contract for task **#65**.

### 8.1 What slice-1 MUST include (and nothing more)
1. **Two frames only: `ODEL` + `ODON`** (§10 appendix). **Skip `HOLD` entirely.**
2. **Source-selection = "delegate to the node the crown just finished serving" (the baton) — no HOLD, no DIAG scan.** The crown is already walking a canary sequence; the node it *just* confirmed healthy on build N is, by definition, a valid source for the *next* target. So the crown keeps one variable — `last_confirmed_holder` — and delegates to it. The first target has no prior holder → the crown **seeds** it the #40 way (fetch-and-serve, or an operator `install` to a chosen seed like id8). After that, the baton passes down the sequence. *(This needs no inventory at all — just the crown's own memory of who it last updated; simpler even than scanning DIAG for a node running build N.)*
3. **Minimum holder persistence: ZERO — the `ODEL` carries `(M, sig)`.** A holder becomes a source only *after* it activates and **reboots** into build N, which clears `.bss` — so holder-side retention would have to be durable NVS. Avoid it: the **crown already holds the signed manifest** (it staged the build), so grow `ODEL` to carry `M`(≤86 B) + `sig`(64 B) (total frame ≈174 B, under the 250 B MTU — §10). The holder pairs the crown-supplied `(M, sig)` with **image bytes read back from its own active slot** and emits a byte-identical `OTAM`. The holder persists **nothing new**; the sig is still the offline-signed one (crown→holder→leaf), so the trust chain is intact and the holder still cannot forge. *(If you ever want a holder to self-serve without a live crown handoff — a v2 case — then persist the 64-byte sig in NVS and reconstruct `M` from `build`+`size`+readback-sha; but slice-1 does not need this.)*
4. **Serve reads back the ACTIVE slot** — extend the #40 gateway relay read-back (today: inactive slot) to also read the **running/active** slot. Flash reads only; safe.
5. **`ODEL` replay guard** — a minimal `term`+`session`: the holder rejects an ODEL whose `term` is stale (dethroned crown), and the target binds to one `(source, session)` (§5.3). Cheap; keep it in slice-1 (it's the split-brain floor).
6. **Fallback to #40 gateway fetch** on `ODON != OK` / serve-deadline timeout. **This is the safety floor — with it, worst-case slice-1 == today's #40.** A serve-time readback-sha self-check at the holder (`ODON=self-slot-verify-failed` on mismatch) triggers the fallback early.
7. **Canary inherited** — reuse the existing single-session #40 scheduler (one target in flight); the only change is `source = baton-holder | self`.
8. **Minimal observability** — add the `source id` to the existing `smol/<leaf>/ota/diag`; nothing more.

### 8.2 The concrete slice-1 PR — id8 serves three leaves (baton)
Assuming the peer-sourcing fw is fleet-wide (§9 bootstrap), a uniformity roll to build N:
1. Operator stages N (`ota_publish.sh stage`) and `install`s the **seed** = **id8** (Nexus). id8 self-fetches its one seed image (or is already on N). The crown sets `last_confirmed_holder = id8` once id8 is confirmed healthy.
2. Crown updates **Herald**: it sends `ODEL{target=Herald, build=N, session, term, M, sig}` to **id8** (the baton holder). No fetch.
3. id8 readback-sha-verifies its active slot, then runs the **existing #40 relay** (`OTAM/OTAD/OTAN`) to Herald — using the crown-supplied `(M, sig)`. Herald verifies sig → HOLE-3 → finalize-sha → activates → self-tests (hear-a-mesh-frame).
4. id8 → `ODON{OK}`; crown confirms Herald healthy → `last_confirmed_holder = Herald` (baton passes) → delegates **Herald** to serve **Aegis**, then Aegis to serve **Dominion** — one at a time. *(Or keep id8 as the sole source for all three — both are valid; the baton spreads serve load, a single source is even simpler. Slice-1 may hard-code either.)*
5. Any failure at any step → crown falls back to a #40 gateway fetch-and-serve for that one leaf, then resumes the baton.

That is the whole slice-1: three followers updated with **zero gateway bulk-fetch**, on the real fleet, with a hard floor of "no worse than #40," and **no new persistent state or broadcast frame** — just `ODEL`/`ODON` + active-slot read-back + the baton variable.

### 8.3 Explicitly deferred to v2 (cut corners, flagged)
- **`HOLD` broadcast + crown source-table auto-inventory + load-spread** (§1.2) — slice-1 uses the baton. Add HOLD when the fleet is large enough that auto-selecting *among many* holders (and spreading serve load beyond a linear baton) matters.
- **Holder-persisted `(M,sig)` / crownless self-serve** — slice-1 carries `(M,sig)` in `ODEL`; persist-and-reconstruct is only needed if a holder must serve without a live crown handoff.
- **Full split-brain hardening** beyond the minimal term/session guard (transient two-crown windows during a contested election) — harden with the #76 election lineage if contested-crown rolls become real.
- **Rich observability** — the `…/ota/holds` inventory topic, `LDBG` source attribution, and the `served-by-peer` vs `fell-back` dashboard metric (§7) — slice-1 ships only the source id on `ota/diag`.
- **Optional ODEL auth via group-HMAC #190** — slice-1 relies on term/session replay-guards (ODEL can't cause a flash, only start a serve of an already-signed image), so a signature is not required; fold in if #190 lands (v345 train).
- **True multi-hop / epidemic spread** — permanent non-goal (incompatible with structural canary), *not* a v2 item; noted only to keep it off the table.

None of the deferrals weaken the trust model — every one sits *above* the receiver's verify-before-flash floor.

---

## 9. Bootstrap sequencing — the "one more roll" irony

Peer-sourcing is the cure for gateway-fetch-per-follower, but the peer-sourcing **code** can only reach a fleet that doesn't yet have it via… gateway-fetch-per-follower. A node running v343 has no `ODEL`/serve path, so it can only *receive* the next build the #40 way.

**So there is exactly ONE more old-style roll, and it's unavoidable** (build numbers below are illustrative — the exact one is whichever train slice-1 / #65 rides; #190 + ledger are on the v345 train, so peer-sourcing lands on that train or the next):
1. Build **v_PS = current + peer-sourcing (§8 slice-1)** (≈ v344/v345). Roll it fleet-wide via the **existing #40 gateway path** — the *last* roll where the gateway bulk-fetches (or relays) for every follower. (This is the roll JP is waiting on for uniformity; it doubles as the bootstrap.)
2. Once v_PS is fleet-wide, **every** node has the serve path. The **next** roll (v_PS+1) seeds **one** node by gateway fetch, then peers serve the rest over ESP-NOW.
3. **After v_PS, the gateway never bulk-fetches for a follower again** — each new build costs exactly one seed fetch (hardened by #217) + N coexist-safe ESP-NOW serves.

This is a general property of self-improving delivery: the improvement ships one generation behind itself. Plan the v_PS roll as a normal canary-sequenced #40 roll (id-by-id, confirm-healthy-then-next); the payoff lands on the roll after.

> **Operator's-eye success signal** (ties to [ota.md §Ground truth during a roll](../../ota.md)): on the v_PS+1 roll, the image-host pcap shows fetch traffic for the **seed only** and **silence** for every follower — that absence, cross-checked against `ODON served-by-peer` counts, is the proof peer-sourcing is live.

---

## 10. Wire-frame appendix (grounded in `ota_mesh.rs`)

**Unchanged #40 frames (receiver path — do not touch):**
| Frame | Dir | Layout |
|---|---|---|
| `OTAM` | src→leaf | `tag[12] target[3] session[2] M_len[1] M[M_len] sig[64]` — signed manifest + ed25519 sig; verify-before-trust |
| `OTAD` | src→leaf | `tag[12] target[3] session[2] seq[2] payload[≤231]` — image chunk at `seq·231` |
| `OTAN` | leaf→src (unicast) | `tag[12] target[3] session[2] window_base[2] bitmap[8]` — windowed NAK; all-zero = advance |
| `LDBG` | leaf→bcast | receive-side self-report (add source id — §7) |

Constants (from `ota_mesh.rs`): `CHUNK_PAYLOAD=231`, `WINDOW_CHUNKS=64`, `WINDOW_BYTES=14784`, `OTAN_BITMAP_BYTES=8`.

**New #237 frames (arbitration only):**
| Frame | Dir | Layout | Slice |
|---|---|---|---|
| `ODEL` | crown→holder (unicast) | `tag[12] target[3] build[u32 LE] session[2] term[u16 LE] M_len[1] M[M_len] sig[64]` — delegate-to-serve **+ the signed manifest** (≈174 B, < 250 MTU); the holder needs no persisted `(M,sig)` | **v1** |
| `ODON` | holder→crown (unicast) | `tag[12] target[3] build[u32 LE] session[2] result[1]` — serve outcome (0=OK · 1=target-unreachable · 2=aborted · 3=self-slot-verify-failed) | **v1** |
| `HOLD` | holder→bcast | `tag[12] id[3] build[u32 LE] flags[1]` (bit0 verified, bit1 on-ch6) — source inventory | ▷ v2 |

> **Slice-1 note:** `ODEL` carries `M`+`sig` so a holder that rebooted into build N (clearing RAM) needs **zero durable OTA-manifest state** — the crown, which staged the build, supplies the signed manifest and the holder supplies the image bytes from its active slot. The v2 `HOLD`-inventory path (holder self-advertises) is the only case that needs holder-persisted `(M,sig)`.

---

## 11. Implementation checklist (for the fw agent — spec hands off here)

**Slice-1 (#65) — the v1 subset (§8):**
1. `ODEL`/`ODON` encode/parse (pure, host-testable — the flood/etx/ledger pattern); `ODEL` carries `M`+`sig`; `term`/`session` replay guards.
2. Crown baton: track `last_confirmed_holder`; `source = baton-holder | self`; seed the first target the #40 way.
3. Holder serve path = the existing gateway relay driver, but **reading back the ACTIVE slot** (running build) and sourced by `ODEL` (with crown-supplied `M`+`sig`) instead of `install`; serve-time readback-sha self-check.
4. Fallback-to-#40-gateway-fetch on `ODON != OK` / timeout.
5. Single-session arbiter reused from #40 (one target in flight).
6. Observability: `source id` on `smol/<leaf>/ota/diag`.
7. Host tests: baton selection, split-brain term/session rejection, corrupt-holder → receiver-sha-reject → gateway fallback.

**v2 (§8.3):** `HOLD` broadcast + crown source-table + load-spread; holder-persisted `(M,sig)`/crownless serve; rich observability; optional #190 ODEL auth.

**Receiver path, HOLE-3, freshness floor, A/B engine: untouched, both slices.**
