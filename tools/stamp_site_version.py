#!/usr/bin/env python3
"""Stamp `site/version.json` — the realm-sigil version surface for the static site.

WHY THIS EXISTS, AND WHY IT IS NOT A HAND-EDITED FILE
CLAUDE.md requires every project with a web presence to expose a realm-sigil version.
smol's site is static GitHub Pages, so it *cannot* serve a dynamic `/api/version` — there
is no server (`site/server.py` is local-dev only and never ships). The honest static
equivalent is a file written AT PUBLISH TIME from the commit being published, which is why
this runs in `.github/workflows/pages.yml` and why `site/version.json` is gitignored.

A hand-maintained version string would be the exact antipattern the site has now been
cleaned of twice (a green LED over a three-week-old archive; a leaderboard drawing labelled
"live"). A number a human updates is a number that is wrong. This one is derived, or absent.

WHERE THE WORDS COME FROM
The corpus is read out of `rust/clock/src/net/names.rs` — the `no_std` port the FIRMWARE
already carries — so the repo holds ONE forge word list, not a third copy of it. If that
file's shape changes, this degrades to no sigil word rather than inventing one (see below).

Canonical realm-sigil is NOT importable here: `sigil.realm.watch/python/pyproject.toml`
declares `build-backend = "setuptools.backends._legacy:_Backend"`, which does not exist, so
`pip install` of it fails outright (verified 2026-07-28). Hence no network dependency in the
deploy path — a broken upstream install must never be able to fail a site publish.

TWO DIFFERENT MAPPINGS, BOTH CORRECT — DO NOT "UNIFY" THEM
  · The FIRMWARE (`names.rs::version_name_for`) maps a small sequential BUILD NUMBER with
    direct modulo, deliberately avoiding sigil's `>>8` (builds 256..=511 all collapse to one
    noun under a shift). 341 = Riveted Bellows.
  · THIS maps a GIT HASH with sigil's canonical `seed % len` / `(seed>>8) % len`, because the
    realm-sigil *web* contract defines `version` as `generate_name(hash, realm)`.
Different inputs, so different formulas are required, not a bug to reconcile.

Realm is `forge` on purpose: names.rs notes forge is the PROVENANCE vocabulary, kept
deliberately distinct from the `fantasy` realm used for node IDENTITY, so "which build" can
never be misread as "which board" at a glance.
"""
from __future__ import annotations

import datetime
import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
NAMES_RS = REPO / "rust" / "clock" / "src" / "net" / "names.rs"
OUT = REPO / "site" / "version.json"
REALM = "forge"
DESCRIPTION = (
    "A fingertip game console, a self-updating ESP-NOW mesh, and a 260K-parameter "
    "transformer writing children's stories — one no_std Rust firmware on an ESP32-C3."
)


def sh(*args: str) -> str:
    """Run a git command; empty string on any failure (CI shallow clones, no git, …)."""
    try:
        return subprocess.check_output(args, cwd=REPO, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return ""


def forge_corpus() -> tuple[list[str], list[str]]:
    """Pull the forge adjectives/nouns out of names.rs.

    Returns ([], []) if anything about the file's shape is unexpected — the caller then omits
    the sigil word entirely. Guessing a name would defeat the point of deriving it.
    """
    try:
        src = NAMES_RS.read_text(encoding="utf-8")
    except OSError:
        return [], []
    block = re.search(r"pub static FORGE:\s*Realm\s*=\s*Realm\s*\{(.*?)\n\};", src, re.S)
    if not block:
        return [], []
    body = block.group(1)

    def field(name: str) -> list[str]:
        m = re.search(rf"{name}:\s*&\[(.*?)\]", body, re.S)
        return re.findall(r'"([^"]+)"', m.group(1)) if m else []

    adjectives, nouns = field("adjectives"), field("nouns")
    # The corpus is 20/20 by construction; a short read means the regex drifted off the file.
    if len(adjectives) < 2 or len(nouns) < 2:
        return [], []
    return adjectives, nouns


def generate_name(short_hash: str) -> str | None:
    """sigil's canonical hash→name. None when the corpus or hash is unusable."""
    adjectives, nouns = forge_corpus()
    if not adjectives or not nouns:
        return None
    try:
        seed = int(short_hash, 16)
    except ValueError:
        return None
    return f"{adjectives[seed % len(adjectives)]} {nouns[(seed >> 8) % len(nouns)]} · {short_hash}"


def main() -> int:
    # Prefer the CI-provided values; fall back to git so a local run produces the real thing.
    sha = (os.environ.get("GITHUB_SHA") or sh("git", "rev-parse", "HEAD"))[:7]
    branch = os.environ.get("GITHUB_REF_NAME") or sh("git", "rev-parse", "--abbrev-ref", "HEAD")
    slug = os.environ.get("GITHUB_REPOSITORY") or "jphein/smol"
    repo_url = f"https://github.com/{slug}"
    # In CI the tree is a pristine checkout, so `dirty` is only ever meaningful locally.
    dirty = bool(sh("git", "status", "--porcelain", "--", "site"))
    built = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    payload = {
        "name": "smol",
        "description": DESCRIPTION,
        "version": generate_name(sha) if sha else None,
        "hash": sha or None,
        "branch": branch or "unknown",
        "dirty": dirty,
        "built": built,
        "realm": REALM,
        "repo": repo_url,
        "commit_url": f"{repo_url}/commit/{sha}" if sha else "",
    }

    OUT.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(f"stamped {OUT.relative_to(REPO)}: {payload['version'] or '(no sigil word)'} "
          f"branch={payload['branch']} dirty={payload['dirty']}")
    # A missing sigil word is a soft failure: the page still shows the hash, and a publish
    # must never be blocked by cosmetics. Say so loudly enough to be noticed in CI logs.
    if not payload["version"]:
        print("WARNING: no sigil word — could not read the FORGE corpus from "
              f"{NAMES_RS.relative_to(REPO)}; the page will show the bare hash.",
              file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
