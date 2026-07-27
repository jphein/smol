//! #227 host verification of the PURE weather codec. `#[path]`-includes the REAL `net/wx.rs`
//! (no drift). Run: `cargo run` — panics on any failure.

#[path = "../../../rust/clock/src/net/wx.rs"]
mod wx;

use wx::{encode_wx, parse_open_meteo, parse_wx, wmo_label, WX_PAYLOAD_MAX};

fn main() {
    // --- Open-Meteo scrape: real response shape, incl. the current_units STRING repeat ------
    // The `current_units` object repeats both keys with string values — the scrape must skip
    // them and land on the numeric values in `current`. This is the exact trap the watch's
    // parser was built around.
    let body: &[u8] = r#"HTTP/1.1 200 OK
Content-Type: application/json

{"latitude":47.6,"longitude":-122.3,"generationtime_ms":0.05,"utc_offset_seconds":0,
"current_units":{"time":"iso8601","interval":"seconds","temperature_2m":"°F","weather_code":"wmo code"},
"current":{"time":"2026-07-20T05:00","interval":900,"temperature_2m":72.6,"weather_code":3}}"#
        .as_bytes();
    let (t, c) = parse_open_meteo(body).expect("scrape");
    assert_eq!(t, 73, "72.6F rounds to 73");
    assert_eq!(c, 3, "WMO code 3");

    // negative temp + rounding down + fractional code position
    let cold: &[u8] = r#"{"current_units":{"temperature_2m":"°F","weather_code":"wmo"},
"current":{"temperature_2m":-8.4,"weather_code":71}}"#
        .as_bytes();
    assert_eq!(parse_open_meteo(cold), Some((-8, 71)), "-8.4F rounds to -8, snow code");

    // missing keys / non-numeric only → None (fetch fails soft)
    assert_eq!(parse_open_meteo(br#"{"current_units":{"temperature_2m":"F"}}"#), None);
    assert_eq!(parse_open_meteo(b"HTTP/1.0 500 nope"), None);
    assert_eq!(parse_open_meteo(b""), None);

    // --- WX| payload codec round-trip --------------------------------------------------------
    let mut buf = [0u8; WX_PAYLOAD_MAX];
    let n = encode_wx(73, 3, &mut buf);
    assert_eq!(&buf[..n], b"WX|73|3", "golden payload bytes");
    assert_eq!(parse_wx(&buf[..n]), Some((73, 3)), "round-trip");

    let n = encode_wx(-8, 71, &mut buf);
    assert_eq!(&buf[..n], b"WX|-8|71", "negative temp payload");
    assert_eq!(parse_wx(&buf[..n]), Some((-8, 71)), "negative round-trip");

    // extremes + width bound
    let n = encode_wx(-999, 255, &mut buf);
    assert_eq!(&buf[..n], b"WX|-999|255");
    assert!(n <= WX_PAYLOAD_MAX, "worst case fits WX_PAYLOAD_MAX");
    assert_eq!(parse_wx(&buf[..n]), Some((-999, 255)));

    // forward-compat: extra future fields are ignored (the #100 additive rule)
    assert_eq!(parse_wx(b"WX|73|3|55|hi"), Some((73, 3)), "extra fields ignored");

    // rejection: wrong tag, garbage, out-of-range, empty
    assert_eq!(parse_wx(b"BATT|48V"), None);
    assert_eq!(parse_wx(b"WX|"), None);
    assert_eq!(parse_wx(b"WX|abc|3"), None);
    assert_eq!(parse_wx(b"WX|1000|3"), None, "temp out of range");
    assert_eq!(parse_wx(b"WX|70|300"), None, "code out of range");
    assert_eq!(parse_wx(b""), None);

    // --- WMO label table: total + short enough for the 72px glass (14 chars of FONT_5X8) ----
    for code in 0..=255u16 {
        let l = wmo_label(code as u8);
        assert!(!l.is_empty() && l.len() <= 13, "label for {code} fits the glass: {l}");
    }
    assert_eq!(wmo_label(0), "Clear");
    assert_eq!(wmo_label(95), "Thunderstorm");
    assert_eq!(wmo_label(42), "Weather", "unknown codes get the safe default");

    println!("wx_verify: ALL CHECKS PASSED (scrape + units-skip + payload round-trip + WMO table)");
}
