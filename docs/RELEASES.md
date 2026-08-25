# Releases — what is published, what it is for, and what it is *not* for

smol publishes two very different kinds of artifact, and confusing them is the expensive mistake
this page exists to prevent.

| | **nightly prerelease** | **versioned release** |
|---|---|---|
| tag | `nightly-<date>` | `v<build>` (sigil-named) |
| built from | a `git archive` of a commit on `main` | the same, plus the `tools/repro_build` ceremony |
| identity stamp | `v<N>+dev.<hash>` — **dev by construction** | `v<N>` — a release identity |
| byte-reproducible? | **no** | **yes** — that is the point |
| credentials | **placeholder** (see below) | placeholder |
| what it is for | USB-flashing a **new or bench** board | the same, plus being the thing OTA serves |
| first one | `nightly-2026-08-24` | **v346** — not yet cut |

> ## The one rule
>
> **Fleet boards update over signed OTA. They do not update from GitHub downloads — ever.**
>
> The OTA path (`smol/ota/staged`, ed25519 over `build|size|sha256`) is the only sanctioned way an
> already-deployed board changes firmware. A GitHub artifact is for putting smol on a board that
> does not have it yet. Everything below is a consequence of that sentence.

---

## 🔴 Published binaries carry PLACEHOLDER credentials

This surprises people, so it goes first and in full.

Release images are built from a clean checkout provisioned by `tools/ci_provision.sh`, whose whole
job is *"throwaway values good enough to COMPILE and never good enough to ship"*. Concretely:

- `WIFI_NETWORK`, `MQTT_USER`, `MQTT_PASS` — the published example placeholders, byte-identical.
- `GROUP_KEY` — a **random throwaway** generated at provisioning time. Not any real fleet key.

**A board flashed from a download boots, drives the OLED, and runs the menu, the games and the
sensors. It will not associate to WiFi, will not reach a broker, and will not talk to an existing
mesh.** That is by design, not a packaging bug.

⚠️ And note the consequence of a baked key in a *public* binary: it is public. Two boards flashed
from the same download share a mesh key anyone can extract. **Treat it as a demo key, never a fleet
key.** To join a real network you must rebuild with your own `rust/clock/src/secrets.rs` — start
from `secrets.rs.example`; see [BUILDING.md](BUILDING.md).

---

## Why a nightly cannot masquerade as a release

`build.rs` stamps a version identity into every image. `SMOL_RELEASE=1` produces a release
identity; **every other build — local, canary, nightly — is stamped `v<N>+dev.<hash>`**.

`nightly-2026-08-24` is stamped `v345+dev.fd7cca7`, and `SMOL_RELEASE` was deliberately left unset
to make it so. This is a **safety property, not an oversight**: a dev identity cannot outrank a real
release on the fleet's build ratchet, so even if someone published a nightly to the OTA topic, a
board running a genuine release would refuse to go backwards onto it.

## Build identity on a nightly, stated plainly

Nightlies are built on the `familiar` build host from a **clean `git archive`** of the commit —
which has **no `.git` directory**. `build.rs` would normally fall back to a placeholder stamp there.
It does not, because `build.rs` carries a documented archive seam that exists (its words) *"exactly
so archive builds are reproducible"*:

```
SMOL_GIT_HASH=<short-hash>   # SMOL_BUILD_NUMBER unset → read from rust/clock/version.txt
                             # SMOL_RELEASE      unset → dev identity
```

So the stamp on a nightly is **honest about three separate things at once**: which commit's content
it was built from, which build number the tree declared, and the fact that it is not a release.
Verify it rather than trust it — the identity string is in the ELF, and the release notes state it
was checked there rather than assumed.

**What a nightly is NOT:** it has not been through the reproducibility ceremony below. It is
*content-built* from a commit — that commit's tracked files plus the provisioner's git-ignored
`board.rs`/`secrets.rs`. Two people building it independently should not expect matching bytes.

---

## The versioned release ceremony (`v346` will be the first)

The thing that makes a versioned release different is **`tools/repro_build`** and the property it
buys: **a fixed `(commit, node-id)` builds to the same bytes on any machine, so the sha256 IS the
image's identity.**

That property was hard-won. Per issue **#44**, the release ELF used to be *not* hash-reproducible,
for two independent reasons:

1. **rustc embeds absolute build paths** — `panic!`'s `file!()` strings for every dependency and
   every `build-std` crate (~62 from the registry, ~3 from the sysroot). Those roots differ per
   host, per user, per working directory, so the same commit built on two machines produced
   different bytes. Fixed by canonicalising both roots with `--remap-path-prefix`. *(A pleasant
   side effect: no `$HOME` path leaks into a public binary.)*
2. **`esp-bootloader-esp-idf` stamps the app descriptor from the wall clock** unless
   `SOURCE_DATE_EPOCH` is set, so two builds of one commit differed by minutes. Fixed by pinning
   `SOURCE_DATE_EPOCH` to the **commit's** Unix time.

Why it mattered enough to fix: without a stable hash, **an image could not be verified against the
board it was about to be flashed to** — which is what compounded the duplicate-node-id outage
(#42), where the wrong image on `id8`/`id9` could not be caught by an image↔board hash check.

⚠️ **`tools/repro_build.sh` is a SOURCED LIBRARY.** Running it directly exits 0 having done
nothing. The real entry points are `repro_build_bin` via `tools/ota_publish.sh`, and
`tools/verify_image.sh`. If you are looking for a green light, ask for **the sha256**, not for
"it passed" — a gate that cannot fail is worse than no gate, and this one has been misread as the
stack gate before.

A versioned release is also **sigil-named**: the build number draws a name from a pinned corpus
(`net/names.rs`). Those names are history and are **never re-synced** — renaming a past build would
rewrite the record of what shipped.

---

## Artifacts, and why the `default` tier has no `.bin`

`nightly-2026-08-24` publishes:

| file | tier | features | what it is |
|---|---|---|---|
| `smol-esp32c3-fleet.bin` | **canonical fleet** | `espnow,cast,io` | merged flashable image — bootloader + OTA partition table + app, 4 MiB |
| `smol-esp32c3-fleet.elf` | canonical fleet | `espnow,cast,io` | same build, for `probe-rs` / `readelf` / symbol work |
| `smol-esp32c3-default.elf` | `default` | none (no radio) | **ELF only** |
| `SHA256SUMS` | — | — | verify with `sha256sum -c SHA256SUMS` |

`espnow,cast,io` is the **canonical tier of record** — `tools/build-matrix.toml`
`canonical_tier = "fleet"`, matching `REPRO_FLEET_FEATURES` in `tools/repro_build.sh`.

**There is no `default` `.bin` because `espflash save-image` (4.5.0) refuses to make one:** the
ESP-IDF *application descriptor* is missing. The `esp_app_desc!()` macro is `wifi`-gated in
`main.rs`, so the no-radio build never emits one, and espflash 4.5 hard-requires it — with or
without a partition table. The ELF is attached for completeness; **the no-radio tier is not
packageable as a flashable image today.** Filed as a follow-up rather than papered over.

---

## Flashing a downloaded image

Always verify first:

```bash
sha256sum -c SHA256SUMS
```

Then:

```bash
# ⚠️ If this board has EVER taken an OTA, clear otadata FIRST — see the trap below.
espflash erase-region --port /dev/ttyACM0 0xf000 0x2000

# Flash the merged image.
espflash write-bin --port /dev/ttyACM0 0x0 smol-esp32c3-fleet.bin
```

### ⚠️ The otadata trap

**After ANY OTA, a USB flash silently lands in the slot that will not run.** The OTA left otadata
pointing at `ota_1`; a subsequent `espflash` write goes to `ota_0`, **succeeds**, and the board
keeps booting the OTA'd image. It reads as a brick or a failed flash and is **neither** — you
flashed fine, into the slot the bootloader is not selecting. The `erase-region` above clears
otadata **only**, sparing `nvs`, so a provisioned node id survives. Full detail:
[BUILDING.md → Gotchas](BUILDING.md#gotchas-the-ones-that-cost-us-time).

**Check the `Loaded app from offset` line after every flash.** It names the slot that actually ran,
and it is the only cheap confirmation you flashed the thing you are about to debug. *(That line
cost an hour on 2026-07-28 while it lived only in operator memory and no `docs/` file.)*

To flash from the ELF instead — `espflash flash --port /dev/ttyACM0 smol-esp32c3-fleet.elf` — note
that this repo's `.cargo/config.toml` runner passes `--partition-table partitions-ota.csv`, which
the merged `.bin` already embeds.

---

## Honest status of a published nightly

`nightly-2026-08-24` was **not flashed**. No board, broker or serial port was touched producing it.
The images compile, link and package; **nothing about that release is evidence that they boot.**

That is the standard this repo holds itself to elsewhere (see
[DOC-UPKEEP.md](DOC-UPKEEP.md)), and a release page is exactly where it is most tempting to drop
it. A future release that *has* been flashed should say so, and say on what.

---

## See also

- [BUILDING.md](BUILDING.md) — toolchain, `secrets.rs`, flashing, and the gotchas in full.
- [ota.md](ota.md) — the signed OTA path fleet boards actually use, including leaf mesh-OTA.
- [protocol.md](protocol.md) — the wire contract, including the image target descriptor a board
  uses to refuse an image that is not for it.
