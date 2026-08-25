//! SMOL-SIDE tag-collision guard for the ELECT frame (#328). Additive and smol-only: it does not
//! touch the wire format, and it is kept out of `wire_tests.rs` / `consensus.rs` so those two stay
//! verbatim-diffable against `esp32c6-watch:crates/mesh-elect/tests/` — the same split
//! `follow_tests.rs` already uses.
//!
//! # Why this exists, given `wire_tests::tag_does_not_collide_with_existing_frames` already runs
//!
//! That test is the DONOR's, and it is verbatim by design — which means smol also inherited its
//! blind spot. Its comment claims it covers *"Every tag in use across both repos today."* On this
//! repo that sentence is false, and has been since it was ported:
//!
//! - it lists 15 tags, **three of which smol does not have** (`PING`, `PINGACK`, `SAY` are
//!   watch-only), and
//! - it **misses nine real smol tags** — `FAM`, `SNK`, `LDBG`, `ODEL`, `ODON`, `OTAD`, `OTAM`,
//!   `OTAN` — plus the versioned `BATT2` / `GRID2` / `UP2` / `RELAYACK2`.
//!
//! The conclusion it draws (byte 7 `b'E'` is free) is still TRUE — verified below against smol's
//! real tag set, not assumed. But a claim that holds by luck is not a guard. smol owns the smol
//! half of a cross-repo claim, so it is asserted here from smol's own prefixes.
//!
//! # Why the list is SCANNED and not written down
//!
//! The thing that actually went wrong was not the missing tags — it was that a hardcoded list
//! stopped describing the codebase and **nothing said so**. Re-typing a fresh list here would rot
//! exactly the same way, one new frame tag from now, and would be the same defect with a newer
//! date on it. (Concretely: the list this guard was first drafted from still carried a bare
//! `SMOLv1 UP `, which no longer exists on `main` — it had already rotted before it was ported.)
//!
//! So the prefix set is derived from the firmware source at run time, and the assertions are made
//! against whatever is there today. A new `SMOLv1 <TAG> ` frame is picked up automatically; if it
//! ever collides with `ELECT`, this fails the gate on the commit that introduces it rather than in
//! a partition six months later.
//!
//! `tools/gate.sh` already made this argument about its own verifier discovery, and it is the same
//! argument one level in:
//!
//! > *Discovered by GLOB, not by a hardcoded list: a new verifier is picked up automatically, which
//! > is the whole point — the last list-shaped gate silently stopped covering what was added after
//! > it.*

use crate::mesh_elect::wire::ELECT_PREFIX;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Firmware source root — `rust/clock/src`, resolved from this crate rather than from the process
/// CWD so the guard behaves the same under `cargo run` and under `tools/gate.sh`.
fn clock_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rust/clock/src")
}

/// Every `.rs` file under `rust/clock/src`, recursively.
fn rust_sources(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read firmware source dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("cannot stat a firmware source entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Scan the firmware for every `SMOLv1` frame prefix it emits or decodes.
///
/// Matches the BYTE-LITERAL form `b"SMOLv1 <TAG> "` only. That is deliberate and it is the reason
/// this scan is trustworthy: prose mentions the string too (`secrets.rs.example` says *"Every
/// SMOLv1 ESP-NOW frame carries a truncated…"*), and a looser grep reads `ESP` as a frame tag and
/// invents a byte-7 collision with `ELECT` that does not exist. Only the literal that a parser can
/// actually dispatch on counts.
fn scan_smol_prefixes() -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_sources(&clock_src(), &mut files);
    assert!(!files.is_empty(), "scanned no firmware sources — the guard would vacuously pass");

    let mut found = BTreeSet::new();
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        for (idx, _) in text.match_indices("b\"SMOLv1 ") {
            let rest = &text[idx + 2..]; // past the `b"`
            let Some(end) = rest.find('"') else { continue };
            let literal = &rest[..end];
            // A dispatchable prefix is `SMOLv1 <TAG> ` — uppercase/digit tag, one trailing space.
            let Some(tag) = literal.strip_prefix("SMOLv1 ").and_then(|t| t.strip_suffix(' ')) else {
                continue;
            };
            if !tag.is_empty() && tag.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
                found.insert(literal.to_string());
            }
        }
    }
    assert!(
        found.len() >= 20,
        "scanned only {} SMOLv1 prefixes — the scanner has probably broken, and a guard that \
         silently stops finding things is the defect this file exists to prevent",
        found.len()
    );
    found
}

/// Byte 7 is the tag's first character — what `parse_frame` keys on after the shared `SMOLv1 `
/// header, and what a human scanning a serial log keys on too. No smol frame may share it with
/// `ELECT`, or an old node misparses an announcement instead of ignoring it.
pub fn elect_tag_is_free_in_smol() {
    assert_eq!(ELECT_PREFIX[7], b'E');
    let elect = std::str::from_utf8(ELECT_PREFIX).expect("ELECT_PREFIX is ASCII");

    let mut checked = 0usize;
    for prefix in scan_smol_prefixes() {
        if prefix == elect {
            continue;
        }
        let byte7 = prefix.as_bytes()[7];
        assert_ne!(
            byte7, ELECT_PREFIX[7],
            "tag collision: {prefix:?} shares byte 7 ({:?}) with {elect:?}",
            byte7 as char
        );
        checked += 1;
    }
    assert!(checked > 0, "no non-ELECT prefixes were checked");
    println!("  smol_wire_compat: byte 7 'E' is free across {checked} live smol prefixes");
}

/// The stronger statement, and the one that actually protects the parsers: no smol frame prefix and
/// `ELECT` may be a prefix of one another. Byte 7 differing implies this today, but the tags are
/// variable-length (`RELAY` / `RELAYACK` / `RELAYACK2` already nest), so the property worth pinning
/// is the one about dispatch, not the one about a single byte.
pub fn no_smol_prefix_nests_with_elect() {
    let elect = std::str::from_utf8(ELECT_PREFIX).expect("ELECT_PREFIX is ASCII");
    for prefix in scan_smol_prefixes() {
        if prefix == elect {
            continue;
        }
        assert!(
            !prefix.starts_with(elect) && !elect.starts_with(&prefix),
            "prefix nesting between {prefix:?} and {elect:?} — one parser would shadow the other"
        );
    }
}

/// `ELECT`'s own parser must reject every other smol frame outright. `wire_tests` proves it rejects
/// malformed ELECT frames; this proves it rejects well-formed frames of a DIFFERENT type, which is
/// what actually arrives on a shared broadcast channel.
pub fn elect_parser_rejects_every_other_smol_frame() {
    use crate::mesh_elect::wire::parse;
    let elect = std::str::from_utf8(ELECT_PREFIX).expect("ELECT_PREFIX is ASCII");
    for prefix in scan_smol_prefixes() {
        if prefix == elect {
            continue;
        }
        // Prefix + plausible payload, padded well past ELECT_LEN so length alone is not the reason
        // it is rejected.
        let mut frame = prefix.clone().into_bytes();
        frame.extend(std::iter::repeat_n(b'0', 64));
        assert!(
            parse(&frame).is_none(),
            "ELECT parse() accepted a {prefix:?} frame — it would feed a foreign frame into the \
             channel announcement path"
        );
    }
}
