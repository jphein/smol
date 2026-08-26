//! Build script: embed the firmware VERSION IDENTITY for `env!()`.
//!
//! Emits two compile-time env vars the crate reads via `env!`:
//!   * `BUILD_HASH`   — git short hash (e.g. `"e4f5a6b"`). Falls back to `"nogit"` (#420),
//!     which names the ABSENCE of an identity rather than standing in for one.
//!   * `BUILD_NUMBER` — the COMMITTED RELEASE ratchet, read from `version.txt`; shown as
//!     `v<N>`. Falls back to `"0"` only when that file is missing or empty — which in
//!     practice never happens, since it is tracked. (This line used to say "monotonic build
//!     count (`git rev-list --count HEAD`)"; #218 replaced the git count with the ratchet
//!     because the count is BRANCH-relative. The sigil version NAME is seeded from this
//!     NUMBER, not from the hash, which the old wording above also got wrong.)
//!   * `SMOL_CHIP_ID` — (#349, reworked by #347 Part 2) the chip this image is built for,
//!     derived from the CHIP FEATURE (`esp32c3` / `esp32c5` / `esp32c6` / `esp32s3`), which is
//!     unambiguous and which `budget.rs` already refuses to leave unset or doubled. The target
//!     triple and `SMOL_CHIP` are kept as CROSS-CHECKS: either may be absent, neither may
//!     contradict the feature, and a disagreement fails the build. `SMOL_CHIP` is therefore no
//!     longer REQUIRED for `riscv32imac` (the C5/C6 triple the feature now discriminates) — it
//!     remains available for naming silicon whose triple this tree does not map.
//!     Consumed by `net::target::SELF_CHIP` and embedded in the image's target descriptor so
//!     a board can refuse an image built for other silicon.
//!   * `SMOL_NODE_ID` — (#42) OPTIONAL per-board id override, emitted ONLY when the
//!     env var is set, so `SMOL_NODE_ID=8 cargo build` builds an id-8 image without
//!     hand-editing `board.rs` (which reads it via `option_env!`, fallback = its own
//!     `NODE_ID` literal). Guards the one-image-to-many flash that collides node ids.
//!
//! Source order (per field): (a) an explicit env var, else (b) `git`, else (c) a
//! fallback constant.
//!
//! ⚠️ DEPLOY CONTRACT — the flash agent builds from a `git archive` tarball, which
//! has **NO `.git` directory**, so the `git` commands here would fail. Such builds MUST
//! pass the identity explicitly from the known commit:
//!     SMOL_GIT_HASH=<short> SMOL_BUILD_NUMBER=<n> cargo build --release …
//! The env path (a) takes precedence exactly so archive builds are reproducible.
//!
//! #420 CORRECTION — this contract used to say such a build "would silently become
//! `"dev"/0`". The NUMBER half was wrong, and the error mattered: `version.txt` is a
//! TRACKED file, so it is present in every archive and every rsync mirror, and the number
//! resolves to its contents (the last RELEASE number), never to `0`. MEASURED in a
//! .git-less mirror of `main`:
//!     BUILD_NUMBER=345   BUILD_HASH=dev   BUILD_DEV=1
//! So `.git` presence is irrelevant to the number — a FULL checkout without
//! `SMOL_BUILD_NUMBER` stamps 345 too. Two boards reporting "345" while running current
//! code (id50, id162, 2026-08-25) were not showing a .git-less fallback; they were showing
//! the release ratchet, and ~40 min of incident forensics went into a "345-era crown"
//! theory that the number could never have supported.
//!
//! The real .git-less hole was the HASH: it degraded to the literal `"dev"`, which is
//! IDENTICAL for every such build ever made, so it discriminates nothing at exactly the
//! moment an operator needs it to. `BUILD_DEV=1` did fire — the image was honestly marked
//! dev — but "dev + a constant" is an honest label with no identity inside it.

use std::process::Command;

fn main() {
    // #420: keep the resolution result, because "could not establish an identity" is a
    // DIFFERENT state from "the identity is X", and the two need different dispositions.
    let hash_resolved = env_or_git("SMOL_GIT_HASH", &["rev-parse", "--short=7", "HEAD"]);
    // `nogit` rather than `dev`: it names the ABSENCE instead of occupying the hash slot with
    // something that reads like a value. Deliberately SHORTER than a real 7-char short hash —
    // `net::names::write_version` writes the hash into a fixed buffer BEFORE the sigil noun
    // (`v345+dev.<hash> Bellows`) with `let _ = write!`, so an over-long token would silently
    // truncate the noun. Being shorter than the normal case makes overflow impossible by
    // construction, rather than by measuring a capacity that could later change.
    let hash = hash_resolved
        .clone()
        .unwrap_or_else(|| "nogit".to_string());
    // #218: the build NUMBER is a COMMITTED ratchet (`version.txt`), NOT `git rev-list
    // --count` — the count is BRANCH-relative, so a newer canary off a side branch stamps
    // a LOWER number than the deployed release and reads as a rollback on every dashboard.
    // The ratchet is content-ordered + bumped on release. Precedence: env (archive/pipeline)
    // > version.txt > fallback.
    let number = env_or_file("SMOL_BUILD_NUMBER", "version.txt").unwrap_or_else(|| "0".to_string());
    // #218: honest dev marker. A build is a RELEASE only when the ship pipeline says so
    // (`SMOL_RELEASE=1`); every other build (local / canary) is dev and displays
    // `v<N>+dev.<hash>` so it can never masquerade as the release. The NUMERIC BUILD_NUMBER
    // is unchanged, so OTA monotonicity holds and a dev build compares as the floor.
    let is_release = std::env::var("SMOL_RELEASE").map(|v| v.trim() == "1").unwrap_or(false);

    // #420 FAIL CLOSED: a build may be unidentifiable, or it may claim to be a release. It may
    // not be both. `nogit` is an acceptable stamp for a local/mirror experiment precisely
    // because the image says so; it is never acceptable on something asserting it IS the
    // release, since that is an artifact that could be flashed or archived as authoritative
    // with nothing inside it to say which commit it came from.
    //
    // This CANNOT fire on any sanctioned path, which is what makes it safe to fail closed
    // rather than warn (verified, not assumed):
    //   * `repro_build_bin` exports SMOL_GIT_HASH (tools/repro_build.sh:406) from a REQUIRED
    //     parameter, and it is the single function every publish path goes through —
    //     ota_publish.sh stage and repro_at_canonical.sh both call it;
    //   * CI sets SMOL_RELEASE nowhere (`grep -rn SMOL_RELEASE .github/workflows/` is empty),
    //     and an actions/checkout tree has a .git anyway;
    //   * a developer's bare `cargo build`, and a mirror build, set no SMOL_RELEASE.
    // The only way to reach this panic is to hand-export SMOL_RELEASE=1 in a tree with no
    // resolvable commit — which is exactly the act that must not quietly succeed.
    if is_release && hash_resolved.is_none() {
        panic!(
            "#420: SMOL_RELEASE=1 but no commit identity could be resolved (no SMOL_GIT_HASH, \
             and `git rev-parse` failed — a mirror or archive tree has no .git).\n\
             Refusing to stamp a RELEASE image that cannot name the commit it came from: it \
             would report v{number} with hash `nogit`, indistinguishable from every other \
             unidentified build.\n\
             Fix: pass the identity explicitly, as the deploy contract requires —\n\
             \x20   SMOL_GIT_HASH=<short7> SMOL_BUILD_NUMBER=<n> SMOL_RELEASE=1 cargo build --release …\n\
             (tools/repro_build.sh's repro_build_bin already does this for every publish path; \
             prefer it over a hand-rolled release build.)"
        );
    }

    println!("cargo:rustc-env=BUILD_HASH={hash}");
    println!("cargo:rustc-env=BUILD_NUMBER={number}");
    println!("cargo:rustc-env=BUILD_DEV={}", if is_release { "0" } else { "1" });

    // #349: the CHIP this image is built for, DERIVED from the target triple rather than
    // declared. An image that names its own silicon is only useful if it cannot lie, and the
    // triple is the one fact a build cannot get wrong. `CARGO_CFG_TARGET_ARCH` is NOT enough —
    // it reports "riscv32" for both the C3 (imc) and the C6 (imac); `TARGET` carries the ISA
    // extensions that actually distinguish them.
    println!("cargo:rustc-env=SMOL_CHIP_ID={}", chip_id());
    println!("cargo:rerun-if-env-changed=SMOL_CHIP");

    // #42: OPTIONAL per-board NODE_ID override. Emitted ONLY when set → a normal build
    // is byte-unchanged; `SMOL_NODE_ID=8 cargo build` overrides board.rs's fallback
    // (read there via `option_env!`). Guards the one-image-to-many id collision.
    if let Ok(node_id) = std::env::var("SMOL_NODE_ID") {
        let node_id = node_id.trim();
        if !node_id.is_empty() {
            println!("cargo:rustc-env=SMOL_NODE_ID={node_id}");
        }
    }

    // Rebuild when the commit moves (real checkout) or an override env changes;
    // all are harmless no-ops in an archive build with none present.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=version.txt");
    println!("cargo:rerun-if-env-changed=SMOL_GIT_HASH");
    println!("cargo:rerun-if-env-changed=SMOL_BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=SMOL_RELEASE");
    println!("cargo:rerun-if-env-changed=SMOL_NODE_ID");
}

/// The four `net::target::CHIP_*` ids, as (feature name, id). ONE list, used by all three
/// sources below, so a fifth chip cannot be taught to two of them and forgotten in the third.
const CHIPS: [(&str, u8); 4] = [("esp32c3", 1), ("esp32c6", 2), ("esp32s3", 3), ("esp32c5", 4)];

/// #349 + #347 Part 2: the `net::target::CHIP_*` id embedded in the image descriptor.
///
/// ── WHAT CHANGED IN PART 2, AND WHY IT IS A CORRECTNESS FIX ────────────────────────────────
/// This used to read the TARGET TRIPLE, with `SMOL_CHIP` as an unchecked override. That was the
/// best available answer when the chip was spelled across eight dependency declarations and
/// nothing in cargo's world knew it. It is no longer: since bd26db1 the build carries a CHIP
/// FEATURE, `budget.rs` compile_errors unless EXACTLY ONE is enabled, and a build script can read
/// features from the environment. So the feature is now the authority, and it is a strictly better
/// one than the triple in both directions:
///
///   * It is UNAMBIGUOUS where the triple is not. `riscv32imac` is the C5 *and* the C6, so the
///     triple path had to return CHIP_UNKNOWN and lean on `SMOL_CHIP` — meaning every C5/C6 build
///     needed an env var carrying information the build already had. Forgetting it failed the
///     build, correctly but confusingly, over a fact that was never actually missing.
///   * It CANNOT SILENTLY DISAGREE. The old override was applied without ever being compared to
///     anything, so `--features esp32c6` with `SMOL_CHIP=esp32c5` stamped a C5 id onto a C6 image
///     — a valid-looking value that `net::target::decide()` would then trust to accept a
///     cross-chip OTA. That is the exact failure #349 exists to prevent, still reachable through
///     #349's own escape hatch. Now a disagreement is a hard build failure.
///
/// `SMOL_CHIP` is KEPT, for the case that justified it: naming silicon whose triple this mapping
/// does not know. It just may no longer contradict the feature.
///
/// `0` (CHIP_UNKNOWN) still means "nothing here knows", and `net/target.rs`'s
/// `const _: () = assert!(SELF_CHIP != CHIP_UNKNOWN)` fails the build rather than shipping an
/// image that claims to be for nothing in particular. That assert exists only on `wifi` tiers, so
/// host/hostsim builds — which have no chip feature and want none — are unaffected.
fn chip_id() -> u8 {
    // Features reach a build script as `CARGO_FEATURE_<NAME>`, uppercased with `-` -> `_`.
    let from_feature: Vec<(&str, u8)> = CHIPS
        .iter()
        .filter(|(name, _)| {
            std::env::var(format!("CARGO_FEATURE_{}", name.to_uppercase())).is_ok()
        })
        .copied()
        .collect();

    // TWO chip features: deliberately NOT this function's error to report. `budget.rs` already
    // refuses it with a message that names the likely cause (`--features esp32c5` without
    // `--no-default-features`, so `default`'s esp32c3 makes a second chip) and the exact fix.
    // Panicking here would preempt that with something worse, since a build script's panic is
    // the first failure the user sees. Emit UNKNOWN and let the good diagnostic win.
    if from_feature.len() > 1 {
        return 0;
    }

    let from_triple = triple_chip();
    let from_env = std::env::var("SMOL_CHIP").ok().map(|v| v.trim().to_string());
    // #335 P1.0 (edition 2024): collapsed into a let-chain. src/main.rs defers its 38 sites behind
    // a crate-level `allow`, but that allow cannot reach here — build.rs is a SEPARATE crate, and
    // this is the one site in it. Nothing Phase 1 rewrites lives in this file, so there is no
    // revertibility argument for deferring a two-line collapse; the edition bump is what makes the
    // let-chain legal in the first place.
    if let Some(name) = from_env.as_deref()
        && !name.is_empty()
        && !CHIPS.iter().any(|(n, _)| *n == name)
    {
        // Previously an unrecognised name fell through to 0, which failed the build later
        // with the SELF_CHIP assert — a message about an unknown chip id that says nothing
        // about the typo that caused it. Fail here, naming the value and the valid set.
        panic!(
            "SMOL_CHIP={name:?} is not a chip this tree knows. Valid: {}. \
             (It overrides the chip NAME for silicon whose triple is ambiguous; it does not \
             need setting when a chip feature is enabled.)",
            CHIPS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" / ")
        );
    }

    if let Some((feat_name, feat_id)) = from_feature.first().copied() {
        // The feature is the authority. Both other sources are now CROSS-CHECKS: each may be
        // absent or ambiguous, but neither may contradict it.
        if let Some(env_name) = from_env.as_deref().filter(|s| !s.is_empty()) {
            assert!(
                env_name == feat_name,
                "chip DISAGREEMENT: the build enables feature `{feat_name}` but SMOL_CHIP says \
                 `{env_name}`. One of them is wrong and guessing which would stamp an image with \
                 silicon it was not built for — which is how an OTA gets accepted cross-chip \
                 (#349). Drop SMOL_CHIP (the feature already names the chip) or fix the feature."
            );
        }
        if let Some((triple_name, triple_id)) = from_triple {
            assert!(
                triple_id == feat_id,
                "chip DISAGREEMENT: feature `{feat_name}` but target triple {:?} is {triple_name}. \
                 The features and the target are chosen TOGETHER, per invocation — see the \
                 per-chip invocations in tools/build-matrix.toml, or run tools/check_chips.sh.",
                std::env::var("TARGET").unwrap_or_default()
            );
        }
        return feat_id;
    }

    // No chip feature. Either a host/hostsim build (no descriptor is emitted on those tiers) or
    // a bare-metal build that budget.rs is about to refuse. Honour SMOL_CHIP, else the triple.
    if let Some(name) = from_env.as_deref().filter(|s| !s.is_empty()) {
        return CHIPS.iter().find(|(n, _)| *n == name).map(|(_, id)| *id).unwrap_or(0);
    }
    from_triple.map(|(_, id)| id).unwrap_or(0)
}

/// The triple -> chip mapping, as a CROSS-CHECK rather than the primary source. `None` means the
/// triple cannot name a chip: `riscv32imac` is shared by the C5 and the C6, and a host/wasm triple
/// is not a chip at all. Both are legitimately unknowable here, which is why this returns Option
/// instead of the old `0` — "ambiguous" and "definitely nothing" were the same value before, and
/// only one of them should silence a cross-check.
///
/// Ordering matters: "riscv32imac" does not contain "riscv32imc" as a prefix-substring, but the
/// more specific arms are matched first anyway so a future triple cannot alias onto the C3.
fn triple_chip() -> Option<(&'static str, u8)> {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.starts_with("xtensa-esp32s3") {
        Some(("esp32s3", 3))
    } else if target.starts_with("riscv32imac") {
        None // C5 or C6 — the feature discriminates; nothing here can.
    } else if target.starts_with("riscv32imc") {
        Some(("esp32c3", 1))
    } else {
        None // host/wasm (hostsim, web-emu)
    }
}

/// Prefer the explicit env override (archive/pipeline), else read `path` (relative to the
/// crate root — the committed ratchet); `None` if neither yields a non-empty value.
fn env_or_file(var: &str, path: &str) -> Option<String> {
    if let Ok(v) = std::env::var(var) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let s = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Prefer the explicit env override (archive builds), else run `git`; `None` if
/// the env var is unset/empty AND git is unavailable or fails (→ caller's fallback).
fn env_or_git(var: &str, git_args: &[&str]) -> Option<String> {
    if let Ok(v) = std::env::var(var) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let out = Command::new("git").args(git_args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}
