//! Network module: WiFi + SNTP (Phase 2) and ESP-NOW + radio switching
//! (Phase 3). Everything here is feature-gated so the default Phase 1 build
//! pulls in none of the esp-wifi stack.
//!
//! Design note: `esp_hal::init()` may only run once and hands out the
//! peripheral singletons. `main` owns that call and passes the radio-related
//! peripherals into this module, so we never double-initialise the HAL.

#[cfg(feature = "wifi")]
mod wifi;

// #233's smoltcp `phy::Device` shim (`radio_dev.rs`, `SmolWifiDevice`) was DELETED by #335
// STEP T. Its own header said it: "when smol moves to embassy-net (#198) this whole module is
// deleted." esp-radio 0.18's STA `Interface` implements `embassy_net_driver::Driver` directly,
// so with the transport on embassy-net there is nothing left for a hand-rolled phy shim to
// adapt — `bring_up_stack` hands the interface straight to `embassy_net::new`.

/// #141: clamp the radio's max TX power. Cheap C3-supermini boards distort their own TX at
/// full power (worse on marginal USB supplies) — the AP receives corrupted auth/ACK frames
/// (the "auth expired at strong signal" / silent-hostapd / mid-transfer-stall class). Units
/// are 0.25 dBm steps (the IDF `esp_wifi_set_max_tx_power` contract, valid range 8..=84);
/// 34 = 8.5 dBm, the sibling-project-proven value for this board class. Requires a STARTED
/// WiFi driver — called at radio init and re-asserted beside every #139 `PowerSaveMode::None`
/// assert (a driver stop/start resets it; connect() does not).
#[cfg(feature = "wifi")]
// #172: a wifi-layer TX-power clamp, but every caller today is on an espnow-gated path
// (run_mqtt_burst / run_ota_fetch / mode). Keep it available in wifi builds; silence
// dead-code in a wifi-without-espnow build so `clippy --features wifi -D warnings` passes.
#[cfg_attr(not(feature = "espnow"), allow(dead_code))]
pub(crate) fn assert_max_tx_power() {
    const MAX_TX_POWER_QDBM: i8 = 34; // 8.5 dBm x 4 (quarter-dBm units)
    let err = unsafe { esp_wifi_sys::include::esp_wifi_set_max_tx_power(MAX_TX_POWER_QDBM) };
    if err != 0 {
        log::debug!("smol #141: esp_wifi_set_max_tx_power -> {err}");
    }
}

/// #204: the crown's CURRENT AP association — `(channel, RSSI dBm, BSSID)` — via the IDF
/// `esp_wifi_sta_get_ap_info` FFI. `None` if not associated or the call errors. Published in the
/// crown's DIAG so the #204 coexist-starvation hypothesis (crown deaf when its roam AP is NOT
/// co-channel with the mesh ch6) is TESTABLE from telemetry — the forensics gap that cost ~3h of
/// pcap tonight (the fw could not report which AP/channel/RSSI a deaf crown was on).
#[cfg(feature = "espnow")]
pub(crate) fn current_ap_info() -> Option<(u8, i8, [u8; 6])> {
    // SAFETY: `esp_wifi_sta_get_ap_info` fills a caller-owned POD record (all-zero is a valid
    // initial state); it reads current-association state only, no aliasing. Same FFI idiom as
    // `assert_max_tx_power` above.
    let mut rec: esp_wifi_sys::include::wifi_ap_record_t = unsafe { core::mem::zeroed() };
    let err = unsafe { esp_wifi_sys::include::esp_wifi_sta_get_ap_info(&mut rec) };
    if err != 0 {
        return None;
    }
    Some((rec.primary, rec.rssi, rec.bssid))
}

// Hand-rolled MQTT 3.1.1 (QoS0) codec for the HA batt/telemetry bridge (v2). Pure
// encode/decode; the socket poll-loop that drives it lives in `wifi.rs`.
#[cfg(feature = "wifi")]
mod mqtt;

#[cfg(feature = "espnow")]
pub mod mode;

// #164 per-peer link-quality (ETX) metric: the PURE reach-register + cost mapping, ported
// from babeld's neighbour.c (see docs/superpowers/research/althea-babel-study.md, #163).
// Host-testable (experiments/etx_verify), no HAL deps. espnow-gated to match its sole
// consumer `mode::Roster` (its `LinkQuality`-per-peer holder) — so the default/wifi Phase-1/2
// builds stay byte-free of it, exactly like `flood`/`wire`.
#[cfg(feature = "espnow")]
pub mod etx;

// #267 cross-burst OTA-fetch resume: the PURE resume-key logic (does a saved cursor match this
// staged image + slot → what offset to resume from). Host-testable (experiments/267_resume_verify),
// no HAL deps. Consumed by `crate::ota::ImageWriter` (the HW flash writer + the .bss cursor), which
// is espnow-gated — so gate it the same, byte-free of the default/wifi builds like `etx`/`flood`.
#[cfg(feature = "espnow")]
pub mod ota_resume;

// #13 routed multi-hop mesh: the PURE managed-flood decision core (SeenSet + forward
// decision + HopLatch escalation state machine), host-testable, no HAL deps. Driven by
// the relay path in `mode`, so espnow-gated.
#[cfg(feature = "espnow")]
pub mod flood;

// #21/#56 keyed-CFG relay SCHEDULING: the PURE rotating-cursor + slot-eviction decisions that
// make "a config the dashboard set is never relayed" structurally impossible. Extracted from
// `mode::broadcast_cached_configs` + `wifi::CfgCache` after both silently starved their tail
// entries on the live fleet. Host-testable (experiments/cfg_relay_verify), no HAL deps;
// espnow-gated like `flood`/`wire` since the relay is the only consumer.
#[cfg(feature = "espnow")]
pub mod cfgsched;

// #13: the PURE SMOLv1 relay-family wire codec (RELAY/RELAYACK/RELAY2/RELAYACK2/BATT2/GRID2 +
// the fixed-width ASCII field helpers), extracted from `mode` so the frame formats are
// host-unit-testable off-target (see `experiments/relay_compat`) — the mixed-fleet / #124
// byte-compat guard. `mode` re-exports it via `use crate::net::wire::*`.
#[cfg(feature = "espnow")]
pub mod wire;

// #217 rung-3: co-channel-preferred crown AP selection + the never-crownless strand-guard state
// machine. PURE (no esp-hal/esp-wifi, no alloc) so it's host-tested verbatim by
// `experiments/ap_select_verify` (#[path]-include, like `wire`); `wifi`/`mode` build ApViews from
// scan results + drive the WiFi association + crown state from its decisions.
#[cfg(feature = "espnow")]
pub mod coexist;

// #381 crown SELF-OTA gating: the pure "may the crown update itself yet" decision, extracted from
// `main`'s `do_install` after a permanently-deaf leaf's armed install was found pinning BOTH gates
// on indefinitely — the crown SKIPS its own install, which is indistinguishable from being up to
// date, so a roll reports success with the crown on the old build. PURE (no esp-hal/esp-radio, no
// alloc), host-tested by `experiments/381_gate_verify` (`#[path]`-include, like `coexist`);
// espnow-gated because the relay is the only thing that can arm the gates.
#[cfg(feature = "espnow")]
pub mod otagate;

// #278 the `SMOLv1 ELECT` frame + epoch/anti-flap core — PURE (no esp-hal/esp-radio, no alloc),
// host-tested by `experiments/mesh_elect_verify` (`#[path]`-include, like `coexist`), which runs
// the esp32c6-watch donor's own tests against this exact file: 10 of them (8 wire + 2 consensus).
// The wire codec is byte-identical to the donor's and that is the load-bearing part — the watch
// holds `ELECT_ENFORCE = false` until smol speaks this frame, because two watches are a quorum
// that could otherwise agree to leave ch6 and strand the C3 fleet (#278, #335).
//
// PARTIAL port, not verbatim: the donor's `Elector` (channel VOTE) is deliberately absent. Per
// #269 the mesh channel DERIVES from the elected gateway's AP channel — a consequence, not a
// vote — so 29 of the donor's 39 tests exercised machinery smol does not have and were dropped
// with it. They live upstream, which still owns the election; re-port rather than reconstruct.
//
// Distinct from `election` (which node is GATEWAY) and from `coexist` (which AP do *I* join, given
// a channel). This module carries and ORDERS the channel the fleet meets on — the value `coexist`
// takes as its `mesh_ch` argument and that smol still hardcodes as `ESP_NOW_FIXED_CHANNEL`.
//
// WIRED (stage 2, #278): `mode::parse_frame` dispatches `Frame::Elect`, the crown announces via
// `RadioManager::elect_tick` and the pre/post bursts in `reassoc_ch6_prefer`, and a leaf tracks
// every announcement in its `Follower`. The stage-1 `#[allow(dead_code)]` is gone, together with
// the matching `[unobservable]` entry in `tools/build-matrix.toml` — the module now emits real
// code in the espnow tier, so that entry's claim became false and the gate said so.
//
// ACTING on an announcement is behind `mesh_elect::FOLLOW_ENABLED`, default OFF. Observing is not:
// a leaf parses, orders and reports every ELECT frame regardless, which is what lets the fleet be
// measured before it is moved (#278's flip criterion is evidence, not review).
#[cfg(feature = "espnow")]
pub mod mesh_elect;

// Configurable best-gateway election — PURE (no esp-hal/esp-wifi, no alloc), host-tested verbatim by
// `experiments/election_verify` (#[path]-include, like `coexist`). Seeded by `wifi`'s MeshElect
// resolver + `mode`'s flush/recovery paths; gated to `wifi` (where MeshElect lives — every election
// path is built with WiFi present).
#[cfg(feature = "wifi")]
pub mod election;

// Minimal HTTP/1.x response-head parsers for the OTA fetch leg — PURE, host-tested verbatim by
// `experiments/ota_http_verify` (#[path]-include). Extracted from `ota` so the #gateway-election
// byte-0 fetch bug (a coalesced header+binary-body segment failing UTF-8 → status None) has ONE
// definition with a regression test. `espnow`-gated (only `run_ota_fetch` uses it).
#[cfg(feature = "espnow")]
pub mod http;

// #181 mesh-ledger cores — the PURE, host-tested L1/L2/L3 primitives (sha256 + ed25519 injected),
// wired into the firmware by `ledger_link` (#182 hash-chain, #183 CT-Merkle anchor, #184 signed
// tree-head; landed inert via PRs #220/#223/#224). Each is a frozen library-of-primitives with a
// full host-test suite (`experiments/{ledger,treehead,sth}_verify`); `ledger_link` wires a
// meaningful SUBSET now (own-chain append + crown anchor/sign + verify-what-you-sign self-check),
// with peer-STH gossip/acceptance the HW-gated L2-coordination follow-up. `#[allow(dead_code)]` on
// the CORE modules (this is a binary crate, so a not-yet-wired pub primitive would trip `-D
// warnings`) — the integration `ledger_link` below carries NO allow, so its own quality is enforced.
#[cfg(feature = "espnow")]
#[allow(dead_code)]
pub mod ledger;
#[cfg(feature = "espnow")]
#[allow(dead_code)]
pub mod treehead;
#[cfg(feature = "espnow")]
#[allow(dead_code)]
pub mod sth;
#[cfg(feature = "espnow")]
pub mod ledger_link;

// #349 image TARGET identity — the structured `TargetId` embedded in every OTA-capable image,
// and the checker a board runs over an incoming one before it commits. PURE (no HAL), so
// `experiments/target_guard_verify` `#[path]`-includes this exact file and proves on the host
// that the guard REFUSES, not merely that it accepts. `wifi`-gated: the tiers that carry no OTA
// engine also carry no descriptor, so the default build stays byte-free of it.
#[cfg(feature = "wifi")]
pub mod target;

// #352 board VARIANT identity — what this board IS (chip + did the OLED answer at boot), and
// the single owner of the Home Assistant `model` label. ORTHOGONAL to `target` above and it must
// stay that way: `TargetId` must be decidable from an IMAGE alone (that is the whole #349
// suitability guard), and a board variant can only be discovered by probing hardware. They
// compose — `profile` borrows `target::SELF_CHIP` rather than re-deriving the chip, which is
// exactly the defect #352 fixed. PURE (no HAL), so `experiments/profile_verify` `#[path]`-includes
// it and covers every chip on the host, including the S3 this tree cannot yet build.
// `wifi`-gated to match its sole consumer, `wifi::device_extras`.
#[cfg(feature = "wifi")]
pub mod profile;

// #25 WLED WiZmote-emit (smol as a WLED "linked remote"). `wled = ["espnow"]`, so
// this is present only in a wled build; the default/wifi/espnow builds are byte-free
// of it (the module is `#![cfg(feature = "wled")]`). Referenced by `app` (the
// WledRemote screen) + `mode` (broadcast_wled_button), so it is `pub`.
#[cfg(feature = "wled")]
pub mod wled;

// #26 smol Cast: stream the gateway's OLED image to a network WLED matrix as
// realtime UDP pixels. `cast = ["wifi"]`. `cast` is the PURE packer + shadow
// framebuffer (host-testable, no HAL deps); `cast_oled` is the DrawTarget tee that
// feeds it (needs ssd1306). Absent from every non-cast build → the default / wifi / espnow
// tiers are byte-free of it, and #351's tools/check_exclusions.py proves that per tier from
// the DWARF line table rather than leaving it as this sentence. (The `wled` TIER is NOT on
// that list: since #350 it is `wled,${canonical}`, and the canonical fleet features include
// `cast`. The old wording named a feature combination nothing builds.)
#[cfg(feature = "cast")]
pub mod cast;
#[cfg(feature = "cast")]
pub mod cast_oled;

// Deterministic magical node names (realm-sigil port). Needs no radio — a node
// derives its OWN name and any peer's name from the logical id alone — so it is
// compiled in ALL builds (peer names are only *displayed* under espnow, but our
// own name is the idle bottom-line label everywhere).
pub mod names;

#[cfg(feature = "wifi")]
pub use wifi::WifiPeripherals;

// #56 keyed CFG: re-export the screen config-channel key so `main` (crate root, outside
// this module) can name it when pulling the screen offer from the keyed relay. `wifi` is
// private to `net`; `mode`/`wifi` reach the const directly, but `main` needs this bridge.
// espnow-gated: `main` consumes it ONLY on the leaf-apply path (`take_cfg_offer`), which is
// espnow-only — a wifi-only build reaches the const in-module (no re-export → no unused-import).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_SCREEN;
// #48 LED mode key — same `main`-bridge rationale as the screen key (espnow leaf-apply path).
// #55/#52 add their keys (P/R) here as each feature wires its apply.
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_LED;
// #43 display-units key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(U)). CFG_TARGET_ALL stays wifi-internal (only mode.rs/wifi.rs name it).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_UNITS;
// #55 plugin-mask key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(P)).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_PLUGINS;
// #52 remote-reboot key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(R), with a boot-debounce before software_reset()).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_REBOOT;
// #45 custom-screen key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(Y); the held layout feeds the Custom plugin render).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_CUSTOM;
// #303 Bard story-prompt key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(T); the apply hands it to bard::set_prompt, which validates it).
#[cfg(all(feature = "espnow", feature = "bard"))]
pub use wifi::CFG_KEY_TALE;
// #302 Bard delivery key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(V); the apply hands it to bard::set_delivery, which clamps and validates it).
#[cfg(all(feature = "espnow", feature = "bard"))]
pub use wifi::CFG_KEY_DELIVERY;
// #100 network-switch key — same `main`-bridge rationale (espnow leaf-apply path via
// take_cfg_offer(N); the apply writes the NVS net-record + reboots into the slot).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_NET;
// #100 Stage 2/3 broker + OTA-host override keys — same `main`-bridge rationale (espnow apply path
// via take_cfg_offer(B)/(O); B writes the NVS record + reboots, O writes it WITHOUT a reboot).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_BROKER;
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_OTA;
// #197 herald NOTIFY key — espnow leaf-apply path (take_cfg_offer(M) → crate::toast::set).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_NOTIFY;
// #gateway-election all-nodes-WiFi DEBUG key — espnow leaf/own apply path
// (take_cfg_offer(A) → RadioManager::set_debug_wifi_all).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_WIFI_ALL;
// #72 IO-registry key — the leaf/own apply path (take_cfg_offer(G) → io::apply_wire re-binds
// the free GPIOs). `io`-gated (⊃ espnow): only the io apply path names it here.
#[cfg(feature = "io")]
pub use wifi::CFG_KEY_IO;
// #72 IO output-control key — the leaf/own apply path (take_cfg_offer(g) → io::apply_set drives
// the bound OUTPUT slots). `io`-gated, same rationale.
#[cfg(feature = "io")]
pub use wifi::CFG_KEY_IO_SET;
// #45: `main` sizes its held Custom-layout buffer to the max keyed value — bridge the const out
// of the private `wifi` module (espnow-only: only the Custom apply path names it).
#[cfg(feature = "espnow")]
pub use wifi::CFG_VALUE_MAX;

// #71 on-demand WiFi-scan key — same `main`-bridge rationale (espnow apply path via
// take_cfg_offer(W) → run_scan).
#[cfg(feature = "espnow")]
pub use wifi::CFG_KEY_SCAN;

// `try_time_sync` is the Phase-2 entry point; under `espnow`, `main` calls
// `mode::start` instead, so only re-export it when espnow is NOT enabled.
#[cfg(all(feature = "wifi", not(feature = "espnow")))]
pub use wifi::try_time_sync;

// #192: the NTP re-sync staleness threshold — read by `main`'s flush-cadence re-sync trigger.
// `wifi` is a private submodule, so main reaches the const only via this re-export.
#[cfg(feature = "espnow")]
pub(crate) use wifi::NTP_RESYNC_AGE_S;

/// Install esp-wifi's heap ONCE. esp-alloc declares the `#[global_allocator]`
/// inside its own crate; this macro just adds an internal-RAM region to it.
/// Defined here so both the Phase 2 (`wifi`) and Phase 3 (`espnow`) code paths
/// share a single heap region rather than each reserving their own.
///
/// #300 (JP's decision, 2026-07-27): 128 → 96 KiB. RADIO-ADJACENT — this is #140's dial, moved
/// for a non-radio reason, so the reasoning belongs here rather than in the bard. The T13 bench
/// measured a 54,856 B stack high-water (WiFi burst + crown duty + stories) against the 14,240 B
/// the bard's `.bss` had left; no `SEQ_CAP` alone closes a 4× gap, so 32 KiB of DRAM comes back
/// from this heap and the rest from a shallower KV cache (`nano_llm::SEQ_CAP` 160 → 80). The
/// #140 audit's own figure is what makes this safe: the low-watermark bottomed at ~52 KB FREE of
/// 128 KiB during crown duty, so 96 KiB still leaves ~20 KB of margin over the measured peak
/// demand. If a future radio change pushes allocation up, this is the first number to re-audit —
/// the RX-buffer tuning in .cargo/config.toml [env] draws from HERE.
///
/// #140: grown 72 → 128 KiB. The gateway free-heap low-watermark bottomed at ~5.9 KB during crown
/// duty (esp-wifi's static RX pool ≈16 KB + dynamic RX churn draw from THIS region), leaving no room
/// to raise the RX buffers that fix the sustained-fetch stalls. The heap audit
/// (scratch/smol-ha-batt/140-heap-audit.md) showed ~120 KB of the C3's 313 KiB DRAM window unused —
/// so growing the heap is the safe unlock: it lifts the low-watermark to ~52 KB even after the
/// #140 static-RX bump (.cargo/config.toml [env]). The region is uninit `.bss` in DMA-capable
/// internal SRAM; 128 KiB keeps `.data`+`.bss` (~191 KB) well under the DRAM window before stack.
#[cfg(feature = "wifi")]
pub fn init_heap() {
    esp_alloc::heap_allocator!(size: 96 * 1024);
}

/// #233/#140: the esp-radio 0.18 controller config. In the old esp-wifi 0.15 stack the
/// RX-buffer counts were compile-time `ESP_WIFI_CONFIG_*` env knobs (.cargo/config.toml);
/// in 0.18 they are runtime `ControllerConfig` fields threaded into `wifi::new`. The
/// #140 tuning (static_rx 16 / dynamic_rx 40 / rx_queue 8 / rx_ba_win 12) is re-applied here.
/// Shared by both radio-init paths (`wifi::try_time_sync` and `mode::RadioManager::new`).
#[cfg(feature = "wifi")]
pub(crate) fn radio_controller_config() -> esp_radio::wifi::ControllerConfig {
    esp_radio::wifi::ControllerConfig::default()
        .with_static_rx_buf_num(16)
        .with_dynamic_rx_buf_num(40)
        .with_rx_queue_size(8)
        .with_rx_ba_win(12)
}

/// #335 STEP T: the always-on embassy-net driver pump. `runner.run().await` drives the internal
/// smoltcp stack's TX/RX/timers and the DHCP client, REPLACING the ~28 hand-driven `iface.poll()`
/// / `dhcp.poll()` sites the burst paths used to carry. It parks awaiting interface readiness, so
/// while it awaits, the executor runs the inline ESP-NOW mesh loop — which is the structural half
/// of the deaf-window kill: the mesh keeps polling *between* the transport's `.await`s.
#[cfg(feature = "wifi")]
#[embassy_executor::task]
async fn net_task(
    mut runner: embassy_net::Runner<'static, esp_radio::wifi::Interface<'static>>,
) -> ! {
    runner.run().await
}

/// #335 STEP T: `StackResources<N>`'s N, chosen rather than inherited — see `bring_up_stack`.
///
/// **Measured cost (#439): `StackResources<N>` = 448 + 352·N bytes**, exactly linear across five
/// probe points, all `MaybeUninit` so it is pure `.bss`. N=5 is **2,208 B**.
///
/// **Why 4, derived from embassy-net 0.9.1's SOURCE against THIS crate's feature set** —
/// `StackResources<SOCK>` is the *entire* `SocketSet` storage (`lib.rs:75`), and the stack adds
/// its own housekeeping sockets into it. Both of those adds are `#[cfg]`-gated, which is the part
/// that decides the number:
///
/// | slot | what | present here? |
/// |---|---|---|
/// | — | DNS (`lib.rs:350`) | **NO** — gated `#[cfg(feature = "dns")]`, and our embassy-net features are `tcp, udp, dhcpv4, medium-ethernet, log` |
/// | 1 | DHCPv4 (`lib.rs:703-710`) | yes — gated `#[cfg(feature = "dhcpv4")]`, which we enable |
/// | 2 | the TCP socket (MQTT session / OTA fetch — never both) | transient |
/// | 3 | the UDP socket (SNTP request, or `cast`'s DNRGB listener) | transient |
/// | 4 | spare — one transient outliving another across a burst boundary | transient |
///
/// ⚠️ **The DNS row is why this const carries a derivation instead of a number.** A first pass at
/// this read `sockets.add(dns::Socket::new(..))` at `lib.rs:350` and counted the slot; the line is
/// real, and the `#[cfg(feature = "dns")]` directly above it is what makes it irrelevant here.
/// Reading a call site without its gate is how a static ends up sized for a build nobody makes.
/// **If a future feature turns `dns` on, this const must go up with it** — that is not automatic
/// and nothing else will notice.
///
/// **The asymmetry that decides the spare, and it is the whole argument:** too HIGH costs 352 B of
/// `.bss` — 7% of the 4,771 B the image is short (T-SCOPE §7.1). Too LOW is a `SocketSet`-full
/// panic at runtime, which on this tree means `custom_halt` → `software_reset` → a boot loop on a
/// fleet that OTAs itself. Those are not comparable costs, and **#404 is the precedent**: a
/// `SocketSet` that quietly ran out of slots took the fleet down hourly for weeks. Pay the 352 B.
#[cfg(feature = "wifi")]
pub(crate) const NET_SOCKETS: usize = 4;

/// #335 STEP T: the ONE embassy-net bring-up, called from BOTH radio tiers.
///
/// It exists as a shared helper for a specific reason rather than for tidiness. PHASE3-PLAN §2.2
/// step 3 names three details as "load-bearing and easy to lose", and a second bring-up site is
/// exactly where they get lost — silently, because every one of them fails in a way that still
/// compiles and still boots:
///
/// 1. **DR-M3, the seed.** Two `rng.random()` draws, per boot. A shared literal gives the entire
///    fleet identical TCP ISNs and ephemeral ports — every board indistinguishable to the broker
///    and to any middlebox, which reads as a network fault, not a firmware one.
/// 2. **`self_mac` before the move.** `interfaces.station` is CONSUMED here, so the caller must
///    read its MAC *first* (#68/#76 self-frame drop). Enforced by the signature: this function
///    takes the interface by value, so a caller that has not already read the MAC cannot get it
///    back. That is deliberate — the ordering is a type-level fact rather than a comment.
/// 3. **Spawn failure is `log::error!`, never `.expect()`.** smol's boot path is panic-free by
///    policy: a panic is MF-2 `software_reset`, i.e. a boot loop.
///
/// The DHCP hostname (`kind 12 = "smol"`) is carried across deliberately, and carrying it needed
/// a **Cargo feature**, not a line of code: `DhcpConfig::hostname` does not exist without
/// `dhcpv4-hostname`, so the natural bring-up — written against embassy-net's default features —
/// compiles fine and silently ships an anonymous DHCP lease. See the feature's comment in
/// `Cargo.toml`. A board identifiable in the lease table is how a wedged transfer gets attributed
/// to the right peer instead of to the wrong board.
#[cfg(feature = "wifi")]
pub(crate) fn bring_up_stack(
    spawner: embassy_executor::Spawner,
    station: esp_radio::wifi::Interface<'static>,
    rng: &mut esp_hal::rng::Rng,
) -> embassy_net::Stack<'static> {
    // DR-M3: per-boot entropy, TWO draws — `rng.random()` is u32 and the seed is u64.
    let seed: u64 = ((rng.random() as u64) << 32) | (rng.random() as u64);

    // DHCP option kind 12, exactly as all three pre-T sites set it. Built through `Default` +
    // the inherent `push_str` rather than by naming `heapless::String<32>`: this crate has NO
    // heapless dependency and hand-rolls `mesh_snake::heapless_line` instead, so borrowing
    // embassy-net's transitive copy would add a direct dep — and a version-pinned one, since the
    // lock already carries heapless 0.8 and 0.9 — to write a four-character string.
    let mut dhcp = embassy_net::DhcpConfig::default();
    dhcp.hostname = Some(Default::default());
    if let Some(h) = dhcp.hostname.as_mut() {
        // Infallible at 4 of 32 bytes; a lost hostname would only cost lease-table identity, and
        // `unwrap` here would trade that for a boot-path panic.
        let _ = h.push_str("smol");
    }

    static NET_RESOURCES: static_cell::StaticCell<embassy_net::StackResources<NET_SOCKETS>> =
        static_cell::StaticCell::new();
    let (stack, runner) = embassy_net::new(
        station,
        embassy_net::Config::dhcpv4(dhcp),
        NET_RESOURCES.init(embassy_net::StackResources::new()),
        seed,
    );

    // Panic-free boot policy. There is exactly ONE fallible step and it is the token:
    // `embassy_executor::Spawner::spawn` returns `()` in 0.10 (spawner.rs:162), so capacity
    // failure can only surface as `net_task(..)` yielding `Err`. Checked rather than assumed —
    // an earlier draft here handled a `Result` from `spawn` and criticised the reference for not
    // doing so; the reference was right and the draft did not compile.
    match net_task(runner) {
        Ok(token) => spawner.spawn(token),
        Err(_) => log::error!(
            "smol #335 T: net_task spawn failed (executor pool exhausted) — transport will not pump"
        ),
    }
    stack
}

/// Phase-1 (default) placeholder used when no radio features are enabled: the
/// caller free-runs the clock from its compile-time start constant.
#[cfg(not(feature = "wifi"))]
pub fn try_time_sync() -> Option<u32> {
    None
}
