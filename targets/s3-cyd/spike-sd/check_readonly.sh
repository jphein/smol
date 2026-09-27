#!/usr/bin/env bash
# check_readonly.sh — hold spike-sd to "read-only by construction" (smol#554).
#
# The probe runs against cards that are not ours (the first was JP's IP-camera debug card, with SSH
# keys on it). "It only reads" was a claim in a doc comment; this makes it a check that fails.
#
#   targets/s3-cyd/spike-sd/check_readonly.sh [dir]     # default: this script's own directory
#
# FAILS (exit 1) when src/*.rs:
#   1. sends any SD command outside the read-only set {0, 8, 9, 10, 17, 41, 55, 58}: an ALLOW-list,
#      so CMD24/25 (write), 27 (program CSD), 28/29/30 (write protect), 32/33/38 (erase),
#      42 (lock), 56 (general command), ACMD23 (pre-erase) and anything not yet thought of all fail.
#      Every `cmd(spi, cs, <idx>, …)` must name its index as a literal, or the check cannot see it;
#   2. frames a command anywhere but `cmd()` (a raw `0x40 |` command byte elsewhere);
#   3. opens a file with any embedded-sdmmc mode but `ReadOnly`, or names a ReadWrite* mode at all;
#   4. calls an embedded-sdmmc write API (file `.write(`, `write_all`, `flush` on a file,
#      `delete_file_in_dir`, `make_dir_in_dir`, `truncate`, `set_len`, `close_file` after write …).
# FAILS (exit 2) when the floor is not met: an absence check that finds nothing prints the same
# green as one that works, so it must find the eight read commands and a ReadOnly open.
set -euo pipefail
dir="${1:-$(cd "$(dirname "$0")" && pwd)}"
src="$dir/src"
[ -d "$src" ] || { echo "check_readonly: no $src" >&2; exit 2; }
fail=0
bad() { echo "check_readonly: FAIL $*"; fail=1; }

# 1. every SD command index, as sent through cmd(spi, cs, IDX, ...)
calls=$(grep -hoE 'cmd\(spi, *cs, *[^,]+,' "$src"/*.rs || true)
nonlit=$(printf '%s\n' "$calls" | grep -vE 'cmd\(spi, *cs, *[0-9]+,' | grep -v '^$' || true)
[ -z "$nonlit" ] || bad "a command index is not a literal: $nonlit"
idx=$(printf '%s\n' "$calls" | grep -oE ', *[0-9]+,$' | tr -dc '0-9\n' | sort -un)
for i in $idx; do
  case " 0 8 9 10 17 41 55 58 " in
    *" $i "*) ;;
    *) bad "SD command CMD$i is not in the read-only set" ;;
  esac
done

# 2. command framing only inside cmd()
raw=$(grep -nE '0x40 *\|' "$src"/*.rs | grep -v 'for b in \[0x40 | idx' || true)
[ -z "$raw" ] || bad "a command byte is framed outside cmd(): $raw"

# 3. file modes
modes=$(grep -hoE '(FileMode|Mode)::[A-Za-z]+' "$src"/*.rs | grep -vE '^Mode::_[0-3]$' | sort -u || true)
for m in $modes; do
  case "$m" in
    FileMode::ReadOnly) ;;
    *) bad "file mode $m (only FileMode::ReadOnly is allowed)" ;;
  esac
done
grep -qnE 'ReadWrite' "$src"/*.rs && bad "a ReadWrite* mode is named: $(grep -nE 'ReadWrite' "$src"/*.rs)"

# 4. embedded-sdmmc write APIs (SpiBus::write is the SPI bus, not the card, and is allowed)
w=$(grep -nE '\.(write|write_all|truncate|set_len)\(|delete_file_in_dir|make_dir_in_dir|delete_dir|\bflush\(\)' "$src"/*.rs \
    | grep -vE 'SpiBus::(write|flush)\(' || true)
[ -z "$w" ] || bad "an embedded-sdmmc write API: $w"

# the floor
for i in 0 8 9 10 17 41 55 58; do
  printf '%s\n' $idx | grep -qx "$i" || { echo "check_readonly: FLOOR CMD$i not found — is the parser still seeing cmd()?"; exit 2; }
done
grep -q 'FileMode::ReadOnly' "$src"/*.rs || { echo "check_readonly: FLOOR no FileMode::ReadOnly open found"; exit 2; }

if [ "$fail" = 0 ]; then
  echo "check_readonly: OK — SD commands {$(printf '%s ' $idx)}, file modes {$modes}, no write APIs"
fi
exit "$fail"
