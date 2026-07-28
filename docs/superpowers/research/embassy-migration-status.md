# Embassy migration — status, cost, and a recommendation

**The question (JP, 2026-07-28):** *"When do we move to Embassy so we can have async?"* — prompted by
*"I'm still getting UI freezes for wifi and stuff."*

**Short answer:** the migration is **much further along than the issue tracker suggests** — a branch
exists that already carries the whole #233 matched-set upgrade *and* an async port through crown
election, and its central claim is **measured on metal, not argued** (a ~15 s mesh-deaf WiFi burst
becomes **169 ms** — ~89×). But **it is not what should ship next**, for one reason that has nothing
to do with code quality: **the fleet is currently one board**, so nothing about post-migration mesh
behaviour can be verified today, and the branch's OTA path is unverified — which means the rollback
from a bad roll is USB, by hand, per board.

**Recommendation: take the interim fix now, finish the migration deliberately behind a two-board
bench.** Detail in §6.

*Every claim below is sourced. Where I could not establish something, §7 says so rather than hedging.*

---

## 1. Where `main` actually is

| Fact | Evidence |
|---|---|
| `main` is a **synchronous superloop** | `rust/clock/src/main.rs:793` — `delay.delay_millis(SUBTICK_MS)` |
| Tick is **20 ms** | `main.rs:254` — `pub(crate) const SUBTICK_MS: u32 = 20` |
| **No Embassy anywhere in the build** | `esp-hal-embassy` appears only in `Cargo.toml`'s version-audit comment (line 25, "*unused; superloop*"). No `embassy-*`, no `esp-rtos` dependency. |
| Pinned stack | `esp-hal =1.0.0-rc.0`, `esp-wifi =0.15.0`, `esp-alloc 0.8`, `esp-storage =0.7.0`, `esp-bootloader-esp-idf =0.2.0` |

So on `main`, a WiFi burst **occupies the only thread**. That is the mechanism behind JP's UI freeze:
nothing else can run, including the display tick and the mesh service.

---

## 2. How far the port actually got — further than the tracker says

Branch **`dream/feat-embassy`**, last commit **`b6413d3`, 2026-07-22**, whose subject calls itself
*"the **FINAL** Phase-3 increment"*.

### It is a working image, not a scaffold
The strongest evidence is that **Phase 2 ran on metal and produced numbers** (§4). You cannot
instrument a deaf window on a scaffold.

### Shape of the change
| File | `main` | branch | delta |
|---|---|---|---|
| `src/net/wifi.rs` | 5,235 | 1,675 | **−3,560** |
| `src/net/mode.rs` | 6,296 | 8,024 | **+1,728** |
| `src/main.rs` | 2,109 | 2,309 | +200 |

15 files, **+3,881 / −4,642**. The bespoke WiFi burst engine was largely *deleted* and its behaviour
re-expressed as tasks in `mode.rs`. Three of them: **`net_task`**, **`wifi_task`**, **`mqtt_task`**
(`mode.rs:7226 / 7242 / 7401`).

### 🔑 The superloop does not die — it *yields*
#198's own description says *"the superloop dies, every plugin tick model changes."* **The branch did
something cheaper and lower-risk.** `main.rs:912`:

```rust
embassy_time::Timer::after(embassy_time::Duration::from_millis(SUBTICK_MS as u64)).await;
```

Same 20 ms tick, same loop, same plugin model — `delay_millis` became `Timer::after(...).await`, so
the loop **yields to the executor** instead of monopolising it. **Plugins, menu and app tick models
appear untouched**, which is why a change this large doesn't touch `snake.rs`, `bench.rs`, `batt.rs`
et al. This materially lowers the risk estimate in #198's framing, and it is the single most
important structural fact in this document.

### Ported, by increment (commit subjects on the branch)
| # | What |
|---|---|
| Phase 1 | `wifi_task` + DR-H1 controller split + ch6 hold during the WiFi window; undroppable STOP_REQ teardown |
| Phase 2 | deaf-window measurement harness, two block models, measurement-board isolation |
| 3-inc1 | uplink-only `mqtt_task` skeleton (#89 non-blocking flush) |
| 3-inc2 | own-node telemetry publish (lock-free SHARED snapshot) |
| 3-inc3a | relayed leaf-status republish (#50b) |
| 3-inc3b1/b2/b3 | downlink foundation + batt/grid (async SUBSCRIBE/drain) · keyed CONFIG (QoS0) · transient COMMANDS (QoS1 + PUBACK) |
| 3-inc3c | OTA **offer** downlink — parse → gate → `OTA_OFFER`, **explicitly "no fetch"** |
| 3-inc3d-1 → d-2 | MC election OBSERVE + PUBLISH → **RESOLVE** (`a209858`-faithful; the fleet forms and re-elects a crown) |

### Not ported / unresolved
- **The OTA fetch trigger.** ⚠️ *I nearly reported the OTA machinery as missing and was wrong* —
  `run_leaf_ota_relay` and `ServeSource::GatewayFetch` **do exist** on the branch
  (`mode.rs:5003`, `main.rs:1734`), so the #40 mesh-relay path survived the port. What is explicitly
  deferred is the **downlink-offer → fetch trigger** (inc3c: "no fetch"), and **no Phase-3 hardware
  run has been reported at all**, so whether an async-socket OTA fetch works is **unknown**. See §5 —
  this is the load-bearing unknown for the whole decision.
- **15 `TODO`/`unimplemented` markers** remain in the branch's `wifi.rs`, including
  `TODO(#198 Phase 3): embassy-net stack construction` and `TODO(#198 Phase 3): reimplement
  run_mqtt_burst as the async MQTT flush task` — both of which *appear already done* (`net_task`
  exists; `mqtt_task` exists). These read as **stale comments left by later increments**, not real
  gaps, but they need triage before anyone treats the file as a map.
- Phase 3 is **not reported complete** on #198 — the latest issue comment is *"Phase 2 … COMPLETE ✅"*.
  The tracker is behind the branch by an entire phase.

---

## 3. What blocks it — **#233 is not a prerequisite, it is already inside the branch**

This is the second surprise. #233 is described as a hard, non-piecemeal predecessor. **The branch has
already done it:**

| Crate | `main` pin | #233 target | **on the branch** |
|---|---|---|---|
| esp-hal | `=1.0.0-rc.0` | 1.1.1 | **1.1** ✅ |
| esp-wifi → **esp-radio** | `=0.15.0` | 0.18.0 | **0.18** ✅ |
| esp-hal-embassy → **esp-rtos** | `=0.9.0` (unused) | 0.3.0 | **0.3** ✅ |
| embassy-executor / -time / -net / -sync | — | — | **0.10 / 0.5 / 0.9.1 / 0.7** ✅ |

**The `esp-hal-embassy` → `esp-rtos` rename is landed and published upstream** — the branch depends on
`esp-rtos 0.3`, so this is settled, not pending.

**Why the set cannot be split** (#233, quoted): *"these CANNOT be bumped piecemeal (esp-radio links
esp-hal INTERNAL APIs; a `Rng::new()` signature change alone breaks the set)."* The canonical trap is
documented on `main` too (`Cargo.toml:16-20`): esp-wifi 0.15.x calls
`esp_hal::rng::Rng::new(peripherals.RNG)`, and esp-hal 1.0.0-rc.1+ changed `Rng::new()` to take **no
argument**, so the pair fails to compile while `cargo semver` still passes.

⚠️ **Two members of that set are OTA-critical and deserve their own gate:** `esp-storage` 0.7 → 0.9
(*"otadata/flash write API changed"*) and `esp-bootloader-esp-idf` 0.2 → 0.5 (*"otadata slot/state API
changed"*). Given [smol's flash-write word-alignment history](../../ota.md) and that
**revert-on-boot-fail is off**, an otadata API change is the highest-consequence line in the table.

**So the real blocker is not dependency work. It is verification.** See §5.

---

## 4. What it actually buys — measured, with the boundary stated

### 🟢 The measured win (Phase 2, on metal, 2-board bench, defmt RTT — #198)
`steady_max_gap` = longest mesh-deaf stretch:

| Run | Condition | Deaf window |
|---|---|---|
| Run-0 | control, no WiFi window | **279 ms** (ambient fleet-flood floor) |
| Run-1 | **async**, co-channel (ch6) WiFi window held | **169 ms** — mesh alive *through* the window |
| Run-3-skip | blocking, mesh-service suppressed | **~15,000 ms** |
| Run-3-spin | blocking, executor held | **~15,000 ms** |

**~15 s → 169 ms, about 89×.** Robust: the blocking baseline lands at ~15 s under *both* emulations,
so the result does not depend on how blocking was modelled.

### What that does and does not fix — three different problems, often conflated

| Problem | Does the executor fix it? |
|---|---|
| **JP's UI freeze** | **Yes, directly.** The freeze is the display tick starving while a blocking WiFi burst owns the only thread. `Timer::after().await` lets the loop keep rendering between await points. This is the same root cause as the deaf window, seen from the user's side. |
| **Mesh deaf-window during a burst** | **Yes — 89×, measured.** Not eliminated: 169 ms remains, against a 279 ms ambient floor. |
| **The single-radio, single-channel constraint** | **No. This is physics, not scheduling.** One radio is on one channel at a time; off-channel WiFi work makes the mesh deaf no matter who schedules it. That is what #23's **co-channel coexist** addresses (keep WiFi and the mesh on the same channel), and it already shipped on `main`. |
| **Crown unicast-RX starvation** | **No.** **#204 (OPEN)** — a crown under bulk inbound goes downstream-deaf within ~1 ms of its own transmit. Reproduces identically on the new esp-radio 0.18 stack, so **the radio rewrite is not the cure** and neither is the executor. |
| **The DRAM ceiling** | **No — that is the C6, not Embassy.** The ROADMAP pairs #198/#233 with "the C6 dissolves the DRAM problem"; the dissolving agent is **512 KB of SRAM on different silicon**, not the async model. Keep the two claims apart. |

> 📌 **"#53" is overloaded in this repo — cite the study by path, not by number.** The brief for
> this task attributed the co-channel physics finding to *"#53's finding,"* and my first draft
> "corrected" that as simply wrong. **Both were half right.** There are two #53s:
> - **GitHub issue #53** = *"battery display shows staleness instead of blanking"*, closed 2026-07-12
>   as *"i don't care about that battery staleness issue."* Unrelated.
> - **[`coexist-disease-esp-radio-018-study.md`](coexist-disease-esp-radio-018-study.md)**, whose own
>   title is *"#53 — the #198-fix question."* **This is the real source**, and it is the authority for
>   the boundary above.
>
> The study is worth reading in full before anyone re-argues this, because its conclusions are
> stronger than a summary: *"the `coex` feature is the wrong mechanism"* — coexistence arbitrates
> Wi-Fi ↔ BT/BLE ↔ 802.15.4, and **ESP-NOW is not a coex participant because ESP-NOW *is* Wi-Fi**
> (vendor action frames on the Wi-Fi MAC), so the disease is Wi-Fi-vs-itself on one MAC and `coex`
> *cannot* arbitrate it; and *"the off-channel variant is RF physics, not software"* — the radio
> cannot change channel while associated, so **no stack upgrade removes a one-radio/one-channel
> constraint.** Its bottom line is explicit: **#198 should not be reprioritised as "the OTA fix."**
> This document agrees, and §4 is that finding applied to JP's question.
>
> The live coexist residual is **#204**. Lesson for the docs: an internal study numbered like a GitHub
> issue collides with it — always cite the path.

---

## 5. Risk — and the thing that actually decides this

This is a **full re-platform of a fleet that currently works**, and three facts stack badly:

1. **The fleet is one board.** As of 2026-07-27/28 the crown (Nexus, **build 906**) reports **no
   peers** (team-lead, from the bench). **Therefore every mesh claim about the migrated firmware is
   untestable today** — including the one that justifies the migration. Phase 2's own numbers required
   a **2-board** bench (DUT + a dedicated ~50 ms ch6 beacon source). *This document deliberately
   promises no verification that nobody can currently perform.*
2. **The OTA path on the branch is unverified** (§2). If a migrated image is rolled and misbehaves,
   recovery is **USB, by hand, per board** — and per [ota.md](../../ota.md) the bootloader's
   revert-on-boot-fail is **off**, so app-side rollback plus canary discipline is the entire safety
   net. A re-platform is exactly the change most likely to break app-side rollback.
3. **`esp-storage` and `esp-bootloader-esp-idf` change their otadata APIs** in the same set (§3). The
   OTA path is being rewritten *and* its dependencies are moving *and* its verification is missing.

### What "half-migrated" looks like
Better than feared, because of the yielding superloop (§2): the app/plugin layer is shared, so a
half-migrated tree is not two firmwares. But **the fleet cannot be half-migrated across boards**: the
mesh, election and OTA-relay wire contracts are shared, and a migrated crown serving unmigrated leaves
is untested. Treat the fleet as all-or-nothing per roll; treat the *tree* as safely incremental.

### Rollback
`main` is intact and unmodified — the branch is not merged. Build 906 is the running image, and the
`main` lineage is reproducible (#44). Rollback of the *code* is free. Rollback of a *rolled fleet* is
the expensive direction, and is the risk being managed.

---

## 6. Recommendation

**Do not merge or roll the migration yet. Take the interim fix first.** In order:

### Step 1 — the interim fix (days, not weeks) · **do this now**
Service the app during burst yields (aurora is already evaluating it). If it removes most of the felt
freeze, JP's actual complaint is answered for a fraction of the cost, **and it does so on the fleet
image that is already proven**. Measure the freeze before and after — `SUBTICK_MS` starvation is
observable without a second board, which is precisely what makes this step available today when the
migration's verification is not.

### Step 2 — restore a two-board bench · **the real gate**
Nothing about the migration can be honestly signed off on one board. This is a hardware/logistics
task, not a code task, and it blocks Step 3 rather than being part of it. Phase 2's rig is the
template: DUT + a dedicated ch6 beacon source.

### Step 3 — finish and gate the branch, in this order
1. **Triage the 15 stale `TODO`s** so the file is a map again (cheap; likely mostly deletions).
2. **Port + verify the OTA path** — offer → fetch → verify → activate → boot-confirm → **app-side
   rollback**, on hardware, before anything else. Until this is green the branch is not rollable, and
   everything else is moot.
3. **Re-run Phase 2's harness** on the completed Phase 3 to confirm the 169 ms result survives the
   full task set (election + MQTT + downlink now share the executor; the 169 ms was measured with
   less running).
4. **Canary one board**, then hold. Per [ROADMAP §3a](../../ROADMAP.md), canary-one-board is
   mandatory, and it matters most here.

### Sizing, honestly
| Step | Effort | Confidence |
|---|---|---|
| 1 — interim fix | days | high — small, local, reversible |
| 2 — two-board bench | a bench session | high, but needs a human and hardware |
| 3.1 — TODO triage | hours | high |
| 3.2 — OTA port + HW verify | **the unknown** — could be days, could expose a real problem | **low.** No Phase-3 HW run exists; this is where a re-platform usually hurts |
| 3.3 — re-measure | a bench session | medium |
| 3.4 — canary + fleet roll | days of soak | medium |

### Should we just wait for the C6?
**No — the two are not substitutes, and treating them as one is the mistake to avoid.** The C6
(#229/esp32c6-watch) solves **DRAM** (512 KB SRAM) and already runs Embassy. It does **not** fix the
C3 fleet's UI freeze, and the C3 boards are the fleet. So: the C6 is the answer to *"we are out of
RAM"*; Embassy-on-C3 is the answer to *"the UI freezes."* **JP asked the second question.** The C6
work does, however, de-risk Step 3 — every Embassy pattern proven on the watch is one smol does not
have to discover.

### The honest bottom line
The migration's benefit is **real, large and measured** (89×), and the port is **most of the way
done** with a lower-risk shape than its own issue predicted. This is not a "don't do it." It is a
**"don't do it blind on a one-board fleet with an unverified OTA path."** The gap between here and
shippable is verification, not code.

---

## 7. What I could not establish

Stated rather than hedged:

- **Does the branch compile today?** Not verified — building is outside this task's remit (docs only)
  and `rust/` has a live agent. Its last commit describes a coherent atomic change, and Phase 2 ran on
  metal, but *"compiles at `b6413d3` on the current toolchain"* is unconfirmed. **Check this first.**
- **Are the 15 `TODO`s stale or live?** They *look* stale (they ask for `net_task`/`mqtt_task`, which
  exist). Not proven.
- **Does the async OTA fetch work?** Unknown, and the highest-value unknown in the document.
- **`origin/feat/233-upgrade-wave`** (last 2026-07-20) vs the branch's bundled upgrade: not diffed.
  Since `dream/feat-embassy` already carries the matched set, the older branch may be redundant —
  worth confirming before anyone invests in it.
- **The one-board fleet state** is team-lead's bench observation (2026-07-27/28), taken on trust and
  attributed; I did not read the broker myself.
- **`SUBTICK_MS` starvation has no measured number on `main`.** Phase 2 measured the *mesh* deaf
  window; the *UI* freeze JP reports is inferred from the same mechanism, not separately timed.
  Step 1 should measure it, so the interim fix can be judged on evidence rather than on feel.

---

*Author: Nebula, 2026-07-28. Sources: `git log`/`git show` on `main` and `dream/feat-embassy`;
issues #198, #233, #204, #53; `rust/clock/Cargo.toml`. Verification discipline per
[docs/DOC-UPKEEP.md](../../DOC-UPKEEP.md) — in particular that a measured number carries its unit and
its subject, and that a brief is a lead rather than a source.*
