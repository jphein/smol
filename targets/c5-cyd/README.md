# c5-cyd — the ESP32-C5 cheap-yellow-display

The fifth fleet target (#399 epic filed it; the NM-CYD-C5 board): an ESP32-C5
CYD intended as a full smol node with the rich-GUI flavor — the touch/display
stack that lives in [`targets/c6-watch`](../c6-watch/) — plus, later, the
802.15.4 side of the Zigbee-bridge role (back-burnered by JP 2026-08-25).

- **Chip**: ESP32-C5 (riscv32imac — chip id 4, feature `esp32c5`) · **node
  ids**: 176–191 (176 = the dev CYD)
- **Today**: the dev board (id176) runs the watch-OS build from the `cyd-c5`
  project and speaks the mesh as a leaf; `rust/clock` checks clean for the chip
  (`tools/check_chips.sh`) but has no board arm here yet.
- **Destination**: board truth (pins, touch transform, partitions) lands here
  once the cyd-c5 lane's board seam routes through the c6-watch subtree —
  coordinate with that lane before adding code; placement is being settled.

Status: mesh-proven dev board; target folder staged ahead of the board seam.
