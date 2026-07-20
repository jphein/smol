# Peer-sourced leaf-mesh-OTA — implementation spec (#237, extends #40)

**Issue:** [#237](https://github.com/jphein/smol/issues/237) · **Extends:** #40 leaf-mesh-OTA · **Depends on:** #217 rung-2 (land first — §6) · **Motivated by:** [#54 mesh-OTA study](../research/reliable-mesh-ota-architectures.md), [#53 coexist physics](../research/coexist-disease-esp-radio-018-study.md), [#204 pcap](https://github.com/jphein/smol/issues/204#issuecomment-5018759525) · **Status:** design/spec only — no firmware · **Author:** nebula-babel · **Date:** 2026-07-19

> **Bottom line up front.** Today every leaf-mesh-OTA image originates from the crown's **WiFi bulk fetch** — the one operation the coexist disease makes hostile (bulk-RX-deaf; the crown blackholes its own download, [#204](https://github.com/jphein/smol/issues/204#issuecomment-5018759525)), and the [#53 study](../research/coexist-disease-esp-radio-018-study.md) proves that stays true on the esp-radio-0.18 stack. Peer-sourcing removes that fetch for every node after the first: once **one** node holds a verified build, the crown **delegates serving** to it, and it relays the **byte-identical signed image** to the next leaf over ESP-NOW using the **unchanged #40 receiver path**. The crown stays the sole canary arbiter (one target at a time, structural). No new trust: the holder is untrusted; the receiver still verifies the ed25519 sig **before any flash write**, so a bad holder can only DoS, never brick. Serving is coexist-safe by construction (ESP-NOW TX-heavy, no WiFi fetch → the disease can't fire).

> ### ⏩ Shipping posture — this is critical path (2026-07-20)
> JP chose "wait for #237 peer-sourcing" over plugging boards / RF changes: the fleet stays mixed
> (343/342) until this ships, so **implementability-now beats completeness.** The receiver is
> untouched, so the smallest *source-side* handoff that safely ships wins. This spec is therefore
> split into a **v1-minimal slice** ([§10](#10-first-implementation-slice-v1-minimal--smallest-safe-pr)) — the smallest PR that lets a
> holder serve the fleet — and **v2 deferrals** (flagged inline as `▷ v2` and listed in §10.3).
> The safety floor is constant across v1/v2: **fallback to the #40 gateway fetch means worst-case
> v1 is exactly today's behavior.** See [§11](#11-bootstrap-sequencing--the-one-more-roll-irony) for the one-more-gateway-roll bootstrap reality.

---

## 0. Scope

**In:** the source-side handoff — how the crown learns a peer holds a verified build, how it delegates serving one-target-at-a-time, how the holder re-serves, and the failure/observability surface.

**Out (unchanged from #40, spec'd here only as invariants that must be preserved):** the receiver reassembly/verify/flash pipeline (`OtaFrame` parse → verify-sig-over-M → HOLE-3 bounds → `LeafImageWriter` → readback-sha → `activate`), the `OTAM/OTAD/OTAN` chunk-transport wire, the staged-manifest MQTT flow (`OTA|build|size|sha256hex|sighex|url`), and the A/B slot / freshness-floor / self-test engine.

**Explicit non-goals:** multi-hop epidemic spread (an updated node auto-serving many peers unbidden). The `OtaRxView.hops` field is already reserved for a future true multi-hop, but this spec keeps serving **crown-arbitrated and single-target** — epidemic spread is incompatible with structural canary and is deferred.

---

## 1. Source-selection — how the crown learns a peer holds verified build N

### 1.1 What "holds" means (precise)
A node is a **valid source for build N** iff it can reproduce the exact signed image on demand:
- it has build N in a **readable slot** (its **active** slot if it is running N, or its **inactive** slot if it fetched/received N but hasn't activated), **and**
- it has **retained** the signed manifest tuple for N: `M = "build|size|sha256hex"` (≤86 B) + the 64-byte ed25519 `sig` (150 B total, one `.bss` record), **and**
- a **self-readback-sha of `slot[..size]` matches** the manifest sha (cheap pre-flight; a source must never offer a copy it can't itself verify).

The manifest+sig retention is the one new persistent-ish state a would-be source keeps. A self-fetched node already parsed `(M, sig)` from `smol/ota/staged`; a relay-received node already got `(M, sig)` in its `OTAM`. Both simply **keep** it (in `.bss`, cleared on a newer stage) instead of discarding after activate.

### 1.2 The HOLDS signal `▷ v2`
> **v1 skips the HOLD frame entirely.** The crown already knows every node's **running build** from DIAG, and the target-uniformity job only needs a source *running the target build*. So v1 source-selection = "a node DIAG-reports running build N" + a **serve-time readback-sha self-check** (the source verifies its own slot when it gets the ODEL; on mismatch it returns `ODON=self-slot-verify-failed` and the crown falls back to gateway fetch — §5.2). That is safe (the receiver still verifies the image sig regardless) and needs **zero new broadcast wire**. HOLD is the v2 upgrade: explicit "can-serve" inventory (retains-(M,sig) + on-ch6 + pre-verified) for **auto-selection + load-spread** across many holders once the fleet is large. The rest of §1.2 specs that v2 frame.

Add a new broadcast self-report — **`SMOLv1 HOLD `** (holder→broadcast, ~10 s cadence, piggybackable on the existing DIAG tick):

```
tag[12] "SMOLv1 HOLD " · id[3] · build[u32 LE] · flags[1]
flags bit0 = slot-readback-sha VERIFIED   bit1 = on ch6 (serve-ready)   bits2-7 reserved
```

The crown accumulates a **source table** `{node_id → (build, serve_ready)}` from HOLD frames (same drain path as DIAG). A node is a **candidate** for delegating build N iff `build == N ∧ flags.verified ∧ flags.on_ch6`. This reuses the roster/DIAG plumbing the crown already runs; no new subscription, no MQTT dependency (leaves have none).

> **Why not derive it from DIAG's running-build alone?** DIAG tells the crown a node is *running* N, but not that it *retains (M,sig)* or that its slot readback still verifies. HOLD is the explicit "I can serve N right now" contract; deriving it implicitly would let the crown delegate to a node that has since dropped its manifest or drifted off ch6.

### 1.3 Source preference order (crown-side)
When the crown needs to update leaf T to build N and T is not the seed, it picks a source in this order:
1. a **candidate holder** on ch6 with the fewest in-flight obligations (load-spread across holders as the fleet fills in),
2. else **itself** via the #40 gateway path (fetch-into-inactive → serve) — the **seed** fetch, or the fallback when no holder qualifies.

The very first node of any new build has no holder → the crown seeds it by fetch (§6 explains why #217 must harden exactly this fetch).

---

## 2. Session wire changes vs #40 — source-side handoff only

**The `OTAM/OTAD/OTAN` transport and the entire receiver are UNCHANGED.** A holder emits byte-identical `OTAM {target, session, m, sig}` / `OTAD {target, session, seq, payload}` and consumes `OTAN {target, session, window_base, bitmap}` — the leaf's `parse_ota_frame` + session state machine can't tell a holder from the gateway, because it verifies `sig` over `m` regardless of sender. That source-agnosticism is the load-bearing property; peer-sourcing adds only crown↔holder arbitration frames.

### 2.1 New arbitration frames (crown↔holder unicast)

**`SMOLv1 ODEL `** — crown→holder **delegate-to-serve** (the only thing that authorizes a serve):
```
tag[12] "SMOLv1 ODEL " · target[3] (leaf T) · build[u32 LE] · session[2] · term[u16 LE] · crown_sig-or-tokenTBD[…]
```
- `session` is minted by the crown (globally the one in-flight session — §4), so the holder serves under the crown's session, and T binds to it (§5.3).
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
- **Pre-flight:** a holder must pass its **own** `slot[..size]` readback-sha before it may set HOLD `flags.verified` (§1.1). This keeps most bad copies from ever being offered.
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

## 8. Wire-frame appendix (grounded in `ota_mesh.rs`)

**Unchanged #40 frames (receiver path — do not touch):**
| Frame | Dir | Layout |
|---|---|---|
| `OTAM` | src→leaf | `tag[12] target[3] session[2] M_len[1] M[M_len] sig[64]` — signed manifest + ed25519 sig; verify-before-trust |
| `OTAD` | src→leaf | `tag[12] target[3] session[2] seq[2] payload[≤231]` — image chunk at `seq·231` |
| `OTAN` | leaf→src (unicast) | `tag[12] target[3] session[2] window_base[2] bitmap[8]` — windowed NAK; all-zero = advance |
| `LDBG` | leaf→bcast | receive-side self-report (add source id — §7) |

Constants (from `ota_mesh.rs`): `CHUNK_PAYLOAD=231`, `WINDOW_CHUNKS=64`, `WINDOW_BYTES=14784`, `OTAN_BITMAP_BYTES=8`.

**New #237 frames (arbitration only):**
| Frame | Dir | Layout |
|---|---|---|
| `HOLD` | holder→bcast | `tag[12] id[3] build[u32 LE] flags[1]` (bit0 verified, bit1 on-ch6) |
| `ODEL` | crown→holder (unicast) | `tag[12] target[3] build[u32 LE] session[2] term[u16 LE]` — delegate-to-serve |
| `ODON` | holder→crown (unicast) | `tag[12] target[3] build[u32 LE] session[2] result[1]` — serve outcome |

---

## 9. Implementation checklist (for the fw agent — spec hands off here)

1. `(M, sig)` retention record in `.bss` (150 B), populated on self-fetch + on relay-receive, cleared on a newer stage; a `holds_build() -> Option<(u32, bool)>` self-verify (readback-sha) gate.
2. `HOLD` encode/parse + crown-side source-table accumulation on the DIAG drain path.
3. `ODEL`/`ODON` encode/parse (pure, host-testable — the flood/etx/ledger pattern) + `term`/`session` replay guards.
4. Crown source-selection + single-session arbiter (extend the existing #40 relay scheduler: source = holder|self).
5. Holder serve path = the existing gateway relay driver, but **reading back the ACTIVE slot** (running build) as well as the inactive slot, and sourced by ODEL instead of `install`.
6. Fallback-to-gateway-fetch on ODON!=OK / timeout.
7. Observability (§7) topics + LDBG source id.
8. Host tests: source-selection preference, split-brain term/session rejection, corrupt-holder → receiver-sha-reject → fallback.

**Receiver path, HOLE-3, freshness floor, A/B engine: untouched.**

---

## 10. First implementation slice (v1-minimal) — smallest safe PR

**Goal of the slice:** the smallest PR that lets **Nexus (id8), already on the target build, serve the remaining leaves (Herald / Aegis / Dominion) over ESP-NOW with zero gateway bulk-fetch for those three** — degrading to exactly #40 on any hiccup. Everything not on this list is deferred (§10.3).

### 10.1 What v1 MUST include (and nothing more)
1. **`(M, sig)` retention** — a 150 B `.bss` record (`M` ≤86 B + 64 B sig), populated when a node self-fetches *or* relay-receives a build, cleared on a newer stage. Without this a holder can't reproduce the signed `OTAM`. *(The only genuinely new persistent state.)*
2. **Serve reads back the ACTIVE slot** — extend the #40 gateway relay read-back (today: inactive slot) to also read the **running/active** slot, so a node serves the build it is *running*. Flash reads only; safe.
3. **`ODEL` / `ODON`** — the two crown↔holder unicast frames (§2.1), with a **minimal** `term`+`session` replay guard (reject an ODEL with a stale term; target binds to one `(source, session)`).
4. **v1 source-selection = DIAG-running-build + serve-time self-verify** (§1.2 `▷ v2` note) — no HOLD frame. The crown picks a node DIAG shows running build N; that node readback-sha-checks its own slot on ODEL receipt and `ODON=self-slot-verify-failed` if it can't (→ fallback).
5. **Fallback to #40 gateway fetch** on `ODON != OK` or serve-deadline timeout. **This is the safety floor — with it, worst-case v1 == today.**
6. **Canary inherited** — reuse the existing single-session #40 scheduler (one target in flight); the only change is `source = holder | self`.
7. **Minimal observability** — add the `source id` to the existing `smol/<leaf>/ota/diag` so a peer-served roll is legible; nothing more.

### 10.2 The concrete first PR — id8 serves three leaves
Assuming v344 (the peer-sourcing fw) is fleet-wide (§11), a uniformity roll to build N:
1. Operator stages N (`ota_publish.sh stage`) and installs to **id8** (Nexus) — id8 self-fetches (its one seed fetch) or is already on N; it retains `(M, sig)`.
2. Crown, updating **Herald**, sees id8 DIAG-running N → mints `session`, sends `ODEL{target=Herald, build=N, session, term}` to id8.
3. id8 readback-sha-verifies its active slot, then runs the **existing #40 relay** (`OTAM/OTAD/OTAN`) to Herald. Herald verifies sig → HOLE-3 → finalize-sha → activates → self-tests (hear-a-mesh-frame). **No gateway fetch.**
4. id8 → `ODON{result=OK}`; crown advances to **Aegis**, then **Dominion**, one at a time.
5. Any failure at any step → crown falls back to a #40 gateway fetch-and-serve for that one leaf, then continues.

That is the whole v1. It exercises the full value (three followers updated with zero gateway bulk-fetch) on the real fleet, with a hard floor of "no worse than #40."

### 10.3 Explicitly deferred to v2 (cut corners, flagged)
- **`HOLD` broadcast + crown source-table auto-inventory + load-spread** (§1.2) — v1 uses DIAG + serve-time self-verify. Add HOLD when the fleet is large enough that auto-selecting *among many* holders and spreading serve load matters.
- **Full split-brain hardening** beyond the minimal term/session guard (transient two-crown windows during a contested election) — v1's single stable crown + term-stamp covers the common case; harden with the #76 election lineage if contested-crown rolls become real.
- **Rich observability** — the `…/ota/holds` inventory topic, `LDBG` source attribution, and the `served-by-peer` vs `fell-back` dashboard metric (§7) — v1 ships only the source id on `ota/diag`.
- **Optional ODEL auth via group-HMAC #190** — v1 relies on term/session replay-guards (ODEL can't cause a flash, only start a serve of an already-signed image), so a signature is not required; fold in if #190 lands.
- **True multi-hop / epidemic spread** — permanent non-goal (incompatible with structural canary), *not* a v2 item; noted only to keep it off the table.

None of the deferrals weaken the trust model — every one of them sits *above* the receiver's verify-before-flash floor.

---

## 11. Bootstrap sequencing — the "one more roll" irony

Peer-sourcing is the cure for gateway-fetch-per-follower, but the peer-sourcing **code** can only reach a fleet that doesn't yet have it via… gateway-fetch-per-follower. A node running v343 has no `ODEL`/serve path, so it can only *receive* the next build the #40 way.

**So there is exactly ONE more old-style roll, and it's unavoidable** (build numbers below are illustrative — the exact one is whichever train slice-1 / #65 rides; #190 + ledger are on the v345 train, so peer-sourcing lands on that train or the next):
1. Build **v_PS = current + peer-sourcing (§10 v1)** (≈ v344/v345). Roll it fleet-wide via the **existing #40 gateway path** — the *last* roll where the gateway bulk-fetches (or relays) for every follower. (This is the roll JP is waiting on for uniformity; it doubles as the bootstrap.)
2. Once v_PS is fleet-wide, **every** node has the serve path. The **next** roll (v_PS+1) seeds **one** node by gateway fetch, then peers serve the rest over ESP-NOW.
3. **After v_PS, the gateway never bulk-fetches for a follower again** — each new build costs exactly one seed fetch (hardened by #217) + N coexist-safe ESP-NOW serves.

This is a general property of self-improving delivery: the improvement ships one generation behind itself. Plan the v_PS roll as a normal canary-sequenced #40 roll (id-by-id, confirm-healthy-then-next); the payoff lands on the roll after.

> **Operator's-eye success signal** (ties to [ota.md §Ground truth during a roll](../../ota.md)): on the v345 roll, the image-host pcap shows fetch traffic for the **seed only** and **silence** for every follower — that absence, cross-checked against `ODON served-by-peer` counts, is the proof peer-sourcing is live.
