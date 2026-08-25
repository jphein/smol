# PORT-SPEC — Embassy Phase 1 onto today's `main`

**Scope:** executor ON + `net_task`/`wifi_task` split + ch6 hold.
**Base:** `main` @ `9c36a25` (2026-08-24). **Reference:** `dream/feat-embassy` @ `b6413d3`. **Merge-base:** `36e6345` (2026-07-21).
**Status of this doc:** read-only recon. Nothing was built, flashed, or committed. Every claim below is either cited to a file:line / commit I read, or explicitly flagged UNVERIFIED.

---

## 0. Two corrections to the briefing, up front

These change the plan, so they lead.

### 0.1 `main` is **edition 2021 / rust-version 1.96**, not edition 2024

The brief states main took "edition 2024" via PR #361. It did not.

```
git show main:rust/clock/Cargo.toml       → edition = "2021", rust-version = "1.96"
git show dream/feat-embassy:…/Cargo.toml  → edition = "2024", rust-version = "1.88"
```
(Working tree agrees with `main`.)

So the edition-2024 migration — reference commit **`d253db2`** (`Phase 0c′ inc7 — edition 2021→2024; all tiers green`) — is **still unported work** and belongs in the Phase 1 entry gate, not in the "already landed" column. It is small (Cargo.toml 2 lines + main.rs 1 line on the reference) but edition 2024 changes `unsafe_op_in_unsafe_fn`, RPIT lifetime capture, and `gen`/`static mut` rules, so it can surface diffuse breakage across a 2,488-line `main.rs` and a 7,569-line `mode.rs` that never saw it. Note also the reference *lowers* `rust-version` to 1.88; main's 1.96 should be kept (higher floor, and 1.96 is what the toolchain gates on today).

**Recommendation:** land edition 2024 as its own commit *before* the executor flip, so an edition breakage and an executor breakage can never be confused for each other.

### 0.2 The reference branch is **not a superset of main** — Phase 0c′ deleted the gateway

This is the single most important structural fact and the brief does not state it.

`net/wifi.rs` line counts:

| rev | LOC | note |
|---|---|---|
| `36e6345` (fork) | 5,135 | hand-driven smoltcp stack |
| `dream/feat-embassy` | **1,675** | stack excised, entry points **stubbed** |
| `main` | **6,158** | stack retained + hardened for ~300 commits |

Reference commit **`a0d3e5a`** ("Phase 0c′ source migration") removed **4,023 lines** from `wifi.rs`. What went:
`create_interface`, `smoltcp_now`, `NtpMachine` (base:614–880), **`mqtt_session` (base:2025–4018, ~2,000 lines)**, `cast_stream`, `publish_ota_progress`, `apply_dhcp`, `tcp_send`, `recv_into`, `MqttScratch`/`JsonScratch`, and the NTP socket storages.

What survives on the reference is the **data model** (`MeshElect`, `GwOwnCfg`, `CfgCache`, `RelayCache`, `ResetReq`, `ScanReq`, `NotifyReq`, `RelayDiag`, `ota_fail`) plus **stubs**:

```rust
pub fn run_mqtt_burst(…30 args…) -> bool {
    log::info!("smol #198: run_mqtt_burst STUBBED (async MQTT flush task in Phase 3)");
    false
}
pub fn run_ota_fetch(…) -> bool {
    log::warn!("smol 0c′: run_ota_fetch STUBBED (OTA HTTP fetch -> embassy-net in Phase 5)");
    false
}
pub fn run_ntp_burst(…)  -> Option<u32> { … "VESTIGIAL" …; None }
pub fn run_ntp_resync(…) -> Option<u32> { … "VESTIGIAL" …; None }
```

This is textbook `[[stubbed-intentions-under-deliver-silently]]`: correct comments describing behaviour the binary does not have. **They never fail — they return `false`/`None` and the caller reads that as "flush didn't work this window".**

**Consequence for the port:** you cannot cherry-pick the reference's Phase-1 commits onto main and get a working image, because `67cc40f` (inc1) *depends on* `a0d3e5a` having already removed the smoltcp callers. Porting `a0d3e5a` verbatim would **delete main's live gateway**: `mqtt_session` on main has since absorbed #21/#56 config-cache convergence, #153 diag, #309 discovery, #324 election gate, #217 `apch=`/`cc=` diagnostics and #188 OTA completion.

**Therefore Phase 1 must be re-planned as "executor ON *underneath* the existing synchronous gateway", not "port 0c′ then 1".** See §3.

---

## 1. What Phase 0c′/1 actually is on the reference branch

14 commits, `1c57ad0` … `34b9c6c`. Churn:

| commit | subject (trimmed) | files | +/- |
|---|---|---|---|
| `1c57ad0` | #233 matched-set manifest bump | Cargo.{toml,lock} | +337/−228 |
| `dd8fa5d` | executor-first manifest — esp-rtos embassy ON | Cargo.{toml,lock} | +360/−5 |
| `a0d3e5a` | **0c′ source migration** (deletes smoltcp) | 7 src | +416/−4081 |
| `d253db2` | **edition 2021→2024** | Cargo.toml, main.rs | +4/−3 |
| `770c549` | 0c′ inc8 GATE — clippy -D clean (5 tiers) | main.rs, net.rs, wifi.rs | +18/−19 |
| `dc3e7b6` | 0c′ inc9 — defmt-rtt canary | cfg, Cargo, main.rs | +86/−3 |
| `277bae6` | **revert** of `dc3e7b6` (team-lead ruling) | same | +1/−73 |
| `67cc40f` | **P1 inc1 — net_task + embassy-net Stack** | 6 | +277/−192 |
| `0b3eb5d` | **P1 inc2 — wifi_task + DR-H1 controller split** | 3 | +203/−39 |
| `45eea58` | **P1 inc3 — yield the mesh loop on Timer::after** | main.rs | +14/−4 |
| `6b94bb9` | P1 inc4 — canary observation logs | mode.rs | +28/−17 |
| `03a09c4` | **P1 — hold ch6 during the WiFi window** | mode.rs | +13/−0 |
| `266dbf0` | P1 — undroppable STOP_REQ teardown | mode.rs | +110/−88 |
| `34b9c6c` | P1 inc0 — restore defmt-rtt canary (cfg-gated) | 5 | +122/−0 |

`dc3e7b6` + `277bae6` cancel out; `34b9c6c` is the surviving canary, behind a dedicated `defmt-canary` feature so fleet builds link zero defmt.

### The Phase-1 mechanism, in one paragraph

`#[esp_rtos::main] async fn main(spawner: Spawner)` replaces `#[main] fn main()`. `esp_rtos::start(timg0.timer0, sw_int.software_interrupt0)` (reference main.rs:452) wires the embassy time-driver + executor. `RadioManager::new(p, id, spawner)` consumes `interfaces.station` into `embassy_net::new(…)` and spawns three tasks (reference mode.rs:2972–2990): `net_task` (pumps `runner.run().await`), `wifi_task` (**sole owner of the `WifiController`**), `mqtt_task` (parked on a Signal). The superloop stays inline in `main`, but paces on `Timer::after(SUBTICK_MS).await` instead of `Delay::delay_millis`, so the executor interleaves the tasks between mesh ticks. Cross-task state is mirror atomics + embassy-sync primitives (reference mode.rs:90–174): `LINK_UP`, `AP_RSSI`, `WIFI_BUSY`, `STOP_REQ`, `WIFI_CMD: Channel<…,4>`, `NTP_RESULT`/`MQTT_OPEN`/`MQTT_DONE`/`FLUSH_RESULT: Signal`, `ROLE_IS_GATEWAY`, `WANTS_ELECT`.

---

## 2. Per-hunk verdicts against today's `main`

### 2.1 Manifest — `rust/clock/Cargo.toml`

Feature-set diff (computed, not asserted):

- **on `main` only** (port must NOT regress): `bard`, `ledger-provision`, `off-fleet`, `stack-paint`, and the two `required-features` stanzas (`[[bin]] clock` + `[[example]] bard_stories`).
- **on reference only** (port must ADD): `defmt-canary`, `phase2-measure`.

| item | main today | reference | verdict |
|---|---|---|---|
| `edition` / `rust-version` | 2021 / 1.96 | 2024 / 1.88 | **port-verbatim (edition only)**, keep main's 1.96 — see §0.1 |
| `esp-rtos` features | `["esp32c3","esp-radio","esp-alloc","log-04"]` | `["esp32c3","embassy","esp-alloc"]` + `esp-rtos/esp-radio` via `wifi` | **re-derive** — union both: `embassy` ON *and* keep `esp-radio` + `log-04` |
| `embassy-executor` 0.10, `embassy-time` 0.5 (`log`), `static_cell` 2.1 | absent | present, ride `hw` | **port-verbatim** |
| `embassy-net` 0.9.1 (`tcp,udp,dhcpv4,medium-ethernet,log`) | absent | rides `wifi` | **port-verbatim** (Phase 1 needs the `Stack`; DNS deliberately off) |
| `embassy-sync` 0.7 | absent | rides `wifi` | **port-verbatim** |
| `portable-atomic` | absent as a direct dep | direct, `wifi`-gated, no features | **port-verbatim + audit** — see RISKS.md §R4 (`[[smol-portable-atomic-riscv-pin]]`) |
| `embassy-futures` (`block_on`) | present, drives the sync path | **dropped** | **KEEP on main** — see §3; dropping it is Phase 3+, not Phase 1 |
| `smoltcp` 0.13 direct | present | **dropped** | **KEEP on main for Phase 1** — `mqtt_session` still needs it |
| `esp-wifi-sys-esp32c3` (aliased) | present (#141 TX clamp, AP-info readback) | dropped | **KEEP** — obsolete-on-reference only because 0c′ deleted its callers |
| `defmt` / `defmt-rtt` 1.x behind `defmt-canary` | absent | present | **port-verbatim** (`34b9c6c`) |
| `sigil-names` path dep, `libm`, `[[bin]]`/`[[example]]` required-features | present | absent (predates) | **do not touch** — reference is simply older |

> The reference's `esp-radio` feature list also drops `log-04`. That is a *regression on main*, which uses `log-04` for the log bridge. Keep main's.

### 2.2 `.cargo/config.toml` — mostly **obsolete**

Reference commit `67cc40f` rewrites the `[env]` block, claiming the #140 RX knobs moved from compile-time `ESP_WIFI_CONFIG_*` to runtime builders in `RadioManager::new`.

**Main already did this, in a different place.** `rust/clock/src/net.rs:331–337`:

```rust
pub(crate) fn radio_controller_config() -> esp_radio::wifi::ControllerConfig {
    esp_radio::wifi::ControllerConfig::default()
        .with_static_rx_buf_num(16).with_dynamic_rx_buf_num(40)
        .with_rx_queue_size(8).with_rx_ba_win(12)
}
```
called from `wifi.rs:516` and `mode.rs:2353`. And `AllChannels` scan lives at `wifi.rs:792`.

**Verdict: OBSOLETE.** Main's factored `radio_controller_config()` is strictly better than the reference's inline builder chain (one definition, two call sites). Take from `67cc40f` only:

```toml
[env]
DEFMT_LOG = "info"
```
…and only together with `34b9c6c`'s `defmt-canary` feature.

### 2.3 `src/main.rs`

| ref hunk | what | main anchor | verdict |
|---|---|---|---|
| `d253db2` | edition-2024 fallout (1 line) | — | port-verbatim |
| `67cc40f` / `dd8fa5d` | `#[main] fn main() -> !` → `#[esp_rtos::main] async fn main(spawner: Spawner) -> !` | `main.rs:576–577` (`#[main]` / `fn main() -> !`) | **re-derive** — mechanically identical, but see RISKS §R1/§R2 |
| `67cc40f` | `esp_rtos::start(timg0.timer0, sw_int.software_interrupt0)` hoisted into `main` | today `esp_rtos::start` is called **late and twice**: `net/wifi.rs:510`, `net/mode.rs:2346` | **re-derive** — must move to `main` and become single-call; the two existing sites are mutually exclusive radio-init paths and both must lose it |
| `45eea58` | retire `Delay`, pace on `Timer::after(SUBTICK_MS).await` | `main.rs:702` (`let delay = Delay::new();`), `main.rs:1050` (splash), `main.rs:2408` (loop tail) | **port-verbatim** — all three sites exist unchanged on main, `SUBTICK_MS = 20` at `main.rs:288` |
| `34b9c6c` | defmt-canary + `log`→`defmt` bridge, `build.rs` | additive | port-verbatim |

`45eea58` is the highest value-per-line change in the whole phase (3 hunks, +14/−4) and it ports clean.

### 2.4 `src/net/mode.rs` — the real work

**2.4.1 Mirror atomics + sync primitives** (ref mode.rs:90–174, from `0b3eb5d`+`266dbf0`) — **port-verbatim**, additive, no main-side collision. Includes the `266dbf0` `STOP_REQ` correction (Oracle §2.5 POINT-2: undroppable level-flag teardown, `swap`-consumed, checked *before* the benign open). Port `266dbf0` folded in, never the pre-`266dbf0` shape.

**2.4.2 `net_task`** (ref mode.rs:7226–7231) — 5 lines, **port-verbatim**.

**2.4.3 `wifi_task`** (ref mode.rs:7242–7400) — **re-derive.** The body as written is Phase-1+2+3 fused (it contains the `ntp_sync` call, the `MQTT_OPEN`/`MqttWindow::Flush|ElectObserve` signalling and the `MQTT_DONE` teardown wait). For a Phase-1-only landing, take: `WIFI_CMD.receive()`, the `STOP_REQ` race consume, `WIFI_BUSY` bracketing, `set_config` + `set_power_saving(None)` (#139), `with_timeout(15s, connect_async())`, the `LINK_UP`/`AP_RSSI` mirror loop on a 500 ms `Timer`, and `disconnect_async` teardown. **Drop** the NTP/MQTT arms until Phases 2/3.

**⚠️ The association config is a genuine conflict, not a merge conflict.** The reference hard-pins the STA:

```rust
StationConfig::default().with_ssid(…).with_password(…)
    .with_channel(assoc_channel)          // = ESP_NOW_FIXED_CHANNEL
    .with_scan_method(ScanMethod::AllChannels)
```

Main no longer wants a static pin. `mode.rs:3558–3584` (`#217r3` crown reassoc) computes a `CrownApDecision` (`CoChannel{ch}` / `OffChannelFallback{ch}` / `NoAp`), applies `with_bssid(b)` **and** `with_channel(c)` from that decision, records `self.my_ap_channel`, and brackets the move with #278 ELECT announce bursts at a single epoch (before *and* after the switch). Hard-pinning ch6 in `wifi_task` would **regress #217r3 / #269 / #278**. See DELTA-MAP.md §2.

**2.4.4 `RadioManager::new` → `(p, id, spawner)`** (ref mode.rs:2912, spawns at 2975/2981/2988) — **re-derive.** Port the `embassy_net::new` bring-up verbatim, including two details worth keeping:
- **DR-M3 seed**: `let seed = ((rng.random() as u64) << 32) | (rng.random() as u64);` — per-boot entropy. A shared literal would give the whole fleet identical TCP ISNs and ephemeral ports.
- `let self_mac = interfaces.station.mac_address();` **must be read before** `interfaces.station` is moved into embassy-net (#68/#76 self-frame drop).
- Spawn errors are `log::error!`, never `.expect()` — smol's boot path is panic-free by policy (a panic → MF-2 `software_reset` → boot loop).

For Phase 1, spawn `net_task` + `wifi_task` only. **Do not** spawn `mqtt_task` (Phase 3).

**2.4.5 The controller move — the largest single collision.**

The reference moves `WifiController` *into* `wifi_task`; `RadioManager` no longer holds it. Reference has **2** `self.controller` references. **Main has 23**, at:

```
2689, 2742 (rssi), 2834, 2843, 2868, 2892 (disconnect_async), 2907 (connect_async),
3367, 3449, 3459, 3558 (disconnect_async), 3571 (set_config), 3572 (connect_async),
4856, 5216 (is_connected), 5219 (disconnect_async), 5290, 5346 (is_connected),
5349 (disconnect_async), 5837, 5854, 6124
```

and main additionally threads a `(&mut self.controller, sta)` pair into `wifi::run_ntp_burst` / `run_mqtt_burst` / `run_ota_fetch` (e.g. `2843`, `2868`, `4856`, `5290`, `5854`). That calling convention has no Phase-1 equivalent.

**Verdict: re-derive, and stage it.** A verbatim controller move requires Phases 2–4 to already exist, because it orphans every one of those 23 sites at once. See §3 for the staging that avoids this.

**2.4.6 ch6 hold** (`03a09c4`, +13 lines) — **port-verbatim with a re-anchor, but gate it.**

Inserts immediately after the `is_gateway` early-return, before the `ota_leaf.is_active()` arm:
```rust
if WIFI_BUSY.load(Ordering::Relaxed) { return; }
if LINK_UP.load(Ordering::Relaxed) { let _ = self.esp_now.set_channel(ESP_NOW_FIXED_CHANNEL); return; }
```
The anchor is intact on main — `leaf_scan_tick`, `mode.rs:2475`, with the comment `// gateway owns its channel via association; never scans` at **`mode.rs:2485`** (reference tip: 3099). The insertion point is unambiguous.

**But the surrounding function has changed a lot** and the second line is now questionable. Between the anchor and the old body, main added: `scan_plan()` (#278 ranked probe plan seeded from the last accepted announcement, `FOLLOW_ENABLED`-gated), the #278 PROBATION unlock arm (`elect_follower.probation_expired`), and #126 `ChannelPark`. Hard-pinning `ESP_NOW_FIXED_CHANNEL` whenever `LINK_UP` now fights the ranked plan on a node whose crown legitimately moved channel. Port the `WIFI_BUSY` guard as-is (it is pure "don't touch the radio mid-assoc" and is unambiguously right); make the `LINK_UP` pin conditional on the crown actually being co-channel. DELTA-MAP §2 has the detail.

**2.4.7 canary logs** (`6b94bb9`) — **port-verbatim.** Logs BSSID/channel/`co_channel=` at assoc. Cheap and it is the Phase-1 acceptance evidence.

### 2.5 `src/net/wifi.rs`, `src/net.rs`, `src/ota.rs`, `src/about.rs`, `src/ota_mesh.rs`

All reference changes here come from `a0d3e5a` (the stack excision) and its clippy follow-up `770c549`.

**Verdict: OBSOLETE for Phase 1 — do not port.** Keep main's `wifi.rs` (6,158 lines) intact. The Phase-1 goal is the executor and the task split; the transport rewrite is Phases 2–4. `a0d3e5a`'s `ota.rs` (+254) changes are entangled with the `ImageWriter` rework and should be re-read against main's #188/#267 OTA work when Phase 4 is planned — not now.

---

## 3. Recommended landing sequence

The reference's own order (0c′ deletes the stack → Phase 1 builds the async one) is **not available to us**, because on our base the thing 0c′ deletes is a hardened, shipping gateway (v917). Invert it: bring the executor up *beside* the synchronous path, and let later phases move traffic across.

| step | content | risk |
|---|---|---|
| **P1.-1** | 🔴 **Not part of Phase 1 — do it first, separately.** Restore the I2C bus + software timeouts lost in the #233 dep wave (2 lines + 1 import, from the reference's `main.rs:532–535`). This is a **live hazard on today's fleet**, not a port concern. See RISKS §R0. | trivial fix, real bug |
| **P1.0** | edition 2021 → 2024 (`d253db2`), keep `rust-version = 1.96`. All tiers green. | low, diffuse |
| **P1.1** | Manifest: add `embassy-executor`/`embassy-time`/`static_cell`/`embassy-sync`/`embassy-net`/`portable-atomic`; add `embassy` to `esp-rtos` features **keeping `esp-radio` + `log-04`**. **Keep** `smoltcp`, `embassy-futures`, `esp-wifi-sys`. | **medium — this is the links-but-dies step** |
| **P1.2** | `#[esp_rtos::main] async fn main(spawner)`; hoist `esp_rtos::start` out of `wifi.rs:510` / `mode.rs:2346` into `main`. **Everything else still synchronous.** `block_on` still drives the radio futures. | **highest — see RISKS §R1** |
| **P1.3** | `45eea58` verbatim (3 sites): `Delay` → `Timer::after(SUBTICK_MS).await`. | low |
| **P1.4** | `net_task` + `embassy_net::new` in `RadioManager::new` (DR-M3 seed, `self_mac` before the move). Stack exists, **nothing uses it yet.** | low-medium (`.bss` growth — RISKS §R3) |
| **P1.5** | `wifi_task` + mirror atomics + `WIFI_CMD` + `266dbf0` STOP_REQ. **This is where the 23 `self.controller` sites must be resolved** — either the task owns the controller and the sync paths go through `WIFI_CMD`, or the split lands controller-last. | **highest structural** |
| **P1.6** | `03a09c4` ch6 hold (`WIFI_BUSY` guard verbatim; `LINK_UP` pin re-derived against #278) + `6b94bb9` canary logs + `34b9c6c` defmt-canary. | low |

P1.0–P1.4 are individually revertible and each keeps a shipping image. P1.5 is the one that cannot be half-landed.

---

## 4. Phase 1 acceptance evidence — use the instrument main already has

Main carries `BurstProbe` (#153/#198, `main.rs:~366–440`), which measures per-burst superloop starvation: `burst`, **`longest app gap`**, `longest yield gap`, paints, yields. Its own doc comment states the case exactly:

> "The Embassy research doc's honest gap was that `SUBTICK_MS` starvation on `main` had never been MEASURED, only inferred from the mesh deaf-window's mechanism… `longest yield gap` is the floor: the app cannot be serviced more often than the radio yields, so if that number is large, no repaint cadence can help and only the Embassy re-platform (#198/#233) can."

**So Phase 1's benefit is already instrumented on main and needs no new tooling.** Capture `BurstProbe` output on v917 (pre) and on the Phase-1 image (post) on the same board under the same duty. That is the number that justifies the phase — and per `[[gate-that-cannot-fail]]`, ask for **the number**, not "green".

Do not accept a Phase-1 sign-off whose evidence is "it links" or "clippy is clean". See RISKS.md §R1.

---

## 5. Open items I could not verify read-only

1. **Current `.stack` region size.** The brief cites 106,464 B. `repro_stack_check` derives the floor by parsing `budget.rs` (verified: `ESP32C3_STACK_FLOOR_BYTES = 74_208`, `rust/clock/src/budget.rs:197`, with a compile-time assert against `ESP32C3_MEASURED_PEAK_BYTES = 55_656` at `:206`/`:213`) but the *region* number requires a release build + `readelf`. **Unverified by me.** Re-derive with `tools/repro_build.sh` → `repro_stack_check` before quoting it.
2. **Whether `esp-rtos` 0.3 with `embassy` + `esp-radio` co-enabled resolves.** The reference enables `embassy` without `esp-radio` at base and adds `esp-radio` via the `wifi` group; main enables `esp-radio` without `embassy`. The union is what P1.1 needs and nobody has built it. This is the first thing to try, and it is cheap.
3. **`embassy-executor` 0.10 task-arena sizing** and where per-task stacks come from under `esp-rtos`. Central to RISKS §R3.
