# P2 — the ESP-NOW mesh relay under an async executor

**The question I was given:** is **P2** (the leaf mesh-OTA relay), now ranked the migration's real
unknown, a **genuine blocker** or **work**?

**The answer is neither, and the distinction matters more than the ranking.**

> ## P2 is **unstarted work**, not a blocker.
> The relay on `dream/feat-embassy` is the **verbatim blocking function from `main`** — same `pub fn`,
> same `tick: &mut dyn FnMut() -> bool` callback (branch `mode.rs:5003`). It was never ported. And its
> WiFi source, `run_ota_fetch`, is a **stub that returns `false`** (branch `wifi.rs:1659-1675`,
> *"STUBBED (OTA HTTP fetch -> embassy-net in Phase 5)"*).
>
> **Consequence: the verification plan cannot be run on `b6413d3` at all.** Phase B (P1, gateway
> self-fetch) fails at the stub on the first attempt, and Phase C (P2) is sequenced behind it.

**And there is a bigger finding than P2, which neither existing document records.** The branch's
merge-base with `main` is `36e6345`; **`main` has 98 commits since**. The branch is 6 days stale
(2026-07-22) and **contains no Bard** — `grep -c bard` on its `main.rs` returns **0**, and all
2,982 lines of `src/bard/` show as deleted in `main → branch`. The verified *"compiles clean in
30.12 s"* result was for a tree **without** the Bard — and `main` currently has only **~2,400 B of
stack slack** before `tools/repro_build.sh` hard-fails, while Embassy adds task stacks and socket
buffers in kilobytes (§6).

**So the decision in front of JP is not "is P2 risky."** It is: **the branch must be rebased across
98 commits including the entire Bard campaign before any of the P1/P2 verification is even
meaningful** — and the rebase, not P2, is where this gets expensive.

*Sources are cited inline by file:line, in both trees. The evidence log with every command and result
is [`scratch/embassy-p2/nebula.md`](../../../scratch/embassy-p2/nebula.md).*

---

## 1. What the relay actually does today — and which properties are load-bearing

The migration risk is *precisely* the set of properties currently implicit in "it runs to completion
in a superloop." Here they are, each with the line that makes it load-bearing.

`run_leaf_ota_relay` (`mode.rs:4330` on `main`) is a **single blocking function that runs for
minutes**: 15 s pre-fetch arm → up to 300 s WiFi fetch → 15 s wake burst → ~98 windowed NAK rounds →
120 s confirm. For a 1,440,528 B image that is **6,237 chunks of 231 B in 98 windows of 64**.

### 🔑 It already has a hand-rolled cooperative scheduler — and that is the whole story

The `tick: &mut dyn FnMut() -> bool` parameter is called at 10 points and returns `true` to abort. So
the relay is **not** naively blocking; it already yields. But look at what the closure does
(`main.rs:1083-1106`):

```rust
led.apply(led::LedState::WifiSync, t);
if matches!(button.poll(t), Some(input::Press::Long)) { relay_abort = true; }
… ota_screen::draw(&mut display, …)
relay_abort
```

**LED, button, display. Nothing on the radio. No `service()`. No `receive()`.**

That is the pivot of this whole document:

> **`tick()` is a bounded, author-chosen, radio-free re-entrancy window.**
> **`.await` is an unbounded one where any other task — including every task that touches the
> radio — may run.**

Porting the relay is not "replace `tick()` with `.await`." It is **widening the re-entrancy window
from three peripherals to the whole program**, and every property below is what currently lives in
that gap.

### The five load-bearing invariants

| # | Invariant | Why it holds today | What breaks under an executor |
|---|---|---|---|
| **1** | **Sole consumer of the ESP-NOW RX queue** | Exactly two consumers exist in the tree: `service()` (`mode.rs:5391`, drains ≤24 of a 10-deep HW queue) and the relay's own inline `receive()` (`:4412/4571/4666/4769`). They never race **only because the relay blocks the loop that calls `service()`.** | A concurrently-running mesh service **silently eats OTANs**. Windows never advance → `RelayFailed` at round 16. The relay also does a deliberate 64-frame stale-queue drain (`:4768`) whose entire premise is that it owns the queue. **This is the #1 hazard and no compiler can see it.** |
| **2** | **Exclusive owner of radio mode + channel** | The relay calls `switch(Mode::EspNow)`/`switch(Mode::WifiSta)` itself, then **spins ≤40 times on `is_connected()`+`disconnect()`** to force the STA to release the PHY before pinning ch6 (`:4504-4513`) — publishing `settle` as proof this was the real off-channel-egress bug. It re-pins `ESP_NOW_FIXED_CHANNEL` before *every* OTAM send (`:4384/4408/4514/4562/4642`). | On the branch `wifi_task` owns the controller. That spin becomes a **two-writer race**, and a `wifi_task` can move the channel *mid-relay* — which today is structurally impossible. |
| **3** | **Exclusive owner of two `static mut` window buffers** | `OTA_WINDOW_BUF` carries the safety argument in prose: *"Alias-safe: exactly one leaf OTA at a time (canary), **single-threaded, single-caller**"* (`ota_mesh.rs:678`). `GW_OTA_WINDOW` is taken by `addr_of_mut!` (`mode.rs:4515`). | The soundness argument is **literally single-threadedness**. It does not survive the sentence being false. |
| **4** | **Wall-clock deadlines that assume busy-poll density** | Every phase is `while now_ms() < deadline` with an inline `receive()`: OTAN wait **800 ms**, rounds max **16**, confirm **120 s**, fetch budget **300 s**, prearm/wake **15 s @ 120 ms**, leaf stall **30 s**, first-chunk grace **330 s**, session cap **600 s**. | The *deadlines* are absolute and survive. The **sampling density inside them does not** — an 800 ms window busy-polled thousands of times behaves nothing like one sampled at each `.await`. **Every constant above was tuned under busy-polling and is unvalidated after the port.** |
| **5** | **The crown is deliberately mesh-silent — and the protocol was redesigned around that** | `mode.rs:4627-4637`: a leaf registers the gateway as an ESP-NOW peer only on receiving an OTA frame *or a gateway HELLO*, and the gateway is **HELLO-silent during the blocking relay** → *"bootstrap deadlock"* → so the OTAM is **broadcast, not unicast**. #3b's pre-fetch arm burst and post-fetch wake burst exist for the same reason: the leaf loses the gateway during the off-channel fetch and starts hopping [1,6,11]. | **These are compensations for blocking.** Async plausibly *retires* them — a real benefit — but it means the port is **not like-for-like**, and each workaround's removal needs its own verification rather than being deleted as dead weight. |

### 🟢 The good news nobody has written down: the leaf half is already async-shaped

The **receive** side is already an event-driven state machine — `on_meta` / `on_data` / `tick` →
`LeafAction` (`ota_mesh.rs:812 / 907 / 1044`), with the cross-cutting hold expressed as a predicate
(`ota_leaf.is_active()` gating scan and re-election at `mode.rs:2325/2403/3026/3067`, plus a
peer-accept widening at `:6043`).

**So "P2" is really two halves with opposite risk profiles.** The **leaf** ports close to free — it is
already a poll-and-dispatch machine. The **gateway serve** half is the blocking monolith. Every
invariant in the table above belongs to the gateway half. **Scoping P2 to "the gateway serve loop"
makes it a much smaller and more honest unit of work than "the mesh relay path."**

---

## 2. Prior art — it exists, and both documents are wrong to say it doesn't

> **Correction.** [`embassy-migration-status.md`](embassy-migration-status.md) §5,
> [`embassy-ota-verification.md`](../plans/embassy-ota-verification.md) §0/§0b, and
> [HANDOFF §4](../HANDOFF-2026-07-28.md) all state that the watch **implements no relay** and that
> P2 has **"no prior art anywhere."** That is **too strong**, and it was reached by grepping for
> *OTA* relay rather than for ESP-NOW.

`~/Projects/esp32c6-watch` (read-only — remote is **wakizashi only**) does ESP-NOW under Embassy:

| What the watch has | Where | How close to P2 |
|---|---|---|
| ESP-NOW on the Embassy stack at all | `Cargo.toml:61 "esp-now"`; `src/net/smol_mesh.rs` (896 L) using `esp_radio::esp_now::{EspNow, EspNowWifiInterface, PeerInfo}` | **Directly relevant.** The crate combination smol needs, meshing in the field |
| A **fragmented, ACKed, retransmitted multi-frame transfer** | `smol_mesh.rs:21-22` — `RELAY` + `RELAYACK` frag bitmaps; `RelayTx` retransmits unacked frags, *"One at a time"*. Comment calls it a *"byte-exact port of wire.rs `encode_relay`"* | **The pattern, at 1/1000 the scale** — ≤4 frags, ~15 s, ≤91 B chunks, leaf→gw **uplink**. smol's OTA relay is 6,237 chunks / 98 windows / 231 B / minutes, gw→leaf **downlink** |
| 🔑 **The radio-arbitration decision, written down as a verdict** | `net_task.rs:563-569` | **This is P2's hardest architectural question, already answered next door** |
| Single-owner-of-`EspNow` preserved across the migration | `main.rs:2543` — *"Main still executes the `set_channel` because the mesh owns the `esp_now` handle"*; `world_snake.rs:22` — *"The app can't own EspNow: the main loop drains `pending_tx()`"* | **The exact shape smol's invariants 1-3 need** |

The verdict itself is the thing to steal:

```rust
mesh_pin_ok: st.radio_started && !st.connected && !st.connecting
          && !st.scanning && !st.scan_pending && !st.assoc_want()
          && st.ota.is_none(),
```

Note the last term: **`ota.is_none()`** — the watch already encodes *"an OTA suppresses the mesh
channel pin"* as an explicit scheduling decision. That is precisely the invariant smol currently gets
for free from blocking.

Three details make it a template rather than an anecdote:
1. **The decision lives in the task; the mechanism stays in main** (`net_task.rs:53-56`) — because the
   mesh owns the handle. Invariants 1-3 survive by keeping single ownership and moving only the
   *policy*.
2. **The verdict is re-read fresh every tick, never from the tick-start snapshot** (`main.rs:2548-2551`,
   *"review F1"*) — because an intervening `.await` can stale it. That is invariant 4's failure mode,
   already found and fixed on the watch.
3. **It is level-reconciled both ways**, so after a scan sweep the pin returns rather than idling
   wherever the sweep stopped.

**Honest scoring:** prior art **exists** for P2's *architecture* (ownership + arbitration) and for
*ESP-NOW under Embassy*. It **does not exist** for P2's *scale* — nothing anywhere drives a
minutes-long, 98-window, thousands-of-chunk ESP-NOW transfer under an executor. So the correct
statement is **"no prior art for the OTA relay; a written verdict for the coexistence question,"**
which is materially better than where both documents left it.

---

## 3. Does coexistence still hold under async?

Taking the three established facts as given (not re-litigated):

| Established fact | Does it change character under async? |
|---|---|
| **coex arbitrates WiFi↔BT, and ESP-NOW *is* WiFi** | **No.** One MAC, one channel, vendor action frames. An executor does not touch it. |
| **The mesh-deaf window is a deliberate duty-cycle choice, not a HW limit** | **Yes — this is the one that changes character, and it is the crux of P2.** Today the "choice" is *implicit in control flow*: the radio is off-channel because a blocking function took it there and will not return until done. Nothing arbitrates, because nothing else can run. Under an executor the same choice becomes a **contended policy between two live tasks** — and a policy needs an **owner**. `main` has no such owner because it needs none; the branch has none because the relay was never ported. **That gap *is* P2.** The watch's `mesh_pin_ok` is the shape of the missing piece. |
| **The OTA "coexist disease" is a channel mismatch (co-channel 48 KB / off-channel 0)** | **Unchanged as physics — but async makes it *easier* to trip.** Today a mid-relay channel move is structurally impossible. On the branch, `wifi_task` can move the channel while the relay runs. So the same failure returns by a **new route**: not an operator mis-channeling an AP, but two tasks disagreeing about who owns the PHY. The relay's `settle` counter (`mode.rs:4503`) already exists to detect exactly this and should be read as the port's canary. |

**Applying the migration's own rule** — *async changes whether other work can run while the radio is
busy; it does not create a second radio* — the relay sits on the **first** line, so async genuinely
helps. But the relay is also the one path that **needs the radio exclusively for minutes**, which
makes it the worst case for the first line rather than a comfortable one.

---

## 4. The smallest experiment that retires P2

Stop-rule-first, in the spirit of the existing plan. **Two experiments, because one of them is free
and the other cannot be avoided.**

### ⚠️ First, the trap that makes a naive Phase C worthless

**A blocking function inside an executor preserves every invariant it needs — by starving everything
else.** The relay on the branch is still the sole RX consumer, still the exclusive radio owner, still
the only toucher of the `static mut` buffers, *because* nothing else gets to run while it executes.

> ### So the branch will most likely **PASS** a P2 relay test while delivering **none** of P2's benefit.
> **The risk is not that P2 fails. It is that P2 passes and is mistaken for done.**
> A green Phase C on `b6413d3` is a **false green** and must not be recorded as retiring P2.

*(Mechanism verified by reading; the starvation itself is **inferred, not measured** — see §6.)*

### E1 — the free baseline, available on `b6413d3` today

**`ota_mesh.rs` differs between the trees by exactly one line**, so the mesh-OTA wire is effectively
identical and **a `main` crown can ODEL-delegate a serve to a branch holder.**

That unlocks a shape strictly better than the plan's Phase B→C:

- **Use `ServeSource::HolderActiveSlot` (#237 peer-serve), not `GatewayFetch`.** It reads the image
  from the holder's *active* slot with **no WiFi fetch** — so it **bypasses the stubbed
  `run_ota_fetch` entirely**, which is the only reason any relay test is possible on this branch.
- **The crown stays on known-good `main`.** This **removes the plan §0 "uncomfortable structural
  fact"** that the DUT must be the crown — the fleet's only uplink is never the experiment.
- **Zero firmware to write.** It runs on `b6413d3` as it stands.

**Setup:** crown `C` on `main` (uplink, safe) · holder `H` on the branch running build X · leaf `L`
needs build X. `C` delegates ODEL to `H`; `H` serves `L` over ESP-NOW from its active slot.

**What E1 actually measures — and it is NOT the transfer verdict.** It is *how badly a blocking relay
poisons the async stack*, which is the number the port must beat.

**FAILURE (declared before running):**

| | Condition | Reading |
|---|---|---|
| **F1** | `mqtt_task`/`net_task` do not recover within 60 s of relay end — broker never re-CONNACKs, telemetry never resumes | The blocking relay **poisons** the embassy-net stack. Hosting unported blocking code in the executor is not a waypoint, it is a regression |
| **F2** | `otan_valid = 0` with `rx_any > 0`, or window 0 never advances, where `main` succeeds on the same image size | A second RX consumer is eating OTANs — invariant 1 already broken |
| **F3** | `settle > 0` together with off-channel OTAM egress | A second writer moved the channel — invariant 2 already broken |
| **F4** | Two boards need USB rescue in one session | Inherited from the existing plan §8 |

**A PASS is not progress** (see the trap above). E1's deliverable is the F1 number plus a go/no-go on
whether the branch can serve at all.

### E2 — the one that actually retires P2

P2's risk is only *realised* once the relay is async, so it cannot be retired without writing some.
The point is to make it **one function**, not a phase:

1. Convert **only** the OTAN wait and inter-chunk pacing in the gateway serve loop to `.await`.
2. Keep `EspNow` **single-owner in main** (the watch's rule) — move policy, not the handle.
3. Add a `relay_active` term to a smol `mesh_pin_ok` equivalent, mirroring the watch's
   `ota.is_none()`, and **re-read it fresh** rather than from a tick-start snapshot.
4. Re-run **E1's exact procedure**, unchanged.

**FAILURE:** any of **F2 / F3** appearing that E1 did **not** show — that is a regression *introduced
by the port*, and the A/B against E1 is what makes it attributable rather than arguable. Plus: the
§1 table-4 constants must be re-derived, not inherited; a transfer that completes on **stale tuning**
is luck, not a pass.

**Retirement criterion:** E2 passes **and** the mesh stays alive through the relay (the actual
benefit), measured with Phase 2's existing harness rather than asserted.

### What E1/E2 explicitly do **not** settle

**`blrev=`.** The board that takes the OTA in E1/E2 is a **leaf**, and a leaf's 232 B DIAG frame
truncates `blrev=` (#306). So the campaign's advertised "free win" is **still not capturable here** —
it needs serial on the leaf, or #306 fixed first. This is the HANDOFF's point, confirmed and now
attached to a specific experiment.

---

## 5. Two corrections to the open measurements

**`brst=` is *structurally* crown-only, not crown-only-because-truncated.** The brief frames it as
*"crown-only **and** truncated off the leaf record,"* and that conjunction misleads. `note_burst` is
called at three sites (`main.rs:1037/1532/1571`), all inside gateway burst paths — and **only a
gateway runs a WiFi telemetry burst at all.** A leaf has *nothing to report*, so truncation is
irrelevant for `brst`. **The sole blocker is that the crown has not taken the instrumented build.**
Truncation *is* the blocker for `blrev=`, which every OTA'd board genuinely has — which is exactly
why HANDOFF §1a calls `blrev` the sharper problem. Keep the two apart.

**Both remain code-readings, and the migration's headline claim depends on one of them.** Any
statement that Embassy fixes JP's freezes is **unmeasured**. §1's invariant table is likewise a
code-reading; it is a strong one (the safety arguments are in the source comments) but nothing here
has been run on hardware.

**The plan's harness floor is stale.** [`embassy-ota-verification.md`](../plans/embassy-ota-verification.md)
§2 says *"confirm you are on `fa2e6aa` or later."* `tools/ota_verify.sh` was fixed **again** at
**`5264e28`** — `-F '%R'` is not a mosquitto specifier and expands to the empty string, so the retain
flag was **never read at all**; it is `%r`. **Use current HEAD.**

---

## 6. What I could not establish

- **That a blocking relay starves `net_task` into MQTT/TCP death.** The mechanism is clear —
  `embassy-net` cannot move packets without its runner, and the relay never reaches an `.await` for
  minutes — but this is **read, not observed**. It is E1's F1 and the single highest-value cheap
  measurement available.
- **Whether the branch builds with the Bard — and this one is quantified enough to predict.** They have
  never coexisted. [ROADMAP §2](../../ROADMAP.md) gives post-#300 geometry on `main`: `.bss`
  **195,224 B** · `.stack` **76,128 B** · esp-wifi heap **96 KiB** · measured stack peak **54,960 B** —
  with `tools/repro_build.sh` **hard-failing below a 76,128 → 73,728 B floor**, i.e. **~2,400 B of
  slack.**

  Embassy adds `.bss` in *kilobytes*: three task stacks (`net_task`/`wifi_task`/`mqtt_task`) plus
  `embassy-net`'s socket RX/TX buffers. And esp-hal **shrinks `.stack` silently as `.bss` grows**. So:

  > **Expect the rebase to break the stack-floor gate, not to squeeze under it.**

  Two consolations and one cost. It **fails closed** — the gate catches this at build time rather than
  on a board (that gate exists precisely because a pre-gate image linked clean with 2,592 B of stack
  and would have died on hardware). And a lever exists: ROADMAP lists `SEQ_CAP` 80→64 freeing ~5.8 KB,
  80→48 freeing ~11.5 KB. **The cost is that the lever is the Bard's sequence length** — so
  *"Embassy may shorten the Bard's context"* is a real product trade JP should get to make explicitly,
  not discover as a build error. **Do not raise the floor to make a build pass** — it is derived from a
  measurement (peak × 4/3).
- **The cost of the 98-commit rebase.** The branch changes 671 lines of `main.rs`; the Bard campaign
  changed the same file heavily. Not attempted, not estimated.
- **Whether E1's ODEL path is wired end-to-end on the branch.** `take_pending_serve` is called
  (branch `main.rs:1010`) and the wire is identical, but I did not trace the MQTT install-order →
  ODEL trigger through the branch's downlink tasks. **Verify this before booking bench time**, or E1
  fails on plumbing rather than on the question.
- **Whether `GatewayFetch` is genuinely deferred or abandoned.** The stub says *"Phase 5"*; no Phase 5
  plan exists in `docs/`.

---

## 7. So what should JP do?

**P2 does not need a decision. The branch does.**

| Option | What it costs | What it buys |
|---|---|---|
| **Run E1 now** | ~1 bench hour, no code, no crown risk | The starvation number, and a go/no-go on the branch serving at all. **Do this regardless of the larger decision** — it is the cheapest real information available |
| **Rebase the branch across the 98 commits, then E2** | The unestimated rebase + one function of async | The only path to a branch that is actually shippable. **This is the real gate, and it is not P2** |
| **Stay on `main`** | Nothing — `main` works, OTA works, the interim freeze fix landed | Benefit 1 is already addressed without the migration. Benefits 2 (89× deaf window) and 3 (BLE) do not expire |

The existing recommendation — *"don't do it blind on an unverified OTA path"* — still holds, but for a
**different and more mundane reason** than the documents give. It is not that P2 is a frightening
unknown. It is that **the branch is six days and 98 commits behind a `main` that has since shipped its
flagship feature, and the two have never been compiled together.** That is ordinary, estimable
engineering work — but it is work nobody has costed, and it sits *upstream* of every experiment in the
verification plan.

**Fix the ranking: the rebase is P0. E1 is free and should run anyway. P2 is P2 — and it is smaller
than advertised, because the leaf half is already async-shaped and the arbitration question has a
written answer next door.**

---

*Author: Nebula, 2026-07-28. Companion to
[embassy-migration-status.md](embassy-migration-status.md) and
[plans/embassy-ota-verification.md](../plans/embassy-ota-verification.md), which this document
corrects in four places (§2 prior art · §1/§4 the unported relay and stubbed fetch · §5 the harness
floor · §5 `brst` framing) and confirms in one (plan §6b, app-side rollback **is** wired on the
branch). Evidence log: [`scratch/embassy-p2/nebula.md`](../../../scratch/embassy-p2/nebula.md).
Verification discipline per [DOC-UPKEEP.md](../../DOC-UPKEEP.md) — every number carries its unit and
its subject, and inferences are marked as inferences.*
