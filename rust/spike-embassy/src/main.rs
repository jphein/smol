//! spike-embassy — an Embassy async-executor vertical slice for smol (#198).
//!
//! De-risks the async migration by porting the SAME radio stack the firmware uses
//! (esp-hal 1.0.0-rc.0 / esp-wifi 0.15 / esp-hal-embassy 0.9) onto the Embassy executor
//! and running THREE tasks concurrently:
//!
//! 1. `clock_task` — a 1 Hz tick (display stubbed to `println!` per #198; the point is
//!    task concurrency, not pixels).
//! 2. `esp_now_rx_task` — async ESP-NOW receive → a peers table (beacon listening).
//! 3. `wifi_sntp_task` — WiFi assoc + DHCP + one SNTP query, running CONCURRENTLY.
//!
//! The thesis (the whole reason for the spike): the clock keeps ticking and ESP-NOW
//! keeps hearing DURING the WiFi association burst, because `async`/`await` generates
//! the state machines the superloop hand-rolls (#89 NtpMachine, the OTA chunk loop…).
//!
//! HW-HELD: this compiles + builds an espflash image, but is NEVER flashed here — the
//! bench board is future work. Correctness is a build-side claim; runtime is future.

#![no_std]
#![no_main]

extern crate alloc;

mod secrets;

// Emit the ESP-IDF app descriptor so espflash builds a bootable image (the 2nd-stage
// bootloader validates it). Required since esp-hal 1.0.
esp_bootloader_esp_idf::esp_app_desc!();

use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::rng::Rng;
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use esp_wifi::esp_now::EspNow;
use esp_wifi::wifi::{ClientConfiguration, Configuration, WifiController, WifiDevice};
use esp_wifi::EspWifiController;
use static_cell::StaticCell;

/// Standard esp-hal example helper: leak a value to a `'static` reference so it can be
/// moved into a spawned task / driver.
macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

/// A tiny last-heard peer table filled by the ESP-NOW RX task — the async analogue of
/// the firmware's PEERS roster. Fixed capacity, no alloc, no mutex (single-owner task).
struct PeerTable {
    macs: heapless::Vec<[u8; 6], 16>,
}

impl PeerTable {
    fn new() -> Self {
        Self { macs: heapless::Vec::new() }
    }
    /// Record a peer MAC; returns true if it was newly seen.
    fn observe(&mut self, mac: [u8; 6]) -> bool {
        if self.macs.contains(&mac) {
            return false;
        }
        let _ = self.macs.push(mac);
        true
    }
    fn len(&self) -> usize {
        self.macs.len()
    }
}

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // esp-wifi needs a heap for its C-side allocations.
    esp_alloc::heap_allocator!(size: 72 * 1024);

    // Two independent timers: SYSTIMER drives the Embassy time queue; TIMG0 backs esp-wifi.
    let systimer = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(systimer.alarm0);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let mut rng = Rng::new(peripherals.RNG);

    let esp_wifi_ctrl = mk_static!(
        EspWifiController<'static>,
        esp_wifi::init(timg0.timer0, rng).unwrap()
    );

    // ONE radio init hands out BOTH the WiFi STA device AND the ESP-NOW handle — this is
    // exactly how the firmware coexists WiFi + ESP-NOW on the single C3 radio.
    let (controller, interfaces) = esp_wifi::wifi::new(esp_wifi_ctrl, peripherals.WIFI).unwrap();
    let sta_device = interfaces.sta;
    let esp_now = interfaces.esp_now;

    // Async network stack (embassy-net) over the STA device, DHCP client.
    let seed = ((rng.random() as u64) << 32) | (rng.random() as u64);
    let (stack, runner) = embassy_net::new(
        sta_device,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    // Spawn the concurrent tasks — THE THESIS: they all run together on one executor.
    spawner.spawn(clock_task()).ok();
    spawner.spawn(esp_now_rx_task(esp_now)).ok();
    spawner.spawn(net_task(runner)).ok();
    spawner.spawn(wifi_sntp_task(controller, stack)).ok();
    // main returns; the executor keeps polling the spawned tasks.
}

/// (1) The clock. Ticks once a second, forever — never blocked by the WiFi burst.
#[embassy_executor::task]
async fn clock_task() {
    let mut secs: u32 = 0;
    loop {
        Timer::after(Duration::from_secs(1)).await;
        secs = secs.wrapping_add(1);
        // Display is stubbed to the console (#198: concurrency, not pixels). A real port
        // renders to the SSD1306 here between await points.
        println!("[clock] {:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60);
    }
}

/// (2) ESP-NOW receive → peers table. Keeps hearing beacons DURING the WiFi burst.
#[embassy_executor::task]
async fn esp_now_rx_task(mut esp_now: EspNow<'static>) {
    let mut peers = PeerTable::new();
    loop {
        let r = esp_now.receive_async().await;
        if peers.observe(r.info.src_address) {
            println!("[esp-now] new peer {:02x?} — {} known", r.info.src_address, peers.len());
        }
    }
}

/// The embassy-net driver pump (infra task for the WiFi stack).
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

/// (3) WiFi assoc + DHCP + one SNTP query — the "burst" the clock must survive.
#[embassy_executor::task]
async fn wifi_sntp_task(mut controller: WifiController<'static>, stack: Stack<'static>) {
    controller
        .set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: secrets::WIFI_SSID.into(),
            password: secrets::WIFI_PASS.into(),
            ..Default::default()
        }))
        .unwrap();
    controller.start_async().await.unwrap();
    println!("[wifi] associating…");
    while controller.connect_async().await.is_err() {
        Timer::after(Duration::from_secs(2)).await;
    }
    println!("[wifi] associated; waiting for DHCP…");
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        println!("[wifi] DHCP up: {}", cfg.address.address());
    }
    match sntp_once(stack).await {
        Some(unix) => println!("[sntp] unix time = {unix}"),
        None => println!("[sntp] no reply"),
    }
}

/// Minimal SNTP: a single 48-byte client request over UDP; returns the server's
/// transmit timestamp as unix seconds. (Enough to exercise the async UDP path.)
async fn sntp_once(stack: Stack<'static>) -> Option<u64> {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 256];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    sock.bind(45123).ok()?;

    let mut req = [0u8; 48];
    req[0] = 0x1b; // LI = 0, VN = 3, Mode = 3 (client)
    // Cloudflare NTP (162.159.200.123:123) — runtime-only; never reached in this HW-held spike.
    let server = embassy_net::IpEndpoint::new(
        embassy_net::IpAddress::v4(162, 159, 200, 123),
        123,
    );
    sock.send_to(&req, server).await.ok()?;

    let mut resp = [0u8; 48];
    let (n, _) = sock.recv_from(&mut resp).await.ok()?;
    if n < 44 {
        return None;
    }
    // Transmit timestamp seconds are at bytes 40..44 (NTP epoch 1900).
    let ntp_secs = u32::from_be_bytes([resp[40], resp[41], resp[42], resp[43]]);
    Some((ntp_secs as u64).wrapping_sub(2_208_988_800))
}
