# Piezo Sound for Block Digger (ESP32-C3 SuperMini)

Adds simple sound effects to the "Block Digger" game: a short blip when you dig,
a lower blip when you place a block, and a little jingle at game start.

Everything here is verified against the actually-installed core:
**arduino-esp32 `3.3.10`** (`esp32-hal-ledc.h/.c`). The ESP32 Arduino core does
**not** provide the plain Arduino `tone()` function, so we drive the buzzer with
the **LEDC** peripheral instead.

> Scope note: this document is a design + reference. Wiring the buzzer and
> pasting these snippets into `blockdigger.ino` is a manual follow-up step; the
> `.ino` is intentionally left unchanged here.

---

## 1. Hardware: pin choice + wiring

### Pin choice — use **GPIO10**

The ESP32-C3 SuperMini exposes 13 GPIOs on its headers: `GPIO0`–`GPIO10`,
`GPIO20`, `GPIO21`. We need a pin that is:

- **not** a strapping pin (those are **GPIO2, GPIO8, GPIO9** — they set boot mode
  and can misbehave if pulled at reset),
- **not** already used by this project (`GPIO5`/`GPIO6` = I²C, `GPIO8` = onboard
  LED, `GPIO9` = BOOT button),
- **not** needed for the serial console.

| Pin        | Status for a buzzer                                                        |
|------------|-----------------------------------------------------------------------------|
| GPIO2/8/9  | ❌ strapping pins — avoid                                                    |
| GPIO4–7    | ❌ GPIO4–5/6–7 tied to JTAG/flash + I²C here — avoid                          |
| GPIO20/21  | ⚠️ UART0 **RX/TX** (console). GPIO21 = TX. Repurposing kills serial console  |
| **GPIO10** | ✅ **safe general-purpose pin, no boot/JTAG role — use this**                 |
| GPIO3      | ✅ safe alternate (also an ADC pin) if GPIO10 is otherwise occupied           |

**Decision: `GPIO10`.** It is verified as a free, non-strapping, non-console pin
on the C3 SuperMini. `GPIO3` is a fine fallback.

> Note on GPIO21: the C3 SuperMini uses **USB-CDC** for the Arduino Serial
> Monitor, so USB logging keeps working even if you take GPIO21. But GPIO21 is
> still the hardware UART0 TX (bootloader/`Serial0` output), so we avoid it to
> keep the physical console intact.

> A passive piezo is a high-impedance capacitive load and does not hold GPIO10
> at a logic level, so it has no effect on boot even though it shares no
> strapping duty.

### Passive vs. active buzzer — this matters

- **Passive piezo** (what this design targets): just a piezo element, no internal
  oscillator. It needs an **AC / square-wave drive** to make sound — you feed it
  a frequency. This is exactly what LEDC produces. Pitch is controllable, so we
  can play different notes. If you connect a passive piezo to a steady DC HIGH it
  will only click once and then go silent.
- **Active buzzer**: has a built-in oscillator. Apply DC power and it emits **one
  fixed tone**; you cannot change the pitch. If you have one of these, ignore the
  LEDC code and just `digitalWrite(PIEZO_PIN, HIGH/LOW)` to turn it on/off.

Rule of thumb: if it makes a tone the instant you touch it to a coin cell, it's
**active**. If touching a coin cell only clicks, it's **passive**.

### Wiring (passive piezo)

```
  ESP32-C3 SuperMini                     Passive piezo
  ┌───────────────┐                      ┌─────────┐
  │        GPIO10  ●─────────────────────● +  (marked leg / longer pin)
  │                │                      │  ((•))  │
  │           GND  ●─────────────────────● −  (other leg)
  └───────────────┘                      └─────────┘
```

- `GPIO10  →  piezo (+)`
- `piezo (−)  →  GND`
- No series resistor is strictly required for a bare piezo element (it's
  capacitive, not a resistive load). If yours is a louder module or you want to
  tame the volume, put a **100 Ω** resistor in series with the `+` leg.
- Polarity is not critical for a bare two-pin piezo disc, but follow the marking
  if present.
- The ESP32-C3 GPIO can source only a few mA; a small piezo is fine driven
  directly. Do **not** drive a large speaker this way — that needs a transistor
  or amp.

---

## 2. Tone generation on ESP32-C3 (LEDC) — verified for core 3.3.10

`tone()` / `noTone()` from AVR Arduino are **not** implemented on the ESP32 core.
We use the LEDC (LED Control) PWM peripheral, which can output a 50%-duty square
wave at an arbitrary frequency — perfect for a passive piezo.

### API check (important: the API changed in 3.x)

The LEDC API was **rewritten in arduino-esp32 3.0**. Confirmed against the
installed header `…/esp32/3.3.10/cores/esp32/esp32-hal-ledc.h`:

| Purpose                       | **3.x API (use this)**                       | Old 2.x API (removed) |
|-------------------------------|----------------------------------------------|-----------------------|
| Attach pin + configure        | `ledcAttach(pin, freq, resolution)` → `bool` | `ledcSetup(chan, freq, res)` + `ledcAttachPin(pin, chan)` |
| Play a tone (50% square wave) | `ledcWriteTone(pin, freq)` → `uint32_t`      | `ledcWriteTone(chan, freq)` |
| Stop tone / set duty          | `ledcWrite(pin, duty)` → `bool`              | `ledcWrite(chan, duty)` |
| Release pin                   | `ledcDetach(pin)` → `bool`                   | — |

Verified facts about 3.3.10:

- `ledcSetup` and `ledcAttachPin` **do not exist** in this core (grep of the core
  sources returns nothing) — code written for 2.x will not compile.
- In 3.x you operate **per-pin**, not per-channel. `ledcAttach()` picks a free
  LEDC channel automatically and returns `false` if none is available.
- Exact signatures from `esp32-hal-ledc.h` (3.3.10):
  ```c
  bool     ledcAttach(uint8_t pin, uint32_t freq, uint8_t resolution);
  uint32_t ledcWriteTone(uint8_t pin, uint32_t freq);   // freq==0 => silent
  bool     ledcWrite(uint8_t pin, uint32_t duty);
  bool     ledcDetach(uint8_t pin);
  ```
- `ledcWriteTone(pin, 0)` sets duty to 0 (silence). We also explicitly
  `ledcWrite(pin, 0)` to be safe.
- The `resolution` passed to `ledcAttach` is the duty resolution in bits. For a
  buzzer the exact value barely matters; **10 bits** is a safe, conventional
  choice on the C3. The initial `freq` given to `ledcAttach` is just a starting
  value — `ledcWriteTone` changes it per beep.

> Bonus: the core also defines `ledcWriteNote(pin, note_t note, uint8_t octave)`
> with a `note_t` enum (`NOTE_C`, `NOTE_Cs`, …). Handy if you'd rather write
> musical notes than raw Hz. We use raw Hz below for clarity.

### Drop-in code

Add near the top of the sketch (after the other `#include`s):

```cpp
// ---- Sound (passive piezo on GPIO10, via LEDC) ---------------------------
static const int  PIEZO_PIN     = 10;    // safe non-strapping pin on C3 SuperMini
static const int  PIEZO_RES     = 10;    // LEDC duty resolution (bits)
static const uint32_t PIEZO_BASE_FREQ = 1000;  // starting freq for ledcAttach

// Musical-ish frequencies (Hz)
#define TONE_DIG    1800   // dig: short bright blip
#define TONE_PLACE   700   // place: lower, softer blip
#define NOTE_C5      523
#define NOTE_E5      659
#define NOTE_G5      784
#define NOTE_C6     1047

bool soundReady = false;

void soundBegin() {
  // Attaches PIEZO_PIN to an auto-assigned LEDC channel.
  // Returns false if no LEDC channel is free.
  soundReady = ledcAttach(PIEZO_PIN, PIEZO_BASE_FREQ, PIEZO_RES);
  ledcWrite(PIEZO_PIN, 0);            // start silent
}

// Blocking beep. `ms` is the duration; simple and fine for a game this small.
void beep(uint32_t freq, uint32_t ms) {
  if (!soundReady) return;
  ledcWriteTone(PIEZO_PIN, freq);    // 50% square wave at `freq`
  delay(ms);
  ledcWriteTone(PIEZO_PIN, 0);       // stop (freq 0 => silent)
  ledcWrite(PIEZO_PIN, 0);           // ensure duty 0, pin idle low
}

// --- Sound effects ---
void sfxDig()   { beep(TONE_DIG,   35); }              // crisp tick when digging
void sfxPlace() { beep(TONE_PLACE, 60); }              // duller thunk when placing

void sfxStart() {                                       // little ascending jingle
  beep(NOTE_C5, 90);
  beep(NOTE_E5, 90);
  beep(NOTE_G5, 90);
  beep(NOTE_C6, 140);
}
```

> **Timing caveat:** `beep()` uses `delay()`, so it blocks the game loop for the
> beep duration. The effects above are deliberately short (35–60 ms) so the
> ~60 fps loop barely notices. The start jingle (~410 ms total) runs once in
> `setup()` before gameplay, so blocking there is fine. If you later want zero
> stutter, switch to a non-blocking timer (see the note at the end).

---

## 3. Where to hook the effects into the game

The game already has clean, isolated event points. Conceptually:

**a) Initialise the buzzer** — in `setup()`, alongside the other peripheral init,
then play the jingle once the world is ready:

```cpp
void setup() {
  Wire.begin(I2C_SDA, I2C_SCL);
  u8g2.begin();
  u8g2.setBusClock(400000);

  soundBegin();                 // <-- add: configure LEDC on GPIO10

  COLS = min((int)(u8g2.getDisplayWidth()  / TILE), MAXW);
  ROWS = min((int)(u8g2.getDisplayHeight() / TILE), MAXH);
  generateWorld();

  sfxStart();                   // <-- add: play start jingle once

  BP32.setup(&onConnect, &onDisconnect);
  BP32.forgetBluetoothKeys();
  BP32.enableVirtualDevice(false);
}
```

**b) Dig blip** — in `dig()`, only when a block is actually removed (inside the
existing `if` so it doesn't beep on empty swings):

```cpp
void dig() {
  int tc = p.x + p.facing, tr = p.y;
  if (!solid(tc, tr)) { tc = p.x; tr = p.y + 1; }
  if (tc >= 0 && tc < COLS && tr >= 0 && tr < ROWS && world[tr][tc] != AIR) {
    world[tr][tc] = AIR;
    if (inventory < 999) inventory++;
    sfxDig();                   // <-- add: blip on successful dig
  }
}
```

**c) Place blip** — in `place()`, right after a block is successfully placed
(inside the existing success `if`):

```cpp
void place() {
  if (inventory <= 0) return;
  int tc = p.x + p.facing, tr = p.y;
  if (solid(tc, tr)) { tc = p.x; tr = p.y + 1; }
  if (tc >= 0 && tc < COLS && tr >= 0 && tr < ROWS && world[tr][tc] == AIR
      && !(tc == p.x && tr == p.y)) {
    world[tr][tc] = DIRT;
    inventory--;
    sfxPlace();                 // <-- add: blip on successful place
  }
}
```

Placing the calls *inside* the success branches means you only hear a sound when
something actually happened — no beep when you dig at air or try to place with an
empty inventory. Both `dig()` and `place()` are already edge-triggered from
`handleInput()` (one button press = one call), so each action makes exactly one
blip.

### Optional flourishes (ideas, not required)

- Jump: `beep(1200, 20);` on the rising edge of "up" in `handleInput()`.
- Landing thud when gravity moves the player down in `loop()`.
- Different dig pitch per block type (e.g. higher for `STONE`, lower for `DIRT`)
  by passing the removed block's type into `sfxDig()`.

---

## Appendix: non-blocking beep (optional upgrade)

If the `delay()` in `beep()` ever causes noticeable stutter, replace it with a
timer that turns the tone off in `loop()` instead of blocking:

```cpp
uint32_t beepOffAt = 0;

void beepStart(uint32_t freq, uint32_t ms) {
  if (!soundReady) return;
  ledcWriteTone(PIEZO_PIN, freq);
  beepOffAt = millis() + ms;
}

// call once per loop():
void soundTick() {
  if (beepOffAt && millis() >= beepOffAt) {
    ledcWriteTone(PIEZO_PIN, 0);
    ledcWrite(PIEZO_PIN, 0);
    beepOffAt = 0;
  }
}
```

This plays only one sound at a time (a new `beepStart` overrides the current
one), which is fine for quick game blips. The multi-note `sfxStart()` jingle
still uses the simple blocking `beep()` since it runs once at boot.

---

### Sources

- Installed core headers (ground truth): `~/.arduino15/packages/esp32/hardware/esp32/3.3.10/cores/esp32/esp32-hal-ledc.h` and `esp32-hal-ledc.c`
- [ESP32-C3 Super Mini Pinout Reference — Last Minute Engineers](https://lastminuteengineers.com/esp32-c3-super-mini-pinout-reference/)
- [ESP32-C3 Super Mini Board — espboards.dev](https://www.espboards.dev/esp32/esp32-c3-super-mini/)
- [GPIO & RTC GPIO — ESP-IDF Programming Guide (ESP32-C3)](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-reference/peripherals/gpio.html)
