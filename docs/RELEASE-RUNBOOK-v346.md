# v346 release runbook — the checklist, not the rationale

> ## 🟡 PREP ONLY. Nothing in this file fires before JP's go.
> This exists so that when STEP T merges and the paint bound passes, task #19 **executes from a
> checklist instead of being reconstructed**. It is written ahead of the event on purpose.

> ### ⏳ DELETE OR SUPERSEDE THIS FILE ONCE v346 IS CUT.
> Its v346 specifics become history the moment the release exists, and anything in it that turns out
> to be *generally* true about versioned releases belongs in `docs/RELEASES.md` instead. A
> version-stamped runbook with no expiry is how a repo accumulates procedures nobody can date — the
> same reason `xtensa-spike.yml`'s schedule block carries a deletion condition.

## What this file deliberately does NOT restate

`docs/RELEASES.md` § *"The versioned release ceremony (`v346` will be the first)"* already covers the
**why**: the repro-build property (a fixed `(commit, node-id)` builds to the same bytes, so the sha256
*is* the identity), #44's two causes (rustc's absolute paths, the wall-clock app descriptor), the
`repro_build.sh`-is-a-sourced-library trap, and sigil names being history that is never re-synced.

**Read that first. It is not repeated here.** Two statements of one fact is the failure mode this repo
spends the most effort on, and a runbook that re-explains the ceremony would drift from it.

---

## 0. Preconditions — all must hold before step 1

| # | gate | how to check |
|---|---|---|
| 0.1 | **STEP T merged** to `main` | verify by CONTENT on `origin/main`, not by a merged badge (a badge is a claim about a PR) |
| 0.2 | **Paint bound passed** | task #20's measured `P` against the shipped-image bound. The number lives with that task; do not copy it here — it has moved once already |
| 0.3 | **`tools/gate.sh host` / `fw` / `excl` all green on the release commit** | run all three; read `GATE_RC`, and confirm the arms you care about are **named** in the output rather than inferred from a green summary |
| 0.4 | **`Cargo.lock` is fresh** | the #460 arm (`PASS lock fresh`) asserts it. A stale lock means `Cargo.toml`'s exact `=` pins are unenforced and two hosts can resolve different graphs from one commit |
| 0.5 | **JP's go** | task #19 is approved in principle; the *cut* is still JP's call |

⚠️ **0.3 is not "CI was green on the PR."** Run the gate on the commit you are about to release, in a
tree that is that commit. Four of my gate arms landed from four branches cut at four different bases
and git auto-merged all of them — meaning git had nothing to flag. Green PRs do not prove a green
main; **verify the merge product.**

---

## 1. The version number and its sigil word

```bash
# 1a. bump the released build number
$EDITOR rust/clock/version.txt     # 345 -> 346
```

**v346 is `Riveted Gear`.** Derived, not chosen — `version_name_for()` in
`rust/clock/src/net/names.rs:256`:

```
noun = FORGE.nouns[n % 20]          adj = FORGE.adjectives[(n / 20) % 20]
346 -> nouns[6]="Gear",  adjectives[17]="Riveted"
```

Verified against every naming control the docs already carry — `341 → Bellows`, `342 → Crucible`,
`345 → Riveted Furnace`, `905 → Flux Furnace`. All four reproduce, which is what makes the formula
reading trustworthy rather than assumed.

⚠️ **Always write the number and the word together** (`v346 Riveted Gear`) — per `DOC-UPKEEP.md`, that
pair self-checks, and it is how "build 905 Riveted Furnace" was caught. Never put a bare live build
number in prose.

---

## 2. Build the image — through `repro_build_bin`, never by hand

```bash
tools/ota_publish.sh stage        # HEAD only; builds + hosts + publishes the staged line
```

**Do not hand-roll a release build.** Three reasons, each already enforced:

- **`repro_build.sh` is a sourced library** — running it directly exits 0 having done nothing
  (`RELEASES.md` covers this; it has been misread as the stack gate before).
- **`stage` refuses what it cannot honestly stamp** (#400): a `<commit>` that is not HEAD → **exit 22**;
  dirty tracked inputs under `rust/clock` → refused unless you pass `--dirty`, which then builds a
  **DEV**-stamped image on purpose.
- **`SMOL_RELEASE=1` is set for you, at `ota_publish.sh:504`** — see §3.

### ⚠️ If you must build outside `stage`, the identity contract is mandatory

`build.rs:95` **fails closed** (#420): `SMOL_RELEASE=1` with no resolvable commit **panics** rather
than stamping `nogit`. The contract is:

```bash
SMOL_GIT_HASH=<short7> SMOL_BUILD_NUMBER=<n> SMOL_RELEASE=1 cargo build --release …
```

`repro_build_bin` already does exactly this (`repro_build.sh:406`), which is why going through it is
the shorter and safer path.

---

## 3. What `SMOL_RELEASE=1` actually means — and a correction worth carrying

**The release-stamping arm EXISTS and is exercised.** It is not outstanding work:

```
tools/ota_publish.sh:504   SMOL_RELEASE=1 repro_build_bin "$CLOCK" "$BIN" "$HASH" "$BUILD"
tools/ota_publish.sh:502   (--dirty path: NO SMOL_RELEASE -> build.rs stamps vN+dev.<hash>)
tools/verify_image.sh:134  release-stamped by default, to match staging
tools/test_ota_publish_guards.sh:181  asserts a clean stage stamps  SMOL_RELEASE=1
tools/test_ota_publish_guards.sh:191  asserts --dirty OMITS it  (DEV stamp)
```

The reasoning is recorded at `ota_publish.sh:482-485`: *"staging IS the release act, so stamp it as one
HERE rather than hoping the operator remembered."* Before that line existed, 913/915 shipped
release-stamped **only because operators exported it by hand.**

⚠️ **So the stamp does not mean "this is the versioned release."** It means *"an identity-bearing,
reproducible build of HEAD with clean inputs"* — which a routine canary stage also is. **The thing
that makes v346 v346 is the NUMBER in `version.txt`.** Do not read a release stamp on a staged canary
as a release having happened.

---

## 4. Verify the image against the commit

```bash
tools/verify_image.sh <commit> --expect <sha256>     # exit 0 match / 3 mismatch
tools/verify_image.sh <commit> --twice               # PROVE determinism: cold vs warm build
```

Ask for **the sha256**, not for "it passed."

### The canonical-path rule (#327) — only if you need cross-host byte-identity

Cargo's unit hashes include the **path of a path dependency**, so the same commit in two different
directories produces two different images. `rust/clock` has two such deps (`sigil-names`,
`esp-wifi-sys-chip`), so the mechanism is present, not hypothetical.

```bash
tools/repro_at_canonical.sh <tree> <out.bin> [--hash H] [--number N]
```

⚠️ A **symlink does not work** — cargo canonicalises, and it fails *silently*, looking like it worked
while pinning nothing. The script exists because a bind mount does work; it refuses an unprovisioned
tree (**exit 4**) rather than generating a random CI `GROUP_KEY` behind your back.

**Not required for a single-host cut.** Reach for it when two machines must agree on the bytes.

---

## 5. Canary — ONE board. This is a mandate, not a preference.

```bash
tools/ota_publish.sh install <id>      # per-device; there is NO fleet-fetch topic
```

**App-side rollback covers a boots-but-unhealthy image. A hard panic or boot-loop can only be
recovered by the 2nd-stage bootloader, whose revert-on-boot-fail is OFF / unproven on hardware
(ROADMAP D2).** So the mass-brick defence is structural: install to one board, confirm it comes back
healthy, then the next. The tooling enforces the shape; the discipline is yours.

- Confirm the canary's version **advances** and it stays owner-locked.
- `id42` is refused by name (#314) — it is the C6 watch's unset-config sentinel, not a node.
- ⚠️ **Before any USB flash after an OTA:** `espflash erase-region --port /dev/ttyACM0 0xf000 0x2000`.
  A post-OTA USB flash silently lands in the slot the bootloader will not select. Check the
  `Loaded app from offset` line on every flash.

---

## 6. Publish the artifacts and flip the row

- Per-target artifacts ride `tools/release_targets.sh` / the release workflow (#413), which walks the
  `targets/*/target.toml` manifests. The combined `SHA256SUMS` job landed in #499.
- **`docs/RELEASES.md` header table** currently reads `first one | nightly-2026-08-24 | **v346** — not
  yet cut`. Flipping that row is part of cutting the release, not a follow-up.
- Verify the published assets by **reading the release**, not the workflow's green tick — a green job
  is a claim about a job.

---

## 7. The two decisions that are JP's, recorded so they are not made by default

### 7.1 Manual cut vs a tag-triggered workflow

**No workflow in this repo has a tag trigger** — `git grep 'tags:' -- .github/workflows/` returns
nothing. `release-targets.yml` is `workflow_dispatch` + a nightly `schedule`.

The argument against adding one is already in the tree, in `fw-gate.yml`'s own trigger comment: `'**'`
is branches-only **because** *"a release tag should not spend forty minutes re-running a gate that
already passed on the commit it points at."*

So the real question is **gate-by-ordering** (main is gated, therefore the tagged commit is gated) vs
**gate-by-re-execution** (#394 item 1 asks for the gate as a precondition step). Both are defensible;
they are different postures, not a right and a wrong answer. **JP's call**, and #394 is where it should
be recorded.

### 7.2 Whether a versioned release re-cuts the per-target artifacts

The nightly already publishes five per-target images. A versioned release either reuses that pipeline
or has its own. Unresolved; not urgent until 7.1 is settled.

---

## What is genuinely NOT built yet

| item | state |
|---|---|
| a **tag-triggered** release job | **does not exist** (§7.1) |
| the `SMOL_RELEASE=1` stamping arm | ✅ **exists and is tested** — see §3. Do not carry this as outstanding work |
| gate-as-an-explicit-precondition-step (#394 item 1) | answered as a *design decision* today (ordering), not as a step. §7.1 |
| `repro_build_bin` ceremony (#394 item 2) | ✅ `release_targets.sh:72,90` |
| deterministic `ci_provision` placeholders (#394 item 4) | ✅ `release_targets.sh:68`, `CI_PROVISION_FIXED_KEY=1` |
| combined `SHA256SUMS` (#394 item 5) | ✅ #499 |
| JP's re-key instructions | ✅ `RELEASES.md` § *"🔑 Re-key before you trust your mesh"* |

---

## Provenance of this file

Every mechanism above was read out of `origin/main` while writing it, not recalled. Two things I got
wrong on the first pass and corrected before they reached this page, recorded because a runbook's
value is entirely in being right:

1. I was briefed that the `SMOL_RELEASE=1` arm was **not yet built**, and my first check appeared to
   agree — because the check was **truncated by a `head -8`** that hid every hit outside `build.rs`.
   The arm exists at `ota_publish.sh:504` and has two test arms. §3 is the corrected version.
2. The sigil word is **computed**, with four documented controls reproduced, rather than continued from
   the v345 pattern by hand.
