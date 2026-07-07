# Voice walkie-talkie over ESP-NOW on the ESP32-C3

> Compiled by the **RELAY** agent, 2026-07-07. Target hardware: ESP32-C3 SuperMini
> (single-core RISC-V @160 MHz, ~400 KB SRAM, **no PSRAM**, **no DAC**, one I²S
> peripheral, single 2.4 GHz radio) + 0.42" SSD1306 OLED on I²C (SDA=GPIO5 / SCL=GPIO6).

## TL;DR

- **Yes, voice-over-ESP-NOW is a real, existing thing** — and one 2026 library
  (**PCMFlowG722**) even lists the **ESP32-C3 as a supported target**. But it's
  comparatively *uncommon*: the dominant DIY walkie-talkie codebase (atomic14)
  offers ESP-NOW as an *option* alongside WiFi/UDP, and the most polished
  **full-duplex** intercoms drop ESP-NOW for WiFi/UDP. Naive raw-PCM-over-ESP-NOW
  is widely reported to sound rough; the serious projects compress (G.722 / ADPCM).
- **A bare C3 can do it — half-duplex, push-to-talk, at ~8–16 kHz voice with a light
  codec.** The C3's single I²S peripheral supports **full-duplex Standard mode**, so
  an INMP441 mic (in) and MAX98357A amp (out) **share one BCLK + one WS** and need
  only **two separate data pins** — 4 GPIOs total. No DAC means I²S out is mandatory
  (the MAX98357A has its own DAC), which you were right about.
- **Codec: IMA ADPCM (~32 kbps) or G.722 (64 kbps) — not raw PCM, not Opus.** ADPCM
  is ~2.4% CPU / 0.1 KB RAM; Opus *encode* would saturate the single 160 MHz core and
  effectively needs PSRAM the C3 doesn't have.
- **A walkie-talkie streams compressed AUDIO — it does NOT use speech-to-text or
  text-to-speech.** (More below; local STT/TTS on a C3 is infeasible anyway.)
- **Difficulty: 3/5 on a bare C3 half-duplex; 4/5 if you chase full-duplex.** An
  **ESP32-S3** (dual-core, PSRAM, 2× I²S) is the materially easier path and is what
  most of the good projects use — but the C3 is genuinely capable for PTT voice.

---

## 1. Existing projects (ranked by relevance to "voice over ESP-NOW on a small ESP32")

**Top-level finding:** true real-time voice over ESP-NOW *exists but is uncommon*.
Many "ESP-NOW walkie-talkie" search hits are (a) the same atomic14 code or forks,
(b) record-then-send (not streaming), or (c) actually WiFi/UDP. The strongest
*purpose-built* ESP-NOW voice work is very recent (2026). Quality is genuinely
constrained by ESP-NOW's 250-byte v1.0 payload, which is exactly why the good
projects use a codec.

### Tier 1 — Purpose-built voice-over-ESP-NOW with real codec engineering

**1. PCMFlowG722 — G.722 HD voice over ESP-NOW** ⭐ *the standout for a C3*
- Repo: https://github.com/tanakamasayuki/PCMFlowG722 · coverage:
  [CNX-Software](https://www.cnx-software.com/2026/05/30/pcmflow722-library-enables-two-way-real-time-hd-voice-over-esp-now-with-g-722-audio-codec/) ·
  [Adafruit blog](https://blog.adafruit.com/2026/05/27/a-voice-codec-for-pcmflow-allowing-two-way-voice-over-esp-now/)
- **Chip:** explicitly **ESP32 / ESP32-S3 / ESP32-C3 / ESP32-C6 / ESP32-P4**. This is
  the *only* project found that names the plain **ESP32-C3** as a target. (Demoed on
  an M5Stack Core2 / original ESP32.)
- Mic/speaker on demo: SPM4123 mic, 1 W speaker.
- **Audio:** 16 kHz 16-bit PCM in → **G.722 wideband at 64 kbps**, frame = **160 bytes
  per 20 ms** → fits one ESP-NOW packet cleanly (160 B < 250 B). ~7 kHz "HD voice",
  better than G.711 telephony.
- **Transport: ESP-NOW** (`examples/EspNowTransceiver/`). **Duplex: half-duplex PTT**
  (hold button to broadcast to one or more peers). Latency not published, but 20 ms
  frames imply low codec latency.

**2. WillyBilly06/esp-now-audio-source (+ -sink) — IMA-ADPCM streamer**
- Repo: https://github.com/WillyBilly06/esp-now-audio-source (pairs with an
  `esp-now-audio-sink` receiver)
- **Chip:** plain **ESP32** (sdkconfig explicitly *not* S3). ESP-IDF, not Arduino.
- Mic/ADC: PCM1808 I²S ADC. Output DAC: PCM5102A. **Audio:** 48 kHz stereo,
  **IMA ADPCM** (~4:1), ~115-byte packets (19 B header + 96 B payload ≈ 2 ms), with
  per-channel ADPCM predictor state + µs timestamp in each packet.
- **Transport: ESP-NOW broadcast.** Sink does **adaptive buffering + packet-loss
  concealment + clock-offset tracking**. **Duplex: one-way hi-fi streaming (music),
  NOT a two-way walkie-talkie.** Technically the most sophisticated ESP-NOW audio
  pipeline found — great reference for the RX-side buffering, wrong shape for comms.

### Tier 2 — Canonical DIY walkie-talkie codebase (ESP-NOW as an option) + forks

**3. atomic14/esp32-walkie-talkie (Chris Greening) — THE reference project**
- Repo: https://github.com/atomic14/esp32-walkie-talkie · video/writeup:
  https://www.atomic14.com/videos/posts/d_h38X4_eQQ ·
  [Hackster](https://www.hackster.io/chris-greening/esp32-walkie-talkie-f8d0dd)
- **Chip:** generic **ESP32** (author used a TinyPICO; "any generic ESP32 dev board").
  Not C3-specific, but the code is chip-agnostic I²S + the transport is `#define`-selectable.
- Mic: **ICS-43434 or INMP441** (I²S MEMS). Speaker: **I²S 3 W amp (MAX98357A-class)
  → 4 Ω speaker**. **Audio: 16 kHz, 8-bit, raw PCM (no codec).**
- **Transport: selectable UDP broadcast OR ESP-NOW** (`config.h`). ESP-NOW mode uses
  **250-byte packets → ~64 packets/sec** (128 samples/packet); UDP mode uses ~1436-byte
  packets. Works with no WiFi network in ESP-NOW mode.
- **Latency/quality:** UDP mode ≈ **~0.5 s delay** (jitter buffer); author calls it
  "a fairly low-quality Walkie-Talkie… sufficient for a hobby project."
  **Duplex: half-duplex PTT**; full-duplex + echo cancellation listed as future work.
  Nearly every other tutorial/fork derives from this.

**4. RASPIAUDIO/esp32-walkie-talkie — atomic14 fork, all-in-one board**
- Repo: https://github.com/RASPIAUDIO/esp32-walkie-talkie
- **Chip/board:** Raspiaudio ESP MUSE PROTO (**ESP32**) with **built-in mic + speaker**
  (no external parts), optional battery. **Transport: defaults to ESP-NOW**, ~50 m range.
  **Duplex: PTT via IO0.** Audio inherits atomic14 (16 kHz). Value = turnkey hardware.

**5. jeffskinnerbox/esp32-walkie-talkie — three-transport variant (design-stage)**
- Repo: https://github.com/jeffskinnerbox/esp32-walkie-talkie
- **Chip:** **ESP32-S2 / S3** (leans S3 for DSP; notes S3 has no DAC → PDM workaround).
  **CircuitPython.** Planned **three modes: UDP, ESP-NOW, and Mumble (internet bridge)**.
  More design doc than finished, but the one project explicitly planning ESP-NOW +
  internet bridging.

### Tier 3 — Analog / built-in ADC-DAC ESP-NOW walkie-talkies (cheapest, low-fi)

**6. Elektor / Clemens Valens "DIY Walkie-Talkie based on ESP-NOW"**
- [Elektor article](https://www.elektormagazine.com/articles/diy-walkie-talkie-based-on-esp-now) ·
  [Hackaday coverage](https://hackaday.com/2023/12/08/diy-walkie-talkie-with-esp32-and-esp-now/)
- **Chip:** ESP32-PICO-KIT. Mic: 1-transistor condenser preamp. Speaker: LM386. Uses
  the **original ESP32's built-in 12-bit ADC + 8-bit DAC** ("just enough for voice").
- **Audio: 8 kHz, 8-bit, ~3.5 kHz telephony bandwidth, ~65 kbit/s, raw.** ESP-NOW
  250-byte packets, out-of-order tolerant. **Duplex: half-duplex** (mutes own output
  while PTT held to avoid feedback; "roger/over" discipline). Cheapest ESP-NOW voice
  approach — **but this path is impossible on the C3, which has no DAC.**

**7. MicroPython ESP-NOW walkie-talkie (community thread)**
- https://github.com/orgs/micropython/discussions/14309 — ESP32, **8 kHz 16-bit mono
  raw PCM**, uses `espnow.MAX_DATA_LEN` (250 B) and **crudely drops data over the limit**.
  Author reports **"sound quality issues"** — a good real-world data point that naive
  raw-PCM-over-ESP-NOW sounds rough.

**8. Durh66/Espnow-walkie-talkie — record-then-send (NOT streaming)**
- https://github.com/Durh66/Espnow-walkie-talkie — ESP32 + **ISD1820 voice-recorder
  module** + LM386. Records ≤10 s then plays/transmits; effectively sends control
  signals, not a real-time stream. Listed for completeness.

### Tier 4 — Adjacent context (not ESP-NOW, or not voice-comms)

- **pschatzmann/arduino-audio-tools — ESP-NOW audio discussion** (best feasibility
  reference): https://github.com/pschatzmann/arduino-audio-tools/discussions/393 —
  measures ESP-NOW at **~33k–37k bytes/s** by default (enough for 8 kHz 16-bit raw);
  up to ~48 kHz with a raised PHY rate but then "loud crackling/popping" on loss.
  Concludes a **codec (SBC) is the reliable path** for live audio. Explains *why*
  purpose-built projects use ADPCM/G.722.
- **sh123/esp32_loradv** — https://github.com/sh123/esp32_loradv — **NOT ESP-NOW.**
  ESP32 handheld, INMP441 + MAX98357A, **Codec2 (700–3200 bps) / Opus**, transport is
  **LoRa** PTT. The LoRa cousin — relevant only if the goal shifts to long range.
- **ESPHome full-duplex intercoms** (fallingaway24, AngeloDL90, samuelthng) — all
  **ESP32-S3 over WiFi/UDP + WebRTC/go2rtc**, bidirectional, Home Assistant. The
  projects to study for *full-duplex* — but they **abandon ESP-NOW for WiFi**. This is
  the clearest evidence that full-duplex ESP32 audio tends to mean WiFi, not ESP-NOW.
- **RevSpace EspNowAudio** — https://revspace.nl/EspNowAudio — 2020 one-way
  ESP8266→ESP32 ESP-NOW proof-of-concept (INMP441), pipeline never finished.

**Which run on a plain ESP32-C3?** Only **PCMFlowG722** explicitly names the C3. The
atomic14 code is chip-agnostic and *should* port to a C3 (I²S + `#define`-selected
ESP-NOW), but the author didn't test it on one. Everything else targets the original
dual-core ESP32 or the S3, is one-way, or uses WiFi/UDP.

---

## 2. C3 feasibility verdict

### Can one C3 core handle I²S-in + ESP-NOW + I²S-out for voice? — Yes, for PTT.

- **I²S is offloaded to DMA (GDMA).** Audio in/out moves without per-sample CPU work,
  which is what makes concurrent I²S + radio realistic on a single 160 MHz core.
- **A light codec leaves the core relaxed.** IMA ADPCM is ~**2.4% CPU / 0.11 KB RAM**
  (Espressif's own codec benchmark) — table lookups, no heavy math. G.722 is similarly
  modest. The single core then mostly juggles the WiFi/ESP-NOW task + a small
  encode/decode + DMA servicing.
- **The #1 practical gotcha: you must pace ESP-NOW sends.** `esp_now_send()` is async
  with a completion callback; sending in a tight loop overruns the internal buffers and
  returns **`ESP_ERR_ESPNOW_NO_MEM`**. Espressif's guidance: send the next packet only
  after the previous send callback returns, and do **no** heavy work in that callback
  (post to a queue). A low-bitrate stream is only ~16–50 packets/sec, so this is easily
  paced — but it must be designed in.

### Realistic sample rate / codec

Real-world ESP-NOW throughput is **~214 kbps in the open, ~555 kbps shielded** (default
1 Mbps PHY). Against that budget:

| Format | Raw bitrate | Fits ESP-NOW? | Packet rate @250 B |
|---|---|---|---|
| 8 kHz, 8-bit mono PCM | 64 kbps | ✅ comfortably | ~32/s |
| 8 kHz, 16-bit mono PCM | 128 kbps | ⚠️ fits but eats most open-air budget | ~64/s (≈15.6 ms) — near the pacing ceiling |
| 16 kHz, 16-bit mono PCM | 256 kbps | ❌ exceeds ~214 kbps open-air | ~128/s |
| **µ-law / G.711 (8 kHz)** | 64 kbps | ✅ comfortably | ~32/s |
| **G.722 (16 kHz wideband)** | 64 kbps | ✅ (160 B / 20 ms) | ~50/s |
| **IMA ADPCM (8 kHz/16-bit, 4:1)** | **~32 kbps** | ✅ **very comfortably** | **~16/s** |

**Recommended: 8–16 kHz mono with IMA ADPCM (~32 kbps) or G.722 (64 kbps).** ADPCM's
low packet rate (~16/s) is a *reliability* win, not just bandwidth — far easier to pace
against the send callback and far more tolerant of the 250-byte payload than raw
16 kHz PCM (which the community reports as crackly). Raw 8 kHz/8-bit works too (the
cheap path) if you want to skip a codec entirely.

### Half-duplex PTT vs full-duplex

- **Half-duplex push-to-talk is the right target** and is what PCMFlowG722, atomic14,
  RASPIAUDIO and the Elektor build all do. Hold a button → capture + encode + send;
  release → receive + decode + play.
- **Full-duplex is possible but hard on a bare C3.** The *I²S* supports full-duplex
  (see below), but the **single radio is fundamentally half-duplex** — it can't
  transmit and receive at the same instant — and doing simultaneous capture-encode-send
  **and** receive-decode-play on one core, plus **acoustic echo cancellation** (or the
  speaker feeds straight back into the mic), is a real DSP burden. This is exactly why
  the polished full-duplex intercoms use WiFi *and* a dual-core S3. On a C3, treat
  full-duplex as a stretch goal, not the baseline.

### The single-radio / single-channel constraint

- **Both peers must be on the same WiFi channel.** `esp_now_send()` returns
  `ESP_ERR_ESPNOW_CHAN` if the current channel doesn't match the peer's. The **C3 has
  one radio**, so it can't be on WiFi (one channel) and ESP-NOW (another) at once — if
  also joined to a router, ESP-NOW is forced onto the router's channel and coexistence
  is time-shared.
- **For a router-less point-to-point walkie-talkie this is a non-issue:** fix both ends
  to the same channel and never touch WiFi. Only if you later want an internet bridge
  (à la jeffskinnerbox's Mumble mode) does the single-channel limit start to bite.

### Honest comparison: bare C3 vs dual-core ESP32 / S3

| Factor | Bare ESP32-C3 | ESP32-S3 (or classic ESP32) |
|---|---|---|
| CPU | 1 core @160 MHz | 2 cores @240 MHz — pin radio to one, audio+codec to the other |
| I²S controllers | **1** (full-duplex OK, shared clocks) | **2** (independent mic & speaker clocking) |
| PSRAM | none | common (8 MB) — unlocks Opus, big jitter buffers |
| DAC | **none** (I²S amp mandatory) | classic ESP32 has DAC (analog path possible); S3 also none |
| Codec ceiling | ADPCM / G.722 (Opus impractical) | Opus / Codec2 feasible on S3+PSRAM |
| Full-duplex | stretch goal | practical (the ESPHome intercoms prove it) |
| Verdict | **fine for half-duplex PTT voice** | **the easier, more headroom-y platform** |

The C3 is genuinely capable of a half-duplex PTT walkie-talkie. But if full-duplex,
higher quality, or an internet bridge matter, the S3 removes almost every constraint at
roughly the same price. **Bare C3 = a satisfying, real project with clear limits;
S3 = the comfortable path.**

---

## 3. Clarifying the STT/TTS misconception

A walkie-talkie **streams raw or compressed AUDIO samples** (PCM → optionally ADPCM /
G.722 → ESP-NOW packets → decode → I²S → speaker). It does **not** convert speech to
text or text back to speech — there is no language step anywhere in the pipeline, just
digitized sound moving from one mic to another speaker. (And to be explicit: **local
speech-to-text / text-to-speech on a bare C3 is infeasible** — those models need far
more CPU/RAM/PSRAM than a single-core, no-PSRAM C3 has; but the walkie-talkie doesn't
need them at all.)

---

## 4. Parts list + wiring for a C3 PTT walkie-talkie prototype

**Key hardware fact (verified):** the ESP32-C3 has **one I²S peripheral**, and it
supports **full-duplex Standard mode** where "the channels share the BCLK and WS
signals." So the INMP441 (I²S RX) and MAX98357A (I²S TX) **share one BCLK and one WS**;
only the data pins differ. Total **4 GPIOs** for the whole audio subsystem + 1 for the
PTT button. The C3 has **no fixed I²S pins** — assign any GPIO via `I2S.setPins()` /
IDF pin config.

### Bill of materials

| Item | Qty | Ballpark (USD) | Notes |
|---|---|---|---|
| INMP441 I²S MEMS mic module | 1 | $1.20–2.00 | tie **L/R → GND** = left channel |
| MAX98357A I²S class-D amp module | 1 | $1.90–3.00 (genuine Adafruit $5.95) | mono, has its **own DAC**, ~3 W into 4 Ω |
| Small speaker, 3 W, 4–8 Ω | 1 | $1–4 | 4 Ω is louder; enclosed sounds better |
| Momentary tactile push button (PTT) | 1 | $0.10–0.30 | wired to GND, `INPUT_PULLUP` |
| (have already) ESP32-C3 SuperMini | 1 | — | powers both modules from 3V3 |

**Add-on BOM total: ~$5–10** with generic parts, **~$15–18** with genuine
Adafruit-grade parts and a nice speaker.

### Suggested GPIO mapping (avoids I²C 5/6, strapping 2/8/9, UART 20/21)

| Signal | C3 GPIO | Wires to |
|---|---|---|
| **I²S BCLK** (shared) | **GPIO4** | INMP441 `SCK` **+** MAX98357A `BCLK` |
| **I²S WS/LRCK** (shared) | **GPIO3** | INMP441 `WS` **+** MAX98357A `LRC` |
| **I²S DIN** (mic → C3, RX) | **GPIO10** | INMP441 `SD` |
| **I²S DOUT** (C3 → amp, TX) | **GPIO7** | MAX98357A `DIN` |
| **PTT button** | **GPIO1** (or GPIO0) | button → GND, `INPUT_PULLUP` |

Leaves GPIO0 spare (ADC-capable), keeps GPIO5/6 for the OLED, never touches strapping
pins (2/8/9), the onboard LED (8), BOOT (9), or the UART console (20/21). GPIO3 is
also an ADC pin but that's irrelevant here; GPIO10 is the same safe pin the sound
doc/NES doc favor.

### Wiring notes / gotchas

```
  ESP32-C3 SuperMini            INMP441 (mic, I2S RX)        MAX98357A (amp, I2S TX)
  ┌───────────────┐            ┌──────────────┐             ┌──────────────┐
  │  3V3  ●────────┼───┬────────● VDD          │        ┌────● VIN (2.5-5.5V)│
  │  GND  ●────────┼───┼───┬────● GND          │      ┌─┼────● GND           │
  │ GPIO4 ●────────┼───┴───┼────● SCK (BCLK) ──┼──────┼─┼────● BCLK          │
  │ GPIO3 ●────────┼───────┴────● WS  (LRCK) ──┼──────┼─┴────● LRC           │
  │ GPIO10●◀───────┼────────────● SD  (DOUT)   │      │      │ DIN ●◀── GPIO7│
  │ GPIO7 ●────────┼────────────────────────────────────────● DIN           │
  │ GPIO1 ●──[btn]─┼── GND      │ L/R ●── GND  │      │      │ GAIN (float=9dB)│
  └───────────────┘            └──────────────┘      │      │ SD (see note)  │
                                                      │      │  +  ●──┐
                                                      │      │  -  ●──┴─ speaker
```

- **INMP441 32-bit gotcha (very common):** it outputs 24-bit data inside **32-bit I²S
  frames**. Configure the RX slot as **32 bits** and shift/mask in software — reading
  it as 16-bit gives zeros/garbage. Tie **L/R → GND** and read the **left** slot.
- **MAX98357A `GAIN`:** leave **floating for 9 dB** (default). Options: 15 dB (100 kΩ to
  GND), 12 dB (GND), 6 dB (VIN), 3 dB (100 kΩ to VIN).
- **MAX98357A `SD` is shutdown *and* channel-select** (not serial data). Board has a
  1 MΩ pull-up to VIN. <0.16 V = amp off (drive low from a GPIO to mute); 0.16–0.77 V =
  (L+R)/2; 0.77–1.4 V = right; >1.4 V = left. For a mono voice stream leave it at the
  board default, or add a resistor to force (L+R)/2, or route it to a spare GPIO for
  software mute.
- **Shared clocks mean shared sample rate/width** for mic and speaker — fine for
  16 kHz-in / 16 kHz-out voice; you can't run them at different rates on the C3's one
  I²S. Power both from **3V3** (run the amp from 5V/VBUS for more volume — its 3.3 V
  logic still works); **all grounds common**; keep I²S runs short (< ~20 cm) since I²S
  has no error checking.

### Firmware sketch (concept)

1. Configure the C3's single I²S in **full-duplex Standard mode**, 16 kHz, shared
   BCLK/WS on GPIO4/GPIO3, DIN=GPIO10, DOUT=GPIO7.
2. `esp_now_init()`, add the peer, **lock both ends to the same channel**, register a
   send callback that just frees an "in-flight" flag.
3. **PTT held (GPIO1 low):** read I²S RX frames → downshift 32→16-bit → encode IMA
   ADPCM → send 160–250 B packets, **one at a time, gated on the send callback**.
4. **PTT released:** on ESP-NOW receive, push into a small jitter buffer → decode
   ADPCM → write I²S TX to the amp.
5. Reference **PCMFlowG722**'s `EspNowTransceiver` example (it targets the C3) and
   **atomic14**'s ring-buffer/PTT structure. Skip full-duplex + echo cancellation for v1.

---

## Difficulty & verdict

- **Difficulty: 3/5** for a **bare-C3 half-duplex PTT** walkie-talkie (ADPCM or G.722,
  8–16 kHz). The pieces are all proven; the real work is I²S full-duplex config, the
  ADPCM codec, and disciplined ESP-NOW send pacing (queue + in-flight flag). Bumps to
  **4/5** if you insist on full-duplex or Opus-grade quality on the C3.
- **Verdict:** **Worth attempting on the bare C3 for a half-duplex PTT link** — it's a
  real, satisfying build with a C3-supported library (PCMFlowG722) to lean on and a
  clean 4-GPIO wiring that dodges every reserved pin. **But if you want full-duplex,
  higher fidelity, or an internet bridge, add an ESP32-S3** (2× I²S, dual core, PSRAM):
  it erases nearly every constraint here at roughly the same cost, and it's what the
  best projects actually use.

---

### Sources

- [PCMFlowG722 (G.722 voice over ESP-NOW; lists ESP32-C3)](https://github.com/tanakamasayuki/PCMFlowG722) · [CNX-Software writeup](https://www.cnx-software.com/2026/05/30/pcmflow722-library-enables-two-way-real-time-hd-voice-over-esp-now-with-g-722-audio-codec/) · [Adafruit blog](https://blog.adafruit.com/2026/05/27/a-voice-codec-for-pcmflow-allowing-two-way-voice-over-esp-now/)
- [WillyBilly06/esp-now-audio-source (IMA-ADPCM over ESP-NOW)](https://github.com/WillyBilly06/esp-now-audio-source)
- [atomic14/esp32-walkie-talkie (UDP or ESP-NOW; 16 kHz 8-bit)](https://github.com/atomic14/esp32-walkie-talkie) · [writeup](https://www.atomic14.com/videos/posts/d_h38X4_eQQ) · [Hackster](https://www.hackster.io/chris-greening/esp32-walkie-talkie-f8d0dd)
- [RASPIAUDIO/esp32-walkie-talkie (ESP-NOW default, all-in-one board)](https://github.com/RASPIAUDIO/esp32-walkie-talkie)
- [jeffskinnerbox/esp32-walkie-talkie (UDP + ESP-NOW + Mumble; S2/S3)](https://github.com/jeffskinnerbox/esp32-walkie-talkie)
- [Elektor: DIY Walkie-Talkie based on ESP-NOW (8 kHz, internal ADC/DAC)](https://www.elektormagazine.com/articles/diy-walkie-talkie-based-on-esp-now) · [Hackaday coverage](https://hackaday.com/2023/12/08/diy-walkie-talkie-with-esp32-and-esp-now/)
- [MicroPython ESP-NOW walkie-talkie thread (raw PCM quality issues)](https://github.com/orgs/micropython/discussions/14309)
- [Durh66/Espnow-walkie-talkie (record-then-send)](https://github.com/Durh66/Espnow-walkie-talkie)
- [pschatzmann/arduino-audio-tools — ESP-NOW audio data-rate discussion](https://github.com/pschatzmann/arduino-audio-tools/discussions/393)
- [sh123/esp32_loradv (LoRa cousin: Codec2/Opus, INMP441+MAX98357A)](https://github.com/sh123/esp32_loradv)
- [ESP-NOW API reference — payload limits, async send, NO_MEM, channel constraint](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/network/esp_now.html)
- [Espressif ESP-FAQ: ESP-NOW throughput ~214 kbps open / ~555 kbps shielded](https://docs.espressif.com/projects/esp-faq/en/latest/application-solution/esp-now.html)
- [Espressif dev blog: ESP-NOW outdoor throughput vs range](https://developer.espressif.com/blog/esp-now-for-outdoor-applications/)
- [ESP-IDF I²S (ESP32-C3): one peripheral, full-duplex shares BCLK+WS](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/peripherals/i2s.html)
- [ESP32-C3 datasheet: no DAC, one I²S, single-core RISC-V @160 MHz](https://documentation.espressif.com/esp32-c3_datasheet_en.html)
- [Espressif esp_audio_codec: IMA-ADPCM ~2.43% CPU / 0.11 KB](https://components.espressif.com/components/espressif/esp_audio_codec)
- [XasWorks/esp-libopus (Opus encode CPU/RAM: PSRAM effectively required)](https://github.com/XasWorks/esp-libopus)
- [Adafruit MAX98357A pinout (GAIN + SD thresholds)](https://learn.adafruit.com/adafruit-max98357-i2s-class-d-mono-amp/pinouts)
- [DroneBot Workshop: ESP32 I²S (INMP441 + MAX98357A, L/R=GND→left)](https://dronebotworkshop.com/esp32-i2s/)
- [atomic14: ESP32 audio input (INMP441 WS-phase, 32-bit frames)](https://www.atomic14.com/2020/09/12/esp32-audio-input)
- [MyEmbeddedSystems: MAX98357A + INMP441 sharing one I²S bus](https://www.myembeddedsystems.com/how-to-tutorials/how-to-use-esp32-for-i2s-audio-playback-and-recording-with-max98357a-and-inmp441/)
- [ESP32-C3 SuperMini pinout (safe pins / strapping / UART / I²C / ADC)](https://lastminuteengineers.com/esp32-c3-super-mini-pinout-reference/)
