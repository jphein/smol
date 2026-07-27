#!/usr/bin/env bash
# bard (#300): golden reference from the independent Python forward pass (see plan Task 6 for
# why upstream runq.c is disqualified for this checkpoint).
set -euo pipefail
cd "$(dirname "$0")/.."
PY=scratch/bard/venv/bin/python
OUT=rust/clock/src/bard/testdata
mkdir -p "$OUT"
"$PY" tools/bard_reference.py rust/clock/model/stories260K-q8.bin \
  --temp 0 --steps 200 -i "Once upon a time, there was a little dragon" \
  --tokens-out "$OUT/golden_tokens.txt" > "$OUT/golden_ref.txt"
echo "golden written: $OUT/golden_ref.txt"
