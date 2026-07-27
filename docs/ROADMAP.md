# smol — roadmap + decision docket

The steering doc: what's **shipped**, what's **in flight**, what's **spec'd** and ready to
build, what's been **researched** (go/no-go), and the open decisions. Companion to the
GitHub tracking issue [#24](https://github.com/jphein/smol/issues/24) (this is the
in-repo narrative version; the issue is the living checklist).

**Honesty rule:** *shipped* means hardware-verified on the bench fleet; nothing here is
overstated. Verification legend: 🟢 hardware-verified · 🟡 compile/spec-verified, not fully
exercised on hardware · ⚪ design only.

**Current released build: v345 "Riveted Furnace."** The number is the committed ratchet in
[`rust/clock/version.txt`](../rust/clock/version.txt) (`345`, set by release commit `315b5c8`),
*not* `git rev-list --count HEAD` — that is only build.rs's fallback when neither the file nor
`SMOL_BUILD_NUMBER` is set. The sigil word is derivable: `version_name_for()` in
`rust/clock/src/net/names.rs` maps `noun = FORGE.nouns[n % 20]`, `adj = FORGE.adjectives[(n / 20) % 20]`,
so `345 → ("Riveted", "Furnace")`. **A build number and a sigil word that don't satisfy that
formula are a bug in this document** — check them together.

> ⚠️ **Canary pins are not releases.** Bench builds get an arbitrary high `SMOL_BUILD_NUMBER`
> (902, 903, **905**, 950 …) so they out-rank the fleet's monotonic OTA gate; #128 tracks the
> pollution that causes. The Bard's on-glass evidence below is stamped **canary 905** — real
> hardware, but a dev pin. **The Bard is on `main` and not yet in a numbered release**
> (`version.txt` is still 345); the v346 wave is where it lands.

---

## 1. 🟢 SHIPPED — on the fleet

| What | Issue | Evidence |
|---|---|---|
| **MQTT-native display link** — collector retired; nodes ↔ HA directly over MQTT (retained downlink + discovery uplink) | #10/#11/#15 | Full leaf→gateway→MQTT→HA path proven across three bench boards; commits `96f44d5`, `bb5092a` |
| **Batt screen + 6-segment SOC pages** — retained `smol/display/batt`; voltage overview + big per-battery SOC/charge detail pages (short-press to page) | #16/#17 | Both payloads cached on all 3 boards; big pages render on glass; commits `96f44d5`/`f6d56d2`/`b7fd71a` |
| **Grid screen** — retained `smol/display/grid` (yurt total + two phase clamps, watts) + `SMOLv1 GRID` mesh frame | #16 | Live HA mirror `sensor.smol_display_grid`; on-glass verified |
| **Default screen at boot** — compile-time `DEFAULT_APP`/`DEFAULT_PAGE` one-shot (long-press always escapes) | #18 | Default build byte-identical (const-false DCE); verified |
| **Per-board config file** — `NODE_ID`/`DEFAULT_APP`/`DEFAULT_PAGE` in a git-ignored `board.rs` (kills the per-board version-sigil "dirty" wart) | #19 | Committed `b7fd71a`; `board.rs.example` in tree |
| **UI responsive during WiFi sync** — defer-while-interacting + long-press abort + "Syncing…" spinner | #20 | Review CLEAN; six build/clippy gates green; commit `0ce1ce9` |
| **HA availability** — discovery `expire_after` so a node goes unavailable after several missed bursts | #12 (fw half) | Live in discovery JSON |
| **Node manager — HA publish/GUI half** — Lovelace + `input_select`/automations publishing retained `smol/<id>/config/default_screen`; mirror sensors | #21 (HA half) | Deployed live to HA (config topics left empty until the firmware consumes them) |
| **The Bard — on-device tiny-LLM storyteller** — a real 260K-param TinyStories transformer (int8, executed-in-place from flash) writes a fresh story per press, typewriter-style; per-node protagonist (id8 → owl); `Bard:0` settable over CFG-S so a node boots straight into composing | #300 | 🟢 on glass id8: 67-token story, **202 ms/tok**, stack high-water **72%** of region, canary heap low-watermark **24,136 B** free of 96 KiB; port proven **bit-for-bit** vs an independent reference (146/146 token ids, full text — `runq.c` was *disqualified* as a reference, so the golden is cross-implementation, not cross-port); PR #301, **canary** build 905 |
| **Signed OTA — dual A/B + ed25519, and over the mesh to WiFi-less leaves** — the gateway fetches a signed image and relays it chunk-by-chunk over ESP-NOW (windowed-NAK); the leaf **verifies the signature before it writes a byte**, then flashes its inactive slot. One runtime-NVS-id image serves the whole fleet | #6/#40/#32 | 🟢 full ~1 MB images delivered over the mesh; verify-before-write on glass. **Canary-one-board is still mandatory** — the reason is in §3 |
| **Routed multi-hop mesh** — a gateway-deaf leaf escalates to a hop-limited managed flood (hop-limit + `(origin, msgid, frag)` seen-set, table-free so it rides re-election for free); the ordinary all-hear case stays byte-identical to single-hop | #13 | 🟢 **first routed frame 2026-07-14** — a gateway-deaf smol delivered telemetry home through a neighbour. PR #123. Honest v1 limits: best-effort uplink ACK, channel-scan throughput cap (#126), observability-via-relay (#124) |
| **Retire the burst — WiFi + ESP-NOW co-channel coexist** — the radio stays up through a WiFi sync, so the mesh never goes deaf; killed the ~15 s flush window and the boot assoc-freeze | #23 + #14/#76 | 🟢 zero mesh loss across dozens of sync windows on all 3 boards; same-channel reassoc; fully-dark dead-owner takeover + split-brain co-boot; handover-standoff/seq-race heals (#114). **Live residuals:** bulk-unicast RX starvation on a crown (#204/#217, open) and cross-channel roam not yet forced (#35, open) |
| **Keyed-CFG channel — the whole remote-config family** — one `SMOLv1 CFG <id><KEY><value>` frame carries every per-node knob over the mesh, all editable from the HA dashboard, no reflash: `S` screen · `L` LED · `U` units · `P` plugins · `Y` custom screen · `B` broker leg · `O` OTA-host allowlist · `R` reboot · `W` scan · `G`/`g` IO pin-map · `T` Bard prompt · `V` Bard delivery pace/mode | #56 + #21/#48/#43/#55/#45/#100/#52/#71/#72/#303/#302 | 🟢 byte-accurate wire in [protocol.md](protocol.md#cfg--keyed-per-node-config-channel-56); most keys apply live, `B` edge-triggers a reboot, `R`/`W` are one-shot and never cached. **`V` now has its bench run** — 160 → 120 ms/char applied *mid-narration* from HA with no restart (#302, §2), so it joins the list; `T` (#303) is merged + hardware-verified. For both, **the fleet is not rolled**. ⚠️ **`N` (WiFi-slot switch) is RETIRED** — #142 moved the fleet to single-network operation and a received `N` is now drained and ignored, so do not describe the slot switch or its "un-brickable auto-revert" as a live feature |
| **Node manager — firmware consume half** — all three sub-tasks landed: CFG-`S` default-screen consume behind a strict panic-free allowlist parse; the mesh topology/RSSI roster to HA; retained `smol/<id>/status` = `STAT\|<screen>:<page>\|<build>` | #21/#74/#50 | 🟢 `net/wifi.rs` publishes `smol/<id>/status` for itself *and* on behalf of leaves; the dashboard consumes it (`ha/dashboard/README.md`). Unlocks live current-screen reflection **and** the running-build read OTA needs |
| **The Mesh Familiar** — one creature inhabits the whole fleet and **hops to a neighbour when you unplug its board**; exactly-one-holder arbitration, migration on loss, orphan re-election | #57 | 🟢 human-verified on glass — pull the plug, watch it jump. PR #99 |
| **World Snake (MMO) · Marauder's Watch · Treasure Hunt · smol Cast** — shared 256×256 toroidal world with treasure-powers; roster-RSSI peer locator; RSSI warmer/colder game; display streamed to a WLED matrix as realtime UDP pixels | #5/#58/#60/#26 | 🟢 flashed and running fleet-wide |
| **Per-node observability + config-drift** — a retained DIAG record per node (uptime, boot-count, reset-reason, boot-slot, last-OTA outcome, heap, flush/verify counters, link quality, `net=`/`brk=`/`otah=`, the applied-config echo `cfg=`, bound-input counters `io=`) plus the gateway OLED as a live HA image | #70/#49/#74 | 🟢 a silent rollback or a drifted node is visible in HA at a glance |
| **Reproducible builds** — the release image is byte-reproducible for a fixed commit (path-remap + `SOURCE_DATE_EPOCH`), so an image's sha256 is a verifiable identity | #44 | 🟢 verified; `tools/repro_build.sh` now also carries the #300 stack-floor gate (§2) |
| **EPEver cloud-logger contained** (homelab infra) — the PE11 DIN converter was a hidden Hi-Flying cloud datalogger acting as a 2nd Modbus master; firewalled at the gateway | — | Bus corruption cut; the Batt SOC is sourced from the BMS, not EPEver |

---

## 2. 🟡 IN FLIGHT / NEXT WAVE

> ### ⚠️ DRAM budget — read this before adding any static buffer (post-#300)
> The Bard put a 260K-param model on the fleet image, and DRAM is now the binding constraint on
> the canonical tier (`espnow,cast,io,bard`). Current geometry: `.bss` **195,224 B** · `.stack`
> **76,128 B** · esp-wifi heap **96 KiB** (low-watermark 24,136 B free) · measured stack peak
> **54,960 B**.
>
> **`tools/repro_build.sh` now hard-fails a release build when the stack region drops below
> 73,728 B** — so there is only **~2,400 B of slack** before a new static allocation *breaks the
> build*, not the board. This is deliberate: the pre-gate image linked clean with 2,592 B of
> stack and would have died on hardware (see the #300 spec amendments). Do **not** raise the floor
> to make a build pass — it is derived from a measurement (peak × 4/3).
>
> If a feature needs more than that slack, the levers, cheapest first: **`SEQ_CAP`** in
> `src/bard/nano_llm.rs` (80→64 frees ~5.8 KB; 80→48 frees ~11.5 KB), the **esp-wifi heap** in
> `net::init_heap` (re-run #140's audit first), the **RX-buffer tuning** in `.cargo/config.toml`.
> Re-measure with `--features stack-paint` under live radio — idle numbers are meaningless.
> **#198/#233 (C6, 512 KB SRAM) dissolves the whole problem.**
>
> **`SEQ_CAP` is cheaper than it was (#302, 2026-07-27):** it no longer caps a STORY at all, only
> how far back the model can remember. The KV cache is a ring and the Bard narrates endlessly, so
> turning the dial down shortens its MEMORY (prose holds together less well across sentences), never
> the length of a tale. The DRAM half of #302 was therefore never needed and the slack is unchanged
> at ~2,280 B: the whole feature cost **48 B** (`.bss` is byte-identical to pre-#302).
>
> Practical consequence for the HW-held PRs (#190 HMAC, #181 ledger, #227 weather): each adds
> static state and will now meet this gate. Budget the `.bss` delta before rebasing, not after.


*"In flight" below means **commits on `main` or an open PR** — not intent. Everything else is
§3. (The #21 node-manager wave that used to sit here shipped 2026-07-10; it is now a §1 row.)*

- **The v346 release train** (PR #266) — three features held at the same bench gate, each
  stacked so the next rebases on the last: **#190 group-HMAC-SHA256 transport auth** (PR #248),
  **#181 mesh-ledger L1–L3 wiring** (PR #249, stacked on #190), **#227 weather-on-glass** (PR
  #250, gateway Open-Meteo fetch → ESP-NOW relay → fleet weather screen). 🟡 cores landed and
  host-tested; firmware wiring is HW-held. **All three add static state — budget the `.bss`
  delta against the stack floor above *before* rebasing, not after.** v346 is also the train the
  Bard lands on: `version.txt` is still 345.
- **The Bard's follow-ups** (#300 shipped, §1). Three tiers, and they are genuinely different —
  the Bard is moving fast enough that lumping them would overstate two of them:
  - 🟢 **#303 — runtime story prompt over CFG-`T`, merged + hardware-verified, fleet NOT rolled.**
    The opening is settable per node from the HA dashboard with no reflash (`3741e69`/`6bda2b0`;
    key documented in [protocol.md](protocol.md#cfg--keyed-per-node-config-channel-56)).
    Leaf-side validation against the model's own 512-token vocabulary is the part that *cannot*
    live gateway-side — the tokenizer lives with the model.
  - 🟢 **#302 — the endless story: ON GLASS.** A rolling KV window removes the terminal state so a
    tale runs indefinitely, with `inf`/`page` delivery and typewriter pace over CFG-`V`. Measured on
    id8 in endless mode, JP's own prompt running:
    | measurement | value |
    |---|---|
    | generation, `inf` mode | **224–274 ms/token** avg, max 281 (reported every 64 tokens) |
    | vs bounded stories (#300 T13) | 202 ms/tok → **10–35% slower**, as predicted: attention now always spans the full window |
    | stack high-water | **55,440 of 75,248 B = 73%**, *byte-identical across all 5 reports* |
    | live `V` change mid-tale | 160 → 120 ms/char applied **while narrating**, no restart |
    | endless-ness | display mirror 75 s apart: 416/2048 px changed, entirely different prose |

    **The leak test is the one that matters, and it is now measured rather than argued:** five
    consecutive reports spanning many tale boundaries returned the *same* high-water byte for byte. A
    sliding window that reused memory imperfectly would creep; this does not move at all. Host tests
    21 → 36; `.bss` byte-identical to pre-#302 and the whole feature costs 48 B of `.data`.
    ⚠️ **Two honest caveats.** (1) Quote the ms/tok as a **range** — the 224↔274 spread tracks whether
    the radio is bursting during that window, not the model. (2) `inf` mode is **near-continuous
    compute**, making it the fleet's **worst-case power draw**; this 🟢 says nothing about battery
    life, and [power.md](power.md) still owes a measurement for the mode the fleet actually runs in.
  - ⚪ **#304** — a custom-trained, realm-flavored model as a weights-only swap. Design only.
  > **The idea worth keeping in mind when writing about any of this:** only ~5 lines × ~15
  > characters are on the glass at once, so **the screen is a window the story moves past, not a
  > container for the story.** That is what makes an unbounded tale conceivable on a chip with
  > 400 KB of RAM — the display was never the limit, the KV cache was.
- **Embassy re-platform** (#198 spike, #233 upgrade wave, PR #247). Phases 0–3 are on `main`:
  the esp-hal 1.1 / esp-rtos-executor / esp-radio source migration (`a0d3e5a`), `wifi_task` +
  ch6-hold during the WiFi window (`0b3eb5d`, `03a09c4`), an undroppable STOP_REQ teardown
  (`266dbf0`), a deaf-window measurement tracker (`eb50384`), and a non-gateway election-OBSERVE
  burst (`ce0f34b`). 🟡 vertical-slice, not a fleet cutover. **This is the path that dissolves
  the DRAM ceiling** — the C6 has 512 KB of SRAM.
- **Crown coexist deafness** (#204, with #217 mitigations; PR #273). The one open pathology with
  fleet-wide impact: a crown under bulk unicast RX goes downstream-deaf within ~1 ms of its own
  transmit and never ACKs a response byte. **Characterised at the packet level and mitigated
  from two sides** — proactive re-association off a weak/off-channel AP (#217 rung-3, PR #273)
  and never fetching on a follower at all (#237 slice-1, shipped in v345) — but **not cured**.
  It reproduces identically on the new esp-radio 0.18 stack, so the radio rewrite is not the fix.
- **OTA robustness residuals** — #267 cross-burst fetch resume-from-offset (PR #272, commit
  `01fc810`) and #195 a self-fetch consecutive-failure cap (PR #225, commit `4c315a2`), which
  bounds how long a broken fetch can hammer the mesh-deaf window.

---

## 3. 🟡 SPEC'D / QUEUED — designed, not yet built

> **OTA (#6) and the node manager (#21) used to live here as "ready to build." Both shipped
> (2026-07-10 → 2026-07-12) and are now §1 rows.** What survives from that section is one
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
The firmware and protocol halves shipped (§1: #21/#74/#50/#56). What is left is Lovelace work:
the **mesh-topology card** (picture-elements v1 — see D9) and an **OTA panel** that expresses
canary-then-rest rather than a single fleet button. The wire is documented in
[protocol.md](protocol.md#cfg--keyed-per-node-config-channel-56) and
[home-assistant.md](home-assistant.md).

### 3c. The queue — spec'd or scoped, awaiting a train
- **#237 slices 2+** — peer-sourced leaf-mesh-OTA beyond slice-1: source inventory, crownless
  self-serve, hands-off rolls (design §12, commit `41e44fb`).
- **#161** a rich on-board mesh-OTA progress screen; **#188** live transfer progress to MQTT for
  the visualizers.
- **#126** latched-leaf channel parking (multi-hop throughput) · **#124** RELAY2/RELAYACK2 → a
  single UP2 uplink envelope · **#165** best-relay selection via link ETX.
- **#75** the dollhouse epic — per-room panels + tag-presence lighting, resting on the #72
  runtime IO registry that already shipped.
- **#152** a WASM web emulator running smol's real game code · **#158** meshscope (shipped as a
  tool) and **#159** the Bevy observatory showpiece.
- **#230** on-device WiFi provisioning (backport from esp32c6-watch) · **#229** the open decision
  on whether the C6 watch is a companion device or a first-class fleet member.

---

## 4. ⚪ RESEARCHED — go/no-go (nothing built)

- **4a. Retire the burst — WiFi + ESP-NOW co-channel coexist — ✅ SHIPPED.** This was the
  research bet, and it paid: the ~15 s mesh-deaf flush window was a *conservative choice*, not a
  hardware limit. **#23 landed 2026-07-12** — the radio now stays up through a WiFi sync, the
  boot assoc-freeze is gone, and much of #20 did become moot (the syncing overlay itself was
  later retired by #153). 🟢 verified on all 3 boards: zero mesh loss across dozens of sync
  windows. Now a §1 row.
  > **The honest residual — read this before assuming coexist is solved.** Ordinary mesh RX
  > while associated is reliable. **Bulk unicast RX on a crown is not:** a fetching crown goes
  > downstream-deaf within ~1 ms of its own transmit (#204). For months this was misread as
  > "coexist physics"; a packet capture and a channel audit split it into two distinct diseases
  > — a **channel mismatch** (crown on a ch1 AP vs a ch6 mesh: co-channel pulled 48 KB where
  > off-channel pulled 0) and a genuine **unicast-RX starvation** under bulk inbound. The
  > channel half is fixed (#217 rung-3 co-channel-preferred crown AP selection); the starvation
  > half is mitigated, not cured (§2).
- **4b. BLE beacon + presence (#22) — ❌ REFUTED on hardware, closed 2026-07-13.** The original
  recommendation (advertise-only iBeacon: cheap, room-level presence via fixed anchors) did not
  survive contact with the chip. **Native BLE wedges the C3's blocking runtime** — ROM busy-waits
  in btdm init / PHY calibration, reproduced at 3 hardware-distinguished hang points under
  *every* init order. Embassy/async is the only supported coexistence shape, which makes this a
  #198 dependency, not a standalone spike. Verdict confidence: high; spike cost: 1 day.
  **smol stays BLE-free**; the presence path is an ESPHome `bluetooth_proxy` on a spare ESP32 →
  HA → gateway-pull-on-flush, tracked in the #75 dollhouse epic where it's consumed. The
  host-tested HCI codec + SightingTable are preserved on `feat/22-ble-observer`. Note that
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
than an open one. **Nine of twelve are now closed;** D6/D9/D11 are what's left.

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
