//! The scry client — `POST /tap` then a STREAMING `/screen` blit.
//!
//! Server: scry-glass on ubox0 (labels/scry/scry-glass.py, API v3). Contract:
//!
//!   POST /tap/<UID>?k=<tok>     -> {"host":"gatekeeper"} | {"host":null,...}
//!   GET  /screen/<host>?k=<tok> -> 153,600 B raw rgb565 BIG-ENDIAN, row-major,
//!                                  origin top-left, 320x240 — display-ready
//!   GET  /screen-unbound/<UID>?k=<tok> -> same, "unbound sigil" frame
//!
//! ⚠️ DOTTED QUAD ONLY. There is no DNS resolver in this firmware, by rule
//! (same rule the watch tree's ota_http follows). SCRY_HOST is an IP.
//!
//! ⚠️ VLAN: ubox0 has legs on VLAN6 (10.0.6.11) and VLAN11 only — NO VLAN8 —
//! and gatekeeper's `iot` zone forwards nothing to admin. **This station must
//! join VLAN6.** A station on the iot SSID gets a TCP connect that never
//! completes, which reads exactly like a dead server. Measured 2026-09-01.
//!
//! THE BLIT IS STREAMED, never buffered: bytes land in a small strip buffer and
//! go straight out as `fill_contiguous` windows. Buffering the whole frame would
//! cost 150 KiB and add a copy for nothing — the panel takes rows as they
//! arrive. (Strip height is the tuning knob; see `STRIP_ROWS`.)

use embedded_graphics::{
    pixelcolor::{raw::RawU16, Rgb565},
    prelude::*,
    primitives::Rectangle,
};
use esp_hal::{delay::Delay, time::Instant as HalInstant, time::Duration as HalDuration};
use esp_println::println;
use smoltcp::{
    iface::{Interface as SmolIface, SocketHandle, SocketSet},
    socket::tcp,
    wire::IpAddress,
};

use crate::radio_dev::SmolWifiDevice;

/// scry-glass, VLAN6 leg. Baked at build time; the default is the measured one.
const SCRY_IP: (u8, u8, u8, u8) = match option_env!("SCRY_HOST") {
    Some(_) => (10, 0, 6, 11), // parsed at build time is overkill; see below
    None => (10, 0, 6, 11),
};
const SCRY_PORT: u16 = 7787;
/// HMAC(secret,"station-163")[:12] — baked via env, never a file, never in-repo.
const SCRY_TOKEN: Option<&str> = option_env!("SCRY_TOKEN");

const W: u32 = 320;
const H: u32 = 240;
/// Rows per blit window. 8 rows = 5,120 B in internal RAM; big enough that the
/// per-window SPI overhead disappears, small enough to never touch PSRAM.
const STRIP_ROWS: u32 = 8;
const STRIP_BYTES: usize = (W * STRIP_ROWS * 2) as usize;

const LOCAL_PORT: u16 = 51_163;
const CONNECT_MS: u64 = 4_000;
const BODY_MS: u64 = 20_000;

fn now_ms() -> u64 {
    HalInstant::now().duration_since_epoch().as_millis()
}

/// Open TCP, send `req`, skip headers, hand the body stream to `on_body`.
/// Returns false (having logged why) on any failure.
fn http<F>(
    iface: &mut SmolIface,
    device: &mut SmolWifiDevice,
    sockets: &mut SocketSet<'static>,
    sock_h: SocketHandle,
    delay: &Delay,
    req: &[u8],
    mut on_body: F,
) -> bool
where
    F: FnMut(&[u8]) -> bool, // false = stop reading
{
    {
        let s = sockets.get_mut::<tcp::Socket>(sock_h);
        if s.is_open() {
            s.abort();
        }
        iface.poll(smoltcp::time::Instant::from_millis(now_ms() as i64), device, sockets);
        let s = sockets.get_mut::<tcp::Socket>(sock_h);
        let addr = IpAddress::v4(SCRY_IP.0, SCRY_IP.1, SCRY_IP.2, SCRY_IP.3);
        if s.connect(iface.context(), (addr, SCRY_PORT), LOCAL_PORT).is_err() {
            println!("[scry] tcp connect rejected locally");
            return false;
        }
    }

    let deadline = HalInstant::now() + HalDuration::from_millis(CONNECT_MS);
    loop {
        iface.poll(smoltcp::time::Instant::from_millis(now_ms() as i64), device, sockets);
        if sockets.get_mut::<tcp::Socket>(sock_h).may_send() {
            break;
        }
        if HalInstant::now() >= deadline {
            println!(
                "[scry] ⚠️ TCP to {}.{}.{}.{}:{} did not open in {} ms.",
                SCRY_IP.0, SCRY_IP.1, SCRY_IP.2, SCRY_IP.3, SCRY_PORT, CONNECT_MS
            );
            println!("[scry]    If the lease is 10.0.8.x you are on the IoT VLAN, which");
            println!("[scry]    cannot reach admin — the station belongs on VLAN6.");
            return false;
        }
        delay.delay_millis(5);
    }

    if sockets.get_mut::<tcp::Socket>(sock_h).send_slice(req).is_err() {
        println!("[scry] could not send request");
        return false;
    }

    // ---- read: skip headers, stream the body ----
    let mut hdr_done = false;
    let mut tail = [0u8; 3]; // rolling CRLFCRLF detector state
    let mut tail_n = 0usize;
    let mut chunk = [0u8; 1460];
    let body_deadline = HalInstant::now() + HalDuration::from_millis(BODY_MS);
    loop {
        iface.poll(smoltcp::time::Instant::from_millis(now_ms() as i64), device, sockets);
        let s = sockets.get_mut::<tcp::Socket>(sock_h);
        if s.can_recv() {
            let n = s.recv_slice(&mut chunk).unwrap_or(0);
            if n == 0 {
                continue;
            }
            let mut start = 0usize;
            if !hdr_done {
                // find \r\n\r\n across chunk boundaries
                for i in 0..n {
                    let b = chunk[i];
                    let expect = match tail_n {
                        0 | 2 => b'\r',
                        _ => b'\n',
                    };
                    if b == expect {
                        tail[tail_n.min(2)] = b;
                        tail_n += 1;
                        if tail_n == 4 {
                            hdr_done = true;
                            start = i + 1;
                            break;
                        }
                    } else {
                        tail_n = if b == b'\r' { 1 } else { 0 };
                    }
                }
                if !hdr_done {
                    continue;
                }
            }
            if start < n && !on_body(&chunk[start..n]) {
                return true; // consumer says done
            }
        } else if !s.may_recv() {
            return hdr_done;
        } else {
            // ⚠️ YIELD. Spinning on iface.poll() starves the esp-rtos WiFi task
            // and the socket never refills — measured 2026-09-01 as 32 of 240
            // rows in 8 s, which reads like a slow server and is not. Same
            // lesson as net.tick's "do not sleep through the window", inverted:
            // here the loop must GIVE time back, not take it.
            delay.delay_millis(2);
        }
        if HalInstant::now() >= body_deadline {
            println!("[scry] body timeout");
            return false;
        }
    }
}

/// `POST /tap/<uid>` — returns the bound host (into `out`), or None if unbound.
pub fn tap<'a>(
    iface: &mut SmolIface,
    device: &mut SmolWifiDevice,
    sockets: &mut SocketSet<'static>,
    sock_h: SocketHandle,
    delay: &Delay,
    uid_hex: &str,
    out: &'a mut [u8; 48],
) -> Option<&'a str> {
    let Some(tok) = SCRY_TOKEN else {
        println!("[scry] no SCRY_TOKEN baked — rebuild with the station token");
        return None;
    };
    let mut req = [0u8; 256];
    let n = fmt(
        &mut req,
        &[
            b"POST /tap/",
            uid_hex.as_bytes(),
            b"?k=",
            tok.as_bytes(),
            b" HTTP/1.1\r\nHost: scry\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        ],
    );

    // Body is tiny JSON: {"host":"gatekeeper"} or {"host":null,...}
    let mut body = [0u8; 128];
    let mut bn = 0usize;
    let ok = http(iface, device, sockets, sock_h, delay, &req[..n], |b| {
        let take = b.len().min(body.len() - bn);
        body[..].get_mut(bn..bn + take).map(|d| d.copy_from_slice(&b[..take]));
        bn += take;
        bn < body.len()
    });
    if !ok {
        return None;
    }
    let text = core::str::from_utf8(&body[..bn]).ok()?;
    let key = "\"host\": \"";
    let i = text.find(key).or_else(|| text.find("\"host\":\""))?;
    let rest = &text[i + key.len()..];
    let end = rest.find('"')?;
    let host = &rest[..end];
    let len = host.len().min(out.len());
    out[..len].copy_from_slice(&host.as_bytes()[..len]);
    core::str::from_utf8(&out[..len]).ok()
}

/// `GET /screen/<host>` (or the unbound frame) streamed straight to the panel.
/// Returns the elapsed milliseconds on success — the number the Slint app wants.
pub fn paint<D>(
    iface: &mut SmolIface,
    device: &mut SmolWifiDevice,
    sockets: &mut SocketSet<'static>,
    sock_h: SocketHandle,
    delay: &Delay,
    display: &mut D,
    path_host: &str,
    unbound: bool,
) -> Option<u64>
where
    D: DrawTarget<Color = Rgb565>,
{
    let tok = SCRY_TOKEN?;
    let mut req = [0u8; 256];
    let n = fmt(
        &mut req,
        &[
            b"GET /",
            if unbound { b"screen-unbound/" as &[u8] } else { b"screen/" as &[u8] },
            path_host.as_bytes(),
            b"?k=",
            tok.as_bytes(),
            b" HTTP/1.1\r\nHost: scry\r\nConnection: close\r\n\r\n",
        ],
    );

    let t0 = now_ms();
    let mut strip = [0u8; STRIP_BYTES];
    let mut have = 0usize;
    let mut row: u32 = 0;
    let mut pixels_ok = true;

    let ok = http(iface, device, sockets, sock_h, delay, &req[..n], |b| {
        let mut src = b;
        while !src.is_empty() && row < H {
            let want = STRIP_BYTES - have;
            let take = want.min(src.len());
            strip[have..have + take].copy_from_slice(&src[..take]);
            have += take;
            src = &src[take..];
            if have == STRIP_BYTES {
                let rows = STRIP_ROWS.min(H - row);
                let area = Rectangle::new(Point::new(0, row as i32), Size::new(W, rows));
                let px = (0..(W * rows) as usize).map(|i| {
                    let hi = strip[i * 2] as u16;
                    let lo = strip[i * 2 + 1] as u16;
                    Rgb565::from(RawU16::new((hi << 8) | lo))
                });
                if display.fill_contiguous(&area, px).is_err() {
                    pixels_ok = false;
                    return false;
                }
                row += rows;
                have = 0;
            }
        }
        row < H
    });

    if !ok || !pixels_ok || row < H {
        println!("[scry] frame incomplete ({} of {} rows)", row, H);
        return None;
    }
    Some(now_ms().saturating_sub(t0))
}

/// Tiny no-alloc concat into a byte buffer; returns bytes written.
fn fmt(out: &mut [u8], parts: &[&[u8]]) -> usize {
    let mut n = 0;
    for p in parts {
        let take = p.len().min(out.len() - n);
        out[n..n + take].copy_from_slice(&p[..take]);
        n += take;
    }
    n
}
