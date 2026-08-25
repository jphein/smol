# RISKS — Embassy Phase 1 onto `main` @ `9c36a25`

Ordered by *(probability × cost of late discovery)*. Each entry says what it is, why the existing gates do **not** catch it, and what would.

The organising principle: **every gate this repo owns runs on an ELF or a host.** Phase 1 changes the *runtime*, and an ELF cannot show a runtime. That gap is R1–R3 and it is the whole reason `[[stack-is-not-headroom]]` and `[[gate-that-cannot-fail]]` are in the memory index.

---

## R0 — 🔴 **A live hazard already shipped on `main`: the OLED I2C bus has no timeout.** Not a port risk — fix it now.

Found while diffing the reference's Phase-0c′ work. **This is not caused by the Embassy port; it is already on `main` and already on the fleet (v917).**

**`main`** (`rust/clock/src/main.rs:78`, `:652–655`):
```rust
i2c::master::{Config as I2cConfig, I2c},      // no BusTimeout, no SoftwareTimeout imported
…
I2cConfig::default().with_frequency(Rate::from_khz(400)),   // ← no timeout of any kind
```

**Reference branch** (`main.rs:163`, `:532–535`) — sets both:
```rust
I2cConfig::default()
    .with_frequency(Rate::from_khz(400))
    .with_timeout(BusTimeout::BusCycles(24))
    .with_software_timeout(SoftwareTimeout::PerByte(Duration::from_millis(1))),
```

**Verified against the crate source, not the migration notes** — `~/.cargo/registry/src/…/esp-hal-1.1.1/src/i2c/master/mod.rs:612–630`:
```rust
impl Default for Config {
    fn default() -> Self {
        Config { frequency: Rate::from_khz(100),
                 #[cfg(i2c_master_has_bus_timeout_enable)] timeout: BusTimeout::Disabled,
                 software_timeout: SoftwareTimeout::None,   // ← the one that matters
                 … } } }
```
and `:2301–2310`, `:2312–2331`: `wait_for_completion_blocking(deadline: Option<Instant>)` runs a bare `loop { … }`, and `all_commands_done` only gives up inside `if let Some(deadline) = deadline && now > deadline`. **With `deadline = None` there is no exit.** A stuck or clock-stretched SCL therefore hangs the per-tick display flush **forever** → WDT / boot-loop.

**Why nobody noticed:** the old esp-hal rc.0 default supplied a ~1 ms/byte software timeout; the 1.1 default removed it. The drift is entirely in **private default field values**, so the #233 dep wave (PR #361) compiled clean and silently changed the behaviour. A headless NACK still returns fast, so a bench smoke test looks fine — the hang needs a *stuck* bus (half-seated connector, ESD, wedged panel).

**This is `[[zero-conflicts-raises-risk]]` exactly:** a dependency-wave bump where git had nothing to flag and the API arity never changed, so nothing forced a look.

**Action:** port the two `.with_*` lines to `main` as a **standalone fix commit now**, ahead of and independent of the Embassy work. It is 2 lines and 1 import. Bench-verify by holding SCL low mid-transaction: the flush must return an error and the board must not reset.

---

## R1 — Executor ON changes preemption. This is the links-but-dies step. **[highest]**

Today `esp_rtos::start(timg0.timer0, sw.software_interrupt0)` is called **late and from two mutually-exclusive radio-init paths** — `net/wifi.rs:510` and `net/mode.rs:2346` — with `esp-rtos` features `["esp32c3","esp-radio","esp-alloc","log-04"]`, i.e. **the embassy executor OFF**. `main` is `#[main] fn main() -> !` (`main.rs:576–577`) running on the linker-defined stack, and radio futures are driven by `embassy_futures::block_on` (23 `self.controller` sites, `mode.rs`).

Phase 1 makes `main` an `async fn` under `#[esp_rtos::main]`, hoists `esp_rtos::start` to the top of boot, and turns `embassy` on. Three things change at once:

1. **`main` stops being the bare-metal entry and becomes a scheduler task.** Anything that assumed the linker stack region *is* main's stack becomes an assumption, not a fact (→ R2).
2. **`block_on` inside an async task is now a *nested* executor.** Main keeps `embassy-futures` through P1.1–P1.4 by design (PORT-SPEC §2.1), so during the transition a `block_on(self.controller.disconnect_async())` runs on a task that the outer executor believes is running. Whether esp-rtos's scheduler preempts that busy-loop to run `net_task` — or deadlocks against it — is **not established by any document I read**, and it is the single most likely "it links, it boots, it wedges" failure.
3. **Priority/preemption between the esp-radio scheduler task and the embassy executor** is now a real relationship rather than a degenerate one.

**Why the gates miss it:** all 21 `tools/gate.sh` steps are `cargo check`/`clippy`/host-test/`readelf`. Not one runs the firmware. `[[gate-that-cannot-fail]]` is the standing lesson.

**What would catch it:** the P1.2 image on one board, with `BurstProbe` output captured (§R9) and the `6b94bb9` canary log line present. Land P1.2 **alone**, not fused with P1.4/P1.5.

---

## R2 — The floor gate and `stack_paint` both measure the *main* stack only, and Phase 1 moves the stacks that matter **[high]**

`tools/repro_build.sh:repro_stack_check` computes `_stack_start − _stack_end` from `readelf`, against `ESP32C3_STACK_FLOOR_BYTES = 74_208` parsed out of `rust/clock/src/budget.rs:197`. That floor is derived — `budget.rs:206`/`:213` hold `ESP32C3_MEASURED_PEAK_BYTES = 55_656` and a `const _: () = assert!(floor >= peak * 4 / 3)` binding them (verified; 55,656 × 4/3 = 74,208 exactly).

The gate's own header already says the quiet part (`tools/repro_build.sh:107–112`):

> "⚠️ The floor bounds the linked REGION, which is all an ELF can show. It cannot see runtime high-water: a struct that lives in a stack-resident `RadioManager` costs real stack and moves this number by almost nothing (#181's `LedgerLink` = 1,760 B on target, but only −32 B of region). So a PASS here means 'the region is not absurdly thin', NOT 'the image has stack headroom'."

**Phase 1 makes this worse in a way the wording does not yet cover.** `net_task`, `wifi_task` (and later `mqtt_task`) get their stacks from the embassy/esp-rtos task arena — **not** from `[_stack_end, _stack_start)`. So after the split:

- the gate keeps measuring one stack out of three or four, and **passing** says even less than it does today;
- `stack_paint.rs` paints `[_stack_end, sp − MARGIN)` using the same two linker symbols (`stack_paint.rs:40–56`), so the `stack-paint` tier measures the **main task only** and is structurally blind to the new tasks;
- therefore `ESP32C3_MEASURED_PEAK_BYTES = 55_656` — the input the whole floor is derived from — **changes meaning** under Phase 1. It was "the deepest path in the program". It becomes "the deepest path in one of four tasks".

**This is `[[stack-is-not-headroom]]` with a new failure surface, and nothing in the tree currently measures the new surface.**

**Action:** treat "extend stack-paint (or an equivalent sentinel) to the spawned tasks' stacks" as a **Phase-1 deliverable, not a follow-up**. Until it exists, no Phase-1 image should be described as having measured stack headroom. Re-derive `ESP32C3_MEASURED_PEAK_BYTES` under live radio after the split and update `budget.rs` — the `const` assert will then tell you whether the floor is stale rather than leaving it to be noticed.

---

## R3 — `.bss` growth silently shrinks the `.stack` region **[high]**

esp-hal sizes `.stack` from what is left after `.bss`. Phase 1 adds, all in static storage: `StackResources<4>` via `static_cell::StaticCell` (`mode.rs:2961–2963` on the reference), embassy-net's internal smoltcp buffers, the embassy-executor task arena, and the `Channel`/`Signal` statics (`mode.rs:90–174`). Every byte of that comes **out of the stack region the gate measures**.

So R2 and R3 point opposite ways and can cancel: the region shrinks toward the 74,208 B floor *while* real per-task stack demand moves off-region. A gate failure would be read as "shrink `.bss`" when the actual condition is "you now have four stacks and measure one". A gate *pass* proves less than it did before.

**Action:** record the `.stack` region number at **every** step P1.0→P1.6, not just at the end, so the step that moved it is unambiguous. Per `[[gate-that-cannot-fail]]`: ask for **the number**, not "green".

---

## R4 — `portable-atomic` becomes a direct dependency again **[medium-high, known-recurrent]**

The reference adds `portable-atomic = { version = "1", optional = true }` under `wifi`, with a comment that it takes **no features** and rides the existing `portable_atomic_unsafe_assume_single_core` cfg (rv32imc has no native atomics; the C3 is single-core, and that cfg is mutually exclusive with `critical-section`).

This is the exact crate that broke the tree before: `[[smol-portable-atomic-riscv-pin]]` — a cargo-update leaked `unsafe-assume-single-core` **into the host build**, and it was a *feature leak*, not a version problem (v345 shipped on 1.14.0; 1.13.1 failed identically). The reference's own comment says it is `wifi`-gated so `hostsim` never pulls it — good, and that is the right guard — but the leak vector is **feature unification across the workspace**, not the dep line.

**Action:** after P1.1, run the `hostsim` tier and `cargo tree -e features -p portable-atomic` for both the firmware and host targets. Do not assume the gate catches it — last time the build gate did catch it, but only after the fact.

---

## R5 — Porting Phase 0c′ verbatim deletes a shipping gateway **[certain if attempted]**

Covered in PORT-SPEC §0.2. Restated here because it is the highest-cost mistake available: reference commit `a0d3e5a` removes 4,023 lines from `wifi.rs` including `mqtt_session` (~2,000 lines) and leaves `run_mqtt_burst`/`run_ota_fetch`/`run_ntp_burst`/`run_ntp_resync` as **stubs returning `false`/`None` with correct-sounding log lines**.

This is `[[stubbed-intentions-under-deliver-silently]]` in its purest form: *the stubs never fail.* A gateway running them flushes nothing, fetches nothing, and syncs nothing — and reports it as an ordinary unsuccessful window. On main those functions now carry #21/#56, #153, #309, #324, #217 and #188.

**Action:** Phase 1 must not touch `wifi.rs`, `ota.rs`, `net.rs`, `about.rs`, `ota_mesh.rs`. If a Phase-1 PR's diffstat shows `wifi.rs` losing lines, that PR is wrong.

---

## R6 — Blocking busy-waits already on main become executor-starving under Phase 1 **[medium-high]**

Phase 1's entire benefit is that awaits let other tasks run. Any *non-awaiting* spin becomes a hard stall of `net_task`/`wifi_task` — including the DHCP/TCP pump.

Known instance, on the crown's re-association path (`mode.rs:3596–3602`):

```rust
self.elect_announcer.moved(now_ms());
while !self.elect_announcer.settled() {
    let t = now_ms();
    if self.elect_announcer.due(t) { self.broadcast_elect(); }
}
```

This spins with no yield for `ANNOUNCE_BURST(6) × ANNOUNCE_GAP_MS(120)` ≈ **600 ms**, and there is a matching pre-move spin at `mode.rs:3552–3557` (`while !self.elect_announcer.clear_to_move()`). Under Phase 1 these become 600 ms with the network stack frozen — on the same path #324 exists to keep short.

**Action:** audit every `while !…` / `loop` in `mode.rs` and `wifi.rs` that is not already awaiting, before P1.5. Convert the two ELECT spins to `Timer::after(…).await` as part of Phase 1, not later. This is cheap and it is a real behavioural improvement, not just a port chore.

---

## R7 — The round-trip rollback canary gate #335 demands (proposed contract)

`[[smol-ota-canary-only]]`: espflash's bundled ESP-IDF v5.1.2 bootloader has **app-rollback OFF**, so there is no automatic revert. The app-side self-rollback is primary: `OTA_MAX_UNCONFIRMED_BOOTS = 3` (`main.rs:553`) → `ota::boot_confirm(false)` → brick-safe flip, and `boot_confirm` refuses to roll back into a slot with no valid image (`ota::slot_has_valid_image`, `ota.rs:1196`).

**Phase 1 is exactly the change that could break the rollback path itself**, because it moves boot ordering (`esp_rtos::start` hoisted ahead of the #226 otadata init and the #40 unconfirmed-boot bookkeeping at `main.rs:~610–640`) and changes what "a healthy boot" means. A firmware whose *rollback* is broken is unrecoverable over the air.

**Proposed gate — must pass before Phase 1 touches more than one board:**

1. **Forward leg.** OTA v917 → Phase-1 image on **one** canary board (`tools/ota_publish.sh stage/install`; needs `PATH=~/.cargo/bin` + `BW_SESSION` per `[[smol-ota-roll-and-cli]]`). Confirm the board self-confirms: otadata goes New → Valid.
2. **Reverse leg — the half usually skipped.** OTA the Phase-1 image **back** to v917 and confirm *that* activates and self-confirms. A one-way canary proves you can leave, not that you can come back.
3. **Forced-rollback leg.** Deliberately fail the self-test (or install an image that cannot reach DHCP) and prove the K-counter reaches 3 and flips to the good slot **without** JP at the bench.
4. **Evidence discipline.** Ground truth is the `Loaded app from offset` line after every flash and an MQTT flip **to a new value** — never a retained topic's persistence (`[[smol-retained-mqtt-ghosts]]`, `[[smol-ota-ground-truth-hierarchy]]`).
5. **The otadata trap.** After any OTA the board runs from `ota_1`; a later USB flash writes `ota_0` and **silently never runs**. `espflash erase-region 0xf000 0x2000` (otadata only — keeps NVS) then reset, and check the `Loaded app from offset` line (`[[smol-espflash-otadata-trap]]`).

Board identity by `ID_SERIAL_SHORT` via **`udevadm`** (passive). `espflash board-info` **resets the target** and has already rebooted JP's live C6 watch once. Never flash the never-flash MAC list (`[[smol-usb-port-map]]`).

---

## R8 — Gates that must still be green after the port

All 21 steps in `tools/gate.sh`. The ones Phase 1 can plausibly break, with why:

| gate | step | Phase-1 exposure |
|---|---|---|
| stack floor vs canonical ELF | `gate.sh:358` | R2/R3 — directly |
| byte-free tier claims (#351) + ELF-symbol corroboration | `:238`, `:383` | new deps must not leak into `default`/`wifi`/`espnow`; `defmt-canary` must link **zero** defmt in fleet builds |
| tier exclusions on a debug-instrumented build (#351) | `:311` | same |
| ELECT send-path checker (#278) | `:194` + `:475` | `tools/check_elect_send_path.py` asserts a **declared count** of raw-send sites (`RAW-SEND-SITES: send_to:1, send_arb_raw:1, run_leaf_ota_relay:3`, `mode.rs:6255`). Any refactor of the send path must update the declaration or CI goes red — by design. |
| build-matrix declarations (#350) | `:207` + `:451` | the two new features (`defmt-canary`, `phase2-measure`) must be declared |
| verifier wiring (#367) | `:179` + `:496` | per-root walk |
| vendored realm-sigil (#384) | `:134` | untouched by Phase 1, but it fails closed |
| clippy `-D warnings`, every tier | `:270` | edition 2024 (P1.0) will move this |
| DIAG shed order (#339) / DIAG budget (#306) | `:152`, `:165` | only if Phase 1 adds a DIAG field |

Canonical fleet tier is `espnow,cast,io` (`tools/build-matrix.toml:103`); canonical chip `esp32c3`.

**Note the shape of the trap in `repro_build.sh`:** it is a **sourced library** — running it bare exits 0 and does nothing. The real stack gate is `repro_build_bin` via `ota_publish.sh`, and `gate.sh:352` sources it. `[[gate-that-cannot-fail]]`.

---

## R9 — Four premises in the briefing that do not hold, each of which would have shaped the plan wrongly

Flagging these as risks, not trivia: each is a premise that would have been built on. Per `[[flagged-caveat-is-not-contained]]`, I re-derived rather than caveated.

1. **"main … edition 2024"** — main is `edition = "2021"`, `rust-version = "1.96"` (`rust/clock/Cargo.toml:4–5`). The edition-2024 migration (`d253db2`) is unported work. → PORT-SPEC §0.1.
2. **"#122 (flush 30→20s) ⇄ #324"** — `RELAY_FLUSH_INTERVAL_MS` is still **`30_000`** on main (`mode.rs:1603`). The 20 s value lives on the **unmerged** branch `feat/122-b1-windows`. And #324's gate is derived from the flush **budget**, not the interval: `REELECT_SILENCE_MS = RELAY_FLUSH_BUDGET.as_secs()*1000 + 2*2000 + 1000 = 20_000` (`mode.rs:2595`), where `RELAY_FLUSH_BUDGET = 15 s` (`wifi.rs:457`, whose own comment names both consumers). At F=30 the operative ladder is `20 s gate < 35 s RECOVERY_STALE_MS < 45 s #136 floor < 90 s MC_STALE_MS`, so **a crown handover faster than 45 s — not 35 s — is the #136-violation signature.**
3. **"the NEW main-side ELECT wire protocol … changes that picture [of broker-mediated election]"** — it does not. See DELTA-MAP §1: `FOLLOW_ENABLED = false` (`election.rs:158`), and the ELECT frame's `gateway` field is read into two `log::info!` format arguments and nothing else. Crown authority is still 100% the retained broker record. This one is *good* news and materially de-risks Phase 3.

4. **"the stagger tail-RTT decision of record: start wide at 25 s"** — real, but it is a **reference-branch** decision, not a main-branch one, and the brief does not say so:

   | rev | `ELECT_TIER_STEP_MS` | citation |
   |---|---|---|
   | `main` | **15_000** | `rust/clock/src/net/election.rs:255` |
   | `dream/feat-embassy` | **25_000** | branch `election.rs:106` |

   The widening to 25 s landed in inc3d-2 (`b6413d3`) and exists **because** the port dropped a209858's **#114 H2 claim-race re-read** (the ~400 ms post-publish socket re-read that let the lowest id yield or re-assert). With that gone, #76 dual-claim protection rests entirely on the fitness stagger plus next-window observe-and-adopt — so 25 s is a **conservative stand-in for a deleted mechanism**, not a measured value. The measurement that would settle it (an isolated two-board simultaneous-boot tail-RTT run on a throwaway broker) **was never performed**.

   **Consequence:** 25 s is coupled to porting the RESOLVE path. Do not carry it to `main` on its own — on today's `main` the #114 H2 re-read still exists, so 15 s is the correct value and widening it would slow every crown recovery for no reason.

---

## R10 — Phase-1 benefit must be a measured number, not a code-reading

Main already carries the instrument: `BurstProbe` (#153/#198, `main.rs:~366–440`) reports `burst`, **`longest app gap`**, `longest yield gap`, paints, yields. Its doc comment states the Embassy case honestly and is worth quoting back at sign-off time:

> "The Embassy research doc's honest gap was that `SUBTICK_MS` starvation on `main` had never been MEASURED, only inferred from the mesh deaf-window's mechanism… `longest yield gap` is the floor: the app cannot be serviced more often than the radio yields, so if that number is large, no repaint cadence can help and only the Embassy re-platform (#198/#233) can."

**Risk:** Phase 1 lands, all 21 gates go green, and it ships with no evidence it did the thing it exists to do — because every gate is a compile-time gate. Capture `BurstProbe` on v917 and on the Phase-1 image, same board, same duty, and put the two numbers in the PR.

---

## R11 — TIMG0 double-ownership: the structural anchor, and it can compile

Today `TIMG0.timer0` is esp-radio's scheduler timebase. `esp_rtos::start` **requires** `timg0.timer0` + `software_interrupt0`. Both claiming TIMG0 is a double-init.

The nasty part is that `TimerGroup::new` / `timg0.timer0` **stay valid names**, so the wrong wiring **compiles while expecting a different tick source**. Symptom is not a crash — it is embassy timers running at the wrong rate.

**Falsifiable check after P1.2:** WiFi still associates, and the SNTP/burst budgets elapse at **wall-clock** rate (time a 15 s budget with a stopwatch). A timebase that is silently 2× or ½× will still "work" and will quietly wreck every #324/#136/#278 window in RISKS §R9.2's ladder.

---

## R12 — Phase 4 carries a brick-class bug that is already identified. Do not let it be re-discovered on hardware.

Recorded in the Phase-4 design review as **P4-H1, "THE brick bug"**, and flagged here because it must survive into whatever Phase-4 plan gets written:

`embassy_sync::Mutex` is **not reentrant.** The 0c′ flash plumbing acquires flash **several independent times per OTA** — `begin()` → `inactive_slot()` → `flash_mut()`, then `flash_mut()` again; `activate()` → `set_slot_new()` → `flash_mut()`. Holding a `FLASH: Mutex` across the OTA turns each internal re-acquisition into `FLASH.lock().await` on a lock the same task already holds → **parks forever, mid-sequence, holding the lock → watchdog reset mid-slot-flip → brick.**

Mandatory shape: acquire the guard **once** and thread `&mut FlashStorage` through *all* of `begin`/`inactive_slot`/`feed`/`flush_stage`/`finalize`/`activate`/`set_slot_new`, with **zero** residual internal `flash_mut()`/`lock()` calls.

Two companions, same review: **P4-H2** — the #40/#237 mesh-relay fetch runs **inline** and predates any `OTA_IN_PROGRESS` atomic, so an offer arriving during a relay can start a **second `ImageWriter` on the same inactive slot**; both entry points must be gated symmetrically, whoever swaps-true first wins and the loser must **never call `begin`**. **P4-M1** — any non-OTA flash writer that bare-locks (notably the runtime `write_net_cfg`, #56, mesh-triggered and inline) **parks the whole loop** for the OTA's duration → crown-silent window → #204 churn.

Also note `FlashStorage::new` is documented **"panics if called more than once"** — a *runtime* panic, not a borrow-check error, and the tree has ~7 ad-hoc construction sites.

---

## R13 — The #204 detector cannot simply be re-armed. Plumb first.

On the reference branch the #204 crown-deafness detector is fully dead, deliberately. The trap for whoever revives it:

`elect.downstream_seen` is **never written** — the async migration severed v904's `got_mc || batt || grid` wire, because `downlink_drain` lives in `mqtt_task` with no access to the inline `elect`. So feeding the detector a real flush result while `downstream_seen` is permanently `false` makes `crown_deaf_streak` climb on **every connected flush** → `deaf_shed` → `flush_incapable` → **spuriously demotes every healthy crown, fleet-wide.**

Order is mandatory: plumb `downstream_seen` (a `DOWNSTREAM_SEEN` signal from `downlink_drain` to the inline detector) **before** un-gating the detector. This is a "correct comment, absent behaviour" trap of exactly the `[[stubbed-intentions-under-deliver-silently]]` shape, except that here re-arming it does active harm rather than nothing.

---

## R14 — Process hazards specific to this port

- **Do not rebase `dream/feat-embassy`.** #335 decision of record; it is a design reference. Read it with `git show`/`git diff`, never `git rebase`/`cherry-pick` onto it.
- **Worktree-isolate concurrent code agents.** `mode.rs` is 7,569 lines and every phase touches it; `[[commit-gate-before-parallel-build]]` and `[[duplicate-dispatch-collides-agents]]` both apply. Post the owner on the issue before dispatching.
- **Never run a repo self-test in the shared tree.** `[[never-git-test-in-the-shared-tree]]`, `[[a self-test hard-reset the whole repo]]` — `test_ha_deploy_guard.sh` ate a 200-line edit set. Commit first.
- **Serialize release builds.** `[[dreamteam-scope-pagecache-kills]]`, refined by `[[smol-builds-low-balloon]]`: checks/clippy are free, release builds are not. Offload to `familiar` if convenient (`[[smol-build-on-familiar]]`).
- **The two `esp_rtos::start` call sites are mutually exclusive radio-init paths** (`wifi.rs:510`, `mode.rs:2346`). Hoisting to `main` must remove **both**, or the second call double-starts the scheduler. Easy to half-do.
- **🔴 NEVER run `cargo fmt` in `rust/clock`.** The tree is not rustfmt-clean and there is **no fmt gate**. One `cargo fmt` reformatted **all 41 files** during Phase-1 inc1, burying the real 6-file change; the whole thing had to be reverted and hand-applied. Every exec log in the reference campaign records "No cargo fmt" for this reason.
- **Build the branch tip, not an increment SHA.** The Phase-2 log records an operator trap: building `89d45e7` (superseded, `SMOL_P2_BLOCKING` bool) while setting the newer `SMOL_P2_BLOCK_MODE` env var yields a **phantom-knob Run-1 image** that the operator believes is Run-3. Any harness env knob added by this port should **fail the build on an unknown value**, not default silently.
- **`probe-rs run` and `espflash monitor` die exit-144 in the agent sandbox.** RTT capture is a JP-run step, in his own terminal (`[[jp-bench-ping-when-physically-needed]]`). Spec measurements as JP-run; do not write a plan whose evidence step an agent cannot execute. `probe-rs` *attach* and `espflash` *flash* do work in-sandbox.
- **Bench boards need a full chip erase to take a baked `SMOL_NODE_ID`** — it only applies on a blank NVS. Convenient side effect: the erase also clears otadata, dodging `[[smol-espflash-otadata-trap]]`.
- **A measurement/canary board must be election-inert.** The reference needed *four* commits to get there (`8f48850` non-electing + ch6-pinned, `61e28a2` advertisement-silent, `c6793fa` the LDBG+STAT+DIAG leaks a lexical `broadcast_*` audit missed, `db6e17c` the behavioural `esp_now_tx()` choke). The lesson generalises: **audit by what the board transmits, not by what the functions are named** — `[[literal-grep-proves-nothing-about-constructed-strings]]`. On today's `main` this matters more, since a stray bench HELLO now feeds a #278 announcer as well as the metric election.
