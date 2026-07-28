# Embassy P2 — evidence log (Nebula, 2026-07-28)

Raw verified evidence behind `docs/superpowers/research/embassy-p2-mesh-relay.md`.
Every row was run against the trees named, at `main` = `de3376f`+ / branch = `b6413d3`.

## Verdict in one line
**P2 is not a blocker and not "work in progress" — it is UNSTARTED.** The relay on the branch is
verbatim blocking code, and its WiFi source (`run_ota_fetch`) is a stub returning `false`. The
verification plan's Phase B/C cannot run on `b6413d3` at all. Separately and more urgently: the
branch is **98 commits behind main and contains no Bard**.

## Verified claims

| # | Claim | Command / site | Result |
|---|---|---|---|
| 1 | `ota_mesh.rs` is at `src/`, NOT `src/net/` | `find . -name '*ota*'` | brief's path wrong; `rust/clock/src/ota_mesh.rs` |
| 2 | Exactly TWO ESP-NOW RX consumers exist | `grep -rn '\.receive()' rust/clock/src/` | `service()` mode.rs:5391 (drains ≤24) + relay inline (mode.rs:4412/4571/4666/4769). Mutually exclusive ONLY because the relay blocks the loop |
| 3 | `tick()` callback is radio-free | main.rs:1083-1106 | does `led.apply` + `button.poll` + `ota_screen::draw`. No `service()`, no `receive()`, no radio |
| 4 | `static mut` buffers justified by single-threadedness | ota_mesh.rs:678 | *"Alias-safe: exactly one leaf OTA at a time (canary), single-threaded, single-caller"*; `GW_OTA_WINDOW` via `addr_of_mut!` mode.rs:4515 |
| 5 | Relay spins to force PHY release | mode.rs:4504-4513 | ≤40 iterations on `is_connected()`+`disconnect()`, `settle` published as proof |
| 6 | Channel re-pinned before every OTAM | mode.rs:4384/4408/4514/4562/4642 | `set_channel(ESP_NOW_FIXED_CHANNEL)` ×5 |
| 7 | Tuning constants | mode.rs:354/356/360, wifi.rs:4538, ota_mesh.rs:564-585 | OTAN wait 800 ms · rounds max 16 · confirm 120 s · fetch budget 300 s · prearm/wake 15 s @120 ms gap · leaf stall 30 s · first-chunk grace 330 s · session max 600 s |
| 8 | Crown is deliberately mesh-silent during relay, and the protocol was redesigned around it | mode.rs:4627-4637 | leaf adds gw peer only on OTA frame or gw HELLO; gw HELLO-silent → *"bootstrap deadlock"* → OTAM **broadcast** not unicast |
| 9 | Leaf receive side is ALREADY async-shaped | ota_mesh.rs:812/907/1044 | `on_meta`/`on_data`/`tick` → `LeafAction` state machine. Only the gateway SERVE half is a blocking monolith |
| 10 | Leaf `is_active()` hold gates 4 sites + 1 peer-accept | mode.rs:2325/2403/3026/3067, 6043 | suppresses scan + re-election; widens peer accept |
| 11 | **Branch relay is NOT ported** | `git show dream/feat-embassy:…/mode.rs` :5003 | still `pub fn` + `tick: &mut dyn FnMut() -> bool`. 8 `async fn` in 8024 lines; 10 `tick()` calls |
| 12 | **Branch relay IS reachable** | branch main.rs:1036, 1733 | same two call sites as main (1078, 1682) |
| 13 | Branch main loop is async | branch main.rs:429 `async fn main`, :912 `Timer::after().await` | 3 tasks: `net_task`/`wifi_task`/`mqtt_task` (branch mode.rs:7226/7242/7402) |
| 14 | **`run_ota_fetch` on the branch is a STUB** | branch wifi.rs:1659-1675 | `log::warn!("run_ota_fetch STUBBED … Phase 5"); false` — so `ServeSource::GatewayFetch` ALWAYS returns `FetchFailed` |
| 15 | Three real stubs, not "stale comments" | branch wifi.rs:707/909/1673 | `try_time_sync` (NTP — *"clock free-runs"*), `run_mqtt_burst` (genuinely superseded by `mqtt_task`), `run_ota_fetch` (Phase 5, NOT superseded) |
| 16 | Plan §6b is CORRECT — `boot_confirm` IS wired on the branch | branch main.rs:509/962/966 + mode.rs:7930 | mirrors main 562/996/1000 + mode.rs:6464. wifi.rs:705's TODO is the **wifi-only bench build**, explicitly *"the fleet (espnow) build's boot_confirm runs from net::mode, unaffected"* |
| 17 | **Watch DOES do ESP-NOW under Embassy** | `~/Projects/esp32c6-watch/src/net/smol_mesh.rs` (896 L), `Cargo.toml:61 "esp-now"` | uses `esp_radio::esp_now::{EspNow,…}`; has RELAY/RELAYACK fragmented+ACKed+retransmitted transfer, *"byte-exact port of wire.rs encode_relay"* |
| 18 | **Watch has the arbitration verdict written down** | watch net_task.rs:563-569 | `mesh_pin_ok = radio_started && !connected && !connecting && !scanning && !scan_pending && !assoc_want() && ota.is_none()` |
| 19 | …decided in the task, executed in main | watch main.rs:2543, 2551-2565 | *"Main still executes the set_channel because the mesh owns the esp_now handle"*; re-read FRESH not from tick-start snapshot (*"review F1"*) |
| 20 | Watch relay scale ≠ smol OTA relay scale | watch smol_mesh.rs:21-22, 81-83 | ≤4 frags, ~15 s, ≤91 B chunks, leaf→gw **uplink**. smol OTA = 6,237 chunks / 98 windows / 231 B / minutes, gw→leaf **downlink** |
| 21 | Mesh-OTA wire is IDENTICAL across trees | `git diff main dream/feat-embassy --stat -- …/ota_mesh.rs` | **1 line** changed → a `main` crown can ODEL-delegate to a branch holder |
| 22 | **Branch is 98 commits behind main; Bard absent** | merge-base `36e6345`; `git rev-list --count` | 98. `grep -c bard` branch main.rs = **0**. All of `src/bard/` (2,982 L on main) shows as deleted |
| 23 | Branch is 6 days stale | `b6413d3` 2026-07-22 vs main 2026-07-28 | the 30.12 s clean build was of a tree WITHOUT the Bard's 96 KiB heap + ~75 KB stack region |
| 24 | ~~`brst=` is STRUCTURALLY crown-only~~ | — | ❌ **WRONG — RETRACTED in round 2, see rows 30-32.** `f`/`n` are crown-only but **`r` is LEAF-only**. I generalised one `!is_gateway()` guard to all three sites without checking each |
| 25 | Harness floor moved again | `5264e28` | `-F '%R'` is not a mosquitto specifier (expands empty); retain flag is `%r`. Plan §2 still says "confirm `fa2e6aa` or later" → **stale, must say `5264e28`** |

## Refuted / corrected prior claims

- ❌ *"no prior art anywhere; the watch implements no relay"* (plan §0/§0b, research §5, HANDOFF §4) —
  **too strong.** No prior art for the *OTA* relay; substantial prior art for ESP-NOW-under-Embassy
  and for the channel-arbitration decision. Rows 17-20.
- ❌ *"the 15 TODOs read as stale comments left by later increments, not real gaps"* (research §2) —
  right for `run_mqtt_burst`, **wrong for `run_ota_fetch`**. Classic DOC-UPKEEP §2: a correct
  observation over-generalised. Row 15.
- ❌ *"the #40 mesh-relay path survived the port"* (research §2) — survived **unported**, with its
  only WiFi source stubbed. Rows 11/14.
- ❌ *"P1: embassy-net sockets, an async HTTP range fetch, flash writes interleaved with the
  executor"* (plan §0) — **none of that exists on the branch.** Row 14.
- ✅ *"app-side self-rollback IS wired on the branch"* (plan §6b) — **confirmed.** Row 16.

## Inferred, NOT measured (flagged as such)

- A blocking multi-minute relay inside the executor starves `net_task` → `embassy-net` cannot move
  packets → MQTT/TCP die for the duration. Mechanism is clear; **not observed on hardware.**
- The relay probably **passes** on the branch precisely because blocking preserves its own
  invariants by starving everything else. So a green Phase C would be a **false green**.
- RAM: 3 Embassy task stacks + the Bard (96 KiB heap, ~75 KB stack region at 73% high-water) have
  never been linked together. Per `stack-is-not-headroom`, "it links" would prove nothing anyway.

---

## Round 2 — the `brst=3009:0:r` reading (2026-07-28, later)

**Handed to me as "the freeze number landed: 3,009 ms re-election freeze, trust the gap not the
duration." REFUTED. It is an artifact, and `main` had already fixed the cause.**

| # | Claim | Verified against | Verdict |
|---|---|---|---|
| 26 | `brst=3009:0:r` is a measured re-election freeze | **`5261df8`** *"fix(diag): brst= attributed gaps to bursts that never ran"*, `main.rs:1046-1058` | ❌ **REFUTED.** The commit names THIS reading. `note_burst` fired unconditionally; `maybe_leaf_reelect` *"returned immediately without doing anything at all"* → no burst ran |
| 27 | "Trust the gap, not the duration" | `fn ran() { self.yields > 0 }` (`main.rs:382`), *"a burst that never yielded never blocked"* | ❌ **INVERTED.** `dur=0` is the *tell* that the gap is misattributed. Discarding the duration discards the evidence |
| 28 | Where the 3,009 ms actually came from | `5261df8` comment | **The OTA/association path in the PREVIOUS tick** — `last_app_ms` is a tick behind at `BurstProbe::begin`. Gap **real**, cause **not re-election** |
| 29 | Fix scope | `main.rs:1055 / 1554 / 1595` | `if probe.ran()` now guards **all three** arms (r, f, n) |
| 30 | Kind `r` is leaf-only | `mode.rs:2396` — `if self.relay.is_gateway { return false; }`, *"only leaves recover here"* | ✅ **team-lead CORRECT.** A leaf re-election re-associates to WiFi → leaves DO burst |
| 31 | Kinds `f`/`n` are crown-only | `relay_ready_to_flush` → `mode.rs:3912` `if !is_gateway && !debug_wifi_all { return false }` | ✅ crown-only **with an escape hatch** — `debug_wifi_all` makes a leaf flush too |
| 32 | **My own round-1 §5 claim** — *"`brst=` is structurally crown-only"* | rows 30-31 | ❌ **WRONG, RETRACTED.** I found one `!r.is_gateway()` guard near the flush region and generalised it to all three `note_burst` sites without checking each. Same DOC-UPKEEP §2 error I criticised in the prior doc — self-inflicted |

**What survives and is worth more than the number:** the 3 s gap was **real** and fell in a path that
**none** of the three instrumented arms cover. **The instrumentation has a coverage hole, and the only
large gap ever observed landed in it.** If JP's freeze lives in the OTA/association path, instrumenting
flush and re-election harder will keep missing it.

**Structural point that survives both errors:** the crown-only kinds (`f`/`n`) are the **visible** ones
(crown self-publishes a full record); the leaf-only kind (`r`) is the **truncated** one (232 B, #306).
Instrumentation and truncation are biased against each other. id8's record escaped only because Nexus had
just associated for an OTA self-fetch and published its **own** full record — *the leaf's freeze metric
was visible only because the leaf had temporarily stopped behaving like a leaf.*

⚠️ **The bad value is RETAINED on the broker.** The next subscriber will re-read it and re-derive the
wrong conclusion. Any `brst=` with a `0` duration is a pre-`5261df8` artifact.

**Benefit 1 remains a code-reading. P2 verdict unchanged** (feasibility never depended on this).
