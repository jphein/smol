# Embassy OTA verification — the procedure that retires the last blocker

**Why this exists.** [research/embassy-migration-status.md](../research/embassy-migration-status.md)
narrows the Embassy migration to **one** blocker: the branch's OTA path has never been exercised. The
risk is not that OTA is hard — it demonstrably works on `main` — it is that **this** OTA is untested,
**and it is the mechanism by which you would undo a bad roll.** This document is the procedure that
retires that, or tells you to walk away.

**Read the stop rule (§8) before starting.** A verification plan without one becomes a sunk-cost march,
and this one is guarding against "the thing that recovers you is the thing that broke."

---

## 0. Shape of the test

**A/B, same board, same image size, same oracle.** `main` is the control and it is *known good* — id5,
id50 and id8 each self-fetched 906 over WiFi and rebooted into it within the last hour (2026-07-28). So
every branch result has a same-day baseline to be compared against, which is far stronger than judging
a branch result on its own.

**Two paths, and they are different mechanisms. Test them separately:**

| | Path | What is new on the branch | Risk |
|---|---|---|---|
| **P1** | **Gateway self-fetch** — crown pulls the image over WiFi/HTTP-range into its inactive slot | embassy-net sockets, an async HTTP range fetch, flash writes interleaved with the executor, on a `wifi.rs` that lost 3,560 lines — **but with a working reference implementation on the same crates (§0b)** | 🟠 **still first** (P2 depends on it), risk **reduced** by prior art |
| **P2** | **Leaf mesh-OTA relay** — crown relays chunk-by-chunk over ESP-NOW (windowed-NAK); leaf verifies the ed25519 signature **before the first byte** | Mostly *old logic under a new executor* — `run_leaf_ota_relay` and `ServeSource::GatewayFetch` survived (`mode.rs:5003`) — **and it has NO prior art anywhere: the watch has no relay** | 🔴 **now the real unknown** |

**P1 goes first, but no longer because it is the riskier half.** Since the prior art landed (§0b) the
ranking has *inverted*: P1 has a working reference implementation on the same crates, while **P2 has
none anywhere** — the watch implements no relay at all. What has not changed is the ordering, and it was
never a preference: **P2 serves an image the crown has already fetched**, so a P1 failure blocks P2
mechanically. You test the better-understood path first because you have to, and the real unknown
second.

⚠️ **The uncomfortable structural fact:** P1 requires the DUT to **be the crown**, and the crown is the
board whose loss is most expensive (it is the fleet's only uplink, hence the only way to observe or roll
anything). §3 handles that with the `channel_hint` lever rather than by hoping.

---

## 0b. Step zero — read the working reference first

**There is prior art for the risky half, on JP's own hardware, on the exact crate versions.**
`~/Projects/esp32c6-watch` (read-only for us; its remote is **wakizashi only — never push there**) runs
`esp-rtos 0.3.0`, **`esp-storage 0.9.0`**, **`esp-bootloader-esp-idf 0.5.0`**, `embassy-executor 0.10.0`
— the branch's set — and does **async OTA over them in the field**.

**Read these three before Phase B, in this order:**

| File | Why |
|---|---|
| `src/net/ota_http.rs` | `pub async fn ota_update(...)`, `Ota::new(region, 2)`, `set_current_ota_state(OtaImageState::New)` — a working async fetch on the same API |
| the same file's boot-confirm path | 🔑 **This closes §9's load-bearing gap.** On first boot it maps `New \| PendingVerify → Valid` and logs *"rollback cancelled"*, and its comment says it is correct **whether or not the bootloader was built with auto-rollback** — which is exactly smol's situation. **You no longer have to derive app-side rollback from scratch; you have a reference to diff against.** |
| `src/net/mqtt_ha.rs` → `check_ota_announce` + `docs/ota-deploy.md` | the retained-announce + **strictly-greater build-id monotonicity gate** that stops a still-retained announce re-trigger-looping — the same hazard smol handles with `staged.build > running` |

### ⚠️ What the prior art actually buys — and the sharper thing it hands us

I agree with the credit, with one correction of emphasis. **The strongest reading is not "de-risked."**
It is: **this API bricked a board on real hardware, the root cause is written down, and it is the same
failure family smol already has an issue for.** That is *better* than reassurance — it is a test case.

`esp32c6-watch` **#55 (CRITICAL)**, *"OTA never writes the running slot"*: the root cause of an
*"eldritch-lantern boot-loop brick"* was that slot selection trusted `Ota::current_app_partition` —
**otadata is a boot REQUEST, not a boot FACT.** Stale otadata (a cable flash never rewrote it) made
*"the other slot"* resolve to **the partition the CPU was executing from**; a retained announce then
zero-touch triggered a self-overwrite, chunk-erasing the live image until it erased in-use WiFi rodata
and died mid read-modify-write → *"No bootable app partitions"*, on every re-flash. **The fix: derive
the running slot from the MMU, not from otadata.**

**smol has the same family already: [#226](https://github.com/jphein/smol/issues/226) — *"blank otadata
→ OTA targets the RUNNING slot (self-overwrite); add first-boot otadata init"*, closed 2026-07-20.** Two
chips, two codebases, one mechanism. So:

- ✅ **`esp-storage 0.9` + `esp-bootloader-esp-idf 0.5` move from "unknown behaviour" to "known-good
  **after a known critical fix**"** — a more useful phrasing than "proven", because it names what to
  check.
- ✅ **§9's app-side-rollback gap is closed by reading, not building.**
- ➕ **A mandatory new test falls out of it** — §6 gains the self-overwrite check.
- ❌ **The blocker is not retired.** Different chip (C6 bootloader quirks, a different partition table —
  their brick involved a *"pre-#50 4 MB layout"*), different codebase, and **smol's leaf mesh-OTA relay
  over ESP-NOW has no counterpart in the watch at all** — confirmed by grep, nothing in its `src/`
  implements a relay. **That half is exactly as unverified as it was.**

**Net effect on this plan: P1's risk drops materially; P2's does not move at all.** The risk table in §0
is re-marked accordingly.

## 1. Preconditions

- [ ] **Four boards live** and on the roster: **id8 Nexus**, **id5 Aegis**, **id50 Ember**, **id51
      Sigil**. Confirm from the crown's **`peers` attribute** — *not* its state, which is only the role
      ([DOC-UPKEEP](../../DOC-UPKEEP.md)).
- [ ] **Branch builds.** `dream/feat-embassy` @ `b6413d3`, verified 2026-07-28: `cargo build --release
      --features espnow,cast,io` → clean in 30.12 s.
- [ ] **espflash v3 on PATH.** v4 refuses esp-hal `1.0.0-rc.0` images
      ([BUILDING.md](../../BUILDING.md)). ⚠️ **Unverified for the branch:** it builds against esp-hal
      **1.1**, so v4 *may* accept branch images — do not rely on either until checked. Have **v3**
      ready, since it is what the control images need.
- [ ] **A `main` recovery image built and hashed**, before you flash anything.
      `tools/verify_image.sh <main-commit>` → note `build size sha256`. **You want this in hand before
      you need it**, not while a board is dark.
- [ ] 🔴 **The Phase-D DUT has already taken ≥2 OTAs** — otherwise rollback is untestable by design
      (§6b). Plan Phases A/B to leave it in that state.
- [ ] **Fleet state (2026-07-28):** all three wall-powered boards on **906** — id5 Aegis, id50 Ember,
      id51 Sigil — plus **id8 Nexus** on the instrumented build. So Phase C's *"canary that is not the
      crown"* is available and a `main` fallback uplink is trivially satisfiable.
- [ ] Broker creds present (`tools/ota_publish.env`), and `tools/ota_verify.sh` runs.

---

## 2. The oracle — one command, and what PASS actually means

**Do not hand-judge these runs.** `tools/ota_verify.sh <board_id> <target_build> [window_s]` already
encodes the discipline that a human eye gets wrong, learned the hard way in the v346 wave:

```bash
tools/ota_verify.sh 51 907 360     # exit 0 = PASS · 1 = FAIL/INFO · 3 = setup error
```

| It checks | Because |
|---|---|
| **`slot=ota_1` + `rst=ota`**, not just `installed_version` | *"installed_version flipped"* is **not** proof. A USB flash shows `slot=0` / `rst=usb-jtag`. **id5 once read as an OTA win when it had been flashed by USB.** `slot=ota_1` is the proof. |
| **Live publish, not retained** (`mosquitto_sub -F '%R'`, retain flag 0) | A fresh subscribe redelivers retained values; a persisted value once produced a false *"fleet installing"* alarm. Retained MQTT has faked liveness in this repo repeatedly. |
| **Death-point:** offset frozen >30 s with `done<total` | The transfer died **at that byte** — far more diagnostic than "it failed". |
| **Off-channel:** crown AP ch ≠ mesh ch | The coexist disease is a **channel mismatch** — proven: co-channel moved 48 KB, off-channel moved 0. This fails *before* you waste a run. |
| **`at=slot`** | Local otadata problem (#226) — **OTA cannot proceed, needs USB.** |
| **`src=id<n>` vs `src=gw`** | Peer-sourced (#237) vs crown WiFi-fetch — i.e. *which path you actually exercised*. Check this or you may believe you tested P1 and have tested P2. |

**That last row is the one to watch in this campaign.** With #237 peer-sourcing live, a run you *intend*
as P1 can be silently served by a peer. **A P1 PASS requires `src=gw`.**

---

## 3. Phase A — control on `main` (do this even though you expect it to pass)

Establishes that the bench, the broker, the image host and the operator are all working **before** the
branch can be blamed for any of them.

1. Pick the DUT: **id51 Sigil** — live, and **not** the crown.
2. Stage a `main` image one build above current: `tools/ota_publish.sh stage`.
3. `tools/ota_publish.sh install 51`.
4. `tools/ota_verify.sh 51 <build> 360`.

**Pass:** exit 0, `slot=ota_1`, `src=gw`.
**If this fails, stop — you have a bench problem, not a branch problem.** Fix that first; a branch run
against a broken bench produces an uninterpretable result, which is worse than no result.

---

## 4. Phase B — P1, gateway self-fetch on the branch

**Make the DUT the crown deliberately, with a known-good board standing by.**

1. **Park a `main` board as fallback crown.** Keep **id8 Nexus** on `main`/interim and WiFi-capable. It
   is your uplink if the DUT dies.
2. **Steer the crown with the operator lever, not luck.** Publish a retained
   `smol/mesh/channel_hint` naming the DUT's AP channel (#155): a board whose AP channel ≠ the hint
   refuses the crown, so the mesh converges onto the board you chose. **Clear the hint** (empty
   retained payload) to restore normal election. ⚠️ A hint no board can satisfy leaves the mesh
   **crownless** — clear it if you abort.
3. **USB-flash the branch image onto the DUT** (id51). Note the `Loaded app from offset` line — see
   §7's trap.
4. Confirm the DUT took the crown: crown's identity via `smol/mesh/channel` = `MC|owner|channel|seq`.
5. Stage a branch image and `install 51`; run the oracle.

**Pass:** exit 0 · `slot=ota_1` · `rst=ota` · **`src=gw`** · and the board comes back **running the new
build and holding the crown**.

**Failure branches — what each one means, and what to do:**

| Symptom | Reading | Action |
|---|---|---|
| Off-channel FAIL before any transfer | crown AP ch ≠ mesh ch | move the crown to the mesh channel and re-run. **Not a branch defect.** |
| **Death-point** at a repeatable byte offset | the async fetch dies at a boundary — chunk edge, socket window, flash page | **highest-value failure.** Record the offset; a *repeatable* one is a bug you can fix. Compare against Phase A's byte-count for the same image size. |
| Death-point at a **random** offset | radio/coexist contention, not the port | re-run co-channel; if it persists, this is #204 territory, not OTA |
| `at=slot` | otadata rejected locally — **the esp-storage/bootloader bump is implicated** (§6) | do **not** retry. Go to §6, then §7 |
| Fetch completes, board boots **`slot=0`** | image written but activation failed | the dangerous one: the new bootloader crate's slot/state API is behaving differently. §6 |
| Board dark / no telemetry **and** absent from the peer list | genuinely down (shortest-chain rule) | §7 recovery |
| Board absent from telemetry but **present in the peer list** | it is **running**, the uplink is broken | *not* a brick. Investigate the WiFi/broker leg, not the image |

---

## 5. Phase C — P2, leaf mesh-OTA relay

Only after Phase B passes. Crown = the branch board from §4; leaf DUT = **id50 Ember** on the branch,
kept **WiFi-less** (no creds) so it *cannot* self-fetch and the relay is the only path.

1. Stage an image; `install 50`.
2. Oracle: `tools/ota_verify.sh 50 <build> 600` — allow longer, the mesh relay is slower than HTTP.

**Pass:** exit 0 · `slot=ota_1` · **`src=id<crown>`** (proving ESP-NOW relay, not a WiFi fetch).

**Specific things to confirm, because they are the safety properties and not merely the feature:**
- [ ] **Signature verified before the first byte written.** A corrupted or wrong-key image must be
      refused with **nothing written to the inactive slot**. Test it deliberately: stage an image signed
      with a wrong key and confirm refusal. *This is the check that matters most on this path* — the
      relay carrying bytes is the easy part; refusing bad bytes is the guarantee.
- [ ] **Windowed-NAK recovery.** Interrupt mid-transfer (power-cycle the crown) and confirm the leaf
      neither bricks nor half-writes.
- [ ] The leaf's **DIAG verify counters** (`vok`/`vfl`) move as expected.

---

## 6. Phase D — the two OTA-critical dependency bumps

`esp-storage` 0.7→0.9 (*"otadata/flash write API changed"*) and `esp-bootloader-esp-idf` 0.2→0.5
(*"otadata slot/state API changed"*) ride the same matched set. **The branch compiling against them
proves the API port, not that otadata behaves** — and **revert-on-boot-fail is off in this bootloader**,
so app-side rollback plus canary discipline is the entire safety net.

Explicit checks, none of which Phases B/C cover implicitly:
- [ ] **A/B slot alternation across two consecutive OTAs.** Install twice; the boot slot must alternate
      `ota_1` → `ota_0` → `ota_1`. A board that installs successfully but always lands in the same slot
      is silently self-overwriting (**#226**) and has *no rollback*.
- [ ] **First-boot confirm.** After activation the app must mark the slot valid. Confirm `at=`/state
      reaches its confirmed value rather than staying pending — an unconfirmed slot is a board one
      reset away from reverting.
- [ ] ✅ **App-side self-rollback — now fully specifiable. See §6b, which is its own phase.**
- [ ] 🔴 **Self-overwrite / running-slot check — added because the prior art bricked on it (§0b).**
      Confirm the branch derives the running slot from a boot **fact**, not from otadata (a boot
      *request*). Test with **deliberately stale otadata**: OTA a board, then USB-flash it *without*
      `erase-region`, then OTA again — the very sequence that bricked the watch. It must target the
      genuinely-inactive slot or refuse. **A failure here is a brick, not a failed download**, which is
      why it belongs above the alignment check and not below it.
- [ ] **Flash-write alignment.** smol has been bitten before by raw flash writes needing a
      multiple-of-4 length (silent `NotAligned`, record never persists). A changed `esp-storage` write
      API is exactly where that class returns. Verify-after-write is the guard.

### 6b. App-side self-rollback — the one that matters, and it IS wired on the branch

**This was §9's load-bearing gap and it is closed** (verified in both trees, 2026-07-28). The branch has
the defence at the same two sites as `main`, with the same role-aware logic. What is unverified is
whether it *behaves under the async executor* — a much smaller residual than "we cannot describe the
test."

**The trigger** (`ota.rs:1042`) is `state ∈ {New, PendingVerify}` — deliberately **not** `PendingVerify`
alone, and the comment at `:1013` explains why: the bootloader **never promotes `New → PendingVerify`
when its rollback config is OFF** (the likely case here), so a `PendingVerify`-only trigger would never
fire on these boards → **no net → brick.**

> 🎁 **Free answer to a question the docs have called UNPROVEN.** `ota.rs:1056` sets
> `bl_auto_revert = matches!(state, OtaImageState::PendingVerify)`. So **reading `PendingVerify` at boot
> is a runtime probe that the bootloader's auto-revert is ON.** Capture that on the first branch OTA and
> you have settled, from the board itself, the question §3 of the research doc records as never tested.
> **Log it deliberately — do not let it go by unrecorded.**

#### ⚠️ Two guards a careless test WILL trip
| Guard | Where | What it means for the test |
|---|---|---|
| **USB-flash exemption** | `ota_was_activated_for(BUILD_NUMBER)`, `ota.rs:1051` | The self-test runs only if the running build matches the one `activate()` tagged. A USB-flashed image is accepted as-is with **no self-test**. **You cannot exercise rollback by USB-flashing a bad image** — it must arrive via a real OTA activate. |
| **Brick-safety refusal** | `can_rollback = target.map(slot_has_valid_image)`, `ota.rs:1078` | It rolls back **only if the other slot holds a valid bootable image.** On a board whose other slot was never written it **accepts the bad image and keeps running** rather than marking both slots unbootable. |

> 🔴 **Therefore a genuine precondition this plan did not have: the DUT must have taken AT LEAST TWO
> OTAs before rollback can be demonstrated at all.** A virgin-slot board cannot show it — and would
> "pass" by *accepting* a bad image, which is the correct behaviour and the wrong test result. **Sequence
> Phase A and Phase B so the Phase-D DUT already has two OTAs behind it.**

#### Health is role-aware — so P1 and P2 have DIFFERENT oracles
- **Crown / reached DHCP** (`mode.rs:7930`): confirms immediately — `if reached_dhcp { boot_confirm(true) }`.
- **Leaf / no DHCP:** confirming on `reached_dhcp=false` would **roll back every mesh-OTA**, since a
  credential-less leaf never does DHCP. So a leaf **defers** to the main loop's mesh predicate — *heard
  ≥1 valid SMOLv1 frame within N s* (`leaf_selftest_pending`).

**State this in each phase:** Phase B's health is *"did it reach DHCP"*; **Phase C's is "did it hear the
mesh."** A test that applies the crown oracle to a leaf will read a correct rollback as a failure.

#### How to trigger a rollback deliberately — and no scratch key needed
Ship an image that **cannot satisfy its own role's predicate**:
- **Crown path:** a deliberately wrong SSID/PSK → never reaches DHCP → self-test fails → rollback.
- **Leaf path:** an image that cannot hear a valid SMOLv1 frame.

> ✅ **This also closes §9's other unknown.** Neither case needs a mis-signed image, so **the negative
> test never touches the fleet's ed25519 trust anchor.** (The wrong-key refusal test in §5 is a
> *separate* property — signature-before-first-byte — and still wants its own answer.)

#### Also worth one run: the crash-loop net
`main.rs:499-509` (branch) holds a **K-counter** path that calls `boot_confirm(false)` after repeated
unconfirmed boots, gated on `ota_was_activated` so a USB flash cannot trip it. That is the **hard-crash**
net, distinct from the unhealthy-boot one above. An image that panics early exercises it.

---

## 7. Recovery — assume you will need it, and read this *before* you do

**Two traps, in the order they will bite you while you are in a hurry:**

1. ⚠️ **After any OTA, otadata points at `ota_1` — so a USB flash silently lands in the slot that will
   not run.** You flash, it succeeds, and the board keeps booting the old image; it looks like a brick
   or a failed flash and it is neither. Clear otadata first:
   ```bash
   espflash erase-region 0xf000 0x2000   # otadata ONLY — preserves NVS, so the node id survives
   # then reset, then flash
   ```
   **Check the `Loaded app from offset` line after every flash** — it tells you which slot actually
   ran. **This cost an hour on 2026-07-28 and is documented nowhere else** (see §9).
2. **espflash v3 for `main` images** — v4 refuses esp-hal `1.0.0-rc.0` images. Branch images are esp-hal
   1.1 and *may* be v4-acceptable; unverified. **Keep v3 available**, because the image you recover
   *to* is the `main` one.

**Recovery sequence:** `erase-region` otadata → reset → espflash v3 the pre-built `main` image from §1 →
confirm the `Loaded app from offset` line → confirm the node **reappears in the crown's peer list**
(shortest-chain: that is liveness; telemetry is a five-hop inference).

**Note what recovery does *not* do:** it does **not** restore a node id. Identity lives in NVS, and
`erase-region 0xf000 0x2000` deliberately spares NVS. If an id is wrong, that is re-provisioning, not
flashing ([BUILDING.md](../../BUILDING.md)).

---

## 8. Stop rule

**Abandon the roll and stay on `main`** — not "debug forward" — if **any** of these:

1. **Phase D's A/B alternation fails.** A board that cannot alternate slots has **no rollback**, and
   rollback is the entire reason this campaign exists. Rolling a fleet in that state means a bad image
   is recovered board-by-board over USB. **This is the hardest stop: no amount of other success
   compensates.**
2. **`at=slot` or a `slot=0` boot recurs after one fix attempt.** That is the new otadata crates
   misbehaving, and it is upstream of everything.
3. **Two boards need USB rescue in one session.** The failure mode being guarded against has arrived;
   further runs are gathering evidence at increasing cost.
4. **P1 death-points at a repeatable offset that resists one focused fix.** File it with the offset —
   that is a *good bug report* and a perfectly respectable place to stop.

**Stopping is cheap and reversible.** `main` is untouched and unmerged, `main` OTA works today, and the
measured 89× benefit does not expire. Restarting later costs a bench session; rolling a fleet you cannot
recover costs a board-by-board USB crawl. **The asymmetry is the whole argument** — and it is why the
stop rule is stated before the procedure rather than after it.

---

## 9. Honest gaps — steps I cannot fully specify from docs alone

Stated as findings rather than hedged, per the brief:

- ~~**App-side self-rollback is not specifiable from documentation.**~~ ✅ **CLOSED by the prior art
  (§0b).** `esp32c6-watch`'s `ota_http.rs` implements the reference on the same crates: stage as
  `OtaImageState::New`, and on first boot map `New | PendingVerify → Valid` ("rollback cancelled"),
  explicitly *"correct whether or not the bootloader was built with auto-rollback."* **Diff the
  branch's `boot_confirm` against that** rather than deriving it. Still worth reading the branch's own
  `ota.rs` to confirm it agrees — but this is now a comparison, not an invention.
- **Whether espflash v4 accepts branch (esp-hal 1.1) images** — untested. Affects only recovery
  convenience, but you find out during a recovery, which is the worst time.
- **The otadata `erase-region` trap is undocumented.** It is in nobody's `docs/`; it survives in
  operator memory and cost an hour today. It belongs in
  [BUILDING.md](../../BUILDING.md) and [ota.md](../../ota.md) as a first-class gotcha — **a separate
  docs fix I am flagging rather than smuggling into this plan.**
- **No expected byte-count/duration baseline is stated** for a healthy fetch. Phase A produces one;
  until it runs, "death-point" is detectable but "slower than it should be" is not.
- ~~**Rollback's negative test needs a mis-signed image.**~~ ✅ **CLOSED — it does not** (§6b): a wrong
  SSID/PSK (crown) or a mesh-silent image (leaf) fails the health predicate without touching the trust
  anchor. **Still open, and narrower:** §5's *signature-before-first-byte* refusal is a different
  property and does still want a deliberately bad signature; whether a scratch key can serve that
  without disturbing the fleet's anchor is unverified.

---

*Author: Nebula, 2026-07-28. Companion to
[research/embassy-migration-status.md](../research/embassy-migration-status.md) §5–6. The oracle is
`tools/ota_verify.sh`, whose header comments are the real authority for pass/fail — this document
sequences it, it does not replace it.*
