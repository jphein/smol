# c3 — the headless fleet node

The original smol target: a bare ESP32-C3 supermini (~$1) with no display, no
buttons beyond BOOT, running the canonical fleet tier — ESP-NOW mesh, SMOLv1,
signed leaf-mesh OTA, crown election.

- **Chip**: ESP32-C3 (riscv32imc) · **node ids**: 1–99 (allocation table in
  `docs/protocol.md`)
- **Firmware**: `rust/clock`, fleet tier (`espnow,cast,io`), no per-board code —
  this folder holds target-level artifacts (partition tables, flash notes,
  release manifests) as they accrue, not a separate crate.
- **Budget row**: `rust/clock/src/budget.rs` `ESP32C3`
- **Images**: `smol-<build>.bin`, delivered over the mesh (fleet updates go
  OTA-only — see `docs/RELEASES.md`).

Status: the shipping fleet. Everything else in `targets/` is measured against
this board's numbers.
