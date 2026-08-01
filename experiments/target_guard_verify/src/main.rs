//! Host verification of the PURE #349 image-target guard. Includes the real
//! `net/target.rs` verbatim (`#[path]`, no drift). Run: `cargo run` — panics on failure.
//!
//! # Why this file is written the way it is
//!
//! smol's OTA chain proves an image is authentic and intact. It proves nothing about
//! **suitability**, and `smol/ota/staged` is retained fleet-wide, so every board sees every
//! announcement. #349 closes that with a structured `TargetId` embedded in the image; this
//! harness is the evidence that the guard built from it can say NO.
//!
//! That emphasis is deliberate. WLED ships a guard of exactly this shape whose minimum-version
//! half is **dead code**: the descriptor is instantiated with a literal `1` where the constant
//! `WLED_CUSTOM_DESC_VERSION` (`= 2`) belongs, and the check reads `> 1`, so the gate cannot
//! fire on any image they ship. A test that only proved "a matching image is accepted" would
//! have passed against that code, unchanged. So the accept case here is ONE assertion, and
//! every rejection reason gets its own — including `min_from_compat`, the field that is dead in
//! the prior art.
//!
//! The final case runs the real streaming scanner over a synthetic ~600 KB image with the
//! descriptor buried at an arbitrary offset, fed in chunk sizes that deliberately split the
//! 16-byte record, because "the scanner finds it when it is 16-byte aligned and alone in the
//! buffer" is not the thing that has to be true on a board.

#[path = "../../../rust/clock/src/net/target.rs"]
mod target;

use target::*;

/// A C3 board on the canonical fleet tier (`espnow,cast,io` → wifi|espnow|io|cast), NVS
/// layout 1. This stands in for `net::target::SELF`, which is cfg-gated to firmware builds.
fn c3_fleet() -> TargetId {
    TargetId {
        desc_version: DESC_VERSION,
        chip: CHIP_ESP32C3,
        features: FEAT_WIFI | FEAT_ESPNOW | FEAT_IO | FEAT_CAST,
        compat: 1,
        min_from_compat: 0,
    }
}

/// The legacy (pre-#349) firmware's announce entry point, transcribed exactly: `ota.rs` did
/// `s.strip_prefix("OTA|")?` and nothing else before splitting fields.
fn legacy_prefix(s: &str) -> Option<&str> {
    s.strip_prefix("OTA|")
}

/// The #349 firmware's dispatch: versioned by PREFIX, never by guessing at field shapes.
/// Returns `(has_target, rest_after_prefix)`.
fn new_prefix(s: &str) -> Option<(bool, &str)> {
    if let Some(r) = s.strip_prefix("OTA2|") {
        Some((true, r))
    } else {
        s.strip_prefix("OTA|").map(|r| (false, r))
    }
}

fn check(name: &str, got: Result<(), TargetReject>, want: Result<(), TargetReject>) {
    assert_eq!(got, want, "{name}: guard returned {got:?}, expected {want:?}");
    match want {
        Ok(()) => println!("  ok      {name} — accepted"),
        Err(r) => println!("  REFUSED {name} — {} ({:?})", r.label(), r),
    }
}

/// Build a synthetic image: `len` bytes of plausible-but-arbitrary filler with `desc`
/// spliced in at `at`. The filler is a cheap LCG rather than zeros so the scanner has to
/// survive bytes that look like data.
fn synth_image(len: usize, at: usize, desc: &[u8; DESC_LEN]) -> Vec<u8> {
    let mut img = Vec::with_capacity(len);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..len {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        img.push((x >> 24) as u8);
    }
    img[at..at + DESC_LEN].copy_from_slice(desc);
    img
}

/// Feed an image through the real scanner in irregular chunks, so the 16-byte descriptor is
/// split across `feed()` calls the way a 4 KB flash readback would split it.
fn scan_chunked(img: &[u8], chunk_sizes: &[usize]) -> DescScan {
    let mut scan = DescScan::new();
    let mut off = 0;
    let mut i = 0;
    while off < img.len() {
        let n = chunk_sizes[i % chunk_sizes.len()].min(img.len() - off);
        scan.feed(&img[off..off + n]);
        off += n;
        i += 1;
    }
    scan
}

fn main() {
    let me = c3_fleet();

    // ---------------------------------------------------------------------------------
    // 1. Codec round-trip. Everything below is worthless if the bytes do not survive.
    // ---------------------------------------------------------------------------------
    println!("codec");
    let enc = me.encode();
    assert_eq!(decode(&enc), Some(me), "encode→decode is not the identity");
    assert_eq!(&enc[0..4], &MAGIC[..], "descriptor does not start with the magic");
    assert_eq!(enc.len(), DESC_LEN);
    println!("  ok      round-trip, magic, {DESC_LEN} B");

    // A corrupt checksum must not decode — this is what lets a linear scan over ~600 KB of
    // arbitrary bytes trust a 4-byte magic hit.
    let mut torn = enc;
    torn[12] ^= 0x01;
    assert_eq!(decode(&torn), None, "a bad checksum still decoded");
    // ...and so must a flipped payload byte, which invalidates the checksum from the other side.
    let mut bent = enc;
    bent[6] ^= 0x08;
    assert_eq!(decode(&bent), None, "a flipped feature bit still decoded");
    assert_eq!(decode(&enc[..DESC_LEN - 1]), None, "a truncated record still decoded");
    println!("  ok      torn / bent / truncated records are refused");

    // ---------------------------------------------------------------------------------
    // 2. The accept case. Exactly one assertion — see the module doc.
    // ---------------------------------------------------------------------------------
    println!("accept");
    check("same board, same tier", decide(me, me), Ok(()));

    // A LEGITIMATE cross-tier upgrade must pass WITHOUT touching the checker. This is the
    // WLED `normalizeReleaseName()` lesson: their identity was one opaque string, so
    // permitting one real cross-upgrade meant hardcoding a suffix strip inside the guard.
    // Here dropping `bard` and `cast` is just a different bitset, and no code knows about it.
    let leaner = TargetId { features: FEAT_WIFI | FEAT_ESPNOW | FEAT_IO, ..me };
    check("same board, fewer optional features", decide(me, leaner), Ok(()));
    let fatter = TargetId { features: me.features | FEAT_WLED | FEAT_BARD, ..me };
    check("same board, more optional features", decide(me, fatter), Ok(()));

    // ---------------------------------------------------------------------------------
    // 3. THE REFUSALS. One per reason. This is the part that matters.
    // ---------------------------------------------------------------------------------
    println!("refuse");

    // (a) Wrong silicon. The bootloader would also catch this — by boot-looping into
    //     rollback. Catching it here costs a download and yields a diagnosis instead.
    check(
        "S3 image on a C3 board",
        decide(me, TargetId { chip: CHIP_ESP32S3, ..me }),
        Err(TargetReject::Chip),
    );
    check(
        "C6 image on a C3 board",
        decide(me, TargetId { chip: CHIP_ESP32C6, ..me }),
        Err(TargetReject::Chip),
    );
    // An image claiming no chip at all is not "compatible with everything".
    check(
        "image declaring CHIP_UNKNOWN",
        decide(me, TargetId { chip: CHIP_UNKNOWN, ..me }),
        Err(TargetReject::Chip),
    );

    // (b) An image that would take away the way back. `run_ota_fetch` is
    //     `#[cfg(feature = "espnow")]`, so a wifi-only image CANNOT self-update — installing
    //     one is a one-way trip to a USB cable, and is refused for the same reason as
    //     dropping wifi entirely.
    check(
        "image without espnow (board could never OTA again)",
        decide(me, TargetId { features: FEAT_WIFI | FEAT_IO, ..me }),
        Err(TargetReject::FeatureLoss),
    );
    check(
        "image without wifi at all",
        decide(me, TargetId { features: FEAT_IO | FEAT_CAST, ..me }),
        Err(TargetReject::FeatureLoss),
    );

    // (c) A bench-only tier reaching a fleet board over the retained fleet-wide staged topic.
    check(
        "mesh-test image on a fleet board",
        decide(me, TargetId { features: me.features | FEAT_MESH_TEST, ..me }),
        Err(TargetReject::FeatureForbidden),
    );
    check(
        "coexist-soak image on a fleet board",
        decide(me, TargetId { features: me.features | FEAT_COEXIST_SOAK, ..me }),
        Err(TargetReject::FeatureForbidden),
    );

    // (d) **The dead-in-WLED gate, firing.** An image that declares it will not install over
    //     NVS layouts older than 2, meeting a board still on layout 1.
    check(
        "image requiring NVS compat >= 2, board on 1",
        decide(me, TargetId { min_from_compat: 2, ..me }),
        Err(TargetReject::CompatTooOld),
    );
    //     ...and the same image on a board that HAS migrated is fine — a gate that refused
    //     unconditionally would be no better than one that never fires.
    let migrated = TargetId { compat: 2, ..me };
    check(
        "image requiring NVS compat >= 2, board on 2",
        decide(migrated, TargetId { min_from_compat: 2, ..migrated }),
        Ok(()),
    );

    // (e) A descriptor format from the future. We cannot read its fields, so we cannot judge
    //     suitability, so we do not install it.
    check(
        "descriptor version newer than we understand",
        decide(me, TargetId { desc_version: DESC_VERSION + 1, ..me }),
        Err(TargetReject::DescVersion),
    );

    // ---------------------------------------------------------------------------------
    // 4. The scanner, over a realistically-sized image, split across chunk boundaries.
    // ---------------------------------------------------------------------------------
    println!("scan");
    const IMG: usize = 600 * 1024;

    // Descriptor at an offset that is aligned to nothing in particular, fed in chunk sizes
    // that are coprime with 16 so the record is split differently on every pass.
    for &at in &[64usize, 4096 + 7, 137_213, IMG - DESC_LEN] {
        let img = synth_image(IMG, at, &enc);
        let scan = scan_chunked(&img, &[4096, 1, 3, 4095, 17, 7]);
        assert_eq!(scan.found(), Some(me), "descriptor at offset {at} was not found");
        check(&format!("found at offset {at}"), scan.verdict(me), Ok(()));
    }

    // The whole point, end to end: a REAL scan of a REAL image stream that ends in a refusal.
    let foreign = TargetId { chip: CHIP_ESP32S3, ..me };
    let img = synth_image(IMG, 90_000, &foreign.encode());
    let scan = scan_chunked(&img, &[4096, 1, 3, 4095, 17, 7]);
    assert_eq!(scan.found(), Some(foreign));
    check("scanned S3 image, judged by a C3 board", scan.verdict(me), Err(TargetReject::Chip));

    // An image that never says what it is for. Every build from #349 carries a descriptor and
    // the monotonicity gate already blocks older builds, so this is the foreign/hand-rolled
    // case — and it must fail CLOSED.
    let mute = synth_image(IMG, 0, &[0u8; DESC_LEN]);
    let scan = scan_chunked(&mute, &[4096, 1, 3, 4095, 17, 7]);
    assert_eq!(scan.found(), None, "found a descriptor in an image that has none");
    check("image with no descriptor", scan.verdict(me), Err(TargetReject::Absent));

    // A magic that appears by accident, with garbage behind it, must not be trusted — and must
    // not blind the scanner to the REAL descriptor that follows it. (The scanner resumes after
    // a failed candidate; if it did not, one stray "SMLT" in .rodata would silently disable the
    // guard on every image — the exact class of quiet failure #349 exists to prevent.)
    let mut decoy = synth_image(IMG, 200_000, &enc);
    decoy[150_000..150_004].copy_from_slice(&MAGIC);
    for i in 0..12 {
        decoy[150_004 + i] = 0xA5; // plausible bytes, wrong checksum
    }
    let scan = scan_chunked(&decoy, &[4096, 1, 3, 4095, 17, 7]);
    assert_eq!(scan.found(), Some(me), "a decoy magic hid the real descriptor");
    check("decoy magic before the real descriptor", scan.verdict(me), Ok(()));

    // ---------------------------------------------------------------------------------
    // 5. The numeric channels (`ota_fail` codes, the leaf's `dbg_verdict` byte) carry the
    //    reason as an ordinal. Round-trip every reason so a wire code can never be decoded
    //    as a different refusal than the one that happened.
    // ---------------------------------------------------------------------------------
    println!("codes");
    let all = [
        TargetReject::Absent,
        TargetReject::DescVersion,
        TargetReject::Chip,
        TargetReject::CompatTooOld,
        TargetReject::FeatureLoss,
        TargetReject::FeatureForbidden,
    ];
    assert_eq!(all.len() as u8, TargetReject::COUNT, "TargetReject::COUNT is stale");
    for r in all {
        assert_eq!(TargetReject::from_code(r.code()), Some(r), "{r:?} code round-trip failed");
        assert!(r.code() < TargetReject::COUNT, "{r:?} code is outside COUNT");
        assert!(r.label().len() <= 12, "{r:?} label is too long for the capped diag payload");
    }
    assert_eq!(TargetReject::from_code(TargetReject::COUNT), None, "unknown code decoded");
    println!("  ok      {} reasons round-trip through their ordinals", TargetReject::COUNT);

    // ---------------------------------------------------------------------------------
    // 6. The MANIFEST — both generations. This is the stranding-risk surface: if the new
    //    parser stopped accepting the legacy form, every board that has not yet been rolled
    //    would silently stop seeing updates, and the only symptom would be a fleet that
    //    quietly stays on an old build. So the legacy form is asserted, not assumed.
    // ---------------------------------------------------------------------------------
    println!("manifest");
    let sha_hex = "a".repeat(64);
    let tgt_hex = core::str::from_utf8(&encode_hex(&me)).unwrap().to_string();

    // #32 legacy M — target None, and that must NOT read as "suitable for anyone".
    let legacy = parse_manifest_str(&format!("905|1156864|{sha_hex}")).expect("legacy M rejected");
    assert_eq!(legacy.build, 905);
    assert_eq!(legacy.size, 1_156_864);
    assert_eq!(legacy.target, None, "legacy M must yield NO target, not a permissive one");
    println!("  ok      legacy `build|size|sha` still parses, target=None");

    // #349 M — target present and equal to what we encoded.
    let v2 = parse_manifest_str(&format!("905|1156864|{sha_hex}|{tgt_hex}")).expect("OTA2 M rejected");
    assert_eq!(v2.target, Some(me), "OTA2 M did not round-trip the target");
    assert_eq!((v2.build, v2.size), (905, 1_156_864));
    println!("  ok      OTA2 `build|size|sha|target` parses, target round-trips");

    // Hex codec edges. A corrupted target must fail the WHOLE manifest, never degrade to None.
    assert_eq!(decode_hex(&tgt_hex[..30]), None, "short target hex decoded");
    assert_eq!(decode_hex(&format!("{}zz", &tgt_hex[..30])), None, "non-hex target decoded");
    let mut bad = tgt_hex.clone();
    bad.replace_range(12..13, if &bad[12..13] == "0" { "1" } else { "0" });
    assert_eq!(decode_hex(&bad), None, "checksum-broken target hex decoded");
    assert_eq!(
        parse_manifest_str(&format!("905|1156864|{sha_hex}|{bad}")),
        None,
        "a manifest with a CORRUPT target parsed anyway — it must fail closed, not fall back to None",
    );
    println!("  ok      corrupt target hex fails the whole manifest (never degrades to None)");

    // Trailing-junk / short-field fail-closed.
    assert_eq!(parse_manifest_str(&format!("905|1156864|{}", &sha_hex[..62])), None);
    assert_eq!(parse_manifest_str(&format!("905|1156864|{sha_hex}|{tgt_hex}|extra")), None);
    assert_eq!(parse_manifest_str("905|notanumber|"), None);
    println!("  ok      short sha / 5th field / bad integer all fail closed");

    // M must fit the signed-message buffer the firmware allocates. Worst case is two 10-digit
    // u32s; if this ever exceeds SIGNED_MSG_MAX the OTAM frame silently truncates.
    let worst = format!("{}|{}|{}|{}", u32::MAX, u32::MAX, sha_hex, tgt_hex);
    assert!(parse_manifest_str(&worst).is_some(), "worst-case M does not parse");
    println!("  ok      worst-case M is {} B (firmware SIGNED_MSG_MAX must be >= this)", worst.len());
    assert!(worst.len() <= 128, "worst-case M ({} B) exceeds SIGNED_MSG_MAX=128", worst.len());

    // ---------------------------------------------------------------------------------
    // 6b. THE NO-STRANDING CLAIM, as a test.
    //
    //     The entire migration rests on one sentence: "firmware older than #349 ignores an
    //     `OTA2|` line cleanly instead of mis-parsing it." That is a claim about
    //     `str::strip_prefix("OTA|")`, and asserting it in a comment is how it would silently
    //     stop being true. So here is the legacy parser's first step, verbatim, run against
    //     both lines. If this ever fails, publishing OTA2 would strand every un-rolled board —
    //     the exact failure this whole design is sequenced to avoid.
    // ---------------------------------------------------------------------------------
    println!("no-stranding");
    let url = "http://10.0.0.1:8087/ota/smol-905.bin";
    let sig_hex = "b".repeat(128);
    let legacy_line = format!("OTA|905|1156864|{sha_hex}|{sig_hex}|{url}");
    let v2_line = format!("OTA2|905|1156864|{sha_hex}|{tgt_hex}|{sig_hex}|{url}");

    // The legacy firmware's ONLY entry point into announce parsing.
    assert!(legacy_prefix(&legacy_line).is_some(), "legacy fw stopped accepting its own format");
    assert!(
        legacy_prefix(&v2_line).is_none(),
        "PRE-#349 FIRMWARE WOULD MIS-PARSE AN OTA2 LINE — publishing it would strand the fleet",
    );
    println!("  ok      pre-#349 fw accepts `OTA|`, cleanly REJECTS `OTA2|` (no mis-slice)");

    // And the new firmware's dispatch accepts both — which is what makes dual-publish work.
    assert_eq!(new_prefix(&legacy_line).map(|(t, _)| t), Some(false));
    assert_eq!(new_prefix(&v2_line).map(|(t, _)| t), Some(true));
    println!("  ok      #349 fw accepts BOTH, and tells them apart by prefix (not by shape)");

    // The OTA2 line's signed prefix must be exactly the M we sign — reconstructed from the
    // wire bytes, since that is how the firmware rebuilds it before verifying.
    let (_, rest) = new_prefix(&v2_line).unwrap();
    let m_expect = format!("905|1156864|{sha_hex}|{tgt_hex}");
    assert!(rest.starts_with(&m_expect), "M is not a contiguous prefix of the OTA2 payload");
    assert_eq!(
        parse_manifest_str(&rest[..m_expect.len()]).and_then(|m| m.target),
        Some(me),
        "the signed prefix of the wire line does not parse back to the target",
    );
    println!("  ok      M is a contiguous prefix of the wire line and re-parses to the target");

    // ---------------------------------------------------------------------------------
    // 7. OPTIONAL: the same scanner over a REAL flashed image.
    //
    //    Everything above runs on bytes this file made up. `SMOL_TARGET_VERIFY_BIN=<path>`
    //    points it at an actual `espflash save-image` artifact instead, which is the only way
    //    to prove the descriptor SURVIVES the link — `#[used]` guards compiler DCE, not
    //    `--gc-sections`, and a guard whose descriptor was quietly dropped by the linker
    //    would look exactly like a guard that works until the day it has to refuse something.
    //
    //    Skipped by default so CI stays hermetic (the gate has no espflash).
    // ---------------------------------------------------------------------------------
    if let Ok(path) = std::env::var("SMOL_TARGET_VERIFY_BIN") {
        println!("real image  {path}");
        let img = std::fs::read(&path).expect("SMOL_TARGET_VERIFY_BIN is not readable");
        let scan = scan_chunked(&img, &[4096]); // the real flash-readback chunk size
        let found = scan.found().expect("no valid target descriptor in the built image");
        println!(
            "  ok      {} B, descriptor found: chip={} features=0x{:04x} compat={} min_from={}",
            img.len(),
            found.chip,
            found.features,
            found.compat,
            found.min_from_compat,
        );
        assert_eq!(found.desc_version, DESC_VERSION);
        assert_ne!(found.chip, CHIP_UNKNOWN, "the built image claims no chip");
        assert!(found.features & FEAT_WIFI != 0, "an OTA-capable image with no wifi bit");
        // The board this image is for accepts it; a board on other silicon does not. Same
        // bytes, same checker, opposite verdicts — the whole issue in two lines.
        let native = TargetId { compat: found.compat, ..found };
        check("real image on its own chip", scan.verdict(native), Ok(()));
        let other = if found.chip == CHIP_ESP32S3 { CHIP_ESP32C3 } else { CHIP_ESP32S3 };
        check(
            "real image on other silicon",
            scan.verdict(TargetId { chip: other, ..native }),
            Err(TargetReject::Chip),
        );
    } else {
        println!("real image  skipped (set SMOL_TARGET_VERIFY_BIN=<path> to scan a built .bin)");
    }

    println!("\ntarget_guard_verify: PASS — the guard accepts what it should and REFUSES 9 ways");
}
