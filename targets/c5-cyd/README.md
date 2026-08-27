# c5-cyd — the ESP32-C5 cheap-yellow-display

The fifth fleet target (#399 epic filed it; the NM-CYD-C5 board): an ESP32-C5
CYD intended as a full smol node with the rich-GUI flavor — the touch/display
stack that lives in [`targets/c6-watch`](../c6-watch/) — plus, later, the
802.15.4 side of the Zigbee-bridge role (back-burnered by JP 2026-08-25).

- **Chip**: ESP32-C5 (riscv32imac — chip id 4, feature `esp32c5`) · **node
  ids**: 176–191 (176 = the dev CYD; block allocated in `docs/protocol.md`)
- **Today**: the dev board (id176) runs the **GUI flavor** — the `board-cyd-c5`
  arm of the `targets/c6-watch` workspace — and speaks the mesh as a leaf.
- **`rust/clock` status: LINKS AND BOOTS, not yet budget-gated** (was "checks-only" until
  #485's hardware run on id176, 2026-08-27: the linked fleet image boots from ota_0, joins
  WiFi, runs its **own** NTP — `tsrc=ntp`, where the GUI image only ever managed `tsrc=mesh` —
  meshes with the live fleet, and registers PSRAM at `heap=8,426,036`). The fleet source
  compiles for the chip
  (`tools/check_chips.sh`; `[chip.esp32c5] checks = true` in
  `tools/build-matrix.toml`) and has its own feature arm in
  `rust/clock/Cargo.toml`. There is **no** measured `ChipBudget` row in
  `src/budget.rs`, which is why `builds` is deliberately `false` — an unmeasured
  chip would be handed the poison row. Measure on hardware per #388, then flip
  it. *"It compiles" is a real claim and a much weaker one than "it builds";
  rounding one to the other is the mistake this row exists to prevent.*
- **No download** (`target.toml` `artifact = false`): the C5 fleet image is
  now LINK- and BOOT-proven (#485) but not yet floor-gated — and linking alone still is not proof
  without the budget row.
- **Destination**: board truth (pins, touch transform, partitions) lands here
  once the cyd-c5 lane's board seam routes through the c6-watch subtree —
  coordinate with that lane before adding code; placement is being settled.

**Milestone of record:** on 2026-08-24 this board became the **first non-C3
silicon ever heard on smol's mesh** (#388).

Status: 🟡 mesh-proven dev board; target folder staged ahead of the board seam.
