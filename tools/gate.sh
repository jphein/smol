#!/usr/bin/env bash
# #338 THE GATE. One script, run identically by CI (.github/workflows/fw-gate.yml) and by a human
# before pushing. It exists because the multi-tier gate everyone cited was PROSE in ONBOARDING.md —
# and on 2026-08-01 main was found red on its own `clippy -D warnings` rule with nobody aware, a new
# feature (`ledger-provision`) was added with no matrix to add it to, and two agents independently
# hand-built baselines to compute a clippy delta. A gate that lives in prose has already stopped
# running.
#
# ONBOARDING.md points AT this file rather than restating the commands, so the two cannot drift.
#
# WHAT IT COVERS
#   0. #350 the build-matrix declarations — the canonical tier matches REPRO_FLEET_FEATURES, every
#      buildable chip has a #348 ChipBudget (and vice versa), nothing ships that nothing builds,
#      and the matrix is one-axis-at-a-time rather than a cross product
#   1. cargo check --release across the build tiers — DERIVED from tools/build-matrix.toml, not
#      listed here (#350). Adding a chip or a tier is a data change in one file.
#   2. cargo clippy --release -D warnings on EVERY tier (#343 — was canonical-only), from the
#      same manifest, so the two lists cannot disagree the way they had already started to
#   3. the host experiments/*_verify suites
#   4. the #300 stack floor — the canonical ELF's .stack region vs the floor declared in
#      rust/clock/src/budget.rs (#348: one definition, parsed by repro_stack_floor). The number
#      is deliberately NOT repeated in this file, in build-matrix.toml, or anywhere else.
#   5. the crate's own tests/*.rs suites via cargo test (#350) — bard / budget / input. Nothing
#      ran these before: 57 tests, including the Bard's bit-for-bit golden, that no gate executed
#   6. tools/test_build_matrix.sh — proof that (0)'s arms can actually fail, and
#      tools/test_stack_floor.sh — proof the #413 per-chip floor lookup can fail (it replaced a
#      gate that was green while chip-blind)
#   7. #351 the BYTE-FREE tier claims, in two DELIBERATELY ASYMMETRIC arms. Read the asymmetry
#      before deleting either as a duplicate of the other:
#      (a) THE PROOF. build-matrix.toml's [tier_exclusive] must still agree with the
#          `#[cfg(feature = …)]` in the source, both directions. Zero build cost, LTO-proof, and
#          it catches the regression (b) structurally cannot — deleting a gate deletes the
#          derived claim along with it, which is exactly what a planted leak demonstrated:
#          src/net/target.rs entered the default tier (11 crate files → 12) and (b) said
#          "0 leaked".
#      (b) CORROBORATION ONLY, and unsound in the false-pass direction. Each tier is LINKED and
#          its DWARF line table is asked which source files contributed code. See the caveat
#          block below for why this is not the proof.
#   8. tools/test_check_exclusions.sh — proof that (7)'s arms can actually fail, including the
#      vacuous-green one (an ELF with no DWARF has an empty file set and every absence "holds")
#
# WHAT IT DOES NOT COVER — read this before trusting a green run:
#   * `mesh-test`: cannot RUN — it needs a per-board `DEAF_MACS` that only a real board.rs has.
#     It does now COMPILE, as a tier (#350). "Cannot run" had been silently widened to "cannot
#     build", which is how it went uncompiled for months.
#   * espflash `save-image` packaging: not run (no espflash in CI). The stack number does not depend
#     on it — it is read from the ELF — but image PACKAGING is therefore unproven here.
#   * Anything requiring hardware: OTA, radio, election, the stack-paint HIGH-WATER measurement.
#     ⚠️ The stack check bounds the linked REGION, not runtime high-water. A struct living in a
#     stack-resident RadioManager can cost ~1.8 KB of real stack and move the region by ~32 B. A
#     green stack line is NOT evidence of runtime headroom (see repro_stack_check's comment).
#   * #351 arm (b) DOES NOT MEASURE THE SHIPPED BINARY, and this is the caveat to read before
#     trusting its silence. It builds with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, and
#     that flag CHANGES THE IMAGE. Measured with an n/d/n sandwich on the fleet tier (harness:
#     tools/measure_debuginfo_delta.sh):
#         control  (no-debug vs no-debug):        3 of 1,027,770 ALLOC bytes differ
#                                                 all 3 in .flash.appdesc (the build stamp);
#                                                 every section same ADDRESS and same SIZE
#         treatment (no-debug vs line-tables):  464,180 of 1,027,816 differ  — and .text
#                                                 grew 871,618 -> 871,664 B  (+46)
#     The control is what makes that attributable: the tree holds still, so the delta is the
#     flag. And the SIZE change is the part that matters more than the 45% — byte churn at
#     constant size would be layout, harmless to a "which modules are present" verdict, whereas
#     a size change means CODEGEN differs, so inlining moved. Inlining is what line-table
#     attribution rides on. Arm (b) is therefore UNSOUND IN THE FALSE-PASS DIRECTION: when it
#     REFUSES, that is a true alarm worth having; when it is silent, it has proved less than it
#     appears to. Arm (a) is the load-bearing one. (An earlier version of this file claimed the
#     ALLOC sections came out byte-identical. That was measured on the pre-#233 tree, where the
#     SIZES happened to match, and it did not survive the dependency wave — a measurement
#     carried across a codegen-relevant boundary without being re-taken.)
#   * Even setting that aside, arm (b) proves no CODE from an excluded module survived. A module
#     of pure `const`/`static` items lands in .rodata with no line-table rows and is invisible to
#     it — `src/secrets.rs` is exactly that, and is declared in build-matrix.toml's
#     [unobservable] for the reason. "BYTE-FREE" is verified for executable bytes, not every byte.
#
# Usage:  tools/gate.sh              # everything
#         tools/gate.sh host         # host suites only (no riscv toolchain needed)
#         tools/gate.sh fw           # tiers + clippy + stack only
#         tools/gate.sh excl         # #351 tier exclusions only
# Env:    CARGO_TARGET_DIR honoured; SMOL_GATE_JOBS caps cargo parallelism.
#
# `excl` is its own mode because it is the only arm that LINKS every tier, and because it builds
# with a different cargo PROFILE (`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`) in a different
# target dir — folding that into `fw` would thrash the fingerprints the stack-floor build depends
# on and put two profiles' artifacts in one rust-cache. CI runs the two as parallel jobs. A human
# running `tools/gate.sh` still gets all of it.
#
# COST, measured rather than assumed: 179 s for all ten links on a cold GitHub runner. An earlier
# version of this comment said ~700 s and cited it as the reason for the split; that number came
# from a local experiment giving each tier its OWN target dir, so it paid for ten cold DEPENDENCY
# builds. The shipped path shares one dir and pays only the crate plus the LTO link per tier.
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
CLOCK="rust/clock"

# ── Temp artifacts live in the REPO, not in /tmp (JP directive 2026-08-25) ─────────────────────
# katana's `/tmp` is a 16 GB **tmpfs** — RAM+swap, not disk. This gate alone writes a log per tier
# per arm (checks, clippy, exclusions, host bins, test suites), and on 2026-08-25 accumulated temp
# artifacts filled 13 GB of that tmpfs and starved the machine. Logs are small; the rule is not
# about this gate's own footprint but about having ONE place per project where temp output goes, so
# nobody has to audit which script was the greedy one.
#
# `$ROOT/tmp` is git-ignored (`tmp/.gitignore` = `*` + `!.gitignore`) and is created on first need.
# Overridable so CI (whose `/tmp` is ordinary disk) or a caller with a scratch volume can redirect.
GATE_TMP="${SMOL_GATE_TMP:-$ROOT/tmp}"
mkdir -p "$GATE_TMP" || { echo "gate: cannot create $GATE_TMP" >&2; exit 2; }
# `mktemp` and every child process honour TMPDIR, so this covers the tools this script CALLS as
# well as its own redirects — which is the point: the callees are where the big artifacts appear.
export TMPDIR="$GATE_TMP"

# #363 — the root the COMPILING arms build from. `$ROOT` (i.e. your checkout) unless the pristine
# mirror below is engaged, in which case the tiers are built from a copy whose git-ignored
# provisioning matches what CI generates. Source-READING arms (shed order, DIAG budget, byte-free
# source scan, the verifier/ELECT checkers) keep using `$ROOT/$CLOCK`: they read TRACKED files,
# which are byte-identical in both trees, and reading them from a mirror would only add a way for
# the two to drift. Always defined, so a `host`-only run never trips `set -u`.
BUILD_ROOT="$ROOT"
WHAT="${1:-all}"
FAILED=()
run_fw=1; run_host=1; run_excl=1
case "$WHAT" in
  host) run_fw=0; run_excl=0 ;;
  fw)   run_host=0; run_excl=0 ;;
  excl) run_host=0; run_fw=0 ;;
  all)  ;;
  *) echo "usage: tools/gate.sh [all|host|fw|excl]" >&2; exit 2 ;;
esac

# shellcheck source=/dev/null
. "$ROOT/tools/repro_build.sh"   # REPRO_TARGET, REPRO_FLEET_FEATURES, repro_cargo_args, repro_stack_check

# #413: the stack arm measures the CANONICAL tier, so it must measure against the CANONICAL CHIP's
# floor — `repro_stack_check` no longer defaults to the C3, deliberately. Read from the manifest
# rather than written here, so there is no second copy of "the canonical chip is esp32c3".
CANON_CHIP="$("$ROOT/tools/build_matrix.py" canonical-chip 2>/dev/null || true)"
[ -n "$CANON_CHIP" ] || { echo "gate: could not resolve meta.canonical_chip from tools/build-matrix.toml" >&2; exit 2; }

JOBS=()
[ -n "${SMOL_GATE_JOBS:-}" ] && JOBS=(-j "$SMOL_GATE_JOBS")

# Where the stack verdict is left for a caller (CI lifts it into the PR job summary).
STACK_REPORT="${SMOL_GATE_STACK_REPORT:-$GATE_TMP/gate-stack-report}"
rm -f "$STACK_REPORT"

step() { printf '\n\033[1m── %s\033[0m\n' "$1"; }
ok()   { printf '   \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { printf '   \033[31mFAIL\033[0m %s\n' "$1"; FAILED+=("$1"); }

# #384 the VENDORED realm-sigil binding still matches upstream. Unconditional and first: it is
# nearly free, and every tier links these words, so drift here invalidates whatever the later arms
# measure.
#
# This arm exists because the CHECKER ALREADY EXISTED AND NOTHING RAN IT. `sigil_vendor.sh --check`
# was written carefully — manifest fails closed, upstream diff skips loudly when the sibling repo is
# absent so CI can always run it — and `grep -rn sigil_vendor .github/workflows/ tools/gate.sh`
# returned nothing. On 2026-08-15 the vendored copy was found stale by realm-sigil `1528f3a`, the
# u64-seed fix for a REAL divergence (a >8 hex-char hash overflowed u32, so rust and go named the
# same commit differently). `--check` had been reporting that correctly, exit 1 and all, to an
# audience of zero.
#
# Which is this file's own opening argument, one level out: a checker nobody invokes is the same
# shape as prose. It has already stopped running.
step "vendored realm-sigil matches upstream (#384)"
if out=$("$ROOT/tools/sigil_vendor.sh" --check 2>&1); then
  printf '%s\n' "$out" | tail -2; ok "sigil vendor"
else
  printf '%s\n' "$out" | sed 's/^/        /'; bad "sigil vendor"
fi

# #460 the committed Cargo.lock is actually consulted. Unconditional and early for the same reason as
# the arm above: if dependency RESOLUTION drifted, every later measurement — the tier checks, the
# stack floor, #390's symbol-size baseline — is about a graph nobody recorded.
#
# `mipidsi` is pinned `=0.10.0` on purpose (s3_oled's ORIENTATION reasoning is tied to 0.10's MADCTL
# semantics) and on 2026-08-26 the lock did not contain it at all, because no build path passes
# `--locked` and so every build silently rewrote the file. The strictest pin cargo offers, enforced by
# nobody. This is #460's option 2 — a gate arm rather than `--locked` spliced into every build
# invocation, which would change how everyone builds and collide with whichever lane holds those
# files. If `--locked` is ever adopted on the build paths, DELETE this arm; two statements of one fact
# is the thing half this file's history is about.
#
# THREE outcomes, not two (see the checker's header for the six measured flag/cache combinations):
# STALE is a hard FAIL; "could not check" (cargo absent, network unreachable) prints loudly and does
# NOT fail, because that state is the instrument being unavailable rather than a defect in the tree —
# and if cargo were genuinely missing, every arm below would fail on its own account.
step "committed Cargo.lock is consulted (#460)"
out=$("$ROOT/tools/check_lock_fresh.sh" 2>&1); lock_rc=$?
case "$lock_rc" in
  0) printf '%s\n' "$out" | sed 's/^/        /'; ok "lock fresh" ;;
  1) printf '%s\n' "$out" | sed 's/^/        /'; bad "lock fresh" ;;
  *) printf '%s\n' "$out" | sed 's/^/        /'
     printf '   \033[33mnote\033[0m %s\n' "lock freshness could not be measured (not a tree defect)" ;;
esac

# Both firmware halves need `board.rs`/`secrets.rs`, so this runs for either — `excl` on its
# own in CI would otherwise fail on the missing files rather than on anything it measures.
if [ "$run_fw" = 1 ] || [ "$run_excl" = 1 ]; then
  step "provisioning (git-ignored; existing files untouched)"
  "$ROOT/tools/ci_provision.sh" "$CLOCK" || { echo "provisioning failed" >&2; exit 1; }

  # #363 THE PRISTINE MIRROR — why this exists, because it is the whole point of the issue.
  #
  # `src/board.rs` and `src/secrets.rs` are GIT-IGNORED. CI generates them from the `.example`
  # files and nothing else; a developer's are whatever their bench needs. So when this gate lints
  # "the codebase" in a developer's checkout it is linting a file CI cannot see — and `clippy
  # -D warnings` promotes any local-only constant into a HARD FAILURE on a tier CI calls green.
  # Observed on `fleet`: three `WEATHER_*` constants, in no example and referenced by no source,
  # failing the tier as "constant is never used".
  #
  # That is not a lint bug, it is a MEASUREMENT bug: the gate was reporting on two different
  # inputs and calling both of them "the fleet tier". A gate that lints a file CI cannot see is
  # measuring two different things, and the verdict it prints does not mean one thing.
  #
  # It cannot be fixed in `board.rs` — there is no committed copy to fix. Every checkout has a
  # different one and none of them is in git. The divergence can therefore only be resolved
  # HERE, by choosing which of the two files the gate is willing to have an opinion about. It
  # chooses CI's, because CI's is the one derived from tracked content.
  #
  # ENGAGED ONLY WHEN IT CHANGES SOMETHING. If your provisioning declares nothing the examples do
  # not — always true in CI, and true for any developer who has not added local symbols — this is
  # a no-op and the tiers build from your tree exactly as before. So CI's build path, and its
  # `Swatinem/rust-cache` keying, are untouched: no cold dependency rebuild is introduced.
  #
  # NON-DESTRUCTIVE, which is load-bearing (#338: a gate people avoid is the failure this whole
  # file is about, and #359's promise is that real credentials survive running it). Your tree is
  # never written — the mirror is a COPY, and `board.rs`/`secrets.rs` are excluded from it so the
  # provisioner fills them from the examples. Deliberately NOT implemented by moving your file
  # aside and restoring it in a trap: a harness EXIT trap has already destroyed uncommitted work
  # in this repo once, and the file at risk here holds real fleet credentials.
  #
  # Stable path, so cargo's incremental state survives between runs (a mirror at a fresh path
  # every time would rebuild the world and make the gate something you skip).
  if [ -z "${SMOL_GATE_LOCAL_PROVISIONING:-}" ]; then
    extras="$("$ROOT/tools/ci_provision.sh" --list-extras "$CLOCK" 2>/dev/null)"
    if [ -n "$extras" ]; then
      step "pristine provisioning mirror (#363)"
      # `/var/tmp`, NOT `/tmp` and NOT `$TMPDIR`. The mirror is a build root: cargo puts a release
      # target dir under it, which is GBs, and `/tmp` is a small tmpfs on at least one machine this
      # repo is built on (familiar: 512 MB). Filling a tmpfs surfaces as a compile error that looks
      # like a code problem — the misattribution this file exists to prevent, and an expensive one
      # because the gate is what you reach for when you already distrust something else. `/var/tmp`
      # is the conventional home for large, longer-lived temp data and is not a tmpfs by default.
      # Override with SMOL_GATE_MIRROR when you want it somewhere specific.
      MIRROR="${SMOL_GATE_MIRROR:-/var/tmp/smol-gate-pristine-$(printf '%s' "$ROOT" | cksum | cut -d' ' -f1)}"
      n=$(printf '%s\n' "$extras" | wc -l)
      printf '   your provisioning declares %s symbol(s) the examples do not:\n' "$n"
      printf '%s\n' "$extras" | sed 's/^/       /'
      printf '   CI never compiles those, so linting your tree would answer a different question.\n'
      printf '   Building the tiers from a mirror provisioned like CI instead: %s\n' "$MIRROR"
      printf '   (SMOL_GATE_LOCAL_PROVISIONING=1 to lint YOUR files instead — useful when the\n'
      printf '    local-only symbol is one you are actively wiring up.)\n'
      # `rsync --delete` so a file deleted in the checkout cannot linger in the mirror and keep a
      # stale tier green. `--exclude` the target dirs (rebuilt in place) and the two git-ignored
      # provisioning files, which the provisioner then creates from the examples.
      # Say so if the mirror's filesystem cannot hold a release build. Cheap, and it converts the
      # failure JP named — "a full /tmp disguises itself as a compile error" — into one line that
      # names the actual cause. A WARNING, not a refusal: the space needed depends on which arms
      # run, and a gate that refuses to start on a guess is worse than one that tells you what it
      # is about to do. If it does fill up, this line is already on screen above the wreckage.
      avail_kb=$(df -Pk "$(dirname "$MIRROR")" 2>/dev/null | awk 'NR==2{print $4}')
      case "$avail_kb" in
        ''|*[!0-9]*) ;;
        *) [ "$avail_kb" -lt 4194304 ] && printf '   \033[33mNOTE\033[0m %s has %s MB free — a release target dir wants GBs.\n         If a tier fails with something that makes no sense, suspect this first,\n         or point SMOL_GATE_MIRROR somewhere roomier.\n' "$(dirname "$MIRROR")" "$((avail_kb / 1024))" ;;
      esac
      # rsync does not create nested destination parents (only the final component), so make them
      # first — without this the very first run of a fresh mirror fails on `mkdir "…/rust/clock"`.
      if mkdir -p "$MIRROR/$CLOCK" "$MIRROR/rust/sigil-names" \
         && rsync -a --delete \
            --exclude='target/' --exclude='src/board.rs' --exclude='src/secrets.rs' \
            "$ROOT/$CLOCK/" "$MIRROR/$CLOCK/" \
         && rsync -a --delete --exclude='target/' \
            "$ROOT/rust/sigil-names/" "$MIRROR/rust/sigil-names/" \
         && "$ROOT/tools/ci_provision.sh" "$MIRROR/$CLOCK" >/dev/null; then
        BUILD_ROOT="$MIRROR"
        ok "mirror provisioned (tiers below build from it, not from your tree)"
      else
        bad "mirror provisioning (#363) — falling back to linting your tree"
      fi
    fi
  fi
fi

if [ "$run_fw" = 1 ]; then
  # Cheap and first, so a source-consistency error is not buried behind ten minutes of LTO.
  # #339: the prose describing the DIAG shed order had drifted from the appends it described, in a
  # direction that would send an operator to the wrong fields. Prose cannot be tested; this can.
  step "DIAG shed order matches its declaration (#339)"
  if out=$("$ROOT/tools/check_shed_order.py" "$CLOCK/src/net/mode.rs" 2>&1); then
    printf '%s\n' "$out"; ok "shed order"
  else
    printf '%s\n' "$out"; bad "shed order"
  fi

  # #306: the DIAG record is a CLIFF — over budget, `encode_publish` publishes NOTHING and a
  # healthy board looks dead. `DIAG_CORE_MAX` is the compile-time proof it cannot get there, but its
  # operands were a hand-summed comment and had drifted 8 B in the unsafe direction while the file
  # carried three different margins in prose. This proves the declared per-field widths ARE the
  # record's fields, and PRINTS the margin rather than saying "green". Its arms are proved able to
  # fail by the step immediately below — which RUNS the harness. This sentence used to say
  # "`tools/test_diag_budget.sh` proves each arm can fail" while nothing here executed it, and that
  # is not a harmless wording: an audit asking "which test_*.sh appear in gate.sh" sees the name,
  # scores the suite covered, and moves on. #400's audit of exactly that shape found four unwired
  # suites and did not find this one. A comment that ASSERTS a property reads the same as a check
  # that ENFORCES it — so never name a self-test here except on the line that invokes it.
  step "DIAG budget arithmetic matches the record (#306)"
  if out=$("$ROOT/tools/check_diag_budget.py" "$CLOCK/src/net/mode.rs" 2>&1); then
    printf '%s\n' "$out"; ok "diag budget"
  else
    printf '%s\n' "$out"; bad "diag budget"
  fi

  # #382: and now the sabotage harness actually RUNS. It has existed since #306 and this gate
  # only ever MENTIONED it — the comment above said "proves each arm can fail" while nothing
  # executed it, which is the precise distinction the merge-guard step below calls "the difference
  # between 'the guard HAS a self-test' and 'the self-test RUNS'". 18 arms, mktemp copies only,
  # no network, ~2 s.
  #
  # It is wired here rather than in a sweep because this gate's own DIAG check was just extended
  # (the DERIVED read-out), and an unrun harness would have made those new arms decorative — a
  # self-test nobody runs is indistinguishable from one that cannot fail.
  step "DIAG budget checker regression suite (#306/#382)"
  if out=$("$ROOT/tools/test_diag_budget.sh" 2>&1); then
    printf '%s\n' "$out" | tail -1; ok "test_diag_budget"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_diag_budget"
  fi

  # #367: a host verifier `#[path]`-includes a firmware source, which reads a FILE and does not
  # care whether the crate declares it as a module. So a verifier can be green against code that
  # is compiled into NO tier — `net/crdt.rs` (#185) is exactly that today, and the gate could not
  # tell it apart from working code. This asserts the one bit that closes it: every `#[path]`
  # target is also a declared module. Known, owned exceptions live in the script's KNOWN_PHANTOMS
  # with their issue, and the check fails BOTH on a new phantom and on an allowlist entry that has
  # since been wired — a list that outlives its reason is how a check stops checking.
  step "host verifiers test code the firmware compiles (#367)"
  if out=$("$ROOT/tools/check_verifier_wiring.py" "$ROOT" 2>&1); then
    printf '%s\n' "$out"; ok "verifier wiring"
  else
    printf '%s\n' "$out"; bad "verifier wiring"
  fi

  # #278/#269: the `SMOLv1 ELECT` frame can RETUNE a leaf's radio, and a leaf cannot verify an
  # announcement before acting on it — esp-radio 0.18 hardcodes `coex_background_scan: false`, so
  # checking costs the association it would need if the announcement were false. An unauthenticated
  # ELECT is therefore a remote fleet-stranding primitive, and #190's group-MAC trailer is appended
  # only on the `send_to` path. Stage 1 wrote that down as prose. This reads source STRUCTURE
  # instead: one sink impl, routed to `send_to`; a sealed frame with no byte accessor; one encoder
  # call site; one spelling of the prefix; and every raw `esp_now.send` declared WITH ITS COUNT.
  # #403 added arm 7 in the same idiom, one invariant over: every MC crown announce is declared
  # (`MC-PUBLISH-SITES`, counted both ways) AND formats the `NonZeroU8`-bound channel. An unknown
  # channel published as the fleet's rendezvous stranded every relay-only leaf for 4 h; both sites
  # live inside the ~2,000-line body #335 STEP T rewrites atomically, which is why the guard is a
  # type and this arm counts rather than inspecting the sites it already knows.
  # `tools/test_check_elect_send_path.sh` proves all of those can fail, plus five fail-closed arms.
  # #335 STEP G. Same idiom, one invariant over: exactly ONE consumer of the STA transport is live
  # per tier. `esp_radio::wifi::Interface` is `Copy`, so `embassy_net::new(station, ..)` does not
  # consume it — a `Stack` can be live beside the `SmolWifiDevice` shim over the same interface, it
  # COMPILES CLEAN, and then both pop `data_queue_rx()` (keyed by `InterfaceType` alone) and steal
  # each other's frames with no error and nothing in a log. Placed here, beside the ELECT checker
  # and before any compile, because it is pure text and because STEP T needs a green baseline to
  # diff against. `tools/test_check_station_consumers.sh` proves all four arms can fail.
  step "one live STA transport consumer per tier (#335 STEP G)"
  if out=$("$ROOT/tools/check_station_consumers.py" "$ROOT" 2>&1); then
    printf '%s\n' "$out"; ok "station consumers"
  else
    printf '%s\n' "$out"; bad "station consumers"
  fi

  step "ELECT reaches the air only authenticated (#278)"
  if out=$("$ROOT/tools/check_elect_send_path.py" "$ROOT" 2>&1); then
    printf '%s\n' "$out"; ok "elect send path"
  else
    printf '%s\n' "$out"; bad "elect send path"
  fi

  # #350: cheap and before any compile, because everything below DERIVES from this manifest —
  # a gate that builds the wrong tier list confidently is worse than one that refuses to start.
  # The arms: the canonical tier matches REPRO_FLEET_FEATURES (the packaging path's own
  # definition); every buildable chip has a #348 ChipBudget and every ChipBudget has a chip row;
  # nothing ships that nothing builds; the emitted matrix is one-axis-at-a-time, not a cross
  # product. `tools/test_build_matrix.sh` (host half) proves each of those can fail.
  step "build matrix declarations (#350)"
  if out=$("$ROOT/tools/build_matrix.py" check 2>&1); then
    printf '%s\n' "$out"; ok "build matrix"
  else
    printf '%s\n' "$out"; bad "build matrix"
  fi

  # The tiers. `default` is the always-green baseline; `wifi`/`espnow` are the documented rungs;
  # the canonical fleet tier is what actually ships. A feature added to Cargo.toml and NOT added
  # here is a code path nothing compiles — the `ledger-provision` case that motivated this issue.
  # Any feature listed in Cargo.toml's [features] that is not covered below should be added.
  # #347: `bard` left the canonical fleet list (it starves the C3's runtime stack), so WITHOUT an
  # explicit tier here nothing would compile it — precisely the "code path nothing compiles" case
  # this gate exists to prevent, and it would rot exactly while it is being kept alive for the S3
  # and C6. The tier is `bard` PLUS the fleet list rather than `bard` alone, because the build that
  # has to keep working on a bigger chip is the COMBINED one. `bard` is named first so the
  # feature-not-on-this-branch SKIP below tests for `bard` and not for `espnow`.
  # #348: the bard tier now also carries `off-fleet`. That feature is the DECLARATION that this
  # build is not the C3 fleet image — which is precisely what this tier is — and without it the
  # #348 budget predicate refuses to compile the Bard on a C3, killing the tier that exists to
  # keep it alive. Drop `off-fleet` here and you will see the guard fire; that is the check
  # working, not the tier breaking. `repro_build_bin` refuses to PACKAGE anything naming it, so
  # this cannot leak into a shipped image.
  # #350: the tier list is DERIVED from tools/build-matrix.toml, not written here. It used to
  # be a literal in this loop and a second literal in the clippy loop below, and the two had
  # already drifted — `ledger-provision` was in one and not the other, so that tier compiled
  # but was never linted. One declaration, two consumers, no third place to forget.
  # #351: the BYTE-FREE claims. Cheap and before any compile, like the shed order — arm (a)
  # is pure source structure, so it costs nothing and runs everywhere. The symbol arm needs a
  # linked ELF and runs after the stack step below. See check_byte_free.py's header for why
  # the two are NOT redundant and which one is load-bearing.
  step "byte-free claims have a mechanism (#351)"
  if out=$("$ROOT/tools/check_byte_free.py" --src "$CLOCK/src" 2>&1); then
    printf '%s\n' "$out"; ok "byte-free (source)"
  else
    printf '%s\n' "$out"; bad "byte-free (source)"
  fi

  step "cargo check — build tiers"
  while IFS=$'\t' read -r name feats; do
    # A tier naming a feature this branch does not have (e.g. ledger-provision before #181 lands)
    # is SKIPPED, not failed — the gate must work on both sides of that merge.
    if [ -n "$feats" ] && ! grep -qE "^${feats%%,*} *=" "$BUILD_ROOT/$CLOCK/Cargo.toml"; then
      printf '   \033[33mSKIP\033[0m %-16s (feature not in Cargo.toml on this branch)\n' "$name"; continue
    fi
    args=(--release "${JOBS[@]}"); [ -n "$feats" ] && args+=(--features "$feats")
    if (cd "$BUILD_ROOT/$CLOCK" && cargo check "${args[@]}") >"$GATE_TMP/gate-$name.log" 2>&1; then
      ok "check $name"
    else
      bad "check $name"; tail -15 "$GATE_TMP/gate-$name.log" | sed 's/^/        /'
    fi
  done < <("$ROOT/tools/build_matrix.py" emit --for check)

  # `-D warnings` on EVERY tier, not just the canonical one. Until #343 this ran on the canonical
  # tier alone because `default`/`wifi` carried dead-code findings (symbols whose only callers are
  # espnow-gated); those now carry item-scoped `#[allow(dead_code)]` with a cited reason, so the
  # gate can cover what ONBOARDING has claimed all along. A tier that only gets `cargo check` has
  # its new warnings invisible — which is how the findings accumulated unnoticed in the first place.
  # #350: derived from the same manifest as the check loop, so "every tier" is now literally
  # true instead of "every tier someone remembered to add here". The fleet tier is named
  # `fleet` in both loops now — it was `canonical` here and `fleet` above, which is why the
  # two lists could disagree without looking like they did. Log path moves with the name:
  # $GATE_TMP/gate-clippy-fleet.log, was $GATE_TMP/gate-clippy-canonical.log (and both were
  # under /tmp until the 2026-08-25 tmpfs directive — see the GATE_TMP block at the top).
  step "cargo clippy -D warnings — every tier"
  while IFS=$'\t' read -r name feats; do
    if [ -n "$feats" ] && ! grep -qE "^${feats%%,*} *=" "$BUILD_ROOT/$CLOCK/Cargo.toml"; then
      printf '   \033[33mSKIP\033[0m %-16s (feature not in Cargo.toml on this branch)\n' "$name"; continue
    fi
    args=(--release "${JOBS[@]}"); [ -n "$feats" ] && args+=(--features "$feats")
    if (cd "$BUILD_ROOT/$CLOCK" && cargo clippy "${args[@]}" -- -D warnings) >"$GATE_TMP/gate-clippy-$name.log" 2>&1; then
      ok "clippy $name"
    else
      bad "clippy $name"
      grep -E "^(error|warning)" "$GATE_TMP/gate-clippy-$name.log" | grep -v "could not compile" \
        | sort -u | sed 's/^/        /' | head -12
    fi
  done < <("$ROOT/tools/build_matrix.py" emit --for clippy)
fi

if [ "$run_excl" = 1 ]; then
  # #351 the BYTE-FREE claims. Cargo.toml, net.rs and main.rs all assert that a lower tier
  # contains none of a higher tier's code ("the default build is BYTE-FREE of it (symbol-
  # absence provable)"), and until now nothing checked a single one of them — the same
  # species of comment as this file's old "any feature not covered below should be added",
  # which #350 found to be false for three features at once.
  #
  # The claims are DERIVED from the `#[cfg(feature = …)]` on each `mod` declaration, walked
  # transitively from src/main.rs, so there is no second hand-written list to drift. The
  # tiers come from the same manifest as the two loops above.
  #
  # This arm LINKS each tier, which the check/clippy loops do not, because the property is about
  # what survives into the binary. Three things to know before reading a green line:
  #   * ⚠️ IT IS NOT THE SHIPPED BINARY. It needs DWARF, and `[profile.release]` sets
  #     `debug = false`, so the builds carry CARGO_PROFILE_RELEASE_DEBUG=line-tables-only — and
  #     that flag CHANGES THE IMAGE: 464,180 of 1,027,816 ALLOC bytes differ and .text grows
  #     +46 B (871,618 -> 871,664), against a control pair differing by 3 bytes of build stamp.
  #     A size change means codegen differs, so inlining moved, and inlining is what line-table
  #     attribution rides on. Hence: CORROBORATION, not proof — see the file header. A refusal
  #     here is a true alarm; silence proves less than it looks like it does.
  #   * it REFUSES an ELF with no DWARF rather than reading an empty file set and calling it
  #     clean. That is the vacuous-green trap and it is the default state of a smol build.
  #   * a separate CARGO_TARGET_DIR, so it cannot invalidate the fingerprints the stack-floor
  #     build below depends on. The ELF is copied out after each tier because the next tier
  #     overwrites it.
  step "tier exclusions — corroboration on a debug-instrumented build (#351)"
  # Keyed PER WORKTREE: a shared dir cross-pollutes even SERIALLY — a stale tier ELF carries
  # comp_dir from whichever worktree built it last, and check_exclusions then fails a healthy
  # tree (seen 2026-08-02: lucid chased a spurious comp_dir failure to exactly this). Per-
  # worktree dirs keep rebuilds warm per checkout, kill cross-worktree pollution AND remove
  # cross-worktree lock contention; the flock below still serializes same-worktree runs.
  EXCL="${SMOL_GATE_EXCL_DIR:-$GATE_TMP/gate-exclusions-$(printf %s "$ROOT" | cksum | cut -d' ' -f1)}"
  mkdir -p "$EXCL/elf"
  # Concurrent gates sharing the default $EXCL wipe each other's build tree mid-compile and
  # report failures that have nothing to do with the code (seen 2026-08-02: "Blocking waiting
  # for file lock" then "couldn't create a temp dir" — the OTHER gate deleted the dir). The
  # dir stays FIXED so warm rebuilds stay warm; an flock serializes users instead. A second
  # gate blocks here until the first finishes — slower, never corrupted. Private-dir runs
  # (SMOL_GATE_EXCL_DIR=...) take their own lock and don't contend.
  exec 9>"$EXCL.lock"
  flock 9 || { bad "tier exclusions (could not take $EXCL.lock)"; exit 1; }
  EXCL_ARGS=()
  while IFS=$'\t' read -r name feats; do
    if [ -n "$feats" ] && ! grep -qE "^${feats%%,*} *=" "$BUILD_ROOT/$CLOCK/Cargo.toml"; then
      printf '   \033[33mSKIP\033[0m %-16s (feature not in Cargo.toml on this branch)\n' "$name"; continue
    fi
    args=(--release --bin clock "${JOBS[@]}"); [ -n "$feats" ] && args+=(--features "$feats")
    if (cd "$BUILD_ROOT/$CLOCK" && CARGO_TARGET_DIR="$EXCL/target" \
          CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build "${args[@]}") \
          >"$GATE_TMP/gate-excl-$name.log" 2>&1; then
      cp "$EXCL/target/$REPRO_TARGET/release/clock" "$EXCL/elf/$name"
      EXCL_ARGS+=(--elf "$name=$feats=$EXCL/elf/$name")
    else
      bad "exclusions build $name"; tail -15 "$GATE_TMP/gate-excl-$name.log" | sed 's/^/        /'
    fi
  done < <("$ROOT/tools/build_matrix.py" emit --for check)
  if [ ${#EXCL_ARGS[@]} -eq 0 ]; then
    bad "tier exclusions (nothing built — no tier could be measured)"
  elif out=$("$ROOT/tools/check_exclusions.py" check "${EXCL_ARGS[@]}" 2>&1); then
    printf '%s\n' "$out"; ok "tier exclusions"
  else
    printf '%s\n' "$out"; bad "tier exclusions"
  fi
fi

if [ "$run_fw" = 1 ]; then
  # #300 stack floor, measured with the SAME function the packaging path uses (repro_stack_check).
  # Built with repro_cargo_args so the ELF matches the shipped one's geometry.
  # #348: the title reads the floor from the same single definition repro_stack_check will use —
  # it was a third hardcoded copy of 73728, which would have gone on announcing the old number
  # while the gate enforced a new one. "unreadable" here is not a failure; repro_stack_check
  # fails closed on it a few lines down, with the diagnostic.
  step "stack floor — canonical ELF vs ${REPRO_STACK_FLOOR:-$(repro_stack_floor "$CANON_CHIP" || echo unreadable)} B"
  if repro_cargo_args "$BUILD_ROOT/$CLOCK" 2>/dev/null && \
     (cd "$BUILD_ROOT/$CLOCK" && cargo build --release "${JOBS[@]}" --features "$REPRO_FLEET_FEATURES" "${REPRO_CARGO_ARGS[@]}") \
       >"$GATE_TMP/gate-stack.log" 2>&1; then
    ELF="${CARGO_TARGET_DIR:-$BUILD_ROOT/$CLOCK/target}/${REPRO_TARGET}/release/clock"
    # Capture the verdict to a file as well as the console: CI lifts it into the job summary so the
    # number is visible without opening logs. Written on FAILURE too — "the stack broke the floor"
    # is the case a reader most needs surfaced, and a report that only exists when green would hide
    # exactly that.
    if out=$(repro_stack_check "$ELF" "$CANON_CHIP" 2>&1); then
      printf '%s\n' "$out"; printf '%s\n' "$out" > "$STACK_REPORT"; ok "stack floor"
    else
      printf '%s\n' "$out"; printf '%s\n' "$out" > "$STACK_REPORT"; bad "stack floor"
    fi
  else
    bad "stack floor (canonical build failed)"
    echo "canonical build failed before the stack could be measured" > "$STACK_REPORT"
    tail -15 "$GATE_TMP/gate-stack.log" | sed 's/^/        /'
  fi
  # #351 arm (b): CORROBORATION on the ELF the stack step just built — the real, non-debug,
  # shipped-geometry binary. The excluded-module list is DERIVED from the tier's features
  # (transitively closed over Cargo.toml) rather than hand-listed. This arm cannot PROVE
  # absence — fat LTO can inline a linked module until no symbol survives — so a pass here
  # adds confidence and never stands alone. Skipped, not failed, if the build above did not
  # produce an ELF: a corroboration arm must not invent a verdict.
  # #438/#335: the stack-paint instrument's own VALIDITY, machine-checked instead of prose.
  #
  # #438's warrant was a sentence: "its `.stack` is byte-identical to the fleet tier's (86,200 B),
  # so the instrument costs zero DRAM and its peak is a peak for the shipped image rather than for
  # a near-miss of it." On 2026-08-27 that quietly stopped being true — STEP T moved `.bss` by
  # different amounts on the two tiers and the regions came apart by 1,136 B, with nothing
  # announcing it because nothing was checking. The peak read off that instrument was about to
  # decide a ship gate at a ~1 KB margin. #434 is the precedent for what a near-miss instrument
  # costs; this is the check that would have caught its return.
  #
  # Byte-identity is NOT what is asserted — that would be demanding a coincidence back. The
  # checkable claim is that the measurement errs SAFE: `paint_region <= fleet_region`, so the
  # instrument always runs with at-most-as-much room as the image we ship.
  #
  # ⚠️ Built into its OWN CARGO_TARGET_DIR on purpose. The canonical ELF lives at
  # `<target>/<triple>/release/clock` and a paint build with the shared target dir would OVERWRITE
  # it — after which the byte-free arm below would silently measure the paint image instead of the
  # shipped one. Separate dir, and this step is placed AFTER the stack floor read for the same
  # reason. (It also cannot reuse the `excl` arm's binaries: those are built with
  # `line-tables-only` debug, which this file's own header records as CHANGING the image.)
  step "paint-instrument warrant — paint vs fleet region (#438)"
  PAINTDIR="${SMOL_GATE_PAINT_DIR:-$GATE_TMP/gate-paint-$(printf %s "$ROOT" | cksum | cut -d' ' -f1)}"
  FLEET_ELF="${CARGO_TARGET_DIR:-$BUILD_ROOT/$CLOCK/target}/${REPRO_TARGET}/release/clock"
  if [ ! -f "$FLEET_ELF" ]; then
    printf '   \033[33mSKIP\033[0m paint warrant — no canonical ELF to compare against\n'
  # ⚠️ `ESP_LOG=info` is NOT optional and NOT cosmetic. `ESP_LOG` is COMPILE-TIME here: without it
  # the sentinel is compiled IN and its report line is compiled OUT, so the board runs the
  # instrument and says nothing. An arm that built the paint tier without it would compare two
  # images NEITHER of which we flash, see byte-identity, and pass — validating a build nobody
  # measures with, which is the exact defect class this arm exists to catch. (2026-08-27: two
  # handoff images shipped in precisely that state and passed md5, region, symbol-count and seed
  # checks. Every number looked right; the soak would have returned nothing.)
  elif (cd "$BUILD_ROOT/$CLOCK" && CARGO_TARGET_DIR="$PAINTDIR" ESP_LOG=info \
          cargo build --release --bin clock "${JOBS[@]}" \
          --features "stack-paint-lite,$REPRO_FLEET_FEATURES" "${REPRO_CARGO_ARGS[@]}") \
        >"$GATE_TMP/gate-paint.log" 2>&1; then
    if out=$("$ROOT/tools/check_paint_warrant.py" --fleet-elf "$FLEET_ELF" \
               --paint-elf "$PAINTDIR/${REPRO_TARGET}/release/clock" 2>&1); then
      printf '%s\n' "$out"; ok "paint warrant"
    else
      printf '%s\n' "$out"; bad "paint warrant"
    fi
  else
    bad "paint warrant (stack-paint-lite build failed)"
    tail -15 "$GATE_TMP/gate-paint.log" | sed 's/^/        /'
  fi

  step "byte-free corroboration — symbols in the canonical ELF (#351)"
  ELF="${CARGO_TARGET_DIR:-$BUILD_ROOT/$CLOCK/target}/${REPRO_TARGET}/release/clock"
  if [ -f "$ELF" ]; then
    if out=$("$ROOT/tools/check_byte_free.py" --src "$CLOCK/src" --elf "$ELF" \
               --features "$REPRO_FLEET_FEATURES" 2>&1); then
      printf '%s\n' "$out" | tail -1; ok "byte-free (symbols)"
    else
      printf '%s\n' "$out"; bad "byte-free (symbols)"
    fi
  else
    printf '   \033[33mSKIP\033[0m byte-free (symbols) — no ELF from the canonical build\n'
  fi

  # #390: the writable-static size baseline, on the SAME ELF the two arms above just used — no
  # extra build. This generalises the #300 stack floor rather than duplicating it: the floor
  # watches one derived aggregate (.stack) against a threshold, this watches the individual
  # statics whose growth is what MOVES that aggregate. The floor says the roof came down; this
  # says which box grew.
  #
  # It exists because #335 P1.1 added six dependency lines and changed no source, and smoltcp's
  # `async` feature (unified in from embassy-net) grew net::wifi::NTP_SOCK_STORAGE by 64 B of
  # .data taken out of the .stack region — invisible to code review (no source diff), to the
  # compiler (no arity change), to repro_stack_check (0.06% of an aggregate), and to #351's
  # checker (presence, not size). It was found by a hand `nm` A/B, which is not a process.
  #
  # The TIER NAME IS DERIVED from the feature set rather than written twice. That is deliberate
  # beyond tidiness: change REPRO_FLEET_FEATURES and the baseline filename changes with it, so
  # the checker reports "no baseline — a gap to close, not a pass" instead of silently comparing
  # the new tier against the old tier's numbers.
  step "writable-static sizes — canonical ELF vs the #390 baseline"
  if [ -f "$ELF" ]; then
    if out=$("$ROOT/tools/check_symbol_sizes.py" --tier "${REPRO_FLEET_FEATURES//,/-}" \
               --elf "$ELF" 2>&1); then
      printf '%s\n' "$out" | tail -1; ok "symbol sizes"
    else
      # Both 1 (drift) and 2 (could not check) are failures here, and the message distinguishes
      # them. A resize is not automatically a bug — but it must not land unreviewed, which is the
      # entire finding of #390.
      printf '%s\n' "$out" | sed 's/^/        /'; bad "symbol sizes"
    fi
  else
    # SKIP, not fail: the stack-floor arm above already failed loudly on a build that produced no
    # ELF, so the gate is red regardless and a second red says nothing new. This is the one shape
    # of skip that is not a vacuous pass — the condition is already reported by a neighbour.
    printf '   \033[33mSKIP\033[0m symbol sizes — no ELF from the canonical build\n'
  fi

  # #420: identity honesty, exercised rather than asserted. Two boards reported build 345 while
  # running current code and ~40 min of incident forensics went into a "345-era crown" theory the
  # number could never have supported. The fix made the unresolvable-hash case say `nogit` instead
  # of the constant `dev`, and made a build claiming SMOL_RELEASE=1 with no resolvable commit REFUSE.
  # Neither behaviour was reachable by any existing arm, so it shipped demonstrated-by-hand — and a
  # guard whose failing path nobody has watched is a guard nobody has watched fail.
  #
  # GIT_DIR=/nonexistent is the whole trick: `git rev-parse` then fails exactly as it does in a
  # `git archive` export or an rsync mirror with no .git, which is what build.rs's env_or_git sees.
  # It costs one env var — no tree copy. A copied tree would ALSO defeat the warm target dir,
  # because cargo's unit hashes include the path (#327), so every dependency would rebuild.
  #
  # ⚠️ THIS ARM MUST STAY LAST IN THE fw BLOCK. It re-runs build.rs with a different BUILD_HASH,
  # which changes the crate's fingerprint — so the canonical ELF the three arms above consume is
  # stale afterwards. Ordering is load-bearing, not cosmetic.
  step "identity honesty — an archive-like build says 'nogit', a release without identity REFUSES (#420)"
  if repro_cargo_args "$BUILD_ROOT/$CLOCK" 2>/dev/null; then
    _id_ok=1
    # (a) claiming a RELEASE with no resolvable commit must FAIL, and say why.
    if out=$( cd "$BUILD_ROOT/$CLOCK" && GIT_DIR=/nonexistent SMOL_RELEASE=1 \
                cargo check --release "${JOBS[@]}" --features "$REPRO_FLEET_FEATURES" \
                "${REPRO_CARGO_ARGS[@]}" 2>&1 ); then
      echo "        a RELEASE build with no resolvable identity SUCCEEDED — the #420 refusal is gone"
      _id_ok=0
    else
      case "$out" in
        *"#420"*) printf '        refuses a release with no identity\n' ;;
        *) echo "        build failed, but NOT with the #420 refusal — refusing to read an"
           echo "        unrelated build error as proof the guard fired:"
           printf '%s\n' "$out" | tail -5 | sed 's/^/          /'; _id_ok=0 ;;
      esac
    fi
    # (b) the same tree, NOT claiming a release, must build and stamp the absence by name. This is
    # the direction that matters for forensics: `dev` was identical for every such build ever made.
    if out=$( cd "$BUILD_ROOT/$CLOCK" && GIT_DIR=/nonexistent \
                cargo check --release "${JOBS[@]}" --features "$REPRO_FLEET_FEATURES" \
                "${REPRO_CARGO_ARGS[@]}" 2>&1 ); then
      _bs="${CARGO_TARGET_DIR:-$BUILD_ROOT/$CLOCK/target}/${REPRO_TARGET}/release/build"
      if grep -qhs 'BUILD_HASH=nogit' "$_bs"/clock-*/output; then
        printf '        stamps BUILD_HASH=nogit when the commit is unresolvable\n'
      else
        echo "        an unidentifiable build did NOT stamp 'nogit' — the fallback is back to"
        echo "        something that reads like a value:"
        grep -hs 'BUILD_HASH=' "$_bs"/clock-*/output | sort -u | sed 's/^/          /'
        _id_ok=0
      fi
    else
      echo "        a dev build with no resolvable identity failed to compile — 'nogit' is meant"
      echo "        to be BUILDABLE; only the release claim refuses:"
      printf '%s\n' "$out" | tail -5 | sed 's/^/          /'; _id_ok=0
    fi
    [ "$_id_ok" = 1 ] && ok "identity honesty" || bad "identity honesty"
  else
    bad "identity honesty (could not resolve cargo args)"
  fi
fi

if [ "$run_host" = 1 ]; then
  # Every experiments/*_verify is a standalone host crate that panics on failure. Discovered by
  # GLOB, not by a hardcoded list: a new verifier is picked up automatically, which is the whole
  # point — the last list-shaped gate silently stopped covering what was added after it.
  step "host verifier suites"
  for d in "$ROOT"/experiments/*/; do
    [ -f "$d/Cargo.toml" ] || continue
    n=$(basename "$d")
    case "$n" in *_verify|relay_compat) ;; *) continue ;; esac
    if (cd "$d" && cargo run --release -q "${JOBS[@]}") >"$GATE_TMP/gate-$n.log" 2>&1; then
      ok "$n"
    else
      bad "$n"; tail -10 "$GATE_TMP/gate-$n.log" | sed 's/^/        /'
    fi
  done

  # #350: the crate's OWN test suites — `rust/clock/tests/*.rs`. Found while wiring the matrix:
  # NOTHING in tools/ or .github/ ran `cargo test`, so `tests/bard.rs` (the Bard's bit-for-bit
  # golden against an independent reference), `tests/input.rs`, and #348's brand-new
  # `tests/budget.rs` were three suites no gate executed. #348's PR reported "8/8 host tests"
  # from a manual run — which is exactly how a suite stops being evidence.
  #
  # `--no-default-features` is LOAD-BEARING, not tidiness: the default features pull the
  # bare-metal stack, and `portable-atomic`'s `unsafe-assume-single-core` then leaks into the
  # HOST build and hard-errors ("not supported yet on this architecture"). Drop the flag and
  # this arm fails on a healthy tree.
  #
  # Discovered by GLOB for the same reason the loop above is: a list would stop covering what
  # is added after it.
  step "crate host test suites (cargo test)"
  HOST_TRIPLE="${SMOL_HOST_TRIPLE:-$(rustc -vV | sed -n 's/^host: //p')}"
  for t in "$ROOT/$CLOCK"/tests/*.rs; do
    [ -f "$t" ] || continue
    n=$(basename "$t" .rs)
    if (cd "$ROOT/$CLOCK" && cargo test --no-default-features --features hostsim \
          --target "$HOST_TRIPLE" --test "$n" "${JOBS[@]}") >"$GATE_TMP/gate-test-$n.log" 2>&1; then
      # Print the count rather than a bare PASS: "8 passed" is checkable, "ok" is not, and a
      # suite that silently starts running ZERO tests still says ok.
      ok "test $n — $(grep -Eo '[0-9]+ passed' "$GATE_TMP/gate-test-$n.log" | tail -1)"
    else
      bad "test $n"; tail -12 "$GATE_TMP/gate-test-$n.log" | sed 's/^/        /'
    fi
  done

  # #350: prove the matrix checker's arms can fail. Pure text, no cargo — see the file header
  # for why a green-only demonstration is not evidence.
  # #351: prove the byte-free source arm can fail. Pure text, no cargo.
  step "byte-free checker regression suite (#351)"
  if out=$("$ROOT/tools/test_byte_free.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_byte_free"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_byte_free"
  fi

  # #413: the per-chip stack-floor lookup, and the same "prove it can fail" discipline. This one
  # earns it especially: the OLD chip-blind gate was GREEN throughout and its verdict happened to
  # be right, because the C3's floor is the highest of the three so a chip-blind check was stricter
  # than a non-C3 chip's own. Nothing in a passing run could have revealed that. The suite uses a
  # `readelf` stub, so it can put a .stack inside the 2,204 B false-REJECT window no real image
  # occupies. Pure text, no cargo, no ELF.
  step "per-chip stack-floor regression suite (#413)"
  if out=$("$ROOT/tools/test_stack_floor.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_stack_floor"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_stack_floor"
  fi

  # #280: prove the stale-config guard's arms can fail. This one is here because the guard SHIPPED
  # with only a by-hand demonstration, and the reviewer named the hole precisely: familiar's
  # `.cargo/config.toml` is currently NOT stale, so a green run there proves nothing about catching
  # anything. The failing state is a FIXTURE — the real 2026-08-25 drift, reconstructed — so the
  # refusal path runs on every gate rather than when the world happens to be broken.
  #
  # Its fourth arm is the one to keep: it asserts that a whole-file grep PASSES on that same
  # fixture, which is what makes the section scope load-bearing rather than stylistic. Anyone who
  # "simplifies" the guard to a file-wide grep fails there. Pure text, no cargo.
  step "stale-config marker guard regression suite (#280)"
  if out=$("$ROOT/tools/test_config_markers.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_config_markers"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_config_markers"
  fi

  # #460: prove the lock arm can fail, and — the harder half — that it cannot fail FALSELY.
  #
  # Its keeper arms are the two negatives. `--offline` makes `cargo metadata --locked` exit 101 on a
  # perfectly FRESH lock whenever the registry cache is cold, which on a clean CI runner is the
  # normal state; and a crate copied without its sibling path deps exits 101 for a third unrelated
  # reason. Both are asserted to read as "could not check", never STALE. Without them the checker's
  # central decision — that only cargo's own `because --locked was passed` sentence licenses the
  # finding — was untested, and sabotaging it was caught by nothing else in the suite.
  #
  # It resolves real dependency graphs rather than matching text, but a warm `cargo metadata --locked`
  # is ~0.13 s and the whole suite measured 0.9 s here — so it is NOT the slow arm its position might
  # suggest. Placed last in the pure-text run only because it is the newest.
  step "Cargo.lock freshness gate regression suite (#460)"
  if out=$("$ROOT/tools/test_lock_fresh.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_lock_fresh"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_lock_fresh"
  fi

  # #426: prove the comment-blind sweep took, in BOTH directions. Three checkers were counting
  # matches inside comments; three others read their DECLARATIONS from comments on purpose, so an
  # over-eager strip would leave those green forever with nothing to check. The "still sees its
  # declarations" arms sit beside the "no longer sees prose" ones for exactly that reason.
  #
  # Every probe uses a BLOCK comment. `//` is safe everywhere — the checkers anchor with `^\s*` —
  # so a line-comment probe passes against the UNFIXED code too and proves nothing. Pure text.
  step "comment-blind checker sweep regression suite (#426)"
  if out=$("$ROOT/tools/test_comment_blind.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_comment_blind"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_comment_blind"
  fi

  step "build-matrix checker regression suite (#350)"
  if out=$("$ROOT/tools/test_build_matrix.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_build_matrix"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_build_matrix"
  fi

  # #351: the same discipline for the exclusion checker, and it matters more here. An ABSENCE
  # check's passing state and its broken state print the same green — "no violations found"
  # and "nothing found at all" are indistinguishable from the outside. Pure text, no cargo,
  # so it runs in the host half where a cross toolchain is not available.
  step "exclusion checker regression suite (#351)"
  if out=$("$ROOT/tools/test_check_exclusions.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_check_exclusions"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_check_exclusions"
  fi

  # #278: same discipline for the ELECT send-path checker, and for the same reason as #351 —
  # every one of its arms is an ABSENCE check ("no second impl", "no stray encoder call", "no
  # undeclared raw send"), and an absence check that has quietly stopped covering anything prints
  # exactly the green an intact one does. Each arm mutates a COPY of the firmware tree into a shape
  # that was enumerated as "satisfies the type system and still strands the fleet", and asserts the
  # checker goes red for the RIGHT REASON. Three further arms delete an anchor and require exit 2.
  step "ELECT send-path checker regression suite (#278)"
  if out=$("$ROOT/tools/test_check_elect_send_path.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_check_elect_send_path"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_check_elect_send_path"
  fi

  # #438/#335: the paint-warrant arm guards an INSTRUMENT rather than the firmware, which makes its
  # own falsifiability the whole question — a validity check that cannot fail is indistinguishable
  # from the prose sentence it replaced, and that sentence is precisely what rotted. Two arms drive
  # the roomier-paint case (including a ONE-BYTE divergence, because a silent tolerance is how a
  # warrant erodes), three fail closed on a renamed symbol / a nonsense region / an unreadable
  # input. That last one is not padding: an unreadable input must exit 2 (BLIND) and never 1
  # (VIOLATED), or a wrong path gets reported as a real and alarming finding that never happened.
  step "paint-warrant checker regression suite (#438)"
  if out=$("$ROOT/tools/test_paint_warrant.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_paint_warrant"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_paint_warrant"
  fi

  # #335 STEP G, same reasoning one invariant over: every arm of the station-consumer checker is an
  # ABSENCE check ("no undeclared device", "no embassy_net::new yet", "no second constructor"), and
  # an absence check that has quietly stopped covering anything prints exactly the green an intact
  # one does. Two of the arms here are REGRESSION arms rather than hazard arms: the checker's first
  # run counted its own doc comment's prose as a real call site, so both directions are pinned —
  # prose naming the constructors must not turn a clean tree red, and commenting out a real site
  # must not turn a dirty one green.
  step "station-consumer checker regression suite (#335 STEP G)"
  if out=$("$ROOT/tools/test_check_station_consumers.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_check_station_consumers"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_check_station_consumers"
  fi

  # The merge guard's own self-test. One line, and it is the difference between "the guard HAS a
  # self-test" and "the self-test RUNS" — the distinction this gate exists to enforce everywhere
  # else. No network: it drives the decision function with synthetic SHAs.
  step "merge guard self-test (tools/merge_pr.sh)"
  if out=$(SELFTEST=1 "$ROOT/tools/merge_pr.sh" 2>&1); then
    printf '%s\n' "$out" | tail -1; ok "merge_pr self-test"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "merge_pr self-test"
  fi

  # #367: prove the verifier-wiring checker's arms can fail. Operates only on mktemp copies —
  # never the working tree. Same reasoning as its siblings: this check exists BECAUSE a green
  # signal over uncompiled code fooled us, so shipping it without watching it go red would be
  # the same mistake one level up.
  step "verifier-wiring checker regression suite (#367)"
  if out=$("$ROOT/tools/test_verifier_wiring.sh" 2>&1); then
    printf '%s\n' "$out" | tail -2; ok "test_verifier_wiring"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_verifier_wiring"
  fi

  # #363: the provisioner's own suite. It existed since #359 and NOTHING RAN IT — `grep -rn
  # test_ci_provision tools/gate.sh .github/workflows/` returned nothing, while every one of its
  # siblings above was wired. That is this file's opening argument yet again: a checker nobody
  # invokes is the same shape as prose, and it has already stopped running.
  #
  # It earns the slot on its own merits now, not just for symmetry: gate.sh decides which tree to
  # BUILD from based on `ci_provision.sh --list-extras`, so a silent regression in that one
  # contract would quietly restore the two-verdict bug #363 exists to remove. Runs only in
  # mktemp dirs; touches nothing in the working tree.
  step "provisioning checker regression suite (#359/#363)"
  if out=$("$ROOT/tools/test_ci_provision.sh" 2>&1); then
    printf '%s\n' "$out" | tail -1; ok "test_ci_provision"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_ci_provision"
  fi

  # #400: and the same argument a third time. An audit of `tools/test_*.sh` against this file found
  # FOUR suites with no gate — so #314's id42-refusal test and the #128 ratchet test have never once
  # been run by CI, and the safety properties they assert were being maintained on trust. Both of
  # these guard tools/ota_publish.sh, which is the one script here that can arm the whole fleet.
  #
  # Deliberately NOT wired, with reasons, so the remainder is a declared gap and not an oversight:
  #   • test_ota_verify.sh      — hermetic but slow (>2 min, replays canned MQTT logs). Not wired
  #     because I did not watch it run to completion, and wiring a suite on the strength of its own
  #     header comment is the inference this file exists to refuse. Worth a follow-up.
  #   • test_ha_deploy_guard.sh — MUST NOT be wired. It commits with `git commit -am` by design and
  #     is built for a scratch clone; running it in a checkout with work in it creates junk commits
  #     (it has already done so here once). Its refusal to run in a working tree IS the guard.
  # Both suites below are offline, cargo-free, and leave the working tree byte-unchanged (verified
  # by running them and checking `git status --porcelain`, not by reading their headers).
  step "OTA publish guards + ratchet regression suites (#314/#128/#400)"
  for _t in test_ota_publish_guards test_ota_ratchet; do
    if out=$("$ROOT/tools/$_t.sh" 2>&1); then
      printf '%s\n' "$out" | tail -1; ok "$_t"
    else
      printf '%s\n' "$out" | sed 's/^/        /'; bad "$_t"
    fi
  done

  # #390: the offline half of the symbol-size guard. The fw arm runs the checker against a real
  # ELF; this proves the PARSE/NORMALISE/COMPARE logic can fail, using a committed real slice of
  # readelf output — no cargo, no ELF, no toolchain.
  # The split is load-bearing in both directions: a readelf output-format change keeps THIS suite
  # green and turns the fw arm red (only the fw arm sees real readelf), while a logic regression
  # turns this suite red even on a machine that cannot build firmware at all. Neither arm alone
  # covers both, which is why both exist.
  step "symbol-size checker regression suite (#390)"
  if out=$("$ROOT/tools/test_symbol_sizes.sh" 2>&1); then
    printf '%s\n' "$out" | tail -1; ok "test_symbol_sizes"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_symbol_sizes"
  fi

  # #419: the board-fact seam. A constant in targets/*/src/board/*.rs is a STATEMENT ABOUT THE
  # HARDWARE; one with zero code readers whose name is ALSO declared file-locally elsewhere means
  # the board says X while the code quietly uses Y, and nothing fails, warns, or compiles
  # differently. Latent in smol today — the arming event is a routine subtree refresh, which is
  # exactly why this is a tripwire rather than a fix. This is the FIRST gate arm covering
  # targets/, and it is text-only (no build, no toolchain), so it belongs in the host arm.
  #
  # Runs on the real tree, then its own regression suite. Both, because they fail on different
  # things: the live run catches an actual refresh that arms the trap, and the suite catches the
  # checker being weakened so that the live run stops being able to see it.
  step "board-fact seam — declared constants with no readers (#419)"
  if out=$("$ROOT/tools/check_board_consts.py" "$ROOT" 2>&1); then
    printf '%s\n' "$out" | head -1; ok "board consts"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "board consts"
  fi
  if out=$("$ROOT/tools/test_board_consts.sh" 2>&1); then
    printf '%s\n' "$out" | tail -1; ok "test_board_consts"
  else
    printf '%s\n' "$out" | sed 's/^/        /'; bad "test_board_consts"
  fi
fi

step "summary"
if [ ${#FAILED[@]} -eq 0 ]; then
  printf '   \033[32mGATE GREEN\033[0m\n'; exit 0
fi
printf '   \033[31mGATE RED — %d failure(s):\033[0m\n' "${#FAILED[@]}"
printf '     • %s\n' "${FAILED[@]}"
exit 1
