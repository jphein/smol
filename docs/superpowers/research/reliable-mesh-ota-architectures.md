# Reliable mesh-OTA reference architectures — mapped to smol — a study

**Issue:** [#54](https://github.com/jphein/smol/issues/54) · **Companion to:** [ota.md](../../ota.md) (Leaf mesh-OTA #40), [#198 Embassy migration](https://github.com/jphein/smol/issues/198), [#204 crown coexist RX-starvation](https://github.com/jphein/smol/issues/204) · **Status:** research/survey only, no firmware · **Author:** nebula-babel · **Date:** 2026-07-19

> **Bottom line up front.** smol already shipped the essence of the winning architecture. The #40 leaf-mesh-OTA relays a full signed image over ESP-NOW (231-byte chunks, 64-chunk windowed NAK, ed25519-verified before any flash write) so WiFi-less leaves **never** do a WiFi bulk fetch — which is exactly the "ESP-NOW-native OTA" reference pattern. The remaining coexist-bulk-OTA disease is now surgically small: **only the gateway still WiFi-fetches, and only that one node goes mesh-deaf** (~17 s) while it does. The best fix is therefore **not a new framework** — it's extending smol's own #40 relay toward **peer-sourced store-and-forward** propagation. The two external mesh frameworks either break smol's headless-leaf economics (esp-mesh-lite: WiFi everywhere) or fork its silicon (Thread: the C3 has no 802.15.4 radio at all).

Verification legend: 🟢 verified against smol source/docs · 🔵 verified via Espressif docs/source · ⚪ inferred/design-judgement.

---

## Ranked verdict

| # | Architecture | Verdict | One-line |
|---|---|---|---|
| **2** | **ESP-NOW-native OTA** | **ADOPT (as an *extension*)** | smol already does the single-hop version; the worthwhile delta is **peer-sourced store-and-forward** so an *already-updated* node sources the next one over ESP-NOW → shrinks/eliminates the gateway's WiFi-fetch window. Keep canary. |
| 1 | esp-mesh-lite (WiFi tree + native OTA) | **SKIP** (ADMIRE the tree-propagation idea) | Elegant OTA-down-the-tree, but it's a WiFi-**everywhere** tree topology — every leaf must WiFi-associate, killing the headless-leaf model. C/ESP-IDF component; no_std-Rust reimpl infeasible. |
| 3 | Thread/OpenThread on C6 | **SKIP now, ADMIRE long-term** | The architecturally "right" mesh (separate 802.15.4 radio class, IP-native, mature Matter/CoAP OTA) — but the **C3 fleet has no 802.15.4 radio at all**, so Thread *forks* the fleet. Revisit only on an all-C6 future (the watch is the first C6). |

---

## The disease, precisely scoped 🟢

From [`ota.md`](../../ota.md) (verbatim): *"the mesh is deaf for the whole download (longer than a normal burst; a proven canary self-updated build 58→59 in ~17 s)."* The mechanism: an ESP32-C3 has **one 2.4 GHz radio**, time-sliced between the WiFi-STA HTTP fetch and ESP-NOW. During an HTTP **bulk** transfer the radio is effectively committed to the AP link, so the node cannot service the mesh — it goes **mesh-deaf** for the whole ~590 KB–1 MB download.

Two things already contain it in main:
- **#40 leaf-mesh-OTA** removed the disease from *leaves* entirely — they receive the image over ESP-NOW and never WiFi-fetch. 🟢
- The **coexist channel policy** (#180/#40, roam-AP pinned to ch6) lets the gateway's WiFi and the mesh share a channel; during the gateway's fetch a leaf *"holds ch6 through the gateway's fetch … treats that silence as 'fetching', not 'gateway dead'."* 🟢

So the disease's **entire remaining surface = the gateway's own WiFi bulk fetch**. Every architecture below is judged on: *does it shrink or remove that last window, at acceptable cost?*

> **Scope boundary — the radio-level root-cause question.** A sibling study asks whether the disease even *reproduces* on the new **esp-radio 0.18 + coex** ([#198](https://github.com/jphein/smol/issues/198) Embassy) stack — i.e. whether esp-radio's coexistence scheduler time-slices bulk RX well enough that a node is no longer fully mesh-deaf during a fetch (the live symptom on the current stack is [#204](https://github.com/jphein/smol/issues/204), crown coexist RX-starvation). That is the *root-cause* question. This study (#54) is orthogonal: *architecture-level* fixes that hold **regardless** of whether the radio-level fix lands. If the radio-level study finds the disease is gone on 0.18, the pressure on Option 2's peer-sourcing extension drops from "fix" to "nice-to-have."

---

## Baseline — smol's current OTA architecture 🟢

The thing every candidate is mapped against ([`ota.md`](../../ota.md), `rust/clock/src/ota.rs`):

- **Transport:** WiFi board = HTTP self-fetch into the inactive A/B slot; WiFi-less leaf = **ESP-NOW relay from the gateway** (#40).
- **Leaf relay wire:** 231-byte chunks, **64 chunks/window**, per-window **missing-bitmap NAK** (retransmit only gaps; all-zero bitmap = advance — the *only* positive ack). Not the general 64 B RELAY path — a dedicated windowed protocol near the 250 B ESP-NOW MTU. 🟢
- **Trust:** ed25519 signature over `M = "build|size|sha256hex"` (offline key), verified **before any flash write**; SHA-256 integrity before `otadata` is touched; partition-scoped writer physically cannot reach the active slot or `otadata`. 🟢
- **Topology:** flat ESP-NOW flood + **elected crown/gateway**; the gateway is the leaves' OTA proxy.
- **Safety:** **canary one board at a time is STRUCTURAL** — "there is **no fleet-fetch topic**"; per-node `install` only. Because 2nd-stage bootloader revert-on-boot-fail is **OFF/unproven**, mass-push is a mass-brick risk. App-side self-rollback (hear-a-mesh-frame / DHCP self-test) is the primary net. 🟢
- **Ordering:** during a leaf relay the gateway **suppresses its own self-OTA** (leaves first, gateway last) so a relay is never cut short by a gateway reboot. 🟢

This baseline is mature and HW-proven. The candidates are judged as *deltas* to it, not greenfield.

---

## Candidate 1 — ESP-WIFI-MESH / esp-mesh-lite 🔵

**What it is.** Espressif's official **WiFi tree mesh**: a root node associates to the router; every other node is a WiFi station that joins a parent's soft-AP, forming a self-healing tree. esp-mesh-lite is the lighter, current-gen variant (the older ESP-WIFI-MESH used the "Mupgrade" OTA helper).

**Native mesh-OTA mechanism (verified).** The root downloads the image **once** from an external URL, then it **propagates down the tree**: children request the image from their parent (not the URL); `esp_mesh_lite_wait_ota_allow()` gates a node so a parent/higher-level node finishes fetching **before** its children pull from it; if a parent can't serve the version, the child **falls back to the external URL**. Mupgrade (legacy) splits the image into fragments and flashes multiple devices in parallel. Recent releases hardened this against **topology change mid-upgrade**.

**Map to smol.**
- ✅ The "root fetches once, propagates to the rest" idea is *exactly* smol's gateway-proxy instinct — validation that the pattern is sound.
- ❌ **Topology mismatch that breaks the product.** esp-mesh-lite is WiFi-**everywhere**: every leaf is a WiFi station holding a soft-AP for its children. smol's headless leaves are **cheap boards that deliberately never WiFi-associate** — that's the entire cost/power argument of the fleet. Putting every leaf on WiFi discards it.
- ❌ **No ESP-NOW.** It replaces the flat flood + crown election with a managed WiFi tree — a total re-architecture, not a delta.
- ❌ **Language/stack.** It's a C ESP-IDF component; smol is `no_std` Rust on esp-hal/esp-radio. A faithful reimpl is a multi-month effort with no reuse.
- ⚪ **Coexist disease:** it *does* dodge the ESP-NOW-vs-WiFi coexist problem — because there's no ESP-NOW at all. But every non-leaf node still does a WiFi bulk transfer to its children; it trades "mesh-deaf during fetch" for "tree-serving load," and the fetch is still WiFi.

**Verdict: SKIP (ADMIRE the tree-propagation idea).** The one transferable idea — *fetch once at the root, propagate to peers* — smol already embodies via the gateway proxy. Everything else costs the headless-leaf economics and a full rewrite.

---

## Candidate 2 — ESP-NOW-native OTA (`esp-now/examples/ota`) 🔵🟢

**What it is (verified).** Espressif's `esp-now` component ships an OTA example: an **initiator** connects to an AP, HTTP-downloads the new image, scans for **responders**, and **chunks the image over ESP-NOW** to them; responders run an OTA task that receives frames, writes flash, and sets next-boot when complete. It can upgrade **multiple responders at once**, bounded by `CONFIG_ESPNOW_OTA_RETRY_COUNT`.

**Map to smol — this *is* #40, with two honest caveats.**
- ✅ **Core pattern already shipped.** "Only the initiator WiFi-fetches; responders receive over ESP-NOW" is smol's leaf-mesh-OTA. smol's windowed-NAK bitmap is arguably *more* bandwidth-efficient than a fixed retry count (it retransmits only gaps). So the stock example is a **peer**, not an upgrade, of what smol has.
- ⚠️ **The example's headline feature is one smol deliberately rejected.** The example upgrades **multiple responders simultaneously**; smol's ota.md makes **canary-one-board STRUCTURAL** (no fleet-fetch topic) because bootloader revert is OFF. Adopting multi-target push would *re-introduce* the mass-brick risk smol engineered away. So the example's most-marketed capability is a **safety anti-pattern** for smol. 🟢
- ⚠️ **The initiator still WiFi-fetches.** In the stock example the initiator has the *identical* coexist window smol's gateway has — so stock Option 2 does **not** fix the last window; it only confirms leaves are already immune.

**The worthwhile delta — peer-sourced (store-and-forward) propagation.** Neither the stock example nor smol #40 does this: today the image always originates from the node that WiFi-fetched. The extension: once the **canary** node holds a verified image, let **it** be the ESP-NOW source for the next node — image travels node→node over the mesh, and the *gateway need not WiFi-fetch at all* for boards a peer can already serve. This:
- removes the gateway's WiFi-fetch window for all-but-the-first update (the first node still fetches once),
- stays inside ESP-NOW (no topology change, headless leaves preserved),
- **composes with canary** — peer-sourcing is still one-target-at-a-time; it changes *who sources*, not *how many update at once*,
- reuses smol's existing 231 B / windowed-NAK / ed25519-before-flash relay wholesale (the receiver path is unchanged; only the *source* moves from gateway to an updated peer).

**Verdict: ADOPT — as an extension, not a wholesale import.** The stock example is validation, not new capability; its multi-target feature is unsafe for smol; but its underlying model, pushed to **peer-sourced store-and-forward**, is the **single best lever on the remaining coexist window** because it builds directly on shipped, HW-proven code. → filed as a design issue, **gated on the esp-radio-0.18 coex root-cause finding** (#198/#204).

---

## Candidate 3 — Thread / OpenThread on the C6 🔵

**What it is (verified).** The ESP32-**C6** has a dedicated **IEEE 802.15.4 radio** (Thread 1.3, Zigbee 3.0, Matter). Thread is an IPv6/6LoWPAN mesh with real self-healing routing where every router is a peer; OTA rides standard, transport-agnostic mechanisms — **Matter OTA / CoAP block-wise transfer** over the Thread mesh (naturally chunked, reliable, multi-hop-routed).

**Map to smol.**
- ✅ **Architecturally the "right" mesh.** A separate low-power radio class means the WiFi-vs-mesh coexist disease **structurally cannot occur** on the mesh side (Thread and WiFi still time-share the *one* 2.4 GHz front-end via PTA coex, but Thread traffic is small, always-on, and never a bulk-fetch monopolizer). Routing + reliable block-wise OTA come for free from a mature stack.
- ❌ **The C3 fleet has NO 802.15.4 radio — "entirely" (verified).** A Thread mesh **cannot include a single existing C3 board.** So this isn't "fix smol's OTA," it's "**abandon the C3 fleet** for an all-C6 mesh." That's a strategic silicon bet, not a coexist fix.
- ❌ **Footprint + rewrite.** OpenThread+Matter stacks want ≥4 MB flash (8 MB for comfortable OTA) and are large C stacks; smol's `no_std` Rust + ESP-NOW-flood identity would be wholesale replaced. The SMOLv1 wire protocol, election, familiar, Snake — all re-homed.
- 🔗 **The one live thread:** the **watch is a C6** already speaking SMOLv1 over ESP-NOW. If the fleet ever migrates to C6 silicon (the #198 Embassy future runs there too), Thread becomes a *real* option and this study should be reopened.

**Verdict: SKIP for the current fleet; ADMIRE as the all-C6 endgame.** Right mesh, wrong silicon for today's boards. Filed as a strategic marker on #198, not an actionable OTA fix.

---

## The focused verdict — which best fixes the coexist-bulk-OTA disease

1. **Radio-level (root cause), the sibling study:** determine whether esp-radio 0.18 + coex still goes mesh-deaf during a bulk fetch (the [#198](https://github.com/jphein/smol/issues/198) Embassy stack; live symptom [#204](https://github.com/jphein/smol/issues/204)). If it doesn't, the disease is *cured at the source* on the #198 stack and the rest is optimization.
2. **Architecture-level (this study):** **extend #40 to peer-sourced store-and-forward relay** (the Option-2 delta). Highest leverage, lowest risk — it reuses smol's HW-proven relay and only moves the *source* of the image from the gateway to an already-updated peer, removing the gateway WiFi-fetch window for every board after the first, while preserving canary and headless leaves.
3. **Reject:** esp-mesh-lite (breaks headless-leaf economics + full rewrite) and Thread-now (forks the C3 fleet). Keep both as documented "if the fleet goes all-WiFi / all-C6" markers.

Do **not** import the esp-now example's multi-target push — it undoes smol's structural canary safety while bootloader revert is unproven.

---

## Sources

- smol: [`docs/ota.md`](../../ota.md) (Leaf mesh-OTA #40, canary rule, wire format, recovery), `rust/clock/src/ota.rs` (self-fetch vs leaf-relay, `OtaLeafSession`).
- esp-mesh-lite native OTA: [ESP-FAQ WiFi-mesh dev framework](https://docs.espressif.com/projects/esp-faq/en/latest/application-solution/wifi-mesh-development-framework.html), [Mupgrade (ESP-MDF)](https://docs.espressif.com/projects/esp-mdf/en/latest/api-guides/mupgrade.html), [esp-mesh-lite CHANGELOG](https://github.com/espressif/esp-mesh-lite/blob/master/components/mesh_lite/CHANGELOG.md).
- ESP-NOW-native OTA: [`esp-now/examples/ota` README](https://github.com/espressif/esp-now/blob/master/examples/ota/README.md), [ESP Component Registry — esp-now OTA example](https://components.espressif.com/components/espressif/esp-now/versions/2.4.0/examples/ota), [esp-now User_Guide](https://github.com/espressif/esp-now/blob/master/User_Guide.md).
- Thread/C6: [ESP32-C6 Matter/Thread sourcing 2026 (Cosolvic)](https://cosolvic.com/blog/esp32-c6-matter-thread-sourcing-2026/), [zigpy: C6/H2 802.15.4 + OpenThread](https://github.com/zigpy/zigpy/discussions/783), [ESP32 SoC comparison (espboards.dev)](https://www.espboards.dev/blog/esp32-soc-options/) — confirms C3 has no 802.15.4.
- ESP-IDF OTA (A/B, rollback): [ESP-IDF OTA guide](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/ota.html).
