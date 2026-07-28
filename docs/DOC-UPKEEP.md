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
| 3 | **`docs/superpowers/specs/` + `plans/`** | Measured numbers — RAM geometry, timings, verification outcomes. **But see the projection trap below** — a spec contains both estimates and measurements, and they look identical. |
| 4 | **`docs/protocol.md`** | The wire. Byte layouts, CFG keys, MQTT topics. The best-maintained doc in the repo — treat a conflict with it as a bug in the *other* doc. |
| 5 | **Closed issues + merged PRs** (`gh issue view`, `gh pr view`) | Whether something shipped, and when. PR bodies carry the measured tables. |
| 6 | **`git log`** | Sequencing, and the *reasoning* — commit bodies here are long on purpose. |
| 7 | `docs/ROADMAP.md`, `README.md`, `site/` | Nothing. These are **derived** — they are the things you are checking. |

⚠️ **Six ways a source misleads you even when you read it correctly** — §2 below. Read those
before trusting anything in the table above; four of them defeat a careful reader who *did* cite a
source.

---

## 2. How sources mislead — six traps

Each of these has bitten this repo, most of them in a single 2026-07-27 audit. They are grouped here
because they share a property: **none is caught by reading more carefully.** They are caught by
asking a *different* question.

### ⚠️ The projection trap — a spec's estimates look exactly like its measurements

A design spec has two kinds of number in it and **the formatting does not distinguish them**:

- The **design body** (`§5 RAM budget`, sizing tables, "image grows X → Y") is **pre-build
  projection**. It was written before anything was compiled.
- The **`✏️ AMENDMENT` blocks** are **measurements**, added after the fact, and they routinely
  *contradict* the body. The body is deliberately left standing as a record of what was expected.

Quoting the body as though it were measured is a real and easy mistake — it happened during the
2026-07-27 pass, to the person writing this file. `ota.md`'s slot-headroom figure went from a stale
**~3.3×** to a *worse* **~2.3×**, because ~2.3× came from the spec's estimate table ("image grows
~590KB → ~880KB — 45% of a slot"). The built image is **1,432,400 B — 70.5 % of the slot, 1.42×
headroom**: ~550 KB heavier than projected. The estimate was not a lie; it was simply an estimate,
sitting in a document full of measurements.

**Rule: for any physical number — image size, RAM, timing — prefer the artifact over any prose.**
Measure the ELF/binary, or take the figure from a source that says it measured one (an amendment,
a PR's measured table, `Cargo.toml`'s partition rationale). If you cannot tell whether a number was
projected or measured, treat it as projected.

### ⚠️ A number you derived is not a number you measured

Sibling of the projection trap, and distinct in a way that matters: the projection trap is
**someone else's estimate mistaken for a measurement**. This one is **your own correct measurement
applied to the wrong dimension.** Both survive the "but I cited a source" defence, which is exactly
why they need writing down.

Four cases, two authors, one day (2026-07-27):

| the error | the real number | the axis it belonged to |
|---|---|---|
| build `45 → 905` | OTA **staged/ratchet** build 905 | **release version** — 345 |
| site reveal rate | 202 ms per **token** (generation) | 160 ms per **character** (CFG-`V` reveal) |
| renderer wrap width | 15 columns (what the mock did) | **14** — `FONT_5X8` on 72 px |
| slot headroom `~2.3×` | a real figure in the #300 spec | a **projection**, not the measurement (1.42×) |

Every one of these was a *correct number*, read accurately, from a real source. Each was then applied
to a quantity it did not describe. Note that no amount of double-checking the number would have
caught any of them — the number was never wrong.

**The test that catches all four is one question: *what are the units, and of what?***

- `905` — 905 *what*? Staged builds and released versions are different counters.
- `202 ms` — per *what*? Generation and reveal are separate clocks on this device.
- `15` columns — of *which font*, on *which panel width*?
- `~2.3×` — headroom measured, or headroom expected?

**State the denominator and the error becomes visible.** So: when a doc quotes a number, make it
carry its unit *and* its subject — "202 ms/token (generation)", "160 ms/char (reveal)", "build 345
(released)", "1.42× (measured)". Verbose beats ambiguous, because a bare number silently accepts any
axis a reader brings to it.

### ⚠️ The orchestrator's brief is a lead, not a source

Adopted as a standing rule 2026-07-27, at the team lead's own request. **Re-derive any claim handed
to you — including from whoever assigned the task — by default, not on suspicion.** Two examples
from a single session, both from the team lead, both *correct when written*:

- "the current build is 905" — the release is **345** (`version.txt`); 905 was an OTA staged/ratchet
  canary number, and 905 doesn't even satisfy the sigil formula that produced "Riveted Furnace".
- "#302's mode/speed work is not built yet" — CFG-`V` had landed and been documented between the
  brief being written and being read. (The *conclusion* survived — still not on glass — but for a
  different reason than stated.)

Neither was carelessness. On a repo moving this fast, a brief is a snapshot of a tree that has since
moved. Read the spec/tree, **then** write. This is cheap and it has caught real defects.

### ⚠️ Upgrades are as invisible as retirements

The mirror image of the retirement blind spot, and the more dangerous one because it makes docs
*understate* the system. **Nobody revisits a doc to make a claim stronger.** When a subsystem
hardens, whoever hardened it updates the spec and the protocol reference — and the guides keep
describing the weaker version indefinitely.

Evidence: **two independent documents** understated the *same* security model. `ONBOARDING.md` and
`home-assistant.md` both described the OTA trust gate as **"SHA-256 verify"** long after #32 made it
**ed25519 signature verification before the leaf writes a single byte** — a far stronger guarantee —
and both still documented an `announce`/`announce/all` fetch path that #32's closure had *removed*.
That is not two mistakes; it is one blind spot hit twice.

So when you check a subsystem, ask both questions:
- *Has anything here been retired?* (the usual sweep)
- *Has anything here got **stronger** since this was written?* Security gates, verification depth,
  brick-safety, error handling. An understated guarantee is still a wrong doc, and it costs you the
  credit for work that was actually done.

### ⚠️ When a premise expires, check the conclusion before deleting

A doc's *reason* can go stale while its *answer* stays right. Deleting the section loses a correct
argument; leaving it stands on a false premise. Narrow the premise instead.

Worked example: `home-assistant.md`'s whole "why not ESPHome" analysis rested on *"a radio that's
off-WiFi ~28 s of every 30 s."* **#23 retired that window.** But the conclusion survives on an
independent leg — **a leaf never associates at all** (only the elected gateway does), so a per-node
persistent TCP socket is not something this topology can offer, window or no window. The section was
kept, the premise re-grounded, and the expiry flagged inline.

Ask: *if the stale premise were simply false, would the conclusion still hold for another reason?*
If yes, re-ground it. If no, the conclusion goes too — and that is a finding worth reporting, not a
quiet deletion.

### ⚠️ Retirements are the blind spot

Closed issues tell you what shipped; nothing tells you
what got *un*-shipped. Grep the tree for the mechanism before describing it as live. Known
retirements that documentation has gotten wrong: **CFG-`N`** WiFi-slot switch and its
"un-brickable auto-revert" (#100 → **retired by #142**, single-network now — a received `N` is
drained and ignored); the **UDP collector** (retired for MQTT-native); the **syncing overlay**
(#153); the **~15 s mesh-deaf burst window** (#23); **native BLE** (#22, refuted on hardware).

---

## 3. Verifying a claim

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

### Never enumerate the fleet

Docs used to say "the id7/8/9 fleet". **Don't.** Live HA on 2026-07-27 showed node ids 5, 7, 8, 9,
13, 42, 50, 51, 122, 236 — and most of those are *not* fleet members: **50 and 51 are id7's and
id9's hardware repurposed as bench measurement DUTs** for #198 Phase 2, **42 is JP's C6 smartwatch**
(a different project), and 122/236 are rig/test ids. Only id8 was reporting; 5/7/9 last spoke
2026-07-26.

So an enumeration is wrong in both directions at once — it lists boards that aren't fleet members
*and* rots every time hardware is repurposed. Instead:

- **In prose, say "the bench fleet."** No numbers.
- **Name a node only where a specific claim was verified on it** — "verified on id8", "202 ms/tok
  measured on id8". That's provenance, and provenance doesn't rot.
- A count of what a past test covered ("across three bench boards") is fine; a claim about the
  *current* roster is not.

---

## 4. Two habits that prevent most rot

Separate from the traps above — those are about *reading* a source; these two are about *writing*
one. Roughly a dozen findings in the last audit trace to their absence:

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

## 5. The website (`site/`)

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

### Display mockups: pixel-true is a constraint, not a design

Every display on the site is the same physical part — **72 × 40, 1-bit, FONT_5X8, 14 × 5 text
cells** — and `site/oled.js` is the one renderer for it (glyph bitmaps extracted from the exact
`embedded-graphics` font the firmware uses; public-domain misc-fixed, attributed in the file).

Three things learned the hard way, 2026-07-27:

1. **Pixel-true is a constraint, not a design — and it can look *worse* than the fake.** The first
   faithful block-digger draw used solid 4 × 4 tiles and the whole ground merged into one bright
   slab. Fixed by drawing terrain as sparse *texture* and leaving only the player solid. Fidelity
   and legibility are separate decisions; satisfying the first does not give you the second.
   **Check at zoom** — screenshot and blow it up 3×. Reasoning about pixels does not work.
2. **Take the constraint literally and it audits your copy.** Four tile labels turned out wider
   than 14 columns (worst: 23 chars), so real hardware would have clipped them — the mockups were
   promising legibility the part does not have. Four glyphs (`◆ · ✓ →`) are outside FONT_5X8's
   ASCII range and would render as the fallback glyph. Both are content bugs that only surface when
   you stop faking the panel.
3. **Distinguish a panel RENDER from a DIAGRAM, and label the diagram.** The World Snake graphic
   depicts the 256 × 256 world — two peers ~200 cells apart, which 72 × 40 px cannot hold — and it
   wore scanlines and a bezel with no caption, so it read as a screenshot. A diagram is legitimate
   and often clearer; it just has to say what it is. It is now labelled *"world map — not what the
   glass shows"* and paired with an actual 72 × 40 render, which demonstrates the point instead of
   asserting it.

Practical rule for a new mockup: if it depicts the OLED, draw it through `oled.js` and let CSS scale
it (`image-rendering: pixelated`, glow as a `filter` **outside** the glass). If it depicts something
larger than the panel, it is a diagram — label it as one. Never leave a third category.

The same discipline applies to **time**, not only space: the Bard panel loops a fixed excerpt while
the real board never stops, so the caption says so. A mockup that differs from the hardware in *any*
dimension — resolution, extent, duration — should say which.

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

## 6. A maintenance pass, in order

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
5. **The retirement sweep** (§2). The direction nothing else checks — and its mirror, the
   upgrade sweep: *has anything here got stronger since it was written?*
6. Run the greps in §4.
7. `site/`: content.json currency, then the §5 checklist.
8. Fix what you have evidence for; write the rest down with the evidence you'd need. Ask rather
   than guess when a claim's evidence is ambiguous.

**Cadence** (agreed 2026-07-27): a **10-minute check after every feature merge or release train** —
just §6.1–6.2 — plus a **full sweep monthly**, or whenever a wave of issues closes at once.

The argument for the per-merge check is a measurement, not a principle: **the Bard scored 1 of 3 on
merge day.** It shipped, hardware-verified, and that same day it was in `README.md`, absent from
`ROADMAP.md §1`, and had **zero** mentions anywhere in `site/` — the most remarkable thing the
project had ever done was invisible on its own front page. Ten minutes of "does the newest shipped
thing exist in all three surfaces?" would have caught it.

The website specifically deserves a look **every time something ships that a stranger would find
remarkable.** That is the whole point of it, and it is the surface most likely to be forgotten
because nothing breaks when it's stale.

### Partially-shipped states

Do not tick a checkbox for a feature that half-landed, and do not badge it 🟢 — record the split
with counts and a date. Worked example, D11: live discovery carried **3 `_voltage` + 3 `_rssi` and
zero `_soc`** on 2026-07-27, so two of four typed-entity kinds exist. "#12 closed" was *not*
evidence the split shipped. **A nearby issue closing is never evidence for the claim next to it.**

---

*Companion: [ROADMAP.md](ROADMAP.md) for status, [protocol.md](protocol.md) for the wire,
[README.md](README.md) for the pitch.*
