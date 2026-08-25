# P1.5 ownership proposal — adversarial review

**Target:** `scratch/embassy-port/p15-ownership-proposal.md` (Morpheus).
**Base reviewed:** worktree `/home/jp/Projects/smol-wt-p1`, `feat/335-phase1` @ `9e86030` (PR #391).
**Method:** every count re-derived from source; every crate claim read in `~/.cargo/registry`. Read-only on all code; this file is the only write.

## Verdict table

| # | claim | verdict |
|---|---|---|
| 1 | six-way pairing, exactly 1:1 | **AMENDED** — pairing is right; "exactly one owned station" is false. A 7th pair exists in a CI-gated tier. |
| 2 | controller does only association-layer work | **CONFIRMED** (strongly; the fn signatures already encode it) |
| 3 | dual-trait shim escape | **REFUTED** — the shim is available *and unnecessary*. The real blocker is elsewhere, and it is worse. |
| 4 | scans need request/response | **CONFIRMED** — and Variant C relocates it into a step it calls "trivial" |
| 5 | BurstProbe flip condition | **REFUTED as unmeasurable** — amendment supplied |
| 6 | Phase-3 atomic commit is clean-revertible | **AMENDED** — source-clean, fleet-dirty |
| — | 19-ref inventory (6/7/4/2) | **CONFIRMED exactly** |
| — | §5 "7,960 B margin to the 80,000 B abort line" | **REFUTED** — 80,000 is not a constant anywhere; real margin is 13,752 B |

**Bottom line:** the recommendation (Variant C, controller-last) **survives**, but almost none of the reasons given for it do. §1's atomicity premise is factually wrong, §5's scarcity argument is arithmetically wrong, and §6's "trivial by then" contradicts §5.1. Adopt the sequence; discard the justification and re-ground it.

---

## 1. PAIRING — AMENDED

**The part that is right, independently re-derived.** `self.sta` has exactly six live borrows in `mode.rs` (the other five `self.sta*` grep hits are `self.stat_cache` / `self.staged_raw`):

| enclosing method (verified by `awk` walk) | station | paired controller | transport fn |
|---|---|---|---|
| `maybe_leaf_reelect` | `mode.rs:2683` | `:2688` | `run_mqtt_burst` |
| `burst_ntp` | `:2837` | `:2842` | `run_ntp_burst` |
| `resync_ntp` | `:2866` | `:2867` | `run_ntp_resync` |
| `run_ota_update` | `:4849` | `:4855` | `run_ota_fetch` |
| `run_leaf_ota_relay` | `:5287` | `:5289` | `run_ota_fetch` |
| `flush_telemetry` | `:5829` | `:5853` | `run_mqtt_burst` |

Six for six, same six methods. **Confirmed.**

**The part that is wrong.** §1 asserts "`embassy_net::new` needs the station **owned**, and there is exactly one". There are **two independent station acquisitions**, in mutually-exclusive tiers:

- `mode.rs:2382` — `sta: Some(SmolWifiDevice::new(interfaces.station))`, from `RadioManager::new`. The espnow tiers.
- **`wifi.rs:515` — `let mut device = SmolWifiDevice::new(interfaces.station);`**, inside `try_time_sync` (`wifi.rs:495`), from its **own** `esp_radio::wifi::new(p.wifi, …)` at `:514`, with its **own** `controller`, feeding `run_ntp_burst(&mut controller, &mut device, …)` at `:527`.

That is a **7th (controller, station) pair**. The repo says so itself at `net.rs:329`: *"Shared by both radio-init paths (`wifi::try_time_sync` and `mode::RadioManager::new`)."*

It is gated `#[cfg(all(feature = "wifi", not(feature = "espnow")))]` — re-export at `net.rs:290`, dispatch at `main.rs:861`. So it is the **`wifi` tier**, which is a declared build-matrix tier (`tools/build-matrix.toml:93`, `features = "wifi"`) and is compiled and clippy'd by `gate.sh:245` and `:270`.

**Why it matters to the atomic move:** `run_ntp_burst` has **two** callers, not one. Rewriting it onto embassy-net sockets breaks `try_time_sync`, which has no `RadioManager`, no `Spawner` threaded to it, and no `Stack`. The transport commit must therefore also convert or `#[cfg]`-out a second bring-up path, or the `wifi` tier goes red in CI. §1's "all six transport consumers move together" should read **seven, across two tiers**.

Not fatal — but it is precisely the missed path the brief asked about, and it enlarges the mandatory-atomic commit that Variant C's whole case rests on.

---

## 2. SEPARABILITY — CONFIRMED

The complete set of controller methods called anywhere in the transport layer (`wifi.rs`):

```
is_connected()      ×4
set_power_saving()  ×2
connect_async()     ×2
rssi()              ×1
disconnect_async()  ×1
```

All five are association-layer. **Zero packet-path calls** — no send, no receive, no token access. Claim confirmed.

It is better-founded than the proposal argues, because the **signatures already encode the seam**:

- `NtpMachine::step_assoc(&mut self, controller: &mut WifiController<'static>)` — `wifi.rs:762`
- `NtpMachine::step_dhcp(&mut self, device: &mut SmolWifiDevice)` — `wifi.rs:841`
- `NtpMachine::step_sntp(&mut self, device: &mut SmolWifiDevice)` — `wifi.rs:870`

The assoc step takes the controller; the packet steps take only the device. The separability is structural, not incidental.

*Nit:* §2's method list omits `disconnect_async`. Still association-layer, verdict unchanged.

---

## 3. THE SHIM ESCAPE — REFUTED (and this is the review's main finding)

Morpheus asked for this to be "refuted rather than assumed." It refutes in the opposite direction from the one expected.

### 3a. `Interface` **is** `Copy`. §1's premise is false.

`esp-radio-0.18.0/src/wifi/mod.rs:1306–1313`:

```rust
/// Wi-Fi interface.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[non_exhaustive]
pub struct Interface<'d> {
    _phantom: PhantomData<&'d ()>,
    mode: InterfaceType,
}
```

The proposal cites this exact line (`:1310`) as "no `Clone`, no `Copy`" — it read the struct and not the derive three lines above. The repo's own code already knew: `radio_dev.rs:19–22` documents it as *"esp-radio's (Copy) STA `Interface` handle… The real rx/tx state lives in esp-radio's global packet queues; this handle is just a lightweight token source."*

**Consequence:** `embassy_net::new(interfaces.station, …)` takes it **by value from a `Copy` type — the original is not consumed.** You can bind the Stack and keep `SmolWifiDevice` at the same time. There is no type-level atomicity constraint at all. A shim is not merely possible; it is **unnecessary**.

### 3b. And no trait signature forbids a dual impl either.

| | smoltcp 0.13.1 `phy::Device` (`src/phy/mod.rs:351–378`) | embassy-net-driver 0.2.0 `Driver` (`src/lib.rs:37–67`) |
|---|---|---|
| RxToken | `type RxToken<'a>: RxToken where Self: 'a` | *identical* |
| TxToken | `type TxToken<'a>: TxToken where Self: 'a` | *identical* |
| receive | `fn receive(&mut self, timestamp: Instant) -> Option<(RxToken<'_>, TxToken<'_>)>` | `fn receive(&mut self, cx: &mut Context) -> Option<(RxToken<'_>, TxToken<'_>)>` |
| transmit | `fn transmit(&mut self, timestamp: Instant) -> Option<TxToken<'_>>` | `fn transmit(&mut self, cx: &mut Context) -> Option<TxToken<'_>>` |
| extra | — | `link_state(&mut self, cx)`, `hardware_address()` |

Structurally isomorphic. Same GAT shape, same `&mut self`, same return shape; the only per-call difference is the "nothing ready" argument (poll timestamp vs waker `Context`). Different trait names ⇒ no associated-type collision. Both impls **already exist in the tree**, on the same underlying type: `radio_dev.rs:61` (`phy::Device for SmolWifiDevice`) and esp-radio `mod.rs:1821` (`Driver for Interface<'_>`).

**There is no signature that forbids the shim.** Definitive.

### 3c. The real blocker: one global RX queue, first-poller-wins.

Both paths bottom out in the same place. esp-radio `mod.rs:1247–1261`:

```rust
fn rx_token(&self) -> Option<(WifiRxToken, WifiTxToken)> {
    let is_empty = self.data_queue_rx().with(|q| q.is_empty());
    …
    self.tx_token().map(|tx| (WifiRxToken { mode: *self }, tx))
}
```

`data_queue_rx()` is keyed **only by `InterfaceType`** (`mode: *self`, here `Station`). embassy's `Driver::receive` calls `self.mode.rx_token()` (`mod.rs:1837`); smol's `phy::Device::receive` calls `self.0.receive()` (`radio_dev.rs:71`) into the same tokens.

So two live stacks on the STA interface **pop from one queue**. A frame consumed by one is gone for the other — nondeterministic packet theft, no error, no panic.

### 3d. Why this makes things worse, not better

The conclusion "all transport consumers move together" is **correct**. But:

> The proposal believes the constraint is enforced by the **type system**. It is not. It is enforced by **nothing**.

A type-level blocker fails at compile time, loudly, in CI. A shared-queue blocker compiles green, passes all 21 gates, links, boots, and then loses packets on a fleet board under load. Anyone reading §1 would reasonably conclude the compiler has their back on the most dangerous refactor in the phase. It does not.

**This is the single biggest unlisted risk in the proposal**, and it is this repo's dominant defect shape — a correct-sounding statement with nothing behind it (`[[stubbed-intentions-under-deliver-silently]]`), reached the same way `[[zero-conflicts-raises-risk]]` describes: nothing forced a second look because nothing complained.

### 3e. Concrete mitigation, in the repo's own idiom

The tree already solves exactly this class with a declared-count structural checker: `tools/check_elect_send_path.py` asserts a declared raw-send-site count (`mode.rs:6255`), wired into `gate.sh:194` with its own regression suite at `:475`.

**Recommend an analogous gate before the transport phase begins** — assert that per feature tier there is exactly one live station consumer, i.e. no `embassy_net::new` co-existing with a `SmolWifiDevice::new`, and a declared count of `SmolWifiDevice::new` sites (currently 2: `mode.rs:2382`, `wifi.rs:515`). That converts the invisible invariant into a red CI light, which is the only form of it anyone will notice.

---

## 4. SCAN ROUND-TRIP — CONFIRMED, and Variant C hides it

Both sites are genuinely response-shaped:

- `run_scan` (`mode.rs:3366`) — `let record = match block_on(self.controller.scan_async(…))`; the caller needs the record back to publish it (#71).
- `reassoc_ch6_prefer` (`mode.rs:3458`) — `let decision = match block_on(self.controller.scan_async(…))`, filtered to the SSID and fed to the pure `select_crown_ap` → `CrownApDecision`, which then drives `with_bssid`/`with_channel` **in the same function**. The caller cannot proceed without the value.

The reference's fire-and-forget `WIFI_CMD` + result-`Signal` idiom does not cover a list-valued reply. **Confirmed, and §5.1's proposed fix (reduce to `ApView` inside `wifi_task`, signal the reduced decision input) is sound** — it keeps the selector pure and the payload small.

**But §5.1 and §6 contradict each other.** The scans are in the *controller* class, so under Variant C they land in **step 3**, which §6 describes as *"Trivial by then: the six paired sites are gone, leaving 13 mechanical conversions."* §5.1 describes the very same work as *"the single largest piece of unported design in the phase."* Both cannot hold. **Variant C does not remove this risk; it relocates it into the step it calls trivial.** Amend §6 to carry the §5.1 warning explicitly.

**Two functions also straddle the phase boundary**, which the clean 6/13 split obscures:

| function | refs | classes spanned |
|---|---|---|
| `reassoc_ch6_prefer` | 3448, 3458, 3557, 3570, 3571 (**5 of 19**) | query + scan + lifecycle |
| `run_leaf_ota_relay` | 5215, 5218, **5289**, 5345, 5348 (**5 of 19**) | query + lifecycle + **paired** |

`run_leaf_ota_relay` is the sharp one: `:5289` is a paired-transport site that must convert in the transport phase, while its four sibling controller calls stay synchronous until the controller phase. So it spends an entire phase in a hybrid state — async transport, sync association, inside one function. That is coherent (the controller is still `RadioManager`-owned), but it is not "mechanical", and nobody has written it down.

---

## 5. FLIP CONDITION — REFUTED as unmeasurable; amendment supplied

`BurstProbe` (`main.rs:408`) carries exactly: `start_ms, last_app_ms, last_yield_ms, worst_app_gap, worst_yield_gap, paints, yields`. `finish(kind: BurstKind, now_ms)` returns `(worst_app_gap, burst_ms)`.

Its only attribution axis is **`BurstKind`** — `TelemetryFlush('f')`, `NtpResync('n')`, `Reelection('r')`, `SelfOta('o')`. `yielded()` and `note_app()` record timestamps only; **nothing tags a yield as "during association" versus "during the MQTT session."**

So the stated condition — *"if the residual starvation is dominated by the association window rather than the MQTT session"* — **cannot be evaluated from BurstProbe output.** `worst_app_gap` is one scalar per burst with no intra-burst decomposition. As written, the flip condition can never be met or missed; it is unfalsifiable.

**Amendment — two ways to make it real:**

1. **Cheap, uses data the field already emits.** Contrast `BurstKind`s on the same board: a `Reelection` (`r`) burst re-associates — `maybe_leaf_reelect` → `switch(Mode::WifiSta)` → `disconnect_async`/`connect_async` (`mode.rs:2891`, `:2906`) — whereas a steady-state `TelemetryFlush` (`f`) on an already-associated crown does not. A large `r`-vs-`f` gap spread is assoc-dominated; a small one is session-dominated. This is **inference, not attribution**, and should be labelled as such — but it needs no code change and the DIAG field already carries the pair.
2. **Direct, small.** Add one `mark()` at the assoc/session boundary (the moment `connect_async` returns) so `finish` can report the gap either side of it. That is the number the flip condition actually asks for.

**Also worth pinning:** this instrument has already produced a misattributed reading — the `brst=3009:0:r` case named in its own `finish` comment, which is why the `worst_app_gap <= burst_ms` structural invariant exists (debug-assert + release warn, deliberately unclamped). Making a variant-selection decision on an undecomposed scalar from an instrument with a known misattribution history is `[[suspect-the-instrument-first]]` territory. Pick the amendment before quoting the condition again.

---

## 6. REVERT PATH — AMENDED: source-clean, fleet-dirty

At the source level the proposal is right. Variant C's seam is genuine: Phase 1 is small and independently revertible, and a `git revert` of the transport commit yields a coherent, compiling tree. **Confirmed.**

But "revertible" in this repo has to mean *the fleet can go back*, and **three state carriers cross the boundary, none of them mentioned:**

1. **Retained MQTT — the big one.** The election record is `MC|<owner>|<ch>|<seq>` on `smol/mesh/channel` (`wifi.rs:77`, `:81`, parser at `:348`), read against `MC_STALE_MS = 90_000` (`wifi.rs:333`) with a frozen-seq liveness test. **Retained topics survive a firmware revert by construction** — that is what retained means (`[[smol-retained-mqtt-ghosts]]`, which defeated hardware verification four times in one night). If the election work adopts the reference's inc3d-2 change of seq semantics (free-running `mc_pub_seq` → resolve-stamped `mc_seen_seq`), a record written by the new image is then interpreted by the old image's frozen-seq test after a revert. Probably survivable, since seq is compared for *change* rather than absolute value — but it is unanalysed, and it is the carrier most likely to bite.
2. **NVS.** `write_net_cfg(NetCfg { broker_fallback: true, .. })` at `wifi.rs:987` is written **from inside the transport layer** and persists across OTA by design. If the async rewrite changes when that flag is set, a reverted image boots against an NVS record written under rules it does not share.
3. **otadata.** Reverting code does not revert boards; going back is another OTA, with `[[smol-espflash-otadata-trap]]` in force (post-OTA the board runs `ota_1`; a later USB flash writes `ota_0` and silently never runs — check the `Loaded app from offset` line).

**Amend §5's revert bullet** to distinguish *source revert* (clean, and genuinely C's strongest structural property) from *fleet revert* (requires an explicit plan for the three carriers above). The proposal's revert analysis is entirely source-level and reads as if it covered both.

---

## 7. INVENTORY — CONFIRMED exactly

23 `self.controller` matches; 4 are comments (`2833`, `2864`, `5836`, `6123`); **19 code references**. Classified by reading each site and walking to its enclosing method:

| class | n | sites |
|---|---|---|
| paired transport | **6** | 2688, 2842, 2867, 4855, 5289, 5853 |
| assoc lifecycle | **7** | 2891, 2906 (`switch`) · 3557, 3570, 3571 (`reassoc_ch6_prefer`) · 5218, 5348 (`run_leaf_ota_relay`) |
| queries | **4** | 2741 `rssi` · 3448 `rssi` · 5215, 5345 `is_connected` |
| scans | **2** | 3366 `run_scan` · 3458 `reassoc_ch6_prefer` |

6 + 7 + 4 + 2 = 19. Matches the proposal exactly, including the individual line numbers. This part was done carefully.

---

## 8. BONUS REFUTATION — the `.bss` scarcity argument is wrong

§5 calls this "the concrete argument, tied to a measurement":

> P1.3 left **7,960 B** of margin between `.stack` (87,960) and the 80,000 B abort line.

**There is no 80,000 B abort line.** Grepped `budget.rs`, `tools/repro_build.sh`, `tools/gate.sh` and all of `rust/clock/src/` — `80_000`/`80000` appears nowhere as a threshold. The only gate is `ESP32C3_STACK_FLOOR_BYTES = 74_208` (`budget.rs:197`), parsed by `repro_stack_floor` and enforced at `gate.sh:358`.

Real margin: **87,960 − 74,208 = 13,752 B** — 73% more headroom than claimed.

And it is more conservative still. The exec log's own §"Two consequences, neither actioned" (lines 365–368) records that `ESP32C3_MEASURED_PEAK_BYTES = 55,656` was measured with the `RadioManager` frame **on the stack**, and P1.3 moved ~18.9 KB of it into `.bss` — so the derived floor is now conservative by roughly that pool.

So the argument presented as Variant C's most concrete is the one that does not survive contact with the constant it cites. `StackResources<4>` plus embassy-net buffers are a real cost worth scheduling deliberately — but the scarcity framing is not supported, and an invented threshold quoted beside three genuinely measured section sizes reads as measured. Drop it, or re-derive it against 74,208 and label the derivation.

---

## 9. Recommendation

**Keep Variant C's sequence. Replace its reasoning.**

Controller-last is right, and for the reason §6 gives that *does* hold: controller-first would run an unproven concurrency pattern (`wifi_task` associating while the superloop drives smoltcp on the same radio) across `reassoc_ch6_prefer` — which I can now confirm holds 5 of the 19 refs across three classes — for the whole duration of the transport phase. That argument stands on its own and does not need §1 or §5.

Adopt with four amendments:

1. **Re-ground §1** on queue arbitration, not ownership. State plainly that `Interface` is `Copy`, that the compiler will **not** stop a second consumer, and that the constraint is a shared `data_queue_rx()`. Then **add the CI gate** (§3e) — a declared-count check in the `check_elect_send_path.py` idiom — because an invariant no gate can see is one this repo has repeatedly discovered the hard way.
2. **Add the 7th path.** `try_time_sync` (`wifi.rs:495`) is a second (controller, station) pair in the CI-gated `wifi` tier, and `run_ntp_burst` has two callers. Decide now whether the transport phase converts it or `#[cfg]`s it out.
3. **Fix the §5.1/§6 contradiction.** The scan round-trip and the two straddling functions (`reassoc_ch6_prefer`, `run_leaf_ota_relay`) make step 3 the opposite of trivial. Size it honestly, or the phase gets planned against a number that is wrong in the expensive direction.
4. **Drop or repair the `.bss` argument** (§8), and **make the flip condition measurable** (§5) before anyone cites it in a decision.

None of these change the answer. All four change what a reader of this document believes is protecting them — which, given §3, is the part that matters.

**Unrelated but adjacent:** RISKS §R11 (TIMG0 / wall-clock timebase) is still open and the proposal's own §"Open questions" #4 says it gates everything. It is a stopwatch against a 15 s budget on the P1.3 image. It should not stay open behind a decision this size.
