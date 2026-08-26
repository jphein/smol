# smol — roadmap + decision docket

**What this document is:** the durable half of steering — conventions that outlive any wave, the
**operational safety rules** (§3a), the **research results including the refutations** (§4), and the
**decision docket with how each call resolved** (§5). Things that are expensive to learn and cheap
to lose.

**What it is NOT, any more: a status snapshot.** §1 and §2 used to list what had shipped and what was
in flight, and on 2026-08-01 they were accurate. **703 commits later they were confidently wrong** —
telling readers, among other things, to redo work that had already landed. That is not a discipline
failure; it is structural. Nothing could make this file fail, so nothing did.

Status now lives where it is machine-checked or actively filed against:
**[#148](https://github.com/jphein/smol/issues/148)** (living status; `tools/status_check.sh`
re-tests its checkable claims and exits non-zero),
**[#24](https://github.com/jphein/smol/issues/24)** (living checklist), and the epics
**[#335](https://github.com/jphein/smol/issues/335)** / **[#347](https://github.com/jphein/smol/issues/347)** /
**[#413](https://github.com/jphein/smol/issues/413)**.

> **If you are about to add a status list here, don't.** Put it in #148, where a checker can call it
> a liar. A second copy is what produced the 703-commit drift above — and the file it drifted in is
> the one README calls "start here."

**Honesty rule:** *shipped* means hardware-verified on the bench fleet; nothing here is
overstated. Verification legend: 🟢 hardware-verified · 🟡 compile/spec-verified, not fully
exercised on hardware · ⚪ design only.

**The released build number is [`rust/clock/version.txt`](../rust/clock/version.txt) — read it
there, not here.** This paragraph used to name the number and its sigil word ("v345 Riveted
Furnace"). Both were correct, and both were a **second statement of a fact the tree already
holds** — so the next release bump would have made this document wrong without touching it. The
formula below is durable; the answer is not, so the answer is no longer written down. (#232's and
#328's lesson applied to a doc: the fix for two statements of one fact is *one statement*, not a
checker on the copy.)

It is the committed ratchet in that file, *not* `git rev-list --count HEAD` — the count is only
build.rs's fallback when neither the file nor `SMOL_BUILD_NUMBER` is set. ⚠️ And per **#420**, a
build from a tree with no resolvable commit now stamps the hash `nogit` rather than a plausible
constant, so a stamp of the form `v<N>+dev.nogit` means *"this image cannot name its own commit"* —
not that it is build N.

The sigil word is derivable: `version_name_for()` in
`rust/clock/src/net/names.rs` maps `noun = FORGE.nouns[n % 20]`, `adj = FORGE.adjectives[(n / 20) % 20]`
(⚠️ **20, not 32 — do not "fix" this.** The forge/**version** table stays pinned at 20×20 inside smol;
only *node identity* moved to lexicon's 32×32 `fleet` group. Upstream's `forge` is a non-superset 14/14,
so adopting it would rename **every past build** — and version names are historical record).

Worked example, kept because it is **arithmetic and therefore cannot rot**: `345 % 20 = 5` →
`nouns[5] = "Furnace"`, and `345 / 20 = 17` → `adjectives[17] = "Riveted"`, so build 345 is
*"Riveted Furnace"* — whether or not 345 is the current release. **Any build number and sigil word
that do not satisfy the formula are a bug**, wherever you find the pair written down; that check now
belongs at the sites that still name a specific build, not here, because this document no longer
names one.

> ⚠️ **Canary pins are not releases.** Bench builds get an arbitrary high `SMOL_BUILD_NUMBER`
> (902, 903, 905, 950 …) so they out-rank the fleet's monotonic OTA gate; #128 tracks the pollution
> that causes, and `ota_publish.sh`'s ratchet heals the number forward rather than letting a pin
> poison it. **A high build number is therefore evidence of a bench pin, not of a release** — the
> two are told apart by the dev marker (`v<N>+dev.<hash>`, #218), never by the number being large.
> Which specific pin is live is a status fact; ask the broker's retained `smol/ota/staged`, or #148.

---

## 1. 🟢 SHIPPED — on the fleet

**Moved out of this document.** What has shipped is a *status* fact, and status facts rot: this
section was last true on 2026-08-01 and the tree has moved 703 commits since. It also duplicated
the tracker, and a roadmap that restates a tracked list is two statements of one fact — the one
that is not machine-checked is the one that goes stale.

Read instead:
- **[#148](https://github.com/jphein/smol/issues/148)** — the living status issue. Its claims carry
  `<!-- check: -->` annotations and `tools/status_check.sh` re-tests the machine-checkable ones and
  exits non-zero, so it cannot rot silently the way this section did.
- **[#24](https://github.com/jphein/smol/issues/24)** — the living checklist.
- `git log --oneline --merges` and the closed-issue list — the primary sources both of the above
  are derived from.


## 2. 🟡 IN FLIGHT / NEXT WAVE

**Moved out of this document** — same reason as §1, more acutely: this was the fastest-moving
section and therefore the most wrong. Current campaign state lives in the epics, which are
maintained because work is filed against them:

- **[#335](https://github.com/jphein/smol/issues/335)** — Embassy re-platform.
- **[#347](https://github.com/jphein/smol/issues/347)** — extract the Bard to its own firmware/repo.
- **[#413](https://github.com/jphein/smol/issues/413)** — per-target release artifacts.
- **[#148](https://github.com/jphein/smol/issues/148)** — the high-leverage queue across all of them.

§5's decision entries still reference "§2"; read those references as "whatever the epics above say
today", which is the point of replacing a snapshot with a pointer.


## 3. 🟡 SPEC'D / QUEUED — designed, not yet built

> **OTA (#6) and the node manager (#21) used to live here as "ready to build." Both shipped
> (2026-07-10 → 2026-07-12) and are tracked as shipped in #148/#24.** What survives from that section is one
> operational rule that has *not* been superseded — §3a.

### 3a. ⚠️ The OTA safety envelope — still binding
OTA ships, but **canary-one-board-at-a-time is still the only mass-brick defense**, and the
reason is worth restating because it is easy to assume otherwise once a feature works:

- A *broken* Rust app **cannot self-revert.** Only the 2nd-stage bootloader can, and only if it
  was built with app-rollback enabled **and** a boot failure actually resets the chip.
- **espflash's bundled ESP-IDF bootloader has app-rollback OFF**, so there is no automatic
  revert on the fleet today. The hardware spike proved otadata *slot-selection* — **not**
  revert-on-boot-fail. The primary defense is therefore the **app-side self-rollback**, plus
  ed25519 verify-before-write (#32) and the reproducible-build sha256 identity (#44).
- **Never fleet-flash blind.** Canary one board, confirm it on glass, then roll. `tools/` carries
  the publish + verify harnesses; the operator guide is [ota.md](ota.md).

### 3b. Node manager — the remaining GUI cards

The firmware and protocol halves shipped. What remains is Lovelace work — the mesh-topology card
(picture-elements v1, see **D9**) and an OTA panel that expresses canary-then-rest rather than a
single fleet button. The wire is documented in
[protocol.md](protocol.md#cfg--keyed-per-node-config-channel-56) and
[home-assistant.md](home-assistant.md).

**The queue that used to follow here has moved to [#148](https://github.com/jphein/smol/issues/148)
and [#24](https://github.com/jphein/smol/issues/24).** A hand-maintained queue beside a tracked one
is how this document came to tell readers to redo settled work: #148's own audit found it listing
"#233 merge (PR #247)" as pending when #233 had already landed by another route and #247 was closed
unmerged.


## 4. ⚪ RESEARCHED — go/no-go (nothing built)

- **4a. Retire the burst — WiFi + ESP-NOW co-channel coexist — ✅ SHIPPED.** This was the
  research bet, and it paid: the ~15 s mesh-deaf flush window was a *conservative choice*, not a
  hardware limit. **#23 landed 2026-07-12** — the radio now stays up through a WiFi sync, the
  boot assoc-freeze is gone, and much of #20 did become moot (the syncing overlay itself was
  later retired by #153). 🟢 verified on all three bench boards (July 2026): zero mesh loss across dozens of sync
  windows. Shipped; tracked in #148/#24.
  > **The honest residual — read this before assuming coexist is solved.** Ordinary mesh RX
  > while associated is reliable. **Bulk unicast RX on a crown is not:** a fetching crown goes
  > downstream-deaf within ~1 ms of its own transmit (#204). For months this was misread as
  > "coexist physics"; a packet capture and a channel audit split it into two distinct diseases
  > — a **channel mismatch** (crown on a ch1 AP vs a ch6 mesh: co-channel pulled 48 KB where
  > off-channel pulled 0) and a genuine **unicast-RX starvation** under bulk inbound. The
  > channel half is fixed (#217 rung-3 co-channel-preferred crown AP selection); the starvation
  > half is mitigated, not cured — #204 is the open issue, and it is the live reference now that
  > §2 no longer carries a snapshot.
- **4b. BLE beacon + presence (#22) — ❌ REFUTED on hardware, closed 2026-07-13.** The original
  recommendation (advertise-only iBeacon: cheap, room-level presence via fixed anchors) did not
  survive contact with the chip. **Native BLE wedges the C3's blocking runtime** — ROM busy-waits
  in btdm init / PHY calibration, reproduced at 3 hardware-distinguished hang points under
  *every* init order. Embassy/async is the only supported coexistence shape, which makes this a
  #198 dependency, not a standalone spike. Verdict confidence: high; spike cost: 1 day.
  **smol stays BLE-free**; the presence path is an ESPHome `bluetooth_proxy` on a spare ESP32 →
  HA → gateway-pull-on-flush, tracked in the #75 dollhouse epic where it's consumed. The
  host-tested HCI codec + SightingTable are preserved on `feat/22-ble-observer`.
  > **The refusal had two legs, and only one has expired.** Proxy/metric BLE was ruled out because
  > *"a single radio **+ the multi-second WiFi hold** preclude it."* **#23 retired the hold**, and
  > #198's async interleave shrinks what remains of it to ~169 ms — so that leg is gone. **The single
  > radio is untouched and load-bearing**, which is why *advertise* becomes cheap under Embassy while
  > *continuous scan* stays refused: async changes whether other work can run while the radio is busy,
  > it does not create a second radio. Full treatment in
  > [research/embassy-migration-status.md](superpowers/research/embassy-migration-status.md) §4.

  Note that
  Marauder's Watch (#58) and Treasure Hunt (#60) deliver proximity **without BLE at all**, from
  ESP-NOW roster RSSI — that turned out to be the better answer.
- **4c. Multi-hop (#13) + self-healing gateway re-election (#14) — ✅ SHIPPED.** Both landed:
  runtime re-election (#14 / #76, dead-owner takeover + split-brain heals) and routed multi-hop
  (#13, PR #123, merged 2026-07-14). A stranded leaf reaches the gateway through a relay via a
  Meshtastic-style **managed flood** (hop-limit + `(origin, msgid, frag)` seen-set, table-free so
  it rides re-election for free); the **first routed frame** was hardware-proven 2026-07-14. Prior
  art credited (ZHNetwork does routed multi-hop ESP-NOW→MQTT→HA). Honest v1 follow-ups: **#126**
  (latched-leaf channel parking / throughput), **#124** (UP2 observability envelope). Byte contract
  in [protocol.md](protocol.md) (RELAY2/RELAYACK2 + BATT2/GRID2).
- **4d. ESPHome / WLED lessons (#12 polish).** No Rust ESPHome firmware exists and the native
  API fights the burst model — **stay on MQTT** (proven strictly better on fit/effort/reuse).
  Steal from WLED (cheap, high-legibility): put every entity under **one HA device** `smol
  <id>`; split the single telemetry text line into **typed** discovery entities
  (`_voltage`/`_soc`/`_rssi`/`_role`); keep `expire_after` (NOT WLED's LWT-offline — it'd flap
  a healthy burst node offline every ~30 s). See [home-assistant.md](home-assistant.md).
  *Honest novelty framing:* the ESP-NOW→MQTT→HA substrate is commodity; smol's whole — a
  no_std Rust game-console mesh + single-radio burst time-share + retained→mesh-rebroadcast
  downlink to display-only leaves — is one-of-a-kind.

---

## 5. 🔵 DECISION DOCKET

Open decisions, ordered by leverage. **Recommendations, not decisions** — ticked as they
resolve, with *how* they resolved, because a decision that quietly went the other way is worse
than an open one.

This section **stays** in a document that just deleted its status sections, and the distinction is
worth being explicit about: a checklist of what is done is regenerable from the tracker, but *why a
call went the way it did* — and especially where it went **against** the recommendation, as **D5**
did — is recoverable from nothing. That is the content worth keeping in-repo.

Counts are deliberately not stated here. This preamble used to say "nine of twelve are now closed;
D6/D9/D11 are what's left", which is a status claim about its own list — the same shape as §1, one
level in. **The checkboxes are the count.** `grep -c '^- \[ \]'` if you need the number, and you will
get today's rather than 2026-08-01's.

- [x] **D1 — Coexist HW spike: retire the burst?** (§4a) · **RESOLVED — GO, and it shipped**
  (#23, 2026-07-12). The recommendation was right and it was the highest-leverage call in this
  docket: the deaf window is gone, the boot assoc-freeze is gone, and #20's overlay was later
  retired outright (#153). Residual in §4a — bulk-unicast crown starvation (#204) is a *different*
  disease and is still open.
- [ ] **D2 — OTA fleet-wide: enable when?** (§3a) · **Operating rule in force, formal gate never
  run.** Practice today is canary-one-board + app-side self-rollback, and that is what §3a
  documents. The **bootloader revert-on-boot-fail hardware test was never performed** — and we
  now know espflash's bundled bootloader ships with app-rollback *off*, so the honest answer is
  that unattended fleet OTA remains ungated. Leaving this open deliberately: the box should not
  be ticked by habit.
- [x] **D3 — OTA authenticity** · **RESOLVED as option A — ed25519 image signing shipped** (#32,
  2026-07-10), stronger than the recommended interim B. The leaf verifies the signature before it
  writes a byte; sha256 is used as *identity* (#44 reproducible builds), never as trust.
- [x] **D4 — OTA rollout targeting** · **RESOLVED as recommended** — per-node install orders, never
  unison. Follow-ups fixed the sharp edges found in practice: orders lost across gateway handover
  (#111), orders burned by a failed relay fetch (#134) and by a gate-rejected announce (#147).
- [x] **D5 — OTA physical long-press to accept** · **RESOLVED, but *not* as recommended — worth
  knowing.** The accept gate is **HA's native Update-entity Install button** (#33), not a press at
  the glass: `ota::OTA_AUTO_INSTALL = false` means a gated announce only advertises
  `latest_version`, and the fetch arms solely on an explicit `install` command. So remote
  mass-flash is defeated by per-node install commands + canary discipline rather than by physical
  presence. Flip that one const to restore legacy auto-install.
- [ ] **D6 — Node-manager config reach** · *All-gateway if you want every node settable from HA
  (all boards carry creds → all read MQTT config); otherwise leaves stay USB-config — honest,
  secure, MQTT-only, no unauth mesh command channel.* Still open in principle, but note the
  keyed-CFG channel (#56) made this largely moot in practice: config reaches leaves **over the
  mesh** from the elected crown, so a leaf needs no creds of its own.
- [x] **D7 — Node-manager apply semantics** · **RESOLVED as recommended.** Most CFG keys apply
  live with no reboot; `N`/`B` edge-trigger one; `R`/`W` are one-shot and never cached; long-press
  → Menu always escapes. Per-key semantics are tabulated in
  [protocol.md](protocol.md#cfg--keyed-per-node-config-channel-56).
- [x] **D8 — Publish `smol/<id>/status`?** · **RESOLVED YES — shipped** (#50, 2026-07-10).
  `net/wifi.rs` publishes retained `smol/<id>/status` = `STAT|<screen>:<page>|<build>` for itself
  *and* on behalf of leaves. It did unlock both promised payoffs: live current-screen reflection
  and the running-build read the OTA no-downgrade gate needs.
- [ ] **D9 — Mesh-topology render** · *picture-elements v1 (vanilla Lovelace, fine for a fixed
  3-board star); a custom HACS card or a `site/` SVG mirror later for a dynamic graph.* Still open
  (§3b) — though `meshscope` (#158) now covers the operator's need out-of-band, which lowers the
  urgency rather than removing it.
- [x] **D10 — BLE beacon (#22)** · **RESOLVED as NO — refuted on hardware** (2026-07-13, §4b).
  Native BLE wedges the C3's blocking runtime in ROM busy-waits; embassy/async is the only
  supported coex shape, so this is now downstream of #198 rather than a standalone spike.
  Proximity shipped anyway, without BLE, from ESP-NOW roster RSSI (#58/#60).
- [ ] **D11 — Structured HA entities + device grouping (#12)** · *Split the telemetry line into
  typed `_voltage`/`_soc`/`_rssi`/`_role` under one `smol <id>` device.* **PARTIALLY SHIPPED — and
  that is why the box stays unticked.** Live discovery on 2026-07-27 carries **3 `_voltage` and 3
  `_rssi` entities and ZERO `_soc`**, so the split landed for two of the four types and the SOC half
  did not. #12 closed 2026-07-12 and #228 enriched the discovery device block
  (model/manufacturer), neither of which is the same thing. A half-landed split is exactly the
  state a docket must not tick — finish `_soc` (and `_role`), then tick.
- [x] **D12 — Multi-hop #13 + self-healing #14** · *SHIPPED — #14 (election #76) + #13 (routed
  multi-hop, PR #123, merged 2026-07-14; first routed frame hardware-proven). Throughput +
  observability follow-ups: #126 / #124.*

---

*Statuses verified against the live tree (`git log`) + hardware findings, not asserted. The
byte-level wire contracts live in [protocol.md](protocol.md); the HA integration in
[home-assistant.md](home-assistant.md) + [`ha/README.md`](../ha/README.md).*
