# Upstream report DRAFT — Xtensa fat-LTO register-scavenger crash

**Status: DRAFT ONLY — NOT FILED. JP decides whether/where to post.**

Two caveats govern this document (both from the discovering run, BUDGET-PREP §6.1, 2026-08-25):

- ⚠️ **Repo choice unverified.** Best guess `esp-rs/rust` (the failing compiler is its `esp`
  channel), expecting triage onward to `espressif/llvm-project` if confirmed as a backend bug —
  but nobody has checked which repo accepts codegen bugs, and **no duplicate search has been
  done**.
- ⚠️ **No minimal reproducer exists.** The current reproduction is "a whole firmware", which is
  not a bug report. `cargo-bisect-rustc` likely does not apply (the fork is not a rustc channel);
  bracketing by esp-hal version may be the practical substitute. Reduce before filing.

smol itself is unblocked (per-chip `opt_level = 2` workaround, `tools/build-matrix.toml`
`[chip.esp32s3]`), so this files whenever it files — the workaround row reverts when upstream
fixes it, which is the reason to file at all.

---

## Escalation addendum (2026-08-25 ~10:2x)

- **Reproduces on the NEWEST fork release**: installed `1.98.0.0` as a separately-named
  toolchain (`esp-test`, katana only — the fleet's `esp` pin untouched) and re-ran the
  fleet-tier repro: **identical `Incomplete scavenging after 2nd pass` at opt=s + fat LTO.**
  The bump path is dead for this member; the workaround row stands, and "affects latest"
  belongs in the report when filed.
- **The family has a THIRD member** (found by the esp32c6-watch lane's S3 seam, their
  branch `feat/cyd-c5-target` @ a97a9d0): `LLVM ERROR: Cannot select:
  XtensaISD::PCREL_WRAPPER TargetConstantPool [2 x float]` — triggered by a Slint scene
  set's float constant pool, at opt 1, 2 AND 3 under fat LTO (thin hits the scavenger;
  lto=off hits a spill crash). Unlike member 1 it has NO known opt-level escape. Their
  repro: `tools/build-s3.sh` on that branch, no hardware needed. **Member 3 also reproduces on 1.98.0.0** (watch-lane test, same branch, opt=2 fat LTO,
  same LTO codegen stage). Both tested members fail on the newest release: **the
  toolchain-bump path is dead for the whole family**, and "affects latest" holds for the
  report across members.

## The report body (draft)

**Title.** `LLVM ERROR: Incomplete scavenging after 2nd pass on xtensa-esp32s3-none-elf at
opt-level=s with fat LTO`

**Environment.** Xtensa Rust fork **1.95.0.0** (`cargo 1.95.0-nightly (f2d3ce0bd 2026-03-21)`),
espup-installed `esp` toolchain, host x86_64-linux. Target `xtensa-esp32s3-none-elf`,
`-Zbuild-std=core,alloc`. esp-hal 1.1.2 / esp-radio 0.18.0 / esp-rtos 0.3.0.

**Reproduce.** A large `no_std` binary crate; `-C opt-level=s -C lto=fat -C codegen-units=1`.
Fails during codegen of the final binary.

**Observed matrix** (the useful part — it brackets the trigger):

| opt | lto | cu | result |
|---|---|---|---|
| s | fat | 1 | `Incomplete scavenging after 2nd pass` |
| s | fat | 2 | same |
| z | fat | 1 | `Error while trying to spill A8 from class AR: Cannot scavenge register without an emergency spill slot!` (in `compiler_builtins`) |
| 2 | fat | 1 | **OK** |
| 3 | fat | 1 | **OK** |
| s | thin | 1 | **OK** |

So: size-optimising levels + fat LTO only. Both messages come from the register scavenger
(**inferred** from the messages — standard reading, not proven).

**Separately worth reporting** (different bug, surfaced by the same matrix run): at
`codegen-units = 4` and `16` the build fails earlier with `error: invalid operand for
instruction` on `rsr a3, LBEG` in inline asm from a dependency — an assembler/subtarget-feature
issue, not a scavenger one. Observed, not diagnosed.
