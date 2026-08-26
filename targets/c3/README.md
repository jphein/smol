# c3 — the headless fleet node

The original smol target: a bare ESP32-C3 supermini (~$1) with no display, no
buttons beyond BOOT, running the canonical fleet tier — ESP-NOW mesh, SMOLv1,
signed leaf-mesh OTA, crown election.

- **Chip**: ESP32-C3 (riscv32imc) · **node ids**: 1–99 (allocation table in
  `docs/protocol.md`)
- **Firmware**: `rust/clock`, fleet tier (`espnow,cast,io`), no per-board code —
  this folder holds target-level artifacts (partition tables, flash notes,
  release manifests) as they accrue, not a separate crate.
- **Budget row**: `rust/clock/src/budget.rs` `ESP32C3` — and it is the only row in
  the fleet whose stack floor is `Derived`: computed from a **measured
  on-hardware peak** (`ESP32C3_MEASURED_PEAK_BYTES`), with a const-assertion
  coupling the two so the floor cannot silently drift below peak × 4/3. That is
  the strongest provenance this project has; every other chip's is weaker and
  says so.
- **Images**: `smol-<build>.bin`, delivered over the mesh — **fleet boards update
  OTA-only**, never from a download. The `target.toml` here declares
  `artifact = true`, so this chip is also the canonical **download** for putting
  smol on a *new* board; see [`docs/RELEASES.md`](../../docs/RELEASES.md) for the
  placeholder-credential and re-key rules that apply to it.
- **Canonical everything**: `tools/build-matrix.toml` names esp32c3 the
  `canonical_chip` and `fleet` the `canonical_tier`, and it is the only chip with
  `builds = true` / `ships = true` today.

Status: 🟢 the shipping fleet. Everything else in `targets/` is measured against
this board's numbers.
