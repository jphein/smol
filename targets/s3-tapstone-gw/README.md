# s3-tapstone-gw — the Tapstone gateway on an ESP32-S3

The same gateway as [`c3-tapstone-gw`](../c3-tapstone-gw/README.md), built for an ESP32-S3. The
wire, the design, the known limits and the bench are all that folder's; this one only changes the
build. smol#548 / smol#549, part of tapstone#132.

**Status: ✅ hardware-verified, 2026-09-26.** Two S3s flashed as ids 61 and 62 on channel 6 passed
`targets/c3-tapstone-gw/bench.py` (exit 0): ROSTER mutual in 3.9 s; A→B 50/50 delivered, p95
14.68 ms; B→A 50/50, p95 17.21 ms; `mac_ok` 100/100; ordinary log lines printing alongside. Numbers
are in the verifying comment on smol#549. The C3 arm has not run on hardware yet.

## Why an S3 at all

- **The boards that exist are S3s.** JP's shrine boards are S3s, and the fleet has spare S3s but no
  spare C3s, so the gateway that actually sits on the arena laptop is an S3.
- **Nothing in the gateway names a chip.** Both chips drive their USB-C port with the same
  USB-Serial-JTAG peripheral, and `ts_gw.rs` only uses that peripheral's RX half. `rust/clock/src`
  needed no S3 cfg for this; only the build invocation differs.
- **The C3 stays the lint arm; the S3 is compiled by its own job.** The S3 is `builds = false` in
  `tools/build-matrix.toml`, so the `firmware` job checks and lints the gateway only as
  `c3-tapstone-gw`. This folder is declared as `[hand_build.s3-tapstone-gw]`: the recipe comes from
  the manifest, `build_matrix.py check` holds this folder to it, and the fw-gate job **`hand builds`**
  (smol#548) installs the pinned espup toolchain and compiles it on every push with
  `tools/gate.sh hand` → `tools/build_hand.sh`. That build uses CI's throwaway `GROUP_KEY`, so it
  proves the image compiles and links, never that it works: the bench below is still the test.

## The recipe

This is the invocation the bench passed on, run from `rust/clock/` on familiar:

```bash
CARGO_UNSTABLE_BUILD_STD=core,alloc CARGO_PROFILE_RELEASE_OPT_LEVEL=2 \
SMOL_NODE_ID=61 SMOL_TS_CHANNEL=6 ESP_LOG=info \
  cargo +esp build --release --no-default-features \
  --features esp32s3,hw,tapstone-gw,espnow,cast,io --target xtensa-esp32s3-none-elf
```

Each part has a reason:

| part | why |
|---|---|
| `+esp`, `. ~/export-esp.sh` | Xtensa is a Tier-3 target; espup's `esp` channel, pinned 1.95.0.0 byte-identical on katana and familiar |
| `CARGO_UNSTABLE_BUILD_STD=core,alloc` | no prebuilt `core` for Xtensa. **Per invocation, never in `.cargo/config.toml`**: that key is global, and on 2026-07-20 it broke every cold C3 build (read the warning block in `rust/clock/.cargo/config.toml`) |
| `CARGO_PROFILE_RELEASE_OPT_LEVEL=2` | the shipping `opt-level = "s"` crashes the Xtensa LLVM backend ("Incomplete scavenging after 2nd pass"). A workaround for this chip only; the global profile is the C3's |
| `--no-default-features … esp32s3,hw` | `default` is `esp32c3,hw`; the chip feature is swapped and `hw` put back |
| `tapstone-gw,espnow,cast,io` | the `tapstone-gw` tier: the gateway plus the fleet features |
| `SMOL_NODE_ID`, `SMOL_TS_CHANNEL`, `ESP_LOG=info` | as in the C3 README: id (seeds a blank NVS only), pinned channel (omit it to scan and lock), logs on the port |

You do not type it. `build.sh` gets it from `tools/build_matrix.py hand-build s3-tapstone-gw`, which
derives it from the manifest, and `tools/test_build_matrix.sh` (in the gate) fails if that
derivation stops producing exactly the line above. `tools/build_hand.sh --print s3-tapstone-gw`
shows what CI compiles (the line above without the per-board id and channel), and
`tools/test_build_hand.sh` fails if that stops being the same build. On familiar, with
`. ~/export-esp.sh`, `tools/gate.sh hand` runs the same compile locally (about 2 min cold).

## Build and flash

Once per checkout, as for the C3: `rust/clock/src/board.rs` and `src/secrets.rs` from their
`.example`s, with the **fleet's** `GROUP_KEY` and `GROUP_KEY_EPOCH` (bw). The all-zero placeholder
key is refused at compile time.

```bash
SMOL_NODE_ID=61 SMOL_TS_CHANNEL=6 targets/s3-tapstone-gw/build.sh   # from katana or on familiar
espflash flash --chip esp32s3 --port /dev/ttyACMx \
  --partition-table targets/s3-cyd/partitions-ota-s3.csv \
  targets/s3-tapstone-gw/target/clock-s3-tapstone-gw-id61.elf
```

`build.sh` follows `targets/s3-cyd/spike/build-remote.sh`: it rsyncs `rust/clock` and its two path
dependencies to `~/builds/s3-tapstone-gw` on familiar (never the Syncthing-mirrored `~/Projects`),
runs the one cargo there with every knob forwarded explicitly, stamps `SMOL_GIT_HASH` from your
tree (the copy has no `.git`), and pulls the ELF back into `target/` here. The rsync copies your
git-ignored `secrets.rs` to the builder; familiar already holds one through the Syncthing mirror.

The partition table is the s3-cyd one (two 6 MiB OTA slots on the 16 MB flash). Its `otadata` is
at the C3's offset, so the C3 README's `espflash erase-region 0xf000 0x2000` applies unchanged if
the board has ever taken an OTA. Identify the port by MAC, never by number. Then run the bench
from that README: `bench.py` finds Espressif ports by USB vendor ID and does not care which chip
answers.
