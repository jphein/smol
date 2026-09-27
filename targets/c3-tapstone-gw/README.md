# c3-tapstone-gw — the Tapstone arena's USB-serial gateway

One ESP32-C3 on the arena laptop's USB. It is an ordinary mesh node with **WiFi off**, and it
bridges the laptop's `tapstone-arena` to the mesh: every received `SMOLv1 MATCH ` frame goes up the
USB as an `@TS1 RX` line, and every `@TS1 TX` line from the laptop goes out as a frame. smol#548,
part of tapstone#132. The host side is tapstone's `rust/tapstone-arena/src/link/{lines,serial}.rs`.

**Status: 🟡 compile- and host-verified, not hardware-verified.** The `tapstone-gw` tier builds and
passes `clippy -D warnings` in `tools/gate.sh`; the line codec is checked on the host against pinned
goldens and against tapstone's own parser. No board has run it yet: the bench below is the
acceptance test, and it needs hands.

- **Chip**: ESP32-C3 · **firmware**: `rust/clock`, tier `tapstone-gw` = `tapstone-gw` + the fleet
  features (`tools/build-matrix.toml`) · **download**: none (`target.toml` says why)
- **Code**: `rust/clock/src/ts_gw.rs` (USB + main-loop driver), `rust/clock/src/net/ts_lines.rs`
  (the line codec), and the `#548` sites in `rust/clock/src/net/mode.rs`
- **Checks**: `experiments/tapstone_gw_verify` (in the gate), `experiments/tapstone_lines_xcheck`
  (needs a tapstone checkout; run by hand), `bench_selftest.py` (the bench script, without boards)

## The wire

The arena spec's D6 table, and nothing else:

| direction | line | when |
|---|---|---|
| gateway → arena | `@TS1 HELLO <mac> <node_id> <fw_hash8> <group_epoch>` | at boot, and on every `PING` |
| gateway → arena | `@TS1 RX <src_id> <rssi> <mac_ok 0\|1> <hex>` | each received MATCH frame, trailer stripped |
| gateway → arena | `@TS1 TXOK <tx_id>` / `@TS1 TXERR <tx_id> <reason>` | each `TX` |
| gateway → arena | `@TS1 ROSTER <id>:<mac>:<rssi>,…` | every 2 s, when at least one peer is known |
| arena → gateway | `@TS1 TX <tx_id> <dst_id\|255> <hex>` | send one MATCH frame (255 = broadcast) |
| arena → gateway | `@TS1 PING` | liveness |

Every other line on the port is one of smol's ordinary logs, which keep printing; the arena ignores
anything without the prefix. Numbers are decimal (the epoch too: the arena reads it as a `u8`), hex is
lowercase on the way out and either case on the way in, fields are separated by exactly one space.

`TXERR` reasons, one token each: `bad-dst`, `bad-hex`, `too-long` (a frame over 241 B, or a line over
the 505 B reader), `not-match` (the gateway sends MATCH frames only), `short` (under the 20 B
header), `extra-field`, `unknown-dst` (no HELLO heard from that id yet, so no MAC), `self-dst`,
`send-failed` (the driver refused it), `no-radio`. A `TX` whose id does not parse cannot be answered
and is logged instead.

## Design

**Why the C3.** It is `canonical_chip` in `tools/build-matrix.toml` and the only chip with
`builds = true`, so this tier is compiled and linted on every gate run, on CI as well as locally. The
S3 would work the same way (same USB-Serial-JTAG peripheral), but CI cannot build Xtensa yet, and the
S3 is the shrine board, which already has a job at the table. A bare `c3` supermini costs about a
dollar and needs nothing but its USB-C port.

**A build flavor, not an `AppKind`.** In smol an "app" is a screen behind the BOOT menu. The gateway
is a change of *role*: it has to skip the WiFi burst that runs at boot, before any app exists, and a
screen cannot do that. So it is a cargo feature plus this target folder, and it needs no menu slot.
(The shrine's own screen, `AppKind::Tapstone`, is smol#543's.)

**WiFi off, never the crown.** A crown's WiFi bursts deafen it for up to ~15 s (protocol draft §1),
and the crown is elected over the MQTT broker, so a node that never associates can never be crowned.
The gateway never associates. The guards, all marked `#548` in `net/mode.rs`:
1. `start()` skips the boot burst (no association, DHCP, SNTP or broker election) and boots as a leaf;
2. `maybe_leaf_reelect()` never runs a recovery burst;
3. `set_debug_wifi_all()` ignores a relayed CFG `A=1`;
4. `switch(Mode::WifiSta)` refuses: the backstop for any path the three above miss.
`ts_gw` also checks the role every 2 s and logs an error if the node ever reports itself crown.

**Finding the mesh channel.** A leaf normally locks onto its elected owner's HELLO, but the gateway
never reads the broker, so it has no owner (its `elected_owner` is its own id). Instead it locks onto
**any** peer HELLO, since every member rides the crown's channel, and the ordinary 6 s silence unlock
re-scans if HELLOs stop. A scanning leaf passing through the wrong channel can lock it briefly; that
leaf dwells 1.5 s and moves on, so the lock lapses and the gateway settles where HELLOs stay. To skip
the scan entirely, build with `SMOL_TS_CHANNEL=<1..13>`: the gateway sits on that channel and never
hops. That suits a table with no crown, or a bench.

**RX.** In `RadioManager::service()`, after the #190 verify (so `mf=`/`mfk=` stay whole, and enforce
mode, once it's on, drops a bad frame here as it does for the fleet) and before `parse_frame`, which
has no MATCH arm yet. The line is written right there, with no queue. `mac_ok` is 1 only for a
verified trailer. The trailer is stripped when it verified *and* when it failed (`BadTag`: the frame
claimed our epoch, so a trailer is there with the wrong key); a frame with no recognisable trailer is
forwarded whole (`ts_lines::forward`, tested against the real `wire.rs`). `<src_id>` is the
*link-layer* sender, as the arena expects: the roster's id for the sender's MAC, or the header's
`src` byte if the roster has no id for it yet. The header's claim never enters the roster, because
unicast ids are resolved against the roster.

**Latency.** The main loop ticks every 20 ms, and that period cannot shrink: its edge detectors look
back exactly one `SUBTICK_MS`. So the gateway keeps the 20 ms tick but spends it in four 5 ms slices,
draining the radio and the USB between them. A frame waits at most one slice before its line is
written, against the spec's 20 ms end-to-end budget.

**TX.** `@TS1 TX` is parsed as strictly as the arena's own parser would read it, then sent through
`send_to`, the choke that appends the #190 trailer, so a gateway frame is authenticated like any
other. `TXOK` means *the driver accepted the frame*. It does not mean *`dst` received it*: smol
never waits for ESP-NOW's delivery callback (`abandon_tx`, #397). A long line would otherwise take
one 64-byte USB packet per loop pass (~160 ms), so the reader spins up to 2 ms for the rest of a
part-read line, within a 4 ms budget per pump.

**Output never interleaves.** Every `@TS1` line is a single `println!`, which holds esp-println's
lock for the whole line and appends the newline. A log line from elsewhere cannot land inside it.
The USB side follows c6-watch's debug console: this module owns only the peripheral's RX half and
never writes through the HAL. `UsbSerialJtag::new` does not reset the C3's USB device (it is in
esp-hal's `KEEP_ENABLED` set), so the host's port stays up.

## Known limits

- **`TXOK` is not delivery.** The arena's own ACK/NAK (protocol §4.4) carries delivery.
- **MATCH is not registered in `classify()`.** Only this build intercepts MATCH frames; a fleet node
  still logs one as an unrecognised frame. That registration is tapstone smol-issues #1, and the
  general app-level `send_to_id` is #5. `RadioManager::ts_send` is this gateway's local version of
  #5, not the shared API.
- **Enforce mode.** When `MAC_ENFORCE` flips, an unverified MATCH frame is dropped before the gateway
  sees it, so the arena gets `mac_ok = 1` or nothing. That is the fleet policy, applied the same way.
- **A mesh OTA can replace it.** `install <id>` on the gateway's own id would flash it with the
  *fleet* image. Don't stage installs for the gateway's id; reflash it over USB.
- **Identity is NVS.** `SMOL_NODE_ID` seeds a blank NVS only, so a reused board keeps its old id.
  `HELLO` reports the id the board actually uses.
- **Opening the port.** The arena opens the port with the `serialport` crate. If an open ever toggles
  DTR/RTS into the C3's reset sequence, the gateway reboots (it says `HELLO` again) or, worse, drops
  into download mode. `bench.py` holds both lines low for that reason; the arena side is worth
  checking the same way.

## Build and flash

From `rust/clock/` (the toolchain and the gotchas are in `docs/BUILDING.md`):

```bash
cp -n src/board.rs.example   src/board.rs      # git-ignored; the id comes from SMOL_NODE_ID below
cp -n src/secrets.rs.example src/secrets.rs    # then set GROUP_KEY + GROUP_KEY_EPOCH to the FLEET's
                                               # (bw); the WiFi/MQTT fields are never used
SMOL_NODE_ID=61 SMOL_TS_CHANNEL=6 ESP_LOG=info \
  cargo build --release --features tapstone-gw,espnow,cast,io
espflash erase-region 0xf000 0x2000 --port /dev/ttyACMx   # ONLY if the board has ever taken an OTA
espflash flash --port /dev/ttyACMx --partition-table partitions-ota.csv \
  "${CARGO_TARGET_DIR:-target}"/riscv32imc-unknown-none-elf/release/clock
```

`ESP_LOG=info` is what keeps smol's ordinary logs on the port (the level is compile-time). Leaving
out `SMOL_TS_CHANNEL` gives the scan-and-lock behaviour. Check the `Loaded app from offset` line
after flashing, as `BUILDING.md` asks, and identify the port by MAC (`udevadm info -n /dev/ttyACMx`),
never by number: on katana `ttyACM0` is a keyboard.

## Bench acceptance — needs JP's hands

The issue's test: a second node's MATCH frames appear as `RX` lines within 20 ms, and a `TX` line
reaches that node, verified by its log. No fleet firmware sends MATCH frames yet, so the second node
is **a second gateway**. Frames go A → B and B → A, and each board's output is the other's log.

1. Pick two C3 boards that are **not** fleet or shrine boards; the flash replaces their firmware.
   Flash both as above, with the fleet's `GROUP_KEY`, **distinct** ids (e.g. `SMOL_NODE_ID=61` and
   `62`), and both with `SMOL_TS_CHANNEL=6`.
2. Plug both into the laptop and run, from the repo root:
   ```bash
   targets/c3-tapstone-gw/bench.py            # or --a <mac> --b <mac> to name them
   ```
   It finds the Espressif ports that answer `PING` with `HELLO`, waits until each lists the other in
   `ROSTER`, then sends 50 MATCH frames each way (alternating unicast and broadcast). Exit 0 means
   every frame got `TXOK` on the sender and an `RX` line with `mac_ok=1` on the receiver, all within
   20 ms, with ordinary log lines printing alongside. It prints p50/p95/max latency per direction.
3. Optional second run without `SMOL_TS_CHANNEL`, with a fleet crown on the air: the two gateways
   should lock onto the crown's channel by its HELLOs (`locked to chN` in their logs) and pass again.
   That is the table's real configuration.

The latency is measured from the host's write of `TX` to board A to the host's read of `RX` from
board B. That includes A's USB-in, parse and send and the air time, on top of B's RX path, so it is
an upper bound on the gateway's RX latency, and a pass is conservative. `bench_selftest.py` proves
the script can fail: its seven fake-gateway cases each break one criterion, and the healthy case
measures the script's own floor (about 0.2–2 ms over a 2 ms fake).
