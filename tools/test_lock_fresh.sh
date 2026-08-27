#!/usr/bin/env bash
# test_lock_fresh.sh — #460. Proof that tools/check_lock_fresh.sh can fail, and cannot FALSELY fail.
#
# A thin wrapper, deliberately: the arms live next to the checker they exercise (its `--self-test`),
# because the fixture is the checker's own real drift and splitting them would give two files that
# have to be kept in step. This file exists so the suite is DISCOVERABLE — `tools/gate.sh`'s host
# block and the `tools/test_*.sh` audit both walk this naming convention, and a `--self-test` flag
# hidden inside a checker is found by neither. (That audit, documented at gate.sh:823, is what caught
# four suites nobody ran.)
#
# What it proves, in one line each:
#   * the real 2026-08-26 drift (mipidsi + embedded-hal-bus absent from the lock) -> STALE
#   * a healthy lock -> FRESH, before and after that edit (so the STALE arm measured the edit)
#   * an UNRELATED cargo exit 101 -> "could not check", never STALE  <- the discriminator
#   * `--offline` on a cold registry cache is a FALSE stale, which is why the checker does not use it
#   * cargo absent / no Cargo.lock / no Cargo.toml / missing dir -> "could not check", never a pass
#   * lock discovery is a scan, not a hand-kept list
set -uo pipefail
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check_lock_fresh.sh" --self-test
