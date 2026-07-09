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

/// HA Mosquitto broker (v2 MQTT-native bridge — the old LAN UDP collector is retired).
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

/// Twin of [`BATT_TOPIC`] (issue #16): the retained grid-power downlink. Subscribed
/// on the SAME MQTT session — one extra SUBSCRIBE on the already-open connection.
#[cfg(feature = "wifi")]
const GRID_TOPIC: &[u8] = b"smol/display/grid";

/// #23 stage 4: the retained single-gateway ELECTION topic — `MC|<owner_id>|<ch>|<seq>`.
/// Broker-mediated so it can't fragment (all gateways reach the one broker regardless
/// of channel); lowest owner_id wins; `seq` is the load-bearing liveness counter.
#[cfg(feature = "wifi")]
const MESH_CHANNEL_TOPIC: &[u8] = b"smol/mesh/channel";

/// #23 stage 3-4 boot ELECTION result, filled by [`mqtt_session`] from the retained
/// `smol/mesh/channel`. A board that reached DHCP is a candidate; the lowest-id
/// candidate is the OWNER (coexist gateway). Non-owners demote to leaf + scan for the
/// owner's HELLO. `channel` is advisory (0 = unknown → leaves discover by scanning).
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
pub struct MeshElect {
    // --- inputs (seeded by the caller from the live RadioManager) ---
    /// Monotonic "now" in ms (same clock the caller uses for scan/liveness), so the
    /// stale-owner timeout is measured on ONE clock across bursts. (The node's own id
    /// is the `node_id` param `mqtt_session` already carries — not duplicated here.)
    pub now_ms: u64,
    // --- persistent staleness observation (in AND out) ---
    /// The owner id of the last retained `MC` record this node observed.
    pub seen_owner: u8,
    /// That record's `seq`.
    pub seen_seq: u32,
    /// When the current `(seen_owner, seen_seq)` pair was FIRST seen (ms). An owner
    /// whose seq stays frozen past `MC_STALE_MS` from here is presumed dead.
    pub seen_ms: u64,
    // --- outputs (applied to the live role by the caller) ---
    /// True iff I claimed / hold ownership (I am the coexist gateway).
    pub i_am_owner: bool,
    /// The elected owner's id (== my_id when I own it).
    pub owner_id: u8,
}

#[cfg(feature = "wifi")]
impl MeshElect {
    pub fn new(my_id: u8) -> Self {
        Self {
            now_ms: 0,
            seen_owner: 0,
            seen_seq: 0,
            seen_ms: 0,
            i_am_owner: false,
            owner_id: my_id,
        }
    }
}

/// OTA (issue #6): retained per-fleet announce topic (`OTA|build|size|sha|url`). The
/// per-BOARD topic `smol/ota/announce/<id>` is built at runtime from `node_id` — the
/// CANARY path (spec §4b-4 Option 1): JP publishes to one id, watches it reach the new
/// build, then publishes the next / the `all` topic. A board subscribes to BOTH.
#[cfg(feature = "wifi")]
const OTA_ANNOUNCE_ALL_TOPIC: &[u8] = b"smol/ota/announce/all";

/// A retained owner whose `seq` has not advanced for this long is presumed DEAD and
/// may be taken over. The owner re-publishes `MC` (seq++) every gateway flush (~30 s),
/// so 3 missed refreshes with a frozen seq is a safe "owner gone" threshold. Consumed
/// by the [`mqtt_session`] adopt decision (a leaf re-election is what re-reads `MC`
/// after a prolonged HELLO silence, giving the stale check a second sample to fire on).
#[cfg(feature = "wifi")]
const MC_STALE_MS: u64 = 90_000;

/// Parse a retained `MC|<owner_id>|<channel>|<seq>` election payload → (owner, ch, seq).
/// ASCII, decimal fields. Returns `None` on any malformed field (panic-free).
#[cfg(feature = "wifi")]
fn parse_mesh_channel(payload: &[u8]) -> Option<(u8, u8, u32)> {
    let s = core::str::from_utf8(payload).ok()?;
    let rest = s.strip_prefix("MC|")?;
    let mut it = rest.split('|');
    let owner: u8 = it.next()?.parse().ok()?;
    let ch: u8 = it.next()?.parse().ok()?;
    let seq: u32 = it.next()?.parse().ok()?;
    Some((owner, ch, seq))
}

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
pub fn try_time_sync(
    p: WifiPeripherals,
    batt: &mut crate::batt::BattCache,
    grid: &mut crate::grid::GridCache,
) -> Option<u32> {
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
    let mut reached_dhcp = false;
    // wifi-only build has no mesh election; pass a throwaway result.
    let mut elect = MeshElect::new(crate::NODE_ID);
    // wifi-only build has no main-loop OTA/config consume; capture (unused) offers.
    let mut _ota_offer: Option<crate::ota::Announce> = None;
    let mut _config_offer: Option<crate::app::DefaultScreen> = None;
    let mut _install_requested = false;
    let synced = run_ntp_burst(
        &mut controller,
        &mut device,
        rng,
        &mut || false,
        &mut reached_dhcp,
        crate::NODE_ID,
        batt,
        grid,
        &mut elect,
        &mut _ota_offer,
        &mut _config_offer,
        &mut _install_requested,
    );
    // OTA MF-1 (wifi-only build): confirm/rollback the running image on its first boot.
    // self-test = reached DHCP (a broken-WiFi image can't reach it; the just-run OTA
    // download proved the network is up, so a healthy image won't false-rollback). May
    // reset (rollback) → never returns in that case.
    crate::ota::boot_confirm(reached_dhcp);
    synced
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
#[allow(clippy::too_many_arguments)] // +grid (issue #16) tips this to 8 params
pub fn run_ntp_burst(
    controller: &mut esp_wifi::wifi::WifiController<'static>,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    mut rng: Rng,
    tick: &mut dyn FnMut() -> bool,
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
    // Twin grid cache (issue #16): filled from `smol/display/grid` on the same
    // downlink session as `batt`.
    grid: &mut crate::grid::GridCache,
    // #23 boot election result (filled from the retained `smol/mesh/channel`).
    elect: &mut MeshElect,
    // #6 OTA: filled with a gated retained announce, if one is present for this board.
    ota_offer: &mut Option<crate::ota::Announce>,
    // #21: filled with the parsed default-screen config for this board, if present.
    config_offer: &mut Option<crate::app::DefaultScreen>,
    // #33: set true iff a retained OTA `install` command is present for this board.
    install_requested: &mut bool,
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
            // COEXIST SOAK (#23 PART 1): pin association to ch1 so the gateway lands
            // on the same channel the leaf pins to (mesh ch == AP ch). The `roam`
            // SSID spans 1/6/11; this restricts the STA to the ch1 AP (north-bedroom).
            #[cfg(feature = "coexist-soak")]
            channel: Some(1),
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
        // #20 abort: a long-press mid-sync bails the burst (tick latches true).
        if tick() {
            return None;
        }
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
        if tick() {
            return None; // #20 abort during DHCP wait
        }
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
    // wifi/espnow build that reaches DHCP, independent of the SNTP result.
    //
    // FRESH deadline (issue #15a): give MQTT its OWN `MQTT_SESSION_BUDGET` window,
    // NOT the enclosing NTP `deadline`. A slow/rate-limited SNTP can consume the
    // whole 30 s `SYNC_BUDGET` (Cloudflare anycast throttling several boards booting
    // together — HW-observed on id8, 2026-07-08), which would leave the clamped
    // deadline already expired and starve the batt fetch to an instant timeout. The
    // boot burst runs BEFORE the main loop, so the ≤ 3 s tail (worst case ~33 s total)
    // costs nothing the mesh cares about; a miss still leaves the cache untouched.
    let mqtt_deadline = Instant::now() + MQTT_SESSION_BUDGET;
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
        grid,
        elect,
        ota_offer,
        config_offer,
        install_requested,
        mqtt_deadline,
        tick,
    );

    synced
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
    tick: &mut dyn FnMut() -> bool,
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
        if tick() {
            return None; // #20 abort during SNTP exchange
        }
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
/// via `write!`. 320 bytes (bumped from 224 for #33, D6) holds the largest discovery
/// config (~170 B) + the update state JSON with `title` (~140 B), each built + sent
/// sequentially so only one need fit at a time — with headroom for future fields.
#[cfg(feature = "wifi")]
struct MqttScratch {
    buf: [u8; 320],
    len: usize,
}

#[cfg(feature = "wifi")]
impl MqttScratch {
    fn new() -> Self {
        Self { buf: [0; 320], len: 0 }
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
    tick: &mut dyn FnMut() -> bool,
) -> bool {
    let mut off = 0;
    while off < data.len() {
        if tick() {
            return false; // #20 abort mid-send (QoS0 — partial send tolerable)
        }
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
    grid: &mut crate::grid::GridCache,
    elect: &mut MeshElect,
    // #6 OTA: filled with a GATED (build>running, host-allowed, size-ok) retained
    // announce if one is present for this board; the caller then triggers the fetch.
    ota_offer: &mut Option<crate::ota::Announce>,
    // #21 node-manager: filled with the parsed retained default-screen command for
    // this board (Set/Clear), if present + valid; None = absent/invalid (keep current).
    config_offer: &mut Option<crate::app::DefaultScreen>,
    // #33 HA Update entity: set true iff a retained `install` command is present for this
    // board (the native Install button) — the caller AND-gates the fetch on it.
    install_requested: &mut bool,
    deadline: Instant,
    tick: &mut dyn FnMut() -> bool,
) -> bool {
    let broker = (IpAddress::Ipv4(MQTT_BROKER_IP), MQTT_BROKER_PORT);
    // Per-board OTA announce topic (the canary path), built from node_id.
    let mut ota_topic = MqttScratch::new();
    let _ = write!(ota_topic, "smol/ota/announce/{}", node_id);
    // #21 per-board node-manager config topic (retained default-screen command).
    let mut cfg_topic = MqttScratch::new();
    let _ = write!(cfg_topic, "smol/{}/config/default_screen", node_id);
    // #33 per-board OTA command topic (HA Update entity → `install`).
    let mut cmd_topic = MqttScratch::new();
    let _ = write!(cmd_topic, "smol/{}/ota/cmd", node_id);

    // --- TCP connect ---
    {
        let socket = sockets.get_mut::<tcp::Socket>(tcp_handle);
        if socket.connect(iface.context(), broker, src_port).is_err() {
            return false;
        }
    }
    loop {
        if tick() {
            return false; // #20 abort during TCP connect wait
        }
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
        if tick() {
            return false; // #20 abort during CONNACK wait (not yet connected)
        }
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

    // --- SUBSCRIBE smol/display/batt + smol/display/grid FIRST ---
    // Subscribe before publishing so the broker queues the RETAINED downlink payloads
    // to us immediately — the downlinks (which every node needs) are prioritized over
    // the loss-tolerant telemetry uplink, and the retained replies stream in while we
    // publish. Both are drained into their caches after the publishes, below. GRID
    // (issue #16) is one extra SUBSCRIBE on the already-open connection (packet-id 2).
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 1, BATT_TOPIC) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 2, GRID_TOPIC) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    // #23 election: subscribe the retained single-gateway topic (packet-id 3).
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 3, MESH_CHANNEL_TOPIC) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    // #6 OTA: subscribe this board's canary topic (packet-id 4) + the fleet topic
    // (packet-id 5). Retained announces stream in with the batt/grid/mc downlinks.
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 4, ota_topic.as_bytes()) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 5, OTA_ANNOUNCE_ALL_TOPIC) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    // #21 node-manager: subscribe this board's retained default-screen config (pid 6).
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 6, cfg_topic.as_bytes()) {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    // #33 HA Update entity: subscribe this board's OTA command topic (pid 7).
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 7, cmd_topic.as_bytes()) {
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
        // `expire_after: 300` (issue #12): leaves emit telemetry ~15 s and gateways
        // flush ~30 s, so 300 s is a ~10× margin — HA auto-marks a node `unavailable`
        // if nothing arrives for 5 min (silent/dead node) without any firmware ping.
        let mut dtopic = MqttScratch::new();
        let _ = write!(dtopic, "homeassistant/sensor/smol{}/telemetry/config", id);
        let mut json = MqttScratch::new();
        let noun = crate::net::names::name_for_id(id).1;
        let _ = write!(
            json,
            "{{\"unique_id\":\"smol{}_telemetry\",\"state_topic\":\"smol/{}/telemetry\",\"name\":\"smol {}\",\"expire_after\":300,\"device\":{{\"identifiers\":[\"smol{}\"],\"name\":\"smol {} {}\"}}}}",
            id, id, id, id, id, noun
        );
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), json.as_bytes(), true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // --- Receive the retained battery + grid payloads (both SUBSCRIBEs above) ---
    // Wait until BOTH retained downlinks land (they arrive back-to-back after the
    // subscribes) or the deadline — whichever first. A topic with no retained message
    // on the broker (e.g. GRID before HA first publishes it) simply never sets its
    // flag, and we time out keeping that cache's prior value — not a failure, a miss.
    let mut got_batt = false;
    let mut got_grid = false;
    // #23 election: capture the retained MC|owner|ch|seq (None = topic empty/absent).
    let mut got_mc = false;
    let mut mc: Option<(u8, u8, u32)> = None;
    // #6/#21 FIX (reboot-free OTA + running-gateway config): after the primary downlinks
    // (batt/grid/mc) are in, keep draining for a bounded SETTLE window so the retained
    // OTA announce + node-manager config — subscribed AFTER mc (pids 4/5/6), so they
    // arrive slightly later in the SAME broker burst — are captured on a RUNNING gateway
    // too. Previously the loop exited the instant batt/grid/mc landed (all retained +
    // instant on a live gateway), draining neither; they were only ever seen at BOOT,
    // where an absent retained MC made the loop wait to `deadline` and drain them
    // incidentally. Bounded → an ABSENT announce/config costs only the settle window,
    // never the full session budget.
    const DOWNLINK_SETTLE: Duration = Duration::from_millis(300);
    let mut settle_deadline: Option<Instant> = None;
    loop {
        if tick() {
            break; // #20 abort during downlink wait → fall through to clean DISCONNECT
        }
        iface.poll(smoltcp_now(), device, sockets);
        recv_into(sockets, tcp_handle, &mut acc, &mut acc_len);
        loop {
            let consumed = match crate::net::mqtt::parse_packet(&acc[..acc_len]) {
                None => break,
                Some((crate::net::mqtt::Incoming::Publish { topic, payload }, consumed)) => {
                    if topic == BATT_TOPIC {
                        let now = Instant::now().duration_since_epoch().as_millis();
                        batt.store(payload, now); // memcpy out before we compact `acc`
                        got_batt = true;
                        log::info!("smol: MQTT batt downlink cached ({} B)", payload.len());
                    } else if topic == GRID_TOPIC {
                        let now = Instant::now().duration_since_epoch().as_millis();
                        grid.store(payload, now); // twin of batt (issue #16)
                        got_grid = true;
                        log::info!("smol: MQTT grid downlink cached ({} B)", payload.len());
                    } else if topic == MESH_CHANNEL_TOPIC {
                        mc = parse_mesh_channel(payload); // #23 election record
                        got_mc = true; // present (unparseable → treated as claimable below)
                    } else if topic == ota_topic.as_bytes() || topic == OTA_ANNOUNCE_ALL_TOPIC {
                        // #6 OTA: parse + GATE the retained announce (monotonicity +
                        // host allowlist + size). Only an actionable one becomes an
                        // offer; a stale/foreign/oversize one is logged + ignored (so a
                        // retained announce for the running build is a no-op every burst).
                        match crate::ota::parse_announce(payload) {
                            Some(a) => match crate::ota::gate(&a) {
                                Ok(()) => {
                                    log::info!(
                                        "smol OTA: announce build {} accepted (running {})",
                                        a.build,
                                        crate::ota::BUILD_NUMBER
                                    );
                                    *ota_offer = Some(a);
                                }
                                Err(why) => log::info!(
                                    "smol OTA: announce build {} ignored ({:?})",
                                    a.build,
                                    why
                                ),
                            },
                            None => log::warn!("smol OTA: malformed announce ignored"),
                        }
                    } else if topic == cfg_topic.as_bytes() {
                        // #21: parse the retained default-screen command (panic-free).
                        // Some(Set/Clear) → offer to `main`; None → invalid/unknown/
                        // wrong-tier → keep current (never apply garbage; the payload is
                        // an untrusted retained value = boot-loop-brick class).
                        *config_offer = crate::app::parse_default_screen(payload);
                        match config_offer {
                            Some(_) => log::info!("smol #21: default-screen config accepted"),
                            None => log::info!("smol #21: default-screen config absent/invalid — keeping current"),
                        }
                    } else if topic == cmd_topic.as_bytes() {
                        // #33 HA Update entity: the install command. PANIC-FREE exact byte
                        // compare only (untrusted RETAINED payload = boot-loop-brick class):
                        // `install` → arm the flag (the caller AND-gates the fetch on an
                        // already-`gate()`d target — this bool never itself touches flash);
                        // any other bytes → ignore. Cleared (empty retained publish) below.
                        if payload == b"install" {
                            *install_requested = true;
                            log::info!("smol #33: OTA install command received");
                        }
                    }
                    consumed
                }
                Some((_, consumed)) => consumed,
            };
            acc.copy_within(consumed..acc_len, 0);
            acc_len -= consumed;
        }
        // Primary downlinks in → drain the bounded settle window (catches the retained
        // OTA announce + config that arrive just after mc), then finish. If batt/grid/mc
        // never all arrive (e.g. absent MC at boot), fall through to the `deadline` break
        // — which still drains ota/config during the wait (the original boot behaviour).
        if got_batt && got_grid && got_mc {
            match settle_deadline {
                None => settle_deadline = Some(Instant::now() + DOWNLINK_SETTLE),
                Some(sd) if Instant::now() > sd => break,
                _ => {}
            }
        }
        if Instant::now() > deadline {
            log::info!("smol: MQTT downlink(s) not all received in budget (keeping cache)");
            break;
        }
    }

    // --- #23 single-gateway ELECTION with RUNTIME re-decision + stale-owner takeover ---
    // (Fixes oracle #1 dead-owner wedge + #2 split-brain: the decision here flows BACK to
    // the live role via `elect`, and a frozen `seq` lets a dead owner be taken over.)
    //
    // 1. Refresh the persistent staleness observation: a *changed* (owner,seq) resets the
    //    "first seen" clock (fresh liveness); an unchanged pair keeps it (staleness accrues).
    // 2. Adopt a lower-id owner ONLY while it is ALIVE (seq advanced within MC_STALE_MS).
    //    A stale (frozen-seq) lower-id owner is DEAD → fall through to CLAIM (take over).
    //    id >= mine, or empty/absent/unparseable, also CLAIM. `channel` stays 0 (advisory;
    //    leaves discover the real channel by scanning the owner's HELLO).
    let claim_seq: Option<u32> = match mc {
        Some((owner, _ch, seq)) => {
            if owner != elect.seen_owner || seq != elect.seen_seq {
                elect.seen_owner = owner;
                elect.seen_seq = seq;
                elect.seen_ms = elect.now_ms;
            }
            let stale = elect.now_ms.saturating_sub(elect.seen_ms) >= MC_STALE_MS;
            if owner < node_id && !stale {
                elect.i_am_owner = false;
                elect.owner_id = owner;
                None
            } else {
                // id >= mine, or a STALE (dead) lower-id owner → claim / take over.
                elect.i_am_owner = true;
                elect.owner_id = node_id;
                Some(seq.wrapping_add(1))
            }
        }
        None => {
            elect.i_am_owner = true;
            elect.owner_id = node_id;
            Some(1)
        }
    };
    if let Some(newseq) = claim_seq {
        // Record my own ownership locally so my seq counts as "fresh" next read, then
        // publish the retained record (the liveness heartbeat other boards watch).
        elect.seen_owner = node_id;
        elect.seen_seq = newseq;
        elect.seen_ms = elect.now_ms;
        let mut mcp = MqttScratch::new();
        let _ = write!(mcp, "MC|{}|0|{}", node_id, newseq);
        if let Some(n) =
            crate::net::mqtt::encode_publish(&mut pkt, MESH_CHANNEL_TOPIC, mcp.as_bytes(), true)
        {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }
    log::info!(
        "smol: mesh election -> {} (owner id{}, seen seq {})",
        if elect.i_am_owner { "OWNER/gateway" } else { "leaf" },
        elect.owner_id,
        elect.seen_seq
    );

    // --- #33 HA Update entity: self-publish retained discovery + state, clear the cmd ---
    // The board is still connected here (free). `installed_version` = our BUILD_NUMBER;
    // `latest_version` = the gated announce's build if one is present (`ota_offer`), else
    // our own build (= up-to-date). `title` carries the sigil forge name. `in_progress`
    // is true iff we just accepted an install this session (fetch fires right after).
    {
        let installed = crate::ota::BUILD_NUMBER;
        let latest = match ota_offer.as_ref() {
            Some(a) => a.build,
            None => installed,
        };
        let noun = crate::net::names::version_name().1;
        // Discovery config (retained) — bound to the SAME device as telemetry via
        // identifiers:["smol<id>"], so Update lands on the existing device card.
        let mut dtopic = MqttScratch::new();
        let _ = write!(dtopic, "homeassistant/update/smol{}/config", node_id);
        let mut djson = MqttScratch::new();
        let _ = write!(
            djson,
            "{{\"~\":\"smol/{}/ota\",\"stat_t\":\"~/state\",\"cmd_t\":\"~/cmd\",\"pl_inst\":\"install\",\"dev_cla\":\"firmware\",\"name\":\"firmware\",\"uniq_id\":\"smol{}_fw\",\"dev\":{{\"ids\":[\"smol{}\"]}}}}",
            node_id, node_id, node_id
        );
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), djson.as_bytes(), true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // State JSON (retained): installed + latest + in_progress + title.
        let mut sjson = MqttScratch::new();
        let _ = write!(
            sjson,
            "{{\"installed_version\":\"{}\",\"latest_version\":\"{}\",\"in_progress\":{},\"title\":\"v{} {}\"}}",
            installed, latest, *install_requested, latest, noun
        );
        let mut stopic = MqttScratch::new();
        let _ = write!(stopic, "smol/{}/ota/state", node_id);
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, stopic.as_bytes(), sjson.as_bytes(), true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // Clear the retained install command once consumed, so it can't replay next boot.
        if *install_requested {
            if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, cmd_topic.as_bytes(), &[], true) {
                let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
            }
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
#[allow(clippy::too_many_arguments)] // +grid (issue #16) tips this to 8 params
pub fn run_mqtt_burst(
    controller: &mut esp_wifi::wifi::WifiController<'static>,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    mut rng: Rng,
    node_id: u8,
    messages: &[(u8, &[u8])],
    batt: &mut crate::batt::BattCache,
    grid: &mut crate::grid::GridCache,
    // #23 fix: caller-OWNED election state (seeded from the live RadioManager role +
    // its persistent staleness observation), filled by `mqtt_session` and read BACK
    // by the caller so the role re-decides at runtime — not just at boot.
    elect: &mut MeshElect,
    // #6 OTA: filled with a gated retained announce, if one is present for this board.
    ota_offer: &mut Option<crate::ota::Announce>,
    // #21: filled with the parsed default-screen config for this board, if present.
    config_offer: &mut Option<crate::app::DefaultScreen>,
    // #33: set true iff a retained OTA `install` command is present for this board.
    install_requested: &mut bool,
    tick: &mut dyn FnMut() -> bool,
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

    // The caller's switch(WifiSta) already issued connect(); wait for it — but a
    // gateway that ROAMED (lost its AP, e.g. JP's 1/6/11 hard-roam) while staying
    // POWERED would otherwise zombie here every flush until the deadline, wedging HA
    // telemetry. R-CONNECT (oracle audit-#1): re-issue connect() on a throttled cadence
    // so a dropped association self-recovers in seconds — this is what makes the roam
    // actually follow. Throttled to avoid connect() re-entrancy thrash (esp-wifi
    // dislikes back-to-back connect() calls); the initial switch()-issued attempt gets
    // one full RECONNECT_EVERY window before the first retry.
    const RECONNECT_EVERY: Duration = Duration::from_millis(2000);
    let mut next_reconnect = Instant::now() + RECONNECT_EVERY;
    while !matches!(controller.is_connected(), Ok(true)) {
        if tick() {
            return false; // #20 abort during flush re-association
        }
        if Instant::now() > next_reconnect {
            let _ = controller.connect();
            next_reconnect = Instant::now() + RECONNECT_EVERY;
        }
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
        if tick() {
            return false; // #20 abort during flush DHCP wait
        }
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
    // #23: refresh election/liveness each flush — the caller's `elect` carries the
    // persistent (owner,seq,seen_ms) observation IN and the re-decided role OUT.
    mqtt_session(
        &mut iface,
        device,
        &mut sockets,
        tcp_handle,
        node_id,
        messages,
        src_port,
        batt,
        grid,
        elect,
        ota_offer,
        config_offer,
        install_requested,
        deadline,
        tick,
    )
}

/// OTA download budget. Unlike a ~1 s telemetry flush, the OTA burst is mesh-DEAF for
/// the whole ~0.6 MB HTTP download (spec §6-R4), so the window is minutes-scale. It is
/// user/announce-initiated + abortable (`tick` long-press), never auto-fleet-wide.
#[cfg(feature = "espnow")]
const OTA_FETCH_BUDGET: Duration = Duration::from_secs(180);

/// Parse a dotted-quad IPv4 literal (the allowlist is IP-only → no DNS on-device).
#[cfg(feature = "espnow")]
fn parse_ipv4(host: &str) -> Option<smoltcp::wire::Ipv4Address> {
    let mut it = host.split('.');
    let a: u8 = it.next()?.parse().ok()?;
    let b: u8 = it.next()?.parse().ok()?;
    let c: u8 = it.next()?.parse().ok()?;
    let d: u8 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None; // >4 octets
    }
    Some(smoltcp::wire::Ipv4Address::new(a, b, c, d))
}

/// #6 OTA FETCH burst (`espnow` gateway, triggered by a gated announce): stream the
/// announced image over plain HTTP/1.0 into the INACTIVE slot, hashing on the fly,
/// verify SHA-256 + size, then activate + reboot. The caller has ALREADY
/// `switch(Mode::WifiSta)`'d. `tick` keeps the UI alive + latches a long-press ABORT
/// (the whole download is mesh-deaf, §6-R4). Returns only on a NON-activating outcome
/// (failure/abort, good slot untouched); on SUCCESS it reboots inside `ota::activate`
/// and never returns.
#[cfg(feature = "espnow")]
pub fn run_ota_fetch(
    controller: &mut esp_wifi::wifi::WifiController<'static>,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    mut rng: Rng,
    announce: &crate::ota::Announce,
    tick: &mut dyn FnMut() -> bool,
) -> bool {
    let Some((host, port, path)) = crate::ota::split_url(announce.url()) else {
        log::error!("smol OTA: malformed announce URL — aborting fetch");
        return false;
    };
    let Some(ip) = parse_ipv4(host) else {
        log::error!("smol OTA: host is not an IPv4 literal (allowlist is IP-only)");
        return false;
    };
    log::info!(
        "smol OTA: fetching build {} ({} B) from {}:{}{}",
        announce.build,
        announce.size,
        host,
        port,
        path
    );

    let mut iface = create_interface(device);
    let mut sockets_storage: [SocketStorage; 2] = Default::default();
    let mut sockets = SocketSet::new(&mut sockets_storage[..]);
    let mut dhcp_socket = dhcpv4::Socket::new();
    dhcp_socket.set_outgoing_options(&[DhcpOption {
        kind: 12,
        data: b"smol",
    }]);
    let dhcp_handle = sockets.add(dhcp_socket);
    let mut tcp_rx = [0u8; 1536];
    let mut tcp_tx = [0u8; 512];
    let tcp_socket = tcp::Socket::new(
        tcp::SocketBuffer::new(&mut tcp_rx[..]),
        tcp::SocketBuffer::new(&mut tcp_tx[..]),
    );
    let tcp_handle = sockets.add(tcp_socket);
    let deadline = Instant::now() + OTA_FETCH_BUDGET;

    // The caller's switch(WifiSta) already issued connect(); wait for association.
    while !matches!(controller.is_connected(), Ok(true)) {
        if tick() {
            return false;
        }
        if Instant::now() > deadline {
            log::warn!("smol OTA: WiFi association timed out");
            return false;
        }
    }
    let _ = controller.set_power_saving(esp_wifi::config::PowerSaveMode::None);

    // Fresh DHCP lease (interface just rebuilt).
    loop {
        if tick() {
            return false;
        }
        iface.poll(smoltcp_now(), device, &mut sockets);
        let configured = {
            let s = sockets.get_mut::<dhcpv4::Socket>(dhcp_handle);
            match s.poll() {
                Some(dhcpv4::Event::Configured(cfg)) => Some((cfg.address, cfg.router)),
                _ => None,
            }
        };
        if let Some((addr, router)) = configured {
            apply_dhcp(&mut iface, addr, router);
            break;
        }
        if Instant::now() > deadline {
            log::warn!("smol OTA: DHCP timed out");
            return false;
        }
    }

    // TCP connect to the (allowlisted) image host.
    let src_port = 49152 + (rng.random() % 16384) as u16;
    {
        let s = sockets.get_mut::<tcp::Socket>(tcp_handle);
        if s.connect(iface.context(), (IpAddress::Ipv4(ip), port), src_port)
            .is_err()
        {
            log::error!("smol OTA: TCP connect failed");
            return false;
        }
    }
    loop {
        iface.poll(smoltcp_now(), device, &mut sockets);
        if sockets.get_mut::<tcp::Socket>(tcp_handle).may_send() {
            break;
        }
        if tick() {
            return false;
        }
        if Instant::now() > deadline {
            log::warn!("smol OTA: TCP connect timed out");
            return false;
        }
    }

    // Send the request in pieces (no format buffer; total « the 512 B tx ring).
    {
        let s = sockets.get_mut::<tcp::Socket>(tcp_handle);
        let ok = s.send_slice(b"GET ").is_ok()
            && s.send_slice(path.as_bytes()).is_ok()
            && s.send_slice(b" HTTP/1.0\r\nHost: ").is_ok()
            && s.send_slice(host.as_bytes()).is_ok()
            && s.send_slice(b"\r\nConnection: close\r\n\r\n").is_ok();
        if !ok {
            log::error!("smol OTA: failed to send HTTP request");
            return false;
        }
    }

    // Open the inactive-slot writer (image is streamed here, never buffered whole).
    let Some(mut writer) = crate::ota::ImageWriter::begin() else {
        log::error!("smol OTA: cannot open inactive slot (no OTA partition table?)");
        return false;
    };
    let target = writer.target();

    // Receive: accumulate the HTTP headers (validate 200 + Content-Length == size),
    // then STREAM every body byte straight into the flash writer.
    let mut header_buf = [0u8; 512];
    let mut header_len = 0usize;
    let mut headers_done = false;
    loop {
        if tick() {
            log::warn!("smol OTA: aborted by long-press (slot untouched)");
            return false;
        }
        iface.poll(smoltcp_now(), device, &mut sockets);
        let mut closed = false;
        {
            let s = sockets.get_mut::<tcp::Socket>(tcp_handle);
            if s.can_recv() {
                let outcome = s.recv(|data| {
                    if !headers_done {
                        let take = core::cmp::min(header_buf.len() - header_len, data.len());
                        if take == 0 {
                            return (0, false); // headers exceed the buffer → give up
                        }
                        header_buf[header_len..header_len + take]
                            .copy_from_slice(&data[..take]);
                        header_len += take;
                        if let Some(bstart) = crate::ota::header_end(&header_buf[..header_len]) {
                            if crate::ota::status_code(&header_buf[..header_len]) != Some(200) {
                                return (take, false); // non-200 → abort
                            }
                            if let Some(cl) = crate::ota::content_length(&header_buf[..header_len])
                            {
                                if cl != announce.size {
                                    return (take, false); // length mismatch → abort
                                }
                            }
                            headers_done = true;
                            // feed body bytes already captured past the header terminator
                            let fed = writer.feed(&header_buf[bstart..header_len]);
                            return (take, fed);
                        }
                        (take, true) // headers still arriving
                    } else {
                        let fed = writer.feed(data);
                        (data.len(), fed)
                    }
                });
                match outcome {
                    Ok(true) => {}
                    Ok(false) => {
                        log::error!("smol OTA: bad HTTP response or flash write error");
                        return false;
                    }
                    Err(_) => closed = true,
                }
            } else if !s.may_recv() {
                closed = true; // peer closed (Connection: close) + rx drained
            }
        }
        if headers_done && writer.written() >= announce.size {
            break;
        }
        if closed {
            break;
        }
        if Instant::now() > deadline {
            log::warn!("smol OTA: download timed out (slot untouched)");
            return false;
        }
    }

    // Integrity gate: exact size + SHA-256. Pass ⇒ activate (reboots). Fail ⇒ discard.
    if writer.finalize(announce.size, &announce.sha256) {
        log::info!("smol OTA: image VERIFIED — activating the new slot");
        crate::ota::activate(target); // reboots on success
        false // reached only if the otadata write failed
    } else {
        log::error!("smol OTA: size/SHA-256 verify FAILED — discarded (good slot intact)");
        false
    }
}
