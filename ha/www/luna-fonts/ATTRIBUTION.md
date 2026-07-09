# Bundled fonts

Both fonts are redistributed under the **SIL Open Font License 1.1** (OFL),
which permits bundling in this repository.

- **VT323** — Peter Hull. https://fonts.google.com/specimen/VT323 (OFL-1.1)
- **IBM Plex Mono** — IBM. https://github.com/IBM/plex (OFL-1.1)

They are loaded locally (no CDN) via `@font-face` in `../themes/smol.yaml`
so the dashboard is self-contained and works offline on the HA host.
