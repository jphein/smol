# OTA firmware updates — operator guide (#6)

Update the fleet over WiFi instead of USB. You publish a retained MQTT **announce**; each
board picks it up **on its next burst**, and — if it's for that board and newer — **auto-fetches**
the image over HTTP into its inactive A/B flash slot, verifies it, activates, reboots, and
self-tests. This is the *how-to-run-it* guide; the design/rationale is in
[home-assistant.md](home-assistant.md) and `scratch/smol-ha-batt/ota-plan.md`.

Verification legend: 🟢 hardware-verified · 🟡 works, not fully hardware-proven · ⚪ design.

> **Status: 🟢 PROVEN.** The engine, `ota_publish.sh`, and the HA panel are all landed, and a canary
> **self-updated build 58→59 over the air in ~17 s** — fetch → SHA-verify → boot `ota_1` → `Valid`. The
> first attempt had failed for an **infra** reason (a missing firewall allow-rule to reach the image host);
> once that rule was added, the end-to-end update succeeded ([#37](https://github.com/jphein/smol/issues/37)
> resolved). This is now a production-proven flow — still **canary one board at a time** (see the one rule below).

## ⚠️ The one rule: CANARY, one board at a time

**Never blind-push the whole fleet.** A *broken* app cannot always roll itself back —
app-side rollback (below) covers a boots-but-unhealthy image, but a hard panic/boot-loop can
only be recovered by the 2nd-stage bootloader, whose **revert-on-boot-fail is OFF / unproven
on hardware** (ROADMAP D2). So the mass-brick defense is procedural: **push to ONE board,
confirm it comes back healthy, then push the next.** The tooling enforces this — the CLI's
`push <id>` is frictionless while `push all` is seat-belted, and the HA panel has **no
"push all" button at all**.

## Publish tool — `tools/ota_publish.sh`

Server-side pipeline: build (or take) an esp-image, host it on the LAN image server, and
publish the retained announce the boards fetch from.

```
tools/ota_publish.sh stage      [<commit>] [--bin <file>]   # build+host+publish smol/ota/staged (NO board acts)
tools/ota_publish.sh push <id>  [<commit>] [--bin <file>]   # stage, then announce to ONE board (CANARY)
tools/ota_publish.sh push all   [<commit>] [--bin <file>] [--force]   # FLEET — seat-belted (see below)
tools/ota_publish.sh clear <id|all>                         # retain-delete the announce (abort)
```

- `<commit>` defaults to `HEAD`; `--bin <file>` hosts an existing `.bin` and skips the build.
- **`push <id>` is the canary path** — never gated, run it freely.
- **`push all` is the seatbelt:** without `--force` it makes you type the exact staged build
  number to confirm and refuses if stdin isn't a TTY; `--force` (scripted use only) skips that.
- Broker creds are sourced from the Mosquitto addon option and **never printed**.
- The script hard-gates image `size ≤ 0x1F0000` (the slot size) before publishing.

> **Public-repo config:** the infra constants in the script (image host, broker, SSH host,
> addon, MQTT user) are **non-real placeholders** — set them to your own, or override any per
> run via env (all are `${VAR:-default}`), e.g.
> `OTA_HOST_IP=… BROKER=… OTA_HOST_SSH=… tools/ota_publish.sh push 7`.

## Canary an OTA — end to end

1. **Build + stage the image.**
   ```
   tools/ota_publish.sh stage
   ```
   Builds the current commit, hosts `smol-<build>.bin` on the image server, and publishes the
   target to `smol/ota/staged` (a **non-acted** topic — no board updates yet; the HA panel
   mirrors it so it can canary without re-hashing).

2. **Push to the canary board** — CLI **or** HA panel:
   - CLI: `tools/ota_publish.sh push 7` (does stage+announce in one; publishes `smol/ota/announce/7`).
   - HA: **smol · OTA push** panel → **"Push staged → id7 (canary)"**.

3. **Watch it update.** **On its next burst** the board sees the gated announce (newer build,
   host allowed, size OK) and runs the update — **the mesh is deaf for the whole download**
   (longer than a normal burst; a proven canary self-updated build 58→59 in **~17 s**). Canary one board
   at a time. Watch the gateway's serial:
   ```
   smol OTA: opening update burst (mesh deaf for the whole download)
   smol OTA: image verified — activating new slot, rebooting
   ```
   (A long-press at the glass **aborts** mid-download — `aborted by long-press (slot
   untouched)` — the board stays on the current image.)

4. **Confirm it came back healthy.** After the reboot the new image self-tests on first boot
   (it must reach DHCP). Success looks like:
   ```
   smol OTA: unconfirmed image on boot — running self-test (bootloader auto-revert false)
   smol OTA: self-test PASS — image CONFIRMED (Valid)
   ```
   and the **boot splash shows the new sigil version name**. A failure logs
   `self-test FAIL — ROLLING BACK to the previous slot` and the board returns to the old
   image on its own.
   > 🟡 **The HA panel's "running build" readout is not live yet.** It needs the firmware to
   > publish `smol/<id>/status` (design F4), which hasn't shipped — until then the panel shows
   > **"unknown"** and the "roll out to rest" button stays **inert (safe)**. **Confirm the
   > canary via serial + the boot-splash version name**, not the panel number, for now.

5. **Roll out to the rest — one at a time.** Only after the canary is confirmed healthy:
   `tools/ota_publish.sh push 8`, then `push 9` (or the panel's per-node buttons). Do **not**
   `push all` while bootloader-revert is unproven.

6. **Abort / clean up.** `tools/ota_publish.sh clear <id|all>` (or the panel's **"Clear all
   announces (abort)"**) retain-deletes the announce so no board re-acts on it.

## The announce (what's on the wire)

Retained MQTT, pipe-delimited (the board reuses its `split('|')` parser):

```
topic:    smol/ota/announce/<id>    (per-id, the canary path)
          smol/ota/announce/all     (fleet — seat-belted; the panel never writes this)
          smol/ota/staged           (staging mirror for the HA panel — NON-acted)
payload:   OTA|<build>|<size>|<sha256hex>|<url>
example:   OTA|52|590304|6122578e…60ea|http://<image-host>:8080/ota/smol-52.bin
```

- `build` — decimal `BUILD_NUMBER`; a board acts **iff `build > its running build`** (this
  monotonicity check blocks both downgrades and retained-announce replay loops).
- `size` — image bytes; bounds-checked (`≤ 0x1F0000`) and cross-checked against HTTP
  `Content-Length`.
- `sha256` — 64 lowercase hex over the exact `.bin`; the integrity gate.
- `url` — HTTP (no TLS in `no_std`); it's the **last** field so it may contain no `|`.

## How recovery works (why canary is enough)

Three layers, in order of how bad the image is:

1. **Corrupt/truncated download** → the running SHA-256 is checked **before otadata is ever
   touched**; a bad image is discarded with the good slot still active. 🟢 safe by construction.
2. **Boots but unhealthy** (e.g. can't reach the network) → the still-running new image
   **self-tests on first boot and flips otadata back to the previous slot itself** (MF-1,
   app-side). Works **even with the bootloader's own rollback disabled** — this is the
   primary net. 🟡 code landed; exercised via canary.
3. **Panics / boot-loops before the self-test** → only the 2nd-stage bootloader can revert,
   and that's **OFF / unproven** (🔴). A panic is forced to *reset* (not hang) so it at least
   re-enters the bootloader, but the real defense is that **only one board was ever at risk** —
   that's the whole point of canary.

## If a board bricks — USB recovery

A board that won't come back (case 3 above) is recovered over USB, exactly like a first
flash. From `rust/clock/` with the board on USB:

```
# build + flash a known-good image WITH the OTA partition table (the cargo runner already
# passes --partition-table partitions-ota.csv):
cargo run --release --features espnow

# …or flash a prebuilt image directly:
espflash flash --monitor --partition-table partitions-ota.csv <known-good.bin> --port /dev/ttyACM0
```

This rewrites the partition table + a blank `otadata` (so the bootloader boots `ota_0`) plus
the image — the board is back. Each board's identity (`NODE_ID`, secrets) comes from its
git-ignored `board.rs`/`secrets.rs`, so flash from that board's own config (see
[BUILDING.md](BUILDING.md)). Credential-less leaves can't OTA at all (the image never crosses
ESP-NOW) — they're USB-only by nature.

## Partition layout (fixed — don't "tidy")

`rust/clock/partitions-ota.csv`, hardware-validated:

```
otadata,  data, ota,   0xf000,   0x2000     # MUST be exactly 0x2000
ota_0,    app,  ota_0, 0x20000,  0x1F0000   # 1.938 MB slot
ota_1,    app,  ota_1, 0x210000, 0x1F0000   # 1.938 MB slot
```

Two ~1.94 MB slots vs a ~590 KB image = ~3.3× headroom. The bundled espflash ESP-IDF v5.1.2
bootloader honors otadata slot-select (proven on hardware). `otadata` must be exactly `0x2000`
or slot-select fails to initialize.

## Security posture (honest)

Plain HTTP + double SHA-256 (announced hash checked before activate; the bootloader's own
esp-image hash at boot) give **integrity, not authenticity** — whoever can publish the
announce controls both the URL and the hash. v1 accepts this because **OTA authority == MQTT
broker write access**, and the broker sits on a trusted LAN; a URL-host allowlist is
defence-in-depth, not authentication. Do not treat `sha256` as trust. Documented upgrade path
(not v1): ed25519 image signing verified before activate.

## Status

🟢 **Canary OTA is PROVEN; fleet-unison stays off.** Engine + `ota_publish.sh` + HA panel are landed, and
a canary **self-updated build 58→59 over the air in ~17 s** — fetch → SHA-verify → activate → boot `ota_1`
→ `Valid`. The first attempt had failed for an **infra** reason (a missing firewall allow-rule to reach the
image host, since added; [#37](https://github.com/jphein/smol/issues/37) resolved) — **not a firmware bug.**
**Bootloader revert-on-boot-fail is still unproven → canary-one-board-at-a-time remains the mass-brick
defense; never `push all` blind.** The HA "running build" / rollout gate awaits the firmware `smol/<id>/status`
publish (F4) — until then confirm canaries by serial + boot-splash version name. Issue #6 (#37 resolved).
