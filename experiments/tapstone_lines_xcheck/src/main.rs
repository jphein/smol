//! #548 cross-repo check. `tl` is tapstone's arena-side `link/lines.rs`, compiled from a tapstone
//! checkout (see build.rs); `ts_lines` is smol's gateway codec, `#[path]`-included. Every line
//! either side can write is read by the other side's real parser. Panics on the first mismatch.

#[allow(dead_code, clippy::all)]
mod tl {
    include!(concat!(env!("OUT_DIR"), "/tapstone_lines.rs"));
}

#[path = "../../../rust/clock/src/net/ts_lines.rs"]
#[allow(dead_code)]
mod ts_lines;

use std::cell::Cell;
use tl::GwLine;
use ts_lines::*;

thread_local!(static CHECKS: Cell<u32> = const { Cell::new(0) });

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {{
        CHECKS.with(|c| c.set(c.get() + 1));
        if !$cond {
            panic!("FAIL {}: {}", stringify!($cond), format!($($msg)+));
        }
    }};
}

/// What the firmware puts on the wire for one line: `println!("{}", Show(…))` — the codec's
/// bytes through the same adapter, then esp-println's `\n`.
fn wire<F: Fn(&mut std::fmt::Formatter<'_>) -> std::fmt::Result>(f: F) -> String {
    format!("{}\n", Show(f))
}

fn mac_str(m: &[u8; 6]) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 13) as u8).collect()
    }
}

fn gateway_to_arena(rng: &mut Lcg) {
    // RX: every rssi, both mac_ok, every frame length the gateway can forward (0..=250).
    for len in 0..=250usize {
        let frame = rng.bytes(len);
        let src = rng.next() as u8;
        let rssi = (len as i32 % 256 - 128) as i8;
        let mac_ok = len % 2 == 0;
        let l = wire(|w| write_rx(w, src, rssi, mac_ok, &frame));
        let got = tl::parse(&l);
        check!(
            got == GwLine::Rx { src, rssi, mac_ok, bytes: frame.clone() },
            "RX len {len}: {l:?} -> {got:?}"
        );
    }
    for rssi in i8::MIN..=i8::MAX {
        let l = wire(|w| write_rx(w, 163, rssi, true, b"SMOLv1 MATCH \x01T"));
        check!(matches!(tl::parse(&l), GwLine::Rx { rssi: r, .. } if r == rssi), "rssi {rssi}");
    }

    // HELLO: the MAC as the arena's D8 discovery compares it (eq_ignore_ascii_case), the node id,
    // the fw token, and the epoch as a decimal u8 (the arena reads `epoch: u8`).
    for (i, fw_src) in ["1a2b3c4d5e6f", "abc1234-dirty", "nogit", "", "0123456789abcdef"].iter().enumerate() {
        let mac: [u8; 6] = rng.bytes(6).try_into().unwrap();
        let node = [0u8, 8, 51, 163, 255][i];
        let fw = fw_hash8(fw_src);
        for epoch in [1u8, 0x1f, 0x81, 0xff] {
            let l = wire(|w| write_hello(w, &mac, node, fw, epoch));
            let got = tl::parse(&l);
            check!(
                got == GwLine::Hello { mac: mac_str(&mac), node, fw: fw.to_string(), epoch },
                "HELLO {l:?} -> {got:?}"
            );
        }
    }

    // TXOK / TXERR, every reason.
    for id in [0u32, 1, 7, 65_536, u32::MAX] {
        let l = wire(|w| write_txok(w, id));
        check!(tl::parse(&l) == GwLine::TxOk(id), "{l:?}");
        for r in [
            Reason::BadDst, Reason::BadHex, Reason::TooLong, Reason::NotMatch, Reason::Short,
            Reason::ExtraField, Reason::UnknownDst, Reason::SelfDst, Reason::SendFailed, Reason::NoRadio,
        ] {
            let l = wire(|w| write_txerr(w, id, r));
            check!(tl::parse(&l) == GwLine::TxErr(id, r.as_str().to_string()), "{l:?}");
        }
    }

    // ROSTER, 1..=16 entries (the roster holds 16).
    for n in 1..=16usize {
        let entries: Vec<(u8, [u8; 6], i8)> = (0..n)
            .map(|_| (rng.next() as u8, rng.bytes(6).try_into().unwrap(), (rng.next() % 256) as u8 as i8))
            .collect();
        let l = wire(|w| write_roster(w, &entries).map(|_| ()));
        let want: Vec<(u8, String, i8)> = entries.iter().map(|(i, m, r)| (*i, mac_str(m), *r)).collect();
        check!(tl::parse(&l) == GwLine::Roster(want), "ROSTER {n}: {l:?}");
    }
    // Why an empty roster is skipped rather than sent: the arena cannot read one.
    for l in ["@TS1 ROSTER\n", "@TS1 ROSTER \n"] {
        check!(matches!(tl::parse(l), GwLine::Log(_)), "empty ROSTER {l:?} is a log line to the arena");
    }

    // smol's own log lines (esp-println, `colors` feature) stay log lines to the arena.
    for l in [
        "\x1b[32mINFO - smol: relay role = leaf\x1b[0m\n",
        "INFO - smol #548: tapstone gateway up\n",
        "ESP-ROM:esp32c3-api1-20210207\n",
    ] {
        check!(matches!(tl::parse(l), GwLine::Log(_)), "log {l:?}");
    }
}

fn arena_to_gateway(rng: &mut Lcg) {
    let mut buf = [0u8; TX_FRAME_MAX];
    check!(parse_inbound(tl::ping_line().as_bytes(), false, &mut buf) == Inbound::Ping, "ping_line");
    for len in MATCH_HEADER_LEN..=TX_FRAME_MAX {
        let mut frame = MATCH_TAG.to_vec();
        frame.extend(rng.bytes(len - MATCH_TAG.len()));
        let id = rng.next();
        let dst = if len % 3 == 0 { 255 } else { rng.next() as u8 };
        let l = tl::tx_line(id, dst, &frame);
        let got = parse_inbound(l.as_bytes(), false, &mut buf);
        check!(got == Inbound::Tx { id, dst, len } && buf[..len] == frame[..], "tx_line len {len}: {got:?}");
    }
    // Frames the gateway refuses still come back as a TXERR the arena can read, naming its id.
    for (frame, reason) in [
        (b"\x01\x02".to_vec(), Reason::NotMatch),
        (b"SMOLv1 MATCH \x01".to_vec(), Reason::Short),
        ({ let mut f = MATCH_TAG.to_vec(); f.resize(TX_FRAME_MAX + 1, 0); f }, Reason::TooLong),
        (Vec::new(), Reason::BadHex),
    ] {
        let l = tl::tx_line(99, 255, &frame);
        let got = parse_inbound(l.as_bytes(), false, &mut buf);
        check!(got == Inbound::TxRefused { id: 99, reason }, "{l:?} -> {got:?}");
        let back = wire(|w| write_txerr(w, 99, reason));
        check!(tl::parse(&back) == GwLine::TxErr(99, reason.as_str().to_string()), "{back:?}");
    }
}

fn main() {
    println!(
        "tapstone lines.rs: {} (fnv1a {})",
        env!("TAPSTONE_LINES_PATH"),
        env!("TAPSTONE_LINES_FNV")
    );
    let mut rng = Lcg(0x5eed_0548);
    gateway_to_arena(&mut rng);
    arena_to_gateway(&mut rng);
    let n = CHECKS.with(|c| c.get());
    assert!(n >= 700, "only {n} checks ran");
    println!("tapstone_lines_xcheck: {n} checks passed");
}
