//! Phase 2 — WiFi STA bring-up + SNTP time sync (blocking, no async runtime).
//!
//! Also hosts the shared radio init used by Phase 3's ESP-NOW switching
//! (`crate::net::mode`).
//!
//! ## Single-radio reality (READ THIS)
//!
//! The ESP32-C3 has ONE 2.4 GHz radio tuned to ONE channel at a time. WiFi
//! (STA) and ESP-NOW share that PHY:
//!   * Associated to an AP -> radio sits on the AP's channel; ESP-NOW works
//!     only on that same channel (all peers must match it).
//!   * Want ESP-NOW on a fixed known channel -> drop the WiFi association and
//!     pin the channel yourself (time-sharing).
//!
//! `crate::net::mode::RadioManager` (Phase 3) makes this trade-off explicit.
//! Phase 2 uses only the WiFi-burst path: connect, DHCP, SNTP, done.
//!
//! We deliberately avoid an async executor and the git-only
//! `blocking-network-stack` crate; instead we drive `smoltcp` directly with a
//! tiny blocking poll loop, which keeps the dependency set on crates.io.

// In the `wifi`-only build every item below is live. When `espnow` is also on,
// `main` drives the radio through `net::mode` instead, so this module's SNTP
// path is present-but-unused; suppress the resulting dead-code noise there.
#![cfg_attr(feature = "espnow", allow(dead_code))]

extern crate alloc;

use core::net::Ipv4Addr;

use esp_hal::{
    peripherals::{RNG, TIMG0, WIFI},
    rng::Rng,
    time::{Duration, Instant},
    timer::timg::TimerGroup,
};
use esp_wifi::{
    wifi::{ClientConfiguration, Configuration},
    EspWifiController,
};
use smoltcp::{
    iface::{Interface, SocketSet, SocketStorage},
    socket::{dhcpv4, udp},
    wire::{DhcpOption, EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Cidr},
};

// -------------------------------------------------------------------------
// Configuration (compile-time placeholders — set before flashing).
// -------------------------------------------------------------------------

// Real values live in the git-ignored `crate::secrets` (repo is public).
use crate::secrets::{WIFI_PASS as WIFI_PASSWORD, WIFI_SSID};

/// NTP server IPv4. We hardcode an anycast IP so we need no DNS resolver in
/// the smoltcp build. time.cloudflare.com's NTP anycast address:
const NTP_SERVER_IP: Ipv4Addr = Ipv4Addr::new(162, 159, 200, 123);
const NTP_PORT: u16 = 123;

/// Relay uplink collector — the homelab receiver on **disks** (`10.0.11.117:9999`,
/// a linger-enabled user systemd unit logging telemetry as JSONL). Hardcoded IPv4
/// (mirrors `NTP_SERVER_IP`) so no DNS resolver is needed. It sits on the SAME
/// VLAN-11 /24 the boards DHCP onto, so the relay path is same-subnet L2 — no
/// gatekeeper hop. UDP is fire-and-forget: `send` "succeeds" locally regardless of
/// the listener, so a genuine outage is an ASSOCIATION failure — which
/// `run_udp_flush` now bounds via `RELAY_FLUSH_BUDGET` (finding-1 fix). Only the
/// `espnow` relay flush references these (see `run_udp_flush`).
#[cfg(feature = "espnow")]
const RELAY_COLLECTOR_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 11, 117);
#[cfg(feature = "espnow")]
const RELAY_COLLECTOR_PORT: u16 = 9999;
/// Max relay-flush datagram = "NNN " (4) + up to `RELAY_MAX_MSG` telemetry bytes.
#[cfg(feature = "espnow")]
const FLUSH_DG_MAX: usize = 320;

/// Seconds between the NTP epoch (1900-01-01) and the Unix epoch (1970-01-01).
const NTP_TO_UNIX_OFFSET: u32 = 2_208_988_800;

/// Overall budget for the WiFi+SNTP burst. If we don't have the time by then,
/// give up and let the clock free-run from its compile-time constant.
const SYNC_BUDGET: Duration = Duration::from_secs(30);

/// Overall budget for a RELAY flush burst (associate + DHCP + UDP sends + drain),
/// MUCH shorter than the NTP burst's 30 s so a gateway can't block the whole
/// firmware loop for 30 s when the AP is down (finding 1b).
///
/// HARDWARE-TUNED 2026-07-07: 6 s was NOT enough on the real AP — wave-3 flashes
/// showed both gateways failing with "relay flush — DHCP timed out" (associate
/// succeeded; the FRESH DHCP exchange overran the remaining budget), 0/N flushes,
/// exactly as the pass-3 review's N2 note predicted. 15 s gives the observed
/// associate+DHCP ~2.5× headroom while keeping the outage freeze bounded and far
/// below the old 30 s spin. Tradeoff unchanged: longer budget = longer worst-case
/// display/input freeze per attempt during an outage.
#[cfg(feature = "espnow")]
const RELAY_FLUSH_BUDGET: Duration = Duration::from_secs(15);

// -------------------------------------------------------------------------
// Peripheral bundle handed over from `main` (single esp_hal::init()).
// -------------------------------------------------------------------------

pub struct WifiPeripherals {
    pub timg0: TIMG0<'static>,
    pub rng: RNG<'static>,
    pub wifi: WIFI<'static>,
}

/// smoltcp wants a monotonic timestamp; derive it from the HAL's clock.
fn smoltcp_now() -> smoltcp::time::Instant {
    smoltcp::time::Instant::from_micros(
        Instant::now().duration_since_epoch().as_micros() as i64
    )
}

/// Build a smoltcp `Interface` bound to the WiFi STA device (verbatim from the
/// esp-wifi `wifi_dhcp` example's `create_interface`).
fn create_interface(device: &mut esp_wifi::wifi::WifiDevice) -> Interface {
    Interface::new(
        smoltcp::iface::Config::new(HardwareAddress::Ethernet(EthernetAddress::from_bytes(
            &device.mac_address(),
        ))),
        device,
        smoltcp_now(),
    )
}

/// Phase 2 entry point: bring WiFi up, DHCP, run one SNTP exchange, return the
/// current Unix time in seconds. Returns `None` on any failure/timeout so the
/// caller falls back to the free-running clock.
pub fn try_time_sync(p: WifiPeripherals) -> Option<u32> {
    super::init_heap();

    // --- Radio init ------------------------------------------------------
    let timg0 = TimerGroup::new(p.timg0);
    // `Rng` is a `Copy` handle; keep our own copy for the SNTP port seed.
    let rng = Rng::new(p.rng);
    let esp_wifi_ctrl: EspWifiController<'static> =
        esp_wifi::init(timg0.timer0, rng).ok()?;
    // Leak the controller so its borrow lives 'static for the rest of the
    // burst; the device is dropped when we return, which stops WiFi cleanly.
    let esp_wifi_ctrl: &'static EspWifiController<'static> =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(esp_wifi_ctrl));

    let (mut controller, interfaces) = esp_wifi::wifi::new(esp_wifi_ctrl, p.wifi).ok()?;
    let mut device = interfaces.sta;

    // Phase-2 (wifi-only) build has no status LED, so the tick is a no-op.
    run_ntp_burst(&mut controller, &mut device, rng, &mut || {})
}

/// Shared WiFi -> DHCP -> SNTP burst, reused by both the Phase-2 `wifi`-only
/// build and the Phase-3 `espnow` build. Associates using the `crate::secrets`
/// credentials, drives a `smoltcp` DHCP+UDP stack over `device`, runs one SNTP
/// exchange, and returns the Unix time (seconds) or `None` on any timeout.
///
/// `tick` is invoked frequently inside every busy-wait loop; the `espnow` build
/// passes a closure that fast-blinks the blue LED so "WiFi/NTP in progress" is
/// visible on hardware. The `wifi`-only build passes a no-op.
///
/// Blocking, no async executor — we poll the stack directly, matching the rest
/// of the firmware's style and keeping the dependency set on crates.io.
pub fn run_ntp_burst(
    controller: &mut esp_wifi::wifi::WifiController<'static>,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    mut rng: Rng,
    tick: &mut dyn FnMut(),
) -> Option<u32> {
    // --- smoltcp stack: DHCP + UDP sockets -------------------------------
    let mut iface = create_interface(device);

    let mut sockets_storage: [SocketStorage; 3] = Default::default();
    let mut sockets = SocketSet::new(&mut sockets_storage[..]);

    let mut dhcp_socket = dhcpv4::Socket::new();
    dhcp_socket.set_outgoing_options(&[DhcpOption {
        kind: 12, // hostname
        data: b"smol",
    }]);
    let dhcp_handle = sockets.add(dhcp_socket);

    // UDP socket for SNTP.
    let mut udp_rx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut udp_rx_data = [0u8; 512];
    let mut udp_tx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut udp_tx_data = [0u8; 512];
    let udp_socket = udp::Socket::new(
        udp::PacketBuffer::new(&mut udp_rx_meta[..], &mut udp_rx_data[..]),
        udp::PacketBuffer::new(&mut udp_tx_meta[..], &mut udp_tx_data[..]),
    );
    let udp_handle = sockets.add(udp_socket);

    // --- WiFi connect ----------------------------------------------------
    controller
        .set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: WIFI_SSID.into(),
            password: WIFI_PASSWORD.into(),
            ..Default::default()
        }))
        .ok()?;
    if !matches!(controller.is_started(), Ok(true)) {
        controller.start().ok()?;
    }
    controller.connect().ok()?;

    let deadline = Instant::now() + SYNC_BUDGET;

    // Wait for association.
    while !matches!(controller.is_connected(), Ok(true)) {
        tick();
        if Instant::now() > deadline {
            log::warn!("smol: WiFi connect timed out");
            return None;
        }
    }
    log::info!("smol: WiFi associated to '{}'", WIFI_SSID);

    // Poll the stack until DHCP yields an address. The DHCP `Event` borrows
    // the socket, so we extract the plain (Ipv4Cidr, router) data inside a
    // short scope, then apply it to the interface once the borrow is released.
    loop {
        tick();
        let ts = smoltcp_now();
        iface.poll(ts, device, &mut sockets);

        let configured = {
            let socket = sockets.get_mut::<dhcpv4::Socket>(dhcp_handle);
            match socket.poll() {
                Some(dhcpv4::Event::Configured(cfg)) => Some((cfg.address, cfg.router)),
                _ => None,
            }
        };

        if let Some((addr, router)) = configured {
            apply_dhcp(&mut iface, addr, router);
            log::info!("smol: DHCP address {}", addr);
            break;
        }
        if Instant::now() > deadline {
            log::warn!("smol: DHCP timed out");
            return None;
        }
    }

    // --- SNTP exchange ---------------------------------------------------
    sntp_query(
        &mut iface,
        device,
        &mut sockets,
        udp_handle,
        rng.random(),
        deadline,
        tick,
    )
}

/// Install the DHCP-provided address + default route on the interface.
fn apply_dhcp(iface: &mut Interface, addr: Ipv4Cidr, router: Option<Ipv4Addr>) {
    iface.update_ip_addrs(|addrs| {
        addrs.clear();
        let _ = addrs.push(IpCidr::Ipv4(addr));
    });
    if let Some(router) = router {
        let _ = iface.routes_mut().add_default_ipv4_route(router);
    }
}

/// Send one SNTP request and parse the reply into a Unix timestamp (seconds).
fn sntp_query(
    iface: &mut Interface,
    device: &mut esp_wifi::wifi::WifiDevice,
    sockets: &mut SocketSet,
    udp_handle: smoltcp::iface::SocketHandle,
    ephemeral_port_seed: u32,
    deadline: Instant,
    tick: &mut dyn FnMut(),
) -> Option<u32> {
    // Bind a pseudo-random ephemeral source port (49152..=65535).
    let src_port = 49152 + (ephemeral_port_seed % 16384) as u16;

    // SNTP/NTPv4 client request: LI=0, VN=4, Mode=3 -> first byte 0x23.
    let mut request = [0u8; 48];
    request[0] = 0x23;

    let server = (IpAddress::Ipv4(NTP_SERVER_IP), NTP_PORT);

    {
        let socket = sockets.get_mut::<udp::Socket>(udp_handle);
        if socket.bind(src_port).is_err() {
            return None;
        }
        if socket.send_slice(&request, server).is_err() {
            // Not connected yet; the poll loop below will retry the send once.
        }
    }

    let mut sent = false;
    loop {
        tick();
        let ts = smoltcp_now();
        iface.poll(ts, device, sockets);

        let socket = sockets.get_mut::<udp::Socket>(udp_handle);

        if !sent && socket.can_send() && socket.send_slice(&request, server).is_ok() {
            sent = true;
        }

        if socket.can_recv() {
            let mut buf = [0u8; 48];
            if let Ok((len, _from)) = socket.recv_slice(&mut buf) {
                if len >= 48 {
                    // Transmit Timestamp seconds field = bytes 40..44, big-endian,
                    // measured from the NTP epoch (1900).
                    let ntp_secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]);
                    if ntp_secs > NTP_TO_UNIX_OFFSET {
                        return Some(ntp_secs - NTP_TO_UNIX_OFFSET);
                    }
                }
            }
        }

        if Instant::now() > deadline {
            log::warn!("smol: SNTP timed out");
            return None;
        }
    }
}

/// Relay uplink flush (`espnow` gateway only). The caller has ALREADY triggered
/// re-association via `RadioManager::switch(Mode::WifiSta)` (which issued
/// `connect()` with the config persisted from the boot NTP burst); here we build
/// a fresh smoltcp stack on the RETAINED STA device, wait for the association +
/// DHCP, then UDP each queued message to the collector as `"<src_id> <telemetry>"`.
///
/// Best-effort + loss-tolerant (it's telemetry): returns `true` iff we associated,
/// got an IP, and sent every message. Connect-wait + DHCP mirror `run_ntp_burst`;
/// the send loop mirrors `sntp_query`. Kept SEPARATE from `run_ntp_burst` (a
/// little duplication) so the hardware-verified NTP path is not disturbed.
///
/// Single-radio airtime: this runs while the PHY is on the AP's channel, so the
/// mesh is deaf on the ESP-NOW channel for its duration — the documented cost.
#[cfg(feature = "espnow")]
pub fn run_udp_flush(
    controller: &mut esp_wifi::wifi::WifiController<'static>,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    mut rng: Rng,
    messages: &[(u8, &[u8])],
    tick: &mut dyn FnMut(),
) -> bool {
    if messages.is_empty() {
        return true;
    }

    let mut iface = create_interface(device);
    let mut sockets_storage: [SocketStorage; 3] = Default::default();
    let mut sockets = SocketSet::new(&mut sockets_storage[..]);

    let mut dhcp_socket = dhcpv4::Socket::new();
    dhcp_socket.set_outgoing_options(&[DhcpOption {
        kind: 12,
        data: b"smol",
    }]);
    let dhcp_handle = sockets.add(dhcp_socket);

    let mut udp_rx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut udp_rx_data = [0u8; 256];
    let mut udp_tx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut udp_tx_data = [0u8; 512];
    let udp_socket = udp::Socket::new(
        udp::PacketBuffer::new(&mut udp_rx_meta[..], &mut udp_rx_data[..]),
        udp::PacketBuffer::new(&mut udp_tx_meta[..], &mut udp_tx_data[..]),
    );
    let udp_handle = sockets.add(udp_socket);

    // FINDING 1b: bound the ENTIRE flush (connect + DHCP + sends + drain) by the
    // short RELAY_FLUSH_BUDGET, not the 30 s NTP budget — a dead AP fails fast so
    // the caller's blocking loop isn't frozen for 30 s.
    let deadline = Instant::now() + RELAY_FLUSH_BUDGET;

    // The caller's switch(WifiSta) already issued connect(); just wait for it.
    while !matches!(controller.is_connected(), Ok(true)) {
        tick();
        if Instant::now() > deadline {
            log::warn!("smol: relay flush — WiFi connect timed out");
            return false;
        }
    }

    // Fresh DHCP lease each burst (the interface was just rebuilt).
    loop {
        tick();
        iface.poll(smoltcp_now(), device, &mut sockets);
        let configured = {
            let socket = sockets.get_mut::<dhcpv4::Socket>(dhcp_handle);
            match socket.poll() {
                Some(dhcpv4::Event::Configured(cfg)) => Some((cfg.address, cfg.router)),
                _ => None,
            }
        };
        if let Some((addr, router)) = configured {
            apply_dhcp(&mut iface, addr, router);
            log::info!("smol: relay flush DHCP {}", addr);
            break;
        }
        if Instant::now() > deadline {
            log::warn!("smol: relay flush — DHCP timed out");
            return false;
        }
    }

    // Bind a pseudo-random ephemeral source port (mirrors sntp_query).
    let src_port = 49152 + (rng.random() % 16384) as u16;
    {
        let socket = sockets.get_mut::<udp::Socket>(udp_handle);
        if socket.bind(src_port).is_err() {
            return false;
        }
    }
    let server = (IpAddress::Ipv4(RELAY_COLLECTOR_IP), RELAY_COLLECTOR_PORT);

    let mut all_sent = true;
    for &(src_id, payload) in messages {
        // Out of the overall budget (finding 1b) — leave the rest for next flush.
        if Instant::now() > deadline {
            all_sent = false;
            break;
        }
        // Datagram = "<3-digit id> <telemetry>".
        let mut dg = [0u8; FLUSH_DG_MAX];
        dg[0] = b'0' + (src_id / 100) % 10;
        dg[1] = b'0' + (src_id / 10) % 10;
        dg[2] = b'0' + src_id % 10;
        dg[3] = b' ';
        let plen = payload.len().min(FLUSH_DG_MAX - 4);
        dg[4..4 + plen].copy_from_slice(&payload[..plen]);
        let dglen = 4 + plen;

        let mut sent = false;
        while !sent {
            tick();
            iface.poll(smoltcp_now(), device, &mut sockets);
            let socket = sockets.get_mut::<udp::Socket>(udp_handle);
            if socket.can_send() && socket.send_slice(&dg[..dglen], server).is_ok() {
                sent = true;
            } else if Instant::now() > deadline {
                all_sent = false; // shared overall budget; rest waits for next flush
                break;
            }
        }
        // Interleave ARP warm-up + egress with the remaining sends: one poll after
        // each enqueue kicks dispatch (an ARP request on a fresh association), so the
        // collector's MAC is likely resolved by the time the drain below runs (N3).
        tick();
        iface.poll(smoltcp_now(), device, &mut sockets);
    }

    // Drain UNTIL the UDP TX buffer is verifiably empty, then return. FINDING N3
    // (HW, 652155b fleet): `send_slice` only ENQUEUES; `iface.poll` dispatches — but
    // on a FRESH association the first dispatch must resolve the collector's MAC via
    // ARP (empty neighbour cache; the AP may power-save-buffer the reply). The old
    // fixed 300 ms drain lost that race → datagrams died in the TX buffer at teardown
    // and `all_sent = true` LIED (the collector saw zero packets). smoltcp RETAINS a
    // packet in the tx buffer until it truly dispatches — verified in smoltcp 0.12
    // `udp::Socket::dispatch`/`PacketBuffer::dequeue_with`: an emit `Err`
    // (neighbour-unknown) consumes 0 bytes, so the packet stays queued — hence
    // `send_queue() == 0` is a true "handed to the radio" signal. Poll until then,
    // bounded by ~2 s AND a small grace past the overall `deadline`; if it never
    // empties, report `all_sent = false` — an HONEST failure the caller's
    // backoff/retry (finding 1) then handles correctly. (So the drain MUST be allowed
    // to finish the job or report failure — the old "can't extend the burst" inverts.)
    let drain_hard = Instant::now() + Duration::from_secs(2);
    let drain_grace = deadline + Duration::from_millis(500);
    loop {
        tick();
        iface.poll(smoltcp_now(), device, &mut sockets);
        if sockets.get_mut::<udp::Socket>(udp_handle).send_queue() == 0 {
            break; // every datagram dispatched from smoltcp toward the radio
        }
        let now = Instant::now();
        if now > drain_hard || now > drain_grace {
            all_sent = false;
            log::warn!("smol: relay flush — TX drain timed out; datagrams undelivered");
            break;
        }
    }
    all_sent
}
