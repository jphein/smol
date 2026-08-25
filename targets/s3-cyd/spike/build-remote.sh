#!/usr/bin/env bash
# Build the s3-cyd spike on familiar (24 cores), pull the ELF back so cargo's
# local `runner` contract (./flash.sh) still works.
#
# Shape copied from cyd-c5/spike/build-remote.sh. Xtensa-specific facts:
#   * familiar got espup 2026-08-24, toolchain PINNED to 1.95.0.0 to byte-match
#     katana (rustc 95e5bda86 / GCC esp-15.2.0_20250920 / clang esp-20.1.1_20250829).
#     CONDITION ON THE PIN: upgrade BOTH hosts in one motion or not at all — two
#     hosts building one crate with different compilers is how irreproducible
#     binaries happen. (JP directive 2026-08-24: xtensa builds move to familiar;
#     this retired PORT-SCOPING's "katana-only" standing exception.)
#   * rsync goes to ~/builds/<name>, NEVER the syncthing-mirrored ~/Projects tree
#     — that keeps target/ (GBs) out of the sync. Do not "simplify" the two into one.
#   * familiar's /tmp is a 512 MB tmpfs (it filled during the espup install and
#     disguised itself as a compile error, same class as smol#363's gate lesson).
#     TMPDIR stays /var/tmp for anything staged remotely.
set -euo pipefail

REMOTE=familiar
RDIR="builds/s3-cyd-spike"
HERE="$(cd "$(dirname "$0")" && pwd)"
TRIPLE=xtensa-esp32s3-none-elf

rsync -a --delete --exclude target/ "$HERE/" "$REMOTE:$RDIR/"

# ---------------------------------------------------------------------------
# M2 WiFi CREDENTIALS — vault -> env -> compile-time. Never to disk, either host.
# ---------------------------------------------------------------------------
# Convention copied from cyd-c5/spike/build-remote.sh so one operator habit
# covers both spikes: the PSK is read from Vaultwarden ON KATANA at build time
# and handed to the remote cargo as an environment variable, where `option_env!`
# bakes it into the image. It is never written to a file on katana, never written
# to a file on familiar, and never echoed into a build log.
#
# ⚠️ THE SSID IS THE FLEET'S, NOT emberburrito's. `jplovescl` is the FT-off IoT
# SSID (VLAN 8) that the smol fleet lives on. emberburrito's board deliberately
# joins the ADMIN VLAN instead, because it is a hearth terminal that talks to
# hearthd on katana's own subnet — that is their product's network, not the
# fleet's. Do not read `burrito-fw/wifi.local.toml`; do not reuse its item.
#
# Vault item: "Homelab jplovescl WiFi (jplovescl SSID)"
#
# Only fetched when a tier that can use it is being built. A default (M1) build
# touches the vault not at all, so `bw` never needs unlocking to rebuild M1.
WIFI_ENV=""

# ---------------------------------------------------------------------------
# SPIKE_HEAP_KB — forwarded for EVERY build, and outside the case on purpose.
# ---------------------------------------------------------------------------
# ⚠️ THIS LINE IS THE EXPERIMENT. It was missing once, and the failure mode is
# the worst kind: `SPIKE_HEAP_KB=64 ./build-remote.sh --features wifi` set the
# variable in KATANA's shell, never passed it over ssh, and familiar happily
# rebuilt nothing (cargo saw no change) and shipped the DEFAULT 96 KiB image.
# The ELF was pulled, the command exited 0, and flashing it would have "proved"
# that 64 KiB works — using a binary that was not 64 KiB.
#
# The tell was a 0.14 s build time where a recompile was expected. If you ever
# see this script finish suspiciously fast after changing a knob, suspect THIS
# line before you believe the result.
[ -n "${SPIKE_HEAP_KB:-}" ] && WIFI_ENV="SPIKE_HEAP_KB=$SPIKE_HEAP_KB"

case "${*:-}" in
    *wifi*|*radio*)
        # `bw` prompts / fails loudly if the vault is locked. That is deliberate:
        # a SILENT fallback to an unauthenticated build would produce an image
        # that boots, says "no wifi credentials", and looks like a firmware bug.
        PSK="$(bw get password "Homelab jplovescl WiFi (jplovescl SSID)")"
        [ -n "$PSK" ] || { echo "build-remote: empty PSK from vault — refusing" >&2; exit 1; }
        # APPEND, never assign: SPIKE_HEAP_KB may already be in WIFI_ENV, and a
        # bare `=` here silently dropped it (caught 2026-08-25 — the isolation
        # build kept shipping the default heap). Every addition below appends.
        WIFI_ENV="$WIFI_ENV SPIKE_WIFI_SSID=jplovescl SPIKE_WIFI_PSK=$(printf %q "$PSK")"

        # MQTT creds for M4 (HA mosquitto; currently the same secret value as the
        # WiFi PSK — if either rotates independently, give mosquitto its own vault item)
        #
        # ⚠️ THAT CAVEAT IS LOAD-BEARING AND UNDERSTATED, so the count is written
        # here: this ONE secret value currently lives in at least THREE places —
        #   1. the vault item read above ("Homelab jplovescl WiFi (jplovescl SSID)")
        #   2. HA's Mosquitto/JuicePassProxy addon option `mqtt_password`, which
        #      smol's own `ha/README.md` calls the canonical source for the broker
        #      password and explicitly says is **not** a vault item
        #   3. baked into whatever image M4 ships
        # Three copies of one secret with no single owner is not a rotation plan.
        # The first rotation of ANY of them silently breaks the other two, and the
        # symptom will be an MQTT CONNACK failure that looks like a network fault.
        # Give mosquitto its own vault item BEFORE that happens, not after.
        WIFI_ENV="$WIFI_ENV SPIKE_MQTT_USER=jp SPIKE_MQTT_PASS=$(printf %q "$PSK")"

        # ---- M3 mode knobs, forwarded when set locally ---------------------
        # SPIKE_ESPNOW_ONLY=1 -> bring the radio up, pin a channel, DO NOT
        # associate. Needed because the AP is on ch1 (glass-verified) and the
        # smol mesh is on ch6: one radio cannot listen to both, so an associated
        # board reports a dead mesh while working perfectly.
        #
        #   SPIKE_ESPNOW_ONLY=1 ./build-remote.sh --features radio
        #   SPIKE_ESPNOW_ONLY=1 SPIKE_ESPNOW_CHANNEL=6 ./build-remote.sh --features radio
        #
        # Only honoured by a `radio` build (net.rs cfg!-gates it), so setting it
        # on a wifi-only build cannot silently stop that build associating.
        [ -n "${SPIKE_ESPNOW_ONLY:-}" ] && WIFI_ENV="$WIFI_ENV SPIKE_ESPNOW_ONLY=$SPIKE_ESPNOW_ONLY"
        [ -n "${SPIKE_ESPNOW_CHANNEL:-}" ] && WIFI_ENV="$WIFI_ENV SPIKE_ESPNOW_CHANNEL=$SPIKE_ESPNOW_CHANNEL"

        # Lengths only — never the values.
        echo "build-remote: wifi creds loaded from vault (ssid jplovescl, psk ${#PSK} chars)"
        echo "build-remote: mqtt creds staged for M4 (user jp, pass ${#PSK} chars — same value as PSK today)"
        if [ -n "${SPIKE_ESPNOW_ONLY:-}" ]; then
            echo "build-remote: ESPNOW-ONLY mode, channel ${SPIKE_ESPNOW_CHANNEL:-6} (no association)"
        fi
        ;;
esac

# Both PATH halves, remote edition — same two-disguise trap as local builds:
# missing first half = `cargo: command not found`; missing second = a
# "linker xtensa-esp32s3-elf-gcc not found" that impersonates a broken toolchain.
#
# NOTE the credentials ride INSIDE the ssh command string as a variable
# assignment prefix, so they exist only in the remote cargo's environment for the
# life of that one process. They are not exported into familiar's shell profile
# and not left in a file. (They ARE visible in the remote process list for the
# duration of the build — a known, accepted limit of this pattern on a trusted
# host; the alternative is a secrets file, which is strictly worse because it
# persists.)
ssh "$REMOTE" "export PATH=\"\$HOME/.cargo/bin:\$PATH\" && . \$HOME/export-esp.sh && \
  cd $RDIR && TMPDIR=/var/tmp $WIFI_ENV cargo build --release ${*:-}"

mkdir -p "$HERE/target/$TRIPLE/release"
rsync -a "$REMOTE:$RDIR/target/$TRIPLE/release/s3-cyd-spike" \
  "$HERE/target/$TRIPLE/release/s3-cyd-spike"
echo "ELF pulled: target/$TRIPLE/release/s3-cyd-spike (flash via ./flash.sh or cargo run --release)"
