#!/usr/bin/env bash
# #152 web-emulator build: compile the wasm from smol's real cores and inline it (base64)
# into a SINGLE self-contained index.html that opens straight from file:// (no server,
# no wasm-bindgen). Regenerate any time the firmware game/render code changes.
#
#   ./build.sh   # → web/emulator/index.html
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$HERE/../../rust/web-emu"
WASM="$CRATE/target/wasm32-unknown-unknown/release/web_emu.wasm"
CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"

echo "→ building web-emu (wasm32, release) …"
( cd "$CRATE" && "$CARGO" +1.96.1 build --release --target wasm32-unknown-unknown )

# Optional size trim if wasm-opt is available (not required — the raw .wasm works).
if command -v wasm-opt >/dev/null 2>&1; then
  echo "→ wasm-opt -Oz …"
  wasm-opt -Oz "$WASM" -o "$WASM.opt" && mv "$WASM.opt" "$WASM"
fi

echo "→ inlining $(stat -c%s "$WASM") B of wasm as base64 into index.html …"
python3 - "$WASM" "$HERE/index.template.html" "$HERE/index.html" <<'PY'
import base64, sys, pathlib
wasm, tmpl, out = sys.argv[1], sys.argv[2], sys.argv[3]
b64 = base64.b64encode(pathlib.Path(wasm).read_bytes()).decode("ascii")
html = pathlib.Path(tmpl).read_text().replace("__WASM_B64__", b64)
pathlib.Path(out).write_text(html)
PY

echo "✓ web/emulator/index.html  ($(stat -c%s "$HERE/index.html") B) — xdg-open it."
