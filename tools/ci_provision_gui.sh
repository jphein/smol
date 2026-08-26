#!/usr/bin/env bash
# ci_provision_gui.sh — provision the GUI workspace for a PUBLIC build (#413 phase 3.1).
#
# The GUI flavor's sibling to tools/ci_provision.sh, and it does the OPPOSITE thing on purpose.
#
# ── WHY THIS EXISTS ───────────────────────────────────────────────────────────
# targets/c6-watch's `.cargo/config.toml` is GITIGNORED and, on a developer's machine, holds
# real credentials under `[env]` — WIFI_SSID / WIFI_PASS / MQTT_BROKER / MQTT_USER / MQTT_PASS /
# OTA_URL, which src/main.rs and src/net/mqtt_ha.rs read with `option_env!` and BAKE INTO THE
# IMAGE at compile time. (esp32c6-watch HANDOFF.md: "credentials — never commit; fambuild
# supplies it to worktrees".) A `git archive` tree therefore has NO build config at all: the
# build cannot run without one, and must never be handed a real one.
#
# So this script writes the build-relevant parts and NOTHING ELSE.
#
# ── ABSENT, NOT PLACEHOLDER — the watch lane's ruling of record ────────────────
# The fleet flavor bakes a PUBLISHED PLACEHOLDER group key so its artifact stays
# byte-reproducible. The GUI flavor deliberately does the reverse. From the watch's own
# .github/workflows/firmware.yml, verbatim:
#
#   "Fake placeholders were considered and REJECTED — they would bake a bogus DEFAULT
#    SSID/broker into the released artifact, which is worse than empty."
#
# Empty is not an oversight here, it is the decision. `option_env!` returns None, and the
# firmware's own compiled-in defaults apply (an empty SSID, and the `192.168.1.10:1883` broker
# literal in src/net/mqtt_ha.rs). Do not "fix" this by adding placeholder creds.
#
# ── ESP_LOG IS LOAD-BEARING, NOT A LOG-VOLUME PREFERENCE ──────────────────────
# It must be SET, and set to the same value the bench builds use. esp-println bakes the logging
# machinery in at compile time: measured at 4,064 B of `.bss` (78,160 B stack gap without it vs
# 74,096 B with). Omitting it ships a DIFFERENT MEMORY LAYOUT from every binary anyone has
# tested — and the watch's #65 established that on this chip an 8-byte shift moved a crash from
# 0% to 100% (the WiFi blob parks its globals at the top of `.bss`, directly under the stack).
# Shipping an untested layout is the risk; the log volume is not.
#
# For the same reason this script adds NO `--remap-path-prefix`. The fleet flavor remaps for
# cross-host byte-reproducibility, but there the remapped build IS the tested one (repro_build
# is also the OTA production path). Here it is not: remapping changes `.rodata` string lengths
# and hence the layout, so a remapped GUI image would be an untested layout shipped for the sake
# of a reproducibility claim. The honest trade is stated on the artifact instead — see the NOTES
# emitted by release_targets.sh.
#
# ── build-std: A NO-OP ON RISC-V, MANDATORY ON XTENSA ─────────────────────────
# `.cargo/config.example.toml` carries `[unstable] build-std`, and the watch's own riscv CI
# omits it, correctly, with the note that on the pinned `stable` toolchain it does nothing —
# the build links the precompiled riscv32imac core/alloc — and that a fresh cargo can
# hard-ERROR on the unstable key.
#
# ⚠️ THAT CLAIM IS TRUE FOR RISC-V AND FALSE FOR XTENSA, and generalising it broke the S3 arm
# with `error[E0463]: can't find crate for 'core'`. MEASURED: espup's `esp` toolchain sysroot
# contains ONLY `x86_64-unknown-linux-gnu` — it ships NO precompiled xtensa-esp32s3-none-elf
# core at all, so core must be built from source. (It carries the `rust-src` component for
# exactly this, and JP's own fambuild tree config has the key.) tools/build-s3.sh does not pass
# -Zbuild-std because it inherits it from that config file rather than not needing it.
#
# So the key is emitted for xtensa targets and omitted for riscv ones — derived from the triple
# rather than passed in, so a new board cannot get this wrong by forgetting an argument.
#
# Usage: ci_provision_gui.sh <gui-workspace-dir> <target-triple> [rustflags-toml-array]
#   rustflags default: ["-C", "force-frame-pointers"]  (the C6/C5 arms' proven flags)
#   The S3 arm passes []  — tools/build-s3.sh builds it with RUSTFLAGS='' and that is the
#   only layout the Xtensa arm has ever been tested at.
# Exit: 0 provisioned · 2 bad usage/environment · 3 the credential assertion FAILED.
set -euo pipefail

WS="${1:?usage: ci_provision_gui.sh <gui-workspace-dir> <target-triple> [rustflags-array]}"
TRIPLE="${2:?usage: ci_provision_gui.sh <gui-workspace-dir> <target-triple> [rustflags-array]}"
RUSTFLAGS_TOML="${3:-[\"-C\", \"force-frame-pointers\"]}"

[ -d "$WS" ] || { echo "ci_provision_gui: no such workspace dir: $WS" >&2; exit 2; }
[ -f "$WS/Cargo.toml" ] || { echo "ci_provision_gui: $WS is not a cargo workspace" >&2; exit 2; }

CFG="$WS/.cargo/config.toml"
mkdir -p "$WS/.cargo"

# If the tree somehow arrived carrying a build config, it is not ours and must not be trusted.
# (A `git archive` tree cannot — the path is gitignored — but this script must also be safe to
# run against a working tree, where that file is exactly the one holding real credentials.)
if [ -f "$CFG" ]; then
  echo "ci_provision_gui: REFUSING — $CFG already exists." >&2
  echo "  A pre-existing build config may carry real credentials, and overwriting it would" >&2
  echo "  destroy a developer's setup. Build public images from a 'git archive' tree." >&2
  exit 3
fi

cat > "$CFG" <<EOF
# GENERATED by tools/ci_provision_gui.sh for a PUBLIC build — do not edit, do not commit.
# Credentials are ABSENT DELIBERATELY (watch firmware.yml's ruling: empty beats a bogus
# default). option_env! -> None -> the firmware's own compiled-in defaults apply.
[build]
target = "$TRIPLE"
rustflags = $RUSTFLAGS_TOML

# NOT a log-volume preference: esp-println bakes this in, worth 4,064 B of .bss, and the bench
# builds carry it. Omitting it would ship an untested memory layout (#65).
[env]
ESP_LOG = "info"
EOF

# Xtensa only — the esp toolchain ships no precompiled core for this triple. See the header.
case "$TRIPLE" in
  xtensa-*)
    cat >> "$CFG" <<'EOF'

[unstable]
build-std = ["alloc", "core"]
EOF
    ;;
esac

# ── THE ASSERTION ─────────────────────────────────────────────────────────────
# A comment claiming "no credentials" is not a guarantee; this is. Every key the firmware reads
# with option_env! is named here explicitly, because the failure mode is a key we forgot to
# think about, not one we listed and mishandled. Keep this list in sync with:
#   grep -rn 'option_env!' targets/c6-watch/src/
FORBIDDEN='WIFI_SSID|WIFI_PASS|MQTT_BROKER|MQTT_USER|MQTT_PASS|OTA_URL|OTA_BUILD'
if grep -nEi "^[[:space:]]*($FORBIDDEN)[[:space:]]*=" "$CFG"; then
  echo "ci_provision_gui: ASSERTION FAILED — a credential key reached the generated config." >&2
  rm -f "$CFG"
  exit 3
fi

# Prove the assertion can actually fail, on demand, so it is not a gate that cannot fail.
# `CI_PROVISION_GUI_SELFTEST=1` appends a forbidden key and expects the grep above to catch it.
if [ "${CI_PROVISION_GUI_SELFTEST:-}" = "1" ]; then
  printf 'WIFI_SSID = "sabotage"\n' >> "$CFG"
  if grep -qEi "^[[:space:]]*($FORBIDDEN)[[:space:]]*=" "$CFG"; then
    echo "ci_provision_gui: SELFTEST PASS — the credential assertion detects a planted key."
    rm -f "$CFG"; exit 0
  fi
  echo "ci_provision_gui: SELFTEST FAIL — a planted credential key was NOT detected." >&2
  rm -f "$CFG"; exit 3
fi

echo "ci_provision_gui: $CFG  (target=$TRIPLE, rustflags=$RUSTFLAGS_TOML, no credentials)"
