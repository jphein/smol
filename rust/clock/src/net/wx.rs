//! #227 weather-on-glass — the PURE weather codec: the no-serde Open-Meteo JSON scrape, the
//! `WX|<tempF>|<code>` mesh-payload codec, and the WMO-code → condition-label table.
//!
//! Ported from the esp32c6-watch reference (`weather.rs`, JP's project — the read-only
//! implementation #227 adopts): find the `"temperature_2m":` / `"weather_code":` keys whose
//! value is NUMERIC (the `current_units` object repeats the same keys with string values —
//! those occurrences are skipped), parse the digits, round on the first fractional digit.
//! No serde, no alloc, total on hostile input.
//!
//! Pure + host-testable (the `flood`/`etx`/`wire` pattern): no HAL deps, `&[u8]` in/out.
//! Host-tested in `experiments/wx_verify` (a real-shape Open-Meteo body + unit-string skip +
//! payload round-trip + malformed rejection). The HTTP driver lives in `net/wifi.rs`
//! (`fetch_weather`); the mesh relay + screen consume [`encode_wx`]/[`parse_wx`].

/// Max `WX|…` mesh payload (bytes) — `WX|-999|99` is 10; 32 leaves room for future fields
/// (parsers read the leading fields and ignore extras — the additive/#100 discipline).
pub const WX_PAYLOAD_MAX: usize = 32;

/// Scrape an Open-Meteo `current=temperature_2m,weather_code` response (headers + JSON body —
/// the keys never appear in headers, so scanning the whole buffer is safe) into
/// `(temp_f, wmo_code)`. `None` if either key has no numeric value.
pub fn parse_open_meteo(resp: &[u8]) -> Option<(i16, u8)> {
    let temp = find_number(resp, b"\"temperature_2m\":")?;
    let code = find_number(resp, b"\"weather_code\":")?;
    Some((temp.clamp(-999, 999) as i16, code.clamp(0, 255) as u8))
}

/// Find `key` followed by a NUMERIC value and parse it (rounded). Occurrences where the value
/// is not numeric (the `current_units` strings, e.g. `"temperature_2m":"°F"`) are skipped.
fn find_number(hay: &[u8], key: &[u8]) -> Option<i32> {
    let mut start = 0;
    while let Some(pos) = find(&hay[start..], key) {
        let val = &hay[start + pos + key.len()..];
        if let Some(n) = parse_rounded(val) {
            return Some(n);
        }
        start += pos + key.len();
    }
    None
}

/// Parse `-?digits(.digits)?`, rounding on the first fractional digit. `None` if the slice
/// doesn't start with a number (e.g. the `"` of a unit string).
fn parse_rounded(s: &[u8]) -> Option<i32> {
    let mut i = 0;
    let neg = *s.first()? == b'-';
    if neg {
        i = 1;
    }
    let mut int: i32 = 0;
    let mut digits = 0;
    while let Some(&c) = s.get(i) {
        if !c.is_ascii_digit() {
            break;
        }
        int = int.saturating_mul(10).saturating_add((c - b'0') as i32);
        digits += 1;
        i += 1;
    }
    if digits == 0 {
        return None;
    }
    let mut round_up = false;
    if s.get(i) == Some(&b'.') {
        if let Some(&frac) = s.get(i + 1) {
            round_up = frac.is_ascii_digit() && frac >= b'5';
        }
    }
    if round_up {
        int += 1;
    }
    Some(if neg { -int } else { int })
}

/// Naive substring search (the haystack is ≤ ~1.5 KB, run once per fetch).
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Encode the mesh payload `WX|<tempF>|<code>` into `out`; returns the length (≤ 10 today).
/// ASCII, sign-prefixed temp, no padding — the same human-greppable style as `BATT|`/`GRID|`.
pub fn encode_wx(temp_f: i16, code: u8, out: &mut [u8]) -> usize {
    let mut n = 0;
    out[..3].copy_from_slice(b"WX|");
    n += 3;
    let mut t = temp_f;
    if t < 0 {
        out[n] = b'-';
        n += 1;
        t = -t;
    }
    n += write_dec(t as u32, &mut out[n..]);
    out[n] = b'|';
    n += 1;
    n += write_dec(code as u32, &mut out[n..]);
    n
}

/// Parse a `WX|<tempF>|<code>` payload (leading fields; extra future fields ignored).
pub fn parse_wx(payload: &[u8]) -> Option<(i16, u8)> {
    let rest = payload.strip_prefix(b"WX|")?;
    let sep = rest.iter().position(|&b| b == b'|')?;
    let temp = parse_int(&rest[..sep])?;
    let code_field = &rest[sep + 1..];
    let code_end = code_field
        .iter()
        .position(|&b| b == b'|')
        .unwrap_or(code_field.len());
    let code = parse_int(&code_field[..code_end])?;
    if !(-999..=999).contains(&temp) || !(0..=255).contains(&code) {
        return None;
    }
    Some((temp as i16, code as u8))
}

/// Minimal unpadded decimal writer; returns digits written. `v ≤ 999` here so ≤ 3 digits.
fn write_dec(mut v: u32, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 10];
    let mut i = 0;
    loop {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
        if v == 0 {
            break;
        }
    }
    for k in 0..i {
        out[k] = tmp[i - 1 - k];
    }
    i
}

/// Parse a small signed ASCII integer (no leading/trailing junk). Total.
fn parse_int(s: &[u8]) -> Option<i32> {
    if s.is_empty() || s.len() > 5 {
        return None;
    }
    let (neg, digits) = match s[0] {
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: i32 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as i32)?;
    }
    Some(if neg { -v } else { v })
}

/// WMO weather-interpretation code → a short condition label (≤ 12 chars — FONT_5X8 fits 14
/// on the 72-px glass). The standard Open-Meteo WMO buckets.
pub fn wmo_label(code: u8) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=57 => "Drizzle",
        61..=65 => "Rain",
        66 | 67 => "Icy rain",
        71..=77 => "Snow",
        80..=82 => "Showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Storm + hail",
        _ => "Weather",
    }
}
