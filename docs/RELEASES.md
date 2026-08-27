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

## Per-target downloads (#413) — the manifests are the matrix

smol is five targets across four chip families now, so "the release artifact" stopped being a
single file. `tools/release_targets.sh` builds the download set **from the target manifests**:

```
targets/<name>/target.toml   →   name, chip, flavor, source, artifact = true|false
```

Iterating the manifests is the whole mechanism. **Adding a `targets/` folder with a manifest that
says `artifact = true` adds a download** — there is no workflow list to edit and no second place
that knows the roster. A manifest that says `artifact = false` **must say why in the file**, and
those reasons are the honest current state of the matrix:

A folder declares up to **two** downloads, because smol has two firmware flavors and one board
ships both. `artifact` is the `rust/clock` **fleet** image; `gui_artifact` + `gui_board` is the
`targets/c6-watch` **GUI** image (phase 3.1). They are independent, which is why they are
separate keys rather than one flag:

| target | chip | fleet | GUI | notes |
|---|---|---|---|---|
| `c3` | esp32c3 | ✅ | — | the canonical fleet image |
| `c3-oled` | esp32c3 | ✅ *alias* | — | `alias_of = "c3"` — **one image, two boards**; resolved, not built twice |
| `s3-cyd` | esp32s3 | ✅ | ✅ | **the only board shipping both flavors** — the case that forced the two axes apart. Xtensa: both need the espup `esp` toolchain |
| `c6-watch` | esp32c6 | ❌ | ✅ | no fleet image — `rust/clock` `cargo check`s clean for the C6 but cannot LINK one without the watch's `widen_rom_region` hook |
| `c5-cyd` | esp32c5 | ❌ | ✅ | **has a download while its fleet arm is still checks-only.** The C5 fleet image is CHECK-proven, not LINK-proven, and needs a measured budget row too |

### Three properties every per-target artifact carries

**1. It is built on the production path, not a parallel one.** Every artifact goes through the same
`repro_chip_spec` + `repro_build_bin` calls the OTA publish path uses, from a `git archive` tree
provisioned by `tools/ci_provision.sh`. A published image is **never** built from a tree carrying a
real `secrets.rs` — the archive+provision step is what makes that structural rather than a habit.
One provisioned tree is shared by every target in a run, deliberately: provisioning per-target
would substitute a different random key per artifact, and the images would then differ **by key
rather than by chip**.

**2. The build stamp is `0`, and that is the honest value.** A download is not a fleet-ratchet
build. Passing `SMOL_BUILD_NUMBER=0` explicitly stops a tree without the stage path's env
injection from stamping `version.txt`'s stale number (#420). **The artifact's identity is its git
hash**, which rides both the ELF and the release notes — not the number on the screen. It never
needs to win a ratchet comparison, because fleet boards do not update from downloads.

**3. Provenance rides the artifact.** Each `.bin` gets a `NOTES.md` beside it stating, in words a
reader with no repo context can act on:

- **the sha256, chip, flavor and git hash;**
- **who the image is for** — new hardware joining a mesh, never a board already on one;
- **its chip's stack-floor provenance**, spelled out rather than named. The three grades are not
  interchangeable: `derived` (a floor computed from a measured on-hardware peak — the strongest
  claim this project makes), `observed-sufficient` (⚠️ the measuring instrument is known-broken on
  that chip, so the floor is the largest region *proven to run clean in bench operation* — real
  protection, weaker provenance, and a regression that overruns it may not be caught before it
  ships), and `boot-assert` (⚠️ a declaration by the firmware itself, the weakest in the fleet);
- **the re-key instruction** (below);
- **the sha-lineage rule**: image shas are comparable **only within one (chip, profile) pair**. The
  S3 builds at a different opt-level — an LLVM scavenger workaround declared in
  `tools/build-matrix.toml` — and a different opt-level legitimately produces a different, equally
  correct image. Comparing an S3 sha against a C3 sha proves nothing.

### The GUI flavor inverts two of those rules — deliberately

The `targets/c6-watch` images (`smol-watch-c6`, `smol-c5-cyd-gui`, `smol-s3-cyd-gui`) are a
different program from the fleet images, and two of the properties above flip:

**1. Credentials are ABSENT, not placeheld.** The fleet bakes a *published placeholder* key so
the artifact stays byte-reproducible. The GUI images carry **nothing** — no SSID, no broker
credentials. This is the watch lane's ruling of record, from its own CI: *"Fake placeholders
were considered and REJECTED — they would bake a bogus DEFAULT SSID/broker into the released
artifact, which is worse than empty."* The firmware reads these with `option_env!` at compile
time, so with none set its own defaults apply: an empty SSID, and the compiled-in
`192.168.1.10:1883` broker literal, which is the honest public value rather than anyone's real
broker. WiFi and node id are then set **on the device** (the Settings app, or
`tools/provision.py`); the broker alone cannot be, because it is compile-time — point it at
your own by rebuilding.

That property is **gated, not asserted**. `tools/ci_provision_gui.sh` writes the build config
(the real one is gitignored and holds live credentials, so a `git archive` tree has none),
refuses to overwrite an existing one, and asserts no `option_env!` key reached it — with a
`CI_PROVISION_GUI_SELFTEST` mode that plants a key to prove the assertion can fail, run in CI
*before* the build that depends on it. Then `release_targets.sh` checks the **artifact**: it
requires the public default broker literal to be *present* in the image, because if a real
broker had been compiled in, the default would be dead code and LTO would drop it. A positive
control, not a hunt for secrets you refuse to enumerate — that kind of grep passes when it is
broken.

**2. They are NOT byte-reproducible, and this is measured.** Two cold builds of one commit, on
one machine, at the same canonical path, with `SOURCE_DATE_EPOCH` pinned, came out **identical
in size and different in 654,632 bytes** — five clusters, including a ~1 MB span across the
code region. The cause is **open**: a megabyte of scattered codegen difference is not something
a path remap would fix, so the fleet's determinism recipe does not transfer to this flavor and
nobody should assume it does. A remap is also deliberately *absent* for an independent reason —
it changes `.rodata` string lengths and hence the memory layout, and the watch's #65 established
that on this chip an 8-byte layout shift moved a crash from 0% to 100%; the release build keeps
the layout the bench builds were tested at. **A GUI artifact's identity is its git hash plus the
sha256 of the published file — do not compare it against a rebuild.**

Two smaller consequences: the GUI `.bin` is a full **16 MB** (a merged flash image spans the
whole part; the firmware is a few MB and the rest is padding), and **no `.elf` is published** —
the watch builds with `debug = 2`, which makes one ~80 MB against the fleet tier's 2 MB.

### 🔑 Re-key before you trust your mesh (#394)

*(Fleet images only — the GUI images have no key to re-key; see above.)*

Published images carry the **published placeholder group key** — on purpose, because a random
per-build key would destroy the byte-reproducibility that makes a sha an identity. The consequence
is worth stating flatly:

> **Placeholder-key boards can mesh only with other placeholder-key boards, and can never join a
> re-keyed fleet.**

To own your mesh: regenerate `GROUP_KEY` in `rust/clock/src/secrets.rs` (32 random bytes — start
from `secrets.rs.example`), rebuild, reflash. That is a rebuild, not a reconfiguration: the key is
compile-time, and no CFG frame or dashboard setting can change it.

*(The GUI flavor has the same shape with a different mechanism — its broker and credentials are
`option_env!` compile-time values, so its public images must carry placeholders too.)*

### Status, stated plainly

**The check that does not go stale: look at the [releases page](https://github.com/jphein/smol/releases).**
If the per-target `.bin` files described above are not attached to a release there, they have not
been published, whatever this document or any issue says. That is deliberate — the sentence below
is dated, and the releases page is not.

As of **2026-08-26**, the only published release is `nightly-2026-08-24`, carrying three C3 assets
and a `SHA256SUMS`. Everything this section describes — the manifests, `tools/release_targets.sh`,
the chip-aware stack-floor gate (#413 phase 2A), the publish path that builds any declared chip
(phase 2B), and the release workflow that runs them (phase 3) — is the mechanism, and the
mechanism landing is **not** the same event as an artifact appearing. The S3's remaining blocker is
narrow and named: a stock GitHub runner has no espup `esp` toolchain, which
`.github/workflows/xtensa-spike.yml` has already shown can be provisioned in-job.

**Until an artifact is on that page, per-target downloads are a capability, not a published fact.**

---

## Artifacts, and why the `default` tier has no `.bin`

`nightly-2026-08-24` publishes:

| file | tier | features | what it is |
|---|---|---|---|
| `smol-esp32c3-fleet.bin` | **canonical fleet** | `espnow,cast,io` | merged flashable image — bootloader + OTA partition table + app, 4 MiB |
| `smol-esp32c3-fleet.elf` | canonical fleet | `espnow,cast,io` | same build, for `probe-rs` / `readelf` / symbol work |
| `smol-esp32c3-default.elf` | `default` | none (no radio) | **ELF only** |
| `SHA256SUMS` | — | — | verify with `sha256sum -c --ignore-missing SHA256SUMS` |

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
# --ignore-missing: SHA256SUMS covers every artifact on the release; you downloaded one.
# Without it the check reports FAILED for the nine files you don't have and exits 1.
sha256sum -c --ignore-missing SHA256SUMS
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

- [RELEASE-RUNBOOK-v346.md](RELEASE-RUNBOOK-v346.md) — the **executable checklist** for cutting
  v346, written ahead of the event so task #19 runs from a list instead of a reconstruction. It
  deliberately does not restate the ceremony above, and it carries its own deletion condition.
- [BUILDING.md](BUILDING.md) — toolchain, `secrets.rs`, flashing, and the gotchas in full.
- [ota.md](ota.md) — the signed OTA path fleet boards actually use, including leaf mesh-OTA.
- [protocol.md](protocol.md) — the wire contract, including the image target descriptor a board
  uses to refuse an image that is not for it.
- [`../targets/`](../targets/) — the target roster itself. Each folder's `target.toml` is the
  manifest this page's per-target section describes, and each `README.md` is that board's own
  hardware truth.
