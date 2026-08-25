# BENCH-RUNBOOK — the scripted return-of-board session (node 162)

**Purpose: zero thinking at the bench.** Every command below is pre-derived, every
expected output is stated, and **every pass/fail criterion is written here, before
the evidence exists.** That ordering is the point — a threshold chosen after
seeing the number is not a threshold, it is a description.

**Scope:** one bench trip, in the order written. Steps 2→6 each assume the
previous one passed; §7 says what to do when one does not.

**Board:** ES3C28P, smol node **162**, sigil `eldritch-insignia`, serial
`14:C1:9F:D1:C8:10`.

Sources are cited by full path throughout, because three sessions have each meant
a different file by a short name.

---

## 0. Before you sit down

| | |
|---|---|
| Vault unlocked | `export BW_SESSION=$(bw unlock --raw)` — needed by `build-remote.sh` for any `wifi`/`radio` build |
| Env, every shell | `export PATH="$HOME/.cargo/bin:$PATH" && source ~/export-esp.sh` |
| Working dir | `cd /home/jp/Projects/smol/targets/s3-cyd/spike` |
| smol-d8 pinged | **only needed before §5 (M3)** — not for §2–§4 |

> **The two-disguise trap** (`spike/rust-toolchain.toml`): a missing environment
> impersonates a broken toolchain. `linker xtensa-esp32s3-elf-gcc not found` means
> you did not `source ~/export-esp.sh`. An error inside `xtensa_lx` usually means
> cargo silently fell back to `stable` because `rust-toolchain.toml` resolves **by
> directory** — `cd` in first, and confirm the output names the crate you meant.

---

## 1. Re-identification — before anything is written to anything

**Do this even though we "know" the serial.** The board has been off the bus; the
bus has changed; and the one thing this bench cannot recover from is writing to
the wrong device.

### 1.1 Passive bus-diff

Recipe from `spike/flash.sh`'s header. **Passive — `udevadm` only.** Do **not**
use `espflash board-info` to identify a port: it RESETS the target it probes, so
the act of identifying the bus reboots every board on it.

```bash
# board UNPLUGGED
for p in /dev/ttyACM* /dev/ttyUSB*; do [ -e "$p" ] || continue;
  udevadm info -q property -n "$p" | sed -n 's/^ID_SERIAL_SHORT=//p';
done | sort > /tmp/bus-before

# plug the board in, wait 2s
for p in /dev/ttyACM* /dev/ttyUSB*; do [ -e "$p" ] || continue;
  udevadm info -q property -n "$p" | sed -n 's/^ID_SERIAL_SHORT=//p';
done | sort > /tmp/bus-after

comm -13 /tmp/bus-before /tmp/bus-after
```

**PASS:** exactly one new line, and it is byte-exactly `14:C1:9F:D1:C8:10`.
**FAIL:** zero lines (not enumerated — cable/power), or more than one (something
else was plugged in too; unplug it and repeat).

### 1.2 ⛔ The reliquary near-miss, restated

```
target (node 162)   14:C1:9F:D1:C8:10   <- the only sanctioned target
reliquary (SEALED)  14:C1:9F:D1:C3:C8   <- NEVER WRITE TO THIS
                    ^^^^^^^^^^^^ FIRST FOUR OCTETS IDENTICAL
```

They differ in the last two octets and **both contain `C8`**. This is the most
confusable pair on the bench and one of them must never be written to.

**Verify with a comparison, not with your eyes:**

```bash
[ "$(comm -13 /tmp/bus-before /tmp/bus-after)" = "14:C1:9F:D1:C8:10" ] \
  && echo "MATCH — proceed" || echo "STOP — not the target"
```

⛔ **If the guard later says the serial is absent, the answer is NEVER to widen
the match to a prefix.** A prefix on this bench matches the sealed board. Find out
why it is absent (wrong board? re-enumerated? cable?). Widening the pattern until
it matches is the exact motion that destroys the vault unit, and it is what a
frustrated person reaches for at 1am. (`spike/flash.sh`, allow-list block.)

---

## 2. Flash 1 — the isolation flash (`SPIKE_HEAP_KB=64`)

**This flash answers the open M2 root-cause question, and it must go first**,
because the default 96 KiB build can only *hide* the answer.

### 2.1 What is being tested

M2's first flash OOM'd during the DHCP wait (`memory allocation of 96 bytes
failed`). Two changes were made together — the heap 64→96 KiB, and a continuous RX
drain replacing a 5% duty cycle. **The C5's counter-example differs from our
failing build in both variables at once, so neither datum isolates the cause**
(`spike/src/net.rs`, the heap block in `init`).

This build applies the cadence fix at the **original** heap.

### 2.2 Build

```bash
cd /home/jp/Projects/smol/targets/s3-cyd/spike
export BW_SESSION=$(bw unlock --raw)          # if not already set
SPIKE_HEAP_KB=64 ./build-remote.sh --features wifi
```

**Expected on stdout:**
```
build-remote: wifi creds loaded from vault (ssid jplovescl, psk 10 chars)
build-remote: mqtt creds staged for M4 (user jp, pass 10 chars — same value as PSK today)
   Compiling s3-cyd-spike v0.1.0 (/home/jp/builds/s3-cyd-spike)
    Finished `release` profile ...
ELF pulled: target/xtensa-esp32s3-none-elf/release/s3-cyd-spike
```

### 2.3 ⚠️ VERIFY THE 64 K ACTUALLY RODE ALONG — do not skip this

**This exact experiment was already broken once.** `build-remote.sh` did not
forward `SPIKE_HEAP_KB` over ssh: the variable was set in katana's shell, never
reached familiar's cargo, and the script cheerfully pulled a **default 96 KiB
image**, exit 0. Flashing it would have "proved" 64 KiB works using a binary that
was not 64 KiB — retiring a live hypothesis on fabricated evidence.

**The tell was a 0.14 s build where a recompile was due.** Exit codes report
whether the command succeeded, never whether it did the thing you meant. For an
experiment whose *configuration* is the variable, **the artifact hash is the only
honest instrument.**

```bash
E=target/xtensa-esp32s3-none-elf/release/s3-cyd-spike

SPIKE_HEAP_KB=64 ./build-remote.sh --features wifi >/dev/null && A=$(md5sum "$E" | cut -d' ' -f1)
./build-remote.sh --features wifi              >/dev/null && B=$(md5sum "$E" | cut -d' ' -f1)
SPIKE_HEAP_KB=64 ./build-remote.sh --features wifi >/dev/null && C=$(md5sum "$E" | cut -d' ' -f1)

echo "64:$A  96:$B  64:$C"
[ "$A" != "$B" ] && [ "$A" = "$C" ] && echo "KNOB REAL — flash \$A" || echo "STOP — knob not effective"
```

**PASS:** `A != B` **and** `A == C` (differs from default, and round-trips).
**FAIL:** any equality between A and B → the forwarding is broken again; fix
`build-remote.sh` before flashing, and do not interpret anything from this flash.

**The last build you run must be the 64 K one** — the ELF on disk is whatever the
last invocation produced, and `flash.sh` flashes that file.

### 2.4 Flash

```bash
cargo run --release --features wifi     # runner = ./flash.sh, the guard
```

**Expected guard output:**
```
[flash guard] /dev/ttyACM0     <whatever>
[flash guard] /dev/ttyACMn     14:C1:9F:D1:C8:10
[flash guard] OK: /dev/ttyACMn is 14:C1:9F:D1:C8:10 — flashing.
```
Any port showing a deny-listed serial is tagged `<- DENY-LISTED, never flash`.

**Independent second check:** espflash prints `MAC address:` before writing. It
must read `14:c1:9f:d1:c8:10`. **If it does not, pull the cable.**

### 2.5 Serial expectations, in order

| # | Expect | Meaning if absent |
|---|---|---|
| 1 | `=== M1 bring-up spike — ES3C28P, smol node id 162 ===` | not our image |
| 2 | `PSRAM: octal ok — 8388608 bytes (8 MiB) mapped at 0x…` | ⚠️ assert the **SIZE**; the base address is image-dependent (burrito-fw `0x3c020000`, this spike `0x3c060000`) — `BOARD.md` |
| 3 | `ILI9341V up: 320x240 landscape, MADCTL 0x28, inverted, NoResetPin` | display init failed |
| 4 | `backlight on — colour test painted` + bars on glass | see §5.3 |
| 5 | `esp-rtos scheduler started` → `radio up` | radio init |
| 6 | `joining "jplovescl" (psk 10 chars)` | creds missing from the build |
| 7 | `associated: ConnectedStationInfo { … channel: 1 … }` | see §7 |
| 8 | `DHCP lease 10.0.8.x/24 gw 10.0.8.1` | **the M2 question** |
| 9 | `[mqtt] connecting 10.0.8.111:1883` → `CONNACK rc=0 — session up` | see §3 / §7 |
| 10 | `[mqtt] retained discovery -> homeassistant/sensor/smol_162/telemetry/config (… B)` | |
| 11 | `heartbeat N — node 162 alive — up \| mqtt: up`, ~1 Hz | |

**Plausible BSSIDs at line 7** — `jplovescl` runs a **channel-1 policy across at
least two APs**: `9e:5c:8e:cb:db:90` (seen by this board at M2) and
`2c:56:dc:df:bd:a0` (seen by the C5). **Any BSSID is fine; `channel: 1` is the
expected value**, and a channel other than 1 is interesting news, not a failure —
record it, because it changes §5's premise.

### 2.6 The OOM verdict table — written before the evidence

Let the board run and read the verdict off it:

| Observation | Verdict | Consequence |
|---|---|---|
| **Lease + CONNACK, still heart-beating after ≥1 h** | **The drain cadence was the killer.** 96 KiB is margin, not correctness. | Record it. `SPIKE_HEAP_KB` stays as documentation of a settled question. |
| **Panics before the lease**, same `allocation of N bytes failed` | **The heap ceiling is real.** The RX pool genuinely does not fit in 64 KiB. | Default 96 KiB is load-bearing; §4 is then the *working* build, not a formality. |
| **Survives the lease, dies later** (minutes–hours) | **Neither, alone — a slow leak.** | Read the trend off the telemetry itself: `heap=<bytes>` in `smol/162/telemetry` (§3.2). A monotonically falling `heap=` is the leak; a flat one exonerates it. |
| **Never associates** | Nothing about the OOM is established. | §7.2. Do not record an OOM verdict from a board that never got to the DHCP wait. |

> **Note what the telemetry payload is for.** `up=<secs>s heap=<bytes>B beat=<n>`
> was chosen partly because **free heap is the direct readout of this exact
> question** (`spike/src/mqtt.rs`). The board reports its own verdict.

---

## 3. HA / wire verification for M4 — run FROM KATANA

### 3.1 ⚠️ Katana uses a different broker leg than the board

Mosquitto on the HA VM is **quad-homed and binds `0.0.0.0`, so every leg is the
same broker** — retention and topics are shared. But a cross-VLAN leg completes
the TCP handshake and then **silently drops the CONNACK**.

| who | leg |
|---|---|
| **the board** (VLAN8, `10.0.8.x`) | `10.0.8.111:1883` |
| **katana** (VLAN6) | **`10.0.6.108:1883`** — re-verified 2026-07-28 |

**Do not run the watcher against `10.0.8.111` from katana** — it will hang, and
you will misread it as the board failing to publish.
Source: `/home/jp/Projects/smol/ha/README.md:317-341`.

```bash
BROKER=10.0.6.108        # katana's leg. NOT the board's.
MQ_USER=jp
```

> ⚠️ `-P <password>` puts the secret in `ps` for every process on the box. On
> single-user katana that is an accepted exposure, but it is an exposure — prefer
> a short-lived shell and do not paste these into a shared log.

### 3.2 Retained-ghost hygiene — clear, THEN watch

**Retained MQTT defeated hardware verification four times in one night.** A
retained payload persists after the publisher dies, so *seeing the right value
proves nothing at all*. **Only a flip to a NEW value is trustworthy liveness;
persistence proves nothing.**

**Before flashing** (or before believing anything below), clear both topics:

```bash
# empty retained payload = delete the retained message
mosquitto_pub -h $BROKER -u $MQ_USER -P "$(bw get password 'Homelab jplovescl WiFi (jplovescl SSID)')" \
  -t 'homeassistant/sensor/smol_162/telemetry/config' -r -n
mosquitto_pub -h $BROKER -u $MQ_USER -P "$(bw get password 'Homelab jplovescl WiFi (jplovescl SSID)')" \
  -t 'smol/162/telemetry' -r -n
```

Then watch, **and only then flash**:

```bash
mosquitto_sub -h $BROKER -u $MQ_USER -P "$(bw get password 'Homelab jplovescl WiFi (jplovescl SSID)')" \
  -v -t 'homeassistant/sensor/smol_162/#' -t 'smol/162/telemetry'
```

**PASS criteria, all three:**

1. **Discovery arrives once, after the clear**, on
   `homeassistant/sensor/smol_162/telemetry/config`, containing
   `"unique_id":"smol_162_telemetry"`, `"expire_after":120`,
   `"identifiers":["smol_162"]`, `"model":"smol ESP32-S3 CYD"`.
2. **Telemetry arrives repeatedly on `smol/162/telemetry`, and the value CHANGES
   between messages** — `beat=` increments, `up=` climbs. A single message, or a
   repeated identical one, is a ghost, not a board.
3. Cadence ≈ **15 s** (`PUBLISH_EVERY_MS`, `spike/src/mqtt.rs`).

**FAIL:** discovery appears *immediately* on subscribe with no board running →
you did not clear, or the clear did not take. Re-run §3.2 from the top.

### 3.3 The HA device page

`http://ha.jphe.in` → Settings → Devices → **`smol 162 cyd`**

| field | expect |
|---|---|
| Model | **`smol ESP32-S3 CYD`** |
| Manufacturer | `jphein` |
| Entity | `Telemetry`, state = the bare payload line |
| Availability | goes *unavailable* ~120 s after the board stops (`expire_after`) |

⚠️ **The model string is hand-written and deliberately distinct** from the Ember
satellites' label, per **#396**'s interim rule — every S3 currently announces as
`"smol ESP32-S3 Ember"` because the BoardProfile arm has no variant axis yet.
**#396 owns the final string**; if HA shows the Ember label, the spike's constant
was overwritten by a profile lookup and that is a regression, not a fix.
(`spike/src/mqtt.rs`; `targets/s3-cyd/PORT-SCOPING.md:24`.)

---

## 4. Flash 2 — the default 96 K build

Only the deltas from §2; everything else is identical.

```bash
./build-remote.sh --features wifi          # no SPIKE_HEAP_KB
md5sum target/xtensa-esp32s3-none-elf/release/s3-cyd-spike   # must equal §2.3's B
cargo run --release --features wifi
```

**Quick checks:** §2.5 lines 1–11 again, and **one fresh telemetry message with a
`beat=` that restarts from 1** (proving you are looking at the new image and not
§2's retained ghost).

**PASS:** lease + CONNACK + telemetry, as §2.
**If §2 passed and §4 fails**, something other than the heap changed — suspect the
build, not the board.

---

## 5. The M3 window

### 5.1 GO protocol — do this BEFORE transmitting

**Ping smol-d8 immediately before the window opens.** Stray `SMOLv1 HELLO` frames
contaminate any #391 executor capture in flight; the C5's window was logged in
theirs. (`targets/s3-cyd/PORT-SCOPING.md:76-83`.)

Confirm with smol-d8, in one message:
1. **id50 is powered and audible** (`AC:A7:04:B9:77:14`) — it is the witness.
2. **No #391 capture is running**, or you have an agreed slot.
3. The window: **≤ 60 s**, duty **16 B HELLO 162, ch6, one frame per 2 s** (~30
   frames).

### 5.2 Build and flash

```bash
SPIKE_ESPNOW_ONLY=1 ./build-remote.sh --features radio
cargo run --release --features radio
```

**`SPIKE_ESPNOW_ONLY=1` is mandatory, not optional.** One radio, one channel: a STA
association owns the channel, the AP is on **ch1**, the mesh is on **ch6**. An
associated board **cannot hear the mesh at all**, and the probe would report a dead
mesh while working perfectly — the failure this fleet already misread once as a
coexistence/physics problem. This is an **SSID-wide policy across ≥2 APs**, so no
roam or retry will land the board on ch6.

**Expected serial:**
```
[net] ESPNOW-ONLY mode: skipping association, channel 6 will be pinned
[radio] channel pinned to 6 (mesh channel; AP is on ch1)
[radio] ESP-NOW ready — broadcasting 16 bytes every ~2 s, node 162
[radio] tx hello (16 B) -> broadcast
```

### 5.3 The three witness channels — what each should read

Witness is **id50**. Frames are sent **without** the #190 trailer; observe-mode
soft-accepts and *counts* them, which is itself the evidence.

| channel | expect | strength |
|---|---|---|
| `smol/50/peers` roster | a row for **162** appears within ~2 window periods | **the proof** — a flip to a NEW value |
| `mf=` MAC-observe counter | climbs by ≈ the number of frames sent (~30 in 60 s) | exact frame-count corroboration |
| mesh LED on id50 | activity during the window | human-visible, weakest |

```bash
mosquitto_sub -h $BROKER -u $MQ_USER -P "$(bw get password 'Homelab jplovescl WiFi (jplovescl SSID)')" \
  -v -t 'smol/50/peers' -t 'smol/50/telemetry'
```

**PASS:** roster gains 162 **and** `mf=` climbs by roughly the frames sent.
**PARTIAL:** `mf=` climbs, roster does not → frames reach the radio but are not
being adopted as a peer. Real signal; record it, do not call it failure.
**FAIL:** neither moves in a full window → §7.5.

**ACK expectations:** an ACK matches on the **14-byte prefix** `SMOLv1 ACK 162`;
the on-air frame is **23 bytes** (prefix + 9-byte #190 trailer). The firmware
prefix-matches deliberately — an equality test would never fire and would report a
healthy link as silent (`spike/src/espnow_probe.rs`). **Absence of an ACK is not
failure**: the roster flip is the proof, and nothing has promised id50 will reply.

### 5.4 Human checks — a person is present anyway

**Panel orientation** — the one unwitnessed display fact on this unit.
Procedure: the M1 colour test paints **R/G/B/W bars top→bottom with a magenta
border**. Glance at the glass.

| what you see | verdict | action |
|---|---|---|
| Red at top, border on all four edges | **correct**, MADCTL `0x28` | record it; clears `board_es3c28p.rs:217` |
| Upside down but bars in order | rotated 180° | `.flip_horizontal()` → `0xE8`. **Never re-add a mirror to fix a rotation.** |
| Mirrored | **should be unreachable** — the compile-time `MY == MX` assert rejects `0x68`/`0xA8` | do not "fix" it at the call site; the assert is wrong and that is the news |

**Four-corner touch tap** — settles the placeholder transform in
`board-staging/board_es3c28p.rs:302-311`. **Now runnable** (`spike/src/touch.rs`):

```bash
./build-remote.sh --features touch
cargo run --release --features touch          # no vault needed; touch is not wifi/radio
```

A `touch` build also paints a **16×16 orange dot at display (4,4) = logical
top-left**. That dot is the *frame-free anchor*: with a finger on the dot, one
glance settles orientation and transform together, with **no reference frame
agreed in advance**. Do the orientation eyeball and this tap in the same build.

**Procedure — ten seconds, once:**

1. Tap the **orange dot**. Expect roughly:
   ```
   [touch] #1 raw=(…,…) mapped=(~0,~0) [transform: PLACEHOLDER retro-go swap_xy=1 invert_x=0 invert_y=1]
   [touch]    corner guess: TOP-LEFT (screen is 320x240 landscape)
   ```
2. Tap the other three corners, clockwise. Four taps → four verbose lines.

| what the log says | verdict |
|---|---|
| every `corner guess` matches the corner you actually tapped | **transform CONFIRMED** — clears the PLACEHOLDER on all three constants |
| corners consistently swapped left↔right | `INVERT_X` is wrong |
| consistently swapped top↔bottom | `INVERT_Y` is wrong |
| x and y transposed | `SWAP_XY` is wrong |
| `I2C read FAILED` / no lines at all | §7.10 — a wedged bus, not an untouched screen |

**Record the four raw pairs verbatim.** They are the evidence; `mapped=` is the
*opinion under test*, which is why the line labels itself PLACEHOLDER. Being
capacitive there is no calibration span to measure — the transform is either right
or visibly wrong, so four taps settle it outright.

⛔ **GPIO18 stays unconfigured** — driving it breaks the FT6336, and its absence is
the trick, not an oversight.

---

## 6. Stack-paint soak → the ChipBudget row

### 6.1 ⚠️ LINK VERDICT FIRST — measured 2026-08-25, and it changes the plan

`BUDGET-PREP.md:165-195` sequences this as *"chip de-pin → `rust/clock` links for
esp32s3 → build `--features stack-paint`"*. **I probed that link. Results:**

| # | invocation | result |
|---|---|---|
| A | `stack-paint,espnow,cast,io` — as configured | ❌ **`rustc-LLVM ERROR: Incomplete scavenging after 2nd pass`** |
| B | canonical `fleet` tier, **no stack-paint** | ❌ **identical error** |
| C | fleet tier + `lto="thin"` | ❌ link fails: undefined `_stack_end_cpu0`, `Timer0`, `__stack_chk_guard` |
| D | fleet tier + `lto="thin"` + `-C link-arg=-Tlinkall.x` | ✅ **LINKS** |
| E | **`stack-paint` + thin + linkall** | ✅ **LINKS** — 2,191,980 B ELF |

**Three findings, in order of importance:**

1. **The blocker is NOT stack-paint.** B fails identically to A, so this is the S3
   build of `rust/clock` generally — an **LLVM Xtensa backend crash at fat-LTO
   codegen**, not a manifest, feature, or toolchain fact. `PORT-SCOPING.md:232`
   lists "remaining to `builds = true`" as a measured budget row + the partition
   table; **this is a third item that list does not mention.**
2. **`rust/clock/.cargo/config.toml`'s `[target.xtensa-esp32s3-none-elf]` block
   supplies only `force-frame-pointers` — no `-C link-arg=-Tlinkall.x`.** The riscv
   sections have it. That is C's failure, and it is a one-line config gap.
3. **With both mitigations the stack-paint tier links today** (E).

**Reproduce E:**
```bash
export PATH="$HOME/.cargo/bin:$PATH" && source ~/export-esp.sh
cd /home/jp/Projects/smol/rust/clock
export CARGO_TARGET_DIR=/var/tmp/s3-linkprobe3 CARGO_UNSTABLE_BUILD_STD=core,alloc
export CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS="-C force-frame-pointers -C link-arg=-Tlinkall.x"
cargo +esp build --release --target xtensa-esp32s3-none-elf \
  --config 'profile.release.lto="thin"' \
  --no-default-features --features esp32s3,stack-paint,espnow,cast,io -j2
```

> ⛔ **THE CAVEAT THAT GOVERNS EVERYTHING BELOW.** E is a **thin-LTO** binary; the
> fleet image is **fat-LTO**. Section sizes and stack high-water are properties of
> a *link*, and these are different links. **A stack floor measured on E is not the
> fleet image's stack floor.** It is a real number about a real binary and a
> genuine unblock for rehearsing the procedure — it is not the row. Recording it as
> the row would be exactly the fabrication `budget.rs` names: *"a guessed budget is
> worse than an absent one."*
>
> **Do not seed `budget.rs` from this, and do not seed it from the spike either**
> (`BUDGET-PREP.md:190-195`). The row lands when the fat-LTO link works.

### 6.2 Soak procedure

Only meaningful with the **radio up and associated**, under the heaviest duty the
board will really see. `budget.rs`: *"Re-derive this when the peak moves, with
`--features stack-paint` under live radio; **idle numbers are meaningless**."*

1. Flash the stack-paint build (guard as §2.4).
2. Radio up **and associated** — not espnow-only, not idle.
3. Run ≥ the C3's evidence bar: the C3's floor rests on **10/10 byte-identical
   high-water reports** (`budget.rs:170-206`). Fewer runs is a weaker number and
   must be recorded as such.
4. Read the reported high-water.

**The C6-style floor failure to watch for:** it does **not** present as a stack
overflow. The WiFi blob keeps its globals at the top of `.bss` directly under the
stack floor, so an overrun corrupts a blob pointer and you **fault inside the WiFi
RX path at connect** — reproducible, and pointing at entirely the wrong place. A
connect-time fault after adding statics is a stack symptom until proven otherwise.
(`spike/README.md`, "The stack floor"; c6-watch's history is the anti-lesson — a
*guessed* floor 15 KB below the real requirement, never firing while the fleet
crashed.)

### 6.3 The exact measurement commands

**Freeze the artifact first.** Every number comes off the frozen copy — not off a
path that a later build can silently replace (`BUDGET-PREP.md:56-74`, and its §4
anti-lesson).

```bash
cp /var/tmp/s3-linkprobe3/xtensa-esp32s3-none-elf/release/clock /tmp/frozen-s3.elf
sha256sum /tmp/frozen-s3.elf          # record this FIRST

readelf -SW /tmp/frozen-s3.elf        # host binutils is sufficient — sections are arch-neutral
espflash save-image --chip esp32s3 /tmp/frozen-s3.elf /tmp/frozen-s3.bin
# → "App/part. size:  N/M bytes"  ... take the LEFT number
```

`--flash-size` does **not** change the app size — only the denominator espflash
prints. Do not reach for it hoping to correct the number.

### 6.4 The recording template — all four fields, or none

⛔ **The poison-row rule (`BUDGET-PREP.md` §4, §5).** A partial row with one real
field and three placeholders makes `fits_flash` answer with fiction where
`UNMEASURED` currently answers an honest *no*. **All four land together, or none
do.** `app_slot_bytes` is settled **in the document** only because it is a
*declaration*, not a measurement.

```
### ChipBudget — esp32s3 (ES3C28P, node 162)
Measured:        <date>              Operator: <who>
ELF sha256:      <from §6.3>
Link profile:    <fat | THIN — and thin is NOT the fleet link, see §6.1>
Tier:            esp32s3,stack-paint,espnow,cast,io
Radio state:     associated to <ssid>, ch <n>, for <duration>
Runs:            <n>/<n> byte-identical high-water reports

free_dram_bytes      = ______   (readelf, method BUDGET-PREP §1.3)
baseline_image_bytes = ______   (espflash save-image, LEFT number, §1.4)
stack_floor_bytes    = ______   ( = ceil(4/3 × measured peak); peak = ______ )
app_slot_bytes       = 6_291_456   (0x600000 — DECLARED, PARTITIONS.md / §5)

Sanity bracket (§1.5 step 5): region size at which boot actually panics = ______
  → if the floor sits below that line, SAY SO in the doc comment rather than
    trusting the formula.
```

---

## 7. Abort criteria — what refuses, and what to do instead

**Two standing rules that override every row below:**
> ⛔ **Never widen a guard to make it match.**
> ⛔ **Never lower a floor to quiet a boot.**

| # | Symptom | Do NOT | Do |
|---|---|---|---|
| 7.1 | Guard: *"no port reports serial …"* | widen to a prefix — it matches the **sealed** reliquary board | re-run §1.1. Wrong board? re-enumerated? cable? |
| 7.1b | Guard: *"several ports report …"* | pick one | stop; that should be impossible, resolve by hand |
| 7.1c | espflash's `MAC address:` ≠ `14:c1:9f:d1:c8:10` | continue | **pull the cable** |
| 7.2 | Never associates | conclude anything about the OOM | check SSID/PSK baked in (line 6 of §2.5). Vault locked at build time → creds absent → the board says so |
| 7.3 | `NO CONNACK in 5000 ms, but TCP OPENED` | assume the broker is down | **wrong broker leg.** The board must use its own subnet's leg. Firmware prints the subnet→leg table |
| 7.4 | Boot assert: `stack gap … < STACK_FLOOR` | lower `STACK_FLOOR` | shrink a static or the heap. If growth is deliberate **and WiFi still joins reliably**, re-measure and re-pin under the new good value |
| 7.5 | M3: no witness channel moves | re-flash and retry blindly | confirm with smol-d8 that id50 is powered and on ch6; confirm `channel pinned to 6` appeared on serial; **check nothing else was transmitting** |
| 7.6 | M3: espnow-only build associates anyway | ignore it | `SPIKE_ESPNOW_ONLY` is `cfg!`-gated on `radio` — a `wifi`-only build ignores it by design. Confirm `--features radio` |
| 7.7 | Stack-paint won't link | force it with `-C opt-level` roulette | §6.1's mitigations are measured. Fat-LTO is an **upstream LLVM crash** — not something a bench session fixes |
| 7.8 | Any measurement is "surprisingly clean" | record it | **audit the instrument before the system.** Three verification attempts in this project's history returned flattering zeros from commands that were erroring |
| 7.10 | Touch: `I2C read FAILED`, or no `[touch]` lines at all | conclude the screen was not tapped | **a wedged bus and an untouched screen are different states and must not be confused** — that ambiguity cost burrito-fw a hardware window. The probe prints good-read counts alongside failures precisely so they are distinguishable. Check the chip-id line appeared at boot |
| 7.9 | An expected line is simply absent from serial | assume it did not happen | `ESP_LOG` is compile-time and release images are quieter than you expect. Confirm the line exists in the build you flashed before concluding the event did not occur |

---

## Appendix — one-screen command sequence

```bash
# 0. setup
export PATH="$HOME/.cargo/bin:$PATH" && source ~/export-esp.sh
export BW_SESSION=$(bw unlock --raw)
cd /home/jp/Projects/smol/targets/s3-cyd/spike

# 1. identify (passive)
for p in /dev/ttyACM*; do udevadm info -q property -n $p | sed -n 's/^ID_SERIAL_SHORT=//p'; done

# 2. isolation flash — verify the knob, THEN flash
SPIKE_HEAP_KB=64 ./build-remote.sh --features wifi
md5sum target/xtensa-esp32s3-none-elf/release/s3-cyd-spike     # compare per §2.3
cargo run --release --features wifi

# 3. watch from katana (NOTE: 10.0.6.108, not the board's leg)
mosquitto_sub -h 10.0.6.108 -u jp -P "$(bw get password 'Homelab jplovescl WiFi (jplovescl SSID)')" \
  -v -t 'homeassistant/sensor/smol_162/#' -t 'smol/162/telemetry'

# 4. default build
./build-remote.sh --features wifi && cargo run --release --features wifi

# 5. M3 — AFTER pinging smol-d8
SPIKE_ESPNOW_ONLY=1 ./build-remote.sh --features radio
cargo run --release --features radio

# 6. stack-paint link (see §6.1's caveat before recording anything)
cd /home/jp/Projects/smol/rust/clock
export CARGO_TARGET_DIR=/var/tmp/s3-linkprobe3 CARGO_UNSTABLE_BUILD_STD=core,alloc
export CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS="-C force-frame-pointers -C link-arg=-Tlinkall.x"
cargo +esp build --release --target xtensa-esp32s3-none-elf --config 'profile.release.lto="thin"' \
  --no-default-features --features esp32s3,stack-paint,espnow,cast,io -j2
```
