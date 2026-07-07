# Bluetooth LE Audio on ESP32 — can the smol C3 do wireless earbuds?

Research doc (2026-07-07). Question: the smol handheld is an **ESP32-C3** ("Bluetooth 5 LE"
per [README](README.md)). Can it stream to **LE Audio / Auracast earbuds**? If not, which
ESP32 chip can, and is it even worth chasing?

> **TL;DR**
> - **The C3 cannot do LE Audio. Not close.** Its radio is Bluetooth **5.0-class LE with
>   *no* Isochronous Channels (ISO)** — the exact Link-Layer feature LE Audio is built on —
>   and Espressif ships **no** LE Audio software stack for it.
> - **No *shipping* ESP32 does LE Audio today.** Espressif's own feature matrix marks
>   **LE Isochronous Channels (BIS/CIS) as unsupported across the controller and *both* host
>   stacks** (Bluedroid + NimBLE). The C6/H2 are "Bluetooth 5.3 certified" but **still have no
>   ISO** — the C6 LE-Audio request was closed **"Won't Do."**
> - **LE Audio is gated to two brand-new chips: the ESP32-H4 and ESP32-S31**, via a binary
>   `esp-ble-audio-lib` (derived from Zephyr). Both were **announced but are not yet in normal
>   mass-market distribution** as of mid-2026, and the stack is preview-grade.
> - **If you want wireless audio on an ESP-class device *now*, the mature path is Classic
>   **A2DP** on the *original* ESP32** (music to any Bluetooth headphones), not LE Audio.
> - **For smol specifically: don't chase Bluetooth audio at all.** Wired **I²S** or the
>   existing **ESP-NOW** plan ([walkie-talkie.md](walkie-talkie.md)) are the sane options on a C3.

---

## 1. What "LE Audio" actually is

LE Audio is the audio system introduced with **Bluetooth Core Specification 5.2** (spec
enhanced **Dec 2019**, announced at **CES Jan 2020**). The full profile suite + codec were
**completed July 2022** — so "arrived in 5.2" is true for the radio foundation, but the usable
stack is a 2022 thing. It is a ground-up replacement for **Classic Bluetooth audio (A2DP + HFP)**,
running on **Bluetooth Low Energy** instead of the old **BR/EDR** ("Bluetooth Classic") radio.

Three pillars:

**a) The LC3 codec** (*Low Complexity Communication Codec*, Fraunhofer IIS + Ericsson,
royalty-free). Replaces A2DP's ancient **SBC**. Bitrate **16–345 kbps per channel**, sampling
**8–48 kHz**, 7.5/10 ms frames. Delivers **equal-or-better quality at roughly half the data rate**
of SBC, with lower power and better packet-loss concealment. (Don't confuse it with **LC3plus** —
a separate ETSI/DECT codec, higher bitrate, *not* royalty-free, *not* the LE Audio codec.)

**b) Isochronous Channels (ISO)** — the make-or-break feature. New **Link-Layer** transport in
5.2 for time-synchronized, latency-bounded audio, so multiple receivers render in lockstep and the
radio sleeps predictably between scheduled bursts. Two kinds:

| | **CIS** (Connected Isochronous Stream) | **BIS** (Broadcast Isochronous Stream) |
|---|---|---|
| Model | Point-to-point, connection-oriented | One-to-many, connectionless |
| Direction | **Bidirectional** | Unidirectional |
| Reliability | Acknowledged (retransmits) | Unacknowledged (retransmit count + interleaving) |
| Grouped as | **CIG** (up to 31 synchronized streams) | **BIG** (up to 31 streams) |
| Powers | **Unicast** LE Audio | **Broadcast** LE Audio / **Auracast** |

**c) Two operating modes:**

- **Unicast (CIS/CIG)** — the connected, two-way mode; the **single replacement for A2DP media
  *and* HFP hands-free at once**. A CIS is bidirectional, so **one** LE connection carries a stereo
  **music** stream *and* a **mic** stream *simultaneously*. This kills Classic Bluetooth's worst
  wart: on Classic, turning on the mic (HFP) collapses playback to low-fi **mono** — LE Audio keeps
  stereo while the mic is live. This is the "wireless headset" mode.
- **Broadcast (BIS/BIG) / Auracast** — one transmitter → **unlimited** receivers, **no pairing**.
  Open or shared-key. Discover-and-join via a phone "assistant," QR, or NFC. Think silent gym TVs,
  airport announcements, multi-language at venues, and modern assistive-listening (telecoil's successor).

**How it differs from Classic A2DP/HFP** (the crux of why old ESP32 code doesn't transfer):

| Layer | Classic (A2DP / HFP) | LE Audio |
|---|---|---|
| Radio | **BR/EDR** (Bluetooth Classic) | **Bluetooth Low Energy** |
| Codec | SBC (media) / CVSD·mSBC (voice) | **LC3** (both) |
| Transport | L2CAP/AVDTP + RFCOMM | **ISO channels** (CIS/BIS) |
| Media + mic | Two profiles; mic ⇒ mono downgrade | One path; simultaneous stereo + mic |
| Topology | One-to-one only | 1:1, multi-stream, **broadcast** |
| Latency | ~100–200 ms | ~20–30 ms |

**Host-stack profiles LE Audio needs** (the "Generic Audio Framework," GAF): **BAP** (base stream
setup), **PACS** (published capabilities), **ASCS** (stream endpoints), **BASS** (broadcast scan),
**CAP** (coordinating layer), **CSIP** (device sets, e.g. L/R buds), plus control profiles
**VCP/MICP/MCP/CCP** and top-level apps **TMAP** (telephony+media), **HAP** (hearing aids),
**PBP** (public Auracast), **GMAP** (gaming). All of this is software that must sit on top of a
controller that *has* ISO — which is exactly what mainstream ESP32s lack.

> Sources: [Bluetooth SIG — LE Audio specs](https://www.bluetooth.com/learn-about-bluetooth/feature-enhancements/le-audio/le-audio-specifications/) ·
> [SIG — ISO channels FAQ](https://www.bluetooth.com/blog/10-frequently-asked-questions-on-le-isochronous-channels/) ·
> [SIG — Auracast overview (PDF)](https://www.bluetooth.com/wp-content/uploads/2024/05/2505_Paper_An-Overview-of-Auracast.pdf) ·
> [Wikipedia — LC3](https://en.wikipedia.org/wiki/LC3_(codec)) ·
> [RF Wireless — LC3 vs SBC](https://www.rfwireless-world.com/terminology/difference-between-lc3-and-sbc-in-ble) ·
> [SoundGuys — LE Audio/LC3](https://www.soundguys.com/bluetooth-le-audio-lc3-explained-28192/) ·
> [FreeCodeCamp — LE Audio Handbook](https://www.freecodecamp.org/news/the-bluetooth-le-audio-handbook/) ·
> [Zephyr — LE Audio architecture](https://docs.zephyrproject.org/latest/services/connectivity/bluetooth/api/audio/bluetooth-le-audio-arch.html) ·
> [Novel Bits — profile stack](https://novelbits.io/bluetooth-le-audio-auracast-profiles/)

---

## 2. Chip support — the crux (radio **and** software stack)

Two things must both be true for LE Audio: (1) the **controller/radio supports ISO channels**,
and (2) there's a **host software stack** (LC3 + BAP/PACS/ASCS/CAP…). Espressif publishes the
answer directly in the ESP-IDF **["Major Feature Support Status"](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-guides/ble/ble-feature-support-status.html)**
matrix, and it is blunt:

> **"LE Isochronous Channels (BIS/CIS)"** is listed under Bluetooth Core 5.2, and it shows the
> **unsupported** marker across **all three** columns — **ESP Controller, ESP-Bluedroid Host, and
> ESP-NimBLE Host** — for the mainstream ESP-IDF chips.

So for every chip you can actually buy in volume today (ESP32, C3, C6, H2, C5, C2, S3), **ISO is
not supported at either the controller or host level.** "Bluetooth 5.3 certified" on the C6/H2 is
about *other* 5.x features (extended advertising, coded/2M PHY, etc.), **not** ISO. The smoking
gun: the GitHub request **["Support LE Audio on ESP32-C6" (#12277)](https://github.com/espressif/esp-idf/issues/12277)**
was **closed with resolution "Won't Do."**

LE Audio is instead being introduced on **two new chips**, the **ESP32-H4** and **ESP32-S31**, via
a dedicated (binary) library **[`esp-ble-audio-lib`](https://github.com/espressif/esp-ble-audio-lib)**
whose repo contains folders only for **`esp32h4`** and **`esp32s31`**, plus the new
**[ESP-BLE-ISO](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s31/api-reference/bluetooth/esp-ble-iso.html)**
and **ESP-BLE-AUDIO** APIs (documented under the S31/H4, marked preview).

### Per-chip table

| Chip | Radio generation (per datasheet) | ISO (CIS/BIS) at radio? | Espressif LE Audio stack? | Status for wireless earbuds |
|---|---|---|---|---|
| **ESP32** (2016) | **BT Classic BR/EDR + BLE 4.2** | ❌ (predates 5.2) | ❌ LE Audio — but ✅ **Classic A2DP/HFP** | ✅ **Best mature ESP path** — A2DP to any BT headphones (see §4) |
| **ESP32-C3** *(smol)* | **Bluetooth 5 (LE)**, 5.0-class | ❌ **No** | ❌ **None** | ❌ **Cannot do LE Audio, nor Classic** (no BR/EDR either) |
| **ESP32-C6** | **Bluetooth 5 (LE)**, "5.3 certified" | ❌ **No** (5.3 ≠ ISO here) | ❌ **"Won't Do"** ([#12277](https://github.com/espressif/esp-idf/issues/12277)) | ❌ No LE Audio; no Classic |
| **ESP32-H2** | **Bluetooth 5 (LE)**, "5.3 certified" | ❌ **No** | ❌ None | ❌ No LE Audio; no Classic |
| **ESP32-C5** | **Bluetooth 5 (LE)**, "Core 6.0 certified" | ❌ **No** (still no ISO listed) | ❌ None | ❌ No LE Audio; no Classic |
| **ESP32-C2** (ESP8684) | **Bluetooth 5 (LE)**, low-cost | ❌ No | ❌ None | ❌ No LE Audio; no Classic |
| **ESP32-S3** | **Bluetooth 5 (LE)** + Wi-Fi | ❌ No | ❌ None | ❌ No LE Audio; no Classic |
| **ESP32-P4** | **No radio at all** | — | — | ❌ Needs a C-series companion (usually C6) for any wireless |
| **ESP32-H4** ⭐ | **Bluetooth 5.4 (LE)**, cert. BT 6.0; +802.15.4 | ✅ **Yes — "LE Audio, LE Isochronous Channels (BIS and CIS)"** | ✅ `esp-ble-audio-lib` (preview, binary) | ⚠️ **The LE Audio chip** — but *announced Apr 2024*, not yet common |
| **ESP32-S31** ⭐ | **Bluetooth 5.4 (LE)** + BT Classic; Wi-Fi 6 + 802.15.4 | ✅ Yes (ESP-BLE-ISO/AUDIO) | ✅ `esp-ble-audio-lib` (preview, binary) | ⚠️ New multi-protocol chip, preview-stage docs |

Key nuances that trip people up:

- **"BT version certified" ≠ "ISO supported."** The C6 is 5.3-certified and the C5 is 6.0-certified,
  yet **neither exposes isochronous channels**. The version bump covers advertising/PHY features.
  Espressif's own framing at the H4 launch: the H4 marks the transition of Espressif's BLE chips
  **"from Bluetooth 5.0 to Bluetooth 5.4"** — i.e. all prior LE chips (C3/C6/H2/C5/…) are, for
  audio purposes, **5.0-class**.
- **The H4/S31 stack is Zephyr-derived and shipped as *binary* libraries** ("derived from the
  Zephyr Project," Apache-2.0, compiled form) — a preview, not a battle-tested product line.
- **Could a third-party stack force ISO onto a C6?** No. **Zephyr** has excellent LE Audio *host*
  samples (BAP Broadcast Source, etc.) and runs on the C6 — but the C6 **controller has no ISO**,
  so there is no transport for those profiles to use. LE Audio on the C6 is repeatedly described as
  "ongoing work / not implemented." The blocker is **silicon**, not just Espressif's SDK.

> Sources: [ESP-IDF feature-support matrix](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-guides/ble/ble-feature-support-status.html) ·
> [ESP32-H4 announcement](https://www.espressif.com/en/news/ESP32-H4) ·
> [ESP32-H4 product page](https://www.espressif.com/en/products/socs/esp32-h4) ·
> [ESP32-S31 product page](https://www.espressif.com/en/products/socs/esp32-s31) ·
> [esp-ble-audio-lib repo](https://github.com/espressif/esp-ble-audio-lib) ·
> [ESP-BLE-ISO API docs](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s31/api-reference/bluetooth/esp-ble-iso.html) ·
> [C6 LE Audio issue #12277 "Won't Do"](https://github.com/espressif/esp-idf/issues/12277) ·
> [ESP32-C6 datasheet page](https://documentation.espressif.com/esp32-c6_datasheet_en.html) ·
> [ESP32-P4 wireless companion (Espressif)](https://developer.espressif.com/blog/wireless-connectivity-solutions-for-esp32-p4/) ·
> [Zephyr ESP32-C6 status](https://hubble.com/community/guides/zephyr-rtos-on-esp32-c6-what-s-supported-and-what-s-still-missing/)

---

## 3. Verdict for smol (the ESP32-C3)

**No — the ESP32-C3 cannot do LE Audio for wireless earbuds/headset. Definitively.** Three
independent reasons, any one of which is fatal:

1. **Radio generation.** The C3 is **Bluetooth 5.0-class LE**. LE Audio requires **5.2+ Isochronous
   Channels**, which the C3 silicon does not implement.
2. **No ISO transport.** Espressif's feature matrix lists **LE Isochronous Channels as unsupported**
   on the controller and on both host stacks (Bluedroid, NimBLE). There is nothing to carry LC3 audio.
3. **No stack.** There is no ESP-IDF LE Audio component, example, or LC3-over-ISO path for the C3;
   the LE Audio libraries (`esp-ble-audio-lib`, ESP-BLE-ISO/AUDIO) target only the **H4/S31**.

And note a *second* limitation unique to the C-series: the C3 has **no Bluetooth Classic (BR/EDR)**
either, so it can't fall back to A2DP the way the original ESP32 can. For the C3, **all** standard
Bluetooth audio routes — LE Audio *and* Classic A2DP — are closed.

---

## 4. If you want ESP + wireless audio, the real paths today

### (a) Music to LE Audio earbuds → **ESP32-H4** (or S31) + `esp-ble-audio-lib` — *bleeding edge*

The only ESP route to true LE Audio. The H4 has the ISO radio and Espressif's preview LE Audio
library. Realistically this means: BT 5.4 chip announced April 2024, **binary** Zephyr-derived
stack, **preview** docs, and **limited retail availability** in mid-2026. Treat as **experimental**,
not production. Fine for a research spike; risky as a product foundation right now.

### (b) Two-way headset + mic → **also H4/S31**, but even less proven

Bidirectional unicast (CIS) is the LE Audio "headset" case. Same chips, and the mic/CIS direction
is the least-exercised part of a preview stack. Expect rough edges. There is no mainstream-ESP
shortcut to a two-way LE Audio headset today.

### (c) The mature alternative — **Classic A2DP on the *original* ESP32** — *works now*

If the real goal is "**play audio to wireless headphones people own**," skip LE Audio and use
**Bluetooth Classic A2DP on the original ESP32** (the 2016 chip with BR/EDR). The community library
**[pschatzmann/ESP32-A2DP](https://github.com/pschatzmann/ESP32-A2DP)** does **A2DP source**
(ESP32 → any BT speaker/headphones, SBC, 44.1 kHz/16-bit stereo) and **A2DP sink**, on
Arduino/PlatformIO/IDF. This is **stable and widely used**.

- Caveat: that library does **A2DP media only** — **HFP/HSP (mic) is not supported** in it, so it's
  music-out, not a two-way headset. (ESP-IDF itself has a separate HFP API, but it's far more work.)
- Caveat: this needs the **original ESP32** (or S31, which also has BR/EDR). The **C3/C6/H2/C5/S3
  have no Classic radio**, so A2DP is impossible on them too.

**Maturity summary:** ESP **LE Audio** = *preview/experimental*, two just-launched chips. ESP
**Classic A2DP** = *production-grade*, original ESP32, today. For anything you want to actually
ship or rely on, A2DP-on-ESP32 is the answer; LE Audio-on-ESP is a 2026-onward bet.

> Sources: [ESP32-A2DP library](https://github.com/pschatzmann/ESP32-A2DP) ·
> [ESP-IDF A2DP API](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/bluetooth/esp_a2dp.html) ·
> [esp-ble-audio-lib](https://github.com/espressif/esp-ble-audio-lib) ·
> [ESP-ADF LC3 support](https://components.espressif.com/components/espressif/esp_audio_codec/versions/2.4.1/readme)

---

## 5. Consumer hardware reality (2026): are LE Audio / Auracast earbuds even common?

Getting there, but **still early-adopter** in mid-2026. Per the Bluetooth SIG, **"2025 is the year
Auracast infrastructure became a reality,"** yet **"adoption remains in early stages."** Projections
put **~3.1 billion LE-Audio devices/year and 1.5 million Auracast venues by *2029***.

**Earbuds/headphones with LE Audio + Auracast (2025–26):** Samsung **Galaxy Buds3 / Buds3 Pro**
(built in); **Galaxy Buds2 Pro** (via firmware update); Google **Pixel Buds Pro 2**; plus a growing
list from JBL/Sony etc. Still a **minority of earbuds in the wild** — the installed base is dominated
by AirPods and older Classic-Bluetooth buds that **do not** speak LE Audio.

**Phones/sources:** LE Audio + Auracast needs **Android 13+** and capable silicon — Samsung Galaxy
S23/S24/S25, Google Pixel (Pixel "Audio Sharing" with Auracast rolled out **Sept 2025**). **Apple
does not support Auracast** as of 2026. Interop across brands is **improving but still fragmented**.

**Real public deployments (2025–26):** Sydney Opera House; Stadium Taranaki (NZ, 21k seats,
multi-language + assistive listening); Singapore LTA MRT pilot; Frankfurt & Amsterdam Schiphol
airport feasibility studies. Real, but pilots — not yet everywhere.

**Bottom line for a hobby device:** if the goal is "reach earbuds people actually own **in 2026**,"
**Classic Bluetooth (A2DP) reaches vastly more devices than LE Audio.** LE Audio's ubiquity is a
*2027–2029* story.

> Sources: [SIG — Auracast market 2026 & beyond](https://www.bluetooth.com/blog/how-auracast-broadcast-audio-is-expanding-audio-streaming-and-a-look-at-the-market-impact-it-could-have-in-2026-and-beyond/) ·
> [SIG — venues deploying Auracast](https://www.bluetooth.com/blog/venues-worldwide-are-already-deploying-auracast-systems/) ·
> [Samsung — Auracast on Galaxy Buds3](https://www.samsung.com/us/support/answer/ANS10003615/) ·
> [Google — Auracast expands to more Android](https://blog.google/products/android/le-audio-auracast-support/) ·
> [9to5Google — Pixel Audio Sharing/Auracast (Sept 2025)](https://9to5google.com/2025/09/03/auracast-le-audio-sharing-coming-to-pixel/)

---

## 6. Auracast angle — could an ESP32 be a *broadcaster*?

**Concept:** Auracast broadcast (BIS/BIG) is one-way, connectionless — arguably simpler than unicast
because there's no per-receiver connection handshake, just periodic advertising + a broadcast ISO
stream. So an ESP32 *broadcaster* is the more plausible LE Audio role.

**Reality on ESP32:**
- **Mainstream chips (C3/C6/H2/C5/S3): no.** Broadcast still needs **BIS**, which is an ISO channel —
  and ISO is unsupported on those controllers (§2). No BIS ⇒ no Auracast source.
- **H4 / S31: yes, in principle.** These expose **ESP-BLE-ISO**, and its documented example is a
  **`big_broadcaster`** that creates a BIG and transmits over BIS — i.e. exactly an Auracast-style
  broadcast source. But it's **preview** software on **just-launched** silicon.
- **Concrete ESP Auracast broadcaster projects:** essentially none mature. Espressif has an internal
  **[ESP-ADF "BLE Auracast" tracking issue (#1544)](https://github.com/espressif/esp-adf/issues/1544)**;
  community DIY Auracast broadcasters overwhelmingly use **Nordic** parts, not ESP32.
- **Zephyr on C6 doesn't rescue it:** Zephyr's BAP Broadcast Source sample is great, but again the
  C6 controller has no ISO to run it over.

**vs. Nordic (the real Auracast dev platform):** For LE Audio / Auracast today, **Nordic nRF5340 /
nRF54 + nRF Connect SDK (Zephyr)** is far ahead — mature ISO controller, reference Auracast
broadcaster/receiver apps, dev kits in hand. ESP32 is **well behind** Nordic here and only just
entering the game with the H4/S31.

**Bottom line:** ESP32 Auracast broadcast is **theoretically possible only on the brand-new H4/S31
and is preview-stage**; on everything you can buy in volume it's **not possible**. If Auracast
broadcasting is the actual goal, **use Nordic**, not ESP32.

> Sources: [ESP-BLE-ISO `big_broadcaster`](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s31/api-reference/bluetooth/esp-ble-iso.html) ·
> [ESP-ADF BLE Auracast issue #1544](https://github.com/espressif/esp-adf/issues/1544) ·
> [Zephyr — Broadcast Audio Source sample](https://dev.to/denladeside/a-simple-broadcast-audio-source-4e8b)

---

## Final verdict

**(a) Can the smol C3 do LE Audio?**
**No — categorically.** Bluetooth 5.0-class radio with **no Isochronous Channels**, **no** Espressif
LE Audio stack, and (bonus) **no Bluetooth Classic** for an A2DP fallback either. Both wireless-audio
doors are shut on the C3.

**(b) If you want ESP + LE Audio, the recommended chip + stack *today*:**
**ESP32-H4** (or ESP32-S31) with Espressif's **`esp-ble-audio-lib` + ESP-BLE-ISO/ESP-BLE-AUDIO**
(Zephyr-derived, binary, **preview**). It's the *only* ESP LE Audio path — but it's brand-new
silicon (H4 announced Apr 2024) with preview-grade software and thin retail availability in mid-2026.
Treat as experimental. If Auracast *broadcasting* is the goal and you want something that works now,
use **Nordic (nRF5340/nRF54)** instead.

**(c) Is it worth it vs. the alternatives?**
For smol, **no.** Ranked for a C3-class handheld:
1. **Wired I²S** (e.g. MAX98357A / PCM5102) — trivial, high quality, zero radio drama, works on the
   C3 right now. **Best default for on-device sound.**
2. **ESP-NOW voice** ([walkie-talkie.md](walkie-talkie.md)) — if the goal is device-to-device
   comms rather than commercial earbuds; already scoped for the C3.
3. **Classic A2DP on the *original* ESP32** — the mature "wireless headphones" route (music-out),
   if you specifically want Bluetooth earbuds and can swap the C3 for an ESP32. Reaches far more
   real-world earbuds than LE Audio does in 2026.
4. **LE Audio on ESP32-H4/S31** — only if LE Audio is the point of the exercise; accept experimental
   status, new-chip supply, and preview software.

**Chasing LE Audio on smol means abandoning the C3 for a hard-to-get new chip to use preview
software, in order to reach a minority of 2026 earbuds. Not worth it.** Wire up I²S, or if you must
go wireless-to-headphones, keep it in your pocket for a future H4/S31 build (or drop to Classic A2DP
on an original ESP32).

---

## All sources

**Standard / fundamentals**
- https://www.bluetooth.com/learn-about-bluetooth/feature-enhancements/le-audio/le-audio-specifications/
- https://www.bluetooth.com/blog/10-frequently-asked-questions-on-le-isochronous-channels/
- https://www.bluetooth.com/wp-content/uploads/2024/05/2505_Paper_An-Overview-of-Auracast.pdf
- https://www.bluetooth.com/wp-content/uploads/2020/01/Bluetooth_5.2_Feature_Overview.pdf
- https://en.wikipedia.org/wiki/LC3_(codec)
- https://en.wikipedia.org/wiki/Bluetooth_Low_Energy
- https://www.rfwireless-world.com/terminology/difference-between-lc3-and-sbc-in-ble
- https://www.soundguys.com/bluetooth-le-audio-lc3-explained-28192/
- https://www.freecodecamp.org/news/the-bluetooth-le-audio-handbook/
- https://novelbits.io/bluetooth-le-audio-auracast-profiles/
- https://docs.zephyrproject.org/latest/services/connectivity/bluetooth/api/audio/bluetooth-le-audio-arch.html
- https://audioxpress.com/news/bluetooth-sig-announces-completion-of-le-audio-specifications

**ESP32 chip support (radio + stack)**
- https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-guides/ble/ble-feature-support-status.html  *(the definitive "ISO unsupported" matrix)*
- https://github.com/espressif/esp-idf/issues/12277  *(C6 LE Audio → "Won't Do")*
- https://www.espressif.com/en/news/ESP32-H4
- https://www.espressif.com/en/products/socs/esp32-h4
- https://www.espressif.com/en/products/socs/esp32-s31
- https://github.com/espressif/esp-ble-audio-lib
- https://docs.espressif.com/projects/esp-idf/en/latest/esp32s31/api-reference/bluetooth/esp-ble-iso.html
- https://documentation.espressif.com/esp32-c6_datasheet_en.html
- https://www.espressif.com/en/products/socs/esp32-c6
- https://developer.espressif.com/blog/wireless-connectivity-solutions-for-esp32-p4/
- https://hubble.com/community/guides/zephyr-rtos-on-esp32-c6-what-s-supported-and-what-s-still-missing/

**Espressif LC3 / audio stack**
- https://components.espressif.com/components/espressif/esp_audio_codec/versions/2.4.1/readme
- https://github.com/espressif/esp-adf
- https://github.com/espressif/esp-adf/issues/1544  *(BLE Auracast tracking)*

**Classic A2DP alternative**
- https://github.com/pschatzmann/ESP32-A2DP
- https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/bluetooth/esp_a2dp.html

**Consumer / Auracast reality (2025–26)**
- https://www.bluetooth.com/blog/how-auracast-broadcast-audio-is-expanding-audio-streaming-and-a-look-at-the-market-impact-it-could-have-in-2026-and-beyond/
- https://www.bluetooth.com/blog/venues-worldwide-are-already-deploying-auracast-systems/
- https://www.samsung.com/us/support/answer/ANS10003615/
- https://blog.google/products/android/le-audio-auracast-support/
- https://9to5google.com/2025/09/03/auracast-le-audio-sharing-coming-to-pixel/
