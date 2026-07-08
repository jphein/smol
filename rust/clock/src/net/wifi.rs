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
    socket::{dhcpv4, tcp, udp},
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

/// HA Mosquitto broker (v2 MQTT-native bridge — the disks UDP collector is retired).
/// Address/creds live in the git-ignored `crate::secrets` (retargetable one-liners —
/// see the secrets comment for the VLAN11-leg rationale + the VLAN6 fallback). Built
/// from the `[u8;4]` there so `secrets.rs` stays a plain imports-free consts file.
#[cfg(feature = "wifi")]
const MQTT_BROKER_IP: Ipv4Addr = Ipv4Addr::new(
    crate::secrets::MQTT_BROKER_IP[0],
    crate::secrets::MQTT_BROKER_IP[1],
    crate::secrets::MQTT_BROKER_IP[2],
    crate::secrets::MQTT_BROKER_IP[3],
);
#[cfg(feature = "wifi")]
const MQTT_BROKER_PORT: u16 = crate::secrets::MQTT_BROKER_PORT;

/// The retained downlink topic every node subscribes to for battery voltages, and
/// the uplink topic template `smol/<id>/telemetry` — see `mqtt_session`.
#[cfg(feature = "wifi")]
const BATT_TOPIC: &[u8] = b"smol/display/batt";

/// Budget for one full MQTT session (TCP connect → CONNECT/CONNACK → publishes →
/// SUBSCRIBE → retained downlink → DISCONNECT). Sub-bound of the enclosing burst
/// so MQTT can't eat the whole flush/NTP window; a miss just leaves the cache be.
/// On a gateway flush the session runs INSIDE the association the flush already
/// holds, so it does not extend the mesh-deaf window beyond `RELAY_FLUSH_BUDGET`.
#[cfg(feature = "wifi")]
const MQTT_SESSION_BUDGET: Duration = Duration::from_millis(3000);

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
pub fn try_time_sync(p: WifiPeripherals, batt: &mut crate::batt::BattCache) -> Option<u32> {
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
    // wifi-only build has no relay/gateway role, so the reached-DHCP flag is unused.
    let mut _reached_dhcp = false;
    run_ntp_burst(
        &mut controller,
        &mut device,
        rng,
        &mut || {},
        &mut _reached_dhcp,
        crate::NODE_ID,
        batt,
    )
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
    // N3c: set true once we've ASSOCIATED + got a DHCP lease (before SNTP runs).
    // The caller uses this — NOT the returned NTP Option — to decide gateway role,
    // so an SNTP outage can't demote a node with a working LAN uplink.
    reached_dhcp: &mut bool,
    // This node's logical id — the MQTT client-id at the tail of the burst is
    // `smol-<node_id>`.
    node_id: u8,
    // HA battery cache filled by the MQTT downlink at the tail of this burst (the
    // spec's boot fetch — every wifi/espnow build that reaches DHCP receives the
    // retained `smol/display/batt` payload here).
    batt: &mut crate::batt::BattCache,
) -> Option<u32> {
    // --- smoltcp stack: DHCP + UDP (SNTP) + TCP (MQTT) sockets -----------
    let mut iface = create_interface(device);

    let mut sockets_storage: [SocketStorage; 4] = Default::default();
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

    // TCP socket for the MQTT downlink (the retained battery payload).
    let mut tcp_rx = [0u8; 512];
    let mut tcp_tx = [0u8; 512];
    let tcp_socket = tcp::Socket::new(
        tcp::SocketBuffer::new(&mut tcp_rx[..]),
        tcp::SocketBuffer::new(&mut tcp_tx[..]),
    );
    let tcp_handle = sockets.add(tcp_socket);

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
            // N3c: association + DHCP reached — this alone qualifies the node as a
            // relay GATEWAY (see start()); the SNTP below is best-effort for TIME.
            *reached_dhcp = true;
            break;
        }
        if Instant::now() > deadline {
            log::warn!("smol: DHCP timed out");
            return None;
        }
    }

    // --- SNTP exchange ---------------------------------------------------
    let synced = sntp_query(
        &mut iface,
        device,
        &mut sockets,
        udp_handle,
        rng.random(),
        deadline,
        tick,
    );

    // --- HA battery downlink (MQTT, on this same open burst) -------------
    // We reached DHCP (the loop above only breaks on a lease), so the LAN + broker
    // are reachable — open a short MQTT session and SUBSCRIBE to the retained
    // `smol/display/batt`, receiving it into the cache. DOWNLINK-ONLY here: at boot
    // there is no telemetry to publish yet (the empty publish list). Runs in every
    // wifi/espnow build that reaches DHCP, independent of the SNTP result. Bounded
    // by MQTT_SESSION_BUDGET; a miss leaves the cache untouched.
    let mqtt_deadline = mqtt_deadline(deadline);
    let mqtt_port = 49152 + (rng.random() % 16384) as u16;
    let _ = mqtt_session(
        &mut iface,
        device,
        &mut sockets,
        tcp_handle,
        node_id,
        &[],
        mqtt_port,
        batt,
        mqtt_deadline,
        tick,
    );

    synced
}

/// The MQTT session deadline: the sooner of `MQTT_SESSION_BUDGET` from now and the
/// enclosing burst's `outer` deadline, so MQTT never overruns the burst it rides.
#[cfg(feature = "wifi")]
fn mqtt_deadline(outer: Instant) -> Instant {
    let own = Instant::now() + MQTT_SESSION_BUDGET;
    if own < outer {
        own
    } else {
        outer
    }
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

use core::fmt::Write as _;

/// Heap-free scratch buffer for building an MQTT topic / client-id / discovery JSON
/// via `write!`. 224 bytes holds the largest discovery config (~150 B) with margin.
#[cfg(feature = "wifi")]
struct MqttScratch {
    buf: [u8; 224],
    len: usize,
}

#[cfg(feature = "wifi")]
impl MqttScratch {
    fn new() -> Self {
        Self { buf: [0; 224], len: 0 }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[cfg(feature = "wifi")]
impl core::fmt::Write for MqttScratch {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Push `data` out a connected TCP socket, polling the stack until it is all queued
/// or `deadline` passes. Our MQTT packets are tiny (< tx buffer), so this normally
/// completes in one `send_slice`; returns false on a socket error or timeout.
#[cfg(feature = "wifi")]
fn tcp_send(
    iface: &mut Interface,
    device: &mut esp_wifi::wifi::WifiDevice,
    sockets: &mut SocketSet,
    handle: smoltcp::iface::SocketHandle,
    data: &[u8],
    deadline: Instant,
    tick: &mut dyn FnMut(),
) -> bool {
    let mut off = 0;
    while off < data.len() {
        tick();
        iface.poll(smoltcp_now(), device, sockets);
        let socket = sockets.get_mut::<tcp::Socket>(handle);
        if socket.can_send() {
            match socket.send_slice(&data[off..]) {
                Ok(n) => off += n,
                Err(_) => return false,
            }
        }
        if Instant::now() > deadline {
            return false;
        }
    }
    iface.poll(smoltcp_now(), device, sockets); // nudge the queued bytes onto the wire
    true
}

/// Append whatever bytes are readable on the TCP socket into the stream accumulator
/// `acc[..*acc_len]` (bounded by its capacity). MQTT is a byte stream, so packets
/// can split/coalesce across reads — [`mqtt_session`] parses whole packets out of
/// this accumulator with `mqtt::parse_packet`.
#[cfg(feature = "wifi")]
fn recv_into(
    sockets: &mut SocketSet,
    handle: smoltcp::iface::SocketHandle,
    acc: &mut [u8],
    acc_len: &mut usize,
) {
    let socket = sockets.get_mut::<tcp::Socket>(handle);
    if socket.can_recv() && *acc_len < acc.len() {
        if let Ok(n) = socket.recv_slice(&mut acc[*acc_len..]) {
            *acc_len += n;
        }
    }
}

/// One short MQTT 3.1.1 QoS0 session over a fresh TCP socket to the HA broker:
/// TCP connect → CONNECT (client-id `smol-<node_id>`, username+password) → CONNACK
/// → SUBSCRIBE `smol/display/batt` (downlink FIRST — the retained payload every node
/// needs is prioritized over loss-tolerant telemetry) → for each `(id, line)` in
/// `telemetry` PUBLISH `smol/<id>/telemetry` (bare line, transient) + a RETAINED
/// discovery config → drain the RETAINED battery payload into `batt` → DISCONNECT.
/// Everything is hard-bounded by `deadline` (a sub-bound of the enclosing burst).
/// `telemetry` empty ⇒ downlink-only (the boot path). Returns whether we CONNECTED
/// (CONNACK rc=0): that is the "flush delivered" signal for the caller's backoff —
/// a downlink miss is NOT a failure (the cache simply keeps its prior value).
#[cfg(feature = "wifi")]
#[allow(clippy::too_many_arguments)]
fn mqtt_session(
    iface: &mut Interface,
    device: &mut esp_wifi::wifi::WifiDevice,
    sockets: &mut SocketSet,
    tcp_handle: smoltcp::iface::SocketHandle,
    node_id: u8,
    telemetry: &[(u8, &[u8])],
    src_port: u16,
    batt: &mut crate::batt::BattCache,
    deadline: Instant,
    tick: &mut dyn FnMut(),
) -> bool {
    let broker = (IpAddress::Ipv4(MQTT_BROKER_IP), MQTT_BROKER_PORT);

    // --- TCP connect ---
    {
        let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
        if socket.connect(iface.context(), broker, src_port).is_err() {
            return false;
        }
    }
    loop {
        tick();
        iface.poll(smoltcp_now(), device, sockets);
        let state = sockets.get_mut::<tcp::Socket>(tcp_handle).state();
        if state == tcp::State::Established {
            break;
        }
        if state == tcp::State::Closed || Instant::now() > deadline {
            log::warn!("smol: MQTT TCP connect failed/timeout");
            return false;
        }
    }

    let mut pkt = [0u8; 320];
    let mut acc = [0u8; 256];
    let mut acc_len = 0usize;

    // --- CONNECT ---
    {
        let mut cid = MqttScratch::new();
        let _ = write!(cid, "smol-{}", node_id);
        let Some(n) = crate::net::mqtt::encode_connect(
            &mut pkt,
            cid.as_bytes(),
            crate::secrets::MQTT_USER.as_bytes(),
            crate::secrets::MQTT_PASS.as_bytes(),
        ) else {
            return false;
        };
        if !tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick) {
            return false;
        }
    }

    // --- CONNACK (require rc=0) ---
    let mut connected = false;
    while !connected {
        tick();
        iface.poll(smoltcp_now(), device, sockets);
        recv_into(sockets, tcp_handle, &mut acc, &mut acc_len);
        loop {
            // Extract Copy scalars inside the match so the borrow of `acc` (via the
            // parsed packet) is released before the `copy_within` compaction below.
            let (consumed, ok, fail) = match crate::net::mqtt::parse_packet(&acc[..acc_len]) {
                None => break,
                Some((crate::net::mqtt::Incoming::ConnAck { return_code }, consumed)) => {
                    (consumed, return_code == 0, return_code != 0)
                }
                Some((_, consumed)) => (consumed, false, false),
            };
            acc.copy_within(consumed..acc_len, 0);
            acc_len -= consumed;
            if fail {
                log::warn!("smol: MQTT CONNACK rejected");
                return false;
            }
            if ok {
                connected = true;
                break;
            }
        }
        if Instant::now() > deadline {
            log::warn!("smol: MQTT CONNACK timeout");
            return false;
        }
    }

    // --- SUBSCRIBE smol/display/batt FIRST ---
    // Subscribe before publishing so the broker queues the RETAINED battery payload
    // to us immediately — the downlink (which every node needs) is prioritized over
    // the loss-tolerant telemetry uplink, and the retained reply streams in while we
    // publish. It is drained into the cache after the publishes, below.
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 1, BATT_TOPIC) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }

    // --- PUBLISH telemetry (transient) + discovery config (retained) per node ---
    for &(id, line) in telemetry {
        if Instant::now() > deadline {
            break;
        }
        // Telemetry: bare line to smol/<id>/telemetry (topic carries the id).
        let mut topic = MqttScratch::new();
        let _ = write!(topic, "smol/{}/telemetry", id);
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, topic.as_bytes(), line, false) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // Discovery: retained config so HA auto-creates a registry-managed sensor.
        let mut dtopic = MqttScratch::new();
        let _ = write!(dtopic, "homeassistant/sensor/smol{}/telemetry/config", id);
        let mut json = MqttScratch::new();
        let noun = crate::net::names::name_for_id(id).1;
        let _ = write!(
            json,
            "{{\"unique_id\":\"smol{}_telemetry\",\"state_topic\":\"smol/{}/telemetry\",\"name\":\"smol {}\",\"device\":{{\"identifiers\":[\"smol{}\"],\"name\":\"smol {} {}\"}}}}",
            id, id, id, id, id, noun
        );
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), json.as_bytes(), true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // --- Receive the retained battery payload (SUBSCRIBE was sent above) ---
    let mut got = false;
    while !got {
        tick();
        iface.poll(smoltcp_now(), device, sockets);
        recv_into(sockets, tcp_handle, &mut acc, &mut acc_len);
        loop {
            let consumed = match crate::net::mqtt::parse_packet(&acc[..acc_len]) {
                None => break,
                Some((crate::net::mqtt::Incoming::Publish { topic, payload }, consumed)) => {
                    if topic == BATT_TOPIC {
                        let now = Instant::now().duration_since_epoch().as_millis();
                        batt.store(payload, now); // memcpy out before we compact `acc`
                        got = true;
                        log::info!("smol: MQTT batt downlink cached ({} B)", payload.len());
                    }
                    consumed
                }
                Some((_, consumed)) => consumed,
            };
            acc.copy_within(consumed..acc_len, 0);
            acc_len -= consumed;
        }
        if Instant::now() > deadline {
            log::info!("smol: MQTT downlink not received in budget (keeping cache)");
            break;
        }
    }

    // --- DISCONNECT (clean goodbye) + close the socket ---
    if let Some(n) = crate::net::mqtt::encode_disconnect(&mut pkt) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    sockets.get_mut::<tcp::Socket>(tcp_handle).close();
    iface.poll(smoltcp_now(), device, sockets);
    connected
}

/// Gateway flush → **MQTT burst** (`espnow` gateway only; v2 — this REPLACES the
/// retired UDP-to-collector egress). The caller has ALREADY triggered re-association
/// via `RadioManager::switch(Mode::WifiSta)`; here we build a fresh smoltcp stack on
/// the RETAINED STA device, wait for association + DHCP, then run ONE
/// [`mqtt_session`]: PUBLISH each queued telemetry to `smol/<id>/telemetry` + a
/// RETAINED discovery config, and SUBSCRIBE `smol/display/batt` to receive the
/// retained battery payload into `batt` (which `main` then re-broadcasts to leaves).
///
/// Returns whether we CONNECTED to the broker (CONNACK ok) — the "flush delivered"
/// signal for the caller's backoff. QoS0 publishes are fire-and-forget once
/// connected (like the old UDP sends), and a downlink miss is NOT a failure.
///
/// Single-radio airtime: this runs while the PHY is on the AP's channel, so the
/// mesh is deaf on the ESP-NOW channel for its duration — the documented cost,
/// bounded by `RELAY_FLUSH_BUDGET` (the MQTT session is a sub-bound within it, so
/// it does not extend the deaf window beyond the flush the mesh already pays for).
#[cfg(feature = "espnow")]
pub fn run_mqtt_burst(
    controller: &mut esp_wifi::wifi::WifiController<'static>,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    mut rng: Rng,
    node_id: u8,
    messages: &[(u8, &[u8])],
    batt: &mut crate::batt::BattCache,
    tick: &mut dyn FnMut(),
) -> bool {
    let mut iface = create_interface(device);
    let mut sockets_storage: [SocketStorage; 2] = Default::default();
    let mut sockets = SocketSet::new(&mut sockets_storage[..]);

    let mut dhcp_socket = dhcpv4::Socket::new();
    dhcp_socket.set_outgoing_options(&[DhcpOption {
        kind: 12,
        data: b"smol",
    }]);
    let dhcp_handle = sockets.add(dhcp_socket);

    // TCP socket for the MQTT session (the UDP collector datagram is retired).
    let mut tcp_rx = [0u8; 512];
    let mut tcp_tx = [0u8; 512];
    let tcp_socket = tcp::Socket::new(
        tcp::SocketBuffer::new(&mut tcp_rx[..]),
        tcp::SocketBuffer::new(&mut tcp_tx[..]),
    );
    let tcp_handle = sockets.add(tcp_socket);

    // FINDING 1b (retained): bound the ENTIRE flush by the short RELAY_FLUSH_BUDGET,
    // not the 30 s NTP budget — a dead AP fails fast so the loop isn't frozen 30 s.
    let deadline = Instant::now() + RELAY_FLUSH_BUDGET;

    // The caller's switch(WifiSta) already issued connect(); just wait for it.
    while !matches!(controller.is_connected(), Ok(true)) {
        tick();
        if Instant::now() > deadline {
            log::warn!("smol: MQTT flush — WiFi connect timed out");
            return false;
        }
    }

    // FINDING N3b (retained): re-assert WiFi power-save OFF after re-association so
    // the AP delivers unicast immediately — the flush's disconnect()→connect()
    // resets the IDF ps state, and here the unicast that matters is the whole TCP /
    // MQTT stream (the old UDP path only needed the ARP reply). Same reasoning,
    // same placement (must be AFTER the reconnect). Tradeoff: higher idle draw.
    let _ = controller.set_power_saving(esp_wifi::config::PowerSaveMode::None);

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
            log::info!("smol: MQTT flush DHCP {}", addr);
            break;
        }
        if Instant::now() > deadline {
            log::warn!("smol: MQTT flush — DHCP timed out");
            return false;
        }
    }

    // One MQTT session: publish queued telemetry + discovery, receive the retained
    // battery downlink into the cache. Local TCP port from the same rng seed the
    // UDP path used. Bounded within the overall flush `deadline`.
    let src_port = 49152 + (rng.random() % 16384) as u16;
    mqtt_session(
        &mut iface,
        device,
        &mut sockets,
        tcp_handle,
        node_id,
        messages,
        src_port,
        batt,
        deadline,
        tick,
    )
}
