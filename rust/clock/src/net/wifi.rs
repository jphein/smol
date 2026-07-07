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

/// Seconds between the NTP epoch (1900-01-01) and the Unix epoch (1970-01-01).
const NTP_TO_UNIX_OFFSET: u32 = 2_208_988_800;

/// Overall budget for the WiFi+SNTP burst. If we don't have the time by then,
/// give up and let the clock free-run from its compile-time constant.
const SYNC_BUDGET: Duration = Duration::from_secs(30);

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
    let rng = Rng::new(p.rng);
    let esp_wifi_ctrl: EspWifiController<'static> =
        esp_wifi::init(timg0.timer0, rng.clone()).ok()?;
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

        if !sent && socket.can_send() {
            if socket.send_slice(&request, server).is_ok() {
                sent = true;
            }
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
