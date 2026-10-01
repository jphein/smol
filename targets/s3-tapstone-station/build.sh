#!/usr/bin/env bash
# Build the Tapstone shrine STATION for an ES3C28P on familiar, and pull the ELF back (tapstone#132 c).
# Copied from targets/s3-tapstone-station/build.sh (read its header: every fact there holds here).
#
#   SMOL_NODE_ID=61 TAPSTONE_STATION_NODE=161 TAPSTONE_DECK=ember-neutral TAPSTONE_INDEX=0 targets/s3-tapstone-station/build.sh
#   SMOL_NODE_ID=62 TAPSTONE_DECK=tide-neutral TAPSTONE_INDEX=1 targets/s3-tapstone-station/build.sh
#
# Shape copied from targets/s3-cyd/spike/build-remote.sh: rsync the source to ~/builds/<name> on
# the builder, run ONE cargo there with every knob on its command line, rsync the ELF back. It runs
# from katana or from familiar itself (then the "remote" is this host and no ssh is used).
#
# THE RECIPE IS NOT IN THIS FILE. `tools/build_matrix.py hand-build s3-tapstone-station` derives it from
# `[hand_build.s3-tapstone-station]` + `[chip.esp32s3]` + Cargo.toml's `default`, and
# tools/test_build_matrix.sh pins the result to the invocation the two-board bench passed on
# (2026-09-26). Change the chip row and this script follows; nothing here to drift.
#
# Facts this script carries, each one learned the hard way somewhere in this repo:
#   * build-std rides the ENVIRONMENT of this one cargo, never .cargo/config.toml: cargo's
#     `[unstable] build-std` is GLOBAL, and the key there broke every cold C3 build on 2026-07-20
#     (the warning block in rust/clock/.cargo/config.toml). Same for the opt-level override: the
#     global release profile is the C3's, and the S3's `2` is an LLVM-scavenger workaround.
#   * EVERY knob is forwarded explicitly over ssh. build-remote.sh's SPIKE_HEAP_KB lesson: a
#     variable set in katana's shell never reaches the remote cargo on its own, cargo sees no
#     change, and a suspiciously fast build ships the image you did not ask for.
#   * The rsync'd tree has no .git, so build.rs would stamp the hash `nogit`; SMOL_GIT_HASH is
#     computed here, from the tree you are building. HELLO reports it as <fw_hash8>.
#   * familiar's /tmp is a 512 MB tmpfs; TMPDIR=/var/tmp for the remote build.
#   * The destination is ~/builds, NEVER the Syncthing-mirrored ~/Projects (target/ is GBs).
#
# ⚠️ THE FLEET GROUP_KEY TRAVELS WITH THE TREE: rsync copies the git-ignored src/secrets.rs and
# src/board.rs into the builder's ~/builds dir. That is where the key already is (familiar's
# Syncthing mirror of ~/Projects/smol carries a secrets.rs); a builder that must NOT hold it
# should not be REMOTE. The all-zero placeholder key is refused at compile time (#336), so a
# tree without the real key fails the build instead of producing a gateway that says mac_ok=0.
#
# Env:
#   SMOL_NODE_ID     REQUIRED. The board's mesh id (61, 62 on the bench). Seeds a blank NVS only.
#   TAPSTONE_DECK    ember-neutral (default) | tide-neutral — a file in rust/tapstone-decks/decks.
#   TAPSTONE_INDEX   the seat's figurine/copy index (default 0); the arena's registry must match.
#   TAPSTONE_STATION_NODE  REQUIRED on the board whose USB holds the arena (the arena drops its
#                    own gateway node's frames as echoes): 161 at the table. Default: SMOL_NODE_ID.
#   TAPSTONE_NO_PROPOSE=1  the stall control: a seat that claims and then plays nothing.
#   SMOL_TS_CHANNEL  optional 1..=13, pins the channel (the bench used 6); unset = scan and lock.
#   ESP_LOG          default `info`, which is what keeps smol's ordinary logs on the port.
#   REMOTE           builder host, default familiar.   RDIR  default builds/s3-tapstone-station.
#   CARGO_TARGET_DIR forwarded if set (as a path ON THE BUILDER); default $RDIR/rust/clock/target.
set -euo pipefail

NAME=s3-tapstone-station
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
CLOCK="$ROOT/rust/clock"
REMOTE="${REMOTE:-familiar}"
RDIR="${RDIR:-builds/$NAME}"

[ -n "${SMOL_NODE_ID:-}" ] || { echo "build: SMOL_NODE_ID is required (e.g. 61); two gateways need distinct ids" >&2; exit 2; }
for f in src/secrets.rs src/board.rs; do
  [ -f "$CLOCK/$f" ] || { echo "build: rust/clock/$f missing — cp $f.example and fill it (targets/c3-tapstone-gw/README.md, Build and flash)" >&2; exit 2; }
done

IFS=$'\t' read -r chip target toolchain build_std opt_level features \
  < <("$ROOT/tools/build_matrix.py" hand-build "$NAME")
[ -n "${features:-}" ] || { echo "build: no recipe from build_matrix.py hand-build $NAME" >&2; exit 2; }
[ "$toolchain" = "-" ] && toolchain=""
[ "$build_std" = "-" ] && build_std=""
[ "$opt_level" = "-" ] && opt_level=""

githash="$(git -C "$ROOT" rev-parse --short=7 HEAD)"
# No `-dirty` suffix: HELLO carries the hash as <fw_hash8>, and the arena reads that field. Say it.
git -C "$ROOT" diff --quiet HEAD -- rust/ || echo "build: ⚠️ rust/ has uncommitted changes; the image says $githash but is not it" >&2

# One env word per knob, each quoted for the remote shell. APPEND only (build-remote.sh's other
# lesson: a bare `=` once dropped a knob that an earlier line had set).
envs="TMPDIR=/var/tmp SMOL_GIT_HASH=$(printf %q "$githash") SMOL_NODE_ID=$(printf %q "$SMOL_NODE_ID")"
envs="$envs ESP_LOG=$(printf %q "${ESP_LOG:-info}")"
[ -n "${SMOL_TS_CHANNEL:-}" ] && envs="$envs SMOL_TS_CHANNEL=$(printf %q "$SMOL_TS_CHANNEL")"
for k in TAPSTONE_DECK TAPSTONE_INDEX TAPSTONE_STATION_NODE TAPSTONE_NO_PROPOSE; do
  [ -n "${!k:-}" ] && envs="$envs $k=$(printf %q "${!k}")"
done
[ -n "$build_std" ] && envs="$envs CARGO_UNSTABLE_BUILD_STD=$build_std"
[ -n "$opt_level" ] && envs="$envs CARGO_PROFILE_RELEASE_OPT_LEVEL=$opt_level"
[ -n "${CARGO_TARGET_DIR:-}" ] && envs="$envs CARGO_TARGET_DIR=$(printf %q "$CARGO_TARGET_DIR")"
tc=""; [ -n "$toolchain" ] && tc="+$toolchain"

# ON the builder, the command still runs in an EMPTY environment (`env -i`), as ssh would give it.
# Measured: a plain `bash -c` inherited the caller's SMOL_TS_CHANNEL, so an image built with the
# forwarding line deleted came out byte-identical to the right one. The local path would have
# hidden exactly the bug the forwarding exists to prevent.
if [ "$(hostname -s)" = "$REMOTE" ]; then
  run() { env -i HOME="$HOME" USER="${USER:-}" LOGNAME="${LOGNAME:-}" PATH=/usr/local/bin:/usr/bin:/bin \
            bash -c "cd \"\$HOME\" && $1"; }
  dst="$HOME/$RDIR"; [ "${RDIR#/}" != "$RDIR" ] && dst="$RDIR"
else
  run() { ssh "$REMOTE" "$1"; }
  dst="$REMOTE:$RDIR"
fi

# The crate plus its path dependencies, same layout, and the vendored decks (#543), which build.rs
# reads and cargo does not know about.
# rust/clock/Cargo.toml's `path = "../*"` siblings, DERIVED (tools/gate.sh's mirror arm, same
# reason): cargo must find every path dependency's directory, optional or not, so a hand list
# breaks the build on the next sibling added. rust/es8311 was the one that would have.
siblings=$(grep -v '^[[:space:]]*#' "$ROOT/rust/clock/Cargo.toml" \
           | sed -n 's/.*path = "\.\.\/\([A-Za-z0-9_.-]*\)".*/\1/p' | sort -u)
[ -n "$siblings" ] || { echo "build: no path siblings found in rust/clock/Cargo.toml" >&2; exit 2; }
run "mkdir -p $RDIR/rust"
for d in clock $siblings tapstone-decks; do
  rsync -a --delete --exclude target/ "$ROOT/rust/$d/" "$dst/rust/$d/"
done

echo "build: $NAME id=$SMOL_NODE_ID seat=${TAPSTONE_STATION_NODE:-$SMOL_NODE_ID} deck=${TAPSTONE_DECK:-ember-neutral}/${TAPSTONE_INDEX:-0} ch=${SMOL_TS_CHANNEL:-scan} hash=$githash on $REMOTE — $tc --features $features"
# Both PATH halves (build-remote.sh): without the first, `cargo: command not found`; without
# export-esp.sh, a missing xtensa-esp32s3-elf-gcc that impersonates a broken toolchain.
run "export PATH=\"\$HOME/.cargo/bin:\$PATH\" && . \$HOME/export-esp.sh && cd $RDIR/rust/clock && \
  $envs cargo $tc build --release --no-default-features --features $features --target $target"

tdir="${CARGO_TARGET_DIR:-$RDIR/rust/clock/target}"
out="$HERE/target/clock-$NAME-id$SMOL_NODE_ID.elf"
mkdir -p "$HERE/target"
if [ "$(hostname -s)" = "$REMOTE" ]; then src="$tdir"; [ "${tdir#/}" = "$tdir" ] && src="$HOME/$tdir"
else src="$REMOTE:$tdir"; fi
rsync -a "$src/$target/release/clock" "$out"
echo "ELF pulled: ${out#"$ROOT"/}"
echo "flash: espflash flash --chip $chip --port /dev/ttyACMx --partition-table targets/s3-cyd/partitions-ota-s3.csv ${out#"$ROOT"/}"
