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
