use tapstone_rules::{Kind, Record};

#[test]
fn record_roundtrips_through_24_bytes() {
    let r = Record {
        seq: 7,
        seat: 1,
        kind: Kind::CastUnit,
        card: 3,
        lane: 2,
        target: 0,
        aux: 0,
        time_ms: 123456,
        uid: [1, 2, 3, 4, 5, 6, 7],
        auth: 0,
    };
    let b = r.encode();
    assert_eq!(b.len(), 24);
    assert_eq!(Record::decode(&b).unwrap(), r);
}

#[test]
fn unknown_kind_byte_is_an_error() {
    let mut b = Record {
        seq: 0,
        seat: 0,
        kind: Kind::Pass,
        card: 0,
        lane: -1,
        target: 0,
        aux: 0,
        time_ms: 0,
        uid: [0; 7],
        auth: 0,
    }
    .encode();
    b[3] = 0xEE;
    assert!(Record::decode(&b).is_none());
}

#[test]
fn short_slice_is_an_error() {
    let b = [0u8; 23];
    assert!(Record::decode(&b).is_none());
}

#[test]
fn reserved_bytes_are_ignored_and_negative_lane_roundtrips() {
    let r = Record {
        seq: 9,
        seat: 0,
        kind: Kind::Advance,
        card: 0,
        lane: -1,
        target: 0,
        aux: 0,
        time_ms: 42,
        uid: [7; 7],
        auth: 1,
    };
    let mut b = r.encode();
    b[9] = 0xFF;
    b[22] = 0xFF;
    b[23] = 0xFF;
    let d = Record::decode(&b).unwrap();
    assert_eq!(d.lane, -1);
    assert_eq!(d, r);
}
