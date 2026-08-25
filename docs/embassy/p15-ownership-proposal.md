# P1.5 — radio-resource ownership: DECISION (rev-2)

> **Variant C adopted 2026-08-24 (JP session e5eea919); reasoning re-grounded per
> `p15-oracle-review.md`; Phase-3 execution blocked on: R11 timebase stopwatch + the #335
> round-trip rollback canary (both need a board).**

**The decision:** who owns the two radio resources — the STA `Interface` and the `WifiController`
— once both the async stack and the shipping synchronous gateway want them, and in what order
they change hands.

**Base:** `feat/335-phase1` @ `9e86030` (PR #391, P1.0–P1.3, HW-canary-held).

## What changed in rev-2

Rev-1's *sequence* survived adversarial review; most of its *reasoning* did not. Six amendments of
record, all folded in below:

| # | rev-1 said | rev-2 says |
|---|---|---|
| 1 | the station is single-owner, so the compiler enforces atomicity | **`Interface` is `Copy`.** Nothing is enforced. The real constraint is RX-queue arbitration — §1 |
| 2 | six consumers, one tier | **seven, across two tiers** — `try_time_sync` is a 7th pair — §1.4 |
| 3 | step 3 is "13 mechanical conversions" | the scan round-trip is real unported design, and two functions straddle the phase boundary — §4.2 |
| 4 | flip on "assoc- vs session-dominated" from `BurstProbe` | unmeasurable as stated; one `mark()` is the measurement — §6.1 |
| 5 | 7,960 B of margin to an 80,000 B abort line | **there is no 80,000 B line.** Margin is 13,752 B and the floor is itself conservative. Argument withdrawn — §5.3 |
| 6 | the Phase-3 commit is cleanly revertible | source-clean, **fleet-dirty**: three state carriers cross the seam — §5.2 |

Rev-1's error in (1) is worth naming rather than quietly fixing: I cited
`esp-radio-0.18.0/src/wifi/mod.rs:1310` for "no `Clone`, no `Copy`" having read the struct and not
the `#[derive(Clone, Copy, …)]` three lines above it — and our own `radio_dev.rs:19` documents the
handle as "(Copy)" in plain English. The evidence was in our own tree and I did not read it.

---

## 1. The real constraint: one RX queue, and nothing guards it

### 1.1 The type system does NOT help here

```rust
// esp-radio-0.18.0/src/wifi/mod.rs:1306-1313
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[non_exhaustive]
pub struct Interface<'d> { _phantom: PhantomData<&'d ()>, mode: InterfaceType }
```

`embassy_net::new(interfaces.station, …)` takes a `Copy` value — **the original is not consumed.**
A `Stack` and a `SmolWifiDevice` can be bound simultaneously, and it compiles. A dual-trait shim is
not merely possible, it is unnecessary: both impls already exist on the same underlying type
(`radio_dev.rs:61` `phy::Device for SmolWifiDevice`; esp-radio `mod.rs:1821` `Driver for Interface`),
and the two traits are structurally isomorphic — same GAT shape, same `&mut self`, differing only in
the "nothing ready" argument (poll timestamp vs waker `Context`).

### 1.2 What actually breaks: `data_queue_rx()` is keyed by interface type alone

```rust
// esp-radio mod.rs:1247
fn rx_token(&self) -> Option<(WifiRxToken, WifiTxToken)> {
    let is_empty = self.data_queue_rx().with(|q| q.is_empty());
    …
    self.tx_token().map(|tx| (WifiRxToken { mode: *self }, tx))
}
```

`mode: *self` — `Station`. embassy's `Driver::receive` (`mod.rs:1837`) and smol's
`phy::Device::receive` (`radio_dev.rs:71`) bottom out in the **same queue**. Two live stacks pop
from one queue; a frame consumed by one is gone for the other. **Nondeterministic packet theft. No
error, no panic, no failing gate.**

> The conclusion "all transport consumers move together" is unchanged and correct.
> What changed is who enforces it: **nothing does.** A type error fails loudly in CI. A shared
> queue compiles green, passes all 21 gates, links, boots, and loses packets on a fleet board
> under load.

This is `[[stubbed-intentions-under-deliver-silently]]` waiting to happen, reached exactly the way
`[[zero-conflicts-raises-risk]]` describes — nothing forces a second look because nothing complains.

### 1.3 Consequently: a structural gate is a Phase-3 DELIVERABLE, not a nicety

In the repo's existing idiom — `tools/check_elect_send_path.py` asserts a declared raw-send-site
count (`mode.rs:6255`), wired at `gate.sh:194` with its own regression suite at `:475`:

* Assert **one live station consumer per feature tier** — no `embassy_net::new` co-existing with a
  `SmolWifiDevice::new` in the same tier.
* Assert a **declared count of `SmolWifiDevice::new` sites**. Currently **2**, both verified:
  `mode.rs:2382` (espnow tiers) and `wifi.rs:515` (the `wifi` tier).

This converts an invisible invariant into a red CI light, which is the only form of it anyone will
notice. It lands **before** the transport phase begins, not with it.

### 1.4 Seven consumers, two tiers

There are **two independent station acquisitions**, in mutually exclusive tiers:

| tier | bring-up | station | controller |
|---|---|---|---|
| espnow (⊃ fleet) | `RadioManager::new` | `mode.rs:2382` | `RadioManager.controller` |
| **`wifi`** | **`try_time_sync` (`wifi.rs:495`)** | **`wifi.rs:515`**, own `wifi::new` at `:514` | **its own local** |

`net.rs:329` says so itself: *"Shared by both radio-init paths (`wifi::try_time_sync` and
`mode::RadioManager::new`)."* The `wifi` tier is declared in `tools/build-matrix.toml:93` and is
compiled and clippy'd by `gate.sh:245` / `:270`.

**`run_ntp_burst` therefore has two callers** — `wifi.rs:526` and `mode.rs:2841`. Rewriting it onto
embassy-net sockets breaks `try_time_sync`, which has no `RadioManager`, no `Spawner`, and no
`Stack`. **The Phase-3 atomic commit spans both tiers**: it converts or `#[cfg]`s out the second
bring-up path, or the `wifi` tier goes red.

So the mandatory-atomic unit is **seven (controller, station) pairs across two tiers**, not six
across one.

### 1.5 The six paired sites (espnow), re-confirmed

| method | station | controller | transport fn |
|---|---|---|---|
| `maybe_leaf_reelect` | `mode.rs:2683` | `:2688` | `run_mqtt_burst` |
| `burst_ntp` | `:2837` | `:2842` | `run_ntp_burst` |
| `resync_ntp` | `:2866` | `:2867` | `run_ntp_resync` |
| `run_ota_update` | `:4849` | `:4855` | `run_ota_fetch` |
| `run_leaf_ota_relay` | `:5287` | `:5289` | `run_ota_fetch` |
| `flush_telemetry` | `:5829` | `:5853` | `run_mqtt_burst` |

Plus the 7th: `try_time_sync` `wifi.rs:515` / its local controller / `run_ntp_burst`.

---

## 2. Separability — confirmed, and structural

Every controller call anywhere in the transport layer:

```
is_connected() ×4   set_power_saving() ×2   connect_async() ×2   rssi() ×1   disconnect_async() ×1
```

All association-layer. **Zero packet-path calls.** (Rev-1 omitted `disconnect_async` from this
list; verdict unchanged.)

Better founded than rev-1 argued, because the signatures already encode the seam:

```
NtpMachine::step_assoc(&mut self, controller: &mut WifiController<'static>)   wifi.rs:762
NtpMachine::step_dhcp (&mut self, device: &mut SmolWifiDevice)                wifi.rs:841
NtpMachine::step_sntp (&mut self, device: &mut SmolWifiDevice)                wifi.rs:870
```

The assoc step takes the controller; the packet steps take only the device. The controller can
change owner without the station changing owner. **That seam is what creates the option space.**

### Site inventory — 19 code refs (23 matches, 4 comments), independently re-confirmed

| class | n | sites |
|---|---|---|
| paired transport | 6 | 2688, 2842, 2867, 4855, 5289, 5853 |
| assoc lifecycle | 7 | 2891, 2906 (`switch`) · 3557, 3570, 3571 (`reassoc_ch6_prefer`) · 5218, 5348 (`run_leaf_ota_relay`) |
| queries | 4 | 2741, 3448 `rssi` · 5215, 5345 `is_connected` |
| scans | 2 | 3366 `run_scan` · 3458 `reassoc_ch6_prefer` |

---

## 3. Variants considered

**A — reference shape (both move at once).** Ports `0b3eb5d` + `266dbf0` + `67cc40f` together;
Phases 2–4 land in one commit, including the `mqtt_session` rewrite. No mid-sequence revert: a
transport bug forces reverting the executor too. On this base it is RISKS §R5 with extra steps.
**Rejected.**

**B — controller-last (PORT-SPEC §2.4.5's own alternative).** Step 1 moves the 7 paired sites + all
station borrows (transport onto embassy-net, `net_task` spawned); step 2 moves the remaining 13
into `wifi_task`. Two real seams. Structurally sound, and its *ordering* is the one adopted. Its
only defect is what it implies about Phase 1 — that P1.4 is business Phase 1 owes.

**D — controller-first (not in the brief; falls out of §2).** `wifi_task` takes the controller now;
the station stays with the gateway. Delivers a genuine partial win — retires the §R6
`while !controller.is_connected()` spins so association stops freezing the superloop — without
touching the transport. **Rejected as the plan of record**, held as a contingency (§6.1): it runs an
unproven concurrency pattern (`wifi_task` associating while the superloop drives smoltcp on the
same radio) across `reassoc_ch6_prefer` — 5 of the 19 refs, three classes — for the entire duration
of the transport phase.

**C — phased-consumer. ADOPTED.** See §4.

---

## 4. Variant C, as adopted

Phase 1 binds nothing. **The station binds when its consumers move, in the transport phase, by
definition** — so PR #391 is complete as shipped rather than short by a step.

### 4.1 Sequence

| # | phase | moves | notes |
|---|---|---|---|
| 0 | **gate first** | — | §1.3's declared-count checker. Lands **before** transport work starts. |
| 1 | **done** — PR #391 | nothing further | executor beside the superloop; merge on the four HW gates |
| 2 | **transport** (atomic) | 7 paired sites + all station borrows, **across both tiers** | `mqtt_session` (~2,000 lines: #21/#56 · #153 · #309 · #324 · #217 · #188) rewritten; `try_time_sync` converted or cfg'd out; station bound; `net_task` spawned because something finally reads it |
| 3 | **controller** | the remaining 13 | `wifi_task` + mirror atomics + `WIFI_CMD` + `266dbf0` `STOP_REQ`. **Not trivial — see §4.2** |

Controller-**last**, for the one rev-1 argument that survived review intact: controller-first (D)
holds a novel, ungateable invariant across the tree's most delicate function for the whole
transport phase. That argument stands on its own and needs neither §1 nor §5.

### 4.2 Phase 3 is NOT "13 mechanical conversions" — rev-1 said so and was wrong

Rev-1 contradicted itself: §5.1 called the scan round-trip "the single largest piece of unported
design in the phase" and §6 called the step containing it "trivial". Both cannot hold. Honest
sizing:

**(a) The scan round-trip is real unported design.** `run_scan` (`3366`) and `reassoc_ch6_prefer`
(`3458`) are response-shaped — `let record = match block_on(scan_async(…))` and
`let decision = match block_on(scan_async(…))`, the latter feeding the pure `select_crown_ap` →
`CrownApDecision` which then drives `with_bssid`/`with_channel` **in the same function**. The
reference's fire-and-forget `WIFI_CMD` + result-`Signal` idiom does not cover a list-valued reply,
and these two sites **have no reference equivalent at all** — `a0d3e5a` deleted their callers and
#217r3/#278's ranked plan is main-only work written after the fork.
*Agreed shape:* `wifi_task` performs the scan, reduces it to the already-pure `ApView` list inside
the task, and signals the **reduced decision input** — keeping the selector pure and the payload
small.

**(b) Two functions straddle the phase boundary.**

| function | refs | classes |
|---|---|---|
| `reassoc_ch6_prefer` | 3448, 3458, 3557, 3570, 3571 (**5 of 19**) | query + scan + lifecycle |
| `run_leaf_ota_relay` | 5215, 5218, **5289**, 5345, 5348 (**5 of 19**) | query + lifecycle + **paired** |

`run_leaf_ota_relay` is the sharp one: `:5289` converts in phase 2 while its four sibling controller
calls stay synchronous until phase 3. It spends an entire phase in a hybrid state — async
transport, sync association, inside one function. Coherent (the controller is still
`RadioManager`-owned), but not mechanical, and nobody had written it down.

### 4.3 Invariants no variant may break

* **#217r3 / #269 / #278** — `reassoc_ch6_prefer` computes a `CrownApDecision`, applies `with_bssid`
  **and** `with_channel`, records `my_ap_channel`, bracketed by ELECT announce bursts at one epoch.
  The reference hard-pins `ESP_NOW_FIXED_CHANNEL` in `wifi_task`; porting that verbatim regresses
  all three. PORT-SPEC §2.4.3 is right and the reference is wrong here.
* **#139** `set_power_saving(None)` re-asserted after every connect · **#141**
  `assert_max_tx_power()` after driver start · **#68/#76** `self_mac` read before the station moves.
* **#278** `check_elect_send_path.py`'s declared count — update it or CI goes red by design.
* **RISKS §R13** — do not re-arm the #204 detector until `downstream_seen` is plumbed.
* **RISKS §R12 (P4-H1)** — `embassy_sync::Mutex` is not reentrant; acquire flash once and thread
  `&mut FlashStorage` through the whole OTA sequence. Brick-class.

---

## 5. Why C, on grounds that survive

### 5.1 The argument that carries the decision

Since §1 shows **nothing enforces** single-consumption, a plan's value is now measured by how small
a window it leaves in which two consumers could coexist. **C never creates a second consumer at
all** — there is no interval where a `Stack` and a `SmolWifiDevice` are both live. A and
B-as-specced (and P1.4 as originally briefed) each open that window deliberately, at a moment when
no gate can see it.

Related and unchanged: an unbound `Stack` plus a `net_task` pumping an interface nothing reads is a
correct-looking construct with no behaviour behind it. It would pass every gate and mean nothing.

### 5.2 Revert path — source-clean, fleet-dirty

At source level C's seam is genuine and is its strongest structural property: Phase 1 is small and
independently revertible, and `git revert` of the transport commit yields a coherent, compiling
tree. Rev-1 stopped there. **Three fleet-state carriers cross the seam and none is covered by a git
revert** — each is an explicit canary/rollback step in the Phase-3 plan:

1. **Retained MQTT election record.** `MC|<owner>|<ch>|<seq>` on `smol/mesh/channel` (`wifi.rs:77`,
   `:81`, parser `:348`), read against `MC_STALE_MS = 90_000` (`wifi.rs:333`) with a frozen-seq
   liveness test. **Retained topics survive a firmware revert by construction** —
   `[[smol-retained-mqtt-ghosts]]`, which defeated hardware verification four times in one night.
   If the election work adopts inc3d-2's seq-semantics change (free-running `mc_pub_seq` →
   resolve-stamped `mc_seen_seq`), a record written by the new image is then read by the old
   image's frozen-seq test after a revert. Probably survivable — seq is compared for *change*, not
   absolute value — but it is unanalysed and it is the carrier most likely to bite.
   **Rollback step:** clear the retained record and re-observe a flip **to a new value** before
   trusting any post-revert election reading.
2. **NVS.** `write_net_cfg(NetCfg { broker_fallback: true, .. })` (`wifi.rs:987`) is written from
   *inside* the transport layer and persists across OTA by design. If the async rewrite changes when
   that flag is set, a reverted image boots against a record written under rules it does not share.
   **Rollback step:** read back and, if divergent, clear net-cfg on the canary.
3. **otadata.** Reverting code does not revert boards; going back is another OTA, with
   `[[smol-espflash-otadata-trap]]` in force. **Rollback step:** check the `Loaded app from offset`
   line after every flash; `espflash erase-region 0xf000 0x2000` (otadata only, keeps NVS) if a USB
   flash must follow an OTA.

### 5.3 The `.bss` scarcity argument is WITHDRAWN

Rev-1 called this "the concrete argument, tied to a measurement":

> P1.3 left 7,960 B of margin between `.stack` (87,960) and the 80,000 B abort line.

**There is no 80,000 B abort line.** It was a tripwire in my dispatch brief — an instruction to stop
and report — and I propagated it into a design document as if it were a repo constant. It appears
nowhere in `budget.rs`, `tools/repro_build.sh`, `tools/gate.sh`, or `rust/clock/src/`.

The only gate is `ESP32C3_STACK_FLOOR_BYTES = 74_208` (`budget.rs:197`, enforced `gate.sh:358`).
**Real margin: 87,960 − 74,208 = 13,752 B** — 73% more than claimed. And the floor is itself
conservative: `ESP32C3_MEASURED_PEAK_BYTES = 55,656` was measured with the `RadioManager` frame on
the stack, and P1.3 moved ~18.9 KB of it into `.bss`, so the true peak is below the number the floor
derives from.

`StackResources<4>` plus embassy-net's buffers remain a real cost worth scheduling deliberately.
They are **not** a scarcity argument, and this document no longer makes one. (An invented threshold
quoted beside three genuinely measured section sizes reads as measured — exactly the failure mode
`[[flagged-caveat-is-not-contained]]` describes.)

---

## 6. Contingency and open items

### 6.1 The flip condition — one measurement, stated

Rev-1's condition ("if residual starvation is dominated by the association window rather than the
MQTT session") **cannot be evaluated from `BurstProbe` output.** The probe (`main.rs:408`) carries
`start_ms, last_app_ms, last_yield_ms, worst_app_gap, worst_yield_gap, paints, yields`; its only
attribution axis is `BurstKind`, and nothing tags a yield as assoc-vs-session. `worst_app_gap` is
one undecomposed scalar per burst. As written the condition could never be met or missed.

**The measurement, chosen:** add **one `mark()` at the assoc/session boundary** — the moment
`connect_async` returns — so `finish` reports the gap either side of it. That is precisely the
number the condition asks for, it is a few lines, and it produces attribution rather than inference.
If the post-`mark` split shows association dominating residual starvation, **Variant D becomes right
for the interim** and the controller moves first.

A zero-cost prior look is available and is explicitly *inference, not attribution*: contrast
`BurstKind`s already in the DIAG field — a `Reelection` (`r`) burst re-associates (`switch` →
`disconnect_async`/`connect_async`, `mode.rs:2891`/`:2906`) where a steady-state `TelemetryFlush`
(`f`) on an associated crown does not, so a large `r`-vs-`f` spread hints assoc-dominated. Useful
for deciding whether to bother with the `mark()`; **not** sufficient to flip the variant on. This
instrument already has a misattribution on record (the `brst=3009:0:r` case named in its own
`finish` comment, which is why the `worst_app_gap <= burst_ms` invariant exists) —
`[[suspect-the-instrument-first]]`.

**Also flips to D:** if `wifi_task`-associating-while-smoltcp-runs is proven safe on a board. Cheap
test — spawn a do-nothing `wifi_task` that only calls `rssi()` on a timer and check the gateway's
flush success rate is unchanged over a soak.

### 6.2 Blocking Phase-3 execution

1. **RISKS §R11 — the timebase stopwatch.** A wrong TIMG0 wiring compiles and yields embassy timers
   at the wrong rate; the symptom is not a crash. Every `WIFI_CMD` timeout and every
   #324/#136/#278 window in any variant depends on it. One stopwatch against a 15 s budget on the
   P1.3 image.
2. **The #335 round-trip rollback canary** — forward leg, **reverse leg**, and forced-rollback leg,
   with §5.2's three carriers handled explicitly.

Both need a board. Neither should stay open behind a decision this size.

### 6.3 Remaining open questions

1. §4.2(a) — is reducing to `ApView` inside `wifi_task` acceptable, or does #278's ranked plan need
   the raw scan?
2. Does the transport phase want DNS? Phase 1 omitted `embassy-net/dns` (hardcoded broker IP,
   portmap §8). If Phase 3 wants it, budget the `.bss` then — deliberately, not as scarcity.
3. `try_time_sync` (§1.4) — convert it to the Stack, or `#[cfg]` it out and let the `wifi` tier lose
   its NTP path? Decide before Phase 3 starts, not during.
