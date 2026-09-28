use tapstone_proto::transcript::{
    HEADER_LEN, RECORD_LEN, TranscriptHeader, record_bytes, transcript_sha,
};
use tapstone_rules::{Kind, Record};

fn header() -> TranscriptHeader {
    TranscriptHeader {
        match_id: 0xA300_1F2C,
        ruleset: 1,
        registry: 2,
        rules: 3,
        seat_nodes: [163, 164],
        deck_sigils: [0xC04B_7A11, 0xE9F2_B3D5],
        start_ts: 1_789_980_000,
    }
}

#[test]
fn the_header_is_thirty_six_bytes_derived_from_its_fields() {
    // magic, match, ruleset, registry, rules, two nodes, two deck sigils, start_ts, pad.
    let fields = [4, 4, 4, 4, 4, 1, 1, 4, 4, 4, 2];
    assert_eq!(HEADER_LEN, fields.iter().sum::<usize>());
    assert_eq!(RECORD_LEN, Record::LEN + 8);
    let b = header().encode();
    assert_eq!(&b[..4], b"TSX1");
    assert_eq!(TranscriptHeader::decode(&b), Some(header()));
    assert_eq!(TranscriptHeader::decode(&b[..35]), None);
}

#[test]
fn the_sha_commits_to_every_byte() {
    let r = Record {
        seq: 2,
        seat: 0,
        kind: Kind::Pass,
        card: 0,
        lane: -1,
        target: 0,
        aux: 0,
        time_ms: 9,
        uid: [0; 7],
        auth: 0,
    };
    let recs = [record_bytes(&r, Some([1; 8])), record_bytes(&r, None)];
    let h = header().encode();
    let base = transcript_sha(&h, &recs);
    for i in 0..HEADER_LEN {
        let mut h2 = h;
        h2[i] ^= 1;
        assert_ne!(
            transcript_sha(&h2, &recs),
            base,
            "header byte {i} not covered"
        );
    }
    for i in 0..RECORD_LEN {
        let mut r2 = recs;
        r2[1][i] ^= 1;
        assert_ne!(transcript_sha(&h, &r2), base, "record byte {i} not covered");
    }
    assert_eq!(&recs[1][24..], &[0; 8], "a lobby record carries no hash");
}
