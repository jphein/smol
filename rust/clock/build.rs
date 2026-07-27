//! Build script: embed the firmware VERSION IDENTITY for `env!()`.
//!
//! Emits two compile-time env vars the crate reads via `env!`:
//!   * `BUILD_HASH`   — git short hash (e.g. `"e4f5a6b"`); seeds the sigil version
//!     name (`net::names::version_name`). Falls back to `"dev"`.
//!   * `BUILD_NUMBER` — monotonic build count (`git rev-list --count HEAD`) shown
//!     as `v<N>`. Falls back to `"0"`.
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
