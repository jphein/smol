//! #13 host byte-compat guard for the SMOLv1 relay wire codec. `#[path]`-includes the REAL
//! `net/wire.rs` (no drift) and asserts the mixed-fleet / rolling-upgrade contract BOTH directions:
//!   (1) a NEW-code frame parses under a VENDORED pre-#13 parser (new leaf -> old gateway), and
//!   (2) an old-format RELAYACK parses under the NEW matcher (old gateway -> new leaf).
//! Plus GOLDEN-BYTE assertions pinning the exact wire layout, and the #13 tag round-trips +
//! disambiguation (a plain RELAY must never classify as RELAY2, and vice-versa). This is the
//! regression the bench-cornered C0 investigation asked for + the guard for the #124 UP2 migration.
//! Run: `cargo run` — panics on any mismatch.

#[path = "../../../rust/clock/src/net/wire.rs"]
mod wire;

// --- VENDORED pre-#13 RELAY parser (frozen copy of mode.rs @ 14868c2, the byte-compat baseline).
// If the REAL `wire::encode_relay` ever drifts from the pre-#13 wire, direction (1) fails loudly. ---
const OLD_RELAY_PREFIX: &[u8] = b"SMOLv1 RELAY ";

fn old_parse_id(rest: &[u8]) -> Option<u8> {
    if rest.len() < 3 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in &rest[..3] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u16;
    }
    (val <= 255).then_some(val as u8)
}

fn old_parse_u5(rest: &[u8]) -> Option<u32> {
    if rest.len() < 5 {
        return None;
    }
    let mut val: u32 = 0;
    for &b in &rest[..5] {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u32;
    }
    Some(val)
}

fn old_parse_relay(data: &[u8]) -> Option<(u8, u16, u8, u8, &[u8])> {
    let rest = data.strip_prefix(OLD_RELAY_PREFIX)?;
    if rest.len() < 14 {
        return None;
    }
    let src_id = old_parse_id(&rest[0..3])?;
    let msgid = u16::try_from(old_parse_u5(&rest[4..9])?).ok()?;
    if !rest[10].is_ascii_digit() || !rest[12].is_ascii_digit() {
        return None;
    }
    Some((src_id, msgid, rest[10] - b'0', rest[12] - b'0', &rest[14..]))
}

fn main() {
    // (1) NEW frame -> OLD parser (a #13 leaf's H=1 RELAY on an old gateway) -----------------
    let mut fb = [0u8; 128];
    let chunk = b"cpu=42 rssi=-70 Grid:1"; // realistic short telemetry
    let n = wire::encode_relay(7, 12345, 0, 2, chunk, &mut fb);
    // GOLDEN bytes — the exact documented RELAY wire layout. This is the #124 migration checkpoint.
    let golden: &[u8] = b"SMOLv1 RELAY 007 12345 0 2 cpu=42 rssi=-70 Grid:1";
    assert_eq!(&fb[..n], golden, "encode_relay H=1 golden wire bytes");
    let (id, mid, frag, cnt, ch) =
        old_parse_relay(&fb[..n]).expect("pre-#13 parser MUST accept a new leaf's H=1 RELAY");
    assert_eq!((id, mid, frag, cnt), (7, 12345, 0, 2), "old parser fields");
    assert_eq!(ch, chunk, "old parser chunk");
    // a max-realistic fragment (RELAY_CHUNK bytes) still round-trips through the old parser.
    let big = [b'x'; wire::RELAY_CHUNK];
    let n2 = wire::encode_relay(255, 65000, 3, 4, &big, &mut fb);
    let (_, _, _, _, ch2) = old_parse_relay(&fb[..n2]).expect("old parser accepts a max fragment");
    assert_eq!(ch2, &big[..], "old parser max chunk");

    // (2) OLD-format RELAYACK -> NEW matcher (an old gateway's ACK on a #13 leaf) --------------
    // The bug the bench chased was "the #13 leaf doesn't recognise the plain RELAYACK". It does —
    // this asserts it against a hand-built old-format frame.
    let old_ack: &[u8] = b"SMOLv1 RELAYACK 12345 003"; // msgid 12345, bitmap 0b011
    let (mid_a, bm) = wire::parse_relayack(old_ack).expect("new matcher MUST accept an old RELAYACK");
    assert_eq!((mid_a, bm), (12345, 3), "new matcher parses old RELAYACK fields");
    // and the new encoder reproduces that exact frame (golden).
    let mut ab = [0u8; 32];
    let an = wire::encode_relayack(12345, 3, &mut ab);
    assert_eq!(&ab[..an], old_ack, "encode_relayack golden wire bytes");

    // #13/#124 RELAYACK2 round-trip + DISAMBIGUATION (the no-flag-day guarantee) --------------
    // (RELAY2 is retired — replaced by UP2 below; RELAYACK2 stays as the flooded ACK.)
    // the gateway's origin-keyed reassembly MAC (never aliases a real Espressif MAC).
    assert_eq!(wire::synth_origin_mac(9), [0, 0, 0, 0, 0, 9], "synth_origin_mac layout");

    let mut a2 = [0u8; 40];
    let a2n = wire::encode_relayack2(9, 42, 3, 2, &mut a2);
    assert!(wire::parse_relayack(&a2[..a2n]).is_none(), "RELAYACK2 must NOT parse as plain RELAYACK");
    assert_eq!(wire::parse_relayack2(&a2[..a2n]).unwrap(), (9, 42, 3, 2), "RELAYACK2 round-trip");
    assert!(wire::parse_relayack2(old_ack).is_none(), "plain RELAYACK must NOT parse as RELAYACK2");

    // Stage B downlink freshness (BATT2/GRID2) round-trip + the seq survives verbatim.
    let mut dl = [0u8; 128];
    let dln = wire::encode_dl(wire::BATT2_PREFIX, 1_700_000_000, b"BATT|48V 52.8V", &mut dl);
    let (seq, pl) = wire::parse_dl(wire::BATT2_PREFIX, &dl[..dln]).expect("BATT2 round-trip");
    assert_eq!(seq, 1_700_000_000, "dl_seq survives verbatim");
    assert_eq!(pl, b"BATT|48V 52.8V", "BATT2 payload verbatim");
    // a BATT2 frame must not parse under the GRID2 prefix (tag isolation).
    assert!(wire::parse_dl(wire::GRID2_PREFIX, &dl[..dln]).is_none(), "BATT2 is not a GRID2");

    // #124 UP2 envelope — golden bytes + wrap→unwrap→re-parse-inner both directions + disambiguation.
    // Wrap a plain RELAY (the RELAY2-subsuming case): the gateway unwraps → the inner is a verbatim
    // RELAY that re-parses under the SAME parse_relay used for a single-hop leaf.
    let inner_relay = {
        let mut ib = [0u8; 128];
        let iln = wire::encode_relay(7, 100, 0, 1, b"cpu=42", &mut ib);
        ib[..iln].to_vec()
    };
    let mut up = [0u8; 256];
    let upn = wire::encode_up2(7, 5, 2, &inner_relay, &mut up);
    // GOLDEN: "SMOLv1 UP2 007 00005 2 " + <the verbatim RELAY frame>.
    let mut golden_up = b"SMOLv1 UP2 007 00005 2 ".to_vec();
    golden_up.extend_from_slice(&inner_relay);
    assert_eq!(&up[..upn], &golden_up[..], "encode_up2 golden wire bytes");
    let (o, em, h, inner) = wire::parse_up2(&up[..upn]).expect("parse_up2");
    assert_eq!((o, em, h), (7, 5, 2), "UP2 header: origin / envelope-msgid / hop");
    // the unwrapped inner is a verbatim RELAY → re-parses under the ordinary parser (gateway dispatch).
    let (rid, rmid, rfrag, rcnt, rchunk) = wire::parse_relay(inner).expect("inner is a verbatim RELAY");
    assert_eq!((rid, rmid, rfrag, rcnt, rchunk), (7, 100, 0, 1, &b"cpu=42"[..]), "inner RELAY round-trip");
    // envelope msgid (5) is DISTINCT from the inner RELAY msgid (100) — the two-layer contract.
    assert_ne!(em, rmid, "envelope msgid != inner RELAY msgid (flood-dedup vs reassembly layers)");
    // disambiguation: UP2 must NOT classify as a plain RELAY, and vice-versa.
    assert!(wire::parse_relay(&up[..upn]).is_none(), "UP2 must NOT parse as plain RELAY");
    assert!(wire::parse_up2(&fb[..n]).is_none(), "a plain RELAY must NOT parse as UP2");
    // CONSTRAINT 1: a max inner is clamped so the envelope never exceeds ESP_NOW_MTU.
    let big_inner = [b'z'; 240];
    let cn = wire::encode_up2(9, 1, 2, &big_inner, &mut up);
    assert!(cn <= wire::ESP_NOW_MTU, "UP2 clamps inner → frame never exceeds ESP_NOW_MTU");
    let (_, _, _, clamped) = wire::parse_up2(&up[..cn]).unwrap();
    assert_eq!(clamped.len(), wire::UP2_INNER_MAX, "inner clamped to UP2_INNER_MAX");

    println!("relay_compat: ALL CHECKS PASSED (bidirectional byte-compat + golden bytes + #13/#124 tag round-trips + disambiguation + UP2 clamp)");
}
