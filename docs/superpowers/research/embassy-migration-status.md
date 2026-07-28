# Embassy migration — status, cost, and a recommendation

**The question (JP, 2026-07-28):** *"When do we move to Embassy so we can have async?"* — prompted by
*"I'm still getting UI freezes for wifi and stuff."*

**Short answer — and it inverted on 2026-07-28.** Async is no longer a C3 migration to finish. It is a
**C6 platform transition**, because **Embassy does not fit on the C3 alongside the Bard.**

> **`Embassy costs 58,144 B` of stack. There are `2,232 B` of slack.**
> Measured, not projected: `.bss+.data+.stack` sums to **285,708 B for both `main` tiers exactly**, so
> `.stack` is purely the leftover of a fixed pool — which makes the deltas clean. The Bard costs
> **39,080 B**; Embassy costs **58,144 B**. Projected `main` + bard + Embassy `.stack` is
> **≈17,800–24,600 B** against a **73,728 B floor** *and* a **measured 54,960 B peak** — so the image
> would **link and then die on hardware**. **Every lever spent to its limit is ≈39.5 KB, still 10–16 KB
> short.** There is no combination of `SEQ_CAP`, heap and RX-buffer tuning that closes it.

So the earlier reading of this document — *"one blocker: the OTA path is unverified"* — was **twice too
kind**, and both corrections are in §5:

1. The OTA path is not unverified, it is **unwritten** (fed by a stub), which makes the
   [verification plan](../plans/embassy-ota-verification.md) **unrunnable against `b6413d3`**.
2. And it would not matter if it were written: **the resulting image cannot run on a C3 that also
   carries the Bard.**

**What survives, and it is a lot.** The measured win is real and unchanged — a ~15 s mesh-deaf WiFi
burst becomes **169 ms (~89×)**, on metal. The #233 matched-set upgrade is done and **builds**. The port
reaches crown election. **None of that is wasted: it is the C6's head start**, and the C6 has 512 KB of
SRAM, which dissolves the constraint rather than negotiating with it.

**Recommendation: stop treating #198 as a migration-in-progress.**
1. **Keep the interim fix** — it already addressed the freeze on the proven image (`443ea34`), without
   Embassy.
2. **Retarget async at the C6** (#229/esp32c6-watch), which already runs this exact stack in the field.
3. **Do not spend more levers on the C3.** The gap is 10–16 KB *after* everything; that is a platform
   answer, not a tuning answer.
4. **BLE follows the platform.** #22's *"embassy/async is the only supported coex shape"* still holds —
   so BLE arrives with the C6 too, and is not a reason to force Embassy onto the C3.

⚠️ **Anything still describing #198 as a migration in progress is now wrong**, including issue text and
any plan that sequences a C3 fleet roll.

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

> # 🔴 Both bullets below were WRONG. Corrected 2026-07-28 — see [embassy-p2-mesh-relay.md](embassy-p2-mesh-relay.md)
>
> 1. **The relay "survived the port" — as UNPORTED BLOCKING CODE.** `run_leaf_ota_relay` on the branch
>    is still a `pub fn` taking the same `tick: &mut dyn FnMut() -> bool` (branch `mode.rs:5003`); there
>    are **8 `async fn` in 8024 lines**. "It exists" was true and read as "it was migrated." It wasn't.
> 2. **`run_ota_fetch` is a STUB returning `false`** (branch `wifi.rs:1659-1675`, *"STUBBED (OTA HTTP
>    fetch -> embassy-net in **Phase 5**)"*). So `ServeSource::GatewayFetch` **always** returns
>    `FetchFailed`, and the async OTA fetch is not *unverified* — **it does not exist.** No Phase 5 plan
>    exists in `docs/`.
> 3. **The TODO triage below was over-generalised.** `run_mqtt_burst` *is* genuinely superseded by
>    `mqtt_task` — that observation was correct. It was then extended to **all** the markers, including
>    `run_ota_fetch` and `try_time_sync` (NTP — *"clock free-runs"*), which are **real gaps**. Exactly
>    DOC-UPKEEP §2: a correct fact attached to the wrong scope.
>
> **Net effect on this document:** §5's "one blocker: the OTA path is unverified" is too kind. The OTA
> path is **unwritten**, and the [verification plan](../plans/embassy-ota-verification.md) is
> **unrunnable** against `b6413d3`.

- **The OTA fetch trigger.** `run_leaf_ota_relay` and `ServeSource::GatewayFetch` **do exist** on the
  branch (`mode.rs:5003`, `main.rs:1734`) — but see the correction above: unported, and fed by a stub.
  What is explicitly deferred is the **downlink-offer → fetch trigger** (inc3c: "no fetch"), and **no
  Phase-3 hardware run has been reported at all**.
- **15 `TODO`/`unimplemented` markers** remain in the branch's `wifi.rs`. Three are **real stubs**
  (`try_time_sync`, `run_mqtt_burst`, `run_ota_fetch` — branch `wifi.rs:707/909/1673`); of those only
  `run_mqtt_burst` is genuinely superseded by `mqtt_task`. Triage the rest before treating the file as
  a map.
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

> 🎁 **And one long-standing unknown can now be settled for free.** [ROADMAP §3a](../../ROADMAP.md) and
> [ota.md](../../ota.md) both record that **bootloader revert-on-boot-fail is UNPROVEN** — the hardware
> test was never run. It turns out the firmware already probes it: `ota.rs:1056` sets
> `bl_auto_revert = matches!(state, OtaImageState::PendingVerify)`, because **the bootloader only
> promotes `New → PendingVerify` when its rollback config is ON.** So the *first* branch OTA that logs
> its boot state answers the question from the board itself, with no separate experiment. Recorded as a
> deliberate capture in [the verification plan](../plans/embassy-ota-verification.md) §6b — it would be a
> shame to run the campaign and not write that number down.

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

### 🔑 The one rule that settles every "does async fix X?" question

State it once and apply it everywhere, because almost every wrong claim in this area is a failure to
apply it:

> ## **Async changes whether other work can run while the radio is busy.**
> ## **It does not create a second radio.**

Everything the executor fixes is on the first line. Everything it cannot touch is on the second. The
deaf-window win, the co-channel constraint, and the BLE-scan question are all the *same* question
asked three times — and the rule answers all three without re-deriving anything.

### What that does and does not fix

| Problem | Does the executor fix it? |
|---|---|
| **JP's UI freeze** | **Yes, directly.** The freeze is the display tick starving while a blocking WiFi burst owns the only thread. `Timer::after().await` lets the loop keep rendering between await points. This is the same root cause as the deaf window, seen from the user's side. |
| **Mesh deaf-window during a burst** | **Yes — 89×, measured.** Not eliminated: 169 ms remains, against a 279 ms ambient floor. |
| **The single-radio, single-channel constraint** | **No — second line of the rule.** One radio sits on one channel at a time; off-channel WiFi work makes the mesh deaf no matter who schedules it. #23's **co-channel coexist** is the fix, and it already shipped on `main`. |
| **Crown unicast-RX starvation** | **No.** **#204 (OPEN)** — a crown under bulk inbound goes downstream-deaf within ~1 ms of its own transmit. Reproduces identically on the new esp-radio 0.18 stack, so **the radio rewrite is not the cure** and neither is the executor. |
| **BLE at all, on the Rust firmware** | **Yes — and it is a whole capability, not a nicety (benefit 3 in §6).** #22 (CLOSED, verdict confidence *high*) refuted native BLE on the blocking runtime: **ROM busy-waits in btdm init / PHY calibration under *every* init order**, at 3 hardware-distinguished hang points, 1 day of spike. Its own conclusion names the exit: ***"embassy/async is the only supported coex shape."*** Deliverables are already banked on `feat/22-ble-observer` — a host-tested HCI codec + `SightingTable` — so this is a resumption, not a fresh start. |
| **BLE beacon — ADVERTISE** (the node *is tracked*) | **Yes, first line of the rule.** Brief periodic adverts fit the burst duty cycle, and room-level presence comes from external fixed anchors (Bermuda/HACS). **This is the real BLE win.** |
| **BLE PROXY / continuous SCAN** (the node *tracks others*) | **No — second line of the rule.** A proxy must listen ~continuously; a board that is also meshing, bursting and running a game is a **lossy part-time scanner**, and no executor conjures the airtime. #22: *"better left to dedicated always-on nodes."* The shipped answer stays **ESPHome `bluetooth_proxy` on a spare ESP32**, consumed by the #75 dollhouse epic. |
| **The DRAM ceiling** | **No — that is the C6, not Embassy.** The ROADMAP pairs #198/#233 with "the C6 dissolves the DRAM problem"; the dissolving agent is **512 KB of SRAM on different silicon**, not the async model. Keep the two claims apart. |

> 📌 **The BLE "no" lost one of its two legs — and still holds.** Worth stating precisely, because it
> is exactly the case [DOC-UPKEEP](../../DOC-UPKEEP.md) means by *when a premise expires, check the
> conclusion before deleting*. The original refusal of proxy/metric BLE rested on **two** reasons:
> *"single radio **+ the multi-second WiFi hold** preclude it"* ([ROADMAP §4b](../../ROADMAP.md)).
> **#23 retired the second leg** — the multi-second hold is gone — and the migration shrinks what
> remains of it to ~169 ms. **The first leg is load-bearing and untouched.** So the verdict survives,
> for a narrower and cleaner reason: not *"the radio is away for seconds at a time"* but simply
> *"there is one radio."* Do not read the retired half as a reason to revisit the conclusion; do read
> it as the reason **advertise** got easier while **scan** did not.

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

This is a **full re-platform of a fleet that currently works.** When this section was first written
three facts stacked badly; **two have since resolved, and the remaining one is the whole decision.**

1. ~~**The fleet is one board.**~~ ✅ **RESOLVED 2026-07-28 — a two-board bench now exists.** JP
   plugged in three more; verified from the crown's peer view and *fresh* telemetry, not retained
   ghosts:
   **Four boards live**, from the crown's ESP-NOW peer attribute + fresh telemetry:
   **id8 Nexus** (interim fix `443ea34`) · **id5 Aegis** (906 — self-fetched over WiFi and rebooted into
   it; HA confirms `installed=906`) · **id50 Ember** (906) · **id51 Sigil** (mid-OTA), plus **id122**,
   a rig id.

   > 📌 **Correction to an earlier reading of this same evidence** — kept because it is the reason the
   > "one board" claim existed at all. id7 and id9 were reported absent; they are neither absent nor
   > present, because **those ids do not exist on the air.** That hardware has run as **id50/id51 since
   > 2026-07-22**, re-provisioned for #198's own Phase-2 measurement work. Their HA entity families kept
   > answering while frozen, which read as *"boards dead"*; and the roster was read from
   > `sensor.smol_8_peers`'s **state** (only the role) rather than its **`peers` attribute**. Both traps
   > are now in [DOC-UPKEEP](../../DOC-UPKEEP.md) §2–3; the identity record is in
   > [BUILDING.md](../../BUILDING.md).

   > 🔎 **Why that last inference is sound, and worth reusing:** absent *telemetry* is weak evidence —
   > it could be the broker, the WiFi leg, or a retained ghost. Absence from the **ESP-NOW peer list**
   > is strong: ESP-NOW needs **no router, no DHCP, no broker**, so a booted smol is seen within
   > seconds. *"Not in the peer list"* therefore means *"not running"*, where *"no telemetry"* only
   > means *"something in a long chain is broken."* Prefer the shortest-chain signal when deciding
   > whether a board is alive.

   So **Phase 2's harness is runnable again** — with a pleasing circularity: **the boards it needs are
   the very boards Phase 2 re-provisioned to be its rig.** Step 2 is off
   the critical path. This does **not** soften the recommendation — see below.
2. **The OTA path on the branch is unverified** (§2). **This is now the only blocker, and it is the
   one that matters.** Note the pointed contrast the bench just supplied: **OTA demonstrably works on
   `main`** — id5 self-fetched 906 over WiFi and came back on it, today. On the branch the same path
   has never been exercised. So the risk is not "OTA is hard"; it is "**this** OTA is untested, and it
   is the mechanism by which you would undo a bad roll." If a migrated image is rolled and misbehaves,
   recovery is **USB, by hand, per board** — and per [ota.md](../../ota.md) the bootloader's
   revert-on-boot-fail is **off**, so app-side rollback plus canary discipline is the entire safety
   net. A re-platform is exactly the change most likely to break app-side rollback.
3. **`esp-storage` and `esp-bootloader-esp-idf` change their otadata APIs** in the same set (§3) — and
   the branch now demonstrably *compiles* against `esp-bootloader-esp-idf 0.5.0` (§7), which proves the
   API port, **not** that otadata behaves. The OTA path is being rewritten *and* its dependencies are
   moving *and* its verification is missing. Two of those three are fine; the third is the gap.

### 🔑 There is a living reference for the whole stack — the unknowns are C3-and-mesh-shaped

`~/Projects/esp32c6-watch` **already runs this stack in the field**: `esp-rtos 0.3.0`, `esp-radio 0.18`,
`esp-hal ~1.1`, `embassy-executor 0.10.0`, `esp-storage 0.9.0`, `esp-bootloader-esp-idf 0.5.0` — the
branch's set, on JP's own hardware. (Read-only for smol; its remote is **wakizashi only**.)

**That reframes the risk.** The open questions are **not** *"does Embassy work on this stack"* — a
shipping smartwatch answers that. They are:

1. **C3-shaped** — different bootloader quirks and a different partition table from the C6.
2. **Mesh-shaped** — the watch has **no ESP-NOW OTA relay**, no crown election to port, no fleet to
   keep alive during a burst. Everything smol-specific is exactly the part with no reference.

> 📌 **Correction, 2026-07-28 — point 2 said "no crown election, no fleet, and nothing mesh-shaped at
> all." Too strong on the middle claim.** The watch **does** do ESP-NOW under Embassy
> (`src/net/smol_mesh.rs`, 896 L, `esp_radio::esp_now`), including a fragmented + ACKed +
> retransmitted `RELAY`/`RELAYACK` transfer that its own comment calls a *"byte-exact port of
> `wire.rs encode_relay`"* — and, decisively, it has **the radio-arbitration verdict written down**:
> `mesh_pin_ok = radio_started && !connected && !connecting && !scanning && !scan_pending &&
> !assoc_want() && ota.is_none()` (`net_task.rs:563-569`), *decided* in the task and *executed* in main
> because **the mesh owns the `esp_now` handle** (`main.rs:2543`). That last term — `ota.is_none()` — is
> precisely the invariant smol currently gets for free from blocking.
> **What survives point 2:** no reference for an **OTA** relay, and none for the **scale** (≤4 frags /
> ~15 s on the watch vs 6,237 chunks / 98 windows / minutes on smol). **What changes:** the hardest
> *architectural* question has a working answer next door. See
> [embassy-p2-mesh-relay.md](embassy-p2-mesh-relay.md) §2.

So every Embassy *pattern* smol needs has been solved once next door, and every smol *mesh* behaviour has
not. Budget accordingly: the framework risk is much lower than a from-scratch re-platform, the
integration risk is unchanged. Details and the concrete reading list are in
[plans/embassy-ota-verification.md](../plans/embassy-ota-verification.md) §0b — including the **critical
brick** the watch hit on this very API, which smol has an issue for already (#226).

### Open question — are the watches already on the mesh?

**The watches speak SMOLv1**, so they are candidate mesh *peers*, not merely a reference codebase. Two
unexplained ids are in play, and there is a mechanism that makes the connection plausible rather than
speculative: the watch treats **`node_id == 42` as a sentinel meaning "derive the id from the MAC"**
(`src/main.rs:1131`). So a watch in the field reports **whatever its MAC derives to** — which makes an
unattributed roster entry like **id122 (*Celestial Crown*)** a plausible watch rather than a rig board,
and `id42` a watch that never got configured. **Unresolved — do not assume either way**; settle it by
matching the roster MAC against the watches. Flagged because a mesh peer nobody has accounted for is a
variable in every mesh measurement taken from now on.

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

### Step 1 — the interim fix · ✅ **LANDED 2026-07-28 while this document was being written**
`443ea34` — *"keep the active screen alive (and the button honoured) through a WiFi burst."* It also
**corrected the diagnosis in a way that matters here**, so the mechanism section above (§1) is right
about the cause but was wrong about the symptom:

> In steady state **nothing paints a clock — the screen is frozen on the last app frame, deliberately**
> (#153: *"a routine burst → draw NOTHING; the last app frame stays frozen on the glass (a still clock
> beats a spinner)"*). The clock-instead-of-your-screen behaviour is **boot-only** (#89 Stage 1
> prologue). So the freeze JP feels is not starvation *painting the wrong thing* — it is a **deliberate
> no-paint policy** that reads as a crash now that the Bard is on the glass: a frozen typewriter looks
> wedged. Same paused-vs-wedged confusion the `|| paused` blink exists to prevent, arriving from the
> other direction.

Sites audited in that commit: boot prologue (paints a clock, boot-only), leaf re-election (nothing),
**telemetry flush (nothing — the ~30 s one JP actually feels)**, NTP re-sync (nothing), OTA self-fetch
and OTA relay ×2 (correctly paint progress). The three "nothing" sites were the targets.

**Consequence for this recommendation:** benefit 1 is now **substantially addressed on the proven fleet
image, without the migration** — exactly the outcome this step was there to test for. That does not
weaken the case for migrating; it **removes the urgency argument** and leaves benefits 2 and 3 (§below)
to stand on their own merits, which is a healthier basis for a re-platform decision than "the UI is
annoying."

⚠️ Still worth doing: **measure it.** `SUBTICK_MS`-era freeze duration has never been timed on `main`
(§7), so "substantially addressed" is currently a code-reading, not a measurement — and it is
observable without a second board, which is what made this step available when the migration's
verification was not.

### Step 2 — restore a two-board bench · ✅ **DONE 2026-07-28, off the critical path**
Nothing about the migration could be honestly signed off on one board. **id5 Aegis and id8 Nexus are
both live** (§5) alongside **id50 Ember** and **id51 Sigil**, which *are* Phase 2's rig boards, so its harness is runnable again. This was
a hardware/logistics gate, not a code task, and it has cleared without anyone writing code for it.

⚠️ **This does not advance the decision by itself.** It removes an *excuse* for not verifying; it does
not perform the verification. Step 3.2 is still the gate.

### Step 3 — finish and gate the branch, in this order
1. **Triage the 15 stale `TODO`s** so the file is a map again (cheap; likely mostly deletions).
2. **Port + verify the OTA path** — offer → fetch → verify → activate → boot-confirm → **app-side
   rollback**, on hardware, before anything else. Until this is green the branch is not rollable, and
   everything else is moot. **The procedure now exists:**
   [plans/embassy-ota-verification.md](../plans/embassy-ota-verification.md) — A/B against a same-day
   `main` control, both paths separately, `tools/ota_verify.sh` as the oracle, an action per failure
   mode, and a stop rule. It also names the steps it *cannot* specify from docs alone (app-side
   rollback is the load-bearing one).
3. **Re-run Phase 2's harness** on the completed Phase 3 to confirm the 169 ms result survives the
   full task set (election + MQTT + downlink now share the executor; the 169 ms was measured with
   less running).
4. **Canary one board**, then hold. Per [ROADMAP §3a](../../ROADMAP.md), canary-one-board is
   mandatory, and it matters most here.

### Three benefits, not one — which changes the sizing conversation

The migration is easy to mis-price as *"a large re-platform to fix a UI freeze."* It is not. It gates
**three** things, and JP should see them added up rather than have to add them up himself:

| # | Benefit | Status | Caveat |
|---|---|---|---|
| 1 | **App responsiveness during bursts** — JP's actual complaint | ✅ **substantially addressed WITHOUT the migration** — `443ea34`, 2026-07-28 | so this benefit **no longer argues for migrating at all**. Not yet measured (§7) |
| 2 | **The mesh deaf-window** | 🟢 **measured: ~15 s → 169 ms, ~89×** | not eliminated; 169 ms against a 279 ms floor |
| 3 | **BLE at all on the Rust firmware** (#22) | 🔓 unblocked — *"embassy/async is the only supported coex shape"*, deliverables banked on `feat/22-ble-observer` | **advertise only.** Proxy/continuous scan stays refused on the second line of the rule |

Benefit 2 is the one with a number on it. Benefit 3 is the one that is otherwise **unreachable** — no
amount of interim polish on the superloop delivers BLE, because #22's hang is in ROM busy-waits under
every init order.

**And Step 1 has now landed, so this is no longer hypothetical:** benefit 1 came without the migration.
The trade to put in front of JP is therefore **narrower and clearer than when he asked**: the migration
is no longer the answer to *"my UI freezes"* — it is the answer to *"I want the mesh to stay alive
through a burst"* (measured, 89×) and *"I want BLE"* (otherwise impossible). If he wants neither of
those yet, **the honest answer is: not now.**

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

- ~~**Does the branch compile today?**~~ ✅ **ANSWERED 2026-07-28 — yes.** Built by team-lead in an
  isolated worktree at `b6413d3`: `cargo build --release --features espnow,cast,io` → **Finished in
  30.12 s**, clean, with `esp-bootloader-esp-idf 0.5.0` and `embassy-net 0.9.1` compiling. So the whole
  #233 matched-set port is not merely written, it **builds**. The remaining unknowns are all
  **behavioural**, which is a much better place to be than "we don't know if it compiles."*
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
