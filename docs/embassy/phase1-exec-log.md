# Phase 1 exec log — P1.0 … P1.4 (#335)

Executor: Morpheus. Worktree `/home/jp/Projects/smol-wt-p1`, branch `feat/335-phase1`.
Scope: P1.0–P1.4 ONLY. **P1.5 / P1.6 are NOT in this branch** — P1.5 (the 23-site controller
move) is gated on the orchestrator's review.

Gate command set (identical every step), run from `rust/clock`:

```
export PATH=$HOME/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/tmp/claude-1000/-home-jp-Projects-smol/e5eea919-…/scratchpad/p1-target
cargo check   --release --features espnow
cargo clippy  --release --features <each tier build_matrix.py emits>
cargo test    --no-default-features --features hostsim --target x86_64-unknown-linux-gnu
cargo build   --release --features espnow ; readelf -SW <elf>
python3 tools/check_verifier_wiring.py ; bash tools/test_verifier_wiring.sh
```

Standing local-only noise, NOT regressions: 3 dead-code warnings for `WEATHER_*` in the
git-ignored `src/board.rs` (#363). `board.rs` / `secrets.rs` are never staged.
Never `cargo fmt` (RISKS §R14).

---

## Section-size trajectory

Floor (`ESP32C3_STACK_FLOOR_BYTES`, budget.rs:197) = **74,208 B**.

| step | commit | `.stack` | Δ | ×floor | `.bss` | Δ | `.data` | `.rodata` |
|---|---|---|---|---|---|---|---|---|
| baseline (P1.1 union probe) | `c00d82d` | 106,976 | — | 1.44 | 156,816 | — | 15,032 | 68,324 |
| P1.0 edition 2024 | `455e866` | 106,976 | 0 | 1.44 | 156,816 | 0 | 15,032 | 68,324 |
| P1.1 embassy crates | `0ad2f8c` | 106,912 | −64 | 1.44 | 156,816 | 0 | 15,096 | 68,324 |
| P1.2 executor ON | `40a6a51` | 106,864 | −48 | 1.44 | 156,864 | +48 | 15,096 | 68,468 |
| P1.3 `Timer::after` | `9e86030` | **87,960** | **−18,904** | **1.185** | 175,768 | +18,904 | 15,096 | 68,996 |
| P1.4 net_task + Stack | — | **NOT LANDED** | — | — | — | — | — | — |

Net across the phase: `.stack` −19,016 B, `.bss` +18,952 B — the two are the same bytes
moving, not two separate costs. See P1.3.

> The baseline `.stack` here is 106,976 B, not the 107,008 B `c00d82d`'s message quotes. Both
> are right: the delta is 32 B of tree-dependent `board.rs`/`secrets.rs` literals in `.data`/
> `.rodata` (Cargo.toml already documents this for the image size, #348). Trajectory is what
> matters, so every row below is measured in THIS tree.

## Host test counts (unchanged unless a row says otherwise)

`bard 41 / budget 10 / input 8`, plus two 0-test targets (lib, and the `clock` lib-test). 0 failed.

---

<a id="f1"></a>
## 🔬 FINDING F1 — a manifest edit can resize a shipping static, and nothing in this repo would say so

Surfaced by P1.1, but it is not about P1.1. Filed here with its own heading because **any** future
manifest change can do this, silently, to any static in the image.

### What happened

P1.1 added six dependency lines and changed no source. The espnow ELF came back with `.data`
+64 B and `.stack` −64 B.

### The exact mechanism

1. **`nm` A/B of the P1.0 and P1.1 ELFs** — identical 419-symbol sets, every symbol the same
   size but one: `net::wifi::NTP_SOCK_STORAGE`, `0x540 → 0x580`.
2. **`cargo tree -e features` diffed across the two manifests** — exactly ONE feature was added
   to smoltcp: **`async`**. It puts a `WakerRegistration` in every socket, so the `Socket` enum
   grows, so an array of them grows.
3. **There is still only ONE `smoltcp v0.13.1` in the graph.** embassy-net 0.9.1 wants the same
   0.13 our direct dep does, so the async stack and the hand-driven one *share the crate* — which
   is how a feature chosen by embassy-net, for embassy-net's sockets, reached inside
   `mqtt_session`'s socket storage. Feature unification is the coupling; sharing the crate is
   what makes it reach a shipping struct.

Chain, in one line: **new dep → feature unification → a shared crate's enum grows → a static in
the running gateway grows → `.stack` shrinks by exactly that much.**

### Why nothing caught it, and why nothing would have

| instrument | why it is blind to this |
|---|---|
| code review | there is no source diff to review |
| the compiler | no API and no arity changed — [[zero-conflicts-raises-risk]] verbatim |
| `repro_stack_check` (stack floor) | watches ONE aggregate (`.stack` region) against a threshold. 64 B is 0.06% — it passes. Worse, the aggregate can hide offsetting moves in both directions |
| `check_exclusions.py` (#351) | asks whether a tier CONTAINS a module's code, never how big anything is |
| `check_byte_free.py` | same — presence/absence, not size |
| the host test suite | never links the firmware |

The only thing that found it was a hand `nm` A/B I ran because a 64 B move I could not explain
bothered me. That is not a gate.

### Why it matters beyond 64 bytes

64 B is harmless. The mechanism is not size-bounded — the *same* chain with a larger struct, or a
dep wave touching several, lands as an unexplained `.stack` loss that a future reader will
attribute to their own commit. And P1.3 produced an 18,952 B move by a related route (a task
POOL, not a dep), which is a number nobody should have to discover by hand either.

### Proposed follow-up — a symbol-size baseline (NOT built here)

The repo already runs this exact pattern three times, so this is idiomatic rather than novel
machinery: #278's declared `RAW-SEND-SITES` count, #350's build-matrix declarations, #367's
verifier-wiring allowlist. All three are "the declaration goes stale → CI goes red, by design."

Sketch: `tools/check_symbol_sizes.py <elf> --baseline tools/symbol-sizes.<tier>.txt`

* Baseline holds every `.data`/`.bss` symbol ≥ 256 B as `name size`, checked in per gated tier.
* Fails when a listed symbol changes size, a new symbol clears the threshold, or a listed one
  vanishes. `--bless` regenerates, so a legitimate resize becomes **a reviewed line in the diff**
  instead of an invisible one.
* It generalises the stack floor rather than duplicating it: the floor watches the one derived
  aggregate, this watches the individual statics whose growth moves it.

⚠️ **The one non-obvious design constraint, learned the hard way here.** Do NOT key the baseline
on mangled symbol names. Rust's crate-metadata hash is in them, and it changed between my own two
builds (`_RNvNtCsjIZ8XU16pSq_5clock3ota…` → `_RNvNtCsa9xoDfnccUU_5clock3ota…`) — the raw `nm`
diff was ~72 lines of pure noise until I compared demangled *name+size* sets instead. Key on the
demangled, hash-stripped path (`clock::net::wifi::NTP_SOCK_STORAGE`) and skip the anonymous
`.L_MergedGlobals*` / `.Lswitch.table.*` rows, which renumber on every build.

Cheaper fallback if that is too much tooling for the value: fold the socket storages into #348's
budget arithmetic in `budget.rs` with a const assert. ~5 lines, no new tools — but it guards
those specific statics, not the class.

**File-ready stub** (not filed — say the word):

> **Title:** `a dependency's feature unification can resize a shipping static and no gate says so`
>
> **Body:** #335 P1.1 added six embassy dep lines and changed no source. embassy-net turned on
> smoltcp's `async` feature; smoltcp is SHARED with the hand-driven gateway (one `v0.13.1` in the
> graph), so `WakerRegistration` per socket grew `net::wifi::NTP_SOCK_STORAGE` 0x540→0x580 —
> +64 B of `.data`, straight out of the `.stack` region, invisible to code review (no source
> diff), to the compiler (no arity change), to `repro_stack_check` (0.06% of an aggregate with a
> threshold), and to #351's exclusion checker (presence, not size). Found only by a hand `nm` A/B.
> Proposal: a per-tier symbol-size baseline in the #278/#350/#367 declaration style —
> `tools/check_symbol_sizes.py` + checked-in `tools/symbol-sizes.<tier>.txt` for `.data`/`.bss`
> symbols ≥256 B, `--bless` to update, so a resize lands as a reviewed diff line. Key on
> DEMANGLED hash-stripped names — the crate-metadata hash in mangled names churns and buries the
> signal. Cheaper alternative: add the socket storages to #348's `budget.rs` arithmetic with a
> const assert (guards these statics, not the class).

---

## P1.-0.5 — `chore`: the orphaned Cargo.lock — `85546e6`

Not a spec step. `c00d82d` committed `Cargo.toml` only, so the branch had been carrying a
lockfile that did not match its manifest. Turning on esp-rtos's `embassy` feature resolves 32
new packages (embassy-executor 0.10 + macros + timer-queue, and the loom/tracing dev-graph
behind them); zero removals, zero version changes to anything already locked.

Landed on its own so each later step's lock delta is attributable to the step that caused it.
No CI job passes `--locked`, which is why this self-healed silently on every build and nothing
flagged it.

---

## P1.0 — edition 2021 → 2024 — `455e866`

Reference: `d253db2`. `rust-version` **stays 1.96** (the reference lowered it to 1.88; main's
floor is higher and 1.96 is what the toolchain gates on).

**Hard fallout: exactly 2 sites, one rule.** `no_mangle` is an unsafe attribute in 2024:

| file | symbol | note |
|---|---|---|
| `src/main.rs:65` | `custom_halt` | the MF-2 panic → `software_reset` hook — the site `d253db2` fixed |
| `src/net/target.rs:654` | `SMOL_TARGET_DESC` | #349, post-dates the reference — the reference could not have known about it |

Every `static mut` access already goes through `addr_of_mut!`, so 2024's static-mut-ref rule
is a no-op, exactly as `d253db2` recorded.

**Soft fallout — the surprise, and it is not in `d253db2`'s message.** 2024 stabilises
let-chains, so clippy's `collapsible_if` can now suggest folding
`if a { if let Some(b) = c { … } }` into one chain. At 2021 it could not make that suggestion
and the tree was clean (baseline clippy: 3 warnings, all `board.rs`). The bump opens **38 fresh
sites across 10 files**:

```
mode.rs 14 · wifi.rs 11 · main.rs 5 · toast.rs 2 · http.rs 2 ·
familiar/mod.rs 1 · coexist.rs 1 · ota.rs 1 · ota_mesh.rs 1
```

23 of the 38 are in the two files P1.2–P1.5 rewrite. Collapsing them would bury a 4-line
edition change in a 38-site reflow and make the step un-revertible in practice, so both crate
roots (`main.rs`, `lib.rs` — they share `toast.rs`/`familiar/`) take a documented
`#![allow(clippy::collapsible_if)]` + TODO. This is what the reference did too, in `770c549`
— though its message calls the lint a "PRE-EXISTING baseline", which on our tree it is not.
The collapse is a separate mechanical cleanup with no Embassy content.

**Gates.** check green on default / wifi / espnow / espnow,cast,io / wled,espnow,cast,io /
mesh-test,espnow / hostsim(x86_64). clippy clean on all 10 tiers `build_matrix.py emit --for
clippy` lists, modulo the 3 `board.rs` warnings. Hosts 41/10/8. Verifier wiring 19 sound /
1 known phantom (185_crdt_verify), exit 0; `test_verifier_wiring.sh` 5/5.

**All four section sizes byte-identical to baseline** — the right answer for an edition +
attribute change with no codegen content, and a useful control for reading P1.1–P1.4's deltas.

**Out of scope, recorded so it is not rediscovered:** `cargo clippy --all-targets` on the
hostsim tier denies `absurd_extreme_comparisons` at `tests/bard.rs:215`
(`assert!(cache_slot(pos) >= KEEP)` where `KEEP` is the type's minimum). Edition-independent,
and `gate.sh`'s clippy loop runs neither `--all-targets` nor the hostsim tier — so it is not a
gate today and I did not touch it.

---

## P1.1 — the Phase-1 embassy crates — `0ad2f8c`

Manifest only. Added per PORT-SPEC §2.1: `embassy-executor 0.10`, `embassy-time 0.5 (log)`,
`static_cell 2.1`, `embassy-net 0.9.1 (tcp,udp,dhcpv4,medium-ethernet,log — no dns)`,
`embassy-sync 0.7`, `portable-atomic 1 (no features)`. Kept: `smoltcp`, `embassy-futures`,
`esp-wifi-sys`, esp-radio's `log-04`, esp-rtos's `esp-radio`.

**Divergence from the reference, and it propagates.** The reference put the executor deps on
`hw` (its D1 "executor-first" call). We gated all six on `wifi`, because esp-rtos supplies
embassy-time's time driver and esp-rtos is `wifi`-gated on this tree — a driverless
embassy-time is a link error the instant anything calls `Timer::after`, so driver and consumer
must share a gate. Hoisting to `hw` would also make the no-radio `default` build allocating and
executor-driven, superseding #44's byte-minimal-default invariant, for no Phase-1 benefit.
This is the decision P1.2 and P1.3 are both shaped by.

### R4 — portable-atomic, verified on both sides rather than asserted

| side | result |
|---|---|
| HOST (`--features hostsim --target x86_64-unknown-linux-gnu`) | **zero** portable-atomic nodes in `cargo tree`. No leak. |
| DEVICE (espnow) | portable-atomic **v1.14.0 was already there**, with `unsafe-assume-single-core` already on, enabled by `esp-sync` + `esp-radio-rtos-driver`. Our featureless dep line adds no version and no feature — it only makes an existing edge nameable. |

`-Zbuild-std` stays removed (`.cargo/config.toml:36-46`). That, not the dep line, was the
2026-07 leak vector — the memory is precise about this and it held.

`embassy-sync` resolves to the **0.7.2 already in the lock**. The 0.6.2 / 0.8.0 copies beside it
are pre-existing (checked `git show HEAD:…/Cargo.lock` at P1.0) — this step adds no new copy.

### The measured surprise: a manifest line resized a shipping struct

`.data` +64, `.stack` −64, with **no source change anywhere**. This is a CLASS, not an
incident — promoted to its own section: see **[Finding F1](#f1)** below.

---

## P1.2 — executor ON — `40a6a51` 🔴 the links-but-dies step (RISKS §R1)

### Entry point, split by tier

main's ~1,900-line body became `async fn run()` — **renamed in place, not re-indented**, so the
diff is two lines at its top — with two three-line entries:

```rust
#[cfg(feature = "wifi")]      #[esp_rtos::main] async fn main(_spawner: Spawner) -> ! { run().await }
#[cfg(not(feature = "wifi"))] #[main]           fn main() -> ! { embassy_futures::block_on(run()) }
```

`dep:embassy-futures` moved `wifi` → `hw` to serve the second one. That is the whole cost of
declining D1, and the no-radio tier does not notice it: its ELF is **byte-identical across
P1.1 → P1.2 in all four sections** (`.data` 1,904 / `.bss` 468 / `.rodata` 19,132 / `.stack`
315,388), with **zero** `esp_rtos` or `embassy_executor` symbols in it.

### ⚠️ Correction to the plan: `#[esp_rtos::main]` does NOT call `esp_rtos::start`

Read the macro (`esp-hal-procmacros-0.22.0/src/rtos_main.rs`). It wraps the body in a `__main`
module, creates an `esp_rtos::embassy::Executor`, and spawns the body as its first task. Nothing
more. The scheduler start is still a hand call — which is exactly why the hoist has somewhere to
land, and worth knowing before someone assumes the attribute did it.

### The hoist, and why it is placed where it is

Both old sites (`wifi.rs:510`, `mode.rs:2346`) are gone, and `WifiPeripherals` is down from
`{timg0, sw_int, wifi}` to `{wifi}`. **The compiler now enforces completeness**: a surviving call
site would fail to build for want of a peripheral to start with. RISKS §R14 calls this "easy to
half-do"; it is now impossible to half-do.

Placed immediately before the radio-bring-up branch, **not** up beside `esp_hal::init`. That is
the instant the two old calls fired, so the hoist changes WHERE the call is written and not WHEN
it runs — #226 otadata init and #40 unconfirmed-boot bookkeeping still complete on a bare context
with no scheduler and no timer driver, as on v917. RISKS §R7 worries that Phase 1 reorders boot
and breaks rollback; keeping the instant unchanged declines that risk instead of arguing about
it. §R11's TIMG0 double-ownership is unchanged too: same timer, same moment, same call.

### Proof the executor LINKED (not "cargo check passed")

`nm` on the espnow ELF — three independent fingerprints of the macro's expansion:

```
esp_rtos::embassy::Executor::run          monomorphised over a closure from
                                          clock::__main::__risc_v_rt__main
embassy_executor::raw::util::UninitCell<clock::__main::__embassy_main_task…>::write_in_place
esp_rtos::timer::TimeDriver::arm_next_wakeup
```

### Still unproven, and no ELF can prove it

RISKS §R1: that a `block_on` busy loop inside an executor task is **preempted** rather than
deadlocked against `net_task`. §R11: that the embassy timebase ticks at **wall-clock** rate.
Both need the image on a board — BurstProbe output, and a stopwatch against a 15 s budget.
**These gates are not a runtime sign-off.**

---

## P1.3 — `Timer::after` — `9e86030`

Ports `45eea58` (DR-H2) at the 3 sites §2.3 names. `SUBTICK_MS` stays **20 ms** — the HELLO /
TIME / beacon / diag detectors each look back exactly one `SUBTICK_MS`
(`now / N != (now − SUBTICK_MS) / N`, ~8 sites through the loop), so the watch's ~400 ms clamp
would step over their boundaries. `button.poll` still runs at the top of every iteration
(≤20 ms input latency, 700 ms long-press intact); no `select(Timer, button_edge)` early-wake,
same call as the reference.

Shape differs from the reference in one place, forced by P1.1's gating. The reference retired
`Delay` outright; we have a no-radio tier with no time driver. So instead of cfg-ing three call
sites there is **one** cfg'd helper — `async fn subtick(&Delay)`, wifi twin awaits
`Timer::after`, no-radio twin calls `delay_millis` and never awaits — and both sites read
`subtick(&delay).await;` on every tier. The tier difference is stated once.

### Cadence constant — ONE definition, both arms (verified)

The two-arm shape introduces a drift risk the reference never had (it had one arm), so this was
checked rather than assumed:

```
main.rs:311   pub(crate) const SUBTICK_MS: u32 = 20;     ← the sole definition
main.rs:2518  Timer::after(Duration::from_millis(SUBTICK_MS as u64)).await   (wifi arm)
main.rs:2526  delay.delay_millis(SUBTICK_MS);                                (no-radio arm)
main.rs:1135  subtick(&delay).await;   (splash)
main.rs:2493  subtick(&delay).await;   (superloop tail)
```

Both arms read the same const; the wifi arm only widens it (`as u64`). Zero bare `20`/`from_millis(20)`
literals anywhere on the pacing path (grepped). The same const also feeds the ~8 look-back edge
tests (`now / N != (now − SUBTICK_MS) / N`), so cadence and detector window cannot desynchronise.

Worth naming: the single-helper shape is *structurally* stronger here than cfg-ing the three call
sites would have been. Three cfg'd sites would be six places for a literal to drift into; this is
one const and two one-line consumers. That is the [[stubbed-intentions-under-deliver-silently]]
shape closed by construction rather than by discipline.

### 🔴 The number moved 17.7%, and it is not what it looks like

```
.stack  106,864 → 87,960   (−18,904; 1.44× floor → 1.185×)
.bss    156,864 → 175,768  (+18,904)
```

`nm` names it exactly: **`clock::__main::__embassy_main::POOL` = 18,952 B**. Giving `run()` real
await points forces the compiler to materialise the whole ~1,900-line function's live state as a
future, and an embassy task's future lives in **statically allocated task storage**. So ~18.9 KB
of main's frame — dominated by the stack-resident `RadioManager` — **migrated from `.stack` into
`.bss`**.

This is RISKS §R2/§R3 happening on schedule, in the direction they warned could be misread. R2's
own text uses this exact example ("a struct that lives in a stack-resident `RadioManager` costs
real stack and moves this number by almost nothing") — it has now moved off the stack entirely.
**Real stack demand should be ~18.9 KB LOWER after this commit, while the gate's number reads
17.7% worse.**

Corroboration that it is migration and not growth:

| tier | `POOL` | note |
|---|---|---|
| espnow | 18,952 B | holds `RadioManager` and the rest of the frame |
| wifi-only | 1,184 B | no `RadioManager` → nothing big to hold |
| default | n/a | no embassy task at all; `.stack` 315,388 / `.bss` 468 both unchanged |

### Two consequences, neither actioned

* `ESP32C3_MEASURED_PEAK_BYTES = 55,656` (budget.rs:206) was measured with that frame ON the
  stack, so the derived 74,208 B floor is now conservative by roughly the POOL. **Not changed** —
  re-deriving needs `stack-paint` under live radio (R2), which is bench work, and a floor lowered
  from an armchair is how the last one ended up at 12,288.
* P1.4 would add `StackResources<4>` + embassy-net's buffers to the **same** `.bss`. From 87,960
  the margin to the 80,000 B abort line is **7,960 B**.

---

## P1.4 — net_task + embassy-net Stack — 🔴 **NOT LANDED. STOPPED AND REPORTED.**

Nothing committed for this step. The blocker is structural, not a build failure.

### The conflict

`embassy_net::new` needs `interfaces.station`, an **owned** `esp_radio::wifi::Interface<'static>`.
On this tree that value is already owned — by the shipping gateway:

```
net/radio_dev.rs:22   pub struct SmolWifiDevice(Interface<'static>);
net/mode.rs:2048          sta: Option<SmolWifiDevice>,
net/mode.rs:2382          sta: Some(SmolWifiDevice::new(interfaces.station)),
```

and `self.sta` is borrowed on **five live gateway paths** — `mode.rs:2683` (`run_mqtt_burst`),
`:2837` (`run_ntp_burst`), `:2866` (`run_ntp_resync`), `:4849`, `:5287` (`run_ota_fetch`),
`:5829` — every flush, on every crown.

Verified against the crate, not inferred (`esp-radio-0.18.0/src/wifi/mod.rs`):

* `:1906` `Interfaces { station: Interface<'d>, access_point: Interface<'d>, esp_now, sniffer }`
  — one owned station value.
* `:1310` `pub struct Interface<'d>` — **no `Clone`, no `Copy`**.
* `:1821` `impl Driver for Interface<'_>` — embassy-net's driver trait is implemented on the very
  type `SmolWifiDevice` newtypes for smoltcp.

**One object, two consumers, no split.** So "the Stack exists, nothing uses it yet" is not free
here. It is free on the reference only because `a0d3e5a` had already deleted every consumer —
which PORT-SPEC §0.2 identifies as the central fact of this port, and then §2.4.4 says
"port-verbatim" anyway. §2.4.4 has the same conflict §2.4.5 has (the 23 `self.controller` sites),
one layer down: **single-owner radio resources shared between the async stack and the shipping
synchronous gateway.** P1.4 and P1.5 are one decision, not two.

### Options, with real costs

**A — stop at P1.3; fold P1.4 into the P1.5 review.** Recommended. The device move and the
controller move are the same problem and should be decided together, with the Phase-2/3 transport
plan in hand. Costs nothing; P1.0–P1.3 are landed, individually revertible, and each keeps a
shipping image.

**B — bring the Stack up behind a new default-off feature.** Proves compile + link + spawn; fleet
image byte-identical. Costs: a build nobody runs (RISKS §R14's phantom-knob trap), plus #350
build-matrix and #351 exclusion declarations for a feature with no consumer. Delivers evidence,
not capability.

**C — move `station` into embassy-net now and reimplement the gateway on the `Stack`.** That is
Phases 2–4, and en route it deletes `mqtt_session` — RISKS §R5, the highest-cost mistake
available. **Refuse.**

**Considered and rejected: bind the Stack to `interfaces.access_point`.** It is a genuinely
unused `Interface<'d>`, so it would compile, link, spawn, and satisfy the letter of "the Stack
exists, nothing uses it yet" while leaving the STA with the gateway. It is also a `Stack` over an
AP that is never started — a correct-looking construct with no behaviour behind it, which is
[[stubbed-intentions-under-deliver-silently]] exactly, and P1.5 would have to unpick it. Recorded
so it reads as declined rather than missed.
