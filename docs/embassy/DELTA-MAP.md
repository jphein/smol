# DELTA-MAP — reconciling reference phases 2–4 against `main` @ `9c36a25`

Reference: `dream/feat-embassy` @ `b6413d3`, forked `36e6345` (2026-07-21). ~300 main commits since.

**Headline:** the collision the brief anticipated — "the NEW main-side ELECT frame vs the reference's 100% broker-mediated election" — **is not a collision.** The two live on orthogonal planes. That materially de-risks Phase 3. What *is* badly collided is Phase 3's transport (main's `mqtt_session` moved +1,141/−118 across 19 issues since the fork) and Phase 2's purpose (main grew a native instrument that partly supersedes the harness).

---

## 1. ELECT-frame vs broker-mediated election — **no conflict; the reference's model still holds**

### The finding

`main`'s crown election is **still 100% broker-authoritative.** #278's `net/mesh_elect.rs` (946 lines) elects nothing — it announces the mesh **channel**, derived from whichever board the broker already crowned, and on `main` it is **observe-only**.

Verified independently, not taken from commit messages:

| claim | evidence |
|---|---|
| The follow path is off | `pub const FOLLOW_ENABLED: bool = false;` — `rust/clock/src/net/election.rs:158` |
| Crown authority is the retained broker record | `relay.is_gateway` is assigned at `mode.rs:2764`, `:2774`, `:5952`, `:6140`, `:6198`, `:7514`; the seeding one is `radio.relay.is_gateway = reached_dhcp && elect.i_am_owner` (`mode.rs:7514`), where `elect` is the `MeshElect` resolved from the retained `MC\|<owner>\|<ch>\|<seq>` payload inside `mqtt_session` |
| The ELECT frame's `gateway` field changes no state | `grep -n "\.gateway" mode.rs` → the only `Frame::Elect`-sourced uses are `mode.rs:6595` and `:6603`, both `log::info!` format arguments |
| Only the crown announces | `elect_tick` early-returns unless `self.relay.is_gateway` — `mode.rs:3008` |

So the reference branch's Phase-3 design — **broker-mediated MC election, OBSERVE + RESOLVE against the retained record** — is still the correct model for `main`. It was not superseded. Reference commits `3c62d4b` (OBSERVE + PUBLISH plumbing + `got_mc`), `ce0f34b` (non-gateway election-OBSERVE burst, `WANTS_ELECT` carve-out), `b6413d3` (RESOLVE port) all remain **conceptually valid**.

### The two planes

| | **crown / gateway** | **mesh channel** |
|---|---|---|
| authority | retained MQTT `smol/mesh/mc` | nobody, on `main` today |
| decided by | `MeshElect` resolver in `mqtt_session` (`wifi.rs:~4300–4620`) + pure core `net/election.rs` | `mesh_elect::Announcer`, derived from `learned_channel` → `my_ap_channel` → `ESP_NOW_FIXED_CHANNEL` |
| on the wire | nothing — broker only | `SMOLv1 ELECT`, 61 B fixed-width ASCII, group-MAC'd |
| acted on? | yes, always | **no** — `FOLLOW_ENABLED = false`; the mesh still rendezvouses on `ESP_NOW_FIXED_CHANNEL` with the `[1,6,11]` legacy ladder |

`net/election.rs` and `net/mesh_elect.rs` do not call each other in either direction. Their only coupling is the single `FOLLOW_ENABLED` bool, which the *callers* read (`mode.rs:3043`, `:4230`, `:6584`) and which also selects `MetricWeights::DOMINANT` vs `FOLLOWING`.

### What Phase 3 must therefore add — and what it must not break

**Add (unchanged from the reference):** the OBSERVE + RESOLVE port, `WANTS_ELECT`, `MC_OBSERVED`, `MqttWindow::{Flush, ElectObserve}`, and the `set_gateway` single-writer discipline (reference `mode.rs` DR-M4 — "`set_gateway` is the SOLE writer; this was the 6th site"). Note main has **six** `is_gateway` write sites, so that discipline is *more* needed now, not less.

**Must not break:** the Layer-2 crown-migration predicates that landed after the fork and live in the pure core — `seize_off_channel_owner` (`election.rs:311`), `yield_to_co_channel_owner` (`election.rs:329`), `refuse_leaf_lock_off_channel` (`election.rs:359`) — and the `smol/mesh/elect` operator lever with its dominance clamp (`election.rs:458–473`). These are resolved *inside* `mqtt_session`'s MC branch. An async re-implementation that reads the retained record but skips these arms would silently regress the co-channel seize.

### The one real interaction: `co_channel`

`co_channel` is a **bool** fitness input ("my AP channel == mesh channel", `election.rs:31`) whose **weight** is now follow-coupled (`d721d3a`):

| weights | `co_channel` | `rssi` | `uptime` | `max_fitness()` |
|---|---|---|---|---|
| `DOMINANT` (shipped, `follow=false`) | **100** | 10 | 1 | 122 |
| `FOLLOWING` (`follow=true`) | **10** | 10 | 1 | 32 |

Under `DOMINANT`, `co_channel` alone (100) outranks everything else combined (22) — a veto. Selection is `MetricWeights::default_for(follow)` (`election.rs:111`), so weights cannot be obtained without stating which world you are in.

**Phase-1 consequence, and it is the sharp edge:** the reference's `wifi_task` hard-pins the STA to `ESP_NOW_FIXED_CHANNEL` (`with_channel(assoc_channel)`), which would make `co_channel` **permanently true** — quietly disabling a veto the fleet's crown-migration logic depends on. See §2.

---

## 2. The ch6 hold vs main's #217r3 / #269 / #278 crown migration — **genuine conflict**

The reference (`03a09c4`, and the `wifi_task` assoc config) assumes a static world: the mesh is on ch6, so pin the STA to ch6 and hold it.

Main no longer lives there. `mode.rs:3532–3602` (`reassoc_ch6_prefer`) computes a `CrownApDecision` (`CoChannel{ch}` / `OffChannelFallback{ch}` / `NoAp`, pure core in `net/coexist.rs:select_crown_ap`), applies `with_bssid(b)` **and** `with_channel(c)` from it (`mode.rs:3567–3571`), records `self.my_ap_channel`, and brackets the move with #278 ELECT announce bursts at one epoch — before *and* after the switch, CSA-shaped.

| reference hunk | verdict |
|---|---|
| `if WIFI_BUSY { return; }` — don't touch the radio mid-assoc | **port-verbatim.** Unambiguously correct; the controller owns the channel during `connect_async`. |
| `if LINK_UP { set_channel(ESP_NOW_FIXED_CHANNEL); return; }` | **re-derive.** Pin to the crown's *actual* channel, not the constant. Gate on the `CoChannel` decision. |
| `wifi_task`'s `with_channel(ESP_NOW_FIXED_CHANNEL)` static assoc pin | **re-derive.** Must consume `CrownApDecision`, or it regresses #217r3/#269 and falsifies `co_channel` (§1). |
| `.with_scan_method(ScanMethod::AllChannels)` | **obsolete** — already on main at `wifi.rs:792`. |

The anchor for `03a09c4` survives: `leaf_scan_tick`, `mode.rs:2475`, comment at `:2485`. But the function around it gained `scan_plan()` (#278 ranked probe ladder), the #278 PROBATION unlock arm (`mode.rs:2515`), and #126 `ChannelPark`. A blind `LINK_UP` pin fights all three.

---

## 3. Phase 2 (deaf-window harness) — **partly superseded; re-scope, don't re-port wholesale**

11 commits (`97690b7` … `7974c9a`), ~700 lines, almost entirely additive behind the `phase2-measure` feature: a measurement tracker, a runner with roles and a run-matrix (`SMOL_P2_STA_CHANNEL`, `SMOL_P2_BLOCK_MODE`), measurement-board isolation (non-electing, ch6-pinned, advertisement-silent so the fleet stays election-inert), and a blocking-mode emulation to compare spin vs skip.

**What changed underneath it:** on **2026-07-28** — seven days *after* the fork — main gained `BurstProbe` (`07aa3a7`, "#153 measure the burst freeze instead of reasoning about it"). It is **not on the reference branch** (`git show dream/feat-embassy:…/main.rs | grep -c BurstProbe` → `0`). It reports `burst`, `longest app gap`, `longest yield gap`, paints and yields — on the **production** path, not a special tier.

### What Phase 2 actually measured — and what it did not

The primary metric was **`steady_max_gap_ms`** (longest single deaf stretch), segmented at `assoc_done` so the scan/assoc transient — a deaf window *neither* lever fixes — could not contaminate the steady-state number. Beacon period was tightened to 50 ms because a 500 ms beacon aliased any sub-500 ms deaf window to 0–1 missed beacons.

| quantity | value | status |
|---|---|---|
| Run-1 (co-channel, both levers ON) `steady_max_gap_ms`, max over 10 windows | **169 ms** | ✅ **MEASURED** |
| Run-0 (no WiFi window) ambient control floor | **279 ms** | ✅ **MEASURED** |
| Run-1 `beacon_rx phase=STEADY` count | **2027** | ✅ **MEASURED** |
| Run-1 `link_held` | true, all 10 windows | ✅ **MEASURED** |
| scan/assoc transient (correctly bucketed out of steady) | 1.6 – 2.7 s | ✅ **MEASURED** |
| Run-2 (off-channel) | — | ❌ **NEVER RUN** — deferred, needed a ch1 AP |
| Run-3 (blocking baseline) `steady_max_gap_ms` ≈ 15,000 ms | — | ❌ **PREDICTED, NOT OBSERVED** |

**Run-1 came in *below* the no-WiFi-window control floor** — the thesis-confirmed outcome by the spec's own acceptance rule, and it closes plan-audit L3 ("deaf-window thesis unverified"). Both levers proven.

**Two caveats that matter for how this gets quoted:**

1. **The 15,000 ms Run-3 figure is the input parameter, not an observation.** `BLOCK_MS` was *set* to 15,000 to match v904's PREARM/flush hold. So the headline `(Run-3 − Run-1)` "total coexist win" is `169 ms measured vs ~15 s assumed`. Quoting "15 s → 169 ms" as two measurements would be `[[suspect-the-instrument-first]]` in reverse — half that comparison is a knob setting.
2. **Run-2 never ran, so Phase 2 did not re-measure co-channel vs off-channel.** The only such datum in the corpus is the older "48 KB vs 0" belief from `[[smol-ota-crown-offchannel-blocker]]`. The channel lever's dominance is inherited, not re-established.

### Port verdict

| what the harness measured | still needed on `main`? |
|---|---|
| superloop / app-service starvation during a WiFi burst | **superseded** by `BurstProbe`, and better: it runs on fleet builds, not a measurement tier |
| **mesh deaf-window** — ESP-NOW frames missed while the radio associates / goes off-channel | **NOT superseded.** `BurstProbe` measures the app gap, not RX loss. This is the half to re-port. |
| co-channel vs off-channel isolation (`SMOL_P2_STA_CHANNEL`) | still the only lever that isolates the channel variable — and still unexercised (Run-2) |
| blocking-mode emulation (`SMOL_P2_BLOCK_MODE=spin\|skip`) | only needed to *close* the Run-3 gap. If you want the honest before/after, `BurstProbe` on v917 is cheaper and uses the real blocking build rather than an emulation of it. |

**Verdict:** port the harness **narrowed to the deaf-window/RX-loss half plus the channel lever**; drop the app-gap tracking as duplicated. Keep the isolation properties (non-electing, advertisement-silent) — they exist so a measurement board cannot perturb the fleet's election, and #324/#278 make that *more* important than at fork time. The measured numbers from the original runs describe the **old** stack and should be treated as historical, not as the Phase-1 baseline; re-baseline with `BurstProbe` on v917 (RISKS §R10).

---

## 4. Phase 3 (`mqtt_task`) — **design valid, transport badly stale**

10 commits, `4d32773` … `b6413d3`:

| ref commit | delivers | status vs main |
|---|---|---|
| `4d32773` | inc1 — uplink-only `mqtt_task` skeleton (#89 non-blocking flush) | **design valid**, re-derive against today's flush |
| `8d947cf` | inc2 — own-node telemetry publish (SHARED-snapshot, lock-free) | design valid |
| `0415a65` | inc3a — relayed leaf-status republish (#50b) | main's `stat_cache` republish has moved |
| `35242d6` | inc3b1 — downlink foundation + batt/grid (async SUBSCRIBE/drain) | design valid |
| `69b165f` | inc3b2 — keyed CONFIG downlink (QoS0, crown-safe) | **stale** — #21/#56 convergence + `net/cfgsched.rs` (171 lines, NEW) landed after |
| `c3d03eb` | inc3b3 — transient COMMANDS downlink (QoS1 + PUBACK) | design valid |
| `943d198` | inc3c — OTA offer downlink (parse → gate → `OTA_OFFER`) | **stale** — #188 OTA completion + #349 `net/target.rs` (687 lines, NEW) add gates it never saw |
| `3c62d4b` / `ce0f34b` / `b6413d3` | inc3d — MC election OBSERVE → burst → RESOLVE | **design valid** (§1), but must absorb the Layer-2 seize/yield arms |

**The staleness is in `wifi.rs`.** Since the fork it took **+1,141/−118 across 30 commits** touching issues `#21 #56 #111 #142 #188 #233 #269 #278 #302 #303 #309 #324 #325 #329 #331 #343 #349 #352 #373`. The reference's async `mqtt_task` is a re-implementation of a **2026-07-21 snapshot** of `mqtt_session`. Anything it reimplements must be re-read against today's function, not ported from the reference's copy.

Also **new on main and absent from the reference entirely** — any async re-implementation must account for these or silently drop them:

| file | lines | what |
|---|---|---|
| `net/mesh_elect.rs` | 946 | #278 ELECT frame + announcer/follower |
| `net/target.rs` | 687 | #349 image-target identity — a board refuses a foreign image |
| `net/cfgsched.rs` | 171 | #21/#56 keyed-CFG relay scheduling |
| `net/profile.rs` | 156 | #325/#331/#352 `BoardProfile` runtime variant identity |
| `net/ledger_link.rs` | 155 | #181 ledger wiring |
| `net/radio_dev.rs` | 85 | #233 transitional smoltcp `phy::Device` shim over 0.18 raw tokens |
| `budget.rs` | 478 | #306/#348 DIAG + stack budget arithmetic (holds the stack floor) |

`net/radio_dev.rs` is worth a specific note: it is the **transitional shim** that lets main keep a hand-driven smoltcp stack on esp-radio 0.18. It exists precisely because main chose *not* to take 0c′'s excision. Phase 3/4 completing is what retires it — it is the marker for "the transport port is done".

---

## 5. Phase 4 (async OTA fetch) — spec'd, never built

The reference has **no Phase 4 commits**; `run_ota_fetch` is a stub (PORT-SPEC §0.2) and specs live in the tarball (`phase4-design-review.md`, `phase4-impl-spec.md`, `phase4-recon-and-spec.md`, `phase4-followon-issues.md`).

Main's OTA moved substantially since the fork (`ota.rs` +775 lines, 9 commits): #188 OTA completion, #267 Range/resume, #217 stall timers, #349 target identity, #226 otadata init. **The Phase-4 spec predates all of it.** Treat those documents as design *input*, re-verified against `ota.rs`/`ota_mesh.rs`/`net/ota_resume.rs`/`net/http.rs` as they stand — not as a plan to execute.

The reference's `a0d3e5a` also carries +254 lines of `ota.rs` change (an `ImageWriter` rework). That is entangled with the stack excision and should be re-read at Phase-4 planning time, not ported now.

**Three things to know before anyone opens Phase 4:**

1. **There is an unresolved design fork in the corpus.** Two Phase-4 documents disagree on the central structure and neither was ratified:

   | | `phase4-impl-spec.md` (07-21) | `phase4-recon-and-spec.md` (07-22) |
   |---|---|---|
   | shape | a spawned **`ota_task`** consuming `OTA_OFFER.wait()` | keep the gate + fetch **in `main`'s async loop** |
   | flash | `FLASH: Mutex` acquired once, `&mut FlashStorage` threaded | reuse the existing `flash_mut()` plumbing |

   The later document's objection is structural and looks right: a separate task **cannot borrow `&mut RadioManager`**, and all the install-trigger state (`install_requested`, `leaf_ota_pending`, `leaf_installs_outstanding`, `self_ota_fail_*`, `ota_fetching`, the crown-state AP gate) lives there. A spawned task forces lifting all of it into shared atomics — a large surface that **re-opens the lock-across-await class**. **Rule on this before any Phase-4 code.**

2. **The offer path is severed, and the failure is silent.** On the reference, `run_ota_fetch` is stubbed *and* nothing sets `RadioManager.ota_offer`, so `take_ota_offer()` always returns `None` → the entire install-trigger block is dead code. Separately, `smol/<id>/ota/cmd = install` is **not routed**, so with `OTA_AUTO_INSTALL=false` (the correct setting) a Phase-4 self-fetch could **never trigger**. Both are easy to miss because the gate logic "works" — it just never fires. Same shape as R5.

3. **`ota::gate()` is not the install-trigger gate.** inc3c already runs `ota::gate()` (build-monotonicity, host allowlist, size) at offer-parse time. It does **not** cover the install *trigger*: `!leaf_ota_pending()` (#1), `!leaf_installs_outstanding()` (#3), `OTA_AUTO_INSTALL || take_install_request()` (#33), `!self_ota_fetch_capped(build)` (#195, max 3). The reference notes this was "safe now" only because `OTA_OFFER` had no consumer. **The moment Phase 4 adds a consumer, the trigger gate becomes load-bearing.** A "simplification" that fetches straight off `OTA_OFFER.wait()` is the brick-adjacent auto-fetch.

---

## 6. Obsolete-because-main-solved-it-differently

| reference work | main's answer | verdict |
|---|---|---|
| `67cc40f`'s `.cargo/config.toml` rewrite (RX knobs → runtime) | `net::radio_controller_config()`, `net.rs:331–337`, called from `wifi.rs:516` + `mode.rs:2353` | **obsolete** — main's is factored better (one definition, two call sites) |
| M5 `ScanMethod::AllChannels` on the assoc config | `wifi.rs:792` | **obsolete** |
| Reference dropping `esp-wifi-sys`, `smoltcp`, `embassy-futures` | main still needs all three until Phase 3/4 land | **obsolete for Phase 1** |
| Reference's `esp-radio` features without `log-04` | main uses `log-04` | **regression if ported** |
| Reference `rust-version = "1.88"` | main is `1.96` | **regression if ported** |
| `dc3e7b6` defmt canary | reverted on the branch itself by `277bae6`; superseded by `34b9c6c` | **obsolete** — port `34b9c6c` only |

---

## 7. Sequencing consequences

1. **Phase 3's election half is safer than the brief assumed.** Broker authority is intact; the reference's OBSERVE/RESOLVE design ports on its own terms. Budget the risk into the *transport* half instead.
2. **Phase 3's transport half is the expensive one.** ~1,100 lines of post-fork `wifi.rs` work must be re-expressed asynchronously. This is the phase to worktree-isolate and to spec against today's `mqtt_session`, function by function.
3. **Don't re-port Phase 2 wholesale** — main's `BurstProbe` covers the app-gap half on the production path.
4. **Phase 4 is a fresh spec**, not a port.
5. **`FOLLOW_ENABLED` is a separate decision from the Embassy migration.** Flipping it changes channel authority fleet-wide. Keep the two changes apart so a mesh regression has one candidate cause, not two.

---

## 8. Loose threads

- **"Stagger tail-RTT: start wide at 25 s"** (brief) — **resolved, and it is branch-only.** `ELECT_TIER_STEP_MS` is `15_000` on `main` (`election.rs:255`) and `25_000` on the reference (`election.rs:106`). The widening landed in inc3d-2 **because** the port dropped a209858's #114 H2 claim-race re-read; it is a conservative stand-in for a deleted mechanism, and the isolated two-board tail-RTT run that would settle it was never performed. **Today's `main` still has the #114 H2 re-read, so 15 s is correct there** — carry 25 s only together with the RESOLVE port. See RISKS §R9.4.
- **The Phase-3 election work was validated on hardware, lightly but meaningfully.** Canary round 11: both bench boards booted the branch tip, observed the live crown, and **deferred correctly — zero spurious claims**, `now GATEWAY|WON` count 0, both logging leaf re-election against the live crown at 15.002 s / 15.016 s. That also proves the broker-mediated election works on metal (a pre-inc3d-2 stub could not elect at all). It does **not** cover the simultaneous-cold-boot dual-claim case, which is exactly what the deferred isolated-broker canary was for.
- **`MQTT_TX` never got a producer** anywhere in Phase 3 — `MqttMsg` stayed `#[allow(dead_code)]`, drained-only. Every Phase-3 payload rode a Signal (latest-state) or the `DOWNLINK` Channel. If a port re-creates `MQTT_TX`, give it a producer or leave it out; an unused channel is another correct-comment-no-behaviour surface.
- **`margin_for` documentation conflict.** `election.rs:96–97` cites `mesh_elect::margin_for` as live anti-flap for channel migration; `mesh_elect.rs:36`, `:40–53`, `:244` say it is retained-but-unwired with `#[allow(dead_code)]`. `SETTLE_MS` *is* wired (`:596`); `margin_for` is not. A comment naming a function that does nothing — the same defect shape the file's own header polices. Worth a cleanup issue.
- **Stale symbol path.** `main.rs:1088` still says `mesh_elect::FOLLOW_ENABLED`; `d721d3a` moved it to `net::election::FOLLOW_ENABLED`. Behaviour described is right, path is wrong. Same for two broken rustdoc links at `mesh_elect.rs:706`, `:712`.
- **`tools/check_elect_send_path.py` declares a raw-send-site count** (`RAW-SEND-SITES: send_to:1, send_arb_raw:1, run_leaf_ota_relay:3`, `mode.rs:6255`). Any Phase-3 refactor of the send path must update that declaration or CI goes red — by design, but easy to trip.
