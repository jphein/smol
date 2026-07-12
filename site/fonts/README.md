# smol site webfonts — self-hosted

These WOFF2s are served locally so the public site makes **no request to
Google's Fonts CDN** — visitors' IPs are never disclosed to a third party.
`fonts.css` provides the `@font-face` rules; `index.html` and `dollhouse.html`
link it instead of `fonts.googleapis.com`.

## Families & weights (exactly what the pages use)
- **Silkscreen** 400, 700  (brand / pixel bits)
- **JetBrains Mono** 400, 500, 700  (mono)
- **Sora** 300, 400, 600, 800  (sans)

Subsets kept: **latin + latin-ext** only. The site has no Cyrillic/Greek/
Vietnamese content, so those subsets would never be fetched — dropping them
is render-identical and keeps the repo lean.

## Licensing
All three are under the **SIL Open Font License 1.1** (OFL) — free to
self-host and redistribute.

## Regenerating
Pull the current WOFF2s + rebuild `fonts.css` from Google's css2 API
(modern-UA request returns WOFF2 with per-subset `unicode-range`), keeping
only the latin / latin-ext blocks:

    family=Silkscreen:wght@400;700
    family=JetBrains+Mono:wght@400;500;700
    family=Sora:wght@300;400;600;800

(The generator script lived in scratch during the #106 pass.)
