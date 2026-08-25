#!/usr/bin/env bash
#
# THE FLASH GUARD, EXECUTABLE — s3-cyd spike (smol node id 162).
#
# ===========================================================================
# THIS SCRIPT REFUSES BY DEFAULT. THAT IS THE FEATURE.
# ===========================================================================
#
# This workstation's USB bus routinely carries, at the same time:
#
#   * live ember.realm.watch voice satellites — THE SAME MODEL AS THIS BOARD,
#     the same ES3C28P, the same 28:84:85:44:* MAC prefix. One of them is
#     deployed off-site in family service. A prefix match is NOT identity.
#   * a sealed reliquary vault unit — also the same model.
#   * JP's two C6 watches, the laundry proxy, a battery emulator.
#   * other agent sessions' dev boards (the NM-CYD-C5 belongs to the cyd-c5
#     session; emberburrito's terminal belongs to morpheus-burrito's lane).
#
# `espflash flash` with no --port picks the FIRST ESP device it finds. So the
# default behaviour of the obvious command is to overwrite somebody's working
# hardware. This script is wired in as the cargo `runner` (.cargo/config.toml)
# so the check cannot be skipped by using the normal command.
#
# It resolves the port BY SERIAL and does nothing at all unless exactly one port
# reports the ONE allowed serial. There is deliberately NO OVERRIDE FLAG: an
# escape hatch here would get used, and the failure it prevents is bricking a
# device that somebody depends on.
#
# Identity is read PASSIVELY, via `udevadm`. Do NOT switch this to `espflash
# board-info` to identify a port: that RESETS the target it probes, which means
# the act of identifying the bus reboots every board on it. (Learned the hard
# way — a live C6 watch was rebooted exactly this way.)
#
# Usage (also what cargo passes):  ./flash.sh <path-to-elf> [extra espflash args]

set -euo pipefail

# espflash lives in ~/.cargo/bin, which a non-login shell does not have on PATH
# (the same two-disguise trap the build docs name). The guard must work when
# invoked directly, not only through `cargo run` — a guard that errors AFTER
# saying "OK — flashing" trains people to bypass it.
PATH="$HOME/.cargo/bin:$PATH"

# ===========================================================================
# THE ALLOW LIST — exactly one entry, and it is EMPTY ON PURPOSE
# ===========================================================================
#
# ARMED 2026-08-24: the board is plugged in and positively identified as
# smol node id 162 (passive bus-diff 23:03; recorded in ../BOARD.md,
# ../README.md and ../PORT-SCOPING.md).
#
# HOW TO RE-CONFIRM, or to identify a replacement board — a passive bus-diff,
# no writes, no resets:
#   1. With the new board UNPLUGGED, snapshot the bus:
#        for p in /dev/ttyACM* /dev/ttyUSB*; do [ -e "$p" ] || continue;
#          udevadm info -q property -n "$p" | sed -n 's/^ID_SERIAL_SHORT=//p';
#        done | sort > /tmp/bus-before
#   2. Plug the new board in, wait 2s, run the same command into /tmp/bus-after.
#   3. `comm -13 /tmp/bus-before /tmp/bus-after` — the single new line is it.
#   4. Cross-check that line against DENY_SERIALS below. If it matches ANY of
#      them, STOP: you have plugged in an existing device, not the new board.
#   5. Put it in ALLOW_SERIAL, and register the MAC with smol-d8 so the fleet's
#      id-block and BoardProfile agree with this file.
#
# An empty value refuses everything, which is the correct behaviour for a guard
# that does not know what it is guarding. That branch is KEPT even though this
# guard is now armed — it is the safe default for anyone cloning this pattern for
# a new board.
#
# ===========================================================================
# ⛔⛔ THIS SERIAL IS SAME-BATCH WITH A SEALED BOARD. READ BEFORE EDITING.
# ===========================================================================
#
#       target (id 162)     14:C1:9F:D1:C8:10   <- the ONLY sanctioned target
#       reliquary (SEALED)  14:C1:9F:D1:C3:C8   <- NEVER WRITE TO THIS
#                           ^^^^^^^^^^^^ FIRST FOUR OCTETS IDENTICAL
#
# They differ only in the last two octets, and both contain "C8". This is the
# single most confusable pair on this bench, and one of them must never be
# written to. An eyeballed comparison WILL eventually confuse them.
#
# THEREFORE, TWO RULES THAT ARE NOT NEGOTIABLE:
#
#   1. The comparison below is BYTE-EXACT (`[ "$serial" = "$ALLOW_SERIAL" ]`).
#      Keep it that way.
#
#   2. ⛔ **NOBODY MAY EVER "FIX" A MISMATCH BY LOOSENING THIS TO A PREFIX**
#      — no `case "$serial" in 14:C1:9F:D1:*)`, no `[[ $serial == 14:C1:9F* ]]`,
#      no truncation, no case-insensitive fuzz. A prefix match on this bench
#      MATCHES THE SEALED BOARD. If the guard says "no port reports this
#      serial", the answer is ALWAYS to find out why (wrong board plugged in?
#      re-enumerated? cable?) — never to widen the pattern until it matches.
#      Widening the match is the exact motion that destroys the vault unit, and
#      it is the motion a frustrated person reaches for at 1am.
readonly ALLOW_SERIAL="14:C1:9F:D1:C8:10"   # smol node id 162. Byte-exact only — see above.

# ===========================================================================
# THE DENY LIST — hard refusal, checked even if ALLOW_SERIAL is set
# ===========================================================================
#
# Belt and braces. If ALLOW_SERIAL is ever filled in with one of these by
# mistake (a copy-paste from the wrong table, a same-model confusion), the deny
# sweep below catches it before any port is opened.
readonly DENY_SERIALS=(
    # ⛔ THE CONFUSABLE ONE. Shares its first four octets with this target's own
    # candidate serial (14:C1:9F:D1:C8:10) and also contains "C8". Sealed vault
    # unit, SAME MODEL (ES3C28P): do not write, do not reset, do not probe.
    "14:C1:9F:D1:C3:C8"   # reliquary — SEALED.
    "28:84:85:44:59:20"   # ember-satellite (JP's desk) — SAME MODEL, LIVE family service (HA Assist).
    "28:84:85:44:3E:C4"   # ember-mobile (battery handheld) — SAME MODEL, LIVE family service.
    "28:84:85:44:3E:A4"   # ember-dad — SAME MODEL, LIVE family service, DEPLOYED OFF-SITE. Maximal caution.
    "28:84:85:44:45:94"   # emberburrito hearth terminal — SAME MODEL, morpheus-burrito's lane, not ours.
    "98:A3:16:A7:2F:E4"   # JP's ESP32-C6 watch — wearable, do not touch.
    "98:A3:16:A5:A7:F8"   # JP's ESP32-C6 watch (second) — wearable, do not touch.
    "E8:06:90:65:9F:E4"   # laundry proxy — live household automation.
    "F0:F5:BD:FD:3C:C0"   # battery emulator — live bench instrument.
    "3C:DC:75:99:8D:18"   # NM-CYD-C5 — the cyd-c5 session's board, not ours.
)

die() { printf '\n\033[1;31m[FLASH GUARD] %s\033[0m\n\n' "$*" >&2; exit 1; }

# --- allow-list guard: refuse before touching anything ----------------------
if [ -z "$ALLOW_SERIAL" ]; then
    die "REFUSING TO FLASH — this guard is not armed.

             ALLOW_SERIAL is empty. That is the safe default: a guard that does
             not know what it is guarding must refuse everything.

             Identify the board with the passive bus-diff recipe in this file's
             header, then paste its serial in — BYTE-EXACT, never a prefix.
             A prefix on this bench matches a SEALED board."
fi

# --- deny-list guard: applies even to a filled-in allow entry ----------------
for denied in "${DENY_SERIALS[@]}"; do
    if [ "$ALLOW_SERIAL" = "$denied" ]; then
        die "REFUSING TO FLASH — ALLOW_SERIAL is set to a DENY-LISTED device ($denied).
             That is a live board belonging to something else. Re-run the
             bus-diff; you have identified the wrong device."
    fi
done

# --- baud guard --------------------------------------------------------------
# This board's USB-Serial/JTAG link CORRUPTS at 460800/921600 (emberboy field
# note, same hardware). Default baud only — structural, not a comment.
for arg in "$@"; do
    case "$arg" in
        --baud|--baud=*|-B|-B*)
            die "refusing --baud: this board's USB-JTAG link corrupts above default baud." ;;
    esac
done

# --- serial resolution (PASSIVE — udevadm only, never espflash board-info) ----
matches=()
for port in /dev/ttyACM* /dev/ttyUSB*; do
    [ -e "$port" ] || continue
    serial="$(udevadm info -q property -n "$port" 2>/dev/null \
              | sed -n 's/^ID_SERIAL_SHORT=//p')"

    tag=""
    for denied in "${DENY_SERIALS[@]}"; do
        [ "$serial" = "$denied" ] && tag="  <- DENY-LISTED, never flash"
    done
    printf '[flash guard] %-16s %s%s\n' "$port" "${serial:-<none>}" "$tag" >&2

    [ "$serial" = "$ALLOW_SERIAL" ] && matches+=("$port")
done

case "${#matches[@]}" in
    0)  die "no port reports serial ${ALLOW_SERIAL}. The s3-cyd spike board is not attached.
             REFUSING TO FLASH — other ESP devices on this bus include live ember
             satellites of the SAME MODEL. Plug the right board in; do not
             retarget this script." ;;
    1)  ;;
    *)  die "several ports report serial ${ALLOW_SERIAL} (${matches[*]}). That should be
             impossible; resolve it by hand rather than letting this guess." ;;
esac

readonly PORT="${matches[0]}"

# --- holder guard: never flash a port somebody else is using ------------------
# A serial monitor, another agent's session, or a stale espflash holding this
# port turns a flash into a corrupted write or a confusing failure. Report the
# holder and STOP. Deliberately does NOT kill it: this script has no idea whose
# process that is, and killing by pattern match is how live work gets destroyed.
if command -v fuser >/dev/null 2>&1; then
    if holders="$(fuser "$PORT" 2>/dev/null)" && [ -n "${holders// /}" ]; then
        printf '[flash guard] %s is held by PID(s):%s\n' "$PORT" "$holders" >&2
        ps -o pid=,user=,cmd= -p ${holders} 2>/dev/null >&2 || true
        die "REFUSING TO FLASH — ${PORT} is open in another process (see above).
             That is probably a serial monitor or another session. Close it
             yourself. This script will not kill a process it cannot identify."
    fi
fi

printf '\033[1;32m[flash guard] OK: %s is %s — flashing.\033[0m\n' "$PORT" "$ALLOW_SERIAL" >&2

# espflash's monitor wants a TTY for its input reader and dies mid-boot without
# one, truncating exactly the log you flashed to read. Detect it rather than
# making every scripted/CI caller remember the flag.
extra=()
[ -t 0 ] || extra+=(--non-interactive)

# espflash prints `MAC address:` before writing — an INDEPENDENT second check.
# It must read ${ALLOW_SERIAL}. If it does not, pull the cable.
exec espflash flash --monitor --port "$PORT" "${extra[@]}" "$@"
