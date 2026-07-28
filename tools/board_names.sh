#!/usr/bin/env bash
# board_names.sh — print the id -> name map from the SAME sigil corpus the firmware compiles in.
#
# Replaces the hand-maintained `scratch/board-names.md`. A hand-kept name map is the same class of
# artefact as the hand-copied word corpus that started all of this: correct when written, silently
# wrong after the next corpus change, and with nothing to catch it. This is derived, so it cannot
# disagree with what a board calls itself.
#
#   tools/board_names.sh              # the boards that have been on the air
#   tools/board_names.sh --all        # all 256 ids
#   tools/board_names.sh 5 8 122      # specific ids
set -euo pipefail
cd "$(dirname "$0")/../rust/viz"
exec cargo run -q -p mesh-model --example board_names -- "$@"
