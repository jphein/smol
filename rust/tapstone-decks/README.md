# tapstone-decks (vendored data)

`decks/*.toml` are byte-identical copies of `decks/` in the public
[jphein/tapstone-game](https://github.com/jphein/tapstone-game) at the tag named in
`VENDOR.sha256`, the same pin as `rust/tapstone-rules`, `-proto`, `-progression` and
`rust/shrine-render`. The station firmware derives its deck tables from them at build time.

Do not edit them here. Change them in tapstone, tag there, then re-vendor:
`tools/tapstone_vendor.sh --sync --tag <tag>` (`tools/gate.sh` runs `--check`).
