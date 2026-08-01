# #122 B1 — flush-period 30→20s: election-window co-derivation

**Author:** morpheus-114h3 · **Branch:** `feat/122-b1-windows` (off main `0a94b1c`) · **Ship: soak-gated.**
Deliverable = analysis + ready branch so #122 is a *merge-decision*, not research. NO flash/MQTT/mesh.

> **PROVENANCE (added 2026-07-31, #324 step 1 — lucid-canary-roll):** this document belongs to the
> UNMERGED `feat/122-b1-windows` branch. **The fleet runs F=30 s** (`RELAY_FLUSH_INTERVAL_MS =
> 30_000`, introduced 76b19e4, never changed on main; the 20 s value exists on exactly one commit,
> the branch head 16351ee). The *invariant reasoning* here transfers to main — §2 invariant 2
> (`REELECT_SILENCE_MS > RELAY_FLUSH_BUDGET`, was 15==15) was fixed on main by 9c3d4f9 and its
> premise was field-confirmed (`brst=500:15020:f`, a 15,020 ms flush) — but every *number* derived
> from F=20 must be RECOMPUTED before use on the F=30 fleet. At F=30 the operative windows are:
> re-elect gate 20 s (derived from budget) < `RECOVERY_STALE_MS` base 35 s < **effective takeover
> floor 45 s** (the #136 runtime floor, F + budget) < `MC_STALE_MS` 90 s. A crown handover faster
> than 45 s — not 35 — is the #136-violation signature. Main is self-consistent at F=30; the
> "at F=20 the floor is 35" note at wifi.rs:~4207 is forward-compat, not a stale leftover.

## 0. TL;DR verdict
B1 (F 30→20s) is **safe for election stability iff it is NOT done in naive lockstep.** The recovery
windows #114 tuned do **not** need to shrink with F — `RECOVERY_STALE_MS` (35s) and `REELECT_SILENCE_MS`
(15s) *gain* margin at F=20 and must be **kept**; shrinking them would erode the H1/H2/H3
false-takeover margins and cause **more** crown churn (tonight's failure mode). The one window that
should scale is `MC_STALE_MS` → 60s (3×F) for a faster never-heard heal (tonight's ~90s → ~60s). The
**binding risk** is `RELAY_FLUSH_BUDGET` (15s), which today *equals* `REELECT_SILENCE_MS` (15s): a
single worst-case (re-assoc) flush can be HELLO-silent right up to the re-elect threshold → a leaf
recovers → churn → the ch-drift/90s re-election seen tonight. B1 doesn't create that coupling but
raises its exposure (more flushes). Flagged as the #1 soak item; not statically "fixed" here because
lowering the budget trades against DHCP headroom (§4).

## 1. Constants today (main `0a94b1c`)
| Const | Value | File | Role |
|---|---|---|---|
| `RELAY_FLUSH_INTERVAL_MS` (F) | 30_000 | mode.rs:1306 | gateway MC-republish cadence — the master clock (the B1 knob) |
| `RELAY_FLUSH_BUDGET` | 15s | wifi.rs:323 | max flush burst (assoc+DHCP+drain); HELLO-silent CPU window worst-case |
| `MQTT_SESSION_BUDGET` | 3s | wifi.rs:302 | sub-bound inside a flush |
| `MC_STALE_MS` | 90_000 | wifi.rs:215 | dead-owner window, **single-signal** (boot/flush + #121 never-heard recovery) |
| `RECOVERY_STALE_MS` (Rs) | 35_000 | wifi.rs:228 | dead-owner window, **HELLO-corroborated** (heard-then-lost recovery) |
| `REELECT_SILENCE_MS` | 15_000 | mode.rs:2038 | owner-HELLO silence before a leaf re-elects (HELLO cadence = 2s) |
| `REELECT_RETRY_MS` (Rt) | 10_000 | mode.rs:2039 | min gap between recovery bursts |
| `RSSI_BUCKET_STEP_MS` | 15_000 | wifi.rs:198 | RSSI backoff step (staggers survivors) |
| `FLUSH_FAILS_BEFORE_DEMOTE` | 3 (count) | mode.rs:1320 | ≈3×F ≈90s of failed flushes → R-DEMOTE |
| `DWELL_MS` | 1500 | mode.rs:1977 | leaf scan dwell per channel 1/6/11 (full cycle ≈4.5s) |

## 2. Invariant chain (the load-bearing "MUST" relationships)
Ordered by how tightly each couples to F:

1. **`RELAY_FLUSH_BUDGET` < F** — a flush must finish before the next tick. 15<30 ✓; at F=20, 15<20 ✓
   but the worst-case deaf duty jumps 50%→75%. **(couples to F)**
2. **`REELECT_SILENCE_MS` > `RELAY_FLUSH_BUDGET`** — a single worst-case (re-assoc) flush is
   HELLO-silent for up to the budget; if that reaches the silence threshold a leaf re-elects against a
   *live* gateway → churn. Today **15 == 15 (marginal/violated)**. Independent of F, but B1 multiplies
   the exposure. **(the binding risk — §4)**
3. **`RECOVERY_STALE_MS` > F** — the window must span a full flush so a live gateway's seq-advance
   resets `alive` (the split-brain guard, wifi.rs:222-224). 35>30 ✓ (margin 5s); at F=20, 35>20 ✓✓
   (margin **15s**). **Rs does NOT need to shrink — it gets safer.** **(one-directional: F↓ ⇒ Rs margin↑)**
4. **`RECOVERY_STALE_MS` > F + `REELECT_RETRY_MS`** (the true no-false-takeover bound: a leaf re-reads
   every Rt, so its *observed* freeze of a happy-path live gateway can reach F+Rt). At F=30: 30+10=40 >
   Rs=35 → the 5s gap is covered only by the HELLO-silence corroboration (heard path) + #121's 90s
   never-heard path. **At F=20: 20+10=30 < Rs=35 → this bound becomes SATISFIED with 5s to spare.**
   i.e. B1 *closes* a corner the current F=30 leaves to corroboration. **(F↓ ⇒ strictly better)**
5. **`MC_STALE_MS` ≈ 3×F** — single-signal "3 missed refreshes" margin (no HELLO corroboration).
   90=3×30. At F=20, 3×20=**60** keeps the semantic; keeping 90 = 4.5× (safer, slower heal). **(couples to F)**
6. **`RSSI_BUCKET_STEP_MS` > `REELECT_RETRY_MS`** — a weaker board gets a burst between the winner's
   claim and its own threshold → reads the winner's MC → adopts → no competing claim (RSSI winner
   stable). 15>10 ✓, **independent of F — keep both.**
7. **`REELECT_RETRY_MS` < `RECOVERY_STALE_MS`** — a leaf re-reads ≥once inside the stale window (to
   observe a seq advance). 10<35 ✓ at both F. **independent of F.**

## 3. Must-shrink / must-not, and proposed values
| Const | Current | Proposed @ F=20 | Shrink w/ F? | Justification |
|---|---|---|---|---|
| `RELAY_FLUSH_INTERVAL_MS` | 30_000 | **20_000** | — (the knob) | B1: ~10s off every UI path (#117 decomposition) |
| `MC_STALE_MS` | 90_000 | **60_000** | **YES (3×F)** | keeps 3-missed-flush semantic; speeds #121 never-heard heal 90→60s (tonight) |
| `RECOVERY_STALE_MS` | 35_000 | **35_000 (keep)** | **NO** | inv #3/#4: gains margin at F=20; shrinking erodes H1/H2/H3 takeover safety |
| `REELECT_SILENCE_MS` | 15_000 | **15_000 (keep)** | **NO** | tied to 2s HELLO cadence, not F; shrinking → false re-elect on transient loss |
| `REELECT_RETRY_MS` | 10_000 | **10_000 (keep)** | **NO** | convergence hysteresis; must stay < RSSI_BUCKET_STEP |
| `RSSI_BUCKET_STEP_MS` | 15_000 | **15_000 (keep)** | **NO** | must exceed REELECT_RETRY; independent of F |
| `RELAY_FLUSH_BUDGET` | 15s | **15s (keep) — SOAK** | conditional | see §4: lowering trades vs DHCP headroom; measure first |
| `FLUSH_FAILS_BEFORE_DEMOTE` | 3 | **3 (keep)** | auto (3×F) | count auto-scales: demote 90s→60s (faster, acceptable) |

**Code changes drafted on the branch: `RELAY_FLUSH_INTERVAL_MS` 30_000→20_000 and `MC_STALE_MS`
90_000→60_000** (plus comment re-derivation). Everything else held, with comments explaining why.

## 4. The binding risk: `RELAY_FLUSH_BUDGET` (15s) == `REELECT_SILENCE_MS` (15s)
The 15s budget (wifi.rs:315-321, HW-tuned) is the *re-assoc* worst case (assoc+fresh-DHCP ≈6s observed
→ 15s gives ~2.5× headroom). In steady COEXIST the gateway stays associated, so a flush is sub-second
and NOT HELLO-silent (0/60 RX loss soak). The 15s only bites on a re-assoc (post-roam / R-CONNECT).
But when it bites, the gateway is HELLO-silent up to 15s = exactly `REELECT_SILENCE_MS` → a leaf can
cross into recovery against a live-but-reassociating owner. **This is the most plausible mechanism for
tonight's crown churn** (§5). B1 raises exposure (flush events per minute: 2→3). Options for the soak:
- **(a)** lower `RELAY_FLUSH_BUDGET` → 10s to restore `SILENCE > BUDGET` margin — BUT that cuts DHCP
  headroom to ~1.6× (risk: flush-fail on a slow AP; wifi.rs:315 says 6s was too little). **Measure DHCP
  time under B1 load before doing this.**
- **(b)** raise `REELECT_SILENCE_MS` → 18-20s — safer against re-assoc churn, costs ~3-5s of heal
  onset; still ≥7 missed HELLOs. Cheaper/safer than (a); the soak should compare.
- **(c)** suppress the leaf's silence accrual while the owner's MC seq is still advancing (a code fix,
  not a constant) — the principled fix, but out of B1's constant-tuning scope; note for a follow-up.
Recommendation: **do NOT bundle a budget change into B1 blind.** Ship F→20 + MC_STALE→60; carry the
BUDGET/SILENCE margin as the primary soak measurement (pick (a)/(b)/(c) from data).

## 5. Tonight's live data point (crown churn → ch drift 6→1, ~90s re-election)
- **~90s re-election** = the #121 never-heard `MC_STALE_MS` (90s) window firing: a leaf lost the crown,
  could not hear the new owner (ch drift → scanning the wrong channel / new owner on a different AP
  channel), so `owner_never_heard=true` → the conservative 90s path. **Not a bug — the fail-safe doing
  its job**, just slow.
- **ch drift 6→1** = the mesh rides the gateway's AP channel; a crown change to a gateway whose learned
  channel differs (or a leaf resetting `learned_channel=0` and scanning 1/6/11, landing on 1). Advisory
  MC channel + the 4.5s scan cycle mean discovery itself is fast; the 90s was the *stale window*, not scan.
- **Would B1 have helped or hurt?**
  - **Helped the duration:** `MC_STALE`→60 makes the never-heard heal ~60s not ~90s. New owner's MC
    (with its channel) also republishes every 20s not 30s → a re-reading leaf picks up the new owner
    ~1.5× sooner.
  - **Could hurt the frequency (if done naive):** at F=20 with BUDGET unchanged, more flushes = more
    re-assoc HELLO-silent windows near the 15s threshold (§4) = **more** churn-triggering events =
    potentially more ch-drift episodes. This is the crux: B1's latency win is real, but the churn
    *trigger* must be addressed (via §4) or B1 makes tonight's class of event more common.
  - **Net:** with F→20 + MC_STALE→60 **and** a §4 budget/silence margin fix, tonight's event is both
    **faster** (60 vs 90s) and **less frequent**. Without §4, faster but more frequent — a wash or worse.

## 6. What the soak must measure (ship gate)
1. DHCP time distribution under F=20 load → decides §4 (a) feasibility (budget→10s?).
2. Re-assoc frequency & the HELLO-silent duration per flush → confirms whether SILENCE>BUDGET margin is
   actually exercised (does tonight's churn reproduce at F=20?).
3. Leaf battery / airtime delta at 3 flushes/min vs 2 (#34-future) — the original power gate.
4. Crown-churn rate over a multi-hour window at F=20 (with and without the §4 fix) vs F=30 baseline.

## 7. Build
`cargo build --release --features espnow,cast,io` on the branch — must pass (see branch commit).
Constants-only change; no logic touched, so no clippy/behavioral surface beyond the timing values.
