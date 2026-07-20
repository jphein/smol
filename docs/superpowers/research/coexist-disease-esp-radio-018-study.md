# Does the coexist bulk-RX disease survive esp-radio 0.18 + coex? (#53 — the #198-fix question)
morpheus-89b (OTA+coexist owner), 2026-07-20. READ-ONLY study. Feeds #198/#22/#204/#26/#54.

## BOTTOM LINE UP FRONT
**No — esp-radio 0.18 + the `coex` feature does NOT fix the smol coexist bulk-RX disease, and
#198 (the Embassy/esp-radio migration) should NOT be reprioritized as "the OTA fix."**

Three independent reasons, strongest first:
1. **The `coex` feature is the wrong mechanism.** ESP-IDF/esp-radio coexistence arbitrates
   **Wi-Fi ↔ Bluetooth/BLE ↔ 802.15.4** by time-division. **ESP-NOW is not a coex participant —
   ESP-NOW *is* Wi-Fi** (vendor action frames on the Wi-Fi MAC). The smol disease is Wi-Fi-STA
   bulk-RX vs ESP-NOW-serving contention — i.e. Wi-Fi-vs-itself on one MAC — which `coex` does not
   and cannot arbitrate. Enabling `coex` changes nothing about it.
2. **The off-channel variant is RF physics, not software.** ESP-NOW must run on the *same channel*
   as the STA's associated AP; the radio cannot switch channels while associated (Espressif
   documented limitation, chip-independent). smol's mesh is ch6; when the crown's STA lands on an
   AP off ch6, the single 2.4 GHz radio can only be on one channel → the "mid-body blackout." No
   stack upgrade removes a one-radio/one-channel constraint.
3. **The watch is not valid positive evidence** (details below): it doesn't exercise the crown
   scenario, it's a C6 not a C3 (chip confound), and it *adds* BLE (which per esp-idf#17874 can
   *stop* ESP-NOW RX — a regression risk, not a fix).

The #198 migration remains worth doing for other reasons (async ergonomics, modern stack, watch
convergence, the #226-cleaner OTA API). It is just **not** the cure for the OTA coexist disease.
The real fixes stay on the smol side (co-channel pin #155 + self-heal #204 + leaf-mesh-OTA #40).

## The disease, precisely (recap #26/#204)
smol crown = WiFi-STA (fetches OTA from disks :8087) **while** serving the ESP-NOW mesh (ch6).
Minutes after crowning it goes bulk-RX-deaf: TX + broadcast RX survive (small frames: MQTT/MC/
retained look healthy, fok climbs), but sustained inbound **unicast** (the ~1 MB OTA body, SNTP
replies) starves. Two channel signatures:
- **off-ch6** (STA on an AP ≠ ch6): total mid-body blackout (`at=stall`/deadline).
- **co-channel ch6**: header corruption (`at=status`, chunk-0). Co-channel is
  NECESSARY-NOT-SUFFICIENT — a duty-cycle component remains even when channels match.
No BLE is involved on smol (native BLE refuted on C3 — btdm busy-waits wedge the ROM).

## Why esp-radio 0.18 `coex` is orthogonal to it
- ESP-IDF RF coexistence: "Wi-Fi, Bluetooth and 802.15.4 modules request RF resources from the
  coexistence module, and the coexistence module decides who will use the RF resource based on
  priority" — TDM slices for Wi-Fi / BT / BLE. ESP-NOW appears **nowhere** because it is carried by
  the Wi-Fi MAC; there is no Wi-Fi↔ESP-NOW arbiter to enable.
- The esp-rs `coex` cargo feature = enable ESP-IDF's **Wi-Fi/BT** software coexistence. The watch
  needs it because it runs **BLE (trouble-host GATT)** alongside Wi-Fi. smol runs **no BLE**, so
  `coex` is irrelevant to smol regardless.
- Maturity caveat: esp-rs coex is explicitly immature — community guidance is "coexistence
  shouldn't be used currently," working "only to some extent on ESP32-C3 and ESP32-S3." And
  esp-idf#17874 reports **ESP-NOW RX stopping completely when BLE is active**. So on the watch's
  stack, adding BLE-coex is a *risk* to ESP-NOW RX, the opposite of curing a Wi-Fi/ESP-NOW issue.

## The RF-physics constraint (the load-bearing point)
- ESP-NOW + Wi-Fi-STA share **one 2.4 GHz radio, one channel, one MAC** — on **both C3 and C6**.
  (The C6's *separate* radio is 802.15.4 for Thread/Zigbee; ESP-NOW still rides the Wi-Fi PHY.)
- "The channel of ESP-NOW must be the same as that of the connected AP … the device cannot switch
  channels after connecting to Wi-Fi" (Espressif). ⇒ off-ch6 association ⇒ ESP-NOW ch6 traffic and
  the WiFi bulk fetch cannot both be served ⇒ blackout. **Unfixable by any stack** — it is the
  radio. The only cures are architectural: keep STA+mesh co-channel, or don't fetch on the crown.
- Co-channel (both ch6): both work but *share the channel's airtime*. A minutes-long bulk RX
  competing with ESP-NOW TX/RX duty cycle = the residual starvation. A newer Wi-Fi driver (bigger
  RX windows, better PS/scheduling) *might* smooth this — but that's speculative and would be a
  Wi-Fi-driver improvement, not a `coex` effect, and it does nothing for the off-channel case.

## Why the watch is NOT valid positive evidence
Read of esp32c6-watch (READ-ONLY): src/net/ota_http.rs, src/main.rs, src/net/smol_mesh.rs.
1. **Doesn't exercise the crown scenario.** The watch self-OTAs (its own firmware from an AP) as a
   *leaf*, triggered only by a manual Power-page reboot tap (main.rs:1301-1313). It is not a
   gateway relaying a fleet's downstream ESP-NOW unicast during a minutes-long inbound bulk body.
2. **Mesh is paused during the fetch.** `ota_update(...).await` is inline in the main task loop;
   `mesh.tick`/`broadcast_diag`/`relay_emit` are all inline in the same loop (main.rs:940-1003) →
   parked at the await → app-level ESP-NOW serving stops during OTA. So even a successful watch OTA
   says nothing about fetching *while actively serving* the mesh.
3. **Chip confound (decisive).** Watch = ESP32-**C6**; smol = ESP32-**C3**. A working watch OTA
   could be the newer C6 radio, not the newer stack. Only a stack-fix transfers to the C3 fleet via
   #198; a chip-fix does not. Nothing observed isolates stack from chip.
4. **Adds BLE.** The watch's coex config is Wi-Fi+BLE+ESP-NOW; per esp-idf#17874 BLE-coex can halt
   ESP-NOW RX. If anything the watch's coex path is *more* fragile for ESP-NOW, not less.
Watch MESH_CHANNEL = 6 (smol_mesh.rs:64), same as smol; PowerSave Minimum under BLE coex, Maximum
after NTP (main.rs:765) — note PS-sleep drops ESP-NOW RX between DTIMs; smol's OTA sets PS=None
(wifi.rs) for exactly this reason. Both stacks expose PS; not a coex fix either way.

## What WOULD actually fix smol OTA (unchanged by this study)
- **Co-channel pin (#155):** force the crown's STA/AP association onto ch6 = mesh channel so
  ESP-NOW and the fetch share one channel (removes the off-channel blackout; leaves duty cycle).
- **Self-heal ladder (#204):** detect bulk-deaf → reassoc-to-ch6 → shed the crown to a node that
  isn't fetching. Still required; the disease is real on our stack.
- **Leaf-mesh-OTA (#40):** a *leaf* fetches over Wi-Fi and relays the image to the crown over
  ESP-NOW, so the crown never does Wi-Fi bulk RX while serving mesh. Architecturally sidesteps the
  contention. (See #54 mesh-OTA reference architectures — this is the strongest structural answer.)
- **Retire-the-burst / explicit time-share coexist:** deliberate Wi-Fi/ESP-NOW time-slicing on ch6.

## Reprioritization recommendation for #198
- **Do NOT reprioritize #198 as the OTA fix.** Justify #198 on its real merits (async/Embassy,
  modern esp-hal 1.1 / esp-storage 0.9 / esp-radio 0.18, the #226-cleaner OTA slot API, watch
  convergence, C6 support). Ship the OTA fix independently and now, on the current 0.2/0.15 stack
  (#155 + #204 + #40), because the migration would not cure the disease anyway.
- Keep #204's ladder on the roadmap regardless of #198 — it addresses a physics/architecture
  problem the migration doesn't touch.

## How to DEFINITIVELY settle it (the experiment, if HW time is spent)
The only conclusive test isolates *stack* from *chip* on the *crown scenario*:
1. On an **ESP32-C3** (same silicon as the fleet), build a minimal esp-radio 0.18 image that
   (a) associates STA to an AP, (b) actively serves ESP-NOW to ≥1 peer, (c) pulls a sustained
   ~1 MB HTTP body — i.e. reproduce the crown, on the new stack, on C3.
2. Run it **off-ch6** and **co-channel ch6**. Measure sustained inbound-unicast completion.
3. Verdict: if off-ch6 still blackholes (it will — physics) and co-channel still starves →
   confirmed not-a-stack-fix. If co-channel now completes cleanly → the newer Wi-Fi driver helps
   the duty-cycle case (a real but partial win; off-channel still needs #155/#204).
Until that C3+0.18 crown test exists, "the watch works" is not evidence either way.

## Sources
- ESP-IDF RF Coexistence guide (Wi-Fi/BT/802.15.4 TDM priority arbitration):
  https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-guides/coexist.html
- ESP-FAQ ESP-NOW (channel-must-match-AP; cannot switch channel while associated):
  https://docs.espressif.com/projects/esp-faq/en/latest/application-solution/esp-now.html
- ESP-NOW + WiFi coexistence channel constraint (community corroboration):
  https://circuitlabs.net/esp-now-with-wifi-coexistence/ ; https://www.esp32.com/viewtopic.php?t=14542
- esp-rs coex feature discussion (WiFi/BT scope, maturity): esp-rs/esp-hal Discussion #3456;
  Coex support tracking: esp-rs/esp-hal Issue #1598
- ESP-NOW RX halts under BLE coex: espressif/esp-idf Issue #17874 (IDFGH-16797)
- WiFi/BLE coex maturity on C3: espressif/esp-idf Issue #11280 (IDFGH-9999)
- Local: esp32c6-watch/{Cargo.toml, src/net/ota_http.rs, src/net/smol_mesh.rs, src/main.rs}
  (esp-radio 0.18 + coex + esp-now + trouble-host BLE; self-OTA on reboot; MESH_CHANNEL=6).
- smol #26 forensics + #204: scratch/ota-blackout/, feat/204-crown-selfheal.
```
NOTE: Verdict confidence HIGH on reasons 1-2 (architecture + physics, documented). The only open
empirical question is whether a newer Wi-Fi *driver* marginally improves the CO-CHANNEL duty-cycle
case — that requires the C3+0.18 crown test above; it does not change the reprioritization call.
```
