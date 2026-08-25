# T-SCOPE — STEP T scoped before any code is written

**Status:** scoping only. No transport code exists on this branch and none should until this document
is agreed. Companion to `PHASE3-PLAN.md` §2 (which stays the spec); this is the pre-flight.

**Why a separate document.** T is the blast-radius step: seven `(controller, station)` pairs, two
mutually exclusive tiers, one atomic commit containing a ~2,000-line `mqtt_session` rewrite. Every
other step in the plan could be recovered by reading its own diff. This one cannot, so the decisions
that size it are taken here, in advance, where they can be argued cheaply.

**What has already changed under the plan.** Three of §2/§7's premises are stale, all because the
work that was supposed to precede T actually did precede it:

| plan says | now | consequence |
|---|---|---|
| §7 Q3: the 7th pair has "no `RadioManager`, no `Spawner` and no `Stack`" | **a `Spawner` exists on the `wifi` tier** — #391's `#[esp_rtos::main] async fn main(_spawner: Spawner)` is `#[cfg(feature = "wifi")]`, not espnow-gated | the convert price drops sharply; see §3 |
| §3.4: two forms of the bounded send, async one deferred "under the executor" | executor is live on every radio tier | STEP B landed the async form; T inherits it |
| §2.2 step 5: "update STEP G's declared count" | G's roster also carries `STATION-STACK-SITES` (empty) and per-site cfg guards | T's proof is mechanical; see §5 |

---

## 1. The seven pairs, and the shape of the boundary

From §2.1, unchanged (line numbers deliberately omitted per §0.2):

| # | `mode.rs::` caller | transport fn | tier |
|---|---|---|---|
| 1 | `maybe_leaf_reelect` | `run_mqtt_burst` | espnow |
| 2 | `burst_ntp` | `run_ntp_burst` | espnow |
| 3 | `resync_ntp` | `run_ntp_resync` | espnow |
| 4 | `run_ota_update` | `run_ota_fetch` | espnow |
| 5 | `run_leaf_ota_relay` | `run_ota_fetch` | espnow |
| 6 | `flush_telemetry` | `run_mqtt_burst` | espnow |
| **7** | **`wifi.rs::try_time_sync`** | **`run_ntp_burst`** | **`wifi`** |

### 1.1 The two tiers are mutually exclusive, and this is the single most load-bearing fact in the step

Pairs 1-6 are reachable only under `espnow`; pair 7 only under `all(wifi, not(espnow))`. **No binary
ever contains both call paths.** STEP G established the mechanism precisely and it is worth restating
because it is counter-intuitive: `mod wifi` is compiled on *every* radio tier, so pair 7's code is
COMPILED into the fleet image — what makes it exclusive is REACHABILITY (the `net.rs` re-export),
not compilation.

Two consequences that pull in opposite directions, and T has to hold both:

- **Good:** "convert pair 7 too" does not mean two embassy-net stacks in one image. It means two
  bring-up *sites in source*, one compiled per tier. The RAM cost is **not additive**.
- **Bad:** "compiled but unreachable" is a claim about a *call path*, not about the memory behind
  it. A static's cost follows its **consumer's** reachability, which can differ from the entry
  point's — and reasoning from the entry point is how you get a confidently wrong memory number.
  §3.2 is that mistake, made and then measured.

---

## 2. The atomic commit boundary — what is inside, and where it is legitimately red

§2.2's internal order stands. Restating the boundary in the form a reviewer needs:

**Inside the single commit** (cannot be split, and the PR must say so rather than let it be
discovered): the `wifi.rs` interface reshape · the `mqtt_session` rewrite (~2,000 lines carrying
#21/#56, #153, #309, #324, #217, #188) · `RadioManager::new` bring-up · pair 7's disposition ·
deletion of the old station consumer · the STEP G roster update.

**Outside it, as their own commits, before T:**
- `CrownApDecision::Deferred` (Addendum A.5) — explicitly "must not ride in the same commit as the
  boundary move".
- *(A `.data`/`.bss` reclamation commit was proposed here and then REFUTED by measurement — see
  §3.2. There is nothing to reclaim. Left in the record so the idea is not re-proposed.)*

**The window where the gate is red BY DESIGN.** Between bring-up (step 3) and deleting the old
consumer (step 5), `check_station_consumers.py` fails: an `embassy_net::new` exists while a
`SmolWifiDevice::new` is still live — arm 2, the packet-theft shape, which is exactly what it was
built to catch. §2.2 already warns that a contributor who "fixes" the declaration there has disarmed
it. **Scoping addition: that warning belongs in the commit message of the commit that opens the
window, not only in the plan** — a contributor mid-rebase reads commit messages, not `PHASE3-PLAN`.

---

## 3. Q3 priced: convert `try_time_sync`

**Decision (JP deferred to the team lead; this is the pricing, not the choice): CONVERT.** The
`wifi` tier exists to be the radio-minimal tier that still tells the time; `#[cfg]`-ing NTP out
leaves it compiling and useless, which is the worse of the two failure modes because CI stays green.

### 3.1 What converting actually costs, now that #391 has landed

§7 priced this as "standing up a second embassy-net bring-up for a tier that exists to be the
radio-minimal one". Two of that sentence's three cost drivers are gone:

| cost driver | §7's assumption | measured / verified now |
|---|---|---|
| no `Spawner` | had to be invented | **`#[cfg(feature = "wifi")] use embassy_executor::Spawner` already exists** (`main.rs`), and `#[esp_rtos::main]` hands one to `main` on the plain `wifi` tier. Thread it into `try_time_sync`. |
| second stack in one image | additive RAM | **not additive** — the tiers are mutually exclusive (§1.1). One `StackResources<N>` is compiled per build. |
| second bring-up in source | real | **still real.** This is the whole remaining cost: a duplicated `embassy_net::new` + `net_task` spawn. Mitigation: factor the bring-up into one `pub(crate) fn bring_up_stack(spawner, station, seed) -> &'static Stack` in `net.rs` and call it from both tiers, so DR-M3's two-draw seed, the `self_mac`-before-move ordering and the `log::error!`-not-`expect()` spawn policy exist **once**. All three are §2.2's named easy-to-lose details; one call site each is the cheapest way to not lose them. |

**Residual price of CONVERT, stated plainly:** one shared bring-up helper (~40 lines), a `Spawner`
threaded through one signature, and `try_time_sync`'s body reshaped from `&mut SmolWifiDevice` to
`&Stack` alongside the other six. It does **not** grow the atomic commit by a second bring-up.

**And to forestall the obvious §3.2 objection:** a shared `bring_up_stack` makes `StackResources<N>`
`wifi`-gated, which looks like exactly the too-loose gating §3.2 warns about. It is not, and the
reason is the rule §3.2 arrives at — *gate a static at its consumer's reachability*. Post-T the
espnow tier is a **genuine consumer** of that stack: pairs 1-6 are all espnow, and they are the bulk
of the transport. The `wifi` gate is therefore correct rather than merely convenient, and the fleet
tier pays for something it uses on every burst. (Contrast the shape §3.2 actually warns about: a
static reachable from *one tier's entry point only*, gated at the feature. That is not this.)

### 3.2 ⚠️ The mandatory clause — and a reclamation that DOES NOT EXIST (I tested it; it failed)

> **A correction I am flagging loudly because I nearly shipped it.** The first draft of this section
> claimed these 3,584 B were *dead* on the fleet tier and reclaimable by tightening one cfg, worth
> "+30% margin". **That was wrong.** The two measurements were right; the causal story I joined them
> with was not — which is precisely the §5.1 failure mode this document elsewhere insists on, made by
> me, inside the document arguing for it. It survived only because I tried the change instead of
> asserting it. **Do not reintroduce this idea; the experiment is recorded below so nobody re-derives
> it from the same two true facts.**

**Measured on `e1ad5f8`, fleet tier (`espnow,cast,io`), release, provisioned by `ci_provision.sh`:**

`try_time_sync` is **absent** from the fleet binary — the function is dead-code-eliminated (symbol
count 0). **Its statics are not.** All seven link in:

| static | size | section | cost |
|---|---|---|---|
| `NTP_SOCK_STORAGE` | 1,408 B | `.data` | DRAM **and** flash (initializer image) |
| `NTP_UDP_RX_META` | 64 B | `.data` | DRAM + flash |
| `NTP_UDP_TX_META` | 64 B | `.data` | DRAM + flash |
| `NTP_UDP_RX_DATA` | 512 B | `.bss` | DRAM |
| `NTP_UDP_TX_DATA` | 512 B | `.bss` | DRAM |
| `NTP_TCP_RX` | 512 B | `.bss` | DRAM |
| `NTP_TCP_TX` | 512 B | `.bss` | DRAM |
| | **3,584 B DRAM** (1,536 of it also flash) | | |

**Why they are there — the part I got wrong.** These statics do not belong to `try_time_sync`. They
belong to **`NtpMachine`**: `NTP_SOCK_STORAGE` is referenced only from `NtpMachine::new`, and
`NtpMachine::new` is called from **`run_ntp_burst`** as well — which is pairs 2 and 3 (`burst_ntp`,
`resync_ntp`), on the **espnow** tier. `run_ntp_burst` and `NtpMachine` are both present in the fleet
binary. So the 3,584 B is **live, correctly gated, and not reclaimable.** The fleet margin stays
**12,000 B**.

**The experiment, recorded so it is not repeated:** tightening all eight statics to
`all(feature = "wifi", not(feature = "espnow"))` does not compile —
`error[E0425]: cannot find value NTP_SOCK_STORAGE in this scope`, six times, from `NtpMachine`'s own
body. The too-tight direction fails **loudly, at compile time**. That is the safe direction.

**What genuinely follows for T — and it is a better rule than the one I first wrote.**

1. **The rule is NOT "always use the reachability gate".** That is what I nearly mandated, and it is
   wrong: these statics are `wifi`-gated *correctly*, because their **consumer** (`NtpMachine`) is
   reachable on every radio tier even though the wifi-tier *entry point* is not. **Gate a static at
   the reachability of its CONSUMER, not of its tier's entry point.**
2. **The two failure directions are not symmetric, and only one is dangerous.** Too tight → a
   compile error naming the symbol (loud, immediate, free). Too loose → DRAM that nothing uses,
   silently, with `.stack` shrinking to pay for it and no gate saying a word. **T's exposure is
   entirely in the second direction.**
3. **So the clause T may not skip is a MEASUREMENT, not a cfg convention.** When T introduces
   `StackResources<N>`, the question "is this consumed by shared machinery (like `NtpMachine`) or
   only by one tier's entry?" decides its gate — and the answer is only trustworthy if the resulting
   `.stack` is A/B'd per tier. If T factors bring-up into one shared helper (§3.1's recommendation),
   the stack resources become shared machinery and the `wifi` gate is *right*, with the fleet tier
   paying for something it genuinely uses.
4. **G's `per-tier` arm does NOT cover this**, and §5 is corrected accordingly. It pins that a
   declared site's cfg guard has not *moved*. It cannot detect a guard that is wrong-but-unchanged,
   nor a static gated more loosely than its consumer. That gap is real, it is what this section walked
   into, and the only instrument that closes it is a per-tier `.stack` A/B.

---

## 4. §2.4's rollback carriers, treated as a SET and proved BEFORE the commit

§2.4 is right that a source revert covers none of the three. Its steps are written as
*post-hoc* procedure ("clear the retained record and re-observe"). The lead's framing — prove them
as a set, before the commit — inverts that, and the useful discovery is that **two of the three stop
needing an experiment at all if T is scoped to not disturb them.**

| carrier | §2.4's post-hoc step | **pre-commit disposition** |
|---|---|---|
| **(a) retained `MC\|owner\|ch\|seq`** | clear it, re-observe a flip to a NEW value | **✅ RULED — inert by construction. T does not touch election seq semantics** (team-lead ruling, 2026-08-25). The risk was conditional on adopting the reference's free-running→resolve-stamped `seq` change; declining it leaves carrier (a) nothing to cross the seam. Enforced as a review assertion: **T's diff contains no `mc_pub_seq` / `mc_seen_seq` producer.** Two independent grounds, and the second is the stronger one: it is cheaper than an experiment on a retained topic that has defeated hardware verification four times (`[[smol-retained-mqtt-ghosts]]`) — *and* it is the correct **ownership boundary**, because JP's dynamic-channel directive (#269) puts election-semantics change in future `FOLLOW_ENABLED` work. T declining it is not T being careful; it is that campaign's change, not this one's. |
| **(b) NVS `broker_fallback`** | read back, clear net-cfg on the canary if divergent | **Provable statically, before the commit.** The hazard is the async rewrite changing *when* the flag is set. Enumerate `write_net_cfg` call sites on main, record the count and their enclosing fns in T's PR body, and assert T does not move or add one. Mechanically checkable today; if it must move, that is a design decision to surface, not a diff to notice later. |
| **(c) otadata** | check `Loaded app from offset` after every flash | **Genuinely procedural — stays post-hoc, and that is correct.** Reverting code does not revert boards. Not a precondition; it is a per-flash checklist item (`[[smol-espflash-otadata-trap]]`), and it belongs in the roll runbook rather than in T's gate. |

**Net effect:** the "carriers unproven as a set" blocker reduces to **one static enumeration (b) and
one scoping commitment (a)**, both completable before a line of T is written. (c) was never a
blocker, only a procedure.

---

## 5. What G's roster and B's taint let T assert MECHANICALLY

This is the part that did not exist when the plan was written, and it is why T is safer now than at
rev-1.

**From STEP G (`check_station_consumers.py`):**
- `STATION-CONSUMER-SITES` **is T's checklist.** T is complete, in the transport sense, exactly when
  that roster reaches zero `SmolWifiDevice::new` sites. Not a judgement — a count, checked both ways.
- `STATION-STACK-SITES: none` → the real site **is T's own proof of arrival.** The roster was
  declared empty specifically so the first `embassy_net::new` could not inherit a silent pass.
- The **`per-tier` arm does NOT enforce §3.2's clause**, and an earlier draft of this document said
  it did. What it actually does: pin the literal cfg guard at each declared site and fail closed if
  one is EDITED. What it cannot do: notice a guard that is wrong-but-unchanged, prove two predicates
  disjoint (no cfg algebra), or say anything about a static gated more loosely than its consumer —
  which is §3.2's whole hazard. **The only instrument for that is a per-tier `.stack` A/B.** Recorded
  as a known gap rather than papered over: G bounds who OWNS the transport, not what it COSTS.
- Arm 2 going red mid-commit is the design (§2), and the commit that opens the window should say so.

**From STEP B (`otam_to` / `TX_ABANDONED`):**
- The OTA announce evidence now **survives the transport move as a usable regression detector.**
  Before B, a T-induced egress failure and a T-induced slowdown both showed up as `otam_ok` not
  incrementing. Now they separate: `otam_ok=0, otam_to=0` is "sends confirmed-fail" (T broke egress);
  `otam_ok=0, otam_to=otam_tx` is "sends do not complete inside 30 ms" (T made the path slower).
  **That distinction is a T acceptance signal, and it exists only because B refused to collapse a
  timeout into a failure.** Read it on the canary before and after.
- Pairs 4 and 5 are the OTA fetch paths, i.e. RISKS §R12's brick-class flash-reentrancy territory.
  B changed nothing there; flagged so it is not assumed covered.

---

## 6. Open, and owned

1. **The real peak — and the instrument had to be rebuilt first (#434).** The bench run was
   attempted and **the measurement apparatus did not survive it.** `stack-paint` implied `bard`, so
   the only measurable composition contained the 260K-parameter transformer — and post-#391 that
   composition no longer boots on the C3: its linked `.stack` is **45,992 B** against a
   **>= 51,008 B** need at esp-hal's init guard (an 88-reset panic loop on id50, stack pointer
   5,016 B below the region floor before `main`). See #434.

   Two consequences this document has to carry:
   - **`ESP32C3_MEASURED_PEAK_BYTES = 55,656` is currently UNREPRODUCIBLE.** It was taken on that
     same bard-inclusive composition. The floor still *protects* — it is peak x 4/3 and the margin
     does not depend on how the peak was obtained — but the value has moved from *derived and
     re-measurable* to *derived from a composition that cannot run*. `budget.rs` now says so at the
     constant.
   - **A prerequisite appeared ahead of the whole order below:** `stack-paint-lite` (the `paint`
     instrument on the FLEET feature set, no `bard`, no `off-fleet`). Measured: its `.stack` is
     **byte-identical** to the fleet tier's (86,200 B), so the instrument costs zero DRAM and its
     peak is a peak for the shipped image rather than for a near-miss of it.

   **Still gates T's merge, not T's scoping.** Until a lite run exists, the honest position for the
   record is unchanged: `.stack` is unmoved by B1/B2 (86,200 B, A/B'd), so the **floor gate** is
   safe; the **peak** is not measured, and a `select` frame is transient stack the region size
   cannot see. **Nothing in T should spend the 12 KB until that number exists** — §3.2 confirms
   12,000 B is all there is, with no reclamation behind it.
2. **`StackResources<N>` sizing.** Not guessed here. `N` is a design input (socket count) and its
   footprint must be *measured* per tier — the instrument §3.2 shows is the only one that catches a
   static gated more loosely than its consumer. Measure it in a throwaway build before T, so T's own
   delta is readable against a known baseline rather than discovered inside a 2,000-line commit.
3. **RISKS §R6's two ELECT spins** (~600 ms non-yielding) freeze `net_task` under the executor.
   §2.3 requires converting them to `Timer::after().await`. Scoping note: that is a *behavioural*
   change to election **timing**, and it is inside T's atomic commit by necessity — the one part of
   T that changes fleet behaviour rather than transport plumbing.

   **This is compatible with §4(a)'s ruling and the distinction is worth stating precisely:** timing
   is not seq semantics. T may change *when* an election step happens; it may not change what a
   `seq` *means*. The review assertion in §4(a) (no `mc_pub_seq`/`mc_seen_seq` producer in T's diff)
   is what keeps those apart mechanically rather than by intent.

   **Consequent acceptance signal, because timing intersects the H1/H2 canary territory:** a bench
   **election-canary observation post-flash** — claim/defer timing against a live crown, same rig as
   the #198 rounds. A transport change that silently moved election timing would otherwise show up
   as a fleet-behaviour surprise rather than as a T finding, and the sub-second H1/H2 results on the
   233 fleet are the baseline it should be read against.

---

## 7. Recommended order

```
0. instrument  stack-paint-lite — decouple paint from bard (#434)         DONE, PR #438
1. paint       sentinel high-water on the LITE tier, P1.3+B1+B2         team lead, bench · gates T merge
2. pre-step    CrownApDecision::Deferred (Addendum A.5)                 own commit
3. pre-step    static enumeration of write_net_cfg call sites (§4b)     recorded in T's PR body
4. measure     StackResources<N> footprint, per tier, throwaway build   before T · sizes the commit
5. STEP T      7 pairs / 2 tiers / 1 commit, pair 7 CONVERTED           the big one
```

Item 0 did not exist when this document was first written — it appeared because the bench run
discovered the instrument was unusable (§6.1). It is the prerequisite for item 1, not a parallel
task. 2, 3 and 4 are startable now and none depends on the paint number; 1 gates 5's *merge*.
Nothing here needs a decision that has not been taken except `N` in §6.2.

**Acceptance signals for T, collected** (they are scattered above by topic, so here they are in one
place): the STEP G roster reaching zero `SmolWifiDevice::new` sites and one `embassy_net::new`
(§5) · `otam_ok` vs `otam_to` read on the canary before and after, which separates "T broke egress"
from "T made the path slower" (§5) · a per-tier `.stack` A/B, the only instrument that catches a
static gated more loosely than its consumer (§3.2) · and a **bench election-canary observation
post-flash**, because §6.3 changes election timing inside the atomic commit (§6.3).

*(A "reclaim 3,584 B" step stood at the head of this list until it was measured and refuted — §3.2.
The margin is 12,000 B and that is the whole budget.)*
