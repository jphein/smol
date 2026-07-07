# Encasing smol in epoxy resin with a cast watch lens, in a donor watch case

> Compiled by the **RESIN** agent, 2026-07-07.
> Target hardware: ESP32-C3 SuperMini + 0.42" SSD1306 OLED (72×40, I²C, SDA=GPIO5/SCL=GPIO6), USB-C, blue LED, ceramic 2.4 GHz WiFi/BLE chip antenna, optional 502030 LiPo (~250 mAh) + TP4056 (see [`docs/power.md`](power.md)).

## TL;DR (read this before you buy resin)

- **Resin is fine for the radio; a metal case is not.** UV resin and 2-part epoxy are effectively transparent to 2.4 GHz, so WiFi/BLE/ESP-NOW keep working *through resin*. A **metal** watch/pocket-watch case is a **Faraday cage** and will badly attenuate or kill the radio — that means **no NTP clock sync and no ESP-NOW peer-LED mesh**. Use a **non-metal case**, or a metal case with a **non-metal back/bezel/crystal**, and keep the PCB's **ceramic-antenna end facing resin/plastic**, never sandwiched against metal.
- **Epoxy cure is exothermic — with a LiPo inside this is a fire risk.** A thick, fast pour can self-heat to **60–100 °C+** (industrial sources cite internal temps over 90 °C / 200 °F), which can cloud/stress the OLED and, around a charged lithium cell, drive **venting or thermal runaway (fire)**. **Do not pot a charged LiPo in a thick exothermic mass.** Prefer to leave the battery *out* of the resin.
- **Full encapsulation is permanent.** No USB-C reflash, no BOOT/RST, no charge port, no battery swap — ever. **Finalize and fully hardware-test the (unified Rust) firmware first**, then pick a power/access strategy *before* you pour.
- **Never pour resin directly on the OLED glass.** Mask/dam it or set a pre-made watch crystal over it with a small **air gap**. A domed crystal is a bonus: it **magnifies** the tiny 72×40 screen.

---

## 1. RF / antenna — the make-or-break issue (critical)

### Resin: transparent. Metal: a Faraday cage.

Cured casting resins (epoxy, polyester, UV/acrylic) are **dielectrics** with no free electrons, so they don't block 2.4 GHz — they only load the antenna slightly (a mild dielectric-loading detune, discussed below). The radio will work through a solid resin puck.

A **metal watch or pocket-watch case is the opposite**: a continuous conductive shell is a classic **Faraday cage**. Metallic housings block Wi-Fi/Bluetooth/GPS; the metal forms a barrier that reflects/absorbs the field, and it doesn't have to be sealed to hurt you — even seams and a snap-on metal caseback couple strongly to the antenna and detune/short it. Smartwatch makers only get a radio out of a metal case by turning the metal *frame itself* into a tuned antenna with carefully engineered feed/short points and a ground gap — you cannot replicate that by dropping a stock SuperMini into a solid steel hunter case.

**What this costs smol specifically:** the C3 has **one radio on one channel**. Kill or cripple it and you lose:
- **NTP/SNTP time sync** over WiFi — the clock can't set itself (`--features wifi`).
- **ESP-NOW** peer switching / the LED mesh (`--features espnow`).
- Any future BLE control (Stadia pad, phone).

A local-only OLED clock that you set by hand would *survive* a metal case; anything networked will not.

### Rules for a working radio

1. **Prefer a non-metal case:** solid resin, plastic/celluloid vintage cases, wood, or a resin "watch" you cast yourself. Fully RF-transparent.
2. **If you love a metal case, break the cage:** use a metal body with a **non-metal front crystal AND a non-metal back** (acrylic/mineral crystal front, resin or plastic caseback), and orient the **ceramic antenna toward one of those non-metal windows**. A metal bezel *ring* with plastic front+back is far better than a full metal can, but expect some range loss.
3. **Keep the antenna end in the clear.** On the SuperMini the ceramic chip antenna is at the **USB-C end** (opposite the OLED). Give it a **keep-out**: no metal (and ideally no dense battery) within ~**10–15 mm** of the antenna, and never lay metal flat across it. Antenna datasheets demand a copper keep-out under the element for exactly this reason; a case wall of metal a couple mm away does the same detuning a ground plane would, only worse.
4. **Mind the LiPo too.** A metal-cased pouch cell is itself a small conductor — keep it away from the antenna end, not stacked on it.
5. **Resin detune is real but small.** Encasing a chip antenna in a solid dielectric shifts its resonance down a little and drops efficiency a few percent (a well-matched antenna can move >3 dB near large dielectric/metal masses). For a short-range BLE/ESP-NOW toy this is usually acceptable; just don't *also* wrap it in metal. If you can, cast a **thinner resin layer over the antenna end** than over the rest.

> **Bottom line (RF):** resin = OK, metal = clock/mesh killer. Non-metal case, or metal case with non-metal front+back and the antenna facing the opening.

---

## 2. Heat & LiPo danger — the fire issue (critical)

### Why epoxy gets hot

Epoxy cures by an **exothermic** reaction. The heat scales with **mixed mass and thickness**: a big blob has little surface area per volume, can't shed heat, and self-heats. Vendor and manufacturer sources are blunt about it — uncontrolled thick pours **crack, bubble, yellow, smoke**, and can exceed **90 °C / 200 °F internally**. Fast "coating"/"jewelry" epoxies and standard UV resin in bulk are the worst offenders; the reaction can run away once it starts.

**What that heat does to smol:**
- **OLED:** the SSD1306 module has a glass panel, a polarizer, and a flimsy **FPC ribbon**. Sustained 60 °C+ can **cloud, stress-crack, or delaminate** it. (Covered again in §4.)
- **Board/plastics:** hot enough to soften connectors and stress solder joints.
- **LiPo: the real hazard.** A lithium cell potted in a hot, curing exothermic mass can be pushed toward **venting and thermal runaway → fire**, and — critically — **a potted cell has nowhere to vent.** Hobby and industry sources agree: *potting a battery with no vent path is "a potential bomb"* because it traps the hot gas a failing cell produces. Purpose-made battery potting compounds exist, but they are **low-exotherm** systems (often silicone or specially filled epoxy) applied thin, and they still design in a **vent** — not a thick clear casting resin poured for looks.

### Rules to not start a fire or ruin the screen

1. **Use low-exotherm "deep-pour"/casting resin, not fast coating epoxy or bulk UV resin.** Deep-pour systems are formulated to stay cool through thick sections (e.g. Craft Resin Deep Pour, Promise Epoxy, Nerpa casting epoxy, TotalBoat ThickSet — all marketed for 1–2"+ layers with low heat). Follow the vendor's **max pour depth per layer**.
2. **Pour thin, in layers, and let each cool.** Thin layers self-heat far less and let you build up around the board without a hot mass. Work at **~18–27 °C** ambient.
3. **Keep the charged LiPo OUT of the resin (strongly recommended).** Encasing the cell buys you nothing but risk (heat during cure, no venting, no swap, no charge access without extra hardware). Options in priority order:
   - **Best: don't encase the battery.** Pot only the board + OLED area; mount the (removable) cell + TP4056 elsewhere in the case cavity, un-potted, on its own JST-PH lead. You keep swap + vent path.
   - If you *must* embed it: use a **low-exotherm/silicone potting compound**, apply **thin**, **fully discharge the cell first** (a low state-of-charge cell is far less energetic), leave a **vent channel**, and accept you're outside normal safety practice.
4. **Never pour a thick blob around a charged cell.** This is the one "don't" that can hurt you.

> **Bottom line (heat/LiPo):** low-exotherm deep-pour resin, thin cool layers, and keep the LiPo un-potted. A charged LiPo in a thick exothermic pour is a genuine fire risk.

---

## 3. Loss of access — decide before you pour (critical)

Full encapsulation is **permanent and total**: you lose **USB-C reflashing, BOOT + RST buttons, the charge port, and battery swap**. Plan for it.

### Step 0 — finalize the firmware FIRST

We are consolidating a **unified Rust `no_std` firmware** (`rust/clock/`, features `wifi`/`espnow`; see the build cheat-sheet). **Do not pour until that firmware is frozen and fully hardware-tested on the actual board** — flashed over USB-C, radio confirmed working *in the intended case material*, clock/mesh verified, brown-out behavior checked on battery. Once it's in resin, `/dev/ttyACM0` is gone forever. (OTA over WiFi is only a partial escape hatch — and it depends on the very radio a metal case would kill, so it's not a substitute for testing first.)

### Then choose ONE power/interaction strategy

| Strategy | Charge / reflash | Buttons / menu | Cost | Best when |
|---|---|---|---|---|
| **(a) Expose the USB-C edge** | Leave the SuperMini's USB-C short edge **reachable at the case rim** → charge (via onboard/TP4056 path) **and** reflash any time | Can also leave BOOT/RST or a side button reachable | Easiest; least "sealed" look | You want a real, updatable device that happens to live in a watch case |
| **(b) Fully sealed + Qi wireless charging** | **No wired access.** Add a **Qi receiver coil** + the TP4056 inside (the **C3 has no onboard Qi**); charge by placing on a Qi pad. Reflash only via **OTA WiFi** — which fails in a metal case | Needs a wireless/sealed input (below) | Coil + charger + board + LiPo must fit; RF must survive the case | You want a seamless sealed puck and accept OTA-only updates |
| **(c) Sealed clock-only, no buttons** | Sealed (charge via (b) or accept a fixed battery life) | **You lose the menu.** smol's menu needs **BOOT (GPIO9)**; a fully sealed build has no button | Simplest mechanically | A pure ambient clock with no interaction |

**About buttons in a sealed build (c):** to keep *any* interaction without a hole, add one of:
- a **capacitive touch** pad wired to a touch-capable GPIO and sensed *through* the thin resin/crystal (resin is a fine dielectric for cap-touch);
- a **reed switch + external magnet** ("swipe a magnet past the case to advance the menu") — fully sealed, RF-safe, mechanically trivial;
- a **Hall-effect sensor** + magnet (same idea, analog).
Either of these lets a sealed clock keep a one-input menu without breaching the case.

> **Bottom line (access):** freeze + hardware-test the Rust firmware, then commit to (a) exposed USB-C, (b) sealed+Qi (extra coil+TP4056, OTA-only), or (c) sealed clock-only with a reed/cap-touch input for the menu. Note (b) and OTA both **depend on the radio the metal case would kill** — another reason to go non-metal.

---

## 4. OLED + lens (important)

### Do NOT pour resin onto the OLED glass

Direct resin contact with the display is a known way to wreck it:
- **Optical:** resin can **cloud/haze** against the polarizer and never reach the crystal-clear look you wanted.
- **Mechanical:** shrinkage stress during cure can **crack or delaminate** the panel, and the exposed **FPC ribbon** is fragile.
- **Longevity:** OLED emitters are moisture/oxygen sensitive — resin sealing has been shown (in OLED-packaging literature) to let moisture/oxygen migrate into the panel and **degrade brightness within days** in humidity/heat. You want an **air gap**, not resin bonded to the emitter.

### Two clean ways to keep the display clear

1. **Mask/dam and leave an air pocket.** Build a small **dam** around the display glass so resin flows *around* it, not over it (an OLED-packaging trick is literally a "trench" that catches overflow so it never touches the display area). Options: a ring of removable putty/tacky clay, a snug plastic collar, or hot-melt around the module footprint. Result: resin body with a **recessed window** over the (untouched) glass.
2. **Set a pre-made watch crystal over it with an air gap (recommended).** Place a mineral/acrylic crystal (or your cast lens, §6) *above* the display with a **~0.5–2 mm air gap**, bonding the crystal only at its **rim** to the resin/bezel, never across the display face. This gives a real "watch dial" look and protects the OLED.

### Aligning the lens over the 72×40 window

- The **lit area is only ~8.5 × 5 mm (72×40 px)** but it sits inside a **larger glass module** — size and center your window/bezel to the **physical glass**, not the lit pixels (same rule as the printed cases in [`docs/cases.md`](cases.md)).
- **Center the lens over the lit window, not the module center**, since the active area is offset within the glass. Dry-fit with the screen **on** (during firmware test) and mark the true center of the lit rectangle, then align the crystal/bezel to that mark.
- **A domed crystal magnifies** — a plus. A high-dome mineral/acrylic crystal or a plano-convex "cabochon" lens acts as a loupe over the tiny screen, making 72×40 far more readable. This is the single best reason to use a domed watch lens here. (Keep the dome's focal behavior in mind: a very steep dome can distort at the edges; test before committing.)

> **Bottom line (OLED):** never pot the glass. Dam it or cap it with a crystal over an air gap, center on the *lit* rectangle, and let the dome magnify.

---

## 5. Donor case sizing

**What has to fit:**
- **Board (SuperMini + 0.42" OLED PCB): ~24.8 × 20.45 mm** (bare SuperMini ~22.5 × 18 mm), a few mm tall with headers.
- **LiPo 502030: 5.3 × 20.5 × 32 mm** — note it's **32 mm long, longer than the board** and 5.3 mm thick (see [`docs/power.md`](power.md)).
- **TP4056 module: ~17 × 17 × ~5 mm** (only if embedded).
- Resin, crystal, and clearances on top.

**Pocket watches are the natural fit** because they're big and deep. Sizing facts:
- Pocket-watch **movement "size"** uses the Lancashire gauge; a **16s** movement is ~**43.2 mm** across, an **18s** ~**44.9 mm**, but the **outside case** of an 18s railroad watch is ~**56 mm (2.25")** — plenty of diameter for a 25×20 mm board.
- **Depth is the real constraint.** A "hunter"/full-hunter or a thick railroad case gives you the vertical room for **board + LiPo (5.3 mm) + resin + crystal + air gap**. Thin dress pocket watches and most wristwatches are **too shallow** once you add a 5 mm cell. Aim for a donor whose interior clears **≥ ~10–12 mm** if you're stacking board over battery, or size the cavity so board and cell sit **side-by-side** (needs the ~56 mm diameter).
- **Crystal diameter** follows the case — pocket-watch crystals commonly run in the **high-30s to ~50 mm** range; you'll fit the lens to the donor's bezel seat, and there's lots of headroom over the ~10×6 mm display window.
- **Wristwatch route:** possible only for the **board-only, no-LiPo** sealed clock — even then most cases are shallow. A large "cushion"/oversized fashion watch is the best wrist bet.

**Donor-hunting checklist:**
- **Non-metal or metal-with-non-metal-back/bezel** (see §1). A cracked-crystal junk pocket watch you re-crystal is ideal and cheap.
- **Interior depth** for your chosen power strategy (board+LiPo stacked vs side-by-side).
- **Bezel/crystal seat** you can drop a mineral/acrylic crystal (or cast lens) into.
- If exposing USB-C (strategy a): a case where the **rim/pendant area** can take a small notch for the port.

> **Bottom line (sizing):** a **pocket watch, hunter/railroad depth, ~45–56 mm case** swallows the 25×20 mm board plus a 502030 easily; wristwatches only work for a battery-less sealed clock.

---

## 6. Resin + mold specifics, and the pour

### Which resin?

| Type | Use for | Notes |
|---|---|---|
| **Low-exotherm deep-pour/casting epoxy** (Craft Resin Deep Pour, Promise Epoxy, Nerpa casting, TotalBoat ThickSet) | **The body around the board** | Slow, cool cure; obey **max depth per layer**; best safety margin near the OLED/LiPo. |
| **UV resin** (acrylic) | **Casting the small clear lens/crystal**, tiny top coats | Cures in seconds under UV, crystal-clear, but **only in thin sections** (it self-heats and yellows in bulk) and needs UV to reach it. Great for a small **cabochon lens**, wrong for potting the whole board. |
| **2-part "coating"/tabletop epoxy** | *Avoid* for this | Fast + exothermic; higher clouding/heat risk around electronics. |

### Molds & supplies

- **Cast the lens** in a **silicone dome/cabochon mold** — cheap sets cover **~5–32 mm** half-sphere/dome cavities (Little Windows, Funshowcase, Sophie & Toffee, MiniatureSweet, LetsResin). Pick a dome diameter that seats in the donor bezel and covers the display window. Silicone molds are self-releasing.
- **Or buy a real crystal** (mineral or acrylic, domed) sized to the donor — often easier and optically better than casting your own; sapphire is overkill and won't magnify as nicely as a dome.
- **Release agent** for anything cast against non-silicone (or against the case): a resin-specific mold release / mold-release wax so parts pop out and resin doesn't bond to the donor where you don't want it.
- **Degassing / bubbles:**
  - **Vacuum-degas the mixed resin** (~29 inHg for a couple minutes) to pull bubbles *before* pouring; ideal for clear castings.
  - **Or pressure-pot** the poured mold (~40–60 psi) to crush bubbles invisibly-small.
  - **Heat gun / torch pass** knocks down surface bubbles right after pouring.
  - Caveat: don't vacuum a **filled silicone mold** (it froths over); degas the resin, then pour. And a pressure-cast part needs a pressure-cast *mold* to avoid distortion.
- **Safety:** **nitrile gloves**, good **ventilation** (or respirator with organic-vapor cartridge — epoxy sensitization is cumulative and permanent), eye protection, cover the bench. UV resin: don't cure on skin, wear UV-safe eyewear.

### Step-by-step (partial pot — the recommended build)

1. **Freeze + fully hardware-test the firmware** on the real board over USB-C (radio confirmed in the *final* case material). Non-negotiable — see §3.
2. **Pick the case (RF-safe, §1) and power/access strategy (§3).** Decide now whether USB-C stays exposed and whether the LiPo is embedded (default: **not** embedded).
3. **Prep the board:** protect the USB-C port (tape/plug) if it must stay usable; mask the LEDs you want visible.
4. **Mask/dam the OLED** (§4) so no resin can touch the glass; plan the display window.
5. **Protect the antenna end:** keep it toward the case opening / a thin-resin zone; keep metal and battery away from it.
6. **Test-fit** board (and, if used, cell + TP4056) in the donor cavity; confirm depth and where the crystal seats.
7. **Cast the lens** separately in the silicone dome mold (UV or deep-pour), degas, cure, demold. Or set aside your bought crystal.
8. **Mix low-exotherm deep-pour resin**, degas.
9. **Pour in THIN layers**, building the body **around** (not over) the OLED and **around** any un-potted battery bay. Let each layer **cool** before the next (feel it; if it's warming past ~body temp, stop and wait). Never pour a thick mass around the cell.
10. **Cure** fully (deep-pour resins can need **24–72 h**); demold if you cast in a form.
11. **Mount in the donor case:** seat the resin body, place the **crystal/lens over the display with an air gap** (bond at the rim only), fit the bezel/back. Route the exposed USB-C to the rim notch if using strategy (a); fit the reed/cap-touch input if using (c).
12. **Final check:** power on, verify screen through the lens, verify radio (NTP/ESP-NOW) *in the assembled case*, verify charging path.

> **Bottom line (materials):** deep-pour epoxy for the body, UV resin or a bought domed crystal for the lens, silicone dome mold + release + degassing, and **thin cool layers around — never over — the OLED and battery.**

---

## Recommended workflow order

1. **Test firmware** — freeze the unified Rust build; flash + fully hardware-test over USB-C, confirm the radio works **in the final case material**. (Once potted, no reflashing.)
2. **Choose power/access strategy** — (a) exposed USB-C, (b) sealed + Qi (add coil + TP4056, OTA-only), or (c) sealed clock-only with a reed-switch/cap-touch input. Decide whether the LiPo is embedded (default: **no**).
3. **Pick an RF-safe case** — non-metal, or metal with non-metal front + back; antenna end faces the opening.
4. **Mask/dam the OLED** — no resin on the glass; center the window on the **lit 72×40 rectangle**.
5. **Cast the lens** — silicone dome mold (UV/deep-pour) or a bought domed mineral/acrylic crystal; degas.
6. **Partial pot** — low-exotherm deep-pour resin, **thin cool layers around** the board, **battery left un-potted**; cure fully.
7. **Mount** — seat in the donor case, crystal over an air gap (rim-bonded), fit bezel/back, route exposed USB-C or the sealed input, final power/radio/charge check.

## Top 3 gotchas

1. **A metal case kills the radio.** Resin is RF-transparent, but a metal watch/pocket-watch case is a Faraday cage → **no NTP, no ESP-NOW**. Use a non-metal case (or non-metal front+back) and keep the ceramic antenna end facing the opening, never against metal.
2. **Exothermic cure + charged LiPo = fire risk.** Thick/fast pours can hit 60–100 °C, cook the OLED, and make a potted cell vent or ignite (no vent path). Use low-exotherm deep-pour resin in **thin, cool layers**, and **keep the LiPo out of the resin**.
3. **Potting is permanent.** No USB-C reflash, no BOOT/RST, no charge port, no battery swap. **Finalize and hardware-test the firmware first**, and never pour resin onto the OLED glass (mask/dam it or cap it with a crystal over an air gap).

---

### Sources

- RF / Faraday cage & antenna: [Wildflower Cases — does a case hurt your signal](https://www.wildflowercases.com/blogs/news/is-your-phone-case-hurting-your-signal), [TE Connectivity — antenna tech for wearables](https://www.te.com/en/industries/personal-electronics-wearable-tech/insights/antenna-technologies-for-wearables.html), [EEVblog — 2.4 GHz Faraday cage for BLE/WiFi](https://www.eevblog.com/forum/rf-microwave/metal-mesh-to-make-2-4ghz-faraday-cage-for-blewifi/), [NextPCB — antenna keep-out best practices](https://www.nextpcb.com/blog/pcb-antenna-layout-and-keep-out-design), [Cadence — ceramic chip vs PCB antenna](https://resources.pcb.cadence.com/blog/2020-understanding-ceramic-chip-antenna-vs-pcb-trace-antenna), [Johanson — understanding chip antennas](https://www.johansontechnology.com/tech-notes/understanding-chip-antennas/).
- Exotherm / thick pours: [INCURE — does epoxy generate heat](https://incurelab.com/wp/does-epoxy-resin-generate-heat-a-manufacturers-guide-to-exothermic-curing), [WEST SYSTEM — uncontrolled cure](https://www.westsystem.com/safety/uncontrolled-cure/), [MasterBond — low-exothermic epoxy systems](https://www.masterbond.com/properties/low-exothermic-epoxy-systems), [Epoxies Etc. — stress-free potting with low-exotherm epoxy](https://epoxies.com/blog/stress-free-potting-with-low-exotherm-epoxy/).
- Potting LiPo/batteries: [Chimera BMX — thermal potting for Li-ion fire safety](https://chimerabmx.com/blogs/archive/thermal-potting-for-l-ion-battery-fire-safety), [Epic Resins — Li-ion potting compounds](https://www.epicresins.com/Batteries/LithiumIon), [MasterBond — potting compounds for batteries](https://www.masterbond.com/industrial-applications/potting-and-encapsulation-compounds-batteries) (forum discussion of vent-or-bomb: Adafruit "Potting Lithium Batteries" thread).
- Resin over OLED / masking: OLED-packaging patents on resin-induced moisture degradation and cover-plate "trench" dams (USPTO 7279063, 11342398, 9240566).
- Deep-pour resin brands / UV vs epoxy: [Chica and Jo — deep-pour comparison](https://www.chicaandjo.com/epoxy-resin-comparison-which-resin-is-best-for-deep-pours/), [Nerpa — casting epoxy for encapsulation](https://www.nerpa.ca/blogs/news/choosing-the-right-casting-epoxy-for-thick-resin-layers-and-encapsulation), [Craft Resin Deep Pour](https://www.craft-resin.com/products/deep-pour), [INCURE — casting vs deep-pour resin](https://incurelab.com/wp/casting-resin-vs-deep-pour-resin-key-differences-and-best-uses).
- Watch crystals: [Monochrome — guide to watch crystals](https://monochrome-watches.com/technical-perspective-comprehensive-guide-to-watch-crystals-plexiglass-mineral-hesalite-sapphire-crystal-history-pros-and-cons/), [Sangamon — pocket watch glass guide](https://sangamonwatches.com/pages/watch-crystals-and-pocket-watch-glass-a-complete-guide-to-types-sizes-and-replacement), [Esslinger — watch crystals](https://www.esslinger.com/watch-crystals/).
- Molds & degassing: [Little Windows — silicone cabochon mold](https://www.little-windows.com/products/silicone-cabochon-mold), [Easy Composites — how/why to degas](https://www.easycomposites.co.uk/learning/how-and-why-to-degas-silicone-rubber-and-casting-resins), [Smooth-On — vacuum & pressure chambers](https://www.smooth-on.com/product-line/pressure-vacuum-chambers/), [Polytek — reduce bubbles in clear casting resin](https://polytek.com/tutorial/tek-tip-reduce-bubbles-in-clear-casting-resin).
- Pocket-watch sizing: [Pocket Watch Database — sizes & measurement chart](https://pocketwatchdatabase.com/reference/sizes), [KeepTheTime — pocket watch size guide](https://www.keepthetime.com/blog/pocket-watch-size-guide/), [Esslinger — pocket watch size chart](https://blog.esslinger.com/pocket-watch-size-chart/).
- smol internal: [`docs/power.md`](power.md), [`docs/cases.md`](cases.md), and the ESP32-C3 build cheat-sheet.
