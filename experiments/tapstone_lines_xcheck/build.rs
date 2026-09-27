//! Copy tapstone's `link/lines.rs` into OUT_DIR so `main.rs` can `include!` it. The copy drops the
//! file's leading `//!` module docs (inner attributes are not allowed at an `include!` site) and
//! nothing else; the digest of the ORIGINAL file is printed at run time so a log names the exact
//! tapstone bytes that were checked.
use std::{env, fs, path::PathBuf};

fn main() {
    let dir = env::var("TAPSTONE_DIR").unwrap_or_else(|_| {
        let here = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        here.join("../../../tapstone").to_string_lossy().into_owned()
    });
    let src = PathBuf::from(&dir).join("rust/tapstone-arena/src/link/lines.rs");
    println!("cargo:rerun-if-env-changed=TAPSTONE_DIR");
    println!("cargo:rerun-if-changed={}", src.display());
    let text = fs::read_to_string(&src)
        .unwrap_or_else(|e| panic!("tapstone lines.rs not found at {} ({e}); set TAPSTONE_DIR", src.display()));
    let body: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//!"))
        .map(|l| format!("{l}\n"))
        .collect();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("tapstone_lines.rs"), body).unwrap();
    // FNV-1a over the original bytes: dependency-free, enough to name a revision in a log.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h = (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    println!("cargo:rustc-env=TAPSTONE_LINES_PATH={}", src.display());
    println!("cargo:rustc-env=TAPSTONE_LINES_FNV={h:016x}");
}
