# DOC-UPKEEP — keeping smol's docs and website true

smol's docs carry an explicit honesty rule: **shipped means hardware-verified on the fleet.**
Verification legend: 🟢 hardware-verified · 🟡 compile/spec-verified · ⚪ design only.

That rule only survives if someone re-derives it periodically, because **docs rot in one
direction**: a doc written while a feature was pending keeps saying "pending" forever, and a doc
written while a feature was live keeps saying "live" after it's retired. Both have happened here.
This file is the checklist for a maintenance pass, so the next one is cheap.

**Under-claiming is fine. Overclaiming is a defect.** If you cannot source a claim, badge it 🟡
and say why — do not upgrade it because it "obviously works by now."

---

## 1. Where truth actually lives

Ranked. When two sources disagree, the higher one wins.

| Rank | Source | Authoritative for |
|---|---|---|
| 1 | **The tree** — `rust/clock/src/**`, `ha/`, `tools/` | What the code actually does. A `const` beats a paragraph. |
| 2 | **`rust/clock/version.txt`** | The released fleet build number. Nothing else. |
| 3 | **`docs/superpowers/specs/` + `plans/`** | Measured numbers — RAM geometry, timings, verification outcomes. Amended in place as hardware findings land, so read to the *end* of a section, not the first claim. |
| 4 | **`docs/protocol.md`** | The wire. Byte layouts, CFG keys, MQTT topics. The best-maintained doc in the repo — treat a conflict with it as a bug in the *other* doc. |
| 5 | **Closed issues + merged PRs** (`gh issue view`, `gh pr view`) | Whether something shipped, and when. PR bodies carry the measured tables. |
| 6 | **`git log`** | Sequencing, and the *reasoning* — commit bodies here are long on purpose. |
| 7 | `docs/ROADMAP.md`, `README.md`, `site/` | Nothing. These are **derived** — they are the things you are checking. |

⚠️ **Retirements are the blind spot.** Closed issues tell you what shipped; nothing tells you
what got *un*-shipped. Grep the tree for the mechanism before describing it as live. Known
retirements that documentation has gotten wrong: **CFG-`N`** WiFi-slot switch and its
"un-brickable auto-revert" (#100 → **retired by #142**, single-network now — a received `N` is
drained and ignored); the **UDP collector** (retired for MQTT-native); the **syncing overlay**
(#153); the **~15 s mesh-deaf burst window** (#23); **native BLE** (#22, refuted on hardware).

---

## 2. Verifying a claim

```bash
# Did it ship, and when?
gh issue view <n> --json number,title,state,closedAt
gh pr view <n> --json state,mergedAt,body            # PR bodies carry the measured tables
gh pr list --state open --json number,title,headRefName   # what is genuinely in flight

# Is the mechanism still in the tree? (retirement check — do this too)
grep -rn "<CONST_OR_TOPIC>" rust/clock/src ha docs/protocol.md

# Measured numbers: cite the spec, don't re-derive from memory
grep -rn "<number>" docs/superpowers/specs/
```

"In flight" should mean **commits on `main` or an open PR** — not intent. Everything else is
spec'd/queued.

### The build number and its sigil word

The single most contradiction-prone fact in the repo. Two independent traps:

1. **The released build is `rust/clock/version.txt`** — currently `345`. It is *not*
   `git rev-list --count HEAD` (that's only `build.rs`'s fallback when neither the file nor
   `SMOL_BUILD_NUMBER` is set, and it reads much higher). Bench builds are stamped with arbitrary
   high `SMOL_BUILD_NUMBER` **canary pins** (902, 903, 905, 950 …) so they out-rank the fleet's
   monotonic OTA gate — see #128. A number in the 900s in a doc is almost certainly a canary pin,
   not a release.
2. **The sigil word is derivable, so a mismatched pair is a bug.** `version_name_for()` in
   `rust/clock/src/net/names.rs`:

   ```
   noun = FORGE.nouns[n % 20]        adj = FORGE.adjectives[(n / 20) % 20]
   ```

   `345 → ("Riveted", "Furnace")`. Check the number and the word **together** — that is how
   "build 905 Riveted Furnace" was caught (905 maps to *Flux Furnace*; only 345 is *Riveted
   Furnace*). Note #218 **changed this formula** — it now uses direct modulo, not sigil's `>>8`
   — so any sigil name written before 2026-07-20 is suspect and must be recomputed.

**Rule: never put a live build number in prose.** Point at `version.txt` instead. If you must
name one, name it *with* its sigil word so the pair self-checks.

---

## 3. The two habits that prevent most rot

Roughly a dozen of the findings in the last audit trace to exactly two missing habits:

1. **Never write a live build number in prose** (above). It is stale the next release.
2. **Date every proof.** "58→59 in ~17 s" reads as present tense forever. Write "58→59 in ~17 s
   **on 2026-07-10**" and it ages into history instead of becoming false. Same for "running now",
   "the fleet is on…", "next wave".

A cheap CI-able smell test for the first class of rot:

```bash
grep -rniE "not yet|hasn't shipped|pending wave|running now|next wave|coming soon" \
  docs/ README.md ONBOARDING.md --include=*.md | grep -v ROADMAP.md
```

Every hit is either a genuine ⚪/🟡 or a piece of rot. Also worth periodically re-checking:

```bash
# issue numbers cited as open that have since closed
grep -rhoE "#[0-9]{1,3}" README.md docs/*.md | sort -u | tr -d '#' | \
  while read n; do echo "$n $(gh issue view $n --json state -q .state 2>/dev/null)"; done
# dead intra-doc anchors (headings get renamed; links don't follow)
grep -rn "](.*\.md#" docs/ README.md
```

Docs should also **cite `io.rs`'s `FREE_PINS`/`RESERVED_PINS` rather than hand-rolling a pin
map** — several docs disagree with the firmware's own reject-list, including one that claims to
"dodge every reserved pin" while assigning the battery ADC.

---

## 4. The website (`site/`)

`site/` auto-deploys to **GitHub Pages on every push touching `site/**`**
(`.github/workflows/pages.yml`, publish dir `site/`). **Your edit is the public face — verify
locally first:**

```bash
python3 site/server.py 8099      # then open http://localhost:8099
```

### ⚠️ The trap: `content.json` beats `index.html`

`site/index.html` holds the chrome plus a **default** for every `data-edit="key"` element.
`site/app.js` then fetches `content.json` and **overwrites any element whose key is present
there**. So:

- **Editing prose in `index.html` for a key that exists in `content.json` changes nothing that a
  visitor sees.** For existing copy, edit **`content.json`**.
- New sections may ship their copy as an `index.html` default (no `content.json` key needed); the
  first WYSIWYG save will absorb it.
- The two therefore drift apart legitimately. Don't "fix" a divergence by syncing blindly —
  decide which one the visitor reads (`content.json`) and correct that.

Prose is meant to be edited through the WYSIWYG (toggle **Edit**, then Save → the server writes
`content.json`). Hand-editing `content.json` is fine — it is plain JSON — but keep it valid and
prefer a script over an editor for bulk changes.

**Measured numbers on the site are deliberately *not* `data-edit` fields** (e.g. the Bard's
receipts). They are evidence tied to a spec, and a stray keystroke in edit mode should not be
able to silently falsify one. Change those in `index.html`.

### Share links + Open Graph move together

`https://jphein.github.io/smol/` is hardcoded in **three** places that must never diverge:
`<link rel="canonical">` + `og:url` + `og:image` in `index.html`'s head; every pre-encoded share
`href` in the `#share` section; and `SHARE_URL` in `app.js`. If the Pages URL ever changes, grep
for `jphein.github.io/smol` and fix all of them.

### Site checklist

- [ ] Favicon present; **dark *and* light** via `prefers-color-scheme` (JP's standing preference).
- [ ] **No external CDNs** — fonts are self-hosted in `site/fonts/` on purpose (#106: a Google
      Fonts `<link>` discloses visitor IPs). Inline or self-host everything.
- [ ] `node --check site/app.js` passes.
- [ ] OG card is a **real raster** at exactly the declared `og:image:width`/`height` (crawlers
      reject SVG and do not resolve relative paths — absolute HTTPS URLs only):
      `python3 -c "from PIL import Image; print(Image.open('site/assets/og-card.png').size)"`
      Regenerate from its checked-in source: see the comment at the top of `site/assets/og-card.html`.
- [ ] New sections carry `class="reveal"` (the `IntersectionObserver` in `app.js` picks up any
      `.reveal` globally). **A section without it will look fine; a section *with* it and no JS
      is invisible** — that's `opacity:0`, not a broken layout.
- [ ] Section kickers are numbered contiguously (`NN — Title`); non-numbered asides use `◇`.
      Inserting a section means renumbering the ones after it, and the nav.
- [ ] No new internal IPs / hostnames / MACs. Pre-existing LAN details are accepted (JP's call)
      — don't scrub them, but don't add more.

---

## 5. A maintenance pass, in order

1. `git log --oneline -30` and `gh pr list --state open` — what happened since last time.
2. **Anything shipped that no doc mentions?** Newest feature first; it is the most likely to be
   missing from `README.md` and completely absent from `site/`. (The Bard shipped and the site
   had zero mentions of it on the day it merged.)
3. **ROADMAP §1/§2/§3 boundaries.** Has a §2 "in flight" item shipped? Has a §3 "ready to build"
   item shipped? These decay silently and make the doc actively misleading — §3 once advertised
   OTA and the node manager as unbuilt while §1 and the README described both as hardware-proven.
4. **Decision docket (§5).** Tick what resolved and record **how** — including decisions that
   went *differently* than recommended. Two had (D3 landed the stronger option; D5's physical
   long-press was never built — the accept gate is HA's Install command). A quietly-reversed
   decision is worse than an open one.
5. **The retirement sweep** (§1 above). The direction nothing else checks.
6. Run the greps in §3.
7. `site/`: content.json currency, then the §4 checklist.
8. Fix what you have evidence for; write the rest down with the evidence you'd need. Ask rather
   than guess when a claim's evidence is ambiguous.

**Cadence:** after every release train or feature merge for a quick §5.1–5.2 pass; a full sweep
roughly monthly, or whenever a wave of issues closes at once. The website specifically deserves a
look **every time something ships that a stranger would find remarkable** — that is the whole
point of it.

---

*Companion: [ROADMAP.md](ROADMAP.md) for status, [protocol.md](protocol.md) for the wire,
[README.md](README.md) for the pitch.*
