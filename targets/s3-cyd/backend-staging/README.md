# backend-staging — the S3 display backend, pre-proven

The 4th `app::Oled` arm for the ES3C28P (smol node 162): smol's logical 72×40 `BinaryColor`
surface, scaled 4× into RGB565 and blitted to a 320×240 ILI9341V.

**Design rationale: [`../DISPLAY-PACKAGE.md`](../DISPLAY-PACKAGE.md) §4, Path A.**

## What this is

**The intake PR's payload, compiled.** An uncompiled draft is smol's dominant defect shape —
a correct-looking comment describing behaviour the binary lacks. The value of this crate is
not its ~150 lines of logic; it is that `cargo check --release` for `xtensa-esp32s3-none-elf`
is **green today**, which proves four independently-developed pieces actually compose:

| piece | contributes |
|---|---|
| `oled-scale` (path dep, 23 host tests) | logical 72×40 `DrawTarget<BinaryColor>` → RGB565 @ 4×, dirty-rect |
| `mipidsi = "=0.10.0"` | `ILI9341Rgb565` + `NoResetPin` |
| `esp-hal 1.1.1` | SPI2, `Output`, `Delay` |
| `../board-staging/board_es3c28p.rs` | pin + geometry constants |

The board constants are `#[path]`-included from the committed file, **not copied** — that
file is dependency-free precisely so this is possible, and a second copy of a pin table is a
second source of truth.

## What this is NOT

- **Not wired into `app.rs`.** That is the intake, routed through smol-d8's lane. The exact
  arm is in `lib.rs`'s `INTEGRATION_SKETCH` doc comment, in a `text` fence so it cannot be
  mistaken for compiling code — it names `crate::` paths that only exist in `rust/clock`.
- **Not flashable.** No `main`, no `[[bin]]`, and **no cargo `runner`** — the watch-port
  convention. This bench carries four live ember.realm.watch family services of the same
  model; a library staging crate must not be able to write to it. Flashing belongs to
  `../spike/flash.sh` and its byte-exact serial guard.
- **Not on-glass verified.** It compiles. Nothing here has been run on hardware.

## Build

```bash
export PATH="$HOME/.cargo/bin:$PATH" && source ~/export-esp.sh   # BOTH, in this order
cd targets/s3-cyd/backend-staging
cargo check --release
cargo clippy --release -- -D warnings
```

Katana only — familiar has no Xtensa toolchain. If the link fails with
``linker `xtensa-esp32s3-elf-gcc` not found``, you did not source `~/export-esp.sh`; that is
not a broken toolchain. Full two-disguise trap in `rust-toolchain.toml`.

## Gate status

| gate | verdict |
|---|---|
| `cargo check --release` (xtensa) | ✅ green, 0 warnings |
| `cargo clippy --release -- -D warnings` | ✅ green |
| toolchain actually used | `esp` (1.95.0-nightly), artifacts in `target/xtensa-esp32s3-none-elf/` — verified, not assumed |

## The one type mismatch this crate caught

`PanelError` is **projected** (`<Panel as DrawTarget>::Error`), not hand-spelled. The naive
spelling `SpiError<esp_hal::spi::Error, Infallible>` is wrong: `ExclusiveDevice` wraps bus
and CS failures in its own `DeviceError` first, so the real type is
`SpiError<DeviceError<esp_hal::spi::Error, Infallible>, Infallible>` — three layers from
three crates. Any literal spelling also silently encodes today's bus-sharing choice and
would break if SPI2 ever became shared. Details at `PanelError`'s doc comment.
