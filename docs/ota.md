# OTA firmware updates — operator guide (#6)

Update the fleet over the air instead of USB. You **stage** a signed image (a retained MQTT
line that *arms* every board's HA Update entity but fetches nothing); then an **install** —
HA's per-node Update button, or `install <id>` — tells **one** board to pull the staged image
into its inactive A/B slot, verify it (SHA-256 **and** an ed25519 signature), activate, reboot,
and self-test. **WiFi-capable boards fetch it themselves over HTTP; the WiFi-less mesh leaves
receive it relayed over ESP-NOW from the gateway (#40).** This is the *how-to-run-it* guide;
the design/rationale is in [home-assistant.md](home-assistant.md).

Verification legend: 🟢 hardware-verified · 🟡 works, not fully hardware-proven · ⚪ design.

> **Status: 🟢 PROVEN.** The engine, `ota_publish.sh`, and the HA panel are all landed, and a canary
> **self-updated build 58→59 over the air in ~17 s** — fetch → SHA-verify → boot `ota_1` → `Valid`. The
> first attempt had failed for an **infra** reason (a missing firewall allow-rule to reach the image host);
> once that rule was added, the end-to-end update succeeded ([#37](https://github.com/jphein/smol/issues/37)
> resolved). This is now a production-proven flow — still **canary one board at a time** (see the one rule below).
> **Leaf mesh-OTA (#40) is also landed + hardware-proven** — the gateway relays a signed image over
> ESP-NOW to a WiFi-less leaf that verifies the signature before flashing (full ~1 MB images delivered
> over the mesh). See [Leaf mesh-OTA](#leaf-mesh-ota--updating-esp-now-only-leaves-40) below.

## ⚠️ The one rule: CANARY, one board at a time

**Never blind-push the whole fleet.** A *broken* app cannot always roll itself back —
app-side rollback (below) covers a boots-but-unhealthy image, but a hard panic/boot-loop can
only be recovered by the 2nd-stage bootloader, whose **revert-on-boot-fail is OFF / unproven
on hardware** (ROADMAP D2). So the mass-brick defense is structural: **install to ONE board,
confirm it comes back healthy, then install the next.** The tooling enforces this — `install`
is per-device (there is **no fleet-fetch topic**), and the HA panel drives it one board at a
time via each node's native Update button.

## Publish tool — `tools/ota_publish.sh`

Server-side pipeline: build (or take) an esp-image, host it on the LAN image server, **sign it**,
and publish the retained **staged** line every board's native HA Update entity reads.

```
tools/ota_publish.sh stage    [<commit>] [--bin <file>] [--build N]   # build+host+sign+publish smol/ota/staged (ARMS every board's Update entity; NO board fetches)
tools/ota_publish.sh install <id>                                     # publish retained INSTALL → smol/<id>/ota/install (per-node canary; the HA Update button is the GUI path)
```

- `<commit>` defaults to `HEAD`; `--bin <file>` hosts an existing `.bin` and skips the build;
  `--build N` overrides the git-derived build number (to canary an uncommitted image).
- **Canary is STRUCTURAL.** `install` is per-device (it mirrors HA's native Update button) and
  there is **no fleet-fetch topic** — the old per-id / `push all` announce act-path is retired
  (Model-A #32 closure). Install one board, confirm its version advances, then the next; never
  script all three at once while bootloader revert-on-boot-fail is unproven.
- `install` is retained + **idempotent** — the firmware gate is `staged.build > running`, so a
  re-fire never re-installs the same build.
- Broker creds are sourced from the Mosquitto addon option and **never printed**.
- The script **signs** the manifest with the offline ed25519 key and hard-gates image
  `size ≤ 0x1F0000` (the slot size) before publishing.

> **Public-repo config:** the infra constants (image host, broker, SSH host, addon, MQTT user)
> are **non-real placeholders** — copy `tools/ota_publish.env.example` → the git-ignored
> `tools/ota_publish.env` and set your own (or override any per run via env, all are
> `${VAR:-default}`), then e.g. `tools/ota_publish.sh stage`.

## Canary an OTA — end to end

1. **Build + stage the image.**
   ```
   tools/ota_publish.sh stage
   ```
   Builds the current commit, hosts `smol-<build>.bin` on the image server, and publishes the
   target to `smol/ota/staged` (a **non-acted** topic — no board updates yet; the HA panel
   mirrors it so it can canary without re-hashing).

2. **Install to the canary board** — HA **or** CLI:
   - HA: the board's **native Update entity** → **Install** (the GUI canary path).
   - CLI: `tools/ota_publish.sh install 7` (publishes retained `smol/7/ota/install`).

3. **Watch it update.** **On its next burst** the board sees the install, checks the staged image
   (newer build, host allowed, size OK) and runs the update — **the mesh is deaf for the whole download**
   (longer than a normal burst; a proven canary self-updated build 58→59 in **~17 s**). Canary one board
   at a time. Watch the gateway's serial:
   ```
   smol OTA: opening update burst (mesh deaf for the whole download)
   smol OTA: image verified — activating new slot, rebooting
   ```
   (A long-press at the glass **aborts** mid-download — `aborted by long-press (slot
   untouched)` — the board stays on the current image.)

4. **Confirm it came back healthy.** After the reboot the new image self-tests on first boot,
   then confirms otadata or app-side rolls back. The self-test is **role-aware**: a **WiFi board**
   (the gateway) confirms by reaching DHCP; a **mesh leaf** confirms by *hearing a mesh frame* —
   it has no WiFi, so DHCP would never pass. Success looks like:
   ```
   smol OTA: unconfirmed image on boot — running self-test (bootloader auto-revert false)
   smol OTA: self-test PASS — image CONFIRMED (Valid)
   ```
   and the **boot splash shows the new sigil version name**. A failure logs
   `self-test FAIL — ROLLING BACK to the previous slot` and the board returns to the old
   image on its own. (A USB/JTAG reflash or a plain power-cycle does **not** self-test — the
   marker is build#-tagged, so only a genuine fresh OTA-activate arms it.)
   > 🟡 **The HA panel's "running build" readout is not live yet.** It needs the firmware to
   > publish `smol/<id>/status` (design F4), which hasn't shipped — until then the panel shows
   > **"unknown"** and the "roll out to rest" button stays **inert (safe)**. **Confirm the
   > canary via serial + the boot-splash version name**, not the panel number, for now.

5. **Roll out to the rest — one at a time.** Only after the canary is confirmed healthy:
   `tools/ota_publish.sh install 8`, then `install 9` (or each board's HA Update button). Do **not**
   install all three at once while bootloader-revert is unproven.

6. **Re-stage to abort.** There is no separate clear step — `install` is idempotent (gated on
   `staged.build > running`), so a board never re-installs the build it's already on. To cancel a
   pending rollout, simply don't fire the remaining installs; to supersede, `stage` a newer build.

### Ground truth during a roll — trust sources in this order

When a roll looks stuck, **which signal you believe decides whether you diagnose the real fault
or chase a phantom.** Trust in this order:

1. **`pcap` on the image host** (`tcpdump` on the HTTP fetch port) — **decisive.** It shows
   whether bytes actually flowed *and were ACKed*. The coexist-bulk-OTA disease is invisible to
   every higher-level log because it's an asymmetric RX-deafness: the board's SYN/ACK/GET all
   reach the server, but the board **never ACKs a single response byte** — only the packet
   capture reveals "server sending into a void." A pcap is what caught it at the packet level
   (see [Leaf mesh-OTA](#leaf-mesh-ota--updating-esp-now-only-leaves-40) / #204).
2. **An MQTT topic flipping to a NEW value** — trustworthy *as a transition*, not as a state.
   Retained topics are ghosts: a persisted old value proves nothing (it survives reboots and
   re-subscribes). Clear-then-watch, and believe the **flip** to a fresh value (e.g. a build#
   or progress% advancing), never the mere presence of a value.
3. **`rangeserver.log`** (the image host's HTTP access log) — **last, and with suspicion.** It is
   **block-buffered and manually tailed**, so a served GET may not appear for a while and a
   stale read can show an old request as if it were current. Reading it as live truth **misled
   two separate sessions** into misdiagnosing a fetch that the pcap showed was already dead.

Rule of thumb: **packets > transitions > logs.** If the pcap and the log disagree, the pcap is
right.

## What's on the wire

Retained MQTT, pipe-delimited (the board reuses its `split('|')` parser):

```
topics:   smol/ota/staged            (retained; ARMS every board's HA Update entity — NO fetch)
          smol/<id>/ota/install      (retained INSTALL; only that board fetches the staged image)
staged payload:   OTA|<build>|<size>|<sha256hex>|<sighex>|<url>
example:          OTA|52|590304|6122578e…60ea|<128-hex ed25519 sig>|http://<image-host>:8080/ota/smol-52.bin
install payload:  INSTALL
```

- `build` — decimal `BUILD_NUMBER`; a board acts **iff `build > its running build`** (this
  monotonicity check blocks both downgrades and retained replay loops).
- `size` — image bytes; bounds-checked (`≤ 0x1F0000`) and cross-checked against HTTP `Content-Length`.
- `sha256` — 64 lowercase hex over the exact `.bin`; the **integrity** gate.
- `sighex` — 128 hex; the **Ed25519 signature** over the manifest `M = "build|size|sha256hex"`
  (fields 1-3 + their two `|`); the **authenticity** gate (see Security posture below).
- `url` — HTTP (no TLS in `no_std`); it's the **last** field so it may contain no `|`.

## Leaf mesh-OTA — updating ESP-NOW-only leaves (#40)

The mesh leaves have no WiFi/MQTT, so the elected **gateway is their OTA proxy.** The flow is
**canary-one-leaf** — exactly one leaf MAC is ever targeted; there is never a broadcast image push:

1. `stage` the fleet-shared, ed25519-signed image (arms every board).
2. `install <leaf-id>` → the gateway **fetches the staged image into its own inactive slot**
   (fetch only — it does not activate), then relays it to that one leaf over ESP-NOW.
3. The relay is chunked with a **windowed NAK**: 231-byte chunks, 64 chunks per window; the leaf
   returns a per-window missing-bitmap and the gateway retransmits only the gaps. An all-zero
   bitmap = "window complete, advance" (the only positive ack). To stay co-channel, the leaf
   **holds ch6 through the gateway's fetch** (the gateway briefly goes off-channel to pull the
   image over WiFi — the leaf treats that silence as "fetching", not "gateway dead").
4. The leaf **verifies the Ed25519 signature before any flash write**, reassembles into its
   inactive slot (every chunk bounds-checked against the *signed* size, and the writer is
   partition-scoped so an out-of-range offset physically cannot reach the active slot or
   `otadata`), and activates only on a full-size + readback-SHA match.
5. On reboot the leaf self-tests by **hearing a mesh frame** (not DHCP) → confirms `otadata` or
   app-side rolls back. A signed-freshness floor (NVS) + build monotonicity block downgrade/replay.
6. Ordering: while a leaf relay is in flight the **gateway suppresses its own self-OTA** (leaves
   update first, the gateway last), so a relay is never cut short by the gateway rebooting.

Diagnostics ride retained topics — `smol/<leaf>/ota/diag` (relay phase) and
`smol/<leaf>/ota/relaydiag` (headless relay-progress %). The last window is expected to finish
**without** an advance-ack (the leaf finalizes and reboots), so the gateway treats last-window
exhaustion as a **confirm**, not a failure.

> **Why relay over ESP-NOW and not an off-the-shelf mesh framework?** The reference
> architectures (Espressif's WiFi-tree esp-mesh-lite, the `esp-now` OTA example, and
> Thread/Matter OTA on the C6) were surveyed against smol's flood+crown model in
> [reliable-mesh-ota-architectures.md](superpowers/research/reliable-mesh-ota-architectures.md)
> (#54). Verdict: smol's leaf relay **is** the ESP-NOW-native pattern; esp-mesh-lite would force
> every headless leaf onto WiFi, and Thread needs an 802.15.4 radio the C3 doesn't have. The one
> worthwhile extension — an already-updated node sourcing the next over ESP-NOW to retire the
> gateway's WiFi-fetch window — is tracked separately (gated on the esp-radio-0.18 coex
> root-cause finding, #198/#204).

## Reproducible builds — verify image ↔ commit before/after a flash (#44)

The release image is **byte-reproducible for a fixed commit**: build it on any machine and
you get the same `.bin`, hence the same `sha256`. Three things make it deterministic — the
version stamp is pinned from the commit (`SMOL_GIT_HASH`/`SMOL_BUILD_NUMBER`, the build.rs
deploy contract); absolute build paths are canonicalised with `--remap-path-prefix`
(dependency + `build-std` `file!()` strings would otherwise embed `$CARGO_HOME`/the rustc
sysroot and differ per machine); and `SOURCE_DATE_EPOCH` is pinned to the commit's own time
(esp-bootloader-esp-idf otherwise stamps the app descriptor from the wall clock). The
mechanism lives in `tools/repro_build.sh`; a plain `cargo build` is untouched (all of it is
applied only by the release/verify tooling).

So the announced `sha256` is a stable **identity** you can check against what's actually on a
board — which is exactly what would have caught the dup-`NODE_ID` outage (#42): the wrong
image flashed to id8/id9 could not be detected by an image↔board hash check because the build
wasn't reproducible.

```
tools/verify_image.sh <commit>                 # build → prints  build size sha256
tools/verify_image.sh <commit> --expect <sha>  # exit 0 if the image is that commit, 3 if not
tools/verify_image.sh <commit> --twice         # proof: two isolated builds → identical sha
tools/verify_image.sh --bin <file>             # just hash an existing .bin (no build)
```

`ota_publish.sh stage` builds through the same reproducible path, so the sha it announces is
the sha `verify_image.sh` reproduces. Since #40's runtime-NVS node-id, one image serves the
whole fleet (the OTA image carries no `SMOL_NODE_ID`; each board reads its id from `nvs` at
runtime) — so there's **one sha per commit**, no board dimension. `verify_image.sh <commit>`
with no `--node-id` reproduces that fleet sha; `--node-id N` is only for a USB **factory**
image built with `SMOL_NODE_ID=N`.

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
the image — the board is back. **Identity is runtime, from NVS:** the baked `board.rs` `NODE_ID`
is only a first-boot **seed** (written to the `nvs` partition on the first USB boot after an
erase-flash); thereafter the board reads its id from NVS. Because OTA writes only the inactive
app slot + `otadata` and **never** touches `nvs`, identity survives any image — so a single
fleet-shared OTA image (built with no `SMOL_NODE_ID`) installs onto id7/id8/id9/… and each keeps
its own id. `board.rs`/`secrets.rs` still hold the per-board factory seed + creds (see
[BUILDING.md](BUILDING.md)); do **not** USB-flash the fleet-staged `.bin` as a factory image
without `SMOL_NODE_ID=<n>`, or a fresh (erased) board seeds NVS to the default id 7.

**Leaves can now be updated over the mesh** — see [Leaf mesh-OTA](#leaf-mesh-ota--updating-esp-now-only-leaves-40)
below. USB stays the recovery path for a hard brick (case 3), not the only update path.

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

Integrity **and** authenticity. The announced SHA-256 proves **integrity** (the bytes are what
the manifest claims), checked before `otadata` is touched; the bootloader re-checks the esp-image
hash at boot. **Authenticity is enforced by Ed25519 (#32):** `ota_publish.sh stage` signs the
manifest `M = "build|size|sha256hex"` with an **offline** key (kept out of the repo, never on
disk in source); the firmware carries the matching **public** verify key baked in as its
root-of-trust. Signing `M` — not the bare digest — binds `build` into the signature, blocking a
rollback/mislabel replay. Over the **unauthenticated ESP-NOW mesh** this is load-bearing: a leaf
**verifies the signature before it flashes a single byte** (see Leaf mesh-OTA below). A URL-host
allowlist stays as defence-in-depth. Do not treat `sha256` alone as trust.

## Status

🟢 **Canary OTA is PROVEN; fleet-unison stays off.** Engine + `ota_publish.sh` + HA panel are landed, and
a canary **self-updated build 58→59 over the air in ~17 s** — fetch → SHA-verify → activate → boot `ota_1`
→ `Valid`. The first attempt had failed for an **infra** reason (a missing firewall allow-rule to reach the
image host, since added; [#37](https://github.com/jphein/smol/issues/37) resolved) — **not a firmware bug.**
**Bootloader revert-on-boot-fail is still unproven → canary-one-board-at-a-time remains the mass-brick
defense; never install the whole fleet at once.** The HA "running build" / rollout gate awaits the firmware `smol/<id>/status`
publish (F4) — until then confirm canaries by serial + boot-splash version name. Issue #6 (#37 resolved).
