# Release runbook — the checklist, not the rationale

> ## 📗 STANDING DOCUMENT. Nothing in it fires without JP's go.
> This is the procedure for cutting a versioned release, so a cut **executes from a checklist instead
> of being reconstructed**. Per-release specifics live in the **run records** at the bottom.
>
> **Prune old run records freely. Never delete the doc.**

> ### ⏳ Why the earlier "delete this file once vN is cut" clause is gone
> It was mine, and it was wrong in two ways that only became visible when someone tried to execute it:
>
> 1. **It instructed deleting the checklist at the exact moment it was mid-execution.** A cut is when
>    this file is *in use*; an expiry keyed to the cut fires during the ceremony it exists to serve.
> 2. **It would have evaporated the run RECORD along with the doc** — including the corrections the
>    document paid for in real errors (see §1's guard, which exists because the first draft got the
>    build number backwards). The next release would have re-derived the same mistakes from scratch.
>
> The instinct behind it was right — *an undated procedure rots* — but it was **applied to the wrong
> artifact**. A runbook is a standing procedure; what is version-specific is the **run record**. So the
> expiry belongs on the records, not on the doc: prune records, keep the procedure.
>
> Same reasoning retires the version from the **filename**. `RELEASE-RUNBOOK-v346.md` → `-v1446.md`
> deferred a path-level falsehood by exactly one release; it goes false again at the next cut **by
> construction**. A rename fixes the instance; removing the version fixes the class.

## What this file deliberately does NOT restate

`docs/RELEASES.md` § *"The versioned release ceremony"* already covers the
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

> ### 🔴 CORRECTED 2026-08-28 — the first draft of this step was WRONG, and wrong in the direction
> that ships a mislabelled release. Read the guard before the commands.
>
> **The build number is an OUTPUT of the ratchet, not an input you choose.** `ota_publish.sh stage`
> computes it as `choose_build(count, staged, override)` = `max(git rev-list --count, retained staged
> build + 1)`, and passes it as `SMOL_BUILD_NUMBER` (`repro_build.sh:406`). `build.rs:72` reads
> `env_or_file("SMOL_BUILD_NUMBER", "version.txt")` — **env WINS over the file.** So on the release
> path `version.txt` is the *fallback for non-stage builds*, **not** the number that ships.
>
> The first draft said "bump `version.txt` 345 → 346, and v346 is *Riveted Gear*." Both halves were
> internally consistent and jointly wrong: the ratchet would have stamped a different number while
> `RELEASES.md` said v346.

**The order is: learn the number → derive the word → make `version.txt` agree.**

```bash
# 1a. LEARN the number the ratchet will use. Do not choose it.
git rev-list --count HEAD                     # the honest commit count
tools/ota_publish.sh legacy-line esp32c3      # (preflight, unrelated — see §2)
#   the retained staged build is the other input; `stage` prints the chosen BUILD before it publishes.

# 1b. DERIVE the word from THAT number (never from version.txt, never by continuing a pattern).
# 1c. THEN set version.txt to match, so a non-stage build reports the same lineage.
```

**Derive the name from the number the ratchet chose** — `version_name_for()` in
`rust/clock/src/net/names.rs:256`. The current release's number and word live in the **run records**
at the bottom, never here — so the worked example below uses a **control**, an already-shipped build
whose name is a historical fact. An example that used the current run would be a third place for the
version to go stale, and it would have gone stale twice already this week:

```
noun = FORGE.nouns[n % 20]          adj = FORGE.adjectives[(n / 20) % 20]
345 -> nouns[5]="Furnace",  adjectives[(345/20)%20 = 17]="Riveted"     # v345 Riveted Furnace, shipped
```

Verified against every naming control the docs already carry — `341 → Bellows`, `342 → Crucible`,
`345 → Riveted Furnace`, `905 → Flux Furnace`. All four reproduce.

> ⚠️ **And here is the lesson the first draft paid for, because those four controls did not save it.**
> They verify the **word against the number**. They say nothing about whether the *number* is right.
> `DOC-UPKEEP`'s rule — *"name it WITH its sigil word so the pair self-checks"* — catches a
> mismatched pair; it cannot catch a **correctly-derived word on the wrong number**, which is
> self-consistent and wrong. **A self-checking pair checks the RELATION, not the INPUTS.**
> Verify the number against `choose_build`'s output *first*, then derive the word from it.

⚠️ **Always write the number and the word together** (`<number> <Word>`; the current pair is in the run
record). Per `DOC-UPKEEP.md` that pair self-checks, and it is how "build 905 Riveted Furnace" was
caught. Never put a bare live build number in prose. **But see the guard above: the pair self-checking
is not the same as the number being right.**

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
that makes a release THAT release is the NUMBER the ratchet chose** — `choose_build`'s output, carried as
`SMOL_BUILD_NUMBER`. (⚠️ The first draft of this sentence said "the number in `version.txt`", which is
the same error §1 corrects: on the release path the env var overrides the file. `version.txt` is what a
NON-stage build falls back to.) Do not read a release stamp on a staged canary as a release having
happened.

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
- **`docs/RELEASES.md`'s header table** carries the current release's row. Advancing it (e.g. *in
  canary* → *cut*) is part of cutting the release, not a follow-up. Read the row rather than trusting
  a quotation here — quoting it would make this line rot every cut, which is the failure this document
  just spent a rename fixing.
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
3. **And that was still not enough** (found 2026-08-28, during the canary): the four controls verified
   the WORD against the NUMBER while the number itself came from `version.txt`, which the release path
   overrides. Corrected in §1, and the general form is worth more than the fix — *thorough verification
   of the wrong proposition is the failure mode that survives being careful*.

---

# Run records

**One entry per cut, newest first.** Prune freely once a release is old enough that nobody would
re-read it — that pruning is what the retired "delete this file" clause was reaching for, applied to
the artifact that is actually version-specific.

A record is worth keeping while it still answers *"what did we learn cutting that one?"* — not merely
because it happened.

## 1447 — `Molten Hammer` (in canary)

| | |
|---|---|
| number | **1447** — from `choose_build(count, staged, override)`, **not** chosen. Read from the STAGE at cut time, not from any document. |
| word | **Molten** — `(1447/20) % 20 = 12`; noun **Hammer** — `1447 % 20 = 7` |
| controls reproduced | 341 Bellows · 342 Crucible · 345 Riveted Furnace · 905 Flux Furnace |
| first cut using this runbook | yes — §0–§6 were unexercised before this |
| names this cut wore before staging | `v346 Riveted Gear` → `1446 Molten Gear` → **1447 Molten Hammer** |

**The number moved three times, and the third move is the one that proves the rule.** 346 → 1446 was
the error below: a number treated as an input when it is an output. But 1446 → **1447** was not an
error at all, and nothing was done wrong to cause it. The ratchet's floor is
`git rev-list --count`, so it counts commits — and **merging the document that named the release
added a commit, which incremented the release it named.** #522 landed at count 1446 and left the
count at 1447. The version is the output of a function whose inputs include this very file.

The consequence is not "be careful", because care cannot help here — the increment happens *at merge*,
after the last moment anyone could edit the text. The consequence is structural, and it is why the
number is quarantined in a run record rather than stated in the document's identity:

> **A release number written in a document that has not merged yet is stale by construction.**
> The document is an input to the number. Read the number FROM THE STAGE at cut time (§1), record it
> here AFTER staging, and never let it into a filename, a heading, or a step.

Had the version stayed in the filename, this cut would have needed a *second* rename — and the rename
commit would have advanced the count again, to 1448. There is no fixed point. That is the whole
argument for the standing-document form, delivered by the machine rather than by me.

**What this run also caught, and why §1 now reads the way it does.** The first draft of §1 said *"bump
`version.txt` 345 → 346, and v346 is Riveted Gear."* Executing step 1 exposed it: `build.rs:72` reads
`env_or_file("SMOL_BUILD_NUMBER", "version.txt")` — **env wins** — and the release path supplies that
env from `choose_build`. So `version.txt` is the fallback for *non-stage* builds, and the ratchet would
have stamped 1446 while the docs said 346.

The half worth carrying forward is **why four naming controls did not catch it**: they verify the
**word against the number**, and say nothing about whether the *number* is right. `DOC-UPKEEP`'s
"name it WITH its sigil word so the pair self-checks" catches a *mismatched* pair; it cannot catch a
**correctly-derived word on the wrong number**, which is self-consistent and wrong.

> **A self-checking pair checks the RELATION, not the INPUTS.**

That sentence is the most expensive thing in this document, and it is the reason the deletion clause
had to go: it would have been thrown away with the doc at the exact cut that produced it.
