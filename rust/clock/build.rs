//! Build script: embed the firmware VERSION IDENTITY for `env!()`.
//!
//! Emits two compile-time env vars the crate reads via `env!`:
//!   * `BUILD_HASH`   — git short hash (e.g. `"e4f5a6b"`); seeds the sigil version
//!     name (`net::names::version_name`). Falls back to `"dev"`.
//!   * `BUILD_NUMBER` — monotonic build count (`git rev-list --count HEAD`) shown
//!     as `v<N>`. Falls back to `"0"`.
//!   * `SMOL_CHIP_ID` — (#349) the chip this image is built for, DERIVED from the target
//!     triple where that is unambiguous (`riscv32imc`→C3, `xtensa-esp32s3`→S3;
//!     `riscv32imac` is C5 *or* C6 and maps to UNKNOWN = build failure on wifi tiers).
//!     Consumed by `net::target::SELF_CHIP` and embedded in the image's target descriptor so
//!     a board can refuse an image built for other silicon. `SMOL_CHIP=<name>` overrides by
//!     name, and is REQUIRED for the ambiguous triples.
//!   * `SMOL_NODE_ID` — (#42) OPTIONAL per-board id override, emitted ONLY when the
//!     env var is set, so `SMOL_NODE_ID=8 cargo build` builds an id-8 image without
//!     hand-editing `board.rs` (which reads it via `option_env!`, fallback = its own
//!     `NODE_ID` literal). Guards the one-image-to-many flash that collides node ids.
//!
//! Source order (per field): (a) an explicit env var, else (b) `git`, else (c) a
//! fallback constant.
//!
//! ⚠️ DEPLOY CONTRACT — the flash agent builds from a `git archive` tarball, which
//! has **NO `.git` directory**, so the `git` commands here would fail and the
//! build would silently become `"dev"/0`. Such builds MUST pass the identity
//! explicitly from the known commit:
//!     SMOL_GIT_HASH=<short> SMOL_BUILD_NUMBER=<n> cargo build --release …
//! The env path (a) takes precedence exactly so archive builds are reproducible.

use std::process::Command;

fn main() {
    let hash = env_or_git("SMOL_GIT_HASH", &["rev-parse", "--short=7", "HEAD"])
        .unwrap_or_else(|| "dev".to_string());
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

/// #349: map the build's target triple to the `net::target::CHIP_*` id embedded in the image
/// descriptor. `SMOL_CHIP` overrides it by NAME (not by number) for a chip whose triple this
/// mapping does not yet know — an explicit, greppable act rather than a silent default.
///
/// `0` (CHIP_UNKNOWN) is returned when the triple is unrecognised, and `net/target.rs` has a
/// `const _: () = assert!(SELF_CHIP != CHIP_UNKNOWN)` — so an unmapped chip fails the BUILD
/// instead of shipping an image that claims to be for nothing in particular. That assert only
/// exists on `wifi` tiers (the ones that can be OTA'd), so a host/hostsim build is unaffected.
fn chip_id() -> u8 {
    if let Ok(name) = std::env::var("SMOL_CHIP") {
        return match name.trim() {
            "esp32c3" => 1,
            "esp32c6" => 2,
            "esp32s3" => 3,
            "esp32c5" => 4,
            _ => 0,
        };
    }
    let target = std::env::var("TARGET").unwrap_or_default();
    // riscv32imc = C3; xtensa-esp32s3 = S3. riscv32imac is AMBIGUOUS — the C5 and the C6
    // share it — so it maps to 0 (CHIP_UNKNOWN) and the wifi-tier const assert fails the
    // build until `SMOL_CHIP=<name>` says which silicon this image is for. A guessed id
    // would be a valid-looking value the suitability check trusts: this exact shape once
    // stamped a C5 build as a C6, which `decide()` would then have accepted cross-chip.
    // Ordering matters: "riscv32imac" contains no "riscv32imc" substring, but match the
    // longer/more specific arms first anyway so a future triple cannot alias onto the C3.
    if target.starts_with("xtensa-esp32s3") {
        3
    } else if target.starts_with("riscv32imac") {
        0 // ambiguous (C5|C6) — require SMOL_CHIP, fail closed via the SELF_CHIP assert
    } else if target.starts_with("riscv32imc") {
        1
    } else {
        0 // host/wasm (hostsim, web-emu) — no descriptor is emitted on those tiers anyway
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
