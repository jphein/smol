#!/usr/bin/env bash
# bard (#300): fetch the stories260K checkpoint + 512-token tokenizer (MIT, karpathy/tinyllamas).
# Artifacts land in scratch/bard/ (git-ignored). Pinned by sha256 — a drifted upstream FAILS.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p scratch/bard
BASE=https://huggingface.co/karpathy/tinyllamas/resolve/main/stories260K
# Baked-in pins. First-run bootstrap: run with SMOL_BARD_PIN=print to get values, then bake them.
SHA_PT="eec953f9d0f139e894ef8996302680e64b24813c7a98425424f5c85f7cf4abb1"
SHA_TOK="037cb335abb25d1fa9e8ecae30ed2a3a8ace9302862ebcdc05d51a6bbb10c312"
fetch() { # $1 file  $2 sha
  local f="scratch/bard/$1"
  [ -f "$f" ] || curl -fL --retry 3 -o "$f" "$BASE/$1"
  local got; got=$(sha256sum "$f" | cut -d' ' -f1)
  if [ "${SMOL_BARD_PIN:-}" = "print" ]; then echo "$1 sha256=$got"; return; fi
  [ "$got" = "$2" ] || { echo "PIN MISMATCH $1: got $got want $2" >&2; exit 1; }
}
fetch stories260K.pt "$SHA_PT"
fetch tok512.bin     "$SHA_TOK"
echo "bard model artifacts OK in scratch/bard/"
