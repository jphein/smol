# PHASE3-PLAN — the Embassy transport + controller port, implementation-ready

> **Status:** plan of record. Derived from `p15-ownership-proposal.md` rev-2 (**Variant C adopted**,
> 2026-08-24) as re-grounded by `p15-oracle-review.md`, against `PORT-SPEC-phase1.md` and `RISKS.md`.
> **No code in this document.** Signatures appear only as interface specs.
>
> **Base:** `main` @ `dd0e4f4`. PR #391 (`feat/335-phase1`, P1.0–P1.3) is HW-canary-held and is a
> *precondition*, not part of this plan.

---

## 0. Read this section first: two terminology traps and a line-number policy

### 0.1 "Phase 3" is overloaded. This document uses ONE numbering.

`p15-ownership-proposal.md` §4.1 numbers its own sequence 0–3, where "phase 2" is the transport move
and "phase 3" is the controller move. The project-level roadmap calls this entire body of work
"Phase 3" (as in "#335 Phase 3", the thing this file is named after). Those two numberings collide
on the word that matters most.

**This plan renumbers once, explicitly, and never uses a bare "Phase 3" again:**

| this doc | p15 §4.1 | content |
|---|---|---|
| **STEP G** | phase 0 | the structural gate — lands first, alone |
| **STEP T** | phase 2 | the transport move (atomic, two tiers) |
| **STEP C** | phase 3 | the controller move (the remaining 13 refs) |
| **STEP B** | — | `SendWaiter` bounding (#397) — independent, lands anytime, see §3 |
| **STEP F** | — | the #404 forensics canary — a *pre-step*, see §6.2 |

Where a source document is quoted, its own numbering is preserved inside the quote and mapped.

### 0.2 The two source docs disagree with each other by one line, and both disagree with main

Not a transcription error — three different bases:

- `p15-ownership-proposal.md` / `p15-oracle-review.md` were written against `feat/335-phase1`
  @ `9e86030` and cite e.g. station `mode.rs:2683`, controller `:2688`.
- `PORT-SPEC-phase1.md` cites `2689`, `2742`, … — main's numbering.
- **Current main `dd0e4f4`** re-derived for this plan: `self.controller` = **23 matches**
  (19 code + 4 comments at `2834`, `2865`, `5837`, `6124`), `self.sta` = **6 borrows** at
  `2684`, `2838`, `2867`, `4850`, `5288`, `5830`, and `SmolWifiDevice::new` at **`mode.rs:2383`**
  and **`wifi.rs:517`** — *not* the `2382`/`515` both p15 docs state.

Every p15 station/controller line number is **main − 1**. The counts and the classification are
exactly right; only the anchors have moved.

> **POLICY, binding on every step below: anchor on `fn` name + a declared COUNT, never on a line
> number.** #405 shipped a `blocked_on` whose line number was 11 lines stale and which had been read
> as current for weeks; the same rot is already present in this document set before any code moved.
> Where this plan must name a location it names `file::function`, and where it must be machine-checked
> it is a declared count in the source (§1).

---

## 1. STEP G — the structural gate, first and alone

**Why it is a deliverable and not a nicety.** The oracle's finding §3 is the single most important
input to this plan: `Interface` is `Copy` (`esp-radio-0.18.0/src/wifi/mod.rs:1306`, `#[derive(Clone,
Copy, …)]`), so `embassy_net::new(interfaces.station, …)` **does not consume it**. A `Stack` and a
`SmolWifiDevice` can be live simultaneously and it compiles. Both bottom out in `data_queue_rx()`,
which is keyed by `InterfaceType` alone — so two consumers pop one queue and frames are stolen
nondeterministically. **No error, no panic, no failing gate.** The invariant that "all transport
consumers move together" is enforced by *nothing*.

That is this repo's dominant defect shape (`[[stubbed-intentions-under-deliver-silently]]`), and the
repo already has the answer pattern for it.

### 1.1 Deliverable: `tools/check_station_consumers.py`

Written in the **`tools/check_elect_send_path.py` idiom**, which is the house pattern for "an
invariant the type system cannot see":

- a **declared count in the source**, checked in **both directions** (so adding a consumer to an
  already-listed function fails too — the property that makes the ELECT checker actually work);
- **fail-closed**: if an anchor is not found, `exit 2`, never a pass;
- **exit codes** `0` ok · `1` violation · `2` malformed — same contract;
- wired into `tools/gate.sh` beside the existing `check_elect_send_path.py` call, with a
  **regression suite** proving each arm can fail (the `gate.sh:475` idiom, and `#350`'s
  `test_build_matrix.sh` lesson: a gate demonstrated only in its passing state is not evidence).

**The declaration**, in the `RAW-SEND-SITES` style already at `mode.rs:6255`:

```
/// STATION-CONSUMER-SITES: mode.rs::RadioManager::new:1, wifi.rs::try_time_sync:1
```

**One deliberate difference from the `RAW-SEND-SITES` precedent, worth deciding rather than
drifting into:** that declaration is in-file and describes only `mode.rs`. This invariant spans
**two files** (`mode.rs` and `wifi.rs`), because that is exactly what makes it dangerous — the two
station consumers are in different files under mutually exclusive `#[cfg]`s, which is why nobody
noticed there were two. **Spec: ONE declaration, file-qualified, in `mode.rs` beside the existing
one**, so the roster cannot be half-updated. The checker must then verify the *named* file/function
pairs exist (fail-closed if a name cannot be resolved) rather than trusting the string.

**The arms** — each one enumerated because it is a way to satisfy the types and still ship the bug:

| arm | what it catches |
|---|---|
| `count` | the declared per-function count of `SmolWifiDevice::new` drifts (currently **2**, both verified on `dd0e4f4`) |
| `coexist` | an `embassy_net::new` appears in a tier that also has a live `SmolWifiDevice::new` — **the packet-theft shape** |
| `per-tier` | the two consumers stop being mutually exclusive, i.e. their `#[cfg]` guards overlap. Today `try_time_sync` is `#[cfg(all(feature = "wifi", not(feature = "espnow")))]` (`net.rs:290`, dispatch `main.rs:861`) and `RadioManager::new` is the espnow tiers. **This arm is the one that matters after STEP T**, because STEP T is what could make them overlap. |
| `no-new-ctor` | a second constructor for the station device appears (`SmolWifiDevice::from`, `::wrap`, a `pub` tuple field) that the count arm would not see — the `no-accessor` arm's analogue |

**Sizing: write-in-one-sitting.** `check_elect_send_path.py` is 293 lines and covers six arms over a
7,569-line file. This is four arms over two anchors. Budget ~150 lines plus a ~60-line regression
suite. It is pure text — no cargo, no board — so it is fully host-provable (§5).

**⚠️ The `per-tier` arm is the load-bearing one and it is the hardest to write.** The other three are
greps with counts. Deciding "these two `#[cfg]` predicates are mutually exclusive" in a Python
checker is not a grep. **Spec'd deliberately narrow:** assert the *literal cfg attribute strings* on
the two declared sites match an allow-list recorded in the declaration, and fail closed on any
change. That detects "someone edited a guard" — which is the actual failure path — without
attempting cfg algebra. Written down as a limitation, not sold as more.

### 1.2 Ordering — this lands BEFORE any transport work, in its own PR

Non-negotiable, and it is cheap to honour: the gate's value is entirely in constraining STEP T, and
a gate that lands *with* the change it guards has never once been red. It also gives STEP T a
green-baseline to diff against.

---

## 2. STEP T — the transport move: one commit, two tiers

**The atomic unit is seven `(controller, station)` pairs across two mutually exclusive tiers.** Not
six across one — that was rev-1's error, and the seventh is in a CI-gated tier
(`tools/build-matrix.toml`'s `wifi` tier, compiled *and* clippy'd by `gate.sh`), so getting it wrong
turns CI red rather than failing quietly.

### 2.1 The seven pairs, by function (line numbers deliberately omitted — §0.2)

| # | `mode.rs::` function | transport fn called | tier |
|---|---|---|---|
| 1 | `maybe_leaf_reelect` | `run_mqtt_burst` | espnow |
| 2 | `burst_ntp` | `run_ntp_burst` | espnow |
| 3 | `resync_ntp` | `run_ntp_resync` | espnow |
| 4 | `run_ota_update` | `run_ota_fetch` | espnow |
| 5 | `run_leaf_ota_relay` | `run_ota_fetch` | espnow |
| 6 | `flush_telemetry` | `run_mqtt_burst` | espnow |
| **7** | **`wifi.rs::try_time_sync`** | **`run_ntp_burst`** | **`wifi`** |

`run_ntp_burst` therefore has **two callers**. `net.rs:329` says so in the tree already: *"Shared by
both radio-init paths."*

### 2.2 Internal order within the single commit

The commit is atomic at the *repository* level (it must be — §1's queue argument), but the work
inside it has a required order. Doing it in a different order produces a tree that cannot be
compiled at any intermediate point, which makes bisecting a mistake inside it impossible.

1. **`wifi.rs` interface first, callers last.** Re-shape `NtpMachine` / the `run_*_burst` family to
   take a `&Stack` instead of `&mut SmolWifiDevice`, keeping the *existing* `step_assoc(&mut
   WifiController)` signature untouched. The seam is already structural: `step_assoc` takes the
   controller, `step_dhcp`/`step_sntp` take only the device. **This step must not touch a single
   controller call** — that is what makes STEP C possible.
2. **`mqtt_session` rewrite** (~2,000 lines, carrying #21/#56, #153, #309, #324, #217, #188). The
   largest single body of work in the plan. It is *inside* the atomic commit and cannot be split out,
   which is the honest cost of Variant C and should be stated in the PR rather than discovered.
3. **`RadioManager::new` bring-up**: `embassy_net::new`, `StackResources<N>`, spawn `net_task`.
   Three details that are load-bearing and easy to lose:
   - **DR-M3 seed** — `seed` from two `rng.random()` draws. A shared literal gives the whole fleet
     identical TCP ISNs and ephemeral ports.
   - **`self_mac` must be read before `interfaces.station` moves** (#68/#76 self-frame drop).
   - **spawn errors are `log::error!`, never `.expect()`** — smol's boot path is panic-free by
     policy; a panic is MF-2 `software_reset` → boot loop.
4. **The seventh path** (`try_time_sync`) — decide *before starting*, not during. See §7 Q3.
5. **Delete the old station consumer** and update STEP G's declared count in the same commit. The
   gate will be red between (3) and (5) **by design** — that is the gate working, and the PR should
   say so, because a contributor who sees it go red mid-work and "fixes" the declaration has
   disarmed it.

### 2.3 Invariants no step may break

Carried forward from `p15` §4.3 and `PORT-SPEC` §2.4.3, restated because each has a named issue
behind it:

- **#217r3 / #269 / #278** — `reassoc_ch6_prefer` computes a `CrownApDecision`, applies
  `with_bssid` **and** `with_channel`, records `my_ap_channel`, bracketed by ELECT announce bursts at
  one epoch. **The reference hard-pins `ESP_NOW_FIXED_CHANNEL` in `wifi_task` and porting that
  verbatim regresses all three.** PORT-SPEC §2.4.3 is right and the reference is wrong here.
- **#139** `set_power_saving(None)` re-asserted after *every* connect. **#141**
  `assert_max_tx_power()` after driver start.
- **RISKS §R13** — do **not** re-arm the #204 detector until `downstream_seen` is plumbed. Re-arming
  it early does *active harm*: `crown_deaf_streak` climbs on every connected flush → `deaf_shed` →
  spuriously demotes every healthy crown, fleet-wide.
- **RISKS §R12 (P4-H1), brick-class** — `embassy_sync::Mutex` is not reentrant. Acquire flash **once**
  and thread `&mut FlashStorage` through the entire OTA sequence, with zero residual internal
  `flash_mut()`/`lock()`. Relevant to STEP T because pairs 4 and 5 are the OTA fetch paths.
- **RISKS §R6** — the two ELECT spins (`while !self.elect_announcer.settled()` and
  `while !…clear_to_move()`) are ~600 ms of non-yielding busy-wait. Under the executor they freeze
  `net_task` including the DHCP/TCP pump. Convert to `Timer::after().await`.
- **RISKS §R14** — 🔴 **never `cargo fmt` in `rust/clock`** (one run reformatted all 41 files and
  buried a 6-file change); **never rebase `dream/feat-embassy`**; worktree-isolate; serialize release
  builds; build the branch tip, not an increment SHA.

### 2.4 The revert story — source-clean, fleet-dirty

`git revert` of the STEP T commit yields a coherent, compiling tree. That is genuine and it is
Variant C's strongest structural property. **It is also not what "revertible" has to mean here.**
Three fleet-state carriers cross the seam and **none** is covered by a source revert. Each gets an
explicit step:

**(a) The retained MQTT election record — the one most likely to bite.**
`MC|<owner>|<ch>|<seq>` on `smol/mesh/channel`, read against `MC_STALE_MS = 90_000` with a
frozen-seq liveness test. **Retained topics survive a firmware revert by construction** —
`[[smol-retained-mqtt-ghosts]]`, which defeated hardware verification four times in one night. If
the election work adopts the reference's seq-semantics change (free-running `mc_pub_seq` →
resolve-stamped `mc_seen_seq`), a record written by the new image is then read by the old image's
frozen-seq test. Probably survivable — seq is compared for *change*, not absolute value — but
unanalysed.
→ **Rollback step:** clear the retained record and re-observe a flip **to a new value** before
trusting any post-revert election reading. Persistence proves nothing.

**(b) NVS.** `write_net_cfg(NetCfg { broker_fallback: true, .. })` is written from *inside* the
transport layer and persists across OTA by design. If the async rewrite changes *when* that flag is
set, a reverted image boots against a record written under rules it does not share.
→ **Rollback step:** read back; if divergent, clear net-cfg on the canary. Note `write_net_cfg` is
also RISKS §R12's **P4-M1** (a bare-locking flash writer that can park the whole loop).

**(c) otadata.** Reverting code does not revert boards; going back is another OTA, with
`[[smol-espflash-otadata-trap]]` in force.
→ **Rollback step:** check the `Loaded app from offset` line after **every** flash;
`espflash erase-region 0xf000 0x2000` (otadata only, keeps NVS) if a USB flash must follow an OTA.

---

## 3. STEP B — bounding the ESP-NOW send (#397). Independent; land it early.

This step is **not** sequenced with T or C — it improves today's superloop and it changes shape under
the executor. It is in this plan because #397 explicitly hands it here.

### 3.1 #397's mechanism, confirmed from source

`esp-radio-0.18.0/src/esp_now/mod.rs:582-606`:

- `SendWaiter::wait()` → `core::mem::forget(self)` then
  `while !ESP_NOW_SEND_CB_INVOKED.load(Acquire) {}` — a bare spin. No timeout, no yield.
- `impl Drop for SendWaiter` → **the same bare spin.**

So a lost TX-done completion does not time out; it pins the CPU until the WDT. **Confirmed exactly as
#397 states it.**

### 3.2 Two corrections to #397's scope — both widen it

**(i) It is FIVE sites, not one.** #397 names `send_to` (`mode.rs:6279` region). Re-derived on
`dd0e4f4`, every raw `esp_now.send(` site:

| site | function | waiter handling |
|---|---|---|
| `5012` | `send_arb_raw` | `let _ = waiter.wait();` |
| **`5250`** | `run_leaf_ota_relay` (pre-announce) | **`let _ = self.esp_now.send(…);` — waiter DISCARDED** |
| `5405` | `run_leaf_ota_relay` | `if waiter.wait().is_ok()` → `otam_ok` |
| `5485` | `run_leaf_ota_relay` (post-fetch) | `if waiter.wait().is_ok()` → `otam_ok` |
| `6299` | `send_to` | `let _ = waiter.wait();` |

**`5250` is the important one and no `waiter.wait()` grep will ever find it.** It discards the
`Result<SendWaiter>`, so the waiter drops immediately — and `Drop` carries the *same* unbounded spin.
It reads as fire-and-forget and is not. This is
`[[literal-grep-proves-nothing-about-constructed-strings]]`: grepping the visible symptom
under-counts the defect. **Any fix that bounds `wait()` and not the discard path leaves this one
live.**

Reconciles cleanly with the existing declared count at `mode.rs:6255`
(`send_to:1, send_arb_raw:1, run_leaf_ota_relay:3` = 5). **The declaration was already right;
#397's scope was not.** Fold the correction back onto #397.

**(ii) Two of the five feed diagnostic ground truth.** `5405`/`5485` use `wait().is_ok()` to
increment `otam_ok`, which exists to *prove egress*. Under a bounded scheme a timeout is neither a
success nor a confirmed failure, and collapsing it into either corrupts the evidence. **Spec: count
timeouts in a third counter** (`otam_to`), or the OTA-mesh announce evidence silently degrades —
`[[suspect-the-instrument-first]]`.

### 3.3 The reference implementation, and the contract it violates

The watch's `src/net/smol_mesh.rs:182 send_bounded` — `select(esp_now.send_async(addr, data),
Timer::after(TX_WAIT_MS))`, `TX_WAIT_MS = 30`, timeout ⇒ `false`. Proven on hardware (it fixed real
UI deaf-windows).

**⚠️ But it violates the documented contract of the API it wraps, and this needs a decision rather
than a copy-paste.** `EspNow::send_async`'s own doc: *"The returned future **must not be dropped
before it's ready** to avoid getting wrong status for sendings."* On timeout, `select` drops the
`SendFuture` before it is ready — precisely the prohibited move.

Characterized from source, so the risk is exact rather than vague:

- `SendFuture` has **no `Drop` impl** (verified: zero matches). So dropping it does **not** spin —
  the async path genuinely escapes the hang. That much is clean.
- But `poll` stores `ESP_NOW_SEND_CB_INVOKED = false` on first poll and then reads the **global**
  `ESP_NOW_SEND_CB_INVOKED` / `ESP_NOW_SEND_STATUS`. A timed-out send's callback lands *later* and
  sets those globals. The **next** send can then read the **previous** send's status as its own.
- Secondarily, dropping the future abandons an in-flight send, so the next `esp_now_send` may fire
  while the previous is outstanding — which is the invariant `SendWaiter::Drop`'s spin exists to
  protect ("prevent the lock on `EspNowSender` getting unlocked before a callback is invoked").

**Failure mode: silent status misattribution, not a hang.** Strictly better than a WDT reset, and
strictly worse than it looks — and it lands on the two sites (§3.2(ii)) whose status *is* the
evidence.

**Spec'd mitigation** (cheap, local, no upstream change): a module-level `TX_ABANDONED` flag. Set it
on timeout; on the next send, if set, bounded-drain `ESP_NOW_SEND_CB_INVOKED` and clear the flag
before issuing. This keeps the egress counters honest and costs a few lines. **Alternative
considered and rejected for now:** an esp-radio generation counter — correct, but upstream, and it
blocks a fix that is live on the fleet today.

### 3.4 Two forms, because the stack changes underneath it

- **On today's superloop (land now):** `mem::forget` the waiter to skip *both* spins, then poll
  `ESP_NOW_SEND_CB_INVOKED` against a 30 ms deadline. `mem::forget` is not an optimisation here —
  it is the only way out, because `Drop` spins too. Must cover the `5250` discard site.
- **Under the executor (STEP T/C):** the watch's `select` form, plus §3.3's mitigation.

---

## 4. STEP C — the controller move: the remaining 13 refs

**Controller-LAST.** The one rev-1 argument that survived review intact, and it stands without §1 or
§5: controller-first (Variant D) would run an unproven concurrency pattern — `wifi_task` associating
while the superloop drives smoltcp on the same radio — across `reassoc_ch6_prefer`, which holds
**5 of the 19 refs across three classes**, for the *entire duration* of the transport phase.

### 4.1 It is NOT "13 mechanical conversions" — rev-1 said so and contradicted itself

`p15` rev-1 called the scan round-trip "the single largest piece of unported design in the phase" and
called the step containing it "trivial". Both cannot hold. The 13:

| class | n | functions |
|---|---|---|
| assoc lifecycle | 7 | `switch` ×2 · `reassoc_ch6_prefer` ×3 · `run_leaf_ota_relay` ×2 |
| queries | 4 | `rssi` ×2 · `is_connected` ×2 |
| **scans** | **2** | **`run_scan` · `reassoc_ch6_prefer`** — real unported design, §4.2 |

**Two functions straddle the T/C boundary**, which the clean 6/13 split obscures:

- `reassoc_ch6_prefer` — 5 refs, three classes (query + scan + lifecycle).
- `run_leaf_ota_relay` — 5 refs, and it is **the sharp one**: one paired-transport ref converts in
  STEP T while its four sibling controller calls stay synchronous until STEP C. It spends an entire
  step in a **hybrid state** — async transport, sync association, inside one function. Coherent (the
  controller is still `RadioManager`-owned) but not mechanical, and until the oracle review nobody
  had written it down. **The plan's mitigation is simply to say so in the PR** and to give that
  function its own acceptance row in §5.

### 4.2 The scan request/RESPONSE design — the actual new design work

Both scan sites are **response-shaped**, which the reference's idiom does not cover:

- `run_scan` — `let record = match block_on(scan_async(…))`; the caller needs the record back to
  publish it (#71).
- `reassoc_ch6_prefer` — `let decision = match block_on(scan_async(…))`, filtered to the SSID, fed to
  the pure `select_crown_ap` → `CrownApDecision`, which then drives `with_bssid`/`with_channel`
  **in the same function**. The caller cannot proceed without the value.

**The reference has no equivalent at all.** Its `WIFI_CMD` is fire-and-forget with a result-`Signal`,
which cannot carry a list-valued reply — and `a0d3e5a` *deleted these two callers*. #278/#368's
ranked probe ladder is **main-only work written after the fork**, so there is nothing to port.

**Agreed shape (from `p15` §4.2(a), oracle-confirmed sound):** `wifi_task` performs the scan,
**reduces it to the already-pure `ApView` list inside the task**, and signals the *reduced decision
input*. This keeps `select_crown_ap` pure and the payload small.

**Interface spec** (shape only):

- `SCAN_REQ: Signal<ScanRequest>` — carries the SSID filter and the reason (`Publish` for `run_scan`,
  `Reassoc` for `reassoc_ch6_prefer`). Distinguishing them matters: the two callers want different
  reductions from the same scan.
- `SCAN_RES` — **the AP LIST NEVER CROSSES THE TASK BOUNDARY.** Signal the *reduced result* per
  consumer: a `CrownApDecision` (a `Copy` enum, ~8 B) for the reassoc path, and the formatted
  record string (~150 B, bounded by construction) for #71. **Superseded spec — see Addendum A §A.4,
  which replaces an earlier draft of this bullet.** A prior version specced an
  `[ApView; 16] + len` payload on the stated grounds that `.with_max(16)` already bounded the scan.
  **It does not** — esp-radio 0.18 accepts and silently discards it (`mode.rs` documents this at
  length; upstream `esp-hal#5583`'s fix is not in 0.18). The list is genuinely unbounded, so a
  16-slot payload would have been an arbitrary cap and #367's objection applies to it after all.
  Addendum A carries the corrected design and the evidence.
- **The incumbent's RSSI must come from the task, and this is easy to miss.**
  `select_crown_ap(aps, mesh_ch, current)` takes `current: Option<ApView>`, and `coexist.rs`
  specifies: *"use the live `get_rssi()` for its `rssi`, not a stale scan entry"*. `rssi()` is a
  **controller** call — so once STEP C moves the controller into `wifi_task`, the live incumbent RSSI
  is only obtainable there. Under Addendum A's design this resolves itself (the selector runs in the
  task, beside the controller); under any design that ships the list out, the payload must carry the
  live incumbent too, or the selector silently starts hysteresis-latching against a stale scan RSSI
  and #217's `HYST_MARGIN_DB` anti-flap stops working as measured.
- **Timeout is mandatory — and it MUST NOT be reported as `NoAp`.** A `with_timeout` around the
  response await is required (a scan that never answers must not wedge the caller, and this is
  exactly the class of window RISKS §R11 can silently break). ⚠️ **An earlier draft of this bullet
  said to treat the timeout as `NoAp` because `select_crown_ap` "already handles" it. That is a
  defect, and the tree forbids it explicitly** — `reassoc_ch6_prefer`'s doc comment: *"the only
  'skip' value available is `NoAp` — which the caller cannot distinguish from 'no usable AP exists'
  and would act on by shedding the crown… If this path ever needs guarding, give the decision an
  explicit `Deferred` variant first — do NOT reuse `NoAp`."* `NoAp` feeds the shed→degraded ladder;
  a timed-out scan would therefore demote a healthy crown. **`CrownApDecision` must gain a
  `Deferred` variant, and it must land BEFORE the scan crosses a task boundary** — the async path
  introduces a *second* "couldn't answer" source (response timeout) on top of the heap-gate skip
  the same comment was written about.
- **`run_scan`'s reduction is NOT `reassoc`'s** — and they are more disjoint than they look. #71
  publishes a *band survey* across all SSIDs, strongest-5; reassoc filters to our own SSID and needs
  the complete filtered set. Full field diff in Addendum A §A.2.

---

## 5. Per-step acceptance evidence

**The number this whole port exists to crush: the ~500 ms `worst_app_gap`.** DIAG format is
`brst=<gap>:<burst>:<kind>` (`mode.rs:3951`), where `kind` ∈ `f` TelemetryFlush · `n` NtpResync ·
`r` Reelection · `o` SelfOta, and a trailing `+` means saturated (`brst=65535:65535:o+` is honestly
"at least 65.5 s"). `SUBTICK_MS = 20`, so a 500 ms gap is ~25 missed subticks.

**Per `[[gate-that-cannot-fail]]`: ask for THE NUMBER, not "green".** Every gate in this repo is a
compile-time gate; all 21 can pass on an image that does nothing it exists to do.

| step | evidence | host-provable? |
|---|---|---|
| **G** | ✅ **LANDED.** `check_station_consumers.py` exits 0 on main (`2 station consumer(s) across 2 declared fn(s); 0 embassy_net::new site(s); 2 cfg guard(s) pinned`); the suite is **21 arms, 0 failed** — 9 hazard (one per enumerated bypass), 8 fail-closed (rc=2), 2 regression, 1 baseline. Declared count = **2** | ✅ **fully host-provable** — pure text, no cargo, no board |
| **B** (sync form) | all **5** sites bounded incl. the `5250` discard; a fabricated lost-completion does not hang; `otam_to` counted separately | ⚠️ **mixed** — the bounding is host-reviewable, but "does not hang under a real lost completion" is **HW-gated** |
| **T** | `brst` on the same board, same duty, **before vs after**; per `BurstKind` — the `f` and `r` rows are the ones that matter. Plus: `wifi` tier compiles *and* clippies (the 7th path), `.stack` ≥ `ESP32C3_STACK_FLOOR_BYTES` = 74,208, image ≤ 0x1F0000 | ⚠️ **compile gates host-provable; the `brst` number is HW-gated** |
| **C** | `brst` again (the assoc-path spins are STEP C's territory); `reassoc_ch6_prefer` still applies **both** `with_bssid` and `with_channel` and still records `my_ap_channel` (#217r3/#269/#278); a scan timeout yields `NoAp` and does not wedge | ⚠️ **the #217r3 invariant is host-provable by inspection + the send-path checker; the ladder behaviour is HW-gated** |

### 5.1 Honest margins, replacing a withdrawn argument

Rev-1 claimed "7,960 B of margin to an 80,000 B abort line". **There is no 80,000 B line** — it was a
tripwire in a dispatch brief that got propagated into a design document as a repo constant. The only
gate is `ESP32C3_STACK_FLOOR_BYTES = 74,208`.

**⚠️ UPDATED 2026-08-25, and the direction is DOWN. The 87,960 / 13,752 B figures above were
measured before #391 landed** — i.e. on Phase 1 rebased onto the *pre*-de-pin main. As merged
(`ec3ee63`), the fleet tier measures:

| | `.stack` |
|---|---|
| `origin/main` before #391, fleet tier, same seed | 106,472 B |
| **as merged, fleet tier** | **86,208 B** |
| `ESP32C3_STACK_FLOOR_BYTES` | 74,208 B |

So the real margin is **12,000 B, not 13,752 B**, and the executor's cost is **20,264 B** of DRAM
(`.bss` growth silently shrinking `.stack` — `[[smol-stack-is-not-headroom]]`). Independently
confirmed: CI's own `gate.sh fw` line on #391's head reads `stack: 86208 B (floor 74208 B)`.

**The number varies by a few bytes between trees, so never assert byte-equality on it — the
inequality is the gate.** Observed range, four runs: 86,208 B (familiar, developer seed) · 86,200 B
(familiar, `tools/ci_provision.sh`) · 86,208 B (GitHub CI, which also runs `ci_provision.sh`) ·
86,208 B (GitHub CI, #391's head). Note what that does NOT say: the delta is *not* "provisioned vs
developer seed", which is what a single familiar-vs-CI comparison first suggested and which CI then
contradicted. `secrets.rs`/`board.rs` are git-ignored and provisioned per tree — and
`ci_provision.sh` substitutes a *fresh random* group key each run — so their literals land in
`.data`/`.rodata` at slightly different sizes and `.stack` is whatever DRAM is left. That is the
tree-dependence this file already warns about under "what this file deliberately does not contain";
attributing it to a specific cause from one observation is how a caveat becomes folklore
(`[[flagged-caveat-is-not-contained]]`).

The floor remains conservative for the reason stated: `ESP32C3_MEASURED_PEAK_BYTES = 55,656` was
measured with the `RadioManager` frame on the stack, ~18.9 KB of which P1.3 moved into `.bss` — so
the true post-P1.3 peak is *lower* than 55,656 and the 1.55× available/peak ratio implied by the
numbers above is pessimistic. **That mitigation is an argument, not a measurement, and it is not a
licence to spend the 12 KB.** Re-measure the peak with sentinel paint before treating the ratio as
recovered.

`StackResources<N>` + embassy-net buffers are a real cost to **schedule deliberately**. They are not
a scarcity argument and this plan does not make one. (An invented threshold quoted beside three
genuinely measured section sizes reads as measured — `[[flagged-caveat-is-not-contained]]`.)

### 5.2 The two board-gated items — both substantially discharged on 2026-08-25

1. **RISKS §R11 — the timebase stopwatch.** A wrong TIMG0 wiring **compiles** and yields embassy
   timers at the wrong rate; the symptom is not a crash. Every `WIFI_CMD` timeout, every scan
   timeout in §4.2, and every #324/#136/#278 window depends on it. **One stopwatch against a 15 s
   budget on the P1.3 image.** Cheapest high-value measurement in the plan.

   **⚠️ HALF-DISCHARGED (2026-08-25).** #391's HW gates measured MQTT publish cadence at **32–33 s
   wall against a 32 s device interval, across multiple intervals** (posted on PR #391). That rules
   out the failure this risk is really about — a timebase silently 2× or ½× — because a 2× error
   would have shown as ~16 s or ~64 s, not 32–33 s. **What it does not do is bound the error
   tightly:** a ~3% skew is inside that measurement's resolution and would still drift a 15 s #324
   window by ~0.5 s. The stopwatch remains the tighter form and is still worth its five minutes, but
   it is **no longer a hard blocker on STEP T** — it is a precision refinement. Downgraded from
   *blocking* to *strongly recommended before the first #324-window-sensitive change*.
2. **The #335 round-trip rollback canary** — forward leg, **reverse leg**, and forced-rollback leg,
   with §2.4's three carriers handled explicitly.

   **⚠️ MOSTLY DISCHARGED (2026-08-25).** 917 → 918 → 920-retreat was confirmed **on glass**, so the
   forward and reverse legs are real rather than argued. **Two gaps remain and they are exactly the
   ones §2.4 exists for:** the three fleet-state carriers were only **partially** exercised (so the
   retained-MC seq-semantics question, the NVS `broker_fallback` question, and the otadata
   `Loaded app from offset` check are each still unproven *as a set*), and the stuck 921 leg is
   **deliberately parked as PR #392's rig** rather than fixed here. **Verdict: the round-trip
   mechanism is proven; the carrier handling is not.** STEP T should not treat §2.4's rollback steps
   as pre-validated — they are still first-run procedures.

Note `probe-rs run` and `espflash monitor` die exit-144 in the agent sandbox — **RTT/serial capture
is a JP-run step** (`[[jp-bench-ping-when-physically-needed]]`). `probe-rs attach` and `espflash
flash` do work. Do not write an evidence step an agent cannot execute.

---

## 6. Folding in tonight's two defects

### 6.1 #403 — post-panic crown announces `MC` on channel 0

Measured, not theorized: a crown published `MC|50|0|<seq>` every ~32 s with MQTT fully working, and
**id8 + id51 were stranded 00:03→04:18** because crown relay is their only MQTT path. A clean-boot
reflash produced an immediate correct `MC|50|6`.

**This plan's position: `channel != 0` should be UNSENDABLE BY CONSTRUCTION, and STEP C is where the
election-adjacent code is already being touched.** Two mechanisms, deliberately both:

- **Structural** — the channel is validated where the announce is *sealed*, not where it is sent, so
  no future call path can bypass it. This is `SealedElect`'s existing shape: the seal is the only way
  to get frame bytes. A zero channel should fail to seal.
- **Machine-checked** — add the claim to `check_elect_send_path.py`, whose whole purpose is
  "the ELECT frame cannot reach the air unauthenticated" and which is the natural home for
  "…or with channel 0". #403 suggests this itself.

⚠️ **Sequencing caveat:** #403 is observed on the **#391 canary** and its root cause is open — is it
an announce/radio-init race specific to the executor's timing, or does main's #278 path have the same
window? **Making channel 0 unsendable is correct either way and does not require the answer**, but it
is a *guard*, not a fix. It converts fleet-stranding into a logged refusal. Say that in the PR; do
not let the guard close the root-cause question.

### 6.2 STEP F — #404's forensics canary, as a pre-step

**#404 re-attributes the peer-join panic to MAIN.** It reproduces on build 918 = current main
(id50, panic within ~60 s of joining a mesh with live peers). **The Phase-1 branch is exonerated —
it inherited the defect.**

**Why this is in this plan:** STEP T moves the transport. If the panic is still live and
unattributed, STEP T's `brst` evidence is being gathered on a board that panics hourly, and the
transport move becomes the prime suspect for a defect that predates it. **Worse: STEP T's async
rewrite could mask it** — a panic in a window that no longer exists in the same shape looks like a
fix and is not.

**Deliverable, as a PRE-step to T:**

- An `ESP_LOG=info` build + serial monitor under crown-with-peers duty (release images are
  serial-silent — the logger is baked at build time, `[[smol-esp-log-compile-time]]`). **JP-run**,
  per §5.2.
- Capture the panic's backtrace before STEP T lands, so post-T behaviour is diffable against a known
  signature.
- **Recommended and cheap:** a **panic COUNTER in DIAG** (#404's own suggestion, byte-budgetable per
  #306). Today DIAG shows only the *last* reset cause, so a panic followed by a power cycle is
  invisible — id8 reports `boot=284` and nobody can say what fraction were panics. Without a counter,
  "STEP T didn't make it worse" is unfalsifiable.

**Do not let STEP T's PR claim the panic is fixed.** If it stops reproducing, that is a *finding to
investigate*, not a result to bank.

---

## 7. Top-3 open design questions

**Q1 — Does #278/#368's ranked ladder survive reduction to `ApView` inside `wifi_task`?**
**✅ ANSWERED — see Addendum A.** Short form: the question contained a false premise. #278/#368's
ranked ladder is **not a scan consumer at all** (`scan_plan()` → `recovery_ladder(channel, follow)`
is pure channel arithmetic over the last accepted announcement), so nothing about it constrains
`ScanOutcome`. There are exactly **two** scan consumers, they read **disjoint** field sets, and the
resolution is to signal each one's *reduced result* rather than any AP list. Addendum A also
overturns two claims this plan previously made (§4.2's `with_max(16)` bound and its
timeout-as-`NoAp`).

**Q2 — Does STEP B's timeout mitigation belong in smol or upstream in esp-radio?**
The watch's `send_bounded` is proven on hardware **and** violates `send_async`'s documented "must not
be dropped before ready" contract (§3.3). My spec'd `TX_ABANDONED` drain is cheap and local but is a
workaround for a global-status API, and it lands on the two sites whose status is diagnostic ground
truth. **The decision:** accept local mitigation now (fleet has a live WDT hazard) and file the
esp-radio generation-counter issue, or hold STEP B for upstream. **Recommendation: local now, file
upstream** — but it is a call about carrying a known-nonconforming API use, and it should be made
explicitly rather than inherited from the watch.

**Q3 — `try_time_sync`: convert it, or `#[cfg]` it out and let the `wifi` tier lose NTP?**
The 7th pair, in a CI-gated tier, with no `RadioManager`, no `Spawner` and no `Stack`. Converting it
means standing up a second embassy-net bring-up for a tier that exists to be the radio-minimal one;
`#[cfg]`-ing it out means the `wifi` tier stops having a time path, which is most of what it is *for*.
**Must be decided before STEP T starts, not during** — it changes the size of the atomic commit, and
it is the difference between "the `wifi` tier compiles" and "the `wifi` tier still does something".
`p15` §6.3 raised it and did not answer it; neither does this plan.

---

## 8. Sequencing summary

```
STEP F  (pre)   #404 forensics canary + DIAG panic counter        JP-run, board
STEP G          check_station_consumers.py + regression suite     host-only, own PR
STEP B          SendWaiter bounding, all 5 sites (#397)           independent, land early
  ── R11: half-discharged (32-33s wall vs 32s dev) ──             recommended, NOT blocking
  ── round-trip: mechanism proven on glass; CARRIERS not ──       §2.4 steps are first-run
  ── Q3 (try_time_sync) DECISION ──                               BLOCKS T — sizes the commit
STEP T          transport: 7 pairs / 2 tiers / 1 commit           the big one
  ── CrownApDecision::Deferred (Addendum A.5) ──                  pre-step, own commit
STEP C          controller: 13 refs + scan reduction (#403 guard)
```

`F`, `G` and `B` are all startable **now** and none depends on PR #391 merging.

**What actually blocks `T` has changed.** As of 2026-08-25 the two board measurements are no longer
the gate (§5.2): R11 is half-discharged and downgraded to *recommended*, and the round-trip's
mechanism is proven on glass — only its **carrier handling** is unproven, which §2.4 already treats
as procedure rather than precondition. **The remaining hard blocker on `T` is Q3** — whether
`try_time_sync` converts, is `#[cfg]`'d out, or keeps a cfg-gated sync NTP path — because that
decision sizes the atomic commit and cannot be taken mid-flight. `C` is blocked on `T`, plus the
`Deferred` pre-step.

---

## Addendum A — Q1 resolved: the scan-consumer field diff, and `ScanOutcome`'s shape

> Read-only investigation against `main` @ `e3a8f5d`. Every field claim below was read at its use
> site, not inferred from a type. **This addendum overturns two claims made earlier in this same
> document** (§4.2's payload spec and its timeout handling); both are corrected in place above
> rather than left standing with a note, because a flagged caveat is not a contained one.

### A.1 The question had a false premise: there are TWO scan consumers, not three

**#278/#368's ranked ladder does not read scan results.** `RadioManager::scan_plan()` is:

```
recovery_ladder(self.elect_follower.decision().channel, election::FOLLOW_ENABLED)
```

and `mesh_elect::recovery_ladder(last_known: u8, follow: bool) -> ([u8; RECOVERY_MAX], usize)` is
pure channel arithmetic over the **last accepted announcement's channel** — it never sees an AP.
Corroborated structurally: `AccessPointInfo` appears in exactly two files, `net/mode.rs` and
`budget.rs` (the latter only to record its measured 47 B size for #367's heap math). `ApView`
appears in `net/coexist.rs` (definition), `net.rs` (a comment) and `net/mode.rs`.

So the ladder places **no constraint** on `ScanOutcome`. That removes a third of Q1's stated
uncertainty, and it means the reassoc reduction is free to be shaped for `select_crown_ap` alone.

The two real consumers, both in `mode.rs`:

| consumer | entry | called from | terminal value |
|---|---|---|---|
| **#71 band survey** | `run_scan` → `format_scan_record` | `main.rs:2164` | a `SCAN\|…` record string → `own_scan` (gateway publishes `smol/<id>/scan`) or `broadcast_scan` (relay) |
| **#204/#217 reassoc** | `reassoc_ch6_prefer` → `select_crown_ap` | `mode.rs` ×3 (`4829`, `4883`, `5970`) | a `CrownApDecision` |

### A.2 The field diff

`esp_radio::wifi::ap::AccessPointInfo` (esp-radio 0.18, `wifi/ap.rs:17`) is the source of truth and
the upper bound on any reduction:

| `AccessPointInfo` field | type | #71 via `format_scan_record` | reassoc via `ApView` |
|---|---|---|---|
| `ssid` | `Ssid` | ✅ **`as_str()`, truncated to `SCAN_SSID_MAX = 12` chars, with `\|` and `,` replaced by `_`** | ⚠️ read as a **FILTER** (`a.ssid.as_str() == net.ssid`) — never carried into `ApView` |
| `bssid` | `[u8; 6]` | ⚠️ **only bytes `[0..3]`** (the OUI), hex-formatted | ✅ **all 6 bytes** |
| `channel` | `u8` | ✅ | ✅ |
| `signal_strength` | `i8` | ✅ | ✅ as `ApView.rssi` (**no clamping** — scan entries are already `i8`; the `clamp(-127,0)` in `mode.rs` applies only to the *incumbent* built from `get_rssi()`) |
| `secondary_channel` | `SecondaryChannel` | ❌ | ❌ |
| `auth_method` | `Option<AuthenticationMethod>` | ❌ | ❌ |
| `country` | `Option<CountryInfo>` (unstable) | ❌ | ❌ |

**Union actually needed: `ssid` (string), full `bssid`, `channel`, `signal_strength`.** Three fields
are read by nobody. One of Q1's specific worries is disposed of: **`auth_method` is unread**, so no
reduction loses it.

**Ordering and cardinality are as load-bearing as the fields, and this is where the two diverge
irreconcilably:**

| | #71 survey | reassoc |
|---|---|---|
| SSID scope | **ALL networks** — it is a band survey; that is the point of publishing it | **our SSID only** |
| ordering | `sort_by_key(Reverse(signal_strength))` — **required**, the record is "the strongest APs" | irrelevant (`max_by_key` inside the selector) |
| cardinality | `truncate(16)` then `.take(SCAN_MAX_APS = 5)` → **at most 5 reach the wire** | **complete filtered set — #367 forbids truncation** |
| heap gate | `scan_heap_ok()` vs `SCAN_HEAP_FLOOR_BYTES`; a low-heap tick **defers** | **deliberately ungated** — `NoAp` would shed the crown |

A single shared payload would have to carry all SSIDs at full cardinality to serve both — which is
precisely the unbounded allocation #367 exists to bound. **The two reductions cannot share a
payload. That is the finding.**

### A.3 The correction that changes the design: `.with_max(16)` bounds NOTHING

§4.2 previously justified a 16-slot payload as "the existing bound restated". It is not a bound.
`mode.rs` documents this in ~40 lines at the call site, verified against esp-radio 0.18 source:

- `scan_async` collects internally via `ScanResults::new(self)?.collect::<Vec<_>>()`;
- `ScanResults::new(_controller)` takes the **controller only** — the `ScanConfig` never reaches it;
  it sets `remaining = bss_total` (the full driver AP count) and walks every one;
- so one `AccessPointInfo` (47 B) is heap-allocated **per AP the radio can see** — ~150 BSSIDs ≈ 7 KB
  steady, ~14 KB while the `Vec` doubles;
- upstream `esp-hal#5583` closed 2026-05-28; **the fix is not in 0.18.0**.

`.with_max(16)` is accepted and silently discarded, kept only as documented intent. **I read the call
and not the comment above it** — the same failure this document's own §0.2 policy was written to
prevent, one field over. Consequence: the AP list is genuinely unbounded, so any fixed-size payload
is an arbitrary cap, and #367's objection applies to it.

### A.4 Recommendation: signal the REDUCED RESULT; the AP list never crosses the boundary

Both consumers' reductions **terminate in a small, bounded value**. Send that, not the list:

| signal | payload | size |
|---|---|---|
| `SCAN_RES_REASSOC` | `CrownApDecision` (`Copy` enum: `CoChannel{bssid,ch}` / `OffChannelFallback{bssid,ch}` / `NoAp` / **new** `Deferred`) | ~8 B |
| `SCAN_RES_RECORD` | the formatted `SCAN\|…` record | ≤ ~150 B by construction (5 APs × (12 SSID + 6 OUI hex + ch + rssi)) |

`wifi_task` owns the controller, so it runs the scan, then runs the reduction **in the task**:
`select_crown_ap(&views, mesh_ch, current)` for a `Reassoc` request, `format_scan_record(&aps)` for a
`Publish` request. `ScanRequest` carries what the task cannot know: the SSID to filter on, `mesh_ch`,
and the request kind.

Why this is the right shape rather than merely a smaller one:

1. **The cardinality question dissolves.** `max_by_key` runs over the complete SSID-filtered set
   inside the task, so #367 is satisfied *by construction* — there is no payload to bound and no
   truncation policy to get wrong.
2. **The field question dissolves too.** Each reduction reads `AccessPointInfo` directly, so neither
   can lose a field to the other's shape. #71 keeps its SSID strings; reassoc keeps its full BSSIDs.
3. **The live-incumbent problem solves itself.** `select_crown_ap`'s `current` needs a live
   `get_rssi()`, which is a controller call — and the selector now runs beside the controller.
4. **Purity and host-testability are preserved.** `select_crown_ap` stays pure in `net/coexist.rs`
   with its existing `experiments/ap_select_verify` harness; `format_scan_record` stays a pure
   formatter. Only the *call sites* move.
5. **Routing stays with the caller.** The record's destination (`own_scan` if gateway, else
   `broadcast_scan`) is a role decision, not a radio one, so signalling the finished string keeps
   that judgement where it already lives.

**Costs, stated rather than discovered:**

- `wifi_task` gains two `mode.rs`-resident call sites' worth of logic. It must not gain the *policy* —
  the OTA-active gate, the heap gate, and the `own_scan`/`broadcast_scan` routing all stay with the
  caller. If policy migrates into the task, this design has failed and the reduction should move
  back out.
- The heap gate becomes **asymmetric across the boundary**, exactly as it is today: `Publish` may be
  deferred on low heap, `Reassoc` may not. The request kind must carry that distinction, or the
  ungated path silently acquires a gate that sheds crowns.
- `alloc::string::String` in a `Signal` payload means an allocation crosses the boundary. Acceptable
  (the tree has no `heapless` dependency — `ota.rs:2540` says so explicitly, and `alloc` is present
  on every radio tier), but it is a real allocation on the OTA-adjacent path and should be sized
  before it lands, not after.

### A.5 Consequent plan changes

1. **`CrownApDecision` gains `Deferred`, as a PRE-step to STEP C** — before any scan crosses a task
   boundary. The async path adds a second "couldn't answer" source (response timeout) to the heap-skip
   the tree's warning was written about, and `NoAp` feeds the shed ladder. Small, mechanical, and it
   must not ride in the same commit as the boundary move.
2. **§4.2's payload spec is superseded** by §A.4 (corrected in place above).
3. **Q1 closes.** Its residual — "does #71 read fields `ApView` drops?" — resolves to **yes, the SSID
   string, and bssid at a different width** — and §A.4 makes it moot by never forcing the two through
   one shape.
