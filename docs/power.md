# Powering smol from a 502030 LiPo with USB-C recharging

> Compiled by the **POWER** agent, 2026-07-07.
> Target hardware: ESP32-C3 SuperMini + 0.42" SSD1306 OLED, single 502030 LiPo (3.7 V nominal, ~250 mAh).

## TL;DR

- **The standard SuperMini has no onboard LiPo charging and no battery connector.** You add charging with a **TP4056/TP4057 USB-C charge+protection board**. (Some *variants* — "SuperMini Plus/v2", the official expansion board, or a different board like the DFRobot Beetle C3 — *do* have charging; verify what you actually have. See [board variance](#board-variance-read-this-first).)
- **Feed the protected cell into the SuperMini's `5V`/`VBUS` pin and let the onboard ME6211 LDO make 3.3 V.** This is the simplest safe path and works for smol's low current. You do **not** strictly need a boost/buck-boost.
- **Never wire a raw 4.2 V cell to the `3V3` pin.** `3V3` is the LDO *output* and connects straight to the ESP32-C3, whose absolute max is **3.6 V**. 4.2 V there **destroys the chip**.
- **Two real gotchas:** (a) don't back-power `5V` while USB is plugged into the SuperMini — you'd fight the USB 5 V rail; add a **Schottky diode** or a switch. (b) The **default TP4056 charges at 1 A, which is 4C for a 250 mAh cell — unsafe.** Swap the program resistor to charge at ~125 mA (0.5C).
- **Battery life:** roughly **2.5–5 h** BLE-active (biggest driver is the radio, ~40–100 mA), **6–10+ h** if you idle the OLED and radio, and **weeks** if you use deep sleep between wakes.

---

## Board variance (read this first)

The "ESP32-C3 SuperMini" name covers several near-identical clones, and the details below vary by revision. Verify *your* board before wiring anything:

| Variant | Onboard charging? | Battery connector? | Notes |
|---|---|---|---|
| **Classic SuperMini** (most common, incl. the 01Space 0.42" OLED board) | **No** | No | Just USB-C/Micro-USB + an LDO. This doc assumes this board. |
| **SuperMini "Plus"/v2** | Sometimes | Sometimes (solder pads) | Adds a WS2812 RGB LED that ruins deep-sleep current (~600 µA–1.5 mA). Charging support is inconsistent between listings — do not assume it. |
| **Official SuperMini Expansion Board** | Yes (add-on) | Yes (PH2.0) | A carrier the SuperMini plugs into; adds USB charging + a Schottky. If you have this, most of section 1 is already solved for you. |
| **DFRobot Beetle ESP32-C3** (a *different* board) | **Yes** (onboard TP4057) | Yes | Not a SuperMini, but the "charging for free" escape hatch if you're willing to swap boards (this is what the DigiPclock build uses). |

> **Honesty flag:** the regulator part number is *usually* an **ME6211C33** (2.0–6.0 V in, ~100 mV dropout at 100 mA, 3.3 V/500 mA out, ~40 µA quiescent). Some clones substitute a different LDO (an "S2WD"-marked part shows up on some units). Behavior is broadly the same but the exact dropout/quiescent numbers can differ. The safe assumptions in this doc hold for any 3.3 V LDO with a low dropout.

### Correcting a common phrasing

You may have seen "the LDO input is a narrow ~3.0–3.6 V." That's imprecise and worth getting right:

- The **`3V3` pin is the LDO *output***, not an input. The "3.0–3.6 V" figure is the range that's *safe to inject into `3V3`* if you bypass the LDO with your own regulated 3.3 V rail — bounded below by the ESP32-C3's brown-out and above by its **3.6 V absolute maximum**.
- The **ME6211's actual input range is 2.0–6.0 V.** With only ~100 mV dropout at smol's currents, it will cleanly regulate 3.3 V from a LiPo all the way from 4.2 V (full) down to ~3.4 V. So feeding the cell into `5V` gives you most of the usable discharge curve — you lose a little at the very bottom, not "most of it."

The core safety point stands: **you cannot connect a raw 4.2 V cell to a 3.3 V pin.** The reason is the 3.6 V chip max, not a narrow LDO input window.

---

## 1. Recommended circuit

### The decision: LDO path vs. boost path

**Recommended (LDO path):** cell → TP4056 (charge + protection) → SuperMini `5V` pin → onboard ME6211 → `3V3` → chip + OLED.

- **Pros:** fewest parts, no extra board, no switching noise near the radio, and the ME6211's low dropout means the board runs from ~4.2 V down to ~3.4 V — essentially the whole flat part of a LiPo curve.
- **Cons:** the LDO is linear, so the ~(Vbatt − 3.3 V) headroom is burned as heat. At smol's currents that's tiny (e.g. at 4.0 V in, 3.3 V out, 50 mA → ~35 mW wasted). Not worth optimizing.

**Alternative (boost path):** cell → TP4056 → **MT3608 boost set to 5.0 V** → SuperMini `5V` → LDO. Or boost to a clean 3.3 V straight into `3V3`.

- **Only worth it if** you want to squeeze the last few percent out of the cell below 3.4 V. **For smol it's a net negative:** the **MT3608's efficiency falls below ~80% under 50 mA loads**, and smol idles well under that, so a boost converter would *waste* energy exactly when you're trying to save it — plus it adds switching noise and cost.

> **Verdict for smol: use the LDO path.** Skip the boost. A buck-boost only makes sense with a higher-voltage source (e.g. 2S) or a load that always draws >100 mA, neither of which applies here.

### Exact wiring (LDO path, TP4056 + Schottky)

```
                 ┌──────────────────────────────┐
   USB-C  ───────┤ TP4056 / TP4057 charge board  │
  (charge)       │  (with DW01+8205 protection)  │
                 │                                │
                 │  B+  ●──────────────► LiPo  +  (red)   502030 3.7V 250mAh
                 │  B−  ●──────────────► LiPo  −  (black)
                 │                                │
                 │  OUT+ ●────►|────┐   (Schottky, e.g. BAT43/SS14; band toward SuperMini)
                 │  OUT− ●───┐      │
                 └───────────┼──────┼─────────────┘
                             │      │
                             │      └─────────────► SuperMini  5V (VBUS) pin
                             └────────────────────► SuperMini  GND pin
```

Pin-by-pin, on the **SuperMini** side:

| SuperMini pin | Connect to | Notes |
|---|---|---|
| **`5V` / `VBUS`** | TP4056 **`OUT+`** *through a Schottky diode* (band/cathode toward the SuperMini) | This feeds the onboard LDO. The diode blocks the SuperMini's USB 5 V from back-feeding into `OUT+`/the cell if you ever plug USB into the *SuperMini's* port. Costs ~0.2–0.3 V (fine — ME6211 still has headroom). |
| **`GND`** | TP4056 **`OUT−`** | Common ground. |
| **`3V3`** | **nothing** (leave as output) | Do not inject here in this design. |

On the **TP4056** side:
- **`B+` → cell red (+)**, **`B− → cell black (−)`**. Use `B+`/`B−` (protected, via the DW01/8205 FETs), **not** `OUT+`/`OUT−`, for the cell.
- **`OUT+`/`OUT−`** feed the system (the SuperMini), as above.
- Charge via the **TP4056's own USB-C port**.

> Use a TP4056 board that has the **DW01 + FS8205A protection pair** (the versions with 6 pins / an extra IC), so the cell gets over-discharge, over-charge, and short-circuit protection. Bare "charge-only" TP4056 boards lack this — for a LiPo you want protection.

### The "won't start on battery" gotcha and other back-feed traps

1. **"It runs on USB but won't boot on battery."** Two usual causes:
   - **Charge current is being mistaken for a dead cell / brown-out.** More often, it's the **Schottky/LDO dropout** stacking up: `OUT+` (≈cell V) minus the Schottky drop must stay above the LDO's minimum. With a ~4.2 V full cell and a 0.3 V Schottky you have ~3.9 V into the ME6211 — plenty. But near end-of-charge (~3.5 V cell − 0.3 V diode = 3.2 V in) you're getting close to brown-out during radio current spikes. Mitigations: use a **low-Vf Schottky (BAT43 ~0.3 V, or an SS14 ~0.3 V at low current)**, or an ideal-diode/P-FET, or accept a slightly earlier cutoff.
   - **Cold-start into a discharged/UVLO'd cell:** if the protection IC has tripped on over-discharge, the board is dead until you plug in USB (which resets the protection). Normal; just recharge.
2. **Do not back-power the SuperMini's `5V` while USB is plugged into the *SuperMini's* USB-C.** The SuperMini connects USB `VBUS` **directly** to its `5V` pin (there is **no onboard reverse-blocking diode** on most units). If your battery rail is also on `5V` with no diode, you'd have two sources fighting. The **Schottky on `OUT+`→`5V`** solves this: it lets battery power flow *in* but blocks USB 5 V from flowing *out* toward the cell. (You'd normally charge via the TP4056's port, not the SuperMini's, so both ports are rarely live at once — but the diode makes it safe if they are.)
3. **Never connect the TP4056's `OUT+` to the SuperMini `3V3` pin.** `OUT+` is ~2.5–4.2 V; anything above 3.6 V on `3V3` kills the chip.
4. **Don't parallel a second 3.3 V LDO onto `3V3`.** Two regulators with the same setpoint fight and can oscillate. If you ever go the "clean 3.3 V into `3V3`" route, you must feed the *bypassed* board (accept that the onboard LDO input is unpowered) — that's fiddly; the `5V`-pin path above avoids it entirely.

> **Charge-while-running caveat:** the TP4056 detects "full" by watching the charge current taper. If smol is drawing current from `OUT+` while charging, that load can confuse end-of-charge detection (it may terminate early or cycle). For a clean charge, power smol *off* (or into deep sleep) while charging, or accept imperfect termination. This is inherent to the cheap TP4056 topology (no true power-path management) — the IP5306 option in section 2 fixes it if it matters to you.

---

## 2. Parts list (approx. cost + links)

Prices are ballpark USD, mid-2026, small-quantity hobbyist sources; all vary.

### Recommended build (LDO path)

| Part | ~Cost | Link / search | Notes |
|---|---|---|---|
| **TP4056 USB-C module with protection** (DW01+8205) | $1–2 (often <$0.50/ea in 5-packs) | Search "TP4056 Type-C 1A charging module current protection". Datasheet/pinout: https://mischianti.org/tp4056-lipo-battery-charger-high-resolution-pinout-datasheet-and-specs/ | Get the **Type-C** version with the **6-pin protection** variant. TP4057 is a pin-similar sibling. |
| **Schottky diode** (BAT43, 1N5817, or SS14) | ~$0.10 | Any parts vendor | Low forward drop; ≥0.5 A rating is ample. Band toward the SuperMini `5V`. |
| **502030 LiPo, 3.7 V 250 mAh, JST-PH 2.0** | $6–9 | e.g. EEMB/AKZYTUE on Amazon (verify polarity!) | **5.3 × 20.5 × 32 mm.** Red = +, black = −. Vendors rate charge at **125 mA (0.5C)**. |
| **~10 kΩ 0603 resistor** (to reprogram TP4056 charge current) | ~$0.05 | Any | Replaces the TP4056's `R3` (default 1.2 kΩ = 1 A). See charge-current note below. |
| **JST-PH 2.0 pigtail/socket** (optional) | ~$0.20 | Any | Only if your TP4056 has solder pads rather than a connector, or you want a removable cell. |

**Recommended-build subtotal: roughly $8–12**, dominated by the battery.

### Alternative: combined charge + boost (fewer boards, but read the tradeoffs)

| Part | ~Cost | Notes |
|---|---|---|
| **IP5306-based module** (charge + protect + 5 V boost + power-path) | $2–4 | Charges at up to ~2.1 A (reprogram down for 250 mAh!), boosts to 5 V feed the `5V` pin. Has **power-path management** so it can run the load *and* charge cleanly — fixes the TP4056 charge-while-running caveat. Downsides: 5 V boost then LDO is double-conversion (less efficient at µA idle), and many IP5306 boards have a button/auto-off that shuts down under light load — check the variant. |
| **MT3608 boost module** (if boosting a plain TP4056 output) | ~$1 | Set to 5.0 V *before* connecting. **Efficiency <80% under 50 mA** — not recommended for smol's low idle. Datasheet: https://docs.cirkitdesigner.com/component/599e2950-f264-4655-9939-93eadd61c264/boost-converter-mt3608 |

> **A neater all-in-one:** several purpose-built "LiPo charger + boost" boards exist (e.g. wagiminator's open-source Power-Boards: https://github.com/wagiminator/Power-Boards). Worth a look if you want one small PCB instead of TP4056 + diode.

### Charge-current note (safety — do this)

The default TP4056 charges at **1 A**. For a **250 mAh** cell that's **4C — too high and a fire/degradation risk.** Target **0.5C ≈ 125 mA**:

> Rprog = 1200 / I(A) = 1200 / 0.125 = **~9.6 kΩ → use 10 kΩ.**

Replace the module's `R3` (the small resistor near the TP4056 IC, usually marked `122` for 1.2 kΩ) with **10 kΩ**. That sets charge current to ~120 mA. (A gentler 0.2–0.3C, i.e. 15–22 kΩ for ~55–80 mA, is even kinder to a small cell and only costs charge time.) A 250 mAh cell at 125 mA charges in roughly 2.5–3 h.

---

## 3. Physical / case impact

**Board footprints:**
- SuperMini + 0.42" OLED PCB: ~24.8 × 20.45 mm.
- **502030 cell: 5.3 × 20.5 × 32 mm** — note it's **32 mm long**, *longer* than the PCB, and 5.3 mm thick.
- **TP4056 Type-C module: ~17 × 17 mm** (some ~26 × 17 mm) **× ~4–5 mm** tall with the USB-C shell, and it **needs its own USB-C edge exposed** for charging.

**Does the charger fit inside the case we just deepened?**

We deepened the interior by **~6.5 mm for the battery only.** Assessment:

- The **502030 alone (5.3 mm)** fits that 6.5 mm depth with ~1 mm to spare. Good.
- **The TP4056 does *not* comfortably also fit that budget** if you try to *stack* it on the battery: 5.3 mm (cell) + ~4–5 mm (TP4056) ≈ **10 mm**, well over 6.5 mm. And even laid *beside* the battery, the 502030's 32 mm length plus a 17 mm board pushes the interior footprint past the ~25 × 20 mm PCB outline — the case would have to grow in **X/Y**, not just depth.
- **The USB-C access is the real constraint.** The SuperMini's own USB-C is on the short edge opposite the OLED. The TP4056 has a *second* USB-C that must reach an outside edge. Two USB-C cutouts on a ~25 mm-wide case is awkward.

**Recommended case approach (in rough order of least effort):**

1. **Side/edge-mount the TP4056** in its own shallow pocket on a *long* side of the case, with its USB-C flush to that side wall — separate from the battery bay. This keeps the deepened floor just for the cell and adds a slim lateral bump (~5 mm) rather than more depth. **Best balance.**
2. **External charging dongle:** don't embed the TP4056 at all. Bring the cell's JST-PH out to a small external "charge caddy" (TP4056 in a tiny clip-on box) and run only the protected `OUT+`/`OUT−` (+Schottky) into the case. Case stays minimal; you unplug to charge. Good if interior volume is precious.
3. **Grow the lid, not the floor:** if you must embed everything, add height on the **lid** side to host the TP4056 above the PCB (not above the battery), and cut its USB-C into the lid's edge. Adds ~5 mm to overall thickness.
4. **Switch to a board with onboard charging** (DFRobot Beetle C3, or the SuperMini expansion carrier) and skip the separate TP4056 entirely — then only *one* USB-C exists and you just need the battery bay. Biggest change, cleanest result. (Cross-references `docs/cases.md` and `docs/wearables.md`.)

**Also budget for:**
- A **power switch** (SPST) in series with `OUT+` if you want a true off (deep sleep still draws µA–mA; see section 4). A slide switch on a side wall is easy.
- Strain relief for the JST-PH lead so repeated charging cycles don't fatigue the cell tabs.

---

## 4. Battery-life estimate (250 mAh)

### Stated assumptions (measured/typical ESP32-C3 figures)

These are **at 3.3 V**; the numbers below fold in the OLED and treat the LDO as ~lossless at these currents (true within a few percent). Sources: robdobson C3 measurements, Espressif docs, OLED vendor figures.

| Mode | ESP32-C3 draw | + 0.42" OLED | ≈ System avg | Basis |
|---|---|---|---|---|
| **BLE advertising, no modem sleep** | ~97 mA avg (peaks ~224 mA) | +~5 mA | **~100 mA** | robdobson |
| **BLE advertising, modem sleep on** | ~42 mA avg | +~5 mA | **~47 mA** | robdobson |
| **BLE connected (modem sleep)** | ~41 mA avg | +~5 mA | **~46 mA** | robdobson |
| **CPU active, radio idle, OLED on** | ~20–27 mA | +~5 mA | **~25–32 mA** | robdobson empty/delay loop |
| **Light sleep (radio parked), OLED off** | ~0.35 mA | ~0 | **~0.4 mA** | robdobson |
| **Deep sleep (bare module)** | ~5–43 µA | 0 | **~0.04 mA** *ideal* | Espressif |
| **Deep sleep (real SuperMini)** | — | — | **~0.4 mA typical** | see caveat |

> The **0.42" OLED is a small load** (72×40 lit pixels; typically **≤5 mA**, often 2–4 mA depending on how many pixels are lit — OLED current scales with lit-pixel count). The **radio dominates** everything.

### Runtime math

Usable capacity from a 250 mAh cell is realistically **~200 mAh** (you don't run it flat; protection cuts off, and the LDO gives out near ~3.4 V input). Runtime ≈ 200 mAh ÷ avg current:

| Usage pattern | Avg current | **Est. runtime** |
|---|---|---|
| BLE advertising, **no** modem sleep (worst case) | ~100 mA | **~2 h** |
| BLE advertising/connected, **modem sleep on** (realistic "smol doing BLE") | ~47 mA | **~4–4.5 h** |
| Active CPU + OLED, radio quiet (e.g. a local game/clock, BLE off) | ~28 mA | **~7 h** |
| Mostly light sleep, brief wakes | ~2–5 mA (duty-cycle dependent) | **~1.5–4 days** |
| Deep sleep with periodic wake (real SuperMini ~0.4 mA floor) | ~0.4 mA | **~3 weeks** (floor-limited) |
| Deep sleep, floor removed (LED + LDO mods, ~40 µA) | ~0.04 mA | **months** (self-discharge dominates) |

**Bottom line:** with BLE active, plan on **~2.5–5 hours** on a single 250 mAh cell. That's the honest number for a BLE handheld. To go longer you must **duty-cycle the radio and OLED**.

### Deep-sleep options to extend it

- **Enable BLE modem sleep** (`CONFIG_BT_CTRL_MODEM_SLEEP`) and **lengthen the advertising interval** (e.g. 1 s): drops advertising from ~97 mA to ~35–42 mA — the single biggest easy win, roughly **2×** runtime.
- **Turn the OLED off / dim it when idle** (SSD1306 sleep command). Saves the few mA and, more importantly, screen burn-in over time.
- **Light sleep between activity** (~0.35 mA, RAM retained, fast wake): great when you need to stay "responsive within a few ms" but idle most of the time. **Caveat:** BLE *connections* can't tolerate arbitrarily long sleeps without dropping — tune to your connection interval.
- **Deep sleep** (~5–43 µA on the bare chip): the deepest saving, but **the ESP32-C3 resets on wake** (RAM lost except RTC memory), so it suits *periodic-wake* firmware (sensor logger, clock that wakes to redraw), **not** a persistently BLE-connected game controller. Wake on timer or on a GPIO (e.g. the GPIO9 button).
- **Mind the SuperMini's deep-sleep floor.** In practice these boards draw **~400 µA in deep sleep**, not the datasheet ~10–43 µA, because of the **onboard power LED and the LDO's quiescent current**. To actually hit the low-µA range you'd **remove/desolder the power LED** and possibly the LDO (or move to a board designed for low sleep). The **"Plus" variant is worse (~600 µA–1.5 mA)** because its WS2812 RGB LED never fully sleeps — avoid that variant for battery use.
- **A hard power switch** on `OUT+` is the only true 0 mA state; deep sleep is low, not zero.

---

## Safety flags (explicit)

- ⚠️ **Do not charge a 250 mAh cell at the TP4056's default 1 A.** Reprogram to ~125 mA (10 kΩ). High-rate charging of a small pouch cell risks swelling, venting, or fire.
- ⚠️ **Never put >3.6 V on the `3V3` pin.** A raw LiPo (up to 4.2 V) on `3V3` permanently damages the ESP32-C3. Use the `5V`-pin+LDO path, or a *regulated, current-limited* 3.3 V rail.
- ⚠️ **Do not "charge" a LiPo with a plain buck/boost set to 4.2 V.** LiPo charging needs a proper CC/CV profile — always a dedicated charge IC (TP4056/TP4057/IP5306/BQ2407x).
- ⚠️ **Use a protected cell + protection module.** LiPos need over-discharge / over-charge / short protection. Prefer the TP4056 board with the DW01+8205 pair.
- ⚠️ **Watch the diode/dropout stack near end-of-charge** so the board doesn't brown out mid-write; a low-Vf Schottky or ideal-diode keeps margin.
- Provide **strain relief** on the cell leads and don't pinch the pouch inside the case.

---

## Sources

- ESP32-C3 SuperMini + TP4056 wiring discussion — https://forum.arduino.cc/t/charging-lithium-ion-battery-using-the-usb-port-on-esp32-c3-supermini-with-tp4056-and-powering-the-esp32-with-the-battery-when-not-connected-via-usb/1302012
- SuperMini review (regulator, no onboard charging) — https://sigmdel.ca/michel/ha/esp8266/super_mini_esp32c3_en.html and https://done.land/components/microcontroller/families/esp/esp32/developmentboards/esp32-c3/c3supermini/
- SuperMini "Plus/v2" (deep-sleep penalty, LDO) — https://mischianti.org/esp32-c3-supermini-plus-v2-high-resolution-pinout-datasheet-and-specs/
- ME6211C33 LDO datasheet (2.0–6.0 V in, ~100 mV dropout, 40 µA Iq) — https://datasheet.lcsc.com/lcsc/Nanjing-Micro-One-Elec-ME6211C33M5G-N_C82942.pdf
- ESP32-C3 power/current measurements (active, BLE, light sleep) — https://robdobson.com/2023/11/investigating-esp32-c3-power-management/
- Espressif ESP32-C3 current-consumption guide — https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-guides/current-consumption-measurement-modules.html
- SuperMini ~400 µA deep-sleep thread — https://www.esp32.com/viewtopic.php?t=44444
- TP4056 pinout / charge-current programming — https://mischianti.org/tp4056-lipo-battery-charger-high-resolution-pinout-datasheet-and-specs/ and https://www.best-microcontroller-projects.com/tp4056.html
- Schottky back-feed protection (XIAO/ESP32 context) — https://www.seeedstudio.com/blog/2025/11/24/designing-a-xiao-expansion-board-using-a-schottky-diode-to-prevent-power-backfeeding/
- MT3608 boost efficiency / IP5306 combined module — https://docs.cirkitdesigner.com/component/599e2950-f264-4655-9939-93eadd61c264/boost-converter-mt3608 and https://github.com/wagiminator/Power-Boards
- 502030 250 mAh cell (dimensions, JST-PH, 0.5C charge) — https://ydlbattery.com/products/3-7v-250mah-502030-lithium-polymer-ion-battery
- OLED current scaling — https://bitbanksoftware.blogspot.com/2019/06/how-much-current-do-oled-displays-use.html
