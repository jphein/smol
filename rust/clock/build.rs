//! Build script: embed the firmware VERSION IDENTITY for `env!()`.
//!
//! Emits two compile-time env vars the crate reads via `env!`:
//!   * `BUILD_HASH`   — git short hash (e.g. `"e4f5a6b"`); seeds the sigil version
//!     name (`net::names::version_name`). Falls back to `"dev"`.
//!   * `BUILD_NUMBER` — monotonic build count (`git rev-list --count HEAD`) shown
//!     as `v<N>`. Falls back to `"0"`.
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
    let number = env_or_git("SMOL_BUILD_NUMBER", &["rev-list", "--count", "HEAD"])
        .unwrap_or_else(|| "0".to_string());

    println!("cargo:rustc-env=BUILD_HASH={hash}");
    println!("cargo:rustc-env=BUILD_NUMBER={number}");

    // Rebuild when the commit moves (real checkout) or the override env changes;
    // both are harmless no-ops in an archive build with neither present.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed=SMOL_GIT_HASH");
    println!("cargo:rerun-if-env-changed=SMOL_BUILD_NUMBER");
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
